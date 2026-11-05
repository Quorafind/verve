//! A JavaScript value copied out of its realm into memory no realm owns, so it can cross to
//! another thread: what an embedder sending a message to an isolate's scripts needs, and what a
//! page and its worker already send each other.
//!
//! [`Cloned`] holds what `structuredClone` copies — primitives, objects, arrays, dates, regular
//! expressions, maps, sets, `ArrayBuffer`s and views of them, errors, blobs and files, cycles and
//! all — and is made into a value again in whichever realm [`Cloned::to_value`] is called in.
//! What cannot be cloned (a function, a symbol, a promise, a platform object) fails with the
//! `DataCloneError` a page's `postMessage` fails with.
//!
//! A clone can also be written as bytes ([`Cloned::to_bytes`]) and read back
//! ([`Cloned::from_bytes`]) — a process's IPC carries it that way. The format is this runtime's
//! own: nothing else is expected to read it.

mod bytes;
mod copy;
mod tree;

use boa_engine::{Context, JsResult, JsValue};

pub use bytes::CloneBytesError;
pub(crate) use copy::{entries, refused};
use tree::Tree;

/// A value out of a realm: `Send`, so it can be handed to another thread.
#[derive(Clone, Debug)]
pub struct Cloned(Tree);

impl Cloned {
    /// `value`, out of `context`'s realm, as `structuredClone` copies it.
    ///
    /// # Errors
    ///
    /// `DataCloneError` for what cannot be cloned.
    pub fn from_value(value: &JsValue, context: &mut Context) -> JsResult<Self> {
        copy::out(value, context).map(Self)
    }

    /// The clone, made again in `context`'s realm.
    ///
    /// # Errors
    ///
    /// What the realm refuses to make: a `Blob` where it has no `Blob` class, or a value its
    /// own constructors throw on.
    pub fn to_value(&self, context: &mut Context) -> JsResult<JsValue> {
        copy::into(&self.0, context)
    }

    /// A clone of `undefined`.
    pub fn undefined() -> Self {
        Self(Tree::Undefined)
    }

    /// A clone of an `Error` with `name` and `message`: what an IPC call that failed on the other
    /// side carries back.
    pub fn from_error_message(name: &str, message: &str) -> Self {
        Self(Tree::Error(vec![
            field("name", name),
            field("message", message),
        ]))
    }

    /// The clone as bytes, to be read back by [`Cloned::from_bytes`] — in another process, or
    /// after being kept somewhere.
    ///
    /// # Errors
    ///
    /// [`CloneBytesError::TooLarge`] for a string, buffer or collection longer than the format
    /// can count (four bytes).
    pub fn to_bytes(&self) -> Result<Vec<u8>, CloneBytesError> {
        let mut out = Vec::new();
        self.0.write_to(&mut out)?;
        Ok(out)
    }

    /// The clone `bytes` holds, written by [`Cloned::to_bytes`].
    ///
    /// # Errors
    ///
    /// What is wrong with bytes that are not one: they end early, hold a tag this runtime never
    /// writes, nest deeper than a clone is read, or go on after the clone is whole.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CloneBytesError> {
        let mut at = 0;
        let tree = Tree::read_from(bytes, &mut at)?;
        if at != bytes.len() {
            return Err(CloneBytesError::Trailing {
                count: bytes.len() - at,
            });
        }
        Ok(Self(tree))
    }
}

fn field(name: &str, value: &str) -> (Vec<u16>, Tree) {
    (tree::utf16(name), Tree::String(tree::utf16(value)))
}
