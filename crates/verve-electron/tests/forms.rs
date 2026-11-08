//! Form controls as a person uses them, injected headless as the shell's messages would
//! (`ToRendererInput`), in the orders Electron 39.8.10 shows (P32 phase 2).

mod support;

use support::{App, TIMEOUT};
use verve_dom::{Key, Modifiers, Pointer};
use verve_electron::msg::{PointerKind, ToRendererInput};

/// A checkbox at the top left (13 by 13), a select and a text field under it.
const PAGE: &str = r#"<style>html,body{margin:0}input,select{display:block;margin:0}</style>
<input id="c" type="checkbox"><select id="s"><option value="a">A</option><option value="b">B</option></select><input id="t" type="text">
<input id="f" type="text" value="abcd efgh"><textarea id="ta" rows="3" cols="8">one
two

four</textarea><span id="w" style="font:13.3333px Arial;white-space:pre">abc</span>"#;

const LISTEN: &str = r#"globalThis.heard = [];
for (const kind of ['click', 'input', 'change']) {
  document.addEventListener(kind, (e) => heard.push(kind + '#' + e.target.id));
}
0"#;

struct Page {
    app: App,
    contents: u32,
}

fn launch() -> Page {
    let page = serde_json::to_string(PAGE).expect("the page is text");
    let main = format!(
        "const {{ BrowserWindow }} = require('electron');
         const win = new BrowserWindow({{ title: 'forms', width: 400, height: 300, show: false }});
         win.loadURL('data:text/html,' + encodeURIComponent({page}));"
    );
    let app = App::launch_script(&main, &[]);
    let contents = app.window("forms").expect("the window was made").contents;
    let ready = |app: &App| {
        app.eval_renderer(
            contents,
            "document.getElementById('c') ? document.getElementById('c').getBoundingClientRect().width : 0",
        ) == "13"
    };
    assert!(app.run_until(ready, TIMEOUT), "the page was laid out");
    app.eval_renderer(contents, LISTEN);
    Page { app, contents }
}

impl Page {
    fn send(&self, input: ToRendererInput) {
        self.app.hub.input(self.contents, input);
    }

    /// The renderer's number for the element with this `id`, from the page the shell last drew.
    fn node_of(&self, id: &str) -> verve_dom::NodeId {
        let window = self.app.window("forms").expect("the window").id;
        let snapshot = self
            .app
            .shell
            .snapshot(window)
            .expect("a page was published");
        let dom = &snapshot.dom;
        let found = dom
            .descendants(dom.document)
            .into_iter()
            .find(|node| dom.get_attr(*node, "id").as_deref() == Some(id));
        found.expect("the element is in the page")
    }

    fn eval(&self, js: &str) -> String {
        self.app.eval_renderer(self.contents, js)
    }

    fn click(&self, at: (f32, f32)) {
        self.press(at, 1, false);
    }

    fn key(&self, key: &str, modifiers: Modifiers) {
        for down in [true, false] {
            let key = Key {
                key: key.into(),
                code: String::new(),
                modifiers,
                repeat: false,
            };
            self.send(ToRendererInput::Key { down, key });
        }
    }

    /// The border box of the element with this `id`: left, top, width, height.
    fn rect(&self, id: &str) -> [f32; 4] {
        let js = format!(
            "JSON.stringify((r => [r.x, r.y, r.width, r.height])(document.getElementById('{id}').getBoundingClientRect()))"
        );
        let text: String = serde_json::from_str(&self.eval(&js)).expect("a string");
        serde_json::from_str(&text).expect("four numbers")
    }

    /// One press and release at `(x, y)` with the button count a click number gives.
    fn press(&self, (x, y): (f32, f32), count: u32, shift: bool) {
        let modifiers = Modifiers {
            shift,
            ..Modifiers::default()
        };
        for kind in [PointerKind::Down, PointerKind::Up] {
            let pointer = Pointer {
                x,
                y,
                count,
                buttons: u16::from(kind == PointerKind::Down),
                modifiers,
                ..Pointer::default()
            };
            self.send(ToRendererInput::Pointer {
                kind,
                pointer,
                target: None,
            });
        }
    }

    /// The pointer moved to `(x, y)` with the primary button held.
    fn drag_to(&self, (x, y): (f32, f32)) {
        let pointer = Pointer {
            x,
            y,
            buttons: 1,
            ..Pointer::default()
        };
        self.send(ToRendererInput::Pointer {
            kind: PointerKind::Move,
            pointer,
            target: None,
        });
    }

