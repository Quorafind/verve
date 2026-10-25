//! What a person does, as the document hears it through the input interface: the pointer
//! coming and going (with `:hover` following it), presses and their clicks, double clicks and
//! the other buttons, the wheel, keys and typing, the focus moving, forms submitted — and the
//! boxes the drawer painted, read back by `getBoundingClientRect` and `elementFromPoint`.
//! Ported in part from su-editor's Obsidian host's `tests/input.rs`.

mod common;

use common::{Page, on_page};
use verve_dom::{Key, Modifiers, Options, Pointer, Rect, Request};

fn log(page: &mut Page) -> String {
    let log = page.eval("log.join(' ')");
    page.eval("log.length = 0");
    log
}

fn page() -> Page {
    let mut page = Page::new(Options::default());
    page.eval(
        r#"
        globalThis.log = [];
        document.head.innerHTML = '<style>.button:hover { color: rgb(255, 0, 0) } .button:active { outline-color: blue }</style>';
        document.body.innerHTML = `
          <div id="panel">
            <button id="go" class="button">Go</button>
            <form id="form"><input id="name" name="name"><input id="check" type="checkbox"><label id="label" for="check">tick</label></form>
            <textarea id="notes"></textarea>
          </div>`;
        const go = document.getElementById('go');
        for (const kind of ['pointerover', 'pointerenter', 'mouseover', 'mouseenter', 'mouseleave', 'mouseout', 'mousedown', 'mouseup', 'click', 'dblclick', 'contextmenu', 'auxclick']) {
            go.addEventListener(kind, (e) => log.push(kind + (e.isTrusted ? '' : '?')));
        }
        document.getElementById('panel').addEventListener('mouseenter', () => log.push('panel-enter'));
        document.getElementById('form').addEventListener('submit', (e) => { e.preventDefault(); log.push('submit ' + new FormData(e.target).get('name')); });
        document.getElementById('check').addEventListener('change', (e) => log.push('change ' + e.target.checked));
        window.addEventListener('wheel', (e) => { log.push('wheel ' + e.deltaY); e.preventDefault(); }, { passive: false });
        document.getElementById('name').addEventListener('keydown', (e) => { if (e.key === 'x') e.preventDefault(); });
        "#,
    );
    let dom = page.document.dom();
    let find = |s: &str| dom.query(dom.document, s).unwrap();
    let layout = [
        (find("#panel"), Rect::new(0., 0., 400., 300.)),
        (find("#go"), Rect::new(10., 10., 80., 20.)),
        (find("#name"), Rect::new(10., 40., 200., 20.)),
        (find("#check"), Rect::new(10., 70., 20., 20.)),
        (find("#label"), Rect::new(40., 70., 50., 20.)),
        (find("#notes"), Rect::new(10., 100., 200., 60.)),
    ];
    drop(dom);
    page.document.set_layout(&mut page.runtime, layout);
    page
}

#[test]
fn the_pointer_comes_goes_and_presses() {
    on_page(|| {
        let mut page = page();
        let go = page.find("#go");
        assert!(page.document.hover_sensitive(go));
        // Onto the button: entered from the outside in, `:hover` following.
        let at = Pointer { x: 20., y: 15., ..Pointer::default() };
        page.document.pointer_move(&mut page.runtime, &at);
        assert_eq!(log(&mut page), "pointerover pointerenter mouseover panel-enter mouseenter");
        assert_eq!(page.document.computed().get(go, "color"), Some("rgb(255, 0, 0)"));
        // A press: `:active` while it is down, the click on the way up.
        page.document.pointer_down(&mut page.runtime, &at);
        assert_eq!(page.document.computed().get(go, "outline-color"), Some("blue"));
        page.document.pointer_up(&mut page.runtime, &at);
        assert_ne!(page.document.computed().get(go, "outline-color"), Some("blue"));
        assert_eq!(log(&mut page), "mousedown mouseup click");
        // The second press of a double click.
        let twice = Pointer { count: 2, ..at };
        page.document.pointer_down(&mut page.runtime, &twice);
        page.document.pointer_up(&mut page.runtime, &twice);
        assert_eq!(log(&mut page), "mousedown mouseup click dblclick");
        // The other buttons.
        for button in [1, 2] {
            let other = Pointer { button, ..at };
            page.document.pointer_down(&mut page.runtime, &other);
            page.document.pointer_up(&mut page.runtime, &other);
        }
        assert_eq!(log(&mut page), "mousedown mouseup auxclick mousedown mouseup contextmenu");
        // Away again.
        page.document.pointer_move(&mut page.runtime, &Pointer { x: 300., y: 250., ..Pointer::default() });
        assert_eq!(log(&mut page), "mouseout mouseleave");
        assert_ne!(page.document.computed().get(go, "color"), Some("rgb(255, 0, 0)"));
        // A click a script makes is not trusted.
        page.eval("document.getElementById('go').click()");
        assert_eq!(log(&mut page), "click?");
        // The wheel, prevented by its listener: the drawer should not scroll.
        assert!(!page.document.wheel(&mut page.runtime, &at, 0., 30.));
        assert_eq!(log(&mut page), "wheel 30");
    });
}

