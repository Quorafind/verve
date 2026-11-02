//! `fs.Stats` and `fs.Dirent`: the numbers Node's `Stats` carries and the kind `Dirent` names,
//! out of what the disk says or what an overlay answered for a packed file — and the tables the
//! two classes are built from.

use std::fs::Metadata;
use std::path::Path;

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer, object::builtins::JsDate,
};

use crate::native::{get, hidden, js_text, object};

use super::sys::Meta;

// ─── Stats and Dirent ───────────────────────────────────────────────────────────────────────

pub(crate) const S_IFMT: u32 = 0o170000;
pub(crate) const S_IFREG: u32 = 0o100000;
pub(crate) const S_IFDIR: u32 = 0o040000;
pub(crate) const S_IFLNK: u32 = 0o120000;
pub(crate) const S_IFIFO: u32 = 0o010000;
pub(crate) const S_IFSOCK: u32 = 0o140000;
pub(crate) const S_IFCHR: u32 = 0o020000;
pub(crate) const S_IFBLK: u32 = 0o060000;

pub(crate) fn millis(time: std::io::Result<std::time::SystemTime>) -> f64 {
    time.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0., |d| d.as_secs_f64() * 1000.)
}

/// The numbers of a `Stats`, in the order Node's `Stats` constructor takes them.
pub(crate) struct StatNumbers {
    pub(crate) dev: f64,
    pub(crate) mode: f64,
    pub(crate) nlink: f64,
    pub(crate) uid: f64,
    pub(crate) gid: f64,
    pub(crate) rdev: f64,
    pub(crate) blksize: f64,
    pub(crate) ino: f64,
    pub(crate) size: f64,
    pub(crate) blocks: f64,
    pub(crate) atime: f64,
    pub(crate) mtime: f64,
    pub(crate) ctime: f64,
    pub(crate) birthtime: f64,
}

#[cfg(unix)]
pub(crate) fn stat_numbers(meta: &Metadata) -> StatNumbers {
    use std::os::unix::fs::MetadataExt;
    StatNumbers {
        dev: meta.dev() as f64,
        mode: f64::from(meta.mode()),
        nlink: meta.nlink() as f64,
        uid: f64::from(meta.uid()),
        gid: f64::from(meta.gid()),
        rdev: meta.rdev() as f64,
        blksize: meta.blksize() as f64,
        ino: meta.ino() as f64,
        size: meta.size() as f64,
        blocks: meta.blocks() as f64,
        atime: meta.atime() as f64 * 1000. + meta.atime_nsec() as f64 / 1e6,
        mtime: meta.mtime() as f64 * 1000. + meta.mtime_nsec() as f64 / 1e6,
        ctime: meta.ctime() as f64 * 1000. + meta.ctime_nsec() as f64 / 1e6,
        birthtime: millis(meta.created()).max(0.),
    }
}

#[cfg(not(unix))]
pub(crate) fn stat_numbers(meta: &Metadata) -> StatNumbers {
    // Windows has no mode: Node makes one of the kind and whether the file is read-only.
    let kind = if meta.is_symlink() {
        S_IFLNK
    } else if meta.is_dir() {
        S_IFDIR
    } else {
        S_IFREG
    };
    let permissions = if meta.permissions().readonly() {
        0o444
    } else {
        0o666
    };
    let size = meta.len() as f64;
    StatNumbers {
        dev: 0.,
        mode: f64::from(kind | permissions),
        nlink: 1.,
        uid: 0.,
        gid: 0.,
        rdev: 0.,
        blksize: 4096.,
        ino: 0.,
        size,
        blocks: (size / 4096.).ceil() * 8.,
        atime: millis(meta.accessed()),
        mtime: millis(meta.modified()),
        ctime: millis(meta.modified()),
        birthtime: millis(meta.created()),
    }
}

/// A `Dirent`'s kind, as Node numbers them (`UV_DIRENT_*`).
pub(crate) const DIRENT_FILE: u32 = 1;
pub(crate) const DIRENT_DIR: u32 = 2;
pub(crate) const DIRENT_LINK: u32 = 3;

pub(crate) fn dirent_kind(file_type: std::io::Result<std::fs::FileType>) -> u32 {
    match file_type {
        Ok(t) if t.is_symlink() => DIRENT_LINK,
        Ok(t) if t.is_dir() => DIRENT_DIR,
        Ok(t) if t.is_file() => DIRENT_FILE,
        _ => 0,
    }
}

/// A path with Windows' `\\?\` taken off, as `realpath` and `readlink` answer one.
pub(crate) fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}

fn date(ms: f64, context: &mut Context) -> JsResult<JsValue> {
    let date = JsDate::new(context);
    date.set_time(ms, context)?;
    Ok(date.into())
}

