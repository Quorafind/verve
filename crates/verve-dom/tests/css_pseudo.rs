//! Pseudo-elements: what `::before`/`::after` generate, the counters they read, and the parts
//! that are styled but are no nodes of the tree.

mod common;

use common::{Page, on_page, styled};
use verve_dom::css::{ContentItem, PseudoElement, ScrollbarPart};
use verve_dom::{NodeId, Pointer, Rect};

/// The text a node's `part` box generates, if it has one.
fn text_of(page: &Page, node: NodeId, part: PseudoElement) -> Option<String> {
    let snapshot = page.document.snapshot();
    let found = snapshot.pseudo.get(&(node, part))?;
    Some(
        found
            .content
            .iter()
            .map(|item| match item {
                ContentItem::Text(text) => text.as_str(),
                _ => "",
            })
            .collect(),
    )
}

/// The `::before` texts of the children of the first `selector` in the page.
fn counted(page: &Page, selector: &str) -> Vec<String> {
    let list = page.find(selector);
    let children: Vec<NodeId> = page.document.dom().children(list).to_vec();
    children
        .into_iter()
        .filter_map(|child| text_of(page, child, PseudoElement::Before))
        .collect()
}

#[test]
fn before_and_after_are_generated_boxes_and_not_child_nodes() {
    on_page(|| {
        let mut page = styled(
            "#target::before { content: attr(data-x); color: red }
             #target::after { content: \"\\f101\"; }",
            r#"<div id="target" data-x="prefix">text</div>"#,
        );
        let target = page.find("#target");
        let snapshot = page.document.update().snapshot;
        let before = snapshot
            .pseudo
            .get(&(target, PseudoElement::Before))
            .expect("a ::before box");
        assert_eq!(before.content, vec![ContentItem::Text("prefix".into())]);
        assert_eq!(
            before.style.get("color").map(String::as_str),
            Some("rgb(255, 0, 0)")
        );
        let after = snapshot
            .pseudo
            .get(&(target, PseudoElement::After))
            .expect("an ::after box");
        assert_eq!(after.content, vec![ContentItem::Text("\u{f101}".into())]);
        // They are not nodes: `childNodes` holds the text only, and `querySelectorAll` finds
        // nothing for them.
        assert_eq!(
            page.eval(
                "document.getElementById('target').childNodes.length + ' ' + \
                 document.querySelectorAll('#target::before').length"
            ),
            "1 0"
        );
        // The element's own style is untouched by what its parts say.
        assert_eq!(snapshot.style(target, "color"), None);
    });
}

#[test]
fn the_css_2_pseudo_elements_are_written_with_one_colon_too() {
    on_page(|| {
        // Font Awesome 4, and every clearfix, still write `:before` and `:after`.
        let page = styled(
            ".icon:before { content: \"\\f101\" } .clear:after { content: \"\"; display: table }",
            r#"<i class="icon"></i><div class="clear"></div>"#,
        );
        assert_eq!(
            text_of(&page, page.find(".icon"), PseudoElement::Before).as_deref(),
            Some("\u{f101}")
        );
        let clear = page.find(".clear");
        let snapshot = page.document.snapshot();
        let after = snapshot.pseudo.get(&(clear, PseudoElement::After)).unwrap();
        assert_eq!(
            after.style.get("display").map(String::as_str),
            Some("table")
        );
    });
}

