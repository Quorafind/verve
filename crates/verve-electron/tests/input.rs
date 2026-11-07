//! What a person does to a window, as the page under it hears it: the shell's messages
//! (`ToRendererInput`) injected headless, in the orders and with the fields Electron 39.8.10 shows
//! for the same input (observed with `webContents.sendInputEvent` on a hidden window).

mod support;

use support::{App, TIMEOUT};
use verve_dom::{Key, Modifiers, Pointer};
use verve_electron::msg::{CompositionEvent, FromShell, PageEvent, PointerKind, ToRendererInput};

/// A page of a button at the top left, a text field under it and a box that scrolls sideways.
const PAGE: &str = r#"<style>html,body{margin:0}button,input{display:block;margin:0;padding:0;border:0;width:100px;height:20px}
#l{display:block;height:20px}#s{width:100px;height:40px;overflow:auto}#s div{width:1000px;height:10px}</style>
<button id="b">go</button><input id="i"><a id="l" href="/x">link</a><div id="s"><div></div></div>"#;

/// Records what the page hears: `heard` is a list of `type:target:detail:buttons`-like words.
const LISTEN: &str = r#"globalThis.heard = [];
for (const kind of ['pointerdown', 'mousedown', 'pointerup', 'mouseup', 'click', 'dblclick', 'auxclick', 'contextmenu', 'wheel', 'keydown', 'keypress', 'beforeinput', 'input', 'keyup']) {
  document.addEventListener(kind, (e) => heard.push(kind + (e.target.id ? '#' + e.target.id : '') + (kind.includes('click') || kind === 'contextmenu' || kind.startsWith('mouse') || kind.startsWith('pointer') ? ':' + e.detail + ':' + e.buttons : '')));
}
0"#;

struct Page {
    app: App,
    contents: u32,
}

fn launch(extra: &str) -> Page {
    let page = serde_json::to_string(PAGE).expect("the page is text");
    let main = format!(
        "const {{ BrowserWindow }} = require('electron');
         globalThis.seen = [];
         const win = new BrowserWindow({{ title: 'input', width: 400, height: 300, show: false }});
         for (const name of ['zoom-changed', 'cursor-changed', 'update-target-url']) {{
           win.webContents.on(name, (event, value) => seen.push(name + ':' + value));
         }}
         {extra}
         win.loadURL('data:text/html,' + encodeURIComponent({page}));"
    );
    let app = App::launch_script(&main, &[]);
    let window = app.window("input").expect("the window was made");
    let contents = window.contents;
    let ready = |app: &App| {
        app.eval_renderer(
            contents,
            "document.getElementById('b') ? document.getElementById('b').getBoundingClientRect().width : 0",
        ) == "100"
    };
    assert!(app.run_until(ready, TIMEOUT), "the page was laid out");
    app.eval_renderer(contents, LISTEN);
    Page { app, contents }
}

impl Page {
    fn send(&self, input: ToRendererInput) {
        self.app.hub.input(self.contents, input);
    }

    fn pointer(&self, kind: PointerKind, (x, y): (f32, f32), count: u32, button: i16) {
        let buttons = match (kind, button) {
            (PointerKind::Up, _) => 0,
            (_, 2) => 2,
            (_, 1) => 4,
            _ => 1,
        };
        self.send(ToRendererInput::Pointer {
            kind,
            pointer: Pointer {
                x,
                y,
                button,
                count,
                buttons,
                ..Pointer::default()
            },
            target: None,
        });
    }

    fn key(&self, down: bool, key: &str, code: &str, modifiers: Modifiers) {
        let key = Key {
            key: key.into(),
            code: code.into(),
            modifiers,
            repeat: false,
        };
        self.send(ToRendererInput::Key { down, key });
    }

    fn eval(&self, js: &str) -> String {
        self.app.eval_renderer(self.contents, js)
    }

    /// What the page has heard, once at least `words` words are in, and cleared.
    fn heard(&self, words: usize) -> String {
        let count = |app: &App| {
            app.eval_renderer(self.contents, "heard.length")
                .parse::<usize>()
                .unwrap_or(0)
        };
        assert!(
            self.app.run_until(|app| count(app) >= words, TIMEOUT),
            "the page heard {} of {words} events: {}",
            count(&self.app),
            self.eval("heard.join(' ')")
        );
        let heard = self.eval("heard.splice(0).join(' ')");
        heard.trim_matches('"').to_owned()
    }

    fn main(&self, js: &str) -> String {
        self.app.eval_main(js)
    }
}

