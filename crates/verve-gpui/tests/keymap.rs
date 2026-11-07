//! GPUI's keys as a page's `KeyboardEvent` has them: every row of P32's table, with the pairs
//! Electron 39.8.10 reports for the same keys.

use gpui::{Keystroke, Modifiers};
use verve_dom::Key;
use verve_gpui::keymap::{dead, key_of, key_with, modifier_changes, modifiers};

fn stroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
    Keystroke {
        modifiers,
        key: key.to_owned(),
        key_char: key_char.map(str::to_owned),
    }
}

fn plain(key: &str) -> Key {
    key_of(&stroke(key, None, Modifiers::default()), false).expect("a key a page hears")
}

fn typing(key: &str, text: &str) -> Key {
    key_of(&stroke(key, Some(text), Modifiers::default()), false).expect("a key a page hears")
}

fn pair(key: &Key) -> (&str, &str) {
    (&key.key, &key.code)
}

#[test]
fn the_keys_that_are_named_keep_their_names() {
    for (name, key, code) in [
        ("enter", "Enter", "Enter"),
        ("tab", "Tab", "Tab"),
        ("backspace", "Backspace", "Backspace"),
        ("delete", "Delete", "Delete"),
        ("escape", "Escape", "Escape"),
        ("space", " ", "Space"),
    ] {
        assert_eq!(pair(&plain(name)), (key, code), "{name}");
    }
}

#[test]
fn the_arrows_and_the_page_keys_are_called_what_the_page_calls_them() {
    for (name, key) in [
        ("left", "ArrowLeft"),
        ("right", "ArrowRight"),
        ("up", "ArrowUp"),
        ("down", "ArrowDown"),
        ("home", "Home"),
        ("end", "End"),
        ("pageup", "PageUp"),
        ("pagedown", "PageDown"),
        ("insert", "Insert"),
    ] {
        assert_eq!(pair(&plain(name)), (key, key), "{name}");
    }
}

#[test]
fn the_function_keys_run_from_f1_to_f24() {
    for number in 1..=24 {
        let name = format!("f{number}");
        let expected = format!("F{number}");
        assert_eq!(pair(&plain(&name)), (&*expected, &*expected), "{name}");
    }
    assert!(key_of(&stroke("f25", None, Modifiers::default()), false).is_none());
    assert!(key_of(&stroke("f0", None, Modifiers::default()), false).is_none());
}

#[test]
fn a_letter_is_its_key_char_and_its_physical_key() {
    for letter in 'a'..='z' {
        let code = format!("Key{}", letter.to_ascii_uppercase());
        let key = typing(&letter.to_string(), &letter.to_string());
        assert_eq!(pair(&key), (&*letter.to_string(), &*code), "{letter}");
    }
}

#[test]
fn shift_makes_the_upper_case_letter_but_keeps_the_physical_key() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    // Observed in Electron 39.8.10: `shift+a` is `A` on `KeyA`.
    let key = key_of(&stroke("a", Some("A"), shift), false).expect("a key");
    assert_eq!(pair(&key), ("A", "KeyA"));
    assert!(key.modifiers.shift && !key.modifiers.ctrl);
    // Without a key char (a chord) the letter is shifted by the map.
    let chord = Modifiers {
        control: true,
        shift: true,
        ..Modifiers::default()
    };
    let key = key_of(&stroke("a", None, chord), false).expect("a key");
    assert_eq!(pair(&key), ("A", "KeyA"));
    assert!(key.modifiers.ctrl && key.modifiers.shift);
}

#[test]
fn control_a_is_a_with_control_held() {
    let control = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    let key = key_of(&stroke("a", None, control), false).expect("a key");
    assert_eq!(pair(&key), ("a", "KeyA"));
    assert!(key.modifiers.ctrl);
}

#[test]
fn a_digit_is_its_character_on_its_digit_key() {
    for digit in '0'..='9' {
        let code = format!("Digit{digit}");
        let key = typing(&digit.to_string(), &digit.to_string());
        assert_eq!(pair(&key), (&*digit.to_string(), &*code), "{digit}");
    }
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    // Observed in Electron 39.8.10: `shift+1` is `!` on `Digit1`.
    let key = key_of(&stroke("1", Some("!"), shift), false).expect("a key");
    assert_eq!(pair(&key), ("!", "Digit1"));
}

#[test]
fn punctuation_has_the_codes_chromium_gives_a_us_keyboard() {
    // Observed in Electron 39.8.10 for each of these keys.
    for (text, code) in [
        ("-", "Minus"),
        ("=", "Equal"),
        ("[", "BracketLeft"),
        ("]", "BracketRight"),
        ("\\", "Backslash"),
        (";", "Semicolon"),
        ("'", "Quote"),
        (",", "Comma"),
        (".", "Period"),
        ("/", "Slash"),
        ("`", "Backquote"),
    ] {
        assert_eq!(pair(&typing(text, text)), (text, code), "{text}");
    }
}

