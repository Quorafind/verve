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
        let at = Pointer {
            x: 20.,
            y: 15.,
            ..Pointer::default()
        };
        page.document.pointer_move(&mut page.runtime, &at);
        assert_eq!(
            log(&mut page),
            "pointerover pointerenter mouseover panel-enter mouseenter"
        );
        assert_eq!(
            page.document.computed().get(go, "color"),
            Some("rgb(255, 0, 0)")
        );
        // A press: `:active` while it is down, the click on the way up.
        page.document.pointer_down(&mut page.runtime, &at);
        assert_eq!(
            page.document.computed().get(go, "outline-color"),
            Some("rgb(0, 0, 255)")
        );
        page.document.pointer_up(&mut page.runtime, &at);
        assert_ne!(
            page.document.computed().get(go, "outline-color"),
            Some("rgb(0, 0, 255)")
        );
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
        assert_eq!(
            log(&mut page),
            "mousedown mouseup auxclick mousedown mouseup auxclick contextmenu"
        );
        // Away again.
        page.document.pointer_move(
            &mut page.runtime,
            &Pointer {
                x: 300.,
                y: 250.,
                ..Pointer::default()
            },
        );
        assert_eq!(log(&mut page), "mouseout mouseleave");
        assert_ne!(
            page.document.computed().get(go, "color"),
            Some("rgb(255, 0, 0)")
        );
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
        assert_eq!(
            page.eval("document.getElementById('form').getBoundingClientRect().height"),
            "50"
        );
        // Scrolling asked for from code is the drawer's to do.
        page.document.take_requests();
        page.eval("document.getElementById('panel').scrollTop = 40; window.scrollTo(0, 10)");
        let requests = page.document.take_requests();
        assert_eq!(requests.len(), 2, "{requests:?}");
        assert!(
            matches!(&requests[0], Request::Scroll(verve_dom::ScrollRequest::To { top: Some(t), .. }) if *t == 40.)
        );
        let panel = page.find("#panel");
        page.eval("globalThis.scrolled = 0; document.getElementById('panel').addEventListener('scroll', () => scrolled++)");
        // Where a script scrolled to reads at once, and `scroll` is heard a task later.
        page.settle();
        assert_eq!(
            page.eval("scrolled + ' ' + document.getElementById('panel').scrollTop"),
            "1 40"
        );
        // The person scrolled further.
        page.document
            .set_scroll_offsets(&mut page.runtime, [(panel, [0., 80.])]);
        assert_eq!(
            page.eval("scrolled + ' ' + document.getElementById('panel').scrollTop"),
            "2 80"
        );
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
        page.document
            .key_down(&mut page.runtime, &Key::named("ArrowLeft"));
        page.document
            .key_down(&mut page.runtime, &Key::named("Backspace"));
        assert_eq!(page.eval("document.getElementById('name').value"), "Aa");
        assert_eq!(
            page.eval("document.getElementById('name').selectionStart"),
            "1"
        );
        page.document.key_down(
            &mut page.runtime,
            &Key {
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
                ..Key::char('a')
            },
        );
        page.document.type_text(&mut page.runtime, "Grace");
        assert_eq!(page.eval("document.getElementById('name').value"), "Grace");
        // Enter in a form's field submits it.
        page.document
            .key_down(&mut page.runtime, &Key::named("Enter"));
        assert_eq!(log(&mut page), "submit Grace");
        // Tab: on to the next thing that takes the focus.
        page.document
            .key_down(&mut page.runtime, &Key::named("Tab"));
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
        assert_eq!(
            page.eval("JSON.stringify(document.getElementById('notes').value)"),
            "\"a\\nb\""
        );
        // Focus events, with what the focus came from.
        page.eval("document.getElementById('name').addEventListener('focus', (e) => log.push('focus from ' + e.relatedTarget.id))");
        page.document.focus(&mut page.runtime, Some(name));
        assert_eq!(log(&mut page), "focus from notes");
        assert_eq!(page.console(), "");
    });
}

fn ring(page: &mut Page) -> String {
    page.eval("document.activeElement.id + ':' + document.activeElement.matches(':focus-visible')")
}

fn click_at(page: &mut Page, x: f32, y: f32) {
    let at = Pointer {
        x,
        y,
        count: 1,
        ..Pointer::default()
    };
    page.document.pointer_down(&mut page.runtime, &at);
    page.document.pointer_up(&mut page.runtime, &at);
}

