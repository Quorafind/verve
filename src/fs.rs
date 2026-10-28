//! `fs`, `fs/promises`: the disk, as Node's `fs` reaches it — a desktop Obsidian plugin may,
//! and some do (exporters, backup and sync plugins, ones that read a vault's own config).
//!
//! Every operation is written once, synchronously, as `fs.<name>Sync` has it; the callback form
//! (`fs.readFile(path, callback)`, the callback called once this turn of the loop is over) and
//! the promise form (`fs.promises.readFile`) are made from it. Errors are Node's: an `Error`
//! with `code` (`ENOENT`, `EEXIST`, `ENOTDIR`, `EISDIR`, `ENOTEMPTY`, `EACCES`, `EPERM`…),
//! `errno`, `syscall` and `path` (and `dest`), and the message Node writes — `ENOENT: no such
//! file or directory, open 'C:\vault\x.md'`.
//!
//! File descriptors are this realm's own: `openSync` answers a number that stands for a file
//! kept open here, and `readSync`, `writeSync`, `fstatSync`, `closeSync` take it; 1 and 2 write
//! to the console. `watch` and `watchFile` answer watchers that never fire — nothing here
//! watches the disk — and close as they should. Paths are strings, `Buffer`s, or `file:` URLs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsArrayBuffer, JsDate, JsPromise},
};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error, function, get, hidden, invoke, js_text,
    methods, object, settled, text_of,
};

// ─── Errors ─────────────────────────────────────────────────────────────────────────────────

/// What Node says for each code.
fn description(code: &str) -> &'static str {
    match code {
        "ENOENT" => "no such file or directory",
        "EEXIST" => "file already exists",
        "ENOTDIR" => "not a directory",
        "EISDIR" => "illegal operation on a directory",
        "ENOTEMPTY" => "directory not empty",
        "EACCES" => "permission denied",
        "EPERM" => "operation not permitted",
        "EBADF" => "bad file descriptor",
        "EBUSY" => "resource busy or locked",
        "EINVAL" => "invalid argument",
        "EXDEV" => "cross-device link not permitted",
        "EROFS" => "read-only file system",
        "ENOSPC" => "no space left on device",
        "ENAMETOOLONG" => "name too long",
        "ELOOP" => "too many symbolic links encountered",
        "EMFILE" => "too many open files",
        _ => "i/o error",
    }
}

/// The code of a system error, read as libuv reads it.
fn code_of(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind as K;
    #[cfg(windows)]
    if let Some(raw) = error.raw_os_error() {
        match raw {
            2 | 3 | 123 | 161 => return "ENOENT",
            5 => return "EPERM",
            6 => return "EBADF",
            17 => return "EXDEV",
            32 | 33 => return "EBUSY",
            80 | 183 => return "EEXIST",
            87 => return "EINVAL",
            112 => return "ENOSPC",
            145 => return "ENOTEMPTY",
            206 => return "ENAMETOOLONG",
            267 => return "ENOTDIR",
            1921 => return "ELOOP",
            _ => {}
        }
    }
    #[cfg(target_os = "linux")]
    if let Some(raw) = error.raw_os_error()
        && let Some((name, _)) = crate::os::ERRNO.iter().find(|(_, n)| *n == i64::from(raw))
    {
        return name;
    }
    match error.kind() {
        K::NotFound => "ENOENT",
        K::PermissionDenied => "EACCES",
        K::AlreadyExists => "EEXIST",
        K::NotADirectory => "ENOTDIR",
        K::IsADirectory => "EISDIR",
        K::DirectoryNotEmpty => "ENOTEMPTY",
        K::ReadOnlyFilesystem => "EROFS",
        K::CrossesDevices => "EXDEV",
        K::InvalidInput | K::InvalidFilename => "EINVAL",
        K::StorageFull => "ENOSPC",
        K::ResourceBusy => "EBUSY",
        _ => "EIO",
    }
}

/// An error as Node's `fs` throws it, for `code` in the call `syscall` on `path` (and `dest`).
fn fs_error(
    context: &mut Context,
    code: &str,
    syscall: &str,
    path: Option<&str>,
    dest: Option<&str>,
) -> JsError {
    let mut message = format!("{code}: {}, {syscall}", description(code));
    if let Some(path) = path {
        message.push_str(&format!(" '{path}'"));
    }
    if let Some(dest) = dest {
        message.push_str(&format!(" -> '{dest}'"));
    }
    let error = JsNativeError::error()
        .with_message(message)
        .into_opaque(context);
    let _ = error.set(
        js_string!("errno"),
        -(crate::os::errno(code) as f64),
        false,
        context,
    );
    let _ = error.set(js_string!("code"), js_text(code), false, context);
    let _ = error.set(js_string!("syscall"), js_text(syscall), false, context);
    if let Some(path) = path {
        let _ = error.set(js_string!("path"), js_text(path), false, context);
    }
    if let Some(dest) = dest {
        let _ = error.set(js_string!("dest"), js_text(dest), false, context);
    }
    JsError::from_opaque(error.into())
}

fn io_error(context: &mut Context, error: std::io::Error, syscall: &str, path: &str) -> JsError {
    fs_error(context, code_of(&error), syscall, Some(path), None)
}

fn io_error2(
    context: &mut Context,
    error: std::io::Error,
    syscall: &str,
    path: &str,
    dest: &str,
) -> JsError {
    fs_error(context, code_of(&error), syscall, Some(path), Some(dest))
}

/// `ERR_FS_EISDIR`: `rm` or `cp` of a folder without `recursive`.
fn is_dir_error(context: &mut Context, syscall: &str, path: &str) -> JsError {
    coded_error(
        context,
        &format!("Path is a directory: {syscall} returned EISDIR (is a directory) {path}"),
        "ERR_FS_EISDIR",
    )
}

// ─── Arguments ──────────────────────────────────────────────────────────────────────────────

/// A path as `fs` takes one: a string, a `Buffer` (its UTF-8), or a `file:` URL.
fn path_of(value: &JsValue, name: &str, context: &mut Context) -> JsResult<String> {
    if value.is_string() {
        return text_of(value, context);
    }
    if let Some(bytes) = crate::encoding::bytes_of(value, context)? {
        return Ok(String::from_utf8_lossy(&bytes).into_owned());
    }
    if value.is_object() && !get(value, "href", context)?.is_undefined() {
        return crate::node_url::path_from_url(value, context);
    }
    let received = if value.is_undefined() {
        "undefined".to_owned()
    } else {
        format!("type {}", value.type_of())
    };
    Err(coded_error(
        context,
        &format!(
            "The \"{name}\" argument must be of type string or an instance of Buffer or URL. Received {received}"
        ),
        "ERR_INVALID_ARG_TYPE",
    ))
}

fn path_arg(args: &[JsValue], at: usize, context: &mut Context) -> JsResult<String> {
    let name = if at == 0 { "path" } else { "dest" };
    path_of(&arg(args, at), name, context)
}

/// An option from an options object; `undefined` when the options are not one.
fn option(options: &JsValue, name: &str, context: &mut Context) -> JsResult<JsValue> {
    if options.is_object() && !options.is_callable() {
        get(options, name, context)
    } else {
        Ok(JsValue::undefined())
    }
}

fn flag(options: &JsValue, name: &str, context: &mut Context) -> JsResult<bool> {
    Ok(option(options, name, context)?.to_boolean())
}

/// The encoding asked for: the options themselves when a string, or their `encoding`.
fn encoding_of(options: &JsValue, context: &mut Context) -> JsResult<Option<String>> {
    if options.is_string() {
        return Ok(Some(text_of(options, context)?));
    }
    let encoding = option(options, "encoding", context)?;
    if encoding.is_null_or_undefined() {
        Ok(None)
    } else {
        Ok(Some(text_of(&encoding, context)?))
    }
}

