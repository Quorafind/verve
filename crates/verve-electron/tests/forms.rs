//! Form controls as a person uses them, injected headless as the shell's messages would
//! (`ToRendererInput`), in the orders Electron 39.8.10 shows (P32 phase 2).

mod support;

use support::{App, TIMEOUT};
use verve_dom::{Key, Modifiers, Pointer};
use verve_electron::msg::{PointerKind, ToRendererInput};

/// A checkbox at the top left (13 by 13), a select and a text field under it.
const PAGE: &str = r#"<style>html,body{margin:0}input,select{display:block;margin:0}</style>
<input id="c" type="checkbox"><select id="s"><option value="a">A</option><option value="b">B</option></select><input id="t" type="text">"#;

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

    fn click(&self, (x, y): (f32, f32)) {
        for kind in [PointerKind::Down, PointerKind::Up] {
            let buttons = u16::from(kind == PointerKind::Down);
            self.send(ToRendererInput::Pointer {
                kind,
                pointer: Pointer {
                    x,
                    y,
                    count: 1,
                    buttons,
                    ..Pointer::default()
                },
                target: None,
            });
        }
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
        "document.getElementById('s').insertAdjacentHTML('beforeend',          '<optgroup disabled><option value=c>C</option></optgroup><option value=d hidden>D</option>'); 0",
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
