//! Inline layout with the headless measurer: every character is half an em wide, a font's ascent
//! is four fifths of an em and its descent the rest, so at `font-size: 10px` a character is 5
//! wide, the line above the baseline is 8 and below it 2, and `line-height: normal` is 10. These
//! tests check the rules (white space, breaking, alignment, `vertical-align`, atomic inlines, what
//! a script reads back), not Chromium's numbers: the real-font tests freeze those.

mod common;

use std::sync::Arc;

use common::{Laid, lay_out, on_page};
use verve_dom::NodeId;
use verve_layout::{Fragment, HeadlessMeasurer, LayoutEngine};

const BASE: &str = "<style>html,body{margin:0}div{font-size:10px}</style>";

/// A page whose body is `html` after the base style.
fn page(html: &str) -> Laid {
    lay_out(&format!("{BASE}{html}"), "")
}

/// The first text node in the element with `id`.
fn text_in(page: &Laid, id: &str) -> NodeId {
    let dom = &page.snapshot.dom;
    dom.subtree(page.node(id))
        .into_iter()
        .find(|node| dom.get(*node).is_some_and(|found| found.is_text()))
        .expect("the element holds text")
}

/// The boxes of all the text in the element, `[x, y, width, height]` each.
fn text_rects(page: &Laid, id: &str) -> Vec<[f32; 4]> {
    let node = text_in(page, id);
    let length = page
        .snapshot
        .dom
        .get(node)
        .and_then(|found| found.data())
        .map_or(0, |text| text.encode_utf16().count());
    page.engine
        .range_rects((node, 0), (node, length))
        .iter()
        .map(|rect| [rect.x, rect.y, rect.width, rect.height])
        .collect()
}

fn client_rects(page: &Laid, id: &str) -> Vec<[f32; 4]> {
    page.engine
        .client_rects(page.node(id))
        .iter()
        .map(|rect| [rect.x, rect.y, rect.width, rect.height])
        .collect()
}

fn height_of(page: &Laid, id: &str) -> f32 {
    page.rect(id)[3]
}

#[test]
fn spaces_collapse_where_the_mode_says() {
    on_page(|| {
        let page = page(
            r#"<div id="normal">a   b</div>
               <div id="pre" style="white-space:pre">a   b</div>
               <div id="line" style="white-space:pre-line">a  b
c</div>
               <div id="nowrap" style="width:20px;white-space:nowrap">aaa bbb</div>
               <div id="wrap" style="width:20px;white-space:pre-wrap">aaaa bbbb</div>"#,
        );
        // "a b" is three characters, "a   b" five.
        assert_eq!(text_rects(&page, "normal"), vec![[0., 0., 15., 10.]]);
        assert_eq!(text_rects(&page, "pre"), vec![[0., 10., 25., 10.]]);
        // pre-line: spaces collapse, the newline breaks.
        assert_eq!(
            text_rects(&page, "line"),
            vec![[0., 20., 15., 10.], [0., 30., 5., 10.]]
        );
        // nowrap: one line however narrow the box.
        assert_eq!(height_of(&page, "nowrap"), 10.);
        assert_eq!(text_rects(&page, "nowrap"), vec![[0., 40., 35., 10.]]);
        // pre-wrap wraps at the space and the space hangs: it is part of the first line's text
        // (a caret can stand in it) but the line is as long as "aaaa".
        assert_eq!(height_of(&page, "wrap"), 20.);
        assert_eq!(
            text_rects(&page, "wrap"),
            vec![[0., 50., 25., 10.], [0., 60., 20., 10.]]
        );
        let lines = page.engine.inline_layout(page.node("wrap")).expect("lines");
        let Fragment::Text(first) = &lines.lines[0].fragments[0] else {
            panic!("text");
        };
        assert_eq!(first.stops.len(), 6);
        assert_eq!(lines.width, 20.);
    });
}

#[test]
fn newlines_break_lines_where_white_space_is_kept() {
    on_page(|| {
        let page = page(
            "<div id=\"pre\" style=\"white-space:pre\">a\nbb</div><div id=\"normal\">a\nbb</div>",
        );
        assert_eq!(height_of(&page, "pre"), 20.);
        assert_eq!(
            text_rects(&page, "pre"),
            vec![[0., 0., 5., 10.], [0., 10., 10., 10.]]
        );
        assert_eq!(height_of(&page, "normal"), 10.);
    });
}