#[test]
fn a_pseudo_element_the_engine_has_no_part_for_matches_nothing() {
    on_page(|| {
        // Not the element itself: `a::file-selector-button { color: red }` is no rule for `a`.
        let mut page = styled(
            "#a::file-selector-button { color: red }
             #b::before { content: \"x\" } #b::before span { color: blue }
             #c { color: green }",
            r#"<div id="a"><span>t</span></div><div id="b"><span>t</span></div><div id="c"></div>"#,
        );
        assert_eq!(
            page.eval("getComputedStyle(document.getElementById('a')).color"),
            "rgb(0, 0, 0)"
        );
        // A pseudo-element ends a selector, so the rule with a `span` after it is dropped,
        // and `querySelector` says why.
        assert_eq!(
            page.eval("getComputedStyle(document.querySelector('#b span')).color"),
            "rgb(0, 0, 0)"
        );
        assert_eq!(
            page.eval(
                "(() => { try { document.querySelector('p::before span'); return 'no'; }
                          catch (e) { return e.name; } })()"
            ),
            "SyntaxError"
        );
    });
}

#[test]
fn content_without_a_value_generates_nothing() {
    on_page(|| {
        let page = styled(
            "#a::before { color: red }
             #b::before { content: none }
             #c::before { content: \"\" }",
            r#"<div id="a"></div><div id="b"></div><div id="c"></div>"#,
        );
        let snapshot = page.document.update().snapshot;
        let (a, b, c) = (page.find("#a"), page.find("#b"), page.find("#c"));
        // `content: normal` — nothing said — makes no box, even with other declarations.
        assert!(!snapshot.pseudo.contains_key(&(a, PseudoElement::Before)));
        assert!(!snapshot.pseudo.contains_key(&(b, PseudoElement::Before)));
        // `content: ""` is a box with no text: the drawer still makes it.
        assert!(snapshot.pseudo.contains_key(&(c, PseudoElement::Before)));
    });
}

#[test]
fn content_reads_strings_urls_quotes_and_alternative_text() {
    on_page(|| {
        let page = styled(
            "#a::before { content: \"a\" attr(data-x) \"b\" }
             #b::before { content: open-quote \"q\" close-quote }
             #c::before { content: url(icon.png) }
             #d::before { content: \"drawn\" / \"read aloud\" }
             #e::before { content: \"line\\A two \\\"q\\\" n\\n\" }",
            r#"<div id="a" data-x="X"></div><div id="b"></div><div id="c"></div>
               <div id="d"></div><div id="e"></div>"#,
        );
        let snapshot = page.document.snapshot();
        let items = |selector: &str| {
            snapshot
                .pseudo
                .get(&(page.find(selector), PseudoElement::Before))
                .map(|found| found.content.clone())
                .unwrap()
        };
        assert_eq!(
            items("#a"),
            vec![
                ContentItem::Text("a".into()),
                ContentItem::Text("X".into()),
                ContentItem::Text("b".into())
            ]
        );
        assert_eq!(
            items("#b"),
            vec![
                ContentItem::OpenQuote,
                ContentItem::Text("q".into()),
                ContentItem::CloseQuote
            ]
        );
        assert_eq!(items("#c"), vec![ContentItem::Image("icon.png".into())]);
        assert_eq!(items("#d"), vec![ContentItem::Text("drawn".into())]);
        // In CSS `\A` is a line feed (the space after it ends the escape), `\"` a quote, and
        // `\n` an `n`.
        assert_eq!(
            items("#e"),
            vec![ContentItem::Text("line\ntwo \"q\" nn".into())]
        );
    });
}