#[test]
fn the_focus_ring_follows_what_the_person_did_as_chromium_shows_it() {
    on_page(|| {
        let mut page = page();
        // Observed in Electron 39.8.10: the pointer on a button shows no ring, on a field it does.
        click_at(&mut page, 20., 15.);
        assert_eq!(ring(&mut page), "go:false");
        click_at(&mut page, 20., 50.);
        assert_eq!(ring(&mut page), "name:true");
        // A script's focus after the pointer: a field shows it, a button does not.
        click_at(&mut page, 20., 15.);
        page.eval("document.getElementById('notes').focus()");
        assert_eq!(ring(&mut page), "notes:true");
        page.eval("document.getElementById('go').focus()");
        assert_eq!(ring(&mut page), "go:false");
        // A key pressed on the button the pointer focused makes the ring show, and Tab shows it.
        page.document
            .key_down(&mut page.runtime, &Key::named("ArrowLeft"));
        assert_eq!(ring(&mut page), "go:true");
        page.document
            .key_down(&mut page.runtime, &Key::named("Tab"));
        assert_eq!(ring(&mut page), "name:true");
        // Observed in Electron 39.8.10 (p32-review focus probes), from a button the mouse
        // focused: Shift alone, Shift+A, F5, Escape and a plain letter show the ring; Control or
        // Alt alone, Ctrl+A, Alt+A and Meta+A do not.
        for (key, shows) in [
            (Key::named("Shift"), true),
            (
                chord(
                    "Control",
                    Modifiers {
                        ctrl: true,
                        ..Modifiers::default()
                    },
                ),
                false,
            ),
            (
                chord(
                    "a",
                    Modifiers {
                        ctrl: true,
                        ..Modifiers::default()
                    },
                ),
                false,
            ),
            (
                chord(
                    "a",
                    Modifiers {
                        alt: true,
                        ..Modifiers::default()
                    },
                ),
                false,
            ),
            (
                chord(
                    "a",
                    Modifiers {
                        meta: true,
                        ..Modifiers::default()
                    },
                ),
                false,
            ),
            (
                chord(
                    "A",
                    Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    },
                ),
                true,
            ),
            (Key::named("Escape"), true),
        ] {
            page.document.focus(&mut page.runtime, None);
            click_at(&mut page, 20., 15.);
            page.document.key_down(&mut page.runtime, &key);
            let expected = format!("go:{shows}");
            assert_eq!(ring(&mut page), expected, "{key:?}");
        }
    });
}

#[test]
fn pointer_events_have_no_click_count_and_the_buttons_held_are_reported() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "globalThis.trace = []; { const target = document.getElementById('go'); \n             for (const kind of ['pointerdown', 'mousedown', 'pointermove', 'pointerup', 'mouseup', 'click', 'dblclick']) \n               target.addEventListener(kind, (e) => trace.push(kind + ':' + e.detail + ':' + e.buttons)); }",
        );
        // Onto the button first, so that what entering logs is not in the way.
        let over = Pointer {
            x: 20.,
            y: 15.,
            ..Pointer::default()
        };
        page.document.pointer_move(&mut page.runtime, &over);
        trace(&mut page);
        let down = Pointer {
            x: 20.,
            y: 15.,
            count: 2,
            buttons: 1,
            ..Pointer::default()
        };
        page.document.pointer_down(&mut page.runtime, &down);
        let drag = Pointer { count: 0, ..down };
        page.document.pointer_move(&mut page.runtime, &drag);
        let up = Pointer { buttons: 0, ..down };
        page.document.pointer_up(&mut page.runtime, &up);
        // Observed in Electron 39.8.10: `detail` is 0 on pointer events and the count on mouse events.
        assert_eq!(
            trace(&mut page),
            "pointerdown:0:1 mousedown:2:1 pointermove:0:1 pointerup:0:0 mouseup:2:0 click:2:0 dblclick:2:0"
        );
    });
}

/// A key `name` with `modifiers` held.
fn chord(name: &str, modifiers: Modifiers) -> Key {
    Key {
        key: name.to_owned(),
        code: name.to_owned(),
        modifiers,
        repeat: false,
    }
}

