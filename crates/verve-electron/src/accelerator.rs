//! `Accelerator`: Electron's keyboard shortcut strings (`"CommandOrControl+Shift+Z"`), parsed.
//!
//! Electron's documentation (`docs/api/accelerator.md`, 39.x) lists the modifiers and the keys;
//! Chromium's parser reads them case-insensitively, takes the last key among the modifiers,
//! and reads a shifted symbol (`!`, `:`, `{`) as the shift modifier and the key that types it
//! on a US keyboard. The result names the key as a W3C `KeyboardEvent.code` (`"KeyA"`,
//! `"Digit1"`, `"F5"`), which is what `muda` and `global-hotkey` take, what a [`verve_dom::Key`]
//! carries, and what the shell compares a pressed key by.
//!
//! The parse depends on the system only in `CommandOrControl`, which is Command on macOS and
//! Control elsewhere; [`Accelerator::parse_for`] takes the system, [`Accelerator::parse`] is the
//! one the program runs on. Whitespace around a `+` is ignored, and `Plus` is `Shift+=`.

use std::fmt;

/// Which system `CommandOrControl` is read for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum System {
    /// macOS: `CommandOrControl` is Command.
    Mac,
    /// Windows and Linux: `CommandOrControl` is Control.
    Other,
}

impl System {
    /// The system this program was built for.
    pub const CURRENT: System = if cfg!(target_os = "macos") {
        System::Mac
    } else {
        System::Other
    };
}

/// The modifier keys of an accelerator. Command, `Super` and `Meta` are one key: the Windows key
/// off macOS, Command on it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    /// `AltGr`, which is Control and Alt held together on Windows.
    pub alt_gr: bool,
}

/// The key of an accelerator, as its W3C `KeyboardEvent.code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccelKey(&'static str);

impl AccelKey {
    /// `"KeyA"`, `"Digit1"`, `"F5"`, `"ArrowUp"`, `"Numpad0"`…
    pub fn code(self) -> &'static str {
        self.0
    }
}

/// A shortcut: the modifiers held and the key pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accelerator {
    pub mods: Mods,
    pub key: AccelKey,
}

/// A string that is no accelerator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceleratorError {
    /// The part that could not be read.
    pub token: String,
}

impl fmt::Display for AcceleratorError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "invalid accelerator: {:?}", self.token)
    }
}

impl std::error::Error for AcceleratorError {}

const LETTERS: [&str; 26] = [
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL",
    "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX",
    "KeyY", "KeyZ",
];

const DIGITS: [&str; 10] = [
    "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
    "Digit9",
];

const NUMPAD: [&str; 10] = [
    "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4", "Numpad5", "Numpad6", "Numpad7",
    "Numpad8", "Numpad9",
];

const FUNCTION: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

/// The keys that are named in words, lower-cased, and their codes.
const NAMED: &[(&str, &str)] = &[
    ("space", "Space"),
    ("tab", "Tab"),
    ("capslock", "CapsLock"),
    ("numlock", "NumLock"),
    ("scrolllock", "ScrollLock"),
    ("backspace", "Backspace"),
    ("delete", "Delete"),
    ("insert", "Insert"),
    ("return", "Enter"),
    ("enter", "Enter"),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("home", "Home"),
    ("end", "End"),
    ("pageup", "PageUp"),
    ("pagedown", "PageDown"),
    ("escape", "Escape"),
    ("esc", "Escape"),
    ("volumeup", "AudioVolumeUp"),
    ("volumedown", "AudioVolumeDown"),
    ("volumemute", "AudioVolumeMute"),
    ("medianexttrack", "MediaTrackNext"),
    ("mediaprevioustrack", "MediaTrackPrevious"),
    ("mediastop", "MediaStop"),
    ("mediaplaypause", "MediaPlayPause"),
    ("printscreen", "PrintScreen"),
    ("numdec", "NumpadDecimal"),
    ("numadd", "NumpadAdd"),
    ("numsub", "NumpadSubtract"),
    ("nummult", "NumpadMultiply"),
    ("numdiv", "NumpadDivide"),
];

/// The punctuation that is typed unshifted, and its code.
const PUNCTUATION: &[(char, &str)] = &[
    (';', "Semicolon"),
    ('=', "Equal"),
    (',', "Comma"),
    ('-', "Minus"),
    ('.', "Period"),
    ('/', "Slash"),
    ('`', "Backquote"),
    ('[', "BracketLeft"),
    ('\\', "Backslash"),
    (']', "BracketRight"),
    ('\'', "Quote"),
];