#[test]
fn a_click_is_heard_in_chromes_order_and_a_double_click_counts_two() {
    let page = launch("");
    let at = (10., 10.);
    // Electron 39.8.10: pointer events have no click count, mouse events have; `buttons` is 1
    // while the primary button is down and 0 after.
    page.pointer(PointerKind::Down, at, 1, 0);
    page.pointer(PointerKind::Up, at, 1, 0);
    assert_eq!(
        page.heard(5),
        "pointerdown#b:0:1 mousedown#b:1:1 pointerup#b:0:0 mouseup#b:1:0 click#b:1:0"
    );
    page.pointer(PointerKind::Down, at, 2, 0);
    page.pointer(PointerKind::Up, at, 2, 0);
    assert_eq!(
        page.heard(6),
        "pointerdown#b:0:1 mousedown#b:2:1 pointerup#b:0:0 mouseup#b:2:0 click#b:2:0 dblclick#b:2:0"
    );
}

#[test]
fn the_other_buttons_say_auxclick_and_the_secondary_one_asks_for_its_menu() {
    let page = launch("");
    let at = (10., 10.);
    // Electron 39.8.10: middle, `auxclick`; secondary, `auxclick` and then `contextmenu`.
    page.pointer(PointerKind::Down, at, 1, 1);
    page.pointer(PointerKind::Up, at, 1, 1);
    assert_eq!(
        page.heard(5),
        "pointerdown#b:0:4 mousedown#b:1:4 pointerup#b:0:0 mouseup#b:1:0 auxclick#b:1:0"
    );
    page.pointer(PointerKind::Down, at, 1, 2);
    page.pointer(PointerKind::Up, at, 1, 2);
    assert_eq!(
        page.heard(6),
        "pointerdown#b:0:2 mousedown#b:1:2 pointerup#b:0:0 mouseup#b:1:0 auxclick#b:1:0 contextmenu#b:0:0"
    );
}

#[test]
fn shift_a_types_an_upper_case_a_with_its_key_and_code() {
    let page = launch("");
    page.eval(
        "document.addEventListener('keydown', (e) => heard.push('key=' + e.key + ' code=' + e.code + ' shift=' + e.shiftKey)); document.getElementById('i').focus(); 0",
    );
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    page.key(true, "A", "KeyA", shift);
    page.key(false, "A", "KeyA", shift);
    // Electron 39.8.10: keydown, keypress, beforeinput, (textInput), input, keyup.
    let heard = page.heard(6);
    assert!(
        heard.starts_with("keydown#i key=A code=KeyA shift=true keypress#i beforeinput#i input#i"),
        "{heard}"
    );
    assert_eq!(page.eval("document.getElementById('i').value"), "\"A\"");
}

#[test]
fn a_composition_ends_as_one_committed_text_in_the_field() {
    // Electron 39.8.10 (as P24 froze it): compositionstart, compositionupdate, beforeinput and
    // input for the first update; the commit is compositionupdate, beforeinput, input, then
    // compositionend; every input is `insertCompositionText` with `isComposing`, and there is no
    // other `input`.
    let page = launch("");
    page.eval(
        "globalThis.words = []; for (const kind of ['compositionstart', 'compositionupdate', 'compositionend', 'beforeinput', 'input']) { document.addEventListener(kind, (e) => words.push(kind + (e.inputType ? ':' + e.inputType : '') + (e.data ? '=' + e.data : '') + (e.isComposing ? '*' : ''))); } document.getElementById('i').focus(); 0",
    );
    page.send(ToRendererInput::Composition(CompositionEvent::Update {
        text: "ni".into(),
        selection: Some(2..2),
    }));
    page.send(ToRendererInput::Composition(CompositionEvent::Commit(
        "你".into(),
    )));
    let value = |app: &App| app.eval_renderer(page.contents, "document.getElementById('i').value");
    assert!(page.app.run_until(|app| value(app) == "\"你\"", TIMEOUT));
    assert_eq!(
        page.eval("words.join(' ')"),
        "\"compositionstart compositionupdate=ni beforeinput:insertCompositionText=ni* input:insertCompositionText=ni* compositionupdate=你 beforeinput:insertCompositionText=你* input:insertCompositionText=你* compositionend=你\""
    );
}

#[test]
fn tab_with_its_keydown_prevented_leaves_the_focus_where_it_is() {
    let page = launch("");
    page.eval(
        "document.getElementById('i').focus(); document.addEventListener('keydown', (e) => { if (e.key === 'Tab') e.preventDefault(); }); 0",
    );
    let none = Modifiers::default();
    page.key(true, "Tab", "Tab", none);
    page.key(false, "Tab", "Tab", none);
    // Keys go in order: once the keyup has been heard the Tab has been dealt with.
    assert_eq!(page.heard(2), "keydown#i keyup#i");
    assert_eq!(page.eval("document.activeElement.id"), "\"i\"");
}