/// Bytes as the caller asked for them: a `Buffer`, or text in `encoding`.
fn bytes_out(bytes: Vec<u8>, encoding: Option<&str>, context: &mut Context) -> JsResult<JsValue> {
    let buffer = crate::encoding::make_buffer(context, bytes)?;
    match encoding {
        Some(encoding) if encoding != "buffer" => {
            invoke(&buffer, "toString", &[js_text(encoding)], context)
        }
        _ => Ok(buffer),
    }
}

/// The bytes of data to write: a string in `encoding`, a buffer or view, or anything else as
/// its string.
fn bytes_in(data: &JsValue, encoding: Option<&str>, context: &mut Context) -> JsResult<Vec<u8>> {
    if !data.is_string()
        && let Some(bytes) = crate::encoding::bytes_of(data, context)?
    {
        return Ok(bytes);
    }
    let text = text_of(data, context)?;
    match encoding {
        None | Some("utf8" | "utf-8" | "UTF-8" | "UTF8") => Ok(text.into_bytes()),
        Some(encoding) => {
            let buffer = crate::native::global(context, "Buffer")?;
            let made = invoke(
                &buffer,
                "from",
                &[js_text(&text), js_text(encoding)],
                context,
            )?;
            Ok(crate::encoding::bytes_of(&made, context)?.unwrap_or_default())
        }
    }
}

/// How a file is opened, from Node's flags: `'r'`, `'w+'`, `'ax'`… or `O_*` bits.
#[derive(Clone, Copy, Default)]
struct Flags {
    read: bool,
    write: bool,
    append: bool,
    create: bool,
    truncate: bool,
    create_new: bool,
}

fn flags_of(value: &JsValue, default: &str, context: &mut Context) -> JsResult<Flags> {
    if let Some(bits) = value.as_number() {
        let bits = bits as i64;
        let access = bits & 3;
        return Ok(Flags {
            read: access == 0 || access == 2,
            write: access == 1 || access == 2,
            append: bits & 0o2000 != 0,
            create: bits & 0o100 != 0,
            truncate: bits & 0o1000 != 0,
            create_new: bits & 0o100 != 0 && bits & 0o200 != 0,
        });
    }
    let text = if value.is_null_or_undefined() {
        default.to_owned()
    } else {
        text_of(value, context)?
    };
    let normalized: String = text.chars().filter(|c| *c != 's').collect();
    let plus = normalized.contains('+');
    let exclusive = normalized.contains('x');
    let base = normalized.trim_matches(|c| c == '+' || c == 'x');
    let flags = match base {
        "r" => Flags {
            read: true,
            write: plus,
            ..Flags::default()
        },
        "w" => Flags {
            read: plus,
            write: true,
            create: true,
            truncate: true,
            create_new: exclusive,
            ..Flags::default()
        },
        "a" => Flags {
            read: plus,
            write: true,
            append: true,
            create: true,
            create_new: exclusive,
            ..Flags::default()
        },
        _ => {
            return Err(coded_error(
                context,
                &format!("The value \"{text}\" is invalid for option \"flags\""),
                "ERR_INVALID_ARG_VALUE",
            ));
        }
    };
    Ok(flags)
}

/// Throws unless the realm may open `path` as `flags` would: to read it, to write it, or both.
fn opening(context: &mut Context, path: &str, flags: Flags) -> JsResult<()> {
    if flags.read {
        crate::permission::read(context, path)?;
    }
    if flags.write || flags.append || flags.create || flags.truncate {
        crate::permission::write(context, path)?;
    }
    Ok(())
}

fn open_with(path: &str, flags: Flags) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(flags.read)
        .write(flags.write && !flags.append)
        .append(flags.append);
    if flags.create_new {
        options.create_new(true);
    } else if flags.create {
        options.create(true);
    }
    if flags.truncate {
        options.truncate(true);
    }
    options.open(path)
}

/// A path with Windows' `\\?\` taken off, as `realpath` and `readlink` answer one.
fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}

// ─── File descriptors ───────────────────────────────────────────────────────────────────────

/// A file `openSync` opened, kept until `closeSync`.
struct OpenFile {
    file: File,
    path: String,
}

/// The realm's open files, by the number `openSync` answered.
#[derive(Default)]
struct Descriptors {
    next: Cell<i32>,
    open: RefCell<HashMap<i32, OpenFile>>,
}

fn descriptors(context: &mut Context) -> Rc<Descriptors> {
    if let Some(table) = context.get_data::<Rc<Descriptors>>() {
        return table.clone();
    }
    let table = Rc::new(Descriptors {
        next: Cell::new(20),
        open: RefCell::new(HashMap::new()),
    });
    context.insert_data(table.clone());
    table
}

fn fd_of(value: &JsValue, context: &mut Context) -> JsResult<i32> {
    match value.as_number() {
        Some(n) if n.fract() == 0. && n >= 0. => Ok(n as i32),
        _ => Err(coded_error(
            context,
            &format!(
                "The \"fd\" argument must be of type number. Received {}",
                value.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        )),
    }
}

/// Runs `f` with the open file `fd`, or throws `EBADF`.
fn with_fd<R>(
    context: &mut Context,
    fd: i32,
    syscall: &str,
    f: impl FnOnce(&mut OpenFile) -> std::io::Result<R>,
) -> JsResult<R> {
    let table = descriptors(context);
    let mut open = table.open.borrow_mut();
    let Some(file) = open.get_mut(&fd) else {
        drop(open);
        return Err(fs_error(context, "EBADF", syscall, None, None));
    };
    let path = file.path.clone();
    let outcome = f(file);
    drop(open);
    outcome.map_err(|e| io_error(context, e, syscall, &path))
}

/// Writes to the console what is written to descriptor 1 or 2.
fn console_write(fd: i32, bytes: &[u8], context: &mut Context) {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let level = if fd == 2 {
        crate::Level::Error
    } else {
        crate::Level::Log
    };
    crate::console_line(context, level, text.to_owned());
}

// ─── Stats and Dirent ───────────────────────────────────────────────────────────────────────

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFIFO: u32 = 0o010000;
const S_IFSOCK: u32 = 0o140000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;

fn millis(time: std::io::Result<std::time::SystemTime>) -> f64 {
    time.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0., |d| d.as_secs_f64() * 1000.)
}

/// The numbers of a `Stats`, in the order Node's `Stats` constructor takes them.
struct StatNumbers {
    dev: f64,
    mode: f64,
    nlink: f64,
    uid: f64,
    gid: f64,
    rdev: f64,
    blksize: f64,
    ino: f64,
    size: f64,
    blocks: f64,
    atime: f64,
    mtime: f64,
    ctime: f64,
    birthtime: f64,
}

