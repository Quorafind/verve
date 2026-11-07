//! Form controls (P32) with the headless measurer (a character is half an em wide): how they
//! size and stretch, and where their caret and selection are. The Chromium sizes the numbers
//! come from were observed in Electron 39.8.10 (`tests/electron/apps/forms`).

mod common;

use common::{Laid, lay_out, on_page};
use verve_dom::Rect;
use verve_layout::{ControlKind, Fill, PaintItem};

const BASE: &str = "<style>html,body{margin:0}</style>";

fn page(html: &str, script: &str) -> Laid {
    lay_out(&format!("{BASE}{html}"), script)
}

fn close(got: f32, want: f32) -> bool {
    (got - want).abs() < 0.02
}

/// The caret of the field `id`, in the frame its text is in.
fn caret(page: &Laid, id: &str) -> Rect {
    let node = page.node(id);
    page.list()
        .items
        .iter()
        .find_map(|item| match item {
            PaintItem::Control {
                node: n,
                kind: ControlKind::TextField { caret, .. },
                ..
            } if *n == node => *caret,
            _ => None,
        })
        .unwrap_or_else(|| panic!("{id} has no caret"))
}

/// The boxes painted as a selection (the highlight colour), in paint order.
fn highlights(page: &Laid) -> Vec<Rect> {
    page.list()
        .items
        .iter()
        .filter_map(|item| match item {
            PaintItem::Box {
                node: 0,
                bounds,
                background,
                ..
            } if matches!(background.first(), Some(Fill::Color(_))) => Some(*bounds),
            _ => None,
        })
        .collect()
}

#[test]
fn fields_stretch_in_a_column_flexbox_and_a_grid_as_they_did_in_chromium() {
    on_page(|| {
        // Observed in Electron 39.8.10 (stretch-out.json): 400 wide for every one of them, a
        // check box too (393: its margins are 4 and 3).
        let items = "<input id=a><textarea id=b></textarea><select id=c><option>One</select>\
                     <button id=d>Go</button><input id=e type=checkbox>";
        let column = page(
            &format!("<div style='display:flex;flex-direction:column;width:400px'>{items}</div>"),
            "",
        );
        for (id, width, height) in [
            ("a", 400., 21.),
            ("b", 400., 36.),
            ("c", 400., 19.),
            ("d", 400., 19.3333),
            ("e", 393., 13.),
        ] {
            let [_, _, w, h] = column.rect(id);
            assert!(close(w, width) && close(h, height), "{id}: {w}x{h}");
        }
        let grid = page(
            &format!(
                "<div style='display:grid;grid-template-columns:400px;width:400px'>{items}</div>"
            ),
            "",
        );
        assert!(close(grid.rect("a")[2], 400.) && close(grid.rect("c")[2], 400.));
        // In a row that is too short a field keeps its size.
        let row = page(
            "<div style='display:flex;width:100px'><input id=a></div>",
            "",
        );
        assert!(close(row.rect("a")[2], 177.), "{:?}", row.rect("a"));
    });
}

#[test]
fn an_input_keeps_its_own_size_in_flow() {
    on_page(|| {
        let flow = page(
            "<input id=a><input id=b size=5><textarea id=c></textarea>",
            "",
        );
        assert!(close(flow.rect("a")[2], 177.) && close(flow.rect("a")[3], 21.));
        assert!(close(flow.rect("b")[2], 72.), "{:?}", flow.rect("b"));
        // 20 columns of the headless "0" (6.667px), rounded up as Chromium does (134), the 15px
        // bar, and 3px of border and padding on each side.
        assert!(
            close(flow.rect("c")[2], 155.) && close(flow.rect("c")[3], 36.),
            "{:?}",
            flow.rect("c")
        );
    });
}

#[test]
fn a_line_height_makes_a_field_taller() {
    on_page(|| {
        // Observed in Electron 39.8.10: `line-height: 30px` makes the input 36 high.
        let tall = page("<input id=a style='line-height:30px'>", "");
        assert!(close(tall.rect("a")[3], 36.), "{:?}", tall.rect("a"));
    });
}

#[test]
fn a_text_input_stands_on_its_text_and_a_check_box_on_its_bottom() {
    on_page(|| {
        // The headless font has an ascent of 0.8 em: 10.667px at 13.333px. The span's top is its
        // baseline less that; the input's text starts 3px down (2 border, 1 padding).
        let line = page(
            "<span id=s style='font:13.3333px Arial'>Hg</span><input id=a value=Hg>\
             <input id=c type=checkbox>",
            "",
        );
        let baseline = line.rect("s")[1] + 10.6667;
        let input = line.rect("a");
        assert!(
            close(input[1] + 3. + 10.6667, baseline),
            "{input:?} {baseline}"
        );
        // A check box's border box ends on the baseline; its bottom margin hangs below.
        let check = line.rect("c");
        assert!(close(check[1] + check[3], baseline), "{check:?} {baseline}");
    });
}