/// The symbols typed with Shift on a US keyboard, and the unshifted key that types each.
const SHIFTED: &[(char, &str)] = &[
    (')', "Digit0"),
    ('!', "Digit1"),
    ('@', "Digit2"),
    ('#', "Digit3"),
    ('$', "Digit4"),
    ('%', "Digit5"),
    ('^', "Digit6"),
    ('&', "Digit7"),
    ('*', "Digit8"),
    ('(', "Digit9"),
    (':', "Semicolon"),
    ('<', "Comma"),
    ('_', "Minus"),
    ('>', "Period"),
    ('?', "Slash"),
    ('~', "Backquote"),
    ('{', "BracketLeft"),
    ('|', "Backslash"),
    ('}', "BracketRight"),
    ('"', "Quote"),
];

/// What one `+`-separated part of an accelerator is.
enum Part {
    Modifier(fn(&mut Mods, System)),
    Key(&'static str, bool),
}

fn part_of(token: &str) -> Option<Part> {
    let lower = token.to_ascii_lowercase();
    let modifier: Option<fn(&mut Mods, System)> = match lower.as_str() {
        "command" | "cmd" | "super" | "meta" => Some(|mods, _| mods.meta = true),
        "control" | "ctrl" => Some(|mods, _| mods.ctrl = true),
        "commandorcontrol" | "cmdorctrl" => Some(|mods, system| match system {
            System::Mac => mods.meta = true,
            System::Other => mods.ctrl = true,
        }),
        "alt" | "option" => Some(|mods, _| mods.alt = true),
        "altgr" => Some(|mods, _| mods.alt_gr = true),
        "shift" => Some(|mods, _| mods.shift = true),
        _ => None,
    };
    if let Some(modifier) = modifier {
        return Some(Part::Modifier(modifier));
    }
    key_of(token, &lower).map(|(code, shifted)| Part::Key(code, shifted))
}

/// The code a key token names, and whether typing it takes Shift.
fn key_of(token: &str, lower: &str) -> Option<(&'static str, bool)> {
    // `Plus` is the key that types `+`, which takes Shift on a US keyboard.
    if lower == "plus" {
        return Some(("Equal", true));
    }
    if let Some((_, code)) = NAMED.iter().find(|(name, _)| *name == lower) {
        return Some((code, false));
    }
    if let Some(number) = lower.strip_prefix('f').filter(|n| !n.starts_with('0'))
        && let Ok(number) = number.parse::<usize>()
    {
        return FUNCTION
            .get(number.wrapping_sub(1))
            .map(|code| (*code, false));
    }
    if let Some(number) = lower.strip_prefix("num").filter(|n| n.len() == 1)
        && let Ok(number) = number.parse::<usize>()
    {
        return NUMPAD.get(number).map(|code| (*code, false));
    }
    let mut chars = token.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if c.is_ascii_alphabetic() {
        return Some((
            LETTERS[usize::from(c.to_ascii_uppercase() as u8 - b'A')],
            false,
        ));
    }
    if c.is_ascii_digit() {
        return Some((DIGITS[usize::from(c as u8 - b'0')], false));
    }
    if let Some((_, code)) = PUNCTUATION.iter().find(|(p, _)| *p == c) {
        return Some((code, false));
    }
    SHIFTED
        .iter()
        .find(|(p, _)| *p == c)
        .map(|(_, code)| (*code, true))
}

impl Accelerator {
    /// Reads `text` as the system this program runs on reads it.
    ///
    /// # Errors
    ///
    /// The part of `text` that is neither a modifier nor a key, or no key at all.
    pub fn parse(text: &str) -> Result<Self, AcceleratorError> {
        Self::parse_for(text, System::CURRENT)
    }

    /// Reads `text` with `CommandOrControl` meaning what it does on `system`.
    ///
    /// # Errors
    ///
    /// As [`Accelerator::parse`].
    pub fn parse_for(text: &str, system: System) -> Result<Self, AcceleratorError> {
        let bad = |token: &str| AcceleratorError {
            token: token.to_owned(),
        };
        let mut mods = Mods::default();
        let mut key: Option<&'static str> = None;
        for token in text.split('+').map(str::trim) {
            match part_of(token).ok_or_else(|| bad(token))? {
                Part::Modifier(apply) => apply(&mut mods, system),
                // Several keys are accepted and the last one is the accelerator's. This is read from
                // Chromium's source (Electron 39's accelerator parser), not observed.
                Part::Key(code, shifted) => {
                    key = Some(code);
                    mods.shift |= shifted;
                }
            }
        }
        key.map(|code| Accelerator {
            mods,
            key: AccelKey(code),
        })
        .ok_or_else(|| bad(text))
    }

