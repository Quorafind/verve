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

impl Clipboard {
    /// Replaces the clipboard's contents with `items`; `false` when the system would not let it.
    pub(crate) fn write(items: &[Clip]) -> bool {
        backend::write(items)
    }

    /// Empties it.
    pub(crate) fn clear() {
        backend::write(&[]);
    }

    /// `readText`: `""` when there is none.
    pub(crate) fn text() -> String {
        backend::text()
    }

    /// `readHTML`: the fragment, without the wrapper the clipboard format adds.
    pub(crate) fn html() -> String {
        backend::html()
    }

    pub(crate) fn rtf() -> String {
        backend::rtf()
    }

    pub(crate) fn image() -> NativeImage {
        backend::image()
    }

    /// `readBookmark`'s URL.
    pub(crate) fn bookmark() -> String {
        backend::bookmark()
    }

    /// `readBuffer`: the bytes under the format called `format`.
    pub(crate) fn buffer(format: &str) -> Vec<u8> {
        backend::buffer(format)
    }

    /// `availableFormats`, by MIME type.
    pub(crate) fn formats() -> Vec<String> {
        backend::formats()
    }

    /// `has`: whether the format called `format` is on the clipboard.
    pub(crate) fn has(format: &str) -> bool {
        backend::has(format)
    }
}