    /// Where a click lands in the text `chars` characters from the left of the field `id`: the
    /// field's border, padding and the width of that many characters of the page's font, and the
    /// middle of the row. The character width and the height of a line are the twin `<span>`'s,
    /// which the page's own layout gave (Chromium's 15 and 17 are not the headless measurer's).
    fn at_chars(&self, id: &str, chars: usize, row: usize) -> (f32, f32) {
        let [left, top, ..] = self.rect(id);
        let twin = "document.getElementById('w').getBoundingClientRect().width / 3";
        let each: f32 = self.eval(twin).parse().expect("a width");
        let (border, padding) = if id == "ta" { (1., 2.) } else { (2., 2.) };
        let line = self.rect("w")[3];
        (
            left + border + padding + each * chars as f32 + 0.1,
            top + border + padding + line * (row as f32 + 0.5),
        )
    }

    /// Waits until `js` answers `want`.
    fn until(&self, js: &str, want: &str) {
        assert!(
            self.app
                .run_until(|app| app.eval_renderer(self.contents, js) == want, TIMEOUT),
            "{js} was {} and not {want}",
            self.eval(js)
        );
    }
}

#[test]
fn a_checkbox_toggles_when_it_is_clicked() {
    let page = launch();
    page.click((6., 6.));
    page.until("document.getElementById('c').checked", "true");
    page.click((6., 6.));
    page.until("document.getElementById('c').checked", "false");
}

#[test]
fn choosing_an_option_gives_input_then_change_and_choosing_it_again_gives_nothing() {
    let page = launch();
    page.send(ToRendererInput::Choose {
        select: page.node_of("s"),
        index: 1,
    });
    page.until("heard.join(' ')", "\"input#s change#s\"");
    page.eval("heard.length = 0");
    page.send(ToRendererInput::Choose {
        select: page.node_of("s"),
        index: 1,
    });
    // Nothing changed, so nothing is said; a later change shows the first one was not queued.
    page.send(ToRendererInput::Choose {
        select: page.node_of("s"),
        index: 0,
    });
    page.until("heard.join(' ')", "\"input#s change#s\"");
    assert_eq!(page.eval("document.getElementById('s').value"), "\"a\"");
}

#[test]
fn a_disabled_option_or_an_index_past_the_list_chooses_nothing() {
    let page = launch();
    page.eval(
        "document.getElementById('s').insertAdjacentHTML('beforeend', \
         '<optgroup disabled><option value=c>C</option></optgroup><option value=d hidden>D</option>'); 0",
    );
    for index in [2, 3, 9] {
        page.send(ToRendererInput::Choose {
            select: page.node_of("s"),
            index,
        });
    }
    page.send(ToRendererInput::Choose {
        select: page.node_of("s"),
        index: 1,
    });
    page.until("heard.join(' ')", "\"input#s change#s\"");
    assert_eq!(page.eval("document.getElementById('s').value"), "\"b\"");
}

#[test]
fn control_z_after_a_script_set_the_value_keeps_that_value() {
    let page = launch();
    page.eval("document.getElementById('t').focus(); 0");
    for letter in ["a", "b", "c"] {
        page.key(letter, Modifiers::default());
    }
    page.eval("document.getElementById('t').value = 'xyz'; 0");
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    page.key("z", ctrl);
    // Electron 39.8.10: the history went with the old value, and the new one stays.
    page.until("document.getElementById('t').value", "\"xyz\"");
}

#[test]
fn control_z_takes_back_an_input_methods_commits_one_by_one() {
    use verve_electron::msg::CompositionEvent;
    let page = launch();
    page.eval("document.getElementById('t').focus(); 0");
    for letter in ["a", "b"] {
        page.key(letter, Modifiers::default());
    }
    for text in ["加", "载"] {
        page.send(ToRendererInput::Composition(CompositionEvent::Update {
            text: text.into(),
            selection: None,
        }));
        page.send(ToRendererInput::Composition(CompositionEvent::Commit(
            text.into(),
        )));
    }
    page.until("document.getElementById('t').value", "\"ab加载\"");
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    // Electron 39.8.10: "ab加" and then "".
    page.key("z", ctrl);
    page.until("document.getElementById('t').value", "\"ab加\"");
    page.key("z", ctrl);
    page.until("document.getElementById('t').value", "\"\"");
}