#[test]
fn the_caret_stands_after_the_characters_before_it() {
    on_page(|| {
        // A character is 6.667 wide at 13.333px; the field's content starts 4px in (2 border,
        // 2 padding). "é" is two bytes and "😀" four, so the caret goes by bytes of the text.
        let field = page(
            "<input id=a value='héllo'>",
            "var a=document.getElementById('a'); a.focus(); a.setSelectionRange(2,2);",
        );
        let at = caret(&field, "a");
        assert!(close(at.x, 4. + 2. * 20. / 3.), "{at:?}");
        assert!(close(at.width, 0.) && close(at.height, 40. / 3.));
        let emoji = page(
            "<input id=a value='a😀b'>",
            "var a=document.getElementById('a'); a.focus(); a.setSelectionRange(3,3);",
        );
        assert!(close(caret(&emoji, "a").x, 4. + 2. * 20. / 3.));
    });
}

#[test]
fn a_password_caret_goes_by_its_bullets() {
    on_page(|| {
        let field = page(
            "<input id=a type=password value='abc'>",
            "var a=document.getElementById('a'); a.focus(); a.setSelectionRange(2,2);",
        );
        assert!(close(caret(&field, "a").x, 4. + 2. * 20. / 3.));
    });
}

#[test]
fn the_caret_after_a_trailing_newline_is_on_the_next_line() {
    on_page(|| {
        let field = page(
            "<textarea id=t></textarea>",
            "var t=document.getElementById('t'); t.value='ab\\n'; t.focus(); t.setSelectionRange(3,3);",
        );
        let at = caret(&field, "t");
        // The content starts 3px in and down (1 border, 2 padding), and a headless line is 13.333 high.
        assert!(close(at.x, 3.) && close(at.y, 3. + 40. / 3.), "{at:?}");
    });
}

#[test]
fn the_caret_after_a_blank_line_is_on_it_and_the_one_after_is_on_the_text() {
    on_page(|| {
        let at = |offset: u32| {
            let script = format!(
                "var t=document.getElementById('t'); t.value='ab\\n\\ncd'; t.focus(); t.setSelectionRange({offset},{offset});"
            );
            caret(&page("<textarea id=t></textarea>", &script), "t")
        };
        let line = 40. / 3.;
        // "ab", a blank line, "cd": the caret after the first break is on the blank line.
        assert!(
            close(at(3).x, 3.) && close(at(3).y, 3. + line),
            "{:?}",
            at(3)
        );
        assert!(
            close(at(4).x, 3.) && close(at(4).y, 3. + 2. * line),
            "{:?}",
            at(4)
        );
        assert!(
            close(at(2).x, 3. + 40. / 3.) && close(at(2).y, 3.),
            "{:?}",
            at(2)
        );
    });
}

#[test]
fn the_text_of_a_field_takes_its_colour_and_letter_spacing() {
    on_page(|| {
        let field = page(
            "<input id=a value='ab' style='color:red;letter-spacing:2px'>",
            "var a=document.getElementById('a'); a.focus(); a.setSelectionRange(2,2);",
        );
        let node = field.node("a");
        let red = field.list().items.iter().any(|item| {
            matches!(item, PaintItem::Glyphs { node: n, color, .. } if *n == node && color[0] > 0.99 && color[1] < 0.01)
        });
        assert!(red, "the glyphs are not red");
        // Two characters of 6.667px and 2px of spacing after each, from 4px in.
        assert!(close(caret(&field, "a").x, 4. + 2. * (20. / 3. + 2.)));
    });
}

#[test]
fn a_selection_over_two_lines_has_a_box_on_each() {
    on_page(|| {
        let field = page(
            "<textarea id=t></textarea>",
            "var t=document.getElementById('t'); t.value='abc\\ndef'; t.focus(); t.setSelectionRange(1,6);",
        );
        let boxes = highlights(&field);
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        // "bc" on the first line and "de" on the second, 6.667 wide a character.
        assert!(close(boxes[0].width, 40. / 3.) && close(boxes[1].width, 40. / 3.));
        assert!(close(boxes[1].y - boxes[0].y, 40. / 3.));
        assert!(close(boxes[0].x, 3. + 20. / 3.) && close(boxes[1].x, 3.));
    });
}

#[test]
fn a_field_without_the_focus_paints_no_selection() {
    on_page(|| {
        let field = page(
            "<input id=a value='abc'><input id=b>",
            "var a=document.getElementById('a'); a.focus(); a.setSelectionRange(0,2); document.getElementById('b').focus();",
        );
        assert!(highlights(&field).is_empty());
    });
}

#[test]
fn an_appearance_none_field_keeps_its_caret() {
    on_page(|| {
        let field = page(
            "<input id=a style='appearance:none' value='abc'>",
            "var a=document.getElementById('a'); a.focus(); a.setSelectionRange(1,1);",
        );
        assert!(close(caret(&field, "a").x, 4. + 20. / 3.));
    });
}

#[test]
fn the_caret_of_a_scrolled_textarea_moves_with_the_scroll() {
    on_page(|| {
        let mut field = page(
            "<textarea id=t rows=2></textarea>",
            "var t=document.getElementById('t'); t.value='1\\n2\\n3\\n4\\n5\\n6'; t.focus(); t.setSelectionRange(11,11);",
        );
        field.scroll_to("t", 0., 1000.);
        let [_, y, _, height] = field.rect("t");
        let node = field.node("t");
        let in_window = field.list().caret_of(node).expect("a caret");
        assert!(
            in_window.y >= y && in_window.y + in_window.height <= y + height + 0.02,
            "{in_window:?} in {y} {height}"
        );
    });
}