#[test]
fn a_br_breaks_the_line_and_a_final_one_adds_none() {
    on_page(|| {
        let page = page(
            r#"<div id="two">a<br>b</div><div id="one">a<br></div><div id="three"><br><br>x</div>"#,
        );
        assert_eq!(height_of(&page, "two"), 20.);
        assert_eq!(height_of(&page, "one"), 10.);
        assert_eq!(height_of(&page, "three"), 30.);
    });
}

#[test]
fn word_break_and_overflow_wrap_decide_where_a_long_word_breaks() {
    on_page(|| {
        let page = page(
            r#"<div id="plain" style="width:20px">abcdefgh</div>
               <div id="all" style="width:20px;word-break:break-all">abcdefgh</div>
               <div id="wrap" style="width:20px;overflow-wrap:break-word">abcdefgh</div>
               <div id="anywhere" style="width:20px;overflow-wrap:anywhere">abcdefgh</div>
               <div id="cjk" style="width:20px">中中中中中中</div>
               <div id="keep" style="width:20px;word-break:keep-all">中中中中中中</div>"#,
        );
        assert_eq!(height_of(&page, "plain"), 10.);
        assert_eq!(height_of(&page, "all"), 20.);
        assert_eq!(
            text_rects(&page, "all"),
            vec![[0., 10., 20., 10.], [0., 20., 20., 10.]]
        );
        assert_eq!(height_of(&page, "wrap"), 20.);
        assert_eq!(height_of(&page, "anywhere"), 20.);
        // Chinese breaks between any two characters; keep-all keeps the run whole.
        assert_eq!(height_of(&page, "cjk"), 20.);
        assert_eq!(height_of(&page, "keep"), 10.);
    });
}

#[test]
fn text_indent_moves_the_start_of_the_first_line() {
    on_page(|| {
        let page = page(
            r#"<div id="d" style="width:60px;text-indent:20px">aaaa bbbb cccc</div>
               <div id="negative" style="width:60px;text-indent:-5px;margin-left:10px">aaaa</div>"#,
        );
        // "aaaa" after the indent, then "bbbb cccc" on a line of its own.
        assert_eq!(
            text_rects(&page, "d"),
            vec![[20., 0., 20., 10.], [0., 10., 45., 10.]]
        );
        // The block starts at 10, and the first line 5 before that.
        assert_eq!(text_rects(&page, "negative"), vec![[5., 20., 20., 10.]]);
    });
}

#[test]
fn text_align_moves_each_line() {
    on_page(|| {
        let page = page(
            r#"<div id="center" style="width:100px;text-align:center">ab</div>
               <div id="right" style="width:100px;text-align:right">ab</div>
               <div id="end" style="width:100px;text-align:end">ab</div>
               <div id="rtl" style="width:100px;direction:rtl">ab</div>"#,
        );
        assert_eq!(text_rects(&page, "center")[0][0], 45.);
        assert_eq!(text_rects(&page, "right")[0][0], 90.);
        assert_eq!(text_rects(&page, "end")[0][0], 90.);
        // Right to left starts at the right edge.
        assert_eq!(text_rects(&page, "rtl")[0][0], 90.);
    });
}

#[test]
fn justify_stretches_the_spaces_of_every_line_but_the_last() {
    on_page(|| {
        let page = page(
            r#"<div id="j" style="width:95px;text-align:justify">aa bb cc dd ee ff gg hh</div>"#,
        );
        // Six words are 85 wide on the first line; the 10 left over goes to five spaces.
        assert_eq!(
            text_rects(&page, "j"),
            vec![[0., 0., 95., 10.], [0., 10., 25., 10.]]
        );
    });
}

#[test]
fn line_height_is_a_number_a_length_a_percentage_or_normal() {
    on_page(|| {
        let page = page(
            r#"<div id="normal">x</div>
               <div id="number" style="line-height:2">x</div>
               <div id="length" style="line-height:30px">x</div>
               <div id="percent" style="line-height:150%">x</div>
               <div id="many" style="line-height:2;width:10px">xx xx xx</div>"#,
        );
        assert_eq!(height_of(&page, "normal"), 10.);
        assert_eq!(height_of(&page, "number"), 20.);
        assert_eq!(height_of(&page, "length"), 30.);
        assert_eq!(height_of(&page, "percent"), 15.);
        assert_eq!(height_of(&page, "many"), 60.);
        // The text sits in the middle of its line: half the leading, 10, above it.
        assert_eq!(text_rects(&page, "length")[0], [0., 40., 5., 10.]);
    });
}