#[cfg(unix)]
fn stat_numbers(meta: &Metadata) -> StatNumbers {
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
fn stat_numbers(meta: &Metadata) -> StatNumbers {
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

fn date(ms: f64, context: &mut Context) -> JsResult<JsValue> {
    let date = JsDate::new(context);
    date.set_time(ms, context)?;
    Ok(date.into())
}

/// Fills a `Stats` with its numbers, and the dates made of them. With `bigint`, the numbers are
/// `BigInt`s and the times have nanosecond twins (`mtimeNs`), as `{ bigint: true }` asks.
fn fill_stats(
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
fn prototype_of(class_name: &str, context: &mut Context) -> JsResult<JsObject> {
    let fs = crate::modules::node_module(context, "fs")?;
    let class = get(&fs, class_name, context)?;
    get(&class, "prototype", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("fs.{class_name} is not a class")))
}

fn stats_object(meta: &Metadata, bigint: bool, context: &mut Context) -> JsResult<JsValue> {
    let stats = object(context);
    stats.set_prototype(Some(prototype_of("Stats", context)?));
    fill_stats(&stats, &stat_numbers(meta), bigint, context)?;
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

const STATS_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
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

/// A `Dirent`'s kind, as Node numbers them (`UV_DIRENT_*`).
const DIRENT_FILE: u32 = 1;
const DIRENT_DIR: u32 = 2;
const DIRENT_LINK: u32 = 3;

fn dirent_kind(file_type: std::io::Result<std::fs::FileType>) -> u32 {
    match file_type {
        Ok(t) if t.is_symlink() => DIRENT_LINK,
        Ok(t) if t.is_dir() => DIRENT_DIR,
        Ok(t) if t.is_file() => DIRENT_FILE,
        _ => 0,
    }
}

fn dirent_is(this: &JsValue, kind: u32, context: &mut Context) -> JsResult<JsValue> {
    let own = get(this, "__kind", context)?.to_number(context)? as u32;
    Ok((own == kind).into())
}

const DIRENT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
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

fn dirent(name: JsValue, parent: &str, kind: u32, context: &mut Context) -> JsResult<JsValue> {
    let entry = object(context);
    entry.set_prototype(Some(prototype_of("Dirent", context)?));
    entry.set(js_string!("name"), name, false, context)?;
    entry.set(js_string!("parentPath"), js_text(parent), false, context)?;
    entry.set(js_string!("path"), js_text(parent), false, context)?;
    hidden(&entry, js_string!("__kind"), kind, context)?;
    Ok(entry.into())
}

// ─── The operations ─────────────────────────────────────────────────────────────────────────

fn read_file(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let options = arg(args, 1);
    let encoding = encoding_of(&options, context)?;
    let bytes = if target.is_number() {
        let fd = fd_of(&target, context)?;
        with_fd(context, fd, "read", |open| {
            let mut bytes = Vec::new();
            open.file.read_to_end(&mut bytes)?;
            Ok(bytes)
        })?
    } else {
        let path = path_of(&target, "path", context)?;
        crate::permission::read(context, &path)?;
        if Path::new(&path).is_dir() {
            return Err(fs_error(context, "EISDIR", "read", None, None));
        }
        std::fs::read(&path).map_err(|e| io_error(context, e, "open", &path))?
    };
    bytes_out(bytes, encoding.as_deref(), context)
}

/// `writeFile` and `appendFile`, to a path or a descriptor.
fn write_file(args: &[JsValue], context: &mut Context, append: bool) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let options = arg(args, 2);
    let encoding = encoding_of(&options, context)?;
    let bytes = bytes_in(&arg(args, 1), encoding.as_deref(), context)?;
    if target.is_number() {
        let fd = fd_of(&target, context)?;
        if fd == 1 || fd == 2 {
            console_write(fd, &bytes, context);
            return Ok(JsValue::undefined());
        }
        with_fd(context, fd, "write", |open| open.file.write_all(&bytes))?;
        return Ok(JsValue::undefined());
    }
    let path = path_of(&target, "path", context)?;
    let flags = flags_of(
        &option(&options, "flag", context)?,
        if append { "a" } else { "w" },
        context,
    )?;
    opening(context, &path, flags)?;
    let outcome = open_with(&path, flags).and_then(|mut file| file.write_all(&bytes));
    outcome.map_err(|e| io_error(context, e, "open", &path))?;
    Ok(JsValue::undefined())
}

fn stat_at(args: &[JsValue], context: &mut Context, follow: bool) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    if follow {
        crate::permission::read(context, &path)?;
    } else {
        crate::permission::read_here(context, &path)?;
    }
    let options = arg(args, 1);
    let meta = if follow {
        std::fs::metadata(&path)
    } else {
        std::fs::symlink_metadata(&path)
    };
    match meta {
        Ok(meta) => stats_object(&meta, flag(&options, "bigint", context)?, context),
        Err(e) => {
            // `{ throwIfNoEntry: false }`: a missing file is `undefined`, not an error.
            let quiet = option(&options, "throwIfNoEntry", context)?;
            if e.kind() == std::io::ErrorKind::NotFound && quiet.is_boolean() && !quiet.to_boolean()
            {
                return Ok(JsValue::undefined());
            }
            Err(io_error(
                context,
                e,
                if follow { "stat" } else { "lstat" },
                &path,
            ))
        }
    }
}

fn read_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::read(context, &path)?;
    let options = arg(args, 1);
    let with_types = flag(&options, "withFileTypes", context)?;
    let recursive = flag(&options, "recursive", context)?;
    let encoding = encoding_of(&options, context)?;
    let name_value = |name: &str, cx: &mut Context| -> JsResult<JsValue> {
        if encoding.as_deref() == Some("buffer") {
            crate::encoding::make_buffer(cx, name.as_bytes().to_vec())
        } else {
            Ok(js_text(name))
        }
    };
    // Breadth first, as Node lists a folder and then each folder in it; names below the first
    // folder are relative to it, `/`-separated as this runtime's `path` writes them.
    let mut out = Vec::new();
    let mut queue: Vec<(PathBuf, String)> = vec![(PathBuf::from(&path), String::new())];
    let mut first = true;
    while !queue.is_empty() {
        let (dir, relative) = queue.remove(0);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if first => return Err(io_error(context, e, "scandir", &path)),
            Err(_) => continue,
        };
        first = false;
        let mut entries: Vec<(String, u32)> = entries
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    dirent_kind(e.file_type()),
                )
            })
            .collect();
        entries.sort();
        let parent = if relative.is_empty() {
            path.clone()
        } else {
            format!("{}/{relative}", path.trim_end_matches(['/', '\\']))
        };
        for (name, kind) in entries {
            let shown = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if recursive && kind == DIRENT_DIR {
                queue.push((dir.join(&name), shown.clone()));
            }
            let value = if with_types {
                let name = name_value(&name, context)?;
                dirent(name, &parent, kind, context)?
            } else {
                name_value(&shown, context)?
            };
            out.push(value);
        }
    }
    Ok(array(context, out))
}

fn make_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::write(context, &path)?;
    let options = arg(args, 1);
    if !flag(&options, "recursive", context)? {
        std::fs::create_dir(&path).map_err(|e| io_error(context, e, "mkdir", &path))?;
        return Ok(JsValue::undefined());
    }
    // Recursively, the answer is the first folder made — none when all were there.
    let absolute = std::path::absolute(&path).unwrap_or_else(|_| PathBuf::from(&path));
    let mut first = None;
    let mut at = Some(absolute.as_path());
    while let Some(dir) = at {
        if dir.exists() {
            break;
        }
        first = Some(dir.to_path_buf());
        at = dir.parent();
    }
    if Path::new(&path).is_file() {
        return Err(fs_error(context, "EEXIST", "mkdir", Some(&path), None));
    }
    std::fs::create_dir_all(&path).map_err(|e| io_error(context, e, "mkdir", &path))?;
    Ok(first.map_or(JsValue::undefined(), |dir| js_text(&dir.to_string_lossy())))
}

fn make_temp_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let prefix = path_arg(args, 0, context)?;
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for _ in 0..100 {
        let mut noise = [0u8; 6];
        let _ = getrandom::fill(&mut noise);
        let suffix: String = noise
            .iter()
            .map(|b| LETTERS[*b as usize % LETTERS.len()] as char)
            .collect();
        let path = format!("{prefix}{suffix}");
        crate::permission::write(context, &path)?;
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(js_text(&path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io_error(context, e, "mkdtemp", &format!("{prefix}XXXXXX"))),
        }
    }
    Err(fs_error(
        context,
        "EEXIST",
        "mkdtemp",
        Some(&format!("{prefix}XXXXXX")),
        None,
    ))
}

