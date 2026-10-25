//! React 18 and react-dom 18, unmodified from npm (`tests/fixtures/node_modules`), rendering
//! into the document and answering what a person does through the input interface.

mod common;

use std::path::PathBuf;

use common::{Page, on_page};
use verve_dom::{Key, Options};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A page with React's production builds and the app in `react-app.js` rendered.
fn app() -> Page {
    let mut page = Page::new(Options::default());
    page.eval("process.env.NODE_ENV = 'production'");
    let path = fixture("react-app.js");
    let source = std::fs::read_to_string(&path).unwrap();
    if let Err(error) = page.runtime.load_module(&path, &source) {
        panic!("the app failed to load: {error}\n{}", page.console());
    }
    page.settle();
    page
}

fn text(page: &Page, selector: &str) -> String {
    let node = page.find(selector);
    page.document.dom().text_content(node)
}

#[test]
fn create_root_renders_state_effects_lists_and_conditions() {
    on_page(|| {
        let mut page = app();
        assert_eq!(text(&page, "#inc"), "count: 0");
        assert_eq!(
            page.eval("document.querySelectorAll('#list > li.todo').length"),
            "2"
        );
        assert_eq!(page.eval("document.title"), "count 0");
        assert_eq!(page.eval("log.join(',')"), "layout true,mount a,mount b");
        // The inline style React set, as the cascade reads it.
        let app = page.find("#app");
        let snapshot = page.document.snapshot();
        assert_eq!(snapshot.style(app, "padding-left"), Some("4px"));
        assert_eq!(snapshot.style(app, "color"), Some("red"));
        assert!(page.find("#maybe") > 0);
        assert_eq!(page.console(), "");
    });
}

#[test]
fn a_press_updates_state_and_renders_again() {
    on_page(|| {
        let mut page = app();
        let inc = page.find("#inc");
        page.document.press(&mut page.runtime, inc);
        page.settle();
        assert_eq!(text(&page, "#inc"), "count: 1");
        page.document.press(&mut page.runtime, inc);
        page.document.press(&mut page.runtime, inc);
        page.settle();
        assert_eq!(text(&page, "#inc"), "count: 3");
        assert_eq!(page.eval("document.title"), "count 3");
        // The same button, not a new one.
        assert_eq!(page.find("#inc"), inc);
        // Conditional rendering.
        let toggle = page.find("#toggle");
        page.document.press(&mut page.runtime, toggle);
        page.settle();
        assert_eq!(page.eval("document.getElementById('maybe')"), "null");
        page.document.press(&mut page.runtime, toggle);
        page.settle();
        assert_eq!(text(&page, "#maybe"), "shown");
        // A keyed list: one taken out, one added.
        let remove = page.find("li[data-item=a] .remove");
        page.document.press(&mut page.runtime, remove);
        page.settle();
        let add = page.find("#add");
        page.document.press(&mut page.runtime, add);
        page.settle();
        assert_eq!(
            page.eval(
                "[...document.querySelectorAll('.todo .label')].map(l => l.textContent).join(',')"
            ),
            "b,c1"
        );
        assert_eq!(page.eval("log.slice(3).join(',')"), "unmount a,mount c1");
        // A checkbox, controlled.
        let check = page.find("#check");
        page.document.press(&mut page.runtime, check);
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('check').checked"),
            "true"
        );
        assert_eq!(
            page.eval("document.getElementById('check').parentNode.textContent"),
            "on"
        );
        assert_eq!(page.console(), "");
    });
}

#[test]
fn typing_into_a_controlled_input_goes_through_react() {
    on_page(|| {
        let mut page = app();
        let field = page.find("#field");
        page.document.press(&mut page.runtime, field);
        assert_eq!(page.document.focused(), Some(field));
        page.document.type_text(&mut page.runtime, "hi");
        page.settle();
        // The app makes what is typed upper case: the field shows what React says.
        assert_eq!(page.eval("document.getElementById('field').value"), "HI");
        assert_eq!(text(&page, "#echo"), "HI");
        page.document
            .key_down(&mut page.runtime, &Key::named("Backspace"));
        page.settle();
        assert_eq!(page.eval("document.getElementById('field').value"), "H");
        assert_eq!(text(&page, "#echo"), "H");
        // An input method's text, at once.
        page.document.insert_text(&mut page.runtime, "ey");
        page.settle();
        assert_eq!(text(&page, "#echo"), "HEY");
        assert_eq!(page.document.draft(field).unwrap().text(), "HEY");
        assert_eq!(page.console(), "");
    });
}

#[test]
fn unmounting_cleans_up() {
    on_page(|| {
        let mut page = app();
        page.eval("unmount()");
        page.settle();
        assert_eq!(
            page.eval("document.getElementById('root').childNodes.length"),
            "0"
        );
        assert_eq!(
            page.eval("log.slice(3).sort().join(',')"),
            "unmount a,unmount b"
        );
        // What React made is let go of, and reclaimed.
        let before = page.document.dom().len();
        let freed = page.document.sweep(&mut page.runtime);
        assert!(freed > 10, "freed {freed}");
        assert_eq!(page.document.dom().len(), before - freed);
        assert_eq!(page.console(), "");
    });
}
