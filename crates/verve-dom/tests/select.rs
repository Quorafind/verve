//! A drop-down `<select>` that is closed: its keys change the choice, and the document decides
//! when its pop-up opens (Electron 39.8.10, hidden windows, a fresh one for each case:
//! `p32c-review/sel3-out.json` and `sel4-out.json`).

mod common;

use common::Page;
use verve_dom::{Key, Modifiers, Options, Pointer};

fn page() -> Page {
    let mut page = Page::new(Options::default());
    page.eval(
        "globalThis.log = [];
         document.body.innerHTML = '<select id=s><option>Apple<option>Banana<option>Bandana<option disabled>Blueberry<option>Cherry<option>Date</select>';
         for (const kind of ['input', 'change', 'keydown', 'keypress', 'mousedown']) {
             document.addEventListener(kind, (e) => log.push(kind + (e.key ? ':' + e.key : '')), true);
         }
         document.getElementById('s').focus();",
    );
    page
}

fn press(page: &mut Page, key: &Key) {
    page.document.key_down(&mut page.runtime, key);
}

fn value(page: &mut Page) -> String {
    page.eval("document.getElementById('s').value")
        .trim_matches('\'')
        .to_owned()
}

fn with(key: &str, modifiers: Modifiers) -> Key {
    Key {
        modifiers,
        ..Key::named(key)
    }
}

const ALT: Modifiers = Modifiers {
    alt: true,
    ctrl: false,
    shift: false,
    meta: false,
};
const CTRL: Modifiers = Modifiers {
    alt: false,
    ctrl: true,
    shift: false,
    meta: false,
};

#[test]
fn page_down_and_page_up_move_three_options_and_skip_the_disabled_one() {
    let mut page = page();
    press(&mut page, &Key::named("PageDown"));
    assert_eq!(value(&mut page), "Cherry");
    press(&mut page, &Key::named("PageDown"));
    assert_eq!(value(&mut page), "Date");
    press(&mut page, &Key::named("Home"));
    press(&mut page, &Key::named("End"));
    press(&mut page, &Key::named("PageUp"));
    assert_eq!(value(&mut page), "Bandana");
}

#[test]
fn letters_typed_in_a_second_make_a_prefix_and_one_letter_again_goes_round() {
    let mut page = page();
    for letter in ['b', 'a', 'n', 'd'] {
        press(&mut page, &Key::char(letter));
    }
    assert_eq!(value(&mut page), "Bandana");
    assert_eq!(
        page.eval("log.filter((k) => k.startsWith('input') || k.startsWith('change')).join(' ')"),
        // b chooses Banana and d Bandana: each change says `input` and `change`.
        "input change input change"
    );
    let mut page = self::page();
    press(&mut page, &Key::char('b'));
    assert_eq!(value(&mut page), "Banana");
    press(&mut page, &Key::char('b'));
    assert_eq!(value(&mut page), "Bandana");
    // A letter on its own (observed in a fresh window: c gives Cherry). Right after b, b it would
    // make the prefix "bbc" instead, which was not observed.
    let mut page = self::page();
    press(&mut page, &Key::char('c'));
    assert_eq!(value(&mut page), "Cherry");
}

#[test]
fn f4_the_alt_arrows_space_and_enter_ask_for_the_pop_up_and_the_page_hears_the_key() {
    let select = |page: &Page| page.find("#s");
    for (name, key) in [
        ("F4", Key::named("F4")),
        ("Alt+Down", with("ArrowDown", ALT)),
        ("Alt+Up", with("ArrowUp", ALT)),
        ("Space", Key::char(' ')),
        ("Enter", Key::named("Enter")),
    ] {
        let mut page = page();
        press(&mut page, &key);
        assert_eq!(
            page.document.take_open_select(),
            Some(select(&page)),
            "{name} asks for it"
        );
        assert!(
            page.eval("log.join(' ')").contains("keydown"),
            "{name} is heard"
        );
        // The choice did not move: the key opened the pop-up instead.
        assert_eq!(value(&mut page), "Apple", "{name}");
    }
}

#[test]
fn ctrl_f4_a_prevented_key_and_a_space_after_letters_do_not_open_it() {
    let mut page = page();
    press(&mut page, &with("F4", CTRL));
    assert_eq!(page.document.take_open_select(), None);
    page.eval("document.addEventListener('keydown', (e) => e.preventDefault())");
    press(&mut page, &Key::named("F4"));
    assert_eq!(page.document.take_open_select(), None);
    let mut page = self::page();
    press(&mut page, &Key::char('b'));
    press(&mut page, &Key::char(' '));
    assert_eq!(page.document.take_open_select(), None);
}

#[test]
fn a_press_opens_the_pop_up_unless_mousedown_was_prevented() {
    let mut page = page();
    let select = page.find("#s");
    page.document
        .pointer_down_on(&mut page.runtime, select, &Pointer::default());
    assert_eq!(page.document.take_open_select(), Some(select));
    page.eval("document.addEventListener('mousedown', (e) => e.preventDefault())");
    page.document
        .pointer_down_on(&mut page.runtime, select, &Pointer::default());
    assert_eq!(page.document.take_open_select(), None);
}

#[test]
fn a_disabled_select_or_a_list_box_asks_for_nothing() {
    let mut page = page();
    page.eval("document.getElementById('s').disabled = true");
    let select = page.find("#s");
    page.document
        .pointer_down_on(&mut page.runtime, select, &Pointer::default());
    assert_eq!(page.document.take_open_select(), None);
    page.eval(
        "document.getElementById('s').disabled = false; document.getElementById('s').size = 3",
    );
    page.document
        .pointer_down_on(&mut page.runtime, select, &Pointer::default());
    assert_eq!(page.document.take_open_select(), None);
}