fn remove(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::write_here(context, &path)?;
    let options = arg(args, 1);
    let (recursive, force) = (
        flag(&options, "recursive", context)?,
        flag(&options, "force", context)?,
    );
    if recursive {
        crate::permission::write_tree(context, &path)?;
    }
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if force && e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JsValue::undefined());
        }
        Err(e) => return Err(io_error(context, e, "lstat", &path)),
    };
    let outcome = if meta.is_dir() {
        if !recursive {
            return Err(is_dir_error(context, "rm", &path));
        }
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_file(&path)
    };
    outcome.map_err(|e| io_error(context, e, "rm", &path))?;
    Ok(JsValue::undefined())
}

fn remove_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::write_here(context, &path)?;
    let outcome = if flag(&arg(args, 1), "recursive", context)? {
        crate::permission::write_tree(context, &path)?;
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_dir(&path)
    };
    outcome.map_err(|e| io_error(context, e, "rmdir", &path))?;
    Ok(JsValue::undefined())
}

fn copy_tree(from: &Path, to: &Path, force: bool, error_on_exist: bool) -> std::io::Result<()> {
    let meta = std::fs::metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(
                &entry.path(),
                &to.join(entry.file_name()),
                force,
                error_on_exist,
            )?;
        }
        return Ok(());
    }
    if to.exists() && !force {
        if error_on_exist {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        return Ok(());
    }
    std::fs::copy(from, to).map(|_| ())
}