#[test]
fn control_z_in_a_field_takes_back_the_typing() {
    let page = launch();
    page.eval("document.getElementById('t').focus(); 0");
    for letter in ["h", "i"] {
        page.key(letter, Modifiers::default());
    }
    page.until("document.getElementById('t').value", "\"hi\"");
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    page.key("z", ctrl);
    page.until("document.getElementById('t').value", "\"\"");
    // Ctrl+Y (redo) brings it back.
    page.key("y", ctrl);
    page.until("document.getElementById('t').value", "\"hi\"");
}

/// Control, its twin, and how the twin's width is taken: as it is, rounded or rounded up.
const TWINS: [(&str, &str, &str); 5] = [
    ("button", "t-button", "exact"),
    ("submit", "t-submit", "exact"),
    ("select", "t-select", "ceil"),
    ("list", "t-list", "exact"),
    ("textarea", "t-textarea", "ceil"),
];
/// The controls of each row of the fixture.
const ROWS: [&[&str]; 5] = [
    &["text", "password", "empty"],
    &["checkbox", "checked", "radio", "radio-on", "range", "color"],
    &["button", "submit", "select", "number"],
    &["progress", "meter", "date", "file"],
    &["textarea", "list"],
];

/// Every control of the fixture `tests/electron/apps/forms` is as big as Electron 39.8.10 made
/// it (`expected-geometry.json`, written by that app's `main.js` at a device scale factor of 1
/// and the language en-US).
///
/// A headless page measures text with `HeadlessMeasurer` (half an em a character), so a size
/// that follows the width of text cannot equal Chromium's. Each of those controls has a twin
/// `<span>` with its text beside it in the fixture, and what is compared is how much bigger the
/// control is than its twin, which does not depend on the measurer. A size that follows the font
/// size alone, and a position within 2px (the headless ascent is not Arial's), are compared
/// directly. Chromium lays out in 1/64 of a pixel.
#[test]
fn the_forms_fixture_has_the_geometry_chromium_gave_it() {
    const FIXTURE: &str = include_str!("../../../tests/electron/apps/forms/index.html");
    const EXPECTED: &str =
        include_str!("../../../tests/electron/apps/forms/expected-geometry.json");
    const LINE_OF_TEXT: [&str; 2] = ["button", "submit"];
    const STANDS_ON_TEXT: [&str; 2] = ["date", "select"];
    const SLACK: f64 = 0.02;
    let page = serde_json::to_string(FIXTURE).expect("the page is text");
    let main = format!(
        "const {{ BrowserWindow }} = require('electron');
         const win = new BrowserWindow({{ title: 'forms', width: 480, height: 270, show: false }});
         win.loadURL('data:text/html,' + encodeURIComponent({page}));"
    );
    let app = App::launch_script(&main, &[]);
    let contents = app.window("forms").expect("the window was made").contents;
    let ask = "JSON.stringify(Object.fromEntries([...document.querySelectorAll('[id]')].map((e) => { const r = e.getBoundingClientRect(); return [e.id, [r.x, r.y, r.width, r.height]]; })))";
    let ready = |app: &App| app.eval_renderer(contents, ask).len() > 100;
    assert!(app.run_until(ready, TIMEOUT), "the page was laid out");
    let ours: String = serde_json::from_str(&app.eval_renderer(contents, ask)).expect("a string");
    let rects = |text: &str| -> std::collections::BTreeMap<String, [f64; 4]> {
        serde_json::from_str(text).expect("rectangles")
    };
    let (ours, theirs) = (rects(&ours), rects(EXPECTED));
    // The top of a row's empty marker is the baseline of the row: how far a control is from it
    // does not depend on where the row is. It is exact for a box that stands on its edge. For one
    // that stands on its text it is off by the difference between the headless ascent (0.8em)
    // and Arial's (about 0.905em, 1.4px at 13.33px) plus Chromium's rounding of the line: 1.83px
    // for the date and the select, so those get 2.5px. A wrong baseline rule is off by 6px or
    // more, which both bounds still catch.
    let marker_gap = |boxes: &std::collections::BTreeMap<String, [f64; 4]>, id: &str, y: f64| {
        let row = ROWS
            .iter()
            .position(|row| row.contains(&id))
            .map_or(1, |at| at + 1);
        (boxes[&format!("m{row}")][1] - y).abs()
    };
    let twin_of = |id: &str| TWINS.iter().find(|(control, ..)| *control == id);
    let y_slack = |id: &str| {
        if STANDS_ON_TEXT.contains(&id) {
            2.5
        } else {
            2.
        }
    };
    let mut wrong = Vec::new();
    for (id, want) in &theirs {
        if id.starts_with("t-") {
            continue;
        }
        let got = ours.get(id).copied().unwrap_or_default();
        let (mut width, mut height) = ((got[2], want[2]), (got[3], want[3]));
        if let Some((_, twin, how)) = twin_of(id) {
            let (Some(here), Some(there)) = (ours.get(*twin).copied(), theirs.get(*twin).copied())
            else {
                wrong.push(format!(
                    "{id}: its twin {twin} is missing from {}",
                    if ours.contains_key(*twin) {
                        "expected-geometry.json"
                    } else {
                        "the page"
                    }
                ));
                continue;
            };
            let take = |size: f64| match *how {
                "ceil" => size.ceil(),
                "round" => size.round(),
                _ => size,
            };
            width = (got[2] - take(here[2]), want[2] - take(there[2]));
            if LINE_OF_TEXT.contains(&id.as_str()) {
                height = (got[3] - here[3], want[3] - there[3]);
            }
        }
        if (width.0 - width.1).abs() > SLACK
            || (height.0 - height.1).abs() > SLACK
            || (marker_gap(&ours, id, got[1]) - marker_gap(&theirs, id, want[1])).abs()
                > y_slack(id)
        {
            wrong.push(format!(
                "{id}: width {:.3} height {:.3} y {:.3}, Chromium {:.3} {:.3} {:.3}",
                width.0, height.0, got[1], width.1, height.1, want[1]
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_click_in_a_field_puts_the_caret_at_the_nearest_boundary() {
    let page = launch();
    page.press(page.at_chars("f", 3, 0), 1, false);
    page.until(
        "document.activeElement.id + document.getElementById('f').selectionStart",
        "\"f3\"",
    );
    // Past the end of the text, and left of the start.
    let [left, top, width, height] = page.rect("f");
    page.press((left + width - 3., top + height / 2.), 1, false);
    page.until("document.getElementById('f').selectionStart", "9");
    page.press((left + 1., top + height / 2.), 1, false);
    page.until("document.getElementById('f').selectionStart", "0");
    // A click in the third line of a text area, which is empty, and at the bottom of its box,
    // which is on the last line it shows ("four", of which x is past the end): the end of that
    // line (Electron 39.8.10, `p32c-review/fld-out.json`).
    page.press(page.at_chars("ta", 1, 2), 1, false);
    page.until("document.getElementById('ta').selectionStart", "8");
    let [tx, ty, tw, th] = page.rect("ta");
    page.press((tx + tw / 2., ty + th - 1.), 1, false);
    page.until("document.getElementById('ta').selectionStart", "13");
}

#[test]
fn a_click_below_the_text_of_an_input_goes_by_x_and_one_on_a_leading_empty_line_is_there() {
    let page = launch();
    // The padding under the row of an input is still its row: 3 characters in is 3, not the end.
    let (x, _) = page.at_chars("f", 3, 0);
    let [_, top, _, height] = page.rect("f");
    page.press((x, top + height - 0.5), 1, false);
    page.until("document.getElementById('f').selectionStart", "3");
    page.press((x, top + 0.5), 1, false);
    page.until("document.getElementById('f').selectionStart", "3");
    // A text area that starts with two empty lines: a click on the second is at 1, not the end.
    page.eval("document.getElementById('ta').value = '\\n\\nabc'; 0");
    page.press(page.at_chars("ta", 2, 1), 1, false);
    page.until("document.getElementById('ta').selectionStart", "1");
    page.press(page.at_chars("ta", 2, 0), 1, false);
    page.until("document.getElementById('ta').selectionStart", "0");
}

#[test]
fn a_mouse_move_does_not_scroll_a_field_back_to_its_caret() {
    let page = launch();
    let lines = "Array.from({ length: 12 }, (_, i) => 'line ' + i).join('\\n')";
    page.eval(&format!(
        "const ta = document.getElementById('ta'); ta.value = {lines}; ta.focus(); ta.setSelectionRange(0, 0); ta.scrollTop = 30; 0"
    ));
    page.until("document.getElementById('ta').scrollTop", "30");
    // Electron 39.8.10: a scrolled text area with the caret out of sight stays so through mouse
    // moves; only typing or the arrows bring the caret back.
    let [x, y, ..] = page.rect("ta");
    page.send(ToRendererInput::Pointer {
        kind: PointerKind::Move,
        pointer: Pointer {
            x: x + 5.,
            y: y + 5.,
            ..Pointer::default()
        },
        target: None,
    });
    assert_eq!(page.eval("document.getElementById('ta').scrollTop"), "30");
    // The caret's line is revealed from the padding box's top (2px of padding).
    page.key("ArrowRight", Modifiers::default());
    page.until("document.getElementById('ta').scrollTop", "2");
}

#[test]
fn double_click_takes_a_word_with_its_spaces_and_shift_click_and_drag_extend() {
    let page = launch();
    page.press(page.at_chars("f", 1, 0), 1, false);
    page.press(page.at_chars("f", 1, 0), 2, false);
    // Electron 39.8.10: the word under the pointer and the space after it.
    page.until("document.getElementById('f').value.slice(document.getElementById('f').selectionStart, document.getElementById('f').selectionEnd)", "\"abcd \"");
    page.press(page.at_chars("f", 1, 0), 1, false);
    page.press(page.at_chars("f", 6, 0), 1, true);
    page.until("document.getElementById('f').selectionStart + ',' + document.getElementById('f').selectionEnd", "\"1,6\"");
    // Triple click selects the whole input; in a text area, the line.
    page.press(page.at_chars("f", 2, 0), 3, false);
    page.until("document.getElementById('f').selectionEnd", "9");
    page.press(page.at_chars("ta", 1, 1), 3, false);
    page.until("document.getElementById('ta').selectionStart + ',' + document.getElementById('ta').selectionEnd", "\"4,8\"");
    // A drag from the second character to beyond the right edge reaches the end.
    let from = page.at_chars("f", 1, 0);
    let [left, _, width, _] = page.rect("f");
    page.send(ToRendererInput::Pointer {
        kind: PointerKind::Down,
        pointer: Pointer {
            x: from.0,
            y: from.1,
            count: 1,
            buttons: 1,
            ..Pointer::default()
        },
        target: None,
    });
    page.drag_to(page.at_chars("f", 4, 0));
    page.until("document.getElementById('f').selectionStart + ',' + document.getElementById('f').selectionEnd", "\"1,4\"");
    page.drag_to((left + width + 40., from.1 + 30.));
    page.until("document.getElementById('f').selectionEnd", "9");
}

#[test]
fn typing_past_the_edge_scrolls_the_field_to_the_caret_and_home_scrolls_back() {
    let page = launch();
    page.press(page.at_chars("f", 9, 0), 1, false);
    page.send(ToRendererInput::Text("0123456789".repeat(8)));
    page.until("document.getElementById('f').scrollLeft > 0", "true");
    // The caret is at the right edge, with the paddings after it: 89 characters in, less the
    // field's client width. (`scrollWidth` is rounded and `scrollLeft` is not, so they are not
    // compared.) The client width is the border box less its 2px borders, taken by hand:
    // verve-dom's `clientWidth` is still the border box for every element, a bug of its own.
    page.until(
        "(e => Math.abs(e.scrollLeft - (89 * document.getElementById('w').getBoundingClientRect().width / 3 + 4 - (e.getBoundingClientRect().width - 4))) < 1)(document.getElementById('f'))",
        "true",
    );
    page.key("Home", Modifiers::default());
    page.until("document.getElementById('f').scrollLeft", "0");
    // A text area follows the caret down its lines.
    page.press(page.at_chars("ta", 0, 0), 1, false);
    for _ in 0..8 {
        page.key("Enter", Modifiers::default());
    }
    page.until("document.getElementById('ta').scrollTop > 0", "true");
}

#[test]
fn the_keys_of_a_closed_select_change_its_choice_at_once() {
    let page = launch();
    page.eval("document.getElementById('s').focus(); 0");
    let value = "document.getElementById('s').value";
    // Electron 39.8.10: Down, Up, End, Home and a letter each change the value with `input` and
    // `change`, and Home on the first option changes nothing.
    page.key("ArrowDown", Modifiers::default());
    page.until(value, "\"b\"");
    page.until("heard.join(' ')", "\"input#s change#s\"");
    page.key("ArrowUp", Modifiers::default());
    page.until(value, "\"a\"");
    page.key("End", Modifiers::default());
    page.until(value, "\"b\"");
    page.key("a", Modifiers::default());
    page.until(value, "\"a\"");
    // Home and Up on the first option change nothing, and say nothing.
    page.eval("heard.length = 0");
    page.key("Home", Modifiers::default());
    page.key("ArrowUp", Modifiers::default());
    assert_eq!(page.eval("heard.join(' ')"), "\"\"");
    // Page Down is three options on, which is the end of two.
    page.key("PageDown", Modifiers::default());
    page.until(value, "\"b\"");
}
