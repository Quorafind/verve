//! The system clipboard, as Electron's `clipboard` module sees it: a set of things written
//! together (a write replaces what was there), and reads that each answer one format.
//!
//! Windows has the real clipboard ([`super::clipboard_windows`]); other platforms keep what is
//! written in the process until they have a native backend, so an app that writes and reads its own
//! clipboard works there.

use crate::pixels::NativeImage;

/// One format a write puts on the clipboard.
#[derive(Clone)]
pub(crate) enum Clip {
    Text(String),
    Html(String),
    Rtf(String),
    Image(NativeImage),
    /// A bookmark's URL; Windows has no place for its title.
    Bookmark(String),
    /// Bytes under a format of the app's own name.
    Buffer {
        format: String,
        bytes: Vec<u8>,
    },
}

/// The clipboard. It has no state of its own: the functions ask the system.
pub(crate) struct Clipboard;

#[cfg(not(windows))]
use super::clipboard_memory as backend;
#[cfg(windows)]
use super::clipboard_windows as backend;

/// Whether the clipboard is the one in memory. It always is where there is no native backend;
/// with `VERVE_NO_SYSTEM_DIALOGS` set it is on Windows too, because that is the switch every test
/// that runs a real shell sets, and no test may read or write the user's clipboard: a headless
/// `webContents.copy()` followed by `clipboard.readText()` would otherwise reach it.
fn in_memory() -> bool {
    cfg!(not(windows)) || std::env::var_os("VERVE_NO_SYSTEM_DIALOGS").is_some()
}

/// Calls `f` of the backend the clipboard is on.
macro_rules! route {
    ($f:ident($($a:expr),*)) => {
        if in_memory() {
            super::clipboard_memory::$f($($a),*)
        } else {
            backend::$f($($a),*)
        }
    };
}

impl Clipboard {
    /// Replaces the clipboard's contents with `items`; `false` when the system would not let it.
    pub(crate) fn write(items: &[Clip]) -> bool {
        route!(write(items))
    }

    /// Empties it.
    pub(crate) fn clear() {
        route!(write(&[]));
    }

    /// `readText`: `""` when there is none.
    pub(crate) fn text() -> String {
        route!(text())
    }

    /// `readHTML`: the fragment, without the wrapper the clipboard format adds.
    pub(crate) fn html() -> String {
        route!(html())
    }

    pub(crate) fn rtf() -> String {
        route!(rtf())
    }

    pub(crate) fn image() -> NativeImage {
        route!(image())
    }

    /// `readBookmark`'s URL.
    pub(crate) fn bookmark() -> String {
        route!(bookmark())
    }

    /// `readBuffer`: the bytes under the format called `format`.
    pub(crate) fn buffer(format: &str) -> Vec<u8> {
        route!(buffer(format))
    }

    /// `availableFormats`, by MIME type.
    pub(crate) fn formats() -> Vec<String> {
        route!(formats())
    }

    /// `has`: whether the format called `format` is on the clipboard.
    pub(crate) fn has(format: &str) -> bool {
        route!(has(format))
    }
}