#[test]
fn counters_are_counted_in_document_order() {
    on_page(|| {
        let mut page = styled(
            "ol { counter-reset: item }
             li::before { content: counter(item) \". \" }
             li { counter-increment: item }",
            r#"<ol><li>a</li><li>b</li><li>c</li></ol>"#,
        );
        assert_eq!(counted(&page, "ol"), vec!["1. ", "2. ", "3. "]);
        // Put in at the front, the item restyled is the new one, and the ones after it count on.
        page.eval(
            "const first = document.createElement('li'); first.textContent = 'z';
             document.querySelector('ol').prepend(first);",
        );
        page.document.update();
        assert_eq!(counted(&page, "ol"), vec!["1. ", "2. ", "3. ", "4. "]);
        // An item taken out leaves the numbering of the rest.
        page.eval("document.querySelector('li').remove()");
        page.document.update();
        assert_eq!(counted(&page, "ol"), vec!["1. ", "2. ", "3. "]);
    });
}

#[test]
fn a_sibling_that_resets_a_counter_takes_it_over() {
    on_page(|| {
        // Two lists in a row, each resetting `item`: the second starts from zero again, and is
        // not nested in the first — `counters()` shows one number, not `1.2`.
        let page = styled(
            "ol { counter-reset: item }
             li { counter-increment: item }
             li::before { content: counters(item, \".\") \" \" }",
            r#"<ol id="one"><li>a</li><li>b</li></ol><ol id="two"><li>c</li></ol>"#,
        );
        assert_eq!(counted(&page, "#one"), vec!["1 ", "2 "]);
        assert_eq!(counted(&page, "#two"), vec!["1 "]);
    });
}

#[test]
fn a_nested_reset_nests_the_counter() {
    on_page(|| {
        let page = styled(
            "ol { counter-reset: item }
             li { counter-increment: item; display: block }
             li::before { content: counters(item, \".\") }",
            r#"<ol id="top"><li>a<ol id="inner"><li>b</li><li>c</li></ol></li><li>d</li></ol>"#,
        );
        assert_eq!(counted(&page, "#top"), vec!["1", "2"]);
        assert_eq!(counted(&page, "#inner"), vec!["1.1", "1.2"]);
    });
}

#[test]
fn counters_skip_what_is_not_rendered_and_keep_their_styles() {
    on_page(|| {
        let page = styled(
            "ol { counter-reset: item }
             li { counter-increment: item }
             li.gone { display: none }
             li::before { content: counter(item, upper-roman) \"/\" counter(item, lower-alpha) }",
            r#"<ol><li>a</li><li class="gone">b</li><li>c</li></ol>"#,
        );
        assert_eq!(counted(&page, "ol"), vec!["I/a", "II/b"]);
    });
}

#[test]
fn computed_style_reads_a_pseudo_element() {
    on_page(|| {
        let mut page = styled(
            "#target::before { content: \"*\"; color: #112233; margin-left: 3px }
             #target { color: red }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(
            page.eval(
                "(() => { const s = getComputedStyle(document.getElementById('target'), '::before');
                   return [s.content, s.color, s.marginLeft, s.display].join('|') })()"
            ),
            "\"*\"|rgb(17, 34, 51)|3px|inline"
        );
        // The element itself answers its own colour, not its part's.
        assert_eq!(
            page.eval("getComputedStyle(document.getElementById('target')).color"),
            "rgb(255, 0, 0)"
        );
        // A part inherits from its element.
        assert_eq!(
            page.eval("getComputedStyle(document.getElementById('target'), '::after').color"),
            "rgb(255, 0, 0)"
        );
        // One that is no pseudo-element at all is a declaration with no values.
        assert_eq!(
            page.eval(
                "(() => { const s = getComputedStyle(document.getElementById('target'), '::bogus');
                   return [s.color, s.display, s.length].join('|') })()"
            ),
            "||0"
        );
    });
}

#[test]
fn what_a_declaration_is_about_is_not_a_property_a_script_can_see() {
    on_page(|| {
        let mut page = styled(
            "#target::before { content: \"*\" }",
            r#"<div id="target">t</div>"#,
        );
        assert_eq!(
            page.eval(
                "(() => { const s = getComputedStyle(document.getElementById('target'), '::before');
                   return [Reflect.ownKeys(s).length, Object.keys(s).length,
                           '__pseudo' in s, JSON.stringify(Object.getOwnPropertyNames(s))].join('|') })()"
            ),
            "0|0|false|[]"
        );
    });
}