/// Throws unless every file `cp` would read under `from` is readable and every one it would
/// write under `to` writable: a link inside the tree is followed, so each is checked where it
/// leads.
fn copy_allowed(context: &mut Context, from: &Path, to: &Path) -> JsResult<()> {
    crate::permission::read(context, from)?;
    crate::permission::write(context, to)?;
    if from.is_dir()
        && let Ok(entries) = std::fs::read_dir(from)
    {
        for entry in entries.flatten() {
            copy_allowed(context, &entry.path(), &to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn copy(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let from = path_arg(args, 0, context)?;
    let to = path_arg(args, 1, context)?;
    copy_allowed(context, Path::new(&from), Path::new(&to))?;
    let options = arg(args, 2);
    let recursive = flag(&options, "recursive", context)?;
    let force = match option(&options, "force", context)? {
        f if f.is_undefined() => true,
        f => f.to_boolean(),
    };
    let error_on_exist = flag(&options, "errorOnExist", context)?;
    if Path::new(&from).is_dir() && !recursive {
        return Err(is_dir_error(context, "cp", &from));
    }
    copy_tree(Path::new(&from), Path::new(&to), force, error_on_exist)
        .map_err(|e| io_error2(context, e, "cp", &from, &to))?;
    Ok(JsValue::undefined())
}

fn access(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    let mode = arg(args, 1).as_number().unwrap_or(0.) as i64;
    crate::permission::read(context, &path)?;
    if mode & 2 != 0 {
        crate::permission::write(context, &path)?;
    }
    let meta = std::fs::metadata(&path).map_err(|e| io_error(context, e, "access", &path))?;
    if mode & 2 != 0 && meta.permissions().readonly() {
        return Err(fs_error(context, "EACCES", "access", Some(&path), None));
    }
    Ok(JsValue::undefined())
}

/// A time as `utimes` takes one: a `Date`, seconds as a number, or a numeric string.
fn time_of(value: &JsValue, context: &mut Context) -> JsResult<std::time::SystemTime> {
    let seconds = if let Some(date) = value
        .as_object()
        .and_then(|o| JsDate::from_object(o.clone()).ok())
    {
        date.get_time(context)?.to_number(context)? / 1000.
    } else {
        value.to_number(context)?
    };
    let seconds = if seconds.is_finite() { seconds } else { 0. };
    Ok(if seconds >= 0. {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(seconds)
    } else {
        std::time::UNIX_EPOCH - std::time::Duration::from_secs_f64(-seconds)
    })
}

/// A file opened only to change its times: Windows wants the right to write attributes, and a
/// folder needs asking for as one.
fn open_for_times(path: &str) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

fn set_times(
    file: &File,
    accessed: std::time::SystemTime,
    modified: std::time::SystemTime,
) -> std::io::Result<()> {
    file.set_times(
        std::fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )
}

fn utimes(args: &[JsValue], context: &mut Context, follow: bool) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    if follow {
        crate::permission::write(context, &path)?;
    } else {
        crate::permission::write_here(context, &path)?;
    }
    let accessed = time_of(&arg(args, 1), context)?;
    let modified = time_of(&arg(args, 2), context)?;
    open_for_times(&path)
        .and_then(|file| set_times(&file, accessed, modified))
        .map_err(|e| io_error(context, e, "utime", &path))?;
    Ok(JsValue::undefined())
}

fn chmod_path(path: &str, mode: u32) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(mode);
    }
    #[cfg(not(unix))]
    {
        // Windows keeps only whether the owner may write.
        permissions.set_readonly(mode & 0o200 == 0);
    }
    std::fs::set_permissions(path, permissions)
}

fn mode_arg(value: &JsValue, context: &mut Context) -> JsResult<u32> {
    if value.is_string() {
        let text = text_of(value, context)?;
        return u32::from_str_radix(&text, 8).map_err(|_| {
            coded_error(context, &format!("The argument 'mode' must be a 32-bit unsigned integer or an octal string. Received '{text}'"), "ERR_INVALID_ARG_VALUE")
        });
    }
    Ok(value.to_number(context)? as u32)
}

fn symlink(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = path_arg(args, 0, context)?;
    let path = path_arg(args, 1, context)?;
    // What the link leads to is checked whenever it is followed; making it is a write here.
    crate::permission::write_here(context, &path)?;
    let kind = arg(args, 2);
    let kind = if kind.is_string() {
        text_of(&kind, context)?
    } else {
        String::new()
    };
    #[cfg(unix)]
    let outcome = {
        let _ = kind;
        std::os::unix::fs::symlink(&target, &path)
    };
    #[cfg(windows)]
    let outcome = {
        let base = Path::new(&path).parent().unwrap_or(Path::new("."));
        let is_dir =
            kind == "dir" || kind == "junction" || (kind.is_empty() && base.join(&target).is_dir());
        if is_dir {
            std::os::windows::fs::symlink_dir(&target, &path)
        } else {
            std::os::windows::fs::symlink_file(&target, &path)
        }
    };
    #[cfg(not(any(unix, windows)))]
    let outcome: std::io::Result<()> = {
        let _ = kind;
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    };
    outcome.map_err(|e| io_error2(context, e, "symlink", &target, &path))?;
    Ok(JsValue::undefined())
}

fn open(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    let flags = flags_of(&arg(args, 1), "r", context)?;
    opening(context, &path, flags)?;
    #[cfg(windows)]
    if flags.read && !flags.write && Path::new(&path).is_dir() {
        return Err(fs_error(context, "EISDIR", "open", Some(&path), None));
    }
    let file = open_with(&path, flags).map_err(|e| io_error(context, e, "open", &path))?;
    let table = descriptors(context);
    let fd = table.next.get();
    table.next.set(fd + 1);
    table.open.borrow_mut().insert(fd, OpenFile { file, path });
    Ok(fd.into())
}

/// Where in a view of bytes to read into or write from, with Node's defaults and its checks.
fn view_range(
    view: &JsValue,
    offset: &JsValue,
    length: &JsValue,
    context: &mut Context,
) -> JsResult<(usize, usize)> {
    let size = get(view, "byteLength", context)?.to_number(context)? as usize;
    let offset = if offset.is_null_or_undefined() {
        0
    } else {
        offset.to_number(context)? as usize
    };
    if offset > size {
        return Err(coded_error(
            context,
            &format!(
                "The value of \"offset\" is out of range. It must be >= 0 && <= {size}. Received {offset}"
            ),
            "ERR_OUT_OF_RANGE",
        ));
    }
    let length = if length.is_null_or_undefined() {
        size - offset
    } else {
        length.to_number(context)? as usize
    };
    if offset + length > size {
        return Err(coded_error(
            context,
            &format!(
                "The value of \"length\" is out of range. It must be >= 0 && <= {}. Received {length}",
                size - offset
            ),
            "ERR_OUT_OF_RANGE",
        ));
    }
    Ok((offset, length))
}

/// Puts `bytes` into the view `view` from `at` on.
fn write_into(view: &JsValue, at: usize, bytes: &[u8], context: &mut Context) -> JsResult<()> {
    let buffer = get(view, "buffer", context)?;
    let start = get(view, "byteOffset", context)?.to_number(context)? as usize + at;
    let buffer = buffer
        .as_object()
        .and_then(|b| JsArrayBuffer::from_object(b.clone()).ok());
    let Some(buffer) = buffer else {
        return Err(crate::native::type_error("not a buffer"));
    };
    if let Some(mut data) = buffer.data_mut() {
        let end = (start + bytes.len()).min(data.len());
        data[start..end].copy_from_slice(&bytes[..end - start]);
    }
    Ok(())
}

/// A position: `null` (or none) for "where the file is", else a number or a bigint.
fn position_of(value: &JsValue, context: &mut Context) -> JsResult<Option<u64>> {
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    let n = if value.is_bigint() {
        value.to_numeric_number(context)?
    } else {
        value.to_number(context)?
    };
    Ok(if n < 0. { None } else { Some(n as u64) })
}

/// Reads at `position` (keeping where the file was), or from where it is.
fn read_at(file: &mut File, into: &mut [u8], position: Option<u64>) -> std::io::Result<usize> {
    let Some(position) = position else {
        return read_full(file, into);
    };
    let was = file.stream_position()?;
    file.seek(SeekFrom::Start(position))?;
    let read = read_full(file, into);
    file.seek(SeekFrom::Start(was))?;
    read
}

fn read_full(file: &mut File, into: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < into.len() {
        match file.read(&mut into[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

fn write_at(file: &mut File, bytes: &[u8], position: Option<u64>) -> std::io::Result<usize> {
    let Some(position) = position else {
        file.write_all(bytes)?;
        return Ok(bytes.len());
    };
    let was = file.stream_position()?;
    file.seek(SeekFrom::Start(position))?;
    let written = file.write_all(bytes);
    file.seek(SeekFrom::Start(was))?;
    written.map(|()| bytes.len())
}

/// `readSync(fd, buffer, offset, length, position)`, also with an options object in place of
/// the last three, or of all four. Answers how many bytes were read, and the buffer.
fn read(args: &[JsValue], context: &mut Context) -> JsResult<(usize, JsValue)> {
    let fd = fd_of(&arg(args, 0), context)?;
    let (buffer, offset, length, position) = {
        let second = arg(args, 1);
        let is_view = crate::encoding::bytes_of(&second, context)?.is_some();
        if !is_view && second.is_object() {
            let buffer = option(&second, "buffer", context)?;
            let buffer = if buffer.is_undefined() {
                crate::encoding::make_buffer(context, vec![0; 16384])?
            } else {
                buffer
            };
            (
                buffer,
                option(&second, "offset", context)?,
                option(&second, "length", context)?,
                option(&second, "position", context)?,
            )
        } else if arg(args, 2).is_object() {
            let o = arg(args, 2);
            (
                second,
                option(&o, "offset", context)?,
                option(&o, "length", context)?,
                option(&o, "position", context)?,
            )
        } else {
            (second, arg(args, 2), arg(args, 3), arg(args, 4))
        }
    };
    let (offset, length) = view_range(&buffer, &offset, &length, context)?;
    let position = position_of(&position, context)?;
    let mut bytes = vec![0; length];
    let read = with_fd(context, fd, "read", |open| {
        read_at(&mut open.file, &mut bytes, position)
    })?;
    write_into(&buffer, offset, &bytes[..read], context)?;
    Ok((read, buffer))
}

/// `writeSync(fd, buffer, offset, length, position)` or `writeSync(fd, string, position,
/// encoding)`. Answers how many bytes were written, and what was written.
fn write(args: &[JsValue], context: &mut Context) -> JsResult<(usize, JsValue)> {
    let fd = fd_of(&arg(args, 0), context)?;
    let data = arg(args, 1);
    let (bytes, position) = if data.is_string() {
        let encoding = arg(args, 3);
        let encoding = if encoding.is_string() {
            Some(text_of(&encoding, context)?)
        } else {
            None
        };
        (
            bytes_in(&data, encoding.as_deref(), context)?,
            position_of(&arg(args, 2), context)?,
        )
    } else if let Some(all) = crate::encoding::bytes_of(&data, context)? {
        let (offset, length, position) = if arg(args, 2).is_object() {
            let o = arg(args, 2);
            (
                option(&o, "offset", context)?,
                option(&o, "length", context)?,
                option(&o, "position", context)?,
            )
        } else {
            (arg(args, 2), arg(args, 3), arg(args, 4))
        };
        let (offset, length) = view_range(&data, &offset, &length, context)?;
        (
            all[offset..offset + length].to_vec(),
            position_of(&position, context)?,
        )
    } else {
        return Err(coded_error(
            context,
            &format!(
                "The \"buffer\" argument must be of type string or an instance of Buffer, TypedArray, or DataView. Received {}",
                data.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        ));
    };
    if fd == 1 || fd == 2 {
        console_write(fd, &bytes, context);
        return Ok((bytes.len(), data));
    }
    let written = with_fd(context, fd, "write", |open| {
        write_at(&mut open.file, &bytes, position)
    })?;
    Ok((written, data))
}

fn statfs(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::read(context, &path)?;
    std::fs::metadata(&path).map_err(|e| io_error(context, e, "statfs", &path))?;
    let out = object(context);
    for (key, value) in [
        ("type", 0.),
        ("bsize", 4096.),
        ("blocks", 1e8),
        ("bfree", 5e7),
        ("bavail", 5e7),
        ("files", 1e7),
        ("ffree", 5e6),
    ] {
        out.set(JsString::from(key), value, false, context)?;
    }
    Ok(out.into())
}

/// The operations, each as `fs` has it synchronously; the callback and promise forms are made
/// from these. The last field says whether `fs.promises` has it.
const OPERATIONS: &[(&str, usize, NativeFunctionPointer, bool)] = &[
    ("readFile", 2, |_, args, cx| read_file(args, cx), true),
    (
        "writeFile",
        3,
        |_, args, cx| write_file(args, cx, false),
        true,
    ),
    (
        "appendFile",
        3,
        |_, args, cx| write_file(args, cx, true),
        true,
    ),
    ("stat", 2, |_, args, cx| stat_at(args, cx, true), true),
    ("lstat", 2, |_, args, cx| stat_at(args, cx, false), true),
    ("statfs", 2, |_, args, cx| statfs(args, cx), true),
    ("readdir", 2, |_, args, cx| read_dir(args, cx), true),
    ("mkdir", 2, |_, args, cx| make_dir(args, cx), true),
    ("mkdtemp", 2, |_, args, cx| make_temp_dir(args, cx), true),
    ("rm", 2, |_, args, cx| remove(args, cx), true),
    ("rmdir", 2, |_, args, cx| remove_dir(args, cx), true),
    (
        "unlink",
        1,
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::write_here(cx, &path)?;
            std::fs::remove_file(&path).map_err(|e| io_error(cx, e, "unlink", &path))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "rename",
        2,
        |_, args, cx| {
            let from = path_arg(args, 0, cx)?;
            let to = path_arg(args, 1, cx)?;
            crate::permission::write_tree(cx, &from)?;
            crate::permission::write_tree(cx, &to)?;
            std::fs::rename(&from, &to).map_err(|e| io_error2(cx, e, "rename", &from, &to))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "copyFile",
        3,
        |_, args, cx| {
            let from = path_arg(args, 0, cx)?;
            let to = path_arg(args, 1, cx)?;
            let mode = arg(args, 2).as_number().unwrap_or(0.) as i64;
            crate::permission::read(cx, &from)?;
            crate::permission::write(cx, &to)?;
            if mode & 1 != 0 && Path::new(&to).exists() {
                return Err(fs_error(cx, "EEXIST", "copyfile", Some(&from), Some(&to)));
            }
            std::fs::copy(&from, &to).map_err(|e| io_error2(cx, e, "copyfile", &from, &to))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    ("cp", 3, |_, args, cx| copy(args, cx), true),
    ("access", 2, |_, args, cx| access(args, cx), true),
    (
        "realpath",
        2,
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::read(cx, &path)?;
            let real =
                std::fs::canonicalize(&path).map_err(|e| io_error(cx, e, "realpath", &path))?;
            bytes_or_text(&plain(&real), &arg(args, 1), cx)
        },
        true,
    ),
    (
        "readlink",
        2,
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::read_here(cx, &path)?;
            let target =
                std::fs::read_link(&path).map_err(|e| io_error(cx, e, "readlink", &path))?;
            bytes_or_text(&plain(&target), &arg(args, 1), cx)
        },
        true,
    ),
    ("symlink", 3, |_, args, cx| symlink(args, cx), true),
    (
        "link",
        2,
        |_, args, cx| {
            let from = path_arg(args, 0, cx)?;
            let to = path_arg(args, 1, cx)?;
            // The new name reaches the same file: whoever may use it must already have been
            // allowed to read and write the file.
            crate::permission::read(cx, &from)?;
            crate::permission::write(cx, &from)?;
            crate::permission::write_here(cx, &to)?;
            std::fs::hard_link(&from, &to).map_err(|e| io_error2(cx, e, "link", &from, &to))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "truncate",
        2,
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            let length = arg(args, 1).as_number().unwrap_or(0.).max(0.) as u64;
            crate::permission::write(cx, &path)?;
            OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_len(length))
                .map_err(|e| io_error(cx, e, "open", &path))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    ("utimes", 3, |_, args, cx| utimes(args, cx, true), true),
    ("lutimes", 3, |_, args, cx| utimes(args, cx, false), true),
    (
        "chmod",
        2,
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            let mode = mode_arg(&arg(args, 1), cx)?;
            crate::permission::write(cx, &path)?;
            chmod_path(&path, mode).map_err(|e| io_error(cx, e, "chmod", &path))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "chown",
        3,
        // Ownership is not this runtime's to change: the file must be there, and stays as it is.
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::write(cx, &path)?;
            std::fs::metadata(&path).map_err(|e| io_error(cx, e, "chown", &path))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "lchown",
        3,
        |_, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::write_here(cx, &path)?;
            std::fs::symlink_metadata(&path).map_err(|e| io_error(cx, e, "lchown", &path))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    ("open", 3, |_, args, cx| open(args, cx), false),
    (
        "close",
        1,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            if descriptors(cx).open.borrow_mut().remove(&fd).is_none() {
                return Err(fs_error(cx, "EBADF", "close", None, None));
            }
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fstat",
        2,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            let meta = with_fd(cx, fd, "fstat", |open| open.file.metadata())?;
            stats_object(&meta, flag(&arg(args, 1), "bigint", cx)?, cx)
        },
        false,
    ),
    (
        "fsync",
        1,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            with_fd(cx, fd, "fsync", |open| open.file.sync_all())?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fdatasync",
        1,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            with_fd(cx, fd, "fdatasync", |open| open.file.sync_data())?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "ftruncate",
        2,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            let length = arg(args, 1).as_number().unwrap_or(0.).max(0.) as u64;
            with_fd(cx, fd, "ftruncate", |open| open.file.set_len(length))?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "futimes",
        3,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            let accessed = time_of(&arg(args, 1), cx)?;
            let modified = time_of(&arg(args, 2), cx)?;
            with_fd(cx, fd, "futime", |open| {
                set_times(&open.file, accessed, modified)
            })?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fchmod",
        2,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            let mode = mode_arg(&arg(args, 1), cx)?;
            let path = with_fd(cx, fd, "fchmod", |open| Ok(open.path.clone()))?;
            chmod_path(&path, mode).map_err(|e| io_error(cx, e, "fchmod", &path))?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fchown",
        3,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            with_fd(cx, fd, "fchown", |_| Ok(()))?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    ("opendir", 2, |_, args, cx| open_dir(args, cx), true),
];

/// A path answered as text, or as a `Buffer` when `{ encoding: 'buffer' }` asks.
fn bytes_or_text(path: &str, options: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    match encoding_of(options, context)?.as_deref() {
        Some("buffer") => crate::encoding::make_buffer(context, path.as_bytes().to_vec()),
        _ => Ok(js_text(path)),
    }
}

// ─── Dir ────────────────────────────────────────────────────────────────────────────────────

/// `opendirSync(path)`: a `Dir`, its entries read when it is opened and handed out one at a time.
fn open_dir(args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::read(context, &path)?;
    let options = object(context);
    options.set(js_string!("withFileTypes"), true, false, context)?;
    let entries = read_dir(&[js_text(&path), options.into()], context)?;
    let dir = object(context);
    dir.set_prototype(Some(prototype_of("Dir", context)?));
    dir.set(js_string!("path"), js_text(&path), false, context)?;
    hidden(&dir, js_string!("__entries"), entries, context)?;
    hidden(&dir, js_string!("__at"), 0, context)?;
    Ok(dir.into())
}

fn dir_next(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let Some(dir) = this.as_object() else {
        return Ok(JsValue::null());
    };
    let entries = crate::native::list_of(&dir.get(js_string!("__entries"), context)?, context)?;
    let at = dir.get(js_string!("__at"), context)?.to_number(context)? as usize;
    hidden(&dir, js_string!("__at"), (at + 1) as f64, context)?;
    Ok(entries.get(at).cloned().unwrap_or(JsValue::null()))
}

const DIR_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("readSync", 0, |this, _, cx| dir_next(this, cx)),
    ("read", 1, |this, args, cx| {
        let next = dir_next(this, cx);
        match args.first().filter(|c| c.is_callable()) {
            Some(callback) => {
                let callback_args = match next {
                    Ok(entry) => vec![JsValue::null(), entry],
                    Err(e) => vec![e.into_opaque(cx)?],
                };
                crate::microtask(cx, callback.clone(), callback_args);
                Ok(JsValue::undefined())
            }
            None => settled(next, cx),
        }
    }),
    ("closeSync", 0, |_, _, _| Ok(JsValue::undefined())),
    ("close", 1, |_, args, cx| {
        match args.first().filter(|c| c.is_callable()) {
            Some(callback) => {
                crate::microtask(cx, callback.clone(), vec![JsValue::null()]);
                Ok(JsValue::undefined())
            }
            None => settled(Ok(JsValue::undefined()), cx),
        }
    }),
    ("entries", 0, |this, _, _| Ok(this.clone())),
    ("next", 0, |this, _, cx| {
        // The async iterator: `for await (const entry of dir)`.
        let entry = dir_next(this, cx)?;
        let result = object(cx);
        result.set(js_string!("done"), entry.is_null(), false, cx)?;
        result.set(
            js_string!("value"),
            if entry.is_null() {
                JsValue::undefined()
            } else {
                entry
            },
            false,
            cx,
        )?;
        settled(Ok(result.into()), cx)
    }),
];

// ─── Watchers ───────────────────────────────────────────────────────────────────────────────

fn emitter_class(context: &mut Context) -> JsResult<JsObject> {
    crate::modules::node_module(context, "events")?
        .as_object()
        .ok_or_else(|| crate::native::type_error("EventEmitter is not a class"))
}

const WATCHER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("close", 0, |this, _, cx| {
        let Some(watcher) = this.as_object() else {
            return Ok(JsValue::undefined());
        };
        if !watcher.get(js_string!("__closed"), cx)?.to_boolean() {
            hidden(&watcher, js_string!("__closed"), true, cx)?;
            let emit = watcher.get(js_string!("emit"), cx)?;
            if emit.is_callable() {
                crate::call(cx, &emit, this, &[js_text("close")])?;
            }
        }
        Ok(JsValue::undefined())
    }),
    ("ref", 0, |this, _, _| Ok(this.clone())),
    ("unref", 0, |this, _, _| Ok(this.clone())),
];

/// A watcher of the class `name` (`FSWatcher`, `StatWatcher`), with `listener` on `event`.
fn watcher(
    context: &mut Context,
    name: &str,
    event: &str,
    listener: Option<&JsValue>,
) -> JsResult<JsValue> {
    let fs = crate::modules::node_module(context, "fs")?;
    let class = get(&fs, name, context)?;
    let class = class
        .as_object()
        .ok_or_else(|| crate::native::type_error("not a class"))?;
    let watcher = class.construct(&[], None, context)?;
    if let Some(listener) = listener {
        invoke(
            &watcher.clone().into(),
            "on",
            &[js_text(event), listener.clone()],
            context,
        )?;
    }
    Ok(watcher.into())
}

// ─── FileHandle ─────────────────────────────────────────────────────────────────────────────

fn handle_fd(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let fd = get(this, "fd", context)?;
    if fd.as_number().is_some_and(|n| n < 0.) {
        return Err(coded_error(context, "file closed", "EBADF"));
    }
    Ok(fd)
}

/// Calls the synchronous operation `name` with the handle's descriptor first, as a promise.
fn handle_op(
    this: &JsValue,
    name: &str,
    rest: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let outcome = (|| {
        let fd = handle_fd(this, context)?;
        let fs = crate::modules::node_module(context, "fs")?;
        let mut args = vec![fd];
        args.extend_from_slice(rest);
        invoke(&fs, &format!("{name}Sync"), &args, context)
    })();
    settled(outcome, context)
}

const FILE_HANDLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("read", 4, |this, args, cx| {
        let outcome = (|| {
            let fd = handle_fd(this, cx)?;
            let mut all = vec![fd];
            all.extend_from_slice(args);
            let (read, buffer) = read(&all, cx)?;
            let out = object(cx);
            out.set(js_string!("bytesRead"), read, false, cx)?;
            out.set(js_string!("buffer"), buffer, false, cx)?;
            Ok(out.into())
        })();
        settled(outcome, cx)
    }),
    ("write", 4, |this, args, cx| {
        let outcome = (|| {
            let fd = handle_fd(this, cx)?;
            let mut all = vec![fd];
            all.extend_from_slice(args);
            let (written, data) = write(&all, cx)?;
            let out = object(cx);
            out.set(js_string!("bytesWritten"), written, false, cx)?;
            out.set(js_string!("buffer"), data, false, cx)?;
            Ok(out.into())
        })();
        settled(outcome, cx)
    }),
    ("readFile", 1, |this, args, cx| {
        handle_op(this, "readFile", args, cx)
    }),
    ("writeFile", 2, |this, args, cx| {
        handle_op(this, "writeFile", args, cx)
    }),
    ("appendFile", 2, |this, args, cx| {
        handle_op(this, "appendFile", args, cx)
    }),
    ("stat", 1, |this, args, cx| {
        handle_op(this, "fstat", args, cx)
    }),
    ("truncate", 1, |this, args, cx| {
        handle_op(this, "ftruncate", args, cx)
    }),
    ("sync", 0, |this, args, cx| {
        handle_op(this, "fsync", args, cx)
    }),
    ("datasync", 0, |this, args, cx| {
        handle_op(this, "fdatasync", args, cx)
    }),
    ("chmod", 1, |this, args, cx| {
        handle_op(this, "fchmod", args, cx)
    }),
    ("chown", 2, |this, args, cx| {
        handle_op(this, "fchown", args, cx)
    }),
    ("utimes", 2, |this, args, cx| {
        handle_op(this, "futimes", args, cx)
    }),
    ("close", 0, |this, _, cx| {
        let outcome = (|| {
            let fd = get(this, "fd", cx)?;
            if fd.as_number().is_some_and(|n| n >= 0.) {
                let fs = crate::modules::node_module(cx, "fs")?;
                invoke(&fs, "closeSync", &[fd], cx)?;
                if let Some(handle) = this.as_object() {
                    handle.set(js_string!("fd"), -1, false, cx)?;
                }
            }
            Ok(JsValue::undefined())
        })();
        settled(outcome, cx)
    }),
];

// ─── The module ─────────────────────────────────────────────────────────────────────────────

/// `fs.promises.readFile`: the operation's outcome as a promise.
fn promised(
    this: &JsValue,
    args: &[JsValue],
    op: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let outcome = crate::call(context, op, this, args);
    settled(outcome, context)
}

/// The callback a callback form was given: its last argument, which must be a function.
fn callback_of<'a>(
    args: &'a [JsValue],
    context: &mut Context,
) -> JsResult<(JsValue, &'a [JsValue])> {
    match args.last() {
        Some(last) if last.is_callable() => Ok((last.clone(), &args[..args.len() - 1])),
        last => Err(coded_error(
            context,
            &format!(
                "The \"cb\" argument must be of type function. Received {}",
                last.map_or("undefined".to_owned(), |l| l.type_of().to_string())
            ),
            "ERR_INVALID_ARG_TYPE",
        )),
    }
}

/// `fs.readFile(path, …, callback)`: the callback called, once this turn is over, with the
/// error or `null` and the answer.
fn with_callback(
    this: &JsValue,
    args: &[JsValue],
    op: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (callback, rest) = callback_of(args, context)?;
    let outcome = crate::call(context, op, this, rest);
    let callback_args = match outcome {
        Ok(value) => vec![JsValue::null(), value],
        Err(error) => vec![error.into_opaque(context)?],
    };
    crate::microtask(context, callback, callback_args);
    Ok(JsValue::undefined())
}

fn class_on(context: &mut Context, fs: &JsObject, spec: ClassSpec<'_>) -> JsResult<JsObject> {
    let name = spec.name.to_owned();
    let (class, prototype) = class(context, spec)?;
    fs.set(JsString::from(name.as_str()), class, false, context)?;
    Ok(prototype)
}

pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let fs = object(context);
    let promises = object(context);
    for (name, length, operation, in_promises) in OPERATIONS {
        let op: JsValue = function(context, &format!("{name}Sync"), *length, *operation).into();
        fs.set(
            JsString::from(format!("{name}Sync")),
            op.clone(),
            false,
            context,
        )?;
        if *in_promises {
            let promise = closure(context, name, *length, promised, op.clone());
            promises.set(JsString::from(*name), promise, false, context)?;
        }
        let callback = closure(context, name, *length + 1, with_callback, op);
        fs.set(JsString::from(*name), callback, false, context)?;
    }
    // `realpath.native`, the same here.
    for name in ["realpath", "realpathSync"] {
        let f = fs.get(JsString::from(name), context)?;
        if let Some(f) = f.as_object() {
            f.set(js_string!("native"), f.clone(), false, context)?;
        }
    }

    // What does not fit the pattern: `exists`, `read` and `write`, `open` as a promise.
    crate::native::exports(
        context,
        &fs,
        &[
            // A path the realm may not read does not exist, as far as it can tell.
            ("existsSync", 1, |_, args, cx| {
                Ok(match path_of(&arg(args, 0), "path", cx) {
                    Ok(path) => {
                        crate::permission::read(cx, &path).is_ok() && Path::new(&path).exists()
                    }
                    Err(_) => false,
                }
                .into())
            }),
            ("exists", 2, |_, args, cx| {
                let (callback, rest) = callback_of(args, cx)?;
                let exists = match path_of(&arg(rest, 0), "path", cx) {
                    Ok(path) => {
                        crate::permission::read(cx, &path).is_ok() && Path::new(&path).exists()
                    }
                    Err(_) => false,
                };
                crate::microtask(cx, callback, vec![exists.into()]);
                Ok(JsValue::undefined())
            }),
            ("readSync", 5, |_, args, cx| Ok(read(args, cx)?.0.into())),
            ("read", 6, |_, args, cx| {
                let (callback, rest) = callback_of(args, cx)?;
                let callback_args = match read(rest, cx) {
                    Ok((n, buffer)) => vec![JsValue::null(), n.into(), buffer],
                    Err(e) => vec![e.into_opaque(cx)?],
                };
                crate::microtask(cx, callback, callback_args);
                Ok(JsValue::undefined())
            }),
            ("writeSync", 5, |_, args, cx| Ok(write(args, cx)?.0.into())),
            ("write", 6, |_, args, cx| {
                let (callback, rest) = callback_of(args, cx)?;
                let callback_args = match write(rest, cx) {
                    Ok((n, data)) => vec![JsValue::null(), n.into(), data],
                    Err(e) => vec![e.into_opaque(cx)?],
                };
                crate::microtask(cx, callback, callback_args);
                Ok(JsValue::undefined())
            }),
            ("watch", 3, |_, args, cx| {
                let watched = path_of(&arg(args, 0), "filename", cx)?;
                crate::permission::read(cx, &watched)?;
                let listener = args.iter().skip(1).find(|a| a.is_callable());
                watcher(cx, "FSWatcher", "change", listener)
            }),
            ("watchFile", 3, |_, args, cx| {
                let watched = path_of(&arg(args, 0), "filename", cx)?;
                crate::permission::read(cx, &watched)?;
                let listener = args.iter().skip(1).find(|a| a.is_callable());
                watcher(cx, "StatWatcher", "change", listener)
            }),
            ("unwatchFile", 2, |_, _, _| Ok(JsValue::undefined())),
        ],
    )?;
    let open_handle = function(context, "open", 3, |_, args, cx| {
        let outcome = (|| {
            let fd = open(args, cx)?;
            let fs = crate::modules::node_module(cx, "fs")?;
            let class = get(&fs, "FileHandle", cx)?;
            let class = class
                .as_object()
                .ok_or_else(|| crate::native::type_error("not a class"))?;
            Ok(class.construct(&[fd], None, cx)?.into())
        })();
        settled(outcome, cx)
    });
    promises.set(js_string!("open"), open_handle, false, context)?;
    let watch = function(context, "watch", 2, |_, args, cx| {
        let watched = path_of(&arg(args, 0), "filename", cx)?;
        crate::permission::read(cx, &watched)?;
        // An async iterator that never yields: nothing here watches the disk.
        let iterator = object(cx);
        methods(
            cx,
            &iterator,
            &[
                ("next", 0, |_, _, cx| {
                    Ok(JsPromise::new_pending(cx).0.into())
                }),
                ("return", 0, |_, _, cx| {
                    let done = object(cx);
                    done.set(js_string!("done"), true, false, cx)?;
                    settled(Ok(done.into()), cx)
                }),
            ],
        )?;
        let symbol = boa_engine::JsSymbol::async_iterator();
        let itself = function(cx, "[Symbol.asyncIterator]", 0, |this, _, _| {
            Ok(this.clone())
        });
        iterator.set(symbol, itself, false, cx)?;
        Ok(iterator.into())
    });
    promises.set(js_string!("watch"), watch, false, context)?;

    // The classes.
    class_on(
        context,
        &fs,
        ClassSpec {
            name: "Stats",
            length: 14,
            init: |this, args, cx| {
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
            },
            methods: STATS_METHODS,
            ..ClassSpec::default()
        },
    )?;
    class_on(
        context,
        &fs,
        ClassSpec {
            name: "Dirent",
            length: 3,
            init: |this, args, cx| {
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
            },
            methods: DIRENT_METHODS,
            ..ClassSpec::default()
        },
    )?;
    let dir_prototype = class_on(
        context,
        &fs,
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
            &fs,
            ClassSpec {
                name,
                parent: Some(&emitter),
                init: emitter_init,
                methods: WATCHER_METHODS,
                ..ClassSpec::default()
            },
        )?;
    }
    let handle_prototype = class_on(
        context,
        &fs,
        ClassSpec {
            name: "FileHandle",
            length: 1,
            init: |this, args, cx| {
                emitter_init(this, &[], cx)?;
                this.set(js_string!("fd"), arg(args, 0), false, cx)
                    .map(|_| ())
            },
            parent: Some(&emitter),
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
    let file_handle = fs.get(js_string!("FileHandle"), context)?;
    promises.set(js_string!("FileHandle"), file_handle, false, context)?;

    let constants = crate::os::constants_object(context, crate::os::FS_CONSTANTS)?;
    fs.set(js_string!("constants"), constants.clone(), false, context)?;
    promises.set(js_string!("constants"), constants, false, context)?;
    for (name, value) in [("F_OK", 0), ("R_OK", 4), ("W_OK", 2), ("X_OK", 1)] {
        fs.set(JsString::from(name), value, false, context)?;
    }
    fs.set(js_string!("promises"), promises, false, context)?;
    // The file streams are the `stream` module's `Readable` and `Writable`, made when first
    // used: requiring `fs` does not make the stream classes.
    crate::native::exports(
        context,
        &fs,
        &[
            ("createReadStream", 2, |_, args, cx| {
                crate::stream::create_read_stream(cx, args)
            }),
            ("createWriteStream", 2, |_, args, cx| {
                crate::stream::create_write_stream(cx, args)
            }),
        ],
    )?;
    crate::native::accessors(
        context,
        &fs,
        &[
            ("ReadStream", |_, _, cx| stream_class(cx, "Readable"), None),
            ("WriteStream", |_, _, cx| stream_class(cx, "Writable"), None),
        ],
    )?;
    Ok(fs.into())
}

/// `fs.ReadStream` and `fs.WriteStream`: the `stream` classes the file streams are made of.
fn stream_class(context: &mut Context, name: &str) -> JsResult<JsValue> {
    let stream = crate::modules::node_module(context, "stream")?;
    get(&stream, name, context)
}

/// The `EventEmitter` part of a watcher or a handle, set up as `EventEmitter.call(this)` would.
fn emitter_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    let emitter = emitter_class(context)?;
    crate::call(context, &emitter.into(), &this.clone().into(), &[])?;
    Ok(())
}

/// `require('fs/promises')`.
pub(crate) fn promises_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let fs = crate::modules::node_module(context, "fs")?;
    get(&fs, "promises", context)
}