#[test]
fn painted_boxes_are_what_geometry_answers() {
    on_page(|| {
        let mut page = page();
        assert_eq!(
            page.eval("(() => { const r = document.getElementById('go').getBoundingClientRect(); return [r.left, r.top, r.width, r.height, r.right, document.getElementById('go').offsetWidth].join(',') })()"),
            "10,10,80,20,90,80"
        );
        assert_eq!(page.eval("document.elementFromPoint(15, 45).id"), "name");
        assert_eq!(page.eval("document.elementFromPoint(300, 200).id"), "panel");
        assert_eq!(page.eval("document.elementFromPoint(900, 900)"), "null");
        assert_eq!(page.document.hit_test(15., 75.), Some(page.find("#check")));
        // A box not measured answers the box round what of it was.
        assert_eq!(page.eval("document.getElementById('form').getBoundingClientRect().height"), "50");
        // Scrolling asked for from code is the drawer's to do.
        page.document.take_requests();
        page.eval("document.getElementById('panel').scrollTop = 40; window.scrollTo(0, 10)");
        let requests = page.document.take_requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert!(matches!(&requests[0], Request::Scroll(verve_dom::ScrollRequest::To { top: Some(t), .. }) if *t == 40.));
        let panel = page.find("#panel");
        page.eval("globalThis.scrolled = 0; document.getElementById('panel').addEventListener('scroll', () => scrolled++)");
        // Where a script scrolled to reads at once, and `scroll` is heard a task later.
        page.settle();
        assert_eq!(page.eval("scrolled + ' ' + document.getElementById('panel').scrollTop"), "1 40");
        // The person scrolled further.
        page.document.set_scroll_offsets(&mut page.runtime, [(panel, [0., 80.])]);
        assert_eq!(page.eval("scrolled + ' ' + document.getElementById('panel').scrollTop"), "2 80");
    });
}

#[test]
fn keys_type_move_the_focus_and_submit() {
    on_page(|| {
        let mut page = page();
        // A press on a field gives it the focus.
        let name = page.find("#name");
        page.document.press(&mut page.runtime, name);
        log(&mut page);
        assert_eq!(page.document.focused(), Some(name));
        assert_eq!(page.eval("document.activeElement.id"), "name");
        page.document.type_text(&mut page.runtime, "Ada");
        // A key its listener prevents types nothing.
        page.document.type_text(&mut page.runtime, "x");
        assert_eq!(page.eval("document.getElementById('name').value"), "Ada");
        // The caret's keys and Backspace.
        page.document.key_down(&mut page.runtime, &Key::named("ArrowLeft"));
        page.document.key_down(&mut page.runtime, &Key::named("Backspace"));
        assert_eq!(page.eval("document.getElementById('name').value"), "Aa");
        assert_eq!(page.eval("document.getElementById('name').selectionStart"), "1");
        page.document.key_down(&mut page.runtime, &Key { modifiers: Modifiers { ctrl: true, ..Modifiers::default() }, ..Key::char('a') });
        page.document.type_text(&mut page.runtime, "Grace");
        assert_eq!(page.eval("document.getElementById('name').value"), "Grace");
        // Enter in a form's field submits it.
        page.document.key_down(&mut page.runtime, &Key::named("Enter"));
        assert_eq!(log(&mut page), "submit Grace");
        // Tab: on to the next thing that takes the focus.
        page.document.key_down(&mut page.runtime, &Key::named("Tab"));
        assert_eq!(page.eval("document.activeElement.id"), "check");
        // Space on a box ticks it; a press on its label does too.
        page.document.key_down(&mut page.runtime, &Key::char(' '));
        page.document.key_up(&mut page.runtime, &Key::char(' '));
        assert_eq!(log(&mut page), "change true");
        let label = page.find("#label");
        page.document.press(&mut page.runtime, label);
        assert_eq!(log(&mut page), "change false");
        // A textarea takes its line breaks.
        let notes = page.find("#notes");
        page.document.focus(&mut page.runtime, Some(notes));
        page.document.type_text(&mut page.runtime, "a\nb");
        assert_eq!(page.eval("JSON.stringify(document.getElementById('notes').value)"), "\"a\\nb\"");
        // Focus events, with what the focus came from.
        page.eval("document.getElementById('name').addEventListener('focus', (e) => log.push('focus from ' + e.relatedTarget.id))");
        page.document.focus(&mut page.runtime, Some(name));
        assert_eq!(log(&mut page), "focus from notes");
        assert_eq!(page.console(), "");
    });
}
