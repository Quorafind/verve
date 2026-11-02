//! The classes `fs` exports — `Stats`, `Dirent`, `Dir`, the watchers and `FileHandle` — and
//! `original-fs` sharing them: made once, with `fs`, so an `original-fs` `Stats` is an `fs`
//! `Stats`.

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, js_string};

use crate::native::{ClassSpec, arg, class, function, get, hidden};

use super::stat::{DIRENT_METHODS, STATS_METHODS, StatNumbers, fill_stats};
use super::{DIR_METHODS, FILE_HANDLE_METHODS, WATCHER_METHODS, emitter_class, emitter_init};

fn class_on(context: &mut Context, fs: &JsObject, spec: ClassSpec<'_>) -> JsResult<JsObject> {
    let name = spec.name.to_owned();
    let (class, prototype) = class(context, spec)?;
    fs.set(JsString::from(name.as_str()), class, false, context)?;
    Ok(prototype)
}

/// `new fs.Stats(dev, mode, …)`: the fields in the order Node's deprecated constructor takes.
fn stats_init(this: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let n = |at: usize, cx: &mut Context| -> JsResult<f64> {
        let v = arg(args, at);
        Ok(if v.is_undefined() {
            0.
        } else {
            v.to_number(cx)?
        })
    };
    let numbers = StatNumbers {
        dev: n(0, cx)?,
        mode: n(1, cx)?,
        nlink: n(2, cx)?,
        uid: n(3, cx)?,
        gid: n(4, cx)?,
        rdev: n(5, cx)?,
        blksize: n(6, cx)?,
        ino: n(7, cx)?,
        size: n(8, cx)?,
        blocks: n(9, cx)?,
        atime: n(10, cx)?,
        mtime: n(11, cx)?,
        ctime: n(12, cx)?,
        birthtime: n(13, cx)?,
    };
    fill_stats(this, &numbers, false, cx)
}

/// `new fs.Dirent(name, type, path)`.
fn dirent_init(this: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    this.set(js_string!("name"), arg(args, 0), false, cx)?;
    let kind = arg(args, 1);
    hidden(
        this,
        js_string!("__kind"),
        if kind.is_undefined() { 0.into() } else { kind },
        cx,
    )?;
    this.set(js_string!("parentPath"), arg(args, 2), false, cx)?;
    this.set(js_string!("path"), arg(args, 2), false, cx)?;
    Ok(())
}

/// The classes `fs` exports: `Stats`, `Dirent`, `Dir`, the watchers and `FileHandle`.
pub(super) fn make_classes(context: &mut Context, fs: &JsObject) -> JsResult<()> {
    class_on(
        context,
        fs,
        ClassSpec {
            name: "Stats",
            length: 14,
            init: stats_init,
            methods: STATS_METHODS,
            ..ClassSpec::default()
        },
    )?;
    class_on(
        context,
        fs,
        ClassSpec {
            name: "Dirent",
            length: 3,
            init: dirent_init,
            methods: DIRENT_METHODS,
            ..ClassSpec::default()
        },
    )?;
    let dir_prototype = class_on(
        context,
        fs,
        ClassSpec {
            name: "Dir",
            methods: DIR_METHODS,
            ..ClassSpec::default()
        },
    )?;
    let symbol = boa_engine::JsSymbol::async_iterator();
    let itself = function(context, "[Symbol.asyncIterator]", 0, |this, _, _| {
        Ok(this.clone())
    });
    dir_prototype.set(symbol, itself, false, context)?;
    let emitter = emitter_class(context)?;
    for name in ["FSWatcher", "StatWatcher"] {
        class_on(
            context,
            fs,
            ClassSpec {
                name,
                parent: Some(&emitter),
                init: emitter_init,
                methods: WATCHER_METHODS,
                ..ClassSpec::default()
            },
        )?;
    }
    file_handle_class(context, fs, &emitter)
}

/// `FileHandle`, an `EventEmitter` with the descriptor it holds.
fn file_handle_class(context: &mut Context, fs: &JsObject, emitter: &JsObject) -> JsResult<()> {
    let handle_prototype = class_on(
        context,
        fs,
        ClassSpec {
            name: "FileHandle",
            length: 1,
            init: |this, args, cx| {
                emitter_init(this, &[], cx)?;
                this.set(js_string!("fd"), arg(args, 0), false, cx)
                    .map(|_| ())
            },
            parent: Some(emitter),
            methods: FILE_HANDLE_METHODS,
            ..ClassSpec::default()
        },
    )?;
    let close = handle_prototype.get(js_string!("close"), context)?;
    // `await using handle = …`: closed when the block ends, where `Symbol.asyncDispose` exists.
    let symbol_class = context.intrinsics().constructors().symbol().constructor();
    let async_dispose = symbol_class.get(js_string!("asyncDispose"), context)?;
    if let Some(symbol) = async_dispose.as_symbol() {
        hidden(&handle_prototype, symbol, close, context)?;
    }
    Ok(())
}

/// `original-fs` exports the very classes `fs` does, made once with it.
pub(super) fn share_classes(context: &mut Context, original: &JsObject) -> JsResult<()> {
    let fs = crate::modules::node_module(context, "fs")?;
    for name in [
        "Stats",
        "Dirent",
        "Dir",
        "FSWatcher",
        "StatWatcher",
        "FileHandle",
    ] {
        let class = get(&fs, name, context)?;
        original.set(JsString::from(name), class, false, context)?;
    }
    Ok(())
}
