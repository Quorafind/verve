//! `original-fs`: `fs` as Node's is, never reading through an overlay — Electron's own module
//! for the applications it packs.
//!
//! Both modules are made by one table ([`super::build`] walks it), and every export it makes
//! carries the [`View`] it reads through: `fs`'s the realm's layers, `original-fs`'s the disk
//! alone. The decision lives in the export, not in anything set while it runs, so JavaScript a
//! call runs meanwhile — a path object's `toString` — reads through the realm's own layers
//! again, and a stream made by one export keeps reading through it for as long as it runs.

use boa_engine::{Context, JsObject, JsResult, JsValue};

use crate::native::closure;
use crate::vfs::View;

/// One of `fs`'s operations and the layer it reads through, held by the export made of it.
#[derive(Clone, Copy)]
pub(crate) struct Export {
    pub(super) operation: super::Operation,
    pub(super) original: bool,
}

impl boa_gc::Finalize for Export {}

// SAFETY: a function pointer and a bool hold no garbage-collected value.
unsafe impl boa_gc::Trace for Export {
    boa_gc::empty_trace!();
}

/// The export `name` of `fs`: `original`'s reads the disk alone.
pub(super) fn entry(
    context: &mut Context,
    original: bool,
    name: &str,
    length: usize,
    operation: super::Operation,
) -> JsObject {
    closure(
        context,
        name,
        length,
        call,
        Export {
            operation,
            original,
        },
    )
}

fn call(
    _this: &JsValue,
    args: &[JsValue],
    export: &Export,
    context: &mut Context,
) -> JsResult<JsValue> {
    let view = View::new(context, export.original);
    (export.operation)(&view, args, context)
}

/// `require('original-fs')`: `fs` as Node's is, never reading through an overlay — Electron's
/// own module for the apps it packs.
pub(crate) fn original_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    super::build(context, true)
}