#[test]
fn moving_events_have_no_detail_and_pointer_ones_no_button_and_cross_in_chromes_order() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "globalThis.trace = []; \n             for (const kind of ['pointerout', 'pointerover', 'mouseout', 'mouseover', 'pointermove', 'mousemove', 'wheel']) \n               document.addEventListener(kind, (e) => trace.push(kind + ':' + e.target.id + ':' + e.detail + ':' + e.button), true);",
        );
        let on_button = Pointer {
            x: 20.,
            y: 15.,
            ..Pointer::default()
        };
        page.document.pointer_move(&mut page.runtime, &on_button);
        trace(&mut page);
        // Observed in Electron 39.8.10 (p32-review focus2 probe): from the button to the field it
        // is `pointerout, pointerover, mouseout, mouseover`; `detail` is 0 on all of them and
        // `button` is -1 on the pointer events and 0 on the mouse ones.
        let on_field = Pointer {
            x: 20.,
            y: 50.,
            ..Pointer::default()
        };
        page.document.pointer_move(&mut page.runtime, &on_field);
        assert_eq!(
            trace(&mut page),
            "pointerout:go:0:-1 pointerover:name:0:-1 mouseout:go:0:0 mouseover:name:0:0 pointermove:name:0:-1 mousemove:name:0:0"
        );
        page.document.wheel(&mut page.runtime, &on_field, 0., 10.);
        assert_eq!(trace(&mut page), "wheel:name:0:0");
    });
}

#[test]
fn a_move_with_a_target_hovers_that_target_whatever_is_painted_under_the_pointer() {
    on_page(|| {
        let mut page = page();
        let go = page.find("#go");
        // The pointer is over the field, but the shell says the button is on top there.
        let at = Pointer {
            x: 20.,
            y: 50.,
            ..Pointer::default()
        };
        page.document.pointer_move_on(&mut page.runtime, go, &at);
        assert_eq!(
            log(&mut page),
            "pointerover pointerenter mouseover panel-enter mouseenter"
        );
    });
}

#[test]
fn a_key_is_unhandled_unless_it_was_prevented_or_did_more_than_type() {
    on_page(|| {
        let mut page = page();
        let name = page.find("#name");
        page.document.focus(&mut page.runtime, Some(name));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // Electron 39.8.10 (menu-order probes): Ctrl+A in an input is its own, Ctrl+B is the
        // menu's; Backspace and the arrows in an input are its own; a typed letter is the menu's
        // as well; the keydown the page prevents is its own.
        for (key, unhandled) in [
            (chord("a", ctrl), false),
            (chord("b", ctrl), true),
            (Key::named("Backspace"), false),
            (Key::named("ArrowLeft"), false),
            (Key::char('q'), true),
            (Key::char('x'), false),
        ] {
            let got = page.document.key_down_unhandled(&mut page.runtime, &key);
            assert_eq!(got, unhandled, "{key:?}");
        }
        // Backspace on the body deletes nothing there, and is the menu's.
        page.document.focus(&mut page.runtime, None);
        let got = page
            .document
            .key_down_unhandled(&mut page.runtime, &Key::named("Backspace"));
        assert!(got);
    });
}

#[test]
fn alt_and_a_letter_types_nothing() {
    on_page(|| {
        let mut page = page();
        let name = page.find("#name");
        page.document.focus(&mut page.runtime, Some(name));
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        page.document.key_down(&mut page.runtime, &chord("a", alt));
        assert_eq!(page.eval("document.getElementById('name').value"), "");
    });
}

/// What the document-level `trace` listeners heard, cleared.
fn trace(page: &mut Page) -> String {
    let heard = page.eval("trace.join(' ')");
    page.eval("trace.length = 0");
    heard
}

#[test]
fn a_paste_and_an_undo_that_changed_something_are_handled_and_one_with_nothing_to_undo_is_not() {
    on_page(|| {
        let mut page = page();
        let name = page.find("#name");
        page.document.focus(&mut page.runtime, Some(name));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // Ctrl+V asks for the clipboard: the keydown's default action is the paste.
        let pasted = page
            .document
            .key_down_unhandled(&mut page.runtime, &chord("v", ctrl));
        assert!(!pasted);
        // Ctrl+Z in an input with nothing to undo does nothing: the menu's (observed in
        // Electron 39.8.10, menu-order probes).
        let undone = page
            .document
            .key_down_unhandled(&mut page.runtime, &chord("z", ctrl));
        assert!(undone);
    });
}