#[test]
fn shifted_punctuation_without_a_key_char_comes_from_the_us_map() {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    for (base, shifted, code) in [
        ("-", "_", "Minus"),
        ("=", "+", "Equal"),
        ("[", "{", "BracketLeft"),
        (";", ":", "Semicolon"),
        ("'", "\"", "Quote"),
        (",", "<", "Comma"),
        (".", ">", "Period"),
        ("/", "?", "Slash"),
        ("`", "~", "Backquote"),
        ("2", "@", "Digit2"),
    ] {
        let key = key_of(&stroke(base, None, shift), false).expect("a key");
        assert_eq!(pair(&key), (shifted, code), "{base}");
    }
}

#[test]
fn a_character_the_us_layout_has_no_key_for_keeps_its_text_and_has_no_code() {
    let key = typing("é", "é");
    assert_eq!(pair(&key), ("é", ""));
}

#[test]
fn a_held_key_is_a_repeat_and_the_windows_key_is_meta() {
    let held = Modifiers {
        platform: true,
        alt: true,
        ..Modifiers::default()
    };
    let key = key_of(&stroke("left", None, held), true).expect("a key");
    assert!(key.repeat);
    assert!(key.modifiers.meta && key.modifiers.alt);
    assert!(
        !key_of(&stroke("left", None, held), false)
            .expect("a key")
            .repeat
    );
    assert!(modifiers(&held).meta);
}

#[test]
fn a_name_that_is_not_a_key_means_nothing_to_a_page() {
    for name in ["shift", "control", "platform", "function", "unknownkey"] {
        assert!(
            key_of(&stroke(name, None, Modifiers::default()), false).is_none(),
            "{name}"
        );
    }
}

#[test]
fn the_context_menu_and_browser_keys_have_their_names() {
    assert_eq!(pair(&plain("menu")), ("ContextMenu", "ContextMenu"));
    assert_eq!(pair(&plain("back")), ("BrowserBack", "BrowserBack"));
    assert_eq!(
        pair(&plain("forward")),
        ("BrowserForward", "BrowserForward")
    );
}

#[test]
fn a_modifier_going_down_or_up_is_the_key_of_that_name_with_the_modifiers_after_it() {
    let none = Modifiers::default();
    let shift = Modifiers {
        shift: true,
        ..none
    };
    // Observed in Electron 39.8.10: `Shift` is on `ShiftLeft`.
    let down = modifier_changes(&none, &shift);
    assert_eq!(down.len(), 1);
    let (is_down, key) = &down[0];
    assert!(*is_down);
    assert_eq!(pair(key), ("Shift", "ShiftLeft"));
    assert!(key.modifiers.shift);
    let up = modifier_changes(&shift, &none);
    assert!(!up[0].0);
    assert!(!up[0].1.modifiers.shift);
    for (modifiers, key, code) in [
        (
            Modifiers {
                control: true,
                ..none
            },
            "Control",
            "ControlLeft",
        ),
        (Modifiers { alt: true, ..none }, "Alt", "AltLeft"),
        (
            Modifiers {
                platform: true,
                ..none
            },
            "Meta",
            "MetaLeft",
        ),
    ] {
        let changes = modifier_changes(&none, &modifiers);
        assert_eq!(pair(&changes[0].1), (key, code));
    }
    // Two at once come in the order Control, Shift, Alt, Meta; none changed is nothing.
    let both = Modifiers {
        control: true,
        alt: true,
        ..none
    };
    let names: Vec<_> = modifier_changes(&none, &both)
        .iter()
        .map(|(_, key)| key.key.clone())
        .collect();
    assert_eq!(names, ["Control", "Alt"]);
    assert!(modifier_changes(&both, &both).is_empty());
}

#[test]
fn shift_is_heard_on_a_shifted_symbol_though_gpui_cleared_it_from_the_keystroke() {
    // gpui_windows turns shift+1 into the key "!" with key_char "!" and no shift in the
    // keystroke (keyboard.rs, rev e679b51); the window still has Shift held.
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    for (text, code) in [
        ("!", "Digit1"),
        (":", "Semicolon"),
        ("+", "Equal"),
        ("?", "Slash"),
    ] {
        let shape = stroke(text, Some(text), Modifiers::default());
        assert!(
            !key_of(&shape, false).expect("a key").modifiers.shift,
            "{text}"
        );
        let key = key_with(&shape, false, &shift).expect("a key");
        assert_eq!(pair(&key), (text, code), "{text}");
        assert!(key.modifiers.shift, "{text}");
    }
}

#[test]
fn a_dead_key_is_called_dead_on_the_code_of_its_key() {
    // The acute accent on a US-International layout: key_char is the accent.
    let accent = key_of(&stroke("'", Some("'"), Modifiers::default()), false).expect("a key");
    let key = dead(accent);
    assert_eq!(pair(&key), ("Dead", "Quote"));
}