#[test]
fn vertical_align_raises_lowers_and_pins_boxes() {
    on_page(|| {
        let page = page(
            r#"<div id="raised">x<span id="r" style="vertical-align:10px">y</span></div>
               <div id="middle" style="line-height:10px">x<span id="m" style="vertical-align:middle;font-size:20px;line-height:20px">y</span></div>
               <div id="top" style="line-height:10px">x<span id="t" style="vertical-align:top;line-height:30px">y</span></div>"#,
        );
        // Raised 10px: the line grows by 10 above, the span is level with the top.
        assert_eq!(height_of(&page, "raised"), 20.);
        assert_eq!(client_rects(&page, "r"), vec![[5., 0., 5., 10.]]);
        assert_eq!(text_rects(&page, "raised")[0], [0., 10., 5., 10.]);
        // Middle: 3.5 below the baseline of a 10px parent whose x-height is 5.
        assert_eq!(height_of(&page, "middle"), 20.);
        assert_eq!(client_rects(&page, "m"), vec![[5., 20., 10., 20.]]);
        // Top: the line grows to hold the 30px box, and the span's text is in its middle.
        assert_eq!(height_of(&page, "top"), 30.);
        assert_eq!(client_rects(&page, "t"), vec![[5., 50., 5., 10.]]);
    });
}

#[test]
fn an_inline_block_sits_on_the_baseline_of_its_last_line() {
    on_page(|| {
        let page = page(
            r#"<div id="empty">x<span id="e" style="display:inline-block;width:20px;height:30px"></span></div>
               <div id="text">x<span id="t" style="display:inline-block">hello</span></div>"#,
        );
        // Nothing in it: its bottom edge is the baseline, so it reaches 30 above.
        assert_eq!(height_of(&page, "empty"), 32.);
        assert_eq!(page.rect("e"), [5., 0., 20., 30.]);
        assert_eq!(text_rects(&page, "empty")[0][1], 22.);
        // With text in it, its own last line sits on the baseline.
        assert_eq!(height_of(&page, "text"), 10.);
        assert_eq!(page.rect("t"), [5., 32., 25., 10.]);
    });
}

#[test]
fn an_inline_block_shrinks_to_fit_its_content() {
    on_page(|| {
        let page = page(
            r#"<div id="wide" style="width:500px"><span id="a" style="display:inline-block">hello</span></div>
               <div id="narrow" style="width:60px"><span id="b" style="display:inline-block">aaaa bbbb cccc</span></div>
               <div id="fixed" style="width:500px"><span id="c" style="display:inline-block;width:30px">aaaa bbbb</span></div>"#,
        );
        assert_eq!(page.rect("a")[2..], [25., 10.]);
        // Longer than the room: as wide as the room, and the text wraps in it.
        assert_eq!(page.rect("b")[2..], [60., 20.]);
        assert_eq!(height_of(&page, "narrow"), 20.);
        // An explicit width wins, and the text wraps inside it.
        assert_eq!(page.rect("c")[2..], [30., 20.]);
    });
}