#[test]
fn a_click_away_during_a_composition_ends_it_as_it_stands_and_the_keyboard_works_after() {
    on_page(|| {
        let mut page = page();
        let name = page.find("#name");
        page.document.focus(&mut page.runtime, Some(name));
        page.eval(
            "globalThis.trace = []; for (const kind of ['compositionend', 'blur', 'input', 'keydown']) \
               document.addEventListener(kind, (e) => trace.push(kind + (e.data ? '=' + e.data : '') + (kind === 'keydown' ? ':' + e.key : '')), true);",
        );
        page.document.compose(&mut page.runtime, "jia", None);
        trace(&mut page);
        // Observed in Electron 39.8.10 (p32-review2 `imeblur` probe): the composed text stays,
        // `compositionend` comes before `blur`, and there is no further update or `input`.
        click_at(&mut page, 20., 15.);
        assert_eq!(trace(&mut page), "compositionend=jia blur");
        assert_eq!(page.eval("document.getElementById('name').value"), "jia");
        // The keyboard then works at the new focus: no `Process` key.
        page.document.key_down(&mut page.runtime, &Key::char('x'));
        assert_eq!(trace(&mut page), "keydown:x");
        assert!(!page.document.is_composing());
    });
}

#[test]
fn which_keydowns_are_handled_is_frozen_from_electron_39_8_10() {
    on_page(|| {
        let mut page = page();
        page.eval(
            "document.body.insertAdjacentHTML('beforeend', '<a id=\"lnk\" href=\"#x\">l</a><input id=\"ro\" readonly><div id=\"ed\" contenteditable></div>')",
        );
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // `(target, key, unhandled)`: the coordinator's `probe-keys` `keys`/`keys3` probes put a
        // menu item on each key; unhandled is the item being clicked. Ctrl+A on the body is the
        // one row that differs from Chromium (it selects the page there; verve-dom selects
        // nothing), and is frozen as verve-dom has it.
        let rows: Vec<(Option<&str>, Key, bool)> = vec![
            (Some("#name"), Key::named("Enter"), true),
            (Some("#notes"), Key::named("Enter"), true),
            (Some("#notes"), chord("Enter", ctrl), true),
            (Some("#go"), Key::named("Enter"), true),
            (Some("#go"), Key::char(' '), true),
            (Some("#check"), Key::char(' '), true),
            (Some("#name"), Key::char(' '), true),
            (None, Key::char(' '), true),
            (Some("#check"), Key::named("Backspace"), true),
            (Some("#ro"), Key::named("Backspace"), true),
            (Some("#name"), Key::named("Escape"), true),
            (Some("#lnk"), Key::named("Enter"), false),
            (Some("#name"), Key::named("Tab"), false),
            (Some("#name"), Key::named("Backspace"), false),
            (Some("#name"), Key::named("Delete"), false),
            (Some("#name"), Key::named("ArrowUp"), false),
            (Some("#name"), chord("a", ctrl), false),
            (None, chord("a", ctrl), true),
        ];
        for (selector, key, unhandled) in rows {
            let node = selector.map(|s| page.find(s));
            page.document.focus(&mut page.runtime, node);
            let got = page.document.key_down_unhandled(&mut page.runtime, &key);
            assert_eq!(got, unhandled, "{selector:?} {key:?}");
        }
    });
}

#[test]
fn an_undo_in_an_editing_region_that_has_something_to_undo_is_handled() {
    on_page(|| {
        let mut page = page();
        page.eval("document.body.insertAdjacentHTML('beforeend', '<div id=\"ed\" contenteditable></div>')");
        let ed = page.find("#ed");
        page.document.focus(&mut page.runtime, Some(ed));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // Nothing to undo yet: the menu's. After typing, the undo runs and the key is handled.
        assert!(
            page.document
                .key_down_unhandled(&mut page.runtime, &chord("z", ctrl))
        );
        page.document.key_down(&mut page.runtime, &Key::char('x'));
        assert_eq!(page.eval("document.getElementById('ed').textContent"), "x");
        assert!(
            !page
                .document
                .key_down_unhandled(&mut page.runtime, &chord("z", ctrl))
        );
    });
}

#[test]
fn a_key_taken_by_an_input_method_is_handled() {
    on_page(|| {
        let mut page = page();
        let name = page.find("#name");
        page.document.focus(&mut page.runtime, Some(name));
        page.document.compose(&mut page.runtime, "ni", None);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert!(
            !page
                .document
                .key_down_unhandled(&mut page.runtime, &chord("b", ctrl))
        );
    });
}
