//! What the display list says about text, with the headless measurer (a character is half an em
//! wide, the ascent four fifths of an em): the glyphs of each piece of text, the lines drawn through
//! them, the boxes of inline elements, the selection, and the order they come in.

mod common;

use common::{Laid, lay_out, on_page};
use verve_dom::NodeId;
use verve_layout::{DecorationKind, Fill, PaintItem};

const BASE: &str = "<style>html,body{margin:0}div{font-size:10px}</style>";

fn page(html: &str) -> Laid {
    lay_out(&format!("{BASE}{html}"), "")
}

fn glyphs(page: &Laid) -> Vec<PaintItem> {
    page.list()
        .items
        .iter()
        .filter(|item| matches!(item, PaintItem::Glyphs { .. }))
        .cloned()
        .collect()
}

fn text_in(page: &Laid, id: &str) -> NodeId {
    let dom = &page.snapshot.dom;
    dom.subtree(page.node(id))
        .into_iter()
        .find(|node| dom.get(*node).is_some_and(|found| found.is_text()))
        .expect("the element holds text")
}

#[test]
fn a_line_of_text_is_one_glyph_item_at_its_baseline() {
    on_page(|| {
        let page = page(r#"<div id="a" style="color:rgb(255,0,0)">abc</div>"#);
        let runs = glyphs(&page);
        let [
            PaintItem::Glyphs {
                origin,
                size,
                color,
                glyphs,
                ..
            },
        ] = runs.as_slice()
        else {
            panic!("one run of glyphs, got {runs:?}");
        };
        // The line is 10 high with the baseline 8 from its top.
        assert_eq!((*origin, *size), ([0., 8.], 10.));
        assert_eq!(*color, [1., 0., 0., 1.]);
        let at: Vec<f32> = glyphs.iter().map(|glyph| glyph.x).collect();
        assert_eq!(at, [0., 5., 10.]);
    });
}

#[test]
fn text_is_painted_over_the_background_of_its_block_and_of_the_block_after() {
    on_page(|| {
        let page = page(
            r#"<div id="a" style="background:#00f">one</div><div id="b" style="background:#0f0">two</div>"#,
        );
        let kinds: Vec<&str> = page
            .list()
            .items
            .iter()
            .filter_map(|item| match item {
                PaintItem::Box { .. } => Some("box"),
                PaintItem::Glyphs { .. } => Some("glyphs"),
                _ => None,
            })
            .collect();
        // Both backgrounds first, then the text of the blocks in order.
        assert_eq!(kinds, ["box", "box", "glyphs", "glyphs"]);
    });
}

#[test]
fn an_inline_element_has_a_box_for_its_background_and_border() {
    on_page(|| {
        let page = page(
            r#"<div>ab <span id="s" style="background:#ff0;border:1px solid #000;padding:0 2px">cd</span></div>"#,
        );
        let items = page.items_of("s");
        let Some(PaintItem::Box {
            bounds,
            background,
            border,
            ..
        }) = items.first()
        else {
            panic!("a box for the span, got {items:?}");
        };
        // "ab " is 15 wide; the border and the padding add 3 on each side of the 10 of "cd".
        assert_eq!((bounds.x, bounds.width), (15., 16.));
        assert_eq!(background, &vec![Fill::Color([1., 1., 0., 1.])]);
        assert_eq!(border.widths, [1.; 4]);
    });
}

#[test]
fn an_inline_element_over_two_lines_keeps_its_border_on_the_outer_sides() {
    on_page(|| {
        let page = page(
            r#"<div style="width:30px"><span id="s" style="border:1px solid #000">aaa bbb</span></div>"#,
        );
        let boxes = page.items_of("s");
        assert_eq!(boxes.len(), 2, "one box on each line, got {boxes:?}");
        let sides = |item: &PaintItem| match item {
            PaintItem::Box { border, .. } => (border.widths[3], border.widths[1]),
            _ => panic!("a box"),
        };
        assert_eq!(sides(&boxes[0]), (1., 0.));
        assert_eq!(sides(&boxes[1]), (0., 1.));
    });
}

#[test]
fn a_decoration_runs_the_width_of_the_text_it_is_on() {
    on_page(|| {
        let page =
            page(r#"<div>a <a id="l" style="text-decoration:underline wavy red">bcd</a></div>"#);
        let lines: Vec<PaintItem> = page
            .list()
            .items
            .iter()
            .filter(|item| matches!(item, PaintItem::Decoration { .. }))
            .cloned()
            .collect();
        let [
            PaintItem::Decoration {
                kind,
                bounds,
                color,
                wavy,
            },
        ] = lines.as_slice()
        else {
            panic!("one underline, got {lines:?}");
        };
        assert_eq!(*kind, DecorationKind::Underline);
        assert_eq!((bounds.x, bounds.width), (10., 15.));
        assert!(bounds.y > 8., "below the baseline: {}", bounds.y);
        assert_eq!(*color, [1., 0., 0., 1.]);
        assert!(*wavy);
    });
}

#[test]
fn a_selection_fills_the_height_of_the_line() {
    on_page(|| {
        let mut page = page(r#"<div id="a" style="line-height:20px">abcdef</div>"#);
        let text = text_in(&page, "a");
        assert!(page.engine.set_selection(Some(((text, 1), (text, 3)))));
        let boxes: Vec<PaintItem> = page
            .list()
            .items
            .iter()
            .filter(|item| matches!(item, PaintItem::Box { node: 0, .. }))
            .cloned()
            .collect();
        let [PaintItem::Box { bounds, .. }] = boxes.as_slice() else {
            panic!("one selection box, got {boxes:?}");
        };
        // Two characters of 5, the whole 20 high line, not the 10 high font.
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (5., 0., 10., 20.)
        );
        // The same selection again changes nothing, and none takes the box away.
        assert!(!page.engine.set_selection(Some(((text, 1), (text, 3)))));
        assert!(page.engine.set_selection(None));
        assert!(
            !page
                .list()
                .items
                .iter()
                .any(|item| matches!(item, PaintItem::Box { node: 0, .. }))
        );
    });
}

#[test]
fn the_text_of_a_clipping_block_is_inside_its_clip_and_scroll_frame() {
    on_page(|| {
        let page = page(
            r#"<div style="width:50px;height:20px;overflow:auto"><div style="height:100px">text</div></div>"#,
        );
        let items = page.list().items.clone();
        let at = items
            .iter()
            .position(|item| matches!(item, PaintItem::Glyphs { .. }))
            .expect("glyphs");
        let opened = items[..at]
            .iter()
            .filter(|item| matches!(item, PaintItem::PushClip { .. }))
            .count();
        let closed = items[..at]
            .iter()
            .filter(|item| matches!(item, PaintItem::PopClip))
            .count();
        assert_eq!(opened - closed, 1, "inside the clip of the scroller");
    });
}

#[test]
fn text_that_is_clipped_away_makes_no_items() {
    on_page(|| {
        let page = page(
            r#"<div style="width:50px;height:10px;overflow:hidden"><div style="margin-top:50px">hidden</div></div>"#,
        );
        assert!(glyphs(&page).is_empty());
    });
}

#[test]
fn a_spans_box_comes_before_its_text_and_lines_go_under_or_over_it() {
    on_page(|| {
        let page = page(
            r#"<div><span style="background:#ff0;text-decoration:underline overline line-through">ab</span></div>"#,
        );
        let order: Vec<String> = page
            .list()
            .items
            .iter()
            .filter_map(|item| match item {
                PaintItem::Box { .. } => Some("box".to_owned()),
                PaintItem::Glyphs { .. } => Some("glyphs".to_owned()),
                PaintItem::Decoration { kind, .. } => Some(format!("{kind:?}")),
                _ => None,
            })
            .collect();
        // Underline and overline under the text, the line through it over.
        assert_eq!(
            order,
            ["box", "Underline", "Overline", "glyphs", "LineThrough"]
        );
    });
}

#[test]
fn the_text_of_a_scroller_is_under_its_scrollbar() {
    on_page(|| {
        let page = page(
            r#"<div style="overflow:scroll;width:50px;height:20px">one two three four five six</div>"#,
        );
        let items = &page.list().items;
        let text = items
            .iter()
            .position(|item| matches!(item, PaintItem::Glyphs { .. }))
            .expect("text");
        let bar = items
            .iter()
            .position(|item| matches!(item, PaintItem::Scroller { .. }))
            .expect("a scroller");
        assert!(text < bar, "text at {text}, the scrollbar at {bar}");
    });
}

#[test]
fn a_line_with_no_height_inside_a_clip_still_draws_its_text() {
    on_page(|| {
        let page = page(
            r#"<div style="overflow:hidden;width:100px;height:20px"><div style="line-height:0">abc</div></div>"#,
        );
        assert_eq!(glyphs(&page).len(), 1);
    });
}