#[test]
fn a_float_is_as_wide_as_its_text() {
    on_page(|| {
        let page = page(r#"<div id="f" style="float:left">aaaa bbbb</div>"#);
        assert_eq!(page.rect("f")[2..], [45., 10.]);
    });
}

#[test]
fn an_image_in_a_line_makes_the_line_tall_enough() {
    on_page(|| {
        let page = page(r#"<div id="d">a<img id="i" width="40" height="20">b</div>"#);
        assert_eq!(height_of(&page, "d"), 22.);
        assert_eq!(page.rect("i"), [5., 0., 40., 20.]);
    });
}

#[test]
fn an_inline_element_over_two_lines_has_a_box_on_each() {
    on_page(|| {
        let page = page(r#"<div id="d" style="width:50px">aaa <span id="s">bbb ccc</span></div>"#);
        assert_eq!(
            client_rects(&page, "s"),
            vec![[20., 0., 15., 10.], [0., 10., 15., 10.]]
        );
        // `getBoundingClientRect` is all of them together.
        assert_eq!(page.rect("s"), [0., 0., 35., 20.]);
    });
}

#[test]
fn an_inline_elements_padding_and_border_take_room_in_the_line() {
    on_page(|| {
        let page = page(
            r#"<div id="d">a<span id="s" style="padding:0 5px;border-left:2px solid">b</span>c</div>"#,
        );
        // 'a' is 5 wide, then 2 + 5 of border and padding, 5 of text, 5 of padding.
        assert_eq!(client_rects(&page, "s"), vec![[5., 0., 17., 10.]]);
        let lines = page.engine.inline_layout(page.node("d")).expect("lines");
        assert_eq!(lines.width, 5. + 17. + 5.);
    });
}

#[test]
fn letter_spacing_and_text_transform_change_the_width() {
    on_page(|| {
        let page = page(
            r#"<div id="spaced" style="letter-spacing:2px">abc</div>
               <div id="upper" style="text-transform:uppercase">straße</div>
               <div id="caps" style="text-transform:capitalize">hello world</div>"#,
        );
        assert_eq!(text_rects(&page, "spaced")[0][2], 21.);
        // The selection still maps back to the node's own text: ß is one character of it.
        assert_eq!(text_rects(&page, "upper")[0][2], 35.);
        assert_eq!(text_rects(&page, "caps")[0][2], 55.);
    });
}

#[test]
fn text_overflow_ellipsis_cuts_the_line_and_adds_the_mark() {
    on_page(|| {
        let page = page(
            r#"<div id="d" style="width:30px;overflow:hidden;white-space:nowrap;text-overflow:ellipsis">abcdefgh</div>"#,
        );
        let lines = page.engine.inline_layout(page.node("d")).expect("lines");
        let [line] = lines.lines.as_slice() else {
            panic!("one line");
        };
        let widths: Vec<(f32, f32, bool)> = line
            .fragments
            .iter()
            .filter_map(|fragment| match fragment {
                Fragment::Text(text) => Some((text.x, text.width, text.generated)),
                Fragment::Atomic { .. } => None,
            })
            .collect();
        // "abcde" fills 25, the mark takes the last 5.
        assert_eq!(widths, vec![(0., 25., false), (25., 5., true)]);
    });
}

#[test]
fn line_clamp_keeps_the_first_lines_and_marks_the_last() {
    on_page(|| {
        let page = page(
            r#"<div id="d" style="display:-webkit-box;-webkit-line-clamp:2;overflow:hidden;width:20px">aaaa bbbb cccc dddd</div>"#,
        );
        assert_eq!(height_of(&page, "d"), 20.);
        let lines = page.engine.inline_layout(page.node("d")).expect("lines");
        assert_eq!(lines.lines.len(), 2);
        let last = &lines.lines[1];
        let marked = last
            .fragments
            .iter()
            .any(|fragment| matches!(fragment, Fragment::Text(text) if text.generated));
        assert!(marked, "the last line ends with an ellipsis");
    });
}

#[test]
fn text_and_blocks_in_one_block_take_anonymous_boxes() {
    on_page(|| {
        let page = page(
            r#"<div id="m">top<div id="blk" style="height:5px"></div>bottom</div>
               <div id="gaps">
                   <div id="a" style="height:5px"></div>
                   <div id="b" style="height:5px"></div>
               </div>"#,
        );
        assert_eq!(height_of(&page, "m"), 25.);
        assert_eq!(page.rect("blk")[1], 10.);
        // White space between blocks makes no line.
        assert_eq!(height_of(&page, "gaps"), 10.);
    });
}

#[test]
fn an_inline_element_around_a_block_is_split_by_it() {
    on_page(|| {
        let page = page(
            r#"<div id="d"><span id="s">one<div id="blk" style="height:5px"></div>two</span></div>"#,
        );
        assert_eq!(height_of(&page, "d"), 25.);
        assert_eq!(client_rects(&page, "s").len(), 2);
    });
}

#[test]
fn generated_text_takes_its_place_in_the_line() {
    on_page(|| {
        let page = lay_out(
            &format!(
                "{BASE}<style>#q::before{{content:\"ab\"}}#q::after{{content:\"cd\"}}</style><div id=\"q\">e</div>"
            ),
            "",
        );
        // "ab" before, "e", "cd" after: 5 characters.
        let lines = page.engine.inline_layout(page.node("q")).expect("lines");
        assert_eq!(lines.width, 25.);
        // The node's own text starts after the generated "ab".
        assert_eq!(text_rects(&page, "q"), vec![[10., 0., 5., 10.]]);
    });
}

#[test]
fn a_point_finds_the_nearest_caret_position() {
    on_page(|| {
        let page = page(r#"<div id="d" style="width:200px">hello world</div>"#);
        let node = text_in(&page, "d");
        assert_eq!(page.engine.text_position_at(12., 5.), Some((node, 2)));
        // Past the end of the line, inside the block: the end of the text.
        assert_eq!(page.engine.text_position_at(150., 5.), Some((node, 11)));
        assert_eq!(page.engine.text_position_at(0.5, 5.), Some((node, 0)));
        // Outside every block there is no text to be near.
        assert_eq!(page.engine.text_position_at(-3., 5.), None);
        assert_eq!(page.engine.text_position_at(12., 500.), None);
    });
}

#[test]
fn a_range_covers_the_text_between_its_ends() {
    on_page(|| {
        let page = page(r#"<div id="d" style="width:30px">aaa bbb ccc</div>"#);
        let node = text_in(&page, "d");
        // From the second character to the seventh: the end of line one, and all of line two.
        let rects = page.engine.range_rects((node, 1), (node, 7));
        let boxes: Vec<[f32; 4]> = rects
            .iter()
            .map(|r| [r.x, r.y, r.width, r.height])
            .collect();
        assert_eq!(boxes, vec![[5., 0., 10., 10.], [0., 10., 15., 10.]]);
        // A collapsed range is a caret: a box with no width, at the end of the first line.
        let caret = page.engine.range_rects((node, 3), (node, 3));
        let caret: Vec<[f32; 4]> = caret
            .iter()
            .map(|r| [r.x, r.y, r.width, r.height])
            .collect();
        assert_eq!(caret, vec![[15., 0., 0., 10.]]);
        assert!(page.engine.range_rects((node, 5), (node, 3)).is_empty());
    });
}

#[test]
fn offsets_after_a_preserved_newline_stay_in_their_line() {
    on_page(|| {
        let page = page(
            r#"<div id="d" style="white-space:pre">ab
cd</div>"#,
        );
        let node = text_in(&page, "d");
        // "ab" is two characters wide, not the whole node's five.
        let rects = page.engine.range_rects((node, 0), (node, 2));
        let boxes: Vec<[f32; 4]> = rects
            .iter()
            .map(|r| [r.x, r.y, r.width, r.height])
            .collect();
        assert_eq!(boxes, vec![[0., 0., 10., 10.]]);
        // A click right of "ab" is the end of "ab", before the newline.
        assert_eq!(page.engine.text_position_at(40., 5.), Some((node, 2)));
        assert_eq!(page.engine.text_position_at(40., 15.), Some((node, 5)));
    });
}

#[test]
fn atomic_inlines_break_where_the_block_wraps() {
    on_page(|| {
        let images = r#"<img width="20" height="10"><img width="20" height="10">text"#;
        let page = page(&format!(
            r#"<div id="wrap" style="width:30px">{images}</div>
               <div id="nowrap" style="width:30px;white-space:nowrap">{images}</div>
               <div id="pre" style="width:30px;white-space:pre">{images}</div>"#
        ));
        let count = |id: &str| {
            page.engine
                .inline_layout(page.node(id))
                .expect("lines")
                .lines
                .len()
        };
        assert_eq!(count("wrap"), 3);
        assert_eq!(count("nowrap"), 1);
        assert_eq!(count("pre"), 1);
    });
}

#[test]
fn an_atomic_inline_with_a_percentage_size_is_a_share_of_its_block() {
    on_page(|| {
        let page = page(
            r#"<div style="width:200px"><span id="a" style="display:inline-block;width:50%">x</span><img id="i" style="width:25%;height:10px"></div>
               <div style="width:200px"><img id="j" style="width:100%;height:5px"></div>"#,
        );
        assert_eq!(page.rect("a")[2], 100.);
        assert_eq!(page.rect("i")[2], 50.);
        assert_eq!(page.rect("j")[2], 200.);
    });
}

#[test]
fn an_atomic_inline_with_no_baseline_sits_on_its_bottom_margin_edge() {
    on_page(|| {
        let page = page(
            r#"<div id="m">x<span id="e" style="display:inline-block;width:20px;height:30px;margin-bottom:6px"></span></div>"#,
        );
        // 30 and its margin reach above the baseline, and the text's descent below it.
        assert_eq!(height_of(&page, "m"), 38.);
        assert_eq!(page.rect("e"), [5., 0., 20., 30.]);
        assert_eq!(text_rects(&page, "m")[0][1], 28.);
    });
}

#[test]
fn bolder_is_taken_once_and_a_percentage_line_height_is_of_the_element_that_wrote_it() {
    on_page(|| {
        let page = page(
            r#"<div id="b" style="font-weight:bolder"><span>x</span></div>
               <div style="line-height:200%"><div id="c" style="font-size:30px">x</div></div>"#,
        );
        let lines = page.engine.inline_layout(page.node("b")).expect("lines");
        let Fragment::Text(text) = &lines.lines[0].fragments[0] else {
            panic!("text");
        };
        // 400 and bolder is 700, however many elements it is inherited through.
        assert_eq!(text.run.font, verve_layout::FontKey(700 << 1));
        assert_eq!(height_of(&page, "c"), 20.);
    });
}

#[test]
fn keep_all_keeps_letters_together_but_not_what_follows_punctuation() {
    on_page(|| {
        let page = page(r#"<div id="d" style="width:20px;word-break:keep-all">中中，中中</div>"#);
        assert_eq!(height_of(&page, "d"), 20.);
    });
}

#[test]
fn the_boxes_of_an_element_on_a_justified_line_reach_its_end() {
    on_page(|| {
        let page = page(
            r#"<div id="j" style="width:50px;text-align:justify">aa <span id="s">bb cc dd ee</span></div>"#,
        );
        assert_eq!(
            client_rects(&page, "s"),
            vec![[20., 0., 30., 10.], [0., 10., 25., 10.]]
        );
    });
}

#[test]
fn an_element_split_by_a_block_has_its_edges_on_the_outer_ends_only() {
    on_page(|| {
        let page = page(
            r#"<div id="d"><span id="s" style="padding:0 3px">one<div style="height:5px"></div>two</span></div>"#,
        );
        assert_eq!(
            client_rects(&page, "s"),
            vec![[0., 0., 18., 10.], [0., 15., 18., 10.]]
        );
    });
}

#[test]
fn an_empty_inline_with_padding_makes_a_line() {
    on_page(|| {
        let page = page(
            r#"<div id="e"><span style="padding:0 4px"></span></div><div id="n"><span></span></div>"#,
        );
        assert_eq!(height_of(&page, "e"), 10.);
        assert_eq!(height_of(&page, "n"), 0.);
    });
}

/// Two thousand paragraphs with inline elements and Chinese text, laid out: the timing is read in
/// release (`cargo test --release -p verve-layout --test inline -- --ignored --nocapture`).
#[test]
#[ignore = "a timing, for release builds"]
fn two_thousand_paragraphs_lay_out() {
    on_page(|| {
        let words = "layout of inline text breaks lines at the places the rules allow";
        let paragraph = format!(
            "<p>{words} <a href=\"#\">link text</a> {words} <code>code()</code> {words} <b>bold run</b> {words} 中文排版按字断行，标点不在行首 {words}</p>"
        );
        let page = lay_out(&format!("{BASE}{}", paragraph.repeat(2000)), "");
        let started = std::time::Instant::now();
        let mut engine = LayoutEngine::new(Arc::new(HeadlessMeasurer));
        let update = page.document.update();
        engine.update(&update.snapshot, &update.changes);
        println!("2000 paragraphs: laid out in {:?}", started.elapsed());
        let node = page.snapshot.dom.body;
        assert!(
            engine
                .rect(node)
                .is_some_and(|rect| rect.height > 2000. * 10.)
        );
    });
}