    /// Whether the pressed `key` is this accelerator: the same modifiers, held and no others, and
    /// the same key. The key is compared by its `code`; one that has none is compared by what it
    /// types.
    pub fn matches(&self, key: &verve_dom::Key) -> bool {
        let held = key.modifiers;
        let want = self.mods;
        let ctrl = want.ctrl || want.alt_gr;
        let alt = want.alt || want.alt_gr;
        if held.ctrl != ctrl
            || held.alt != alt
            || held.shift != want.shift
            || held.meta != want.meta
        {
            return false;
        }
        if key.code.is_empty() {
            return self.types(&key.key);
        }
        key.code == self.key.code()
    }

    /// Whether `typed` — a `KeyboardEvent.key` — is what the key of this accelerator types.
    fn types(&self, typed: &str) -> bool {
        let code = self.key.code();
        let letter = code.strip_prefix("Key").filter(|rest| rest.len() == 1);
        let digit = code.strip_prefix("Digit");
        let wanted = letter.or(digit).map(str::to_ascii_lowercase);
        match wanted {
            Some(wanted) => typed.eq_ignore_ascii_case(&wanted),
            None => typed == code,
        }
    }
}

impl fmt::Display for Accelerator {
    /// The accelerator as the shell's menus write it: `Ctrl+Shift+KeyZ`.
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mods = self.mods;
        let named = [
            (mods.ctrl, "Ctrl"),
            (mods.alt, "Alt"),
            (mods.alt_gr, "AltGr"),
            (mods.shift, "Shift"),
            (mods.meta, "Meta"),
        ];
        for (held, name) in named {
            if held {
                write!(out, "{name}+")?;
            }
        }
        write!(out, "{}", self.key.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Accelerator {
        Accelerator::parse_for(text, System::Other).expect(text)
    }

    #[test]
    fn modifiers_are_read_in_every_alias_and_any_case() {
        let ctrl = parse("ctrl+a");
        assert!(ctrl.mods.ctrl && !ctrl.mods.meta);
        assert_eq!(parse("CONTROL+A"), ctrl);
        assert_eq!(parse("CommandOrControl+A"), ctrl);
        assert_eq!(parse("cmdorctrl+a"), ctrl);
        let command = parse("Cmd+A");
        assert!(command.mods.meta && !command.mods.ctrl);
        assert_eq!(parse("Command+A"), command);
        assert_eq!(parse("Super+A"), command);
        assert_eq!(parse("Meta+A"), command);
        assert_eq!(parse("Option+A"), parse("Alt+A"));
        assert!(parse("AltGr+A").mods.alt_gr);
        assert!(parse("shift+a").mods.shift);
    }

    #[test]
    fn command_or_control_is_command_on_a_mac() {
        let mac = Accelerator::parse_for("CmdOrCtrl+Q", System::Mac).unwrap();
        assert!(mac.mods.meta && !mac.mods.ctrl);
    }

    #[test]
    fn every_key_name_is_read() {
        let keys = [
            ("A", "KeyA"),
            ("z", "KeyZ"),
            ("0", "Digit0"),
            ("9", "Digit9"),
            ("F1", "F1"),
            ("f24", "F24"),
            ("Space", "Space"),
            ("Tab", "Tab"),
            ("Capslock", "CapsLock"),
            ("Numlock", "NumLock"),
            ("Scrolllock", "ScrollLock"),
            ("Backspace", "Backspace"),
            ("Delete", "Delete"),
            ("Insert", "Insert"),
            ("Return", "Enter"),
            ("Enter", "Enter"),
            ("Up", "ArrowUp"),
            ("Down", "ArrowDown"),
            ("Left", "ArrowLeft"),
            ("Right", "ArrowRight"),
            ("Home", "Home"),
            ("End", "End"),
            ("PageUp", "PageUp"),
            ("PageDown", "PageDown"),
            ("Escape", "Escape"),
            ("Esc", "Escape"),
            ("VolumeUp", "AudioVolumeUp"),
            ("VolumeDown", "AudioVolumeDown"),
            ("VolumeMute", "AudioVolumeMute"),
            ("MediaNextTrack", "MediaTrackNext"),
            ("MediaPreviousTrack", "MediaTrackPrevious"),
            ("MediaStop", "MediaStop"),
            ("MediaPlayPause", "MediaPlayPause"),
            ("PrintScreen", "PrintScreen"),
            ("num0", "Numpad0"),
            ("num9", "Numpad9"),
            ("numdec", "NumpadDecimal"),
            ("numadd", "NumpadAdd"),
            ("numsub", "NumpadSubtract"),
            ("nummult", "NumpadMultiply"),
            ("numdiv", "NumpadDivide"),
            (";", "Semicolon"),
            ("=", "Equal"),
            (",", "Comma"),
            ("-", "Minus"),
            (".", "Period"),
            ("/", "Slash"),
            ("`", "Backquote"),
            ("[", "BracketLeft"),
            ("\\", "Backslash"),
            ("]", "BracketRight"),
            ("'", "Quote"),
        ];
        for (name, code) in keys {
            let found = parse(&format!("Ctrl+{name}"));
            assert_eq!(found.key.code(), code, "{name}");
            assert!(!found.mods.shift, "{name}");
        }
    }

    #[test]
    fn plus_is_the_shifted_equal_key_and_spaces_around_a_plus_are_ignored() {
        let plus = parse("CmdOrCtrl+Plus");
        assert_eq!(plus.key.code(), "Equal");
        assert!(plus.mods.shift && plus.mods.ctrl);
        assert_eq!(parse(" Ctrl + Shift + a "), parse("Ctrl+Shift+A"));
        assert_eq!(parse("Ctrl+A+B"), parse("Ctrl+B"));
    }

    #[test]
    fn a_shifted_symbol_is_shift_and_the_key_that_types_it() {
        for (symbol, code) in SHIFTED {
            let found = parse(&format!("Ctrl+{symbol}"));
            assert_eq!(found.key.code(), *code, "{symbol}");
            assert!(found.mods.shift, "{symbol}");
        }
    }

    #[test]
    fn strings_that_are_no_accelerator_are_refused() {
        for text in [
            "",
            "Ctrl",
            "Ctrl+",
            "+A",
            "NotAKey",
            "F25",
            "F0",
            "num10",
            "Ctrl+Shift",
            "A B",
            "Ctrl+Foo",
        ] {
            assert!(
                Accelerator::parse_for(text, System::Other).is_err(),
                "{text:?}"
            );
        }
        assert_eq!(
            Accelerator::parse("Ctrl+Foo").unwrap_err().token,
            "Foo".to_owned()
        );
    }

    fn pressed(code: &str, key: &str, mods: Mods) -> verve_dom::Key {
        verve_dom::Key {
            key: key.to_owned(),
            code: code.to_owned(),
            modifiers: verve_dom::Modifiers {
                ctrl: mods.ctrl,
                shift: mods.shift,
                alt: mods.alt,
                meta: mods.meta,
            },
            repeat: false,
        }
    }

    #[test]
    fn a_key_matches_with_exactly_the_modifiers_of_the_accelerator() {
        let open = parse("Ctrl+O");
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert!(open.matches(&pressed("KeyO", "o", ctrl)));
        assert!(!open.matches(&pressed("KeyO", "o", Mods::default())));
        let more = Mods {
            shift: true,
            ..ctrl
        };
        assert!(!open.matches(&pressed("KeyO", "O", more)));
        assert!(!open.matches(&pressed("KeyP", "p", ctrl)));
    }

    #[test]
    fn a_key_without_a_code_is_matched_by_what_it_types() {
        let open = parse("Ctrl+O");
        let ctrl = Mods {
            ctrl: true,
            ..Mods::default()
        };
        assert!(open.matches(&pressed("", "o", ctrl)));
        assert!(open.matches(&pressed("", "O", ctrl)));
        assert!(!open.matches(&pressed("", "p", ctrl)));
        assert!(parse("Ctrl+Enter").matches(&pressed("", "Enter", ctrl)));
    }

    #[test]
    fn alt_gr_is_control_and_alt_held_together() {
        let combo = Mods {
            ctrl: true,
            alt: true,
            ..Mods::default()
        };
        assert!(parse("AltGr+Q").matches(&pressed("KeyQ", "q", combo)));
    }

    #[test]
    fn an_accelerator_is_written_with_its_modifiers_first() {
        assert_eq!(parse("shift+ctrl+z").to_string(), "Ctrl+Shift+KeyZ");
        assert_eq!(parse("F11").to_string(), "F11");
    }
}
