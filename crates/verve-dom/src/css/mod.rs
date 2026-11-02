//! How a node looks: the stylesheets — the user agent's defaults, the embedder's, every
//! `<style>` element and adopted sheet in the document — cascaded onto the tree in
//! [`crate::dom`].
//!
//! Selectors are the web's (Selectors level 4, less what needs layout): types, `*`, `#id`,
//! `.class`, attributes with every operator and the `i` flag, the descendant, child, next- and
//! subsequent-sibling combinators, `:not()`, `:is()`, `:where()`, `:has()`, the structural
//! pseudo-classes (`:nth-child(2n+1 of S)` and its family), and the state ones — `:hover` and
//! `:active` from the pointer, `:focus`, `:focus-within`, `:checked`, `:disabled`… A
//! pseudo-element (`::before`) is the part of an element a rule is about, styled beside the
//! element and given to the drawer as a [`PseudoBox`]; it matches no element to a query. One
//! that does not parse is `querySelector`'s `SyntaxError`, and a stylesheet's rule that has one
//! is dropped, as a browser drops it.
//!
//! The cascade is origin, importance, `@layer`, specificity and order; the inline style wins
//! over every rule of its importance; the inherited properties come down from the parent;
//! `inherit`, `initial`, `unset` and `revert-layer` are honoured; and `var()` reads custom
//! properties, which inherit like any other — so `var(--text-muted)` follows whatever the
//! document set `--text-muted` to. `@media` is evaluated against the [`Environment`] (the
//! viewport, the colour scheme); `@supports` blocks apply; the rest (`@keyframes`,
//! `@font-face`) are not rules about a node.
//!
//! What comes out is each property's value as text; turning `12px` into a length and `#fff`
//! into a colour is the drawer's, with [`length`] and [`color`].
//!
//! Lifted from su-editor's Obsidian host (its cascade, `var()` and incremental restyle) with a
//! selector engine of its own.

mod boxes;
mod calc;
mod cascade;
mod color;
mod counters;
mod layer;
mod media;
mod pseudo;
mod scope;
mod selector;
mod sheet;
mod styler;
mod syntax;
mod values;

pub use calc::{LengthCalc, length, length_calc};
pub(crate) use cascade::style_of_part;
pub use cascade::{Computed, Origin, Style, style_of};
pub use color::{color, color_with};
pub use media::{Environment, MediaQuery};
pub use pseudo::{ContentItem, PseudoBox, PseudoElement, ScrollbarPart};
pub use selector::{MatchContext, Selector, SelectorError, SelectorList};
pub use sheet::{Declaration, Sheet, declarations, split_rules};
pub use styler::Styler;
pub(crate) use values::{ROOT_FONT_SIZE, font_longhands, font_size_px, value_or_initial};
pub use values::{USER_AGENT_SHEET, initial_value, is_inherited};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Dom;

    fn styler(sheet: &Sheet) -> Styler {
        let document = Dom::new().document;
        Styler::new(
            &[(Origin::Author, sheet, document)],
            &Environment::default(),
        )
    }

    #[test]
    fn rules_cascade_by_specificity_order_and_inheritance() {
        let mut dom = Dom::new();
        let modal = dom.create_element("div");
        dom.set_attr(modal, "class", "modal");
        let title = dom.create_element("div");
        dom.set_attr(title, "class", "modal-title");
        dom.append(dom.body, modal);
        dom.append(modal, title);
        dom.set_style(dom.body, "--text-normal", "#112233");
        let sheet = Sheet::parse(
            "/* c */ .modal { color: var(--text-normal); padding: 4px 8px }
             div { color: red }
             .modal > .modal-title { font-weight: 600 }
             @media (max-width: 10px) { .modal-title { font-size: 20px } }
             @media (min-width: 10px) { .modal-title { font-style: italic } }
             .modal .missing { color: blue }
             .modal-title { color: green !important }
             #nope, .modal-title:not(.other) { text-align: center }",
        );
        let computed = Computed::cascade(&dom, &styler(&sheet));
        assert_eq!(computed.get(modal, "color"), Some("rgb(17, 34, 51)"));
        assert_eq!(computed.get(modal, "padding-left"), Some("8px"));
        assert_eq!(computed.get(title, "color"), Some("rgb(0, 128, 0)"));
        assert_eq!(computed.get(title, "font-weight"), Some("600"));
        assert_eq!(computed.get(title, "font-size"), None);
        assert_eq!(computed.get(title, "font-style"), Some("italic"));
        assert_eq!(computed.get(title, "text-align"), Some("center"));
        assert_eq!(color("#ff000080").unwrap()[3], 128. / 255.);
        assert_eq!(color("rebeccapurple").unwrap()[0], 0x66 as f32 / 255.);
        assert_eq!(length("1.5em", 10.), Some(15.));
    }

    #[test]
    fn selectors_query_the_tree() {
        let mut dom = Dom::new();
        let a = dom.create_element("div");
        dom.set_attr(a, "id", "x");
        let b = dom.create_element("input");
        dom.set_attr(b, "type", "text");
        let c = dom.create_element("p");
        dom.set_attr(c, "data-kind", "lead-in");
        let d = dom.create_element("p");
        dom.append(dom.body, a);
        dom.append(a, b);
        dom.append(a, c);
        dom.append(a, d);
        assert_eq!(dom.query(dom.document, "#x input[type=text]"), Some(b));
        assert_eq!(dom.query(dom.document, "body > div"), Some(a));
        assert_eq!(dom.query(dom.document, "html > input"), None);
        assert_eq!(dom.query_all(dom.document, "div, input").len(), 2);
        assert_eq!(dom.query(dom.document, "input + p"), Some(c));
        assert_eq!(dom.query_all(dom.document, "input ~ p").len(), 2);
        assert_eq!(dom.query(dom.document, "p:nth-child(3)"), Some(d));
        assert_eq!(dom.query(dom.document, "p:last-child"), Some(d));
        assert_eq!(dom.query(dom.document, "p:nth-of-type(1)"), Some(c));
        assert_eq!(dom.query(dom.document, "[data-kind|=lead]"), Some(c));
        assert_eq!(dom.query(dom.document, "[data-kind^='lead']"), Some(c));
        assert_eq!(dom.query(dom.document, "p:not([data-kind])"), Some(d));
        assert_eq!(dom.query(dom.document, "div:has(> input)"), Some(a));
        assert_eq!(dom.query(dom.document, ":is(span, p)"), Some(c));
        assert!(SelectorList::parse("div[").is_err());
        assert!(SelectorList::parse("p:nope").is_err());
        assert!(SelectorList::parse("a::before").is_ok());
        assert_eq!(dom.query(dom.document, ".a\\:b"), None);
    }

    #[test]
    fn media_queries_read_the_environment() {
        let env = Environment {
            width: 500.,
            dark: true,
            ..Environment::default()
        };
        assert!(MediaQuery::new("(max-width: 600px)").matches(&env));
        assert!(!MediaQuery::new("screen and (min-width: 600px)").matches(&env));
        assert!(MediaQuery::new("(prefers-color-scheme: dark)").matches(&env));
        assert!(MediaQuery::new("print, (width < 600px)").matches(&env));
        assert!(!MediaQuery::new("not all and (prefers-color-scheme: dark)").matches(&env));
    }
}