/// Fills a `Stats` with its numbers, and the dates made of them. With `bigint`, the numbers are
/// `BigInt`s and the times have nanosecond twins (`mtimeNs`), as `{ bigint: true }` asks.
pub(crate) fn fill_stats(
    target: &JsObject,
    n: &StatNumbers,
    bigint: bool,
    context: &mut Context,
) -> JsResult<()> {
    let number = |value: f64| -> JsValue {
        if bigint {
            boa_engine::JsBigInt::new(value.floor() as i64).into()
        } else {
            value.into()
        }
    };
    for (key, value) in [
        ("dev", n.dev),
        ("mode", n.mode),
        ("nlink", n.nlink),
        ("uid", n.uid),
        ("gid", n.gid),
        ("rdev", n.rdev),
        ("blksize", n.blksize),
        ("ino", n.ino),
        ("size", n.size),
        ("blocks", n.blocks),
        ("atimeMs", n.atime),
        ("mtimeMs", n.mtime),
        ("ctimeMs", n.ctime),
        ("birthtimeMs", n.birthtime),
    ] {
        target.set(JsString::from(key), number(value), false, context)?;
    }
    if bigint {
        for (key, value) in [
            ("atimeNs", n.atime),
            ("mtimeNs", n.mtime),
            ("ctimeNs", n.ctime),
            ("birthtimeNs", n.birthtime),
        ] {
            target.set(
                JsString::from(key),
                boa_engine::JsBigInt::new((value * 1e6) as i64),
                false,
                context,
            )?;
        }
    }
    for (key, value) in [
        ("atime", n.atime),
        ("mtime", n.mtime),
        ("ctime", n.ctime),
        ("birthtime", n.birthtime),
    ] {
        let date = date(value, context)?;
        target.set(JsString::from(key), date, false, context)?;
    }
    Ok(())
}

/// A prototype of one of `fs`'s classes: `fs.Stats.prototype`, `fs.Dirent.prototype`.
pub(crate) fn prototype_of(class_name: &str, context: &mut Context) -> JsResult<JsObject> {
    let fs = crate::modules::node_module(context, "fs")?;
    let class = get(&fs, class_name, context)?;
    get(&class, "prototype", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("fs.{class_name} is not a class")))
}

pub(crate) fn stats_object(meta: &Meta, bigint: bool, context: &mut Context) -> JsResult<JsValue> {
    let stats = object(context);
    stats.set_prototype(Some(prototype_of("Stats", context)?));
    fill_stats(&stats, &meta.numbers(), bigint, context)?;
    Ok(stats.into())
}

/// `isFile()` and the rest, of a `Stats`: its `mode`'s kind.
fn mode_is(this: &JsValue, kind: u32, context: &mut Context) -> JsResult<JsValue> {
    let mode = get(this, "mode", context)?;
    let mode = if mode.is_bigint() {
        mode.to_numeric_number(context)?
    } else {
        mode.to_number(context)?
    };
    Ok(((mode as u32) & S_IFMT == kind).into())
}

pub(crate) const STATS_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("isFile", 0, |this, _, cx| mode_is(this, S_IFREG, cx)),
    ("isDirectory", 0, |this, _, cx| mode_is(this, S_IFDIR, cx)),
    ("isSymbolicLink", 0, |this, _, cx| {
        mode_is(this, S_IFLNK, cx)
    }),
    ("isFIFO", 0, |this, _, cx| mode_is(this, S_IFIFO, cx)),
    ("isSocket", 0, |this, _, cx| mode_is(this, S_IFSOCK, cx)),
    ("isCharacterDevice", 0, |this, _, cx| {
        mode_is(this, S_IFCHR, cx)
    }),
    ("isBlockDevice", 0, |this, _, cx| mode_is(this, S_IFBLK, cx)),
];

fn dirent_is(this: &JsValue, kind: u32, context: &mut Context) -> JsResult<JsValue> {
    let own = get(this, "__kind", context)?.to_number(context)? as u32;
    Ok((own == kind).into())
}

pub(crate) const DIRENT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("isFile", 0, |this, _, cx| dirent_is(this, DIRENT_FILE, cx)),
    ("isDirectory", 0, |this, _, cx| {
        dirent_is(this, DIRENT_DIR, cx)
    }),
    ("isSymbolicLink", 0, |this, _, cx| {
        dirent_is(this, DIRENT_LINK, cx)
    }),
    ("isFIFO", 0, |this, _, cx| dirent_is(this, 4, cx)),
    ("isSocket", 0, |this, _, cx| dirent_is(this, 5, cx)),
    ("isCharacterDevice", 0, |this, _, cx| dirent_is(this, 6, cx)),
    ("isBlockDevice", 0, |this, _, cx| dirent_is(this, 7, cx)),
];

pub(crate) fn dirent(
    name: JsValue,
    parent: &str,
    kind: u32,
    context: &mut Context,
) -> JsResult<JsValue> {
    let entry = object(context);
    entry.set_prototype(Some(prototype_of("Dirent", context)?));
    entry.set(js_string!("name"), name, false, context)?;
    entry.set(js_string!("parentPath"), js_text(parent), false, context)?;
    entry.set(js_string!("path"), js_text(parent), false, context)?;
    hidden(&entry, js_string!("__kind"), kind, context)?;
    Ok(entry.into())
}