#[test]
fn a_placeholders_and_scrollbars_style_travel_through_the_snapshot() {
    on_page(|| {
        let page = styled(
            "input::placeholder { color: #999999 }
             ::-webkit-scrollbar { width: 10px }
             ::-webkit-scrollbar-thumb { background-color: #333333; border-radius: 5px }
             ::selection { background-color: #ffff00 }",
            r#"<input id="field" placeholder="type here"><div id="box" style="overflow: auto">x</div>"#,
        );
        let snapshot = page.document.update().snapshot;
        let field = page.find("#field");
        let box_ = page.find("#box");
        let placeholder = snapshot
            .pseudo
            .get(&(field, PseudoElement::Placeholder))
            .expect("a ::placeholder style");
        assert_eq!(
            placeholder.style.get("color").map(String::as_str),
            Some("rgb(153, 153, 153)")
        );
        // `::placeholder` is no box: it only carries a style.
        assert!(placeholder.content.is_empty());
        let thumb = PseudoElement::WebkitScrollbar(ScrollbarPart::Thumb);
        let found = snapshot
            .pseudo
            .get(&(box_, thumb))
            .expect("a scrollbar thumb");
        assert_eq!(
            found.style.get("background-color").map(String::as_str),
            Some("rgb(51, 51, 51)")
        );
        assert!(snapshot.pseudo.contains_key(&(
            box_,
            PseudoElement::WebkitScrollbar(ScrollbarPart::Scrollbar)
        )));
        // `::selection` is on every element, as the universal selector reaches them.
        let selection = snapshot
            .pseudo
            .get(&(box_, PseudoElement::Selection))
            .expect("a ::selection style");
        assert_eq!(
            selection.style.get("background-color").map(String::as_str),
            Some("rgb(255, 255, 0)")
        );
    });
}

#[test]
fn a_part_is_only_worked_out_for_the_elements_that_can_have_it() {
    on_page(|| {
        // A bare `::placeholder` and `::-webkit-scrollbar` are about every element, but only a
        // field has a placeholder and only something that scrolls has a bar.
        let page = styled(
            "::placeholder { color: red } ::-webkit-scrollbar { width: 8px }
             #scroller { overflow-y: scroll }",
            r#"<input id="field"><textarea id="area"></textarea><div id="plain">x</div>
               <div id="scroller">y</div>"#,
        );
        let snapshot = page.document.snapshot();
        let has = |selector: &str, part: PseudoElement| {
            snapshot.pseudo.contains_key(&(page.find(selector), part))
        };
        let bar = PseudoElement::WebkitScrollbar(ScrollbarPart::Scrollbar);
        assert!(has("#field", PseudoElement::Placeholder));
        assert!(has("#area", PseudoElement::Placeholder));
        assert!(!has("#plain", PseudoElement::Placeholder));
        assert!(has("#scroller", bar));
        assert!(has("html", bar));
        assert!(!has("#plain", bar));
        assert!(!has("#field", bar));
    });
}

#[test]
fn the_marker_of_a_list_item_is_generated() {
    on_page(|| {
        let page = styled("li::marker { color: red }", "<ul><li id=one>a</li></ul>");
        let snapshot = page.document.update().snapshot;
        let one = page.find("#one");
        let marker = snapshot
            .pseudo
            .get(&(one, PseudoElement::Marker))
            .expect("a ::marker");
        assert_eq!(
            marker.style.get("color").map(String::as_str),
            Some("rgb(255, 0, 0)")
        );
    });
}

#[test]
fn a_change_only_a_part_shows_marks_the_element_restyled() {
    on_page(|| {
        let mut page = styled(
            "#target::before { content: attr(data-x) }",
            r#"<div id="target" data-x="a">t</div>"#,
        );
        let target = page.find("#target");
        page.document.update();
        // The element's own style is the same after the attribute changes; its `::before` is
        // not, and a drawer that only looks at restyled nodes must hear of it.
        page.eval("document.getElementById('target').setAttribute('data-x', 'b')");
        let update = page.document.update();
        assert!(update.changes.restyled.contains(&target));
        assert_eq!(
            text_of(&page, target, PseudoElement::Before).as_deref(),
            Some("b")
        );
        // Nothing changed: nothing is restyled.
        assert!(page.document.update().changes.restyled.is_empty());
    });
}

#[test]
fn the_boxes_of_a_node_that_is_gone_go_with_it() {
    on_page(|| {
        let mut page = styled(
            "#target::before { content: \"x\" }",
            r#"<div id="target">t</div>"#,
        );
        let target = page.find("#target");
        page.document.update();
        assert!(
            page.document
                .snapshot()
                .pseudo
                .contains_key(&(target, PseudoElement::Before))
        );
        page.eval("document.getElementById('target').remove()");
        page.document.update();
        page.document.sweep(&mut page.runtime);
        page.document.sweep(&mut page.runtime);
        assert!(
            !page
                .document
                .snapshot()
                .pseudo
                .contains_key(&(target, PseudoElement::Before))
        );
    });
}

#[test]
fn a_hover_rule_on_a_part_follows_the_pointer() {
    on_page(|| {
        let mut page = styled(
            "a:hover::before { content: \"go\" }",
            r#"<a id="link">t</a>"#,
        );
        let link = page.find("#link");
        page.document
            .set_layout(&mut page.runtime, [(link, Rect::new(0., 0., 100., 20.))]);
        page.document.update();
        // The part appears with the pointer, so the element is one to follow it on.
        assert!(page.document.hover_sensitive(link));
        assert_eq!(text_of(&page, link, PseudoElement::Before), None);
        page.document.pointer_move(
            &mut page.runtime,
            &Pointer {
                x: 10.,
                y: 10.,
                ..Pointer::default()
            },
        );
        let update = page.document.update();
        assert_eq!(
            text_of(&page, link, PseudoElement::Before).as_deref(),
            Some("go")
        );
        assert!(update.changes.restyled.contains(&link));
    });
}

#[test]
fn a_pseudo_element_counts_with_its_own_counter_properties() {
    on_page(|| {
        // The canonical example: the counter is incremented by the `::before` that reads it.
        let page = styled(
            "body { counter-reset: s } h3::before { counter-increment: s; content: counter(s) }",
            "<h3>a</h3><h3>b</h3><h3>c</h3>",
        );
        let texts = counted(&page, "body");
        assert_eq!(texts, vec!["1", "2", "3"]);
        // And an `::after`, which comes after the children, counts after them.
        let page = styled(
            "body { counter-reset: s } p::after { counter-increment: s; content: \"\" } p::before { content: counter(s) }
             p { counter-increment: s }",
            "<p>a</p><p>b</p>",
        );
        let texts = counted(&page, "body");
        assert_eq!(texts, vec!["1", "3"]);
    });
}

#[test]
fn a_pseudo_element_ends_its_selector_and_replaced_elements_have_none() {
    on_page(|| {
        let mut page = styled(
            "img::before, input::after, #p::before { content: \"x\" } #q::before.foo { color: red }",
            r#"<img id="i"><input id="f"><p id="p"></p><p id="q"></p>"#,
        );
        let snapshot = page.document.snapshot();
        assert!(
            !snapshot
                .pseudo
                .contains_key(&(page.find("#i"), PseudoElement::Before))
        );
        assert!(
            !snapshot
                .pseudo
                .contains_key(&(page.find("#f"), PseudoElement::After))
        );
        assert!(
            snapshot
                .pseudo
                .contains_key(&(page.find("#p"), PseudoElement::Before))
        );
        assert_eq!(
            page.eval(
                "['p::before.foo', 'p::before::after', 'p::before:hover'].map(s => {
                   try { document.querySelector(s); return 'ok'; } catch (e) { return e.name; }
                 }).join()"
            ),
            "SyntaxError,SyntaxError,ok"
        );
    });
}