#[test]
fn tab_moves_the_focus_and_the_ring_shows_after_it_but_not_after_a_click() {
    let page = launch("");
    let ring = || {
        page.eval(
            "document.activeElement.id + ':' + document.activeElement.matches(':focus-visible')",
        )
    };
    let none = Modifiers::default();
    // Electron 39.8.10: a click on a button shows no ring; Tab to the next thing does.
    page.pointer(PointerKind::Down, (10., 10.), 1, 0);
    page.pointer(PointerKind::Up, (10., 10.), 1, 0);
    assert!(page.app.run_until(|_| ring() == "\"b:false\"", TIMEOUT));
    page.key(true, "Tab", "Tab", none);
    page.key(false, "Tab", "Tab", none);
    assert!(page.app.run_until(|_| ring() == "\"i:true\"", TIMEOUT));
    let back = Modifiers {
        shift: true,
        ..none
    };
    page.key(true, "Tab", "Tab", back);
    assert!(page.app.run_until(|_| ring() == "\"b:true\"", TIMEOUT));
}

#[test]
fn the_window_losing_and_gaining_the_keyboard_is_the_documents_focus() {
    let page = launch("");
    let has = |app: &App| app.eval_renderer(page.contents, "document.hasFocus()");
    page.send(ToRendererInput::Focus(false));
    assert!(page.app.run_until(|app| has(app) == "false", TIMEOUT));
    page.send(ToRendererInput::Focus(true));
    assert!(page.app.run_until(|app| has(app) == "true", TIMEOUT));
}

#[test]
fn shift_turns_the_wheel_sideways_and_control_asks_to_zoom_instead_of_scrolling() {
    let page = launch("");
    let over = Pointer {
        x: 10.,
        y: 70.,
        ..Pointer::default()
    };
    // Electron 39.8.10: with Shift the page still hears `deltaY`, and the box scrolls sideways.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    page.send(ToRendererInput::Wheel {
        pointer: Pointer {
            modifiers: shift,
            ..over
        },
        dx: 0.,
        dy: 60.,
        target: None,
    });
    assert_eq!(page.heard(1), "wheel#s");
    let left =
        |app: &App| app.eval_renderer(page.contents, "document.getElementById('s').scrollLeft");
    assert!(page.app.run_until(|app| left(app) == "60", TIMEOUT));
    // With Control: `zoom-changed` ('in' for a wheel turned up, whose page `deltaY` is negative),
    // and nothing scrolls.
    let ctrl = Pointer {
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
        ..over
    };
    for dy in [-100., 100.] {
        page.send(ToRendererInput::Wheel {
            pointer: ctrl,
            dx: 0.,
            dy,
            target: None,
        });
    }
    let seen = |app: &App| app.eval_main("seen.join(' ')");
    assert!(page.app.run_until(
        |app| seen(app) == "\"zoom-changed:in zoom-changed:out\"",
        TIMEOUT
    ));
    assert_eq!(left(&page.app), "60");
}

#[test]
fn a_wheel_the_page_prevents_neither_scrolls_nor_zooms() {
    let page = launch("");
    page.eval(
        "document.addEventListener('wheel', (e) => e.preventDefault(), { passive: false }); 0",
    );
    let ctrl = Pointer {
        x: 10.,
        y: 70.,
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::default()
        },
        ..Pointer::default()
    };
    page.send(ToRendererInput::Wheel {
        pointer: ctrl,
        dx: 0.,
        dy: -100.,
        target: None,
    });
    assert_eq!(page.heard(1), "wheel#s");
    // The page has answered the wheel, so what it would have caused is not waiting behind it.
    assert_eq!(page.main("seen.length"), "0");
}

#[test]
fn what_the_shell_sees_of_the_pointer_becomes_webcontents_events() {
    let page = launch("");
    let contents = page.contents;
    for event in [
        PageEvent::CursorChanged("hand".into()),
        PageEvent::UpdateTargetUrl("https://example.test/x".into()),
        PageEvent::UpdateTargetUrl(String::new()),
        PageEvent::CursorChanged("pointer".into()),
    ] {
        page.app.hub.from_shell(FromShell::Page { contents, event });
    }
    let seen = |app: &App| app.eval_main("seen.join(' ')");
    assert!(page.app.run_until(
        |app| seen(app)
            == "\"cursor-changed:hand update-target-url:https://example.test/x update-target-url: cursor-changed:pointer\"",
        TIMEOUT
    ));
}
