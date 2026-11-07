//! What a person does, as the embedder reports it: the modifier keys held, the pointer and a key.

/// The modifier keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// The pointer, where it is and what it pressed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    /// The button pressed or let go: 0 primary, 1 middle (auxiliary), 2 secondary.
    pub button: i16,
    /// How many presses in a row this is: 2 for the second of a double click.
    pub count: u32,
    pub modifiers: Modifiers,
    /// The buttons held while this happens, as `MouseEvent.buttons` has them (primary 1,
    /// secondary 2, auxiliary 4, back 8, forward 16): the pointer after the event, so a button
    /// that went down is in it and one that came up is not.
    pub buttons: u16,
}

/// A key, as `KeyboardEvent` has it: `key` (`"a"`, `"Enter"`, `"ArrowLeft"`) and `code`
/// (`"KeyA"`, `"Enter"`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub key: String,
    pub code: String,
    pub modifiers: Modifiers,
    pub repeat: bool,
}

impl Key {
    /// The key that types `c`, with its `code`.
    pub fn char(c: char) -> Self {
        let code = if c.is_ascii_alphabetic() {
            format!("Key{}", c.to_ascii_uppercase())
        } else if c.is_ascii_digit() {
            format!("Digit{c}")
        } else if c == ' ' {
            "Space".to_owned()
        } else {
            String::new()
        };
        Key {
            key: c.to_string(),
            code,
            modifiers: Modifiers {
                shift: c.is_ascii_uppercase(),
                ..Modifiers::default()
            },
            repeat: false,
        }
    }

    /// A named key: `Key::named("Enter")`, `Key::named("Backspace")`.
    pub fn named(name: &str) -> Self {
        Key {
            key: name.to_owned(),
            code: name.to_owned(),
            ..Key::default()
        }
    }
}

/// What a person did to the page last: whether a focus the page asks for shows its ring
/// (`:focus-visible`). Before any input, a script's focus shows it, as in Chromium.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LastInput {
    #[default]
    Nothing,
    Keyboard,
    Pointer,
}
