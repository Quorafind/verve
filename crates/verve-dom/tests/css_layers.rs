//! `@layer`: the order layers are declared in, what an unlayered rule does to them, and what
//! `!important` reverses — against Tailwind v4's own output, whose every rule is layered.

mod common;

use common::{Page, on_page, style, styled};
use verve_dom::Options;

const TAILWIND: &str = include_str!("fixtures/tailwind4/utilities.css");

#[test]
fn tailwind_utilities_override_base_and_components() {
    on_page(|| {
        let mut page = styled(
            TAILWIND,
            r#"<h1 id="title" class="text-sm text-red-500">Title</h1>
               <div id="box" class="p-4 btn">box</div>"#,
        );
        // `base` resets the heading's size; `utilities` sets it again, being the last layer —
        // and a font size is computed to pixels.
        assert_eq!(style(&mut page, "title", "font-size"), "14px");
        assert_eq!(
            style(&mut page, "title", "color"),
            "oklch(63.7% 0.237 25.331)"
        );
        // `utilities` is after `components`: both say what the box's colour is, the utility wins.
        assert_eq!(
            style(&mut page, "box", "color"),
            "oklch(63.7% 0.237 25.331)"
        );
        assert_eq!(page.console(), "");
    });
}

#[test]
fn a_layer_declared_first_comes_first() {
    on_page(|| {
        // `@layer b, a;` orders the layers before either has a rule, so `a` beats `b` even
        // though `b`'s selector would win on specificity.
        let mut page = styled(
            "@layer b, a;
             @layer a { div { letter-spacing: 1px } }
             @layer b { #target { letter-spacing: 2px } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "1px");
        let mut page = styled(
            "@layer a, b;
             @layer b { div { letter-spacing: 2px } }
             @layer a { #target { letter-spacing: 1px } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
    });
}

#[test]
fn a_layers_own_rules_beat_its_sublayers() {
    on_page(|| {
        // Chrome: 1px. The rules written directly in `outer` are its last sublayer, so they
        // win over `outer.inner` however the two are ordered in the text.
        let mut page = styled(
            "@layer outer {
               div { letter-spacing: 1px }
               @layer inner { #target { letter-spacing: 2px } }
             }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "1px");
        let mut page = styled(
            "@layer outer.inner { #target { letter-spacing: 2px } }
             @layer outer { div { letter-spacing: 1px } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "1px");
        // A sibling layer declared later still beats the whole of `outer`, sublayers and all.
        let mut page = styled(
            "@layer outer { div { letter-spacing: 1px } @layer inner { div { letter-spacing: 3px } } }
             @layer later { div { letter-spacing: 2px } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
    });
}

#[test]
fn an_unlayered_rule_beats_every_layer() {
    on_page(|| {
        let mut page = styled(
            "@layer base { #target { letter-spacing: 1px } }
             div { letter-spacing: 2px }",
            r#"<div id="target">t</div>"#,
        );
        // The unlayered rule wins even though `#target` is the more specific selector.
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
        // An inline style is unlayered too, and wins over it.
        let mut page = styled(
            "@layer base { #target { letter-spacing: 1px } }
             div { letter-spacing: 2px }",
            r#"<div id="target" style="letter-spacing: 3px">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "3px");
    });
}

#[test]
fn important_reverses_the_layer_order() {
    on_page(|| {
        // Normal: the later layer wins. Important: the earlier one does, and unlayered
        // important — the last layer — loses to both.
        let mut page = styled(
            "@layer first, second;
             @layer first { #target { letter-spacing: 1px !important } }
             @layer second { div { letter-spacing: 2px !important } }
             #target { letter-spacing: 3px !important }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "1px");
        let mut page = styled(
            "@layer first, second;
             @layer first { #target { letter-spacing: 1px !important } }
             @layer second { div { letter-spacing: 2px !important } }
             #target { letter-spacing: 3px }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "1px");
        // An inline `!important` beats every layered `!important`.
        let mut page = styled(
            "@layer first { #target { letter-spacing: 1px !important } }",
            r#"<div id="target" style="letter-spacing: 3px !important">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "3px");
    });
}

#[test]
fn revert_layer_falls_back_to_the_layer_before() {
    on_page(|| {
        let mut page = styled(
            "@layer base, theme;
             @layer base { #target { letter-spacing: 1px; padding: 2px } }
             @layer theme { #target { letter-spacing: 2px; padding: revert-layer } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
        assert_eq!(style(&mut page, "target", "padding-top"), "2px");
        // Nothing under it: the property is what it would be unset — here, its initial value.
        let mut page = styled(
            "@layer theme { #target { padding: revert-layer } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "padding-top"), "0px");
    });
}

#[test]
fn revert_layer_rolls_back_to_the_user_agents_value() {
    on_page(|| {
        // The user agent's `div { display: block }` is under every author layer: with no
        // layer under it, `revert-layer` rolls back to it, not to an unset `display`.
        let mut page = styled(
            "div { display: revert-layer }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "display"), "block");
        let mut page = styled(
            "@layer a { div { display: flex } } div { display: revert-layer }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "display"), "flex");
    });
}

#[test]
fn a_layer_is_one_layer_across_sheets() {
    on_page(|| {
        let mut page = Page::new(Options::default());
        page.eval(
            r#"document.head.innerHTML = `<style>
                 @layer base, utilities;
                 @layer base { div { letter-spacing: 1px } }
               </style>
               <style>
                 @layer utilities { #target { letter-spacing: 2px } }
                 @layer base { #target { letter-spacing: 3px } }
               </style>`;
               document.body.innerHTML = '<div id="target">t</div>';"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
    });
}

#[test]
fn anonymous_layers_are_layers_of_their_own() {
    on_page(|| {
        // In one sheet, ordered where they appear.
        let mut page = styled(
            "@layer { #target { letter-spacing: 1px } }
             @layer { div { letter-spacing: 2px } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
        // In two sheets: each sheet's first anonymous layer is its own, not one layer shared
        // by the name they would both be given.
        let mut page = Page::new(Options::default());
        page.eval(
            r#"document.head.innerHTML = `<style>
                 @layer { #target { letter-spacing: 1px } }
               </style>
               <style>
                 @layer { div { letter-spacing: 2px } }
               </style>`;
               document.body.innerHTML = '<div id="target">t</div>';"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "2px");
        // Inside a named layer, and holding a named one: the nesting is of the layers.
        let mut page = styled(
            "@layer { @layer x { #target { letter-spacing: 1px } } }
             @layer { div { letter-spacing: 2px } }
             @layer y { @layer { #target { letter-spacing: 4px } } div { letter-spacing: 3px } }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(style(&mut page, "target", "letter-spacing"), "3px");
    });
}

#[test]
fn a_shadow_tree_orders_its_own_layers() {
    on_page(|| {
        let mut page = styled("@layer a, b;", r#"<div id="host"></div>"#);
        // The document says `a` before `b`; the shadow tree says it the other way round, and
        // its own order is the one its rules follow.
        assert_eq!(
            page.eval(
                r#"(() => {
                    const root = document.getElementById('host').attachShadow({ mode: 'open' });
                    root.innerHTML = `<style>
                        @layer b, a;
                        @layer a { div { letter-spacing: 1px } }
                        @layer b { #t { letter-spacing: 2px } }
                    </style><div id="t">t</div>`;
                    return getComputedStyle(root.getElementById('t')).letterSpacing;
                })()"#
            ),
            "1px"
        );
    });
}

#[test]
fn revert_layer_finds_what_shorthands_and_longhands_said() {
    on_page(|| {
        // A longhand in the layer below, a shorthand that reverts: Chrome gives the 2px back.
        let mut page = styled(
            "@layer a { #t { padding-top: 2px } } @layer b { #t { padding: revert-layer } }",
            r#"<div id="t">t</div>"#,
        );
        assert_eq!(style(&mut page, "t", "padding-top"), "2px");
        assert_eq!(style(&mut page, "t", "padding-left"), "0px");
        // A shorthand below, a longhand that reverts, and the longhands the others left alone.
        let mut page = styled(
            "@layer a { #t { padding: 2px } } @layer b { #t { padding-top: revert-layer; padding-left: 5px } }",
            r#"<div id="t">t</div>"#,
        );
        assert_eq!(style(&mut page, "t", "padding-top"), "2px");
        assert_eq!(style(&mut page, "t", "padding-left"), "5px");
        assert_eq!(style(&mut page, "t", "padding-right"), "2px");
    });
}
