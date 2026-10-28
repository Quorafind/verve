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

/// A page with `react-form.js` rendered.
fn form() -> Page {
    let mut page = Page::new(Options::default());
    page.eval("process.env.NODE_ENV = 'production'");
    let path = fixture("react-form.js");
    let source = std::fs::read_to_string(&path).unwrap();
    if let Err(error) = page.runtime.load_module(&path, &source) {
        panic!("the form failed to load: {error}\n{}", page.console());
    }
    page.settle();
    page
}

#[test]
fn portals_svg_selects_textareas_hover_focus_keys_and_submit() {
    on_page(|| {
        let mut page = form();
        assert_eq!(text(&page, "#status"), "pear|first|none|0");
        // A portal into the body; SVG in its namespace; raw HTML.
        assert_eq!(
            page.eval("document.getElementById('portal').parentNode === document.body"),
            "true"
        );
        assert_eq!(
            page.eval("(() => { const p = document.querySelector('#icon path'); return [p.namespaceURI, p.getAttribute('stroke-width'), p.getAttribute('class'), document.getElementById('icon').getAttribute('viewBox')].join(' ') })()"),
            "http://www.w3.org/2000/svg 2 line 0 0 10 10"
        );
        assert_eq!(
            page.eval("document.getElementById('raw').innerHTML"),
            "<b>bold</b> &amp; plain"
        );
        assert_eq!(page.eval("document.getElementById('fruit').value"), "pear");
        // The layout, reported: a layout effect reads it at the next render.
        let hover = page.find("#hover");
        page.document.set_layout(
            &mut page.runtime,
            [(hover, verve_dom::Rect::new(0., 50., 120., 20.))],
        );
        // A controlled select, chosen.
        let fruit = page.find("#fruit");
        page.document.choose(&mut page.runtime, fruit, "plum");
        page.settle();
        assert_eq!(text(&page, "#status"), "plum|first|none|120");
        // Hover through React's enter and leave, which it makes from mouseover and mouseout.
        page.document.pointer_move(
            &mut page.runtime,
            &verve_dom::Pointer {
                x: 10.,
                y: 60.,
                ..Default::default()
            },
        );
        page.settle();
        assert_eq!(text(&page, "#hover"), "in");
        page.document.pointer_move(
            &mut page.runtime,
            &verve_dom::Pointer {
                x: 500.,
                y: 500.,
                ..Default::default()
            },
        );
        page.settle();
        assert_eq!(text(&page, "#hover"), "out");
        // A controlled textarea: focus, typing, a key handler, blur.
        let notes = page.find("#notes");
        page.document.focus(&mut page.runtime, Some(notes));
        page.settle();
        assert!(text(&page, "#status").contains("|notes|"));
        page.document
            .key_down(&mut page.runtime, &Key::named("End"));
        page.document.type_text(&mut page.runtime, " line\nnext");
        page.document
            .key_down(&mut page.runtime, &Key::named("Escape"));
        page.document.focus(&mut page.runtime, None);
        page.settle();
        assert_eq!(
            text(&page, "#status"),
            "plum|first line\nnext|left notes|120"
        );
        // The submit button submits the form, which React hears and prevents.
        let submit = page.find("#submit");
        page.document.take_requests();
        page.document.press(&mut page.runtime, submit);
        page.settle();
        assert_eq!(
            page.eval("log.join(',')"),
            "escape,submit plum first line\nnext"
        );
        assert!(
            !page
                .document
                .take_requests()
                .iter()
                .any(|r| matches!(r, verve_dom::Request::Submit(_)))
        );
        assert_eq!(page.console(), "");
    });
}
