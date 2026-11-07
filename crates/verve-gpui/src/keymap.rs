//! GPUI's keys as the page's `KeyboardEvent` has them.
//!
//! GPUI names a key by its logical name (`"enter"`, `"left"`, `"a"`) and says the text it makes
//! (`key_char`); the page wants `key` (what it types or is called) and `code` (which key on the
//! keyboard). GPUI has no scan code, so `code` is inferred for a US layout: another layout gets
//! the code of the key that makes the same character on a US keyboard, which is not what
//! Chromium reports (a known deviation, recorded in P32's 「状态」).
//!
//! The pairs below are what Electron 39.8.10 reports for the same keys (observed with
//! `webContents.sendInputEvent` on a hidden window).

use gpui::Keystroke;
use verve_dom::{Key, Modifiers};

/// The modifiers a page sees for the ones GPUI says are held. The Windows key (`platform` on
/// Windows) is `meta`, as it is for Chromium.
pub fn modifiers(held: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        ctrl: held.control,
        shift: held.shift,
        alt: held.alt,
        meta: held.platform,
    }
}

/// The key a page hears for `keystroke`, `held` if the system is repeating it. `None` for a key
/// that means nothing to a page (GPUI's own names for a lone modifier).
pub fn key_of(keystroke: &Keystroke, held: bool) -> Option<Key> {
    key_with(keystroke, held, &keystroke.modifiers)
}

/// [`key_of`] with the modifiers `held_now` as the shell tracks them. GPUI's Windows backend
/// clears `shift` from the keystroke when the key is turned into its shifted character (`!`, `:`,
/// `+`), so the page would never hear Shift on those keys: the modifiers come from the window.
pub fn key_with(keystroke: &Keystroke, held: bool, held_now: &gpui::Modifiers) -> Option<Key> {
    let shift = held_now.shift;
    let (key, code) = named(&keystroke.key).or_else(|| typed(keystroke, shift))?;
    Some(Key {
        key,
        code,
        modifiers: modifiers(held_now),
        repeat: held,
    })
}

/// `key` as a dead key (an accent that waits for the letter it goes on): Chromium's `key` for it
/// is `Dead`, on the code of the key that was pressed.
pub fn dead(key: Key) -> Key {
    Key {
        key: "Dead".to_owned(),
        ..key
    }
}

/// The `key` and `code` of a key GPUI names rather than types.
fn named(name: &str) -> Option<(String, String)> {
    let both = |key: &str, code: &str| Some((key.to_owned(), code.to_owned()));
    match name {
        "enter" => both("Enter", "Enter"),
        "tab" => both("Tab", "Tab"),
        "backspace" => both("Backspace", "Backspace"),
        "delete" => both("Delete", "Delete"),
        "escape" => both("Escape", "Escape"),
        "space" => both(" ", "Space"),
        "left" => both("ArrowLeft", "ArrowLeft"),
        "right" => both("ArrowRight", "ArrowRight"),
        "up" => both("ArrowUp", "ArrowUp"),
        "down" => both("ArrowDown", "ArrowDown"),
        "home" => both("Home", "Home"),
        "end" => both("End", "End"),
        "pageup" => both("PageUp", "PageUp"),
        "pagedown" => both("PageDown", "PageDown"),
        "insert" => both("Insert", "Insert"),
        "menu" => both("ContextMenu", "ContextMenu"),
        "back" => both("BrowserBack", "BrowserBack"),
        "forward" => both("BrowserForward", "BrowserForward"),
        _ => {
            let number = name.strip_prefix('f')?.parse::<u8>().ok()?;
            (1..=24).contains(&number).then(|| {
                let function = format!("F{number}");
                (function.clone(), function)
            })
        }
    }
}

/// The `key` and `code` of a key that types a character: the text it made if GPUI says one,
/// else the character with Shift applied to it.
fn typed(keystroke: &Keystroke, shift: bool) -> Option<(String, String)> {
    let mut chars = keystroke.key.chars();
    let base = chars.next().filter(|_| chars.next().is_none())?;
    let key = match keystroke.key_char.as_deref() {
        Some(text) if !text.is_empty() => text.to_owned(),
        _ if shift => shifted(base).to_string(),
        _ => base.to_string(),
    };
    Some((key, code_of(base).to_owned()))
}

/// The character `c` makes with Shift held on a US keyboard.
fn shifted(c: char) -> char {
    match c {
        'a'..='z' => c.to_ascii_uppercase(),
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => c,
    }
}

/// The `code` of the US-layout key that makes `c`, shifted or not; empty for a character no key
/// of that layout makes.
fn code_of(c: char) -> &'static str {
    const LETTERS: [&str; 26] = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK",
        "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    const DIGITS: [&str; 10] = [
        "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
        "Digit9",
    ];
    match c {
        'a'..='z' => LETTERS[c as usize - 'a' as usize],
        'A'..='Z' => LETTERS[c as usize - 'A' as usize],
        '0'..='9' => DIGITS[c as usize - '0' as usize],
        ')' => "Digit0",
        '!' => "Digit1",
        '@' => "Digit2",
        '#' => "Digit3",
        '$' => "Digit4",
        '%' => "Digit5",
        '^' => "Digit6",
        '&' => "Digit7",
        '*' => "Digit8",
        '(' => "Digit9",
        '-' | '_' => "Minus",
        '=' | '+' => "Equal",
        '[' | '{' => "BracketLeft",
        ']' | '}' => "BracketRight",
        '\\' | '|' => "Backslash",
        ';' | ':' => "Semicolon",
        '\'' | '"' => "Quote",
        ',' | '<' => "Comma",
        '.' | '>' => "Period",
        '/' | '?' => "Slash",
        '`' | '~' => "Backquote",
        _ => "",
    }
}

/// The modifier keys that went down or came up between two states, as `(down, key)` in the
/// order Control, Shift, Alt, Meta. A key's own event carries the modifiers as they are after it,
/// so the `keydown` of Shift has `shiftKey` and its `keyup` does not.
pub fn modifier_changes(before: &gpui::Modifiers, after: &gpui::Modifiers) -> Vec<(bool, Key)> {
    let held = modifiers(after);
    let keys = [
        (before.control, after.control, "Control", "ControlLeft"),
        (before.shift, after.shift, "Shift", "ShiftLeft"),
        (before.alt, after.alt, "Alt", "AltLeft"),
        (before.platform, after.platform, "Meta", "MetaLeft"),
    ];
    keys.into_iter()
        .filter(|(was, is, ..)| was != is)
        .map(|(_, is, key, code)| {
            let key = Key {
                key: key.to_owned(),
                code: code.to_owned(),
                modifiers: held,
                repeat: false,
            };
            (is, key)
        })
        .collect()
}
