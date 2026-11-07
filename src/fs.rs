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

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
    object::builtins::{JsArrayBuffer, JsDate, JsPromise},
};

use crate::native::{
    arg, array, closure, coded_error, function, get, hidden, invoke, js_text, methods, object,
    settled, text_of,
};
use crate::vfs::View;

mod classes;
mod fd;
mod original;
mod stat;
pub(crate) mod sys;

pub(crate) use fd::Handle;
pub(crate) use original::original_module;
pub(crate) use sys::{Scope, may_write, may_write_all};

use classes::{make_classes, share_classes};
use fd::{fd_of, with_fd};
use original::entry;
use stat::{DIRENT_DIR, dirent, plain, prototype_of, stats_object};

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
        "ENOTSUP" => "operation not supported on socket",
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
        K::Unsupported => "ENOTSUP",
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
    error_with_message(context, &message, code, syscall, path, dest)
}

/// An `fs` error with the words the caller says it in: an overlay's own, or Node's.
fn error_with_message(
    context: &mut Context,
    message: &str,
    code: &str,
    syscall: &str,
    path: Option<&str>,
    dest: Option<&str>,
) -> JsError {
    let error = JsNativeError::error()
        .with_message(message.to_owned())
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

/// An `io::Error` as `fs` throws it: what the overlay makes of it when the path is its own,
/// else Node's words. `dest` is the other end of a two-path call, as `rename` has one.
fn io_error(
    view: &View,
    context: &mut Context,
    error: std::io::Error,
    syscall: &str,
    path: &str,
    dest: Option<&str>,
) -> JsError {
    let code = code_of(&error);
    if let Some(message) = view.error_message(&error, Path::new(path)) {
        return error_with_message(context, &message, code, syscall, Some(path), dest);
    }
    fs_error(context, code, syscall, Some(path), dest)
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
            let buffer = JsValue::from(crate::native::buffer_class(context)?);
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

/// One of `fs`'s operations: what it does with the arguments, read and written through the
/// `View` the export that made it chose — the realm's layers, or the disk alone for
/// `original-fs`.
type Operation = fn(&View, &[JsValue], &mut Context) -> JsResult<JsValue>;

/// How a file is opened, from Node's flags: `'r'`, `'w+'`, `'ax'`… or `O_*` bits.
#[derive(Clone, Copy, Default)]
pub(crate) struct Flags {
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
fn opening(view: &View, context: &mut Context, path: &str, flags: Flags) -> JsResult<()> {
    if flags.read {
        crate::permission::read(context, path)?;
    }
    if flags.write || flags.append || flags.create || flags.truncate {
        may_write(view, context, path, "open", Scope::Path)?;
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

// ─── The operations ─────────────────────────────────────────────────────────────────────────

fn read_file(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let options = arg(args, 1);
    let encoding = encoding_of(&options, context)?;
    let bytes = if target.is_number() {
        let fd = fd_of(&target, context)?;
        with_fd(context, fd, "read", |open| open.handle.read_to_end())?
    } else {
        let path = path_of(&target, "path", context)?;
        crate::permission::read(context, &path)?;
        if sys::is_dir(view, Path::new(&path)) {
            return Err(fs_error(context, "EISDIR", "read", None, None));
        }
        sys::read(view, Path::new(&path))
            .map_err(|e| io_error(view, context, e, "open", &path, None))?
    };
    bytes_out(bytes, encoding.as_deref(), context)
}

/// `writeFile` and `appendFile`, to a path or a descriptor.
fn write_file(
    view: &View,
    args: &[JsValue],
    context: &mut Context,
    append: bool,
) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let options = arg(args, 2);
    let encoding = encoding_of(&options, context)?;
    let bytes = bytes_in(&arg(args, 1), encoding.as_deref(), context)?;
    if target.is_number() {
        let fd = fd_of(&target, context)?;
        if fd == 1 || fd == 2 {
            fd::console_write(fd, &bytes, context);
            return Ok(JsValue::undefined());
        }
        with_fd(context, fd, "write", |open| {
            open.handle.write_at(&bytes, None)
        })?;
        return Ok(JsValue::undefined());
    }
    let path = path_of(&target, "path", context)?;
    let flags = flags_of(
        &option(&options, "flag", context)?,
        if append { "a" } else { "w" },
        context,
    )?;
    opening(view, context, &path, flags)?;
    let outcome = open_with(&path, flags).and_then(|mut file| file.write_all(&bytes));
    outcome.map_err(|e| io_error(view, context, e, "open", &path, None))?;
    Ok(JsValue::undefined())
}

fn stat_at(
    view: &View,
    args: &[JsValue],
    context: &mut Context,
    follow: bool,
) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    if follow {
        crate::permission::read(context, &path)?;
    } else {
        crate::permission::read_here(context, &path)?;
    }
    let options = arg(args, 1);
    match sys::stat(view, Path::new(&path), follow) {
        Ok(meta) => stats_object(&meta, flag(&options, "bigint", context)?, context),
        Err(e) => {
            // `{ throwIfNoEntry: false }`: a missing file is `undefined`, not an error.
            let quiet = option(&options, "throwIfNoEntry", context)?;
            if e.kind() == std::io::ErrorKind::NotFound && quiet.is_boolean() && !quiet.to_boolean()
            {
                return Ok(JsValue::undefined());
            }
            Err(io_error(
                view,
                context,
                e,
                if follow { "stat" } else { "lstat" },
                &path,
                None,
            ))
        }
    }
}

fn read_dir(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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
        let entries = match sys::read_dir(view, &dir) {
            Ok(entries) => entries,
            Err(e) if first => return Err(io_error(view, context, e, "scandir", &path, None)),
            Err(_) => continue,
        };
        first = false;
        let mut entries: Vec<(String, u32)> = entries;
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

fn make_dir(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    may_write(view, context, &path, "mkdir", Scope::Path)?;
    let options = arg(args, 1);
    if !flag(&options, "recursive", context)? {
        std::fs::create_dir(&path).map_err(|e| io_error(view, context, e, "mkdir", &path, None))?;
        return Ok(JsValue::undefined());
    }
    // Recursively, the answer is the first folder made — none when all were there.
    let absolute = std::path::absolute(&path).unwrap_or_else(|_| PathBuf::from(&path));
    let mut first = None;
    let mut at = Some(absolute.as_path());
    while let Some(dir) = at {
        if sys::exists(view, dir) {
            break;
        }
        first = Some(dir.to_path_buf());
        at = dir.parent();
    }
    if sys::is_file(view, Path::new(&path)) {
        return Err(fs_error(context, "EEXIST", "mkdir", Some(&path), None));
    }
    std::fs::create_dir_all(&path).map_err(|e| io_error(view, context, e, "mkdir", &path, None))?;
    Ok(first.map_or(JsValue::undefined(), |dir| js_text(&dir.to_string_lossy())))
}

fn make_temp_dir(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
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
        may_write(view, context, &path, "mkdtemp", Scope::Path)?;
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(js_text(&path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(io_error(
                    view,
                    context,
                    e,
                    "mkdtemp",
                    &format!("{prefix}XXXXXX"),
                    None,
                ));
            }
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

fn remove(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::write_here(context, &path)?;
    let options = arg(args, 1);
    let (recursive, force) = (
        flag(&options, "recursive", context)?,
        flag(&options, "force", context)?,
    );
    let scope = if recursive { Scope::Tree } else { Scope::Here };
    may_write(view, context, &path, "rm", scope)?;
    let meta = match sys::stat(view, Path::new(&path), false) {
        Ok(meta) => meta,
        Err(e) if force && e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JsValue::undefined());
        }
        Err(e) => return Err(io_error(view, context, e, "lstat", &path, None)),
    };
    let outcome = if meta.is_dir() {
        if !recursive {
            return Err(is_dir_error(context, "rm", &path));
        }
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_file(&path)
    };
    outcome.map_err(|e| io_error(view, context, e, "rm", &path, None))?;
    Ok(JsValue::undefined())
}

fn remove_dir(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    let recursive = flag(&arg(args, 1), "recursive", context)?;
    let scope = if recursive { Scope::Tree } else { Scope::Here };
    may_write(view, context, &path, "rmdir", scope)?;
    let outcome = if recursive {
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_dir(&path)
    };
    outcome.map_err(|e| io_error(view, context, e, "rmdir", &path, None))?;
    Ok(JsValue::undefined())
}

fn copy_tree(
    view: &View,
    from: &Path,
    to: &Path,
    force: bool,
    error_on_exist: bool,
) -> std::io::Result<()> {
    let meta = sys::stat(view, from, true)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for (name, _) in sys::read_dir_whole(view, from)? {
            copy_tree(
                view,
                &from.join(&name),
                &to.join(&name),
                force,
                error_on_exist,
            )?;
        }
        return Ok(());
    }
    if sys::exists(view, to) && !force {
        if error_on_exist {
            return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
        }
        return Ok(());
    }
    sys::copy(view, from, to)
}

/// Throws unless every file `cp` would read under `from` is readable and every one it would
/// write under `to` writable: a link inside the tree is followed, so each is checked where it
/// leads.
fn copy_allowed(context: &mut Context, view: &View, from: &Path, to: &Path) -> JsResult<()> {
    crate::permission::read(context, from)?;
    crate::permission::write(context, to)?;
    if sys::is_dir(view, from) {
        for (name, _) in sys::read_dir(view, from).unwrap_or_default() {
            copy_allowed(context, view, &from.join(&name), &to.join(&name))?;
        }
    }
    Ok(())
}

fn copy(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let from = path_arg(args, 0, context)?;
    let to = path_arg(args, 1, context)?;
    copy_allowed(context, view, Path::new(&from), Path::new(&to))?;
    may_write(view, context, &to, "cp", Scope::Path)?;
    let options = arg(args, 2);
    let recursive = flag(&options, "recursive", context)?;
    let force = match option(&options, "force", context)? {
        f if f.is_undefined() => true,
        f => f.to_boolean(),
    };
    let error_on_exist = flag(&options, "errorOnExist", context)?;
    if sys::is_dir(view, Path::new(&from)) && !recursive {
        return Err(is_dir_error(context, "cp", &from));
    }
    copy_tree(
        view,
        Path::new(&from),
        Path::new(&to),
        force,
        error_on_exist,
    )
    .map_err(|e| io_error(view, context, e, "cp", &from, Some(&to)))?;
    Ok(JsValue::undefined())
}

fn access(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    let mode = arg(args, 1).as_number().unwrap_or(0.) as i64;
    crate::permission::read(context, &path)?;
    if mode & 2 != 0 {
        crate::permission::write(context, &path)?;
    }
    let meta = sys::stat(view, Path::new(&path), true)
        .map_err(|e| io_error(view, context, e, "access", &path, None))?;
    if let Some(error) = sys::write_refusal(view, &meta, Path::new(&path), mode & 2 != 0) {
        return Err(io_error(view, context, error, "access", &path, None));
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

fn utimes(view: &View, args: &[JsValue], context: &mut Context, follow: bool) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    let accessed = time_of(&arg(args, 1), context)?;
    let modified = time_of(&arg(args, 2), context)?;
    let scope = if follow { Scope::Path } else { Scope::Here };
    may_write(view, context, &path, "utime", scope)?;
    open_for_times(&path)
        .and_then(|file| set_times(&file, accessed, modified))
        .map_err(|e| io_error(view, context, e, "utime", &path, None))?;
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

fn symlink(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = path_arg(args, 0, context)?;
    let path = path_arg(args, 1, context)?;
    // What the link leads to is checked whenever it is followed; making it is a write here.
    may_write(view, context, &path, "symlink", Scope::Here)?;
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
    outcome.map_err(|e| io_error(view, context, e, "symlink", &target, Some(&path)))?;
    Ok(JsValue::undefined())
}

fn open(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    let flags = flags_of(&arg(args, 1), "r", context)?;
    opening(view, context, &path, flags)?;
    #[cfg(windows)]
    if flags.read && !flags.write && sys::is_dir(view, Path::new(&path)) {
        return Err(fs_error(context, "EISDIR", "open", Some(&path), None));
    }
    let handle = sys::open(view, Path::new(&path), flags)
        .map_err(|e| io_error(view, context, e, "open", &path, None))?;
    Ok(fd::take(context, handle, path).into())
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
        open.handle.read_into(&mut bytes, position)
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
        fd::console_write(fd, &bytes, context);
        return Ok((bytes.len(), data));
    }
    let written = with_fd(context, fd, "write", |open| {
        open.handle.write_at(&bytes, position)
    })?;
    Ok((written, data))
}

fn statfs(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::read(context, &path)?;
    sys::stat(view, Path::new(&path), true)
        .map_err(|e| io_error(view, context, e, "statfs", &path, None))?;
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

/// `fs.promises.watch(path)`: an async iterator that never yields — nothing here watches the
/// disk — and ends when it is asked to.
fn watched_directory(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let watched = path_of(&arg(args, 0), "filename", context)?;
    crate::permission::read(context, &watched)?;
    let iterator = object(context);
    methods(
        context,
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
    let itself = function(context, "[Symbol.asyncIterator]", 0, |this, _, _| {
        Ok(this.clone())
    });
    iterator.set(symbol, itself, false, context)?;
    Ok(iterator.into())
}

fn unwatch(_: &View, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

/// `fs.existsSync(path)`, and what `fs.exists` answers asynchronously: a path the realm may not
/// read does not exist, as far as it can tell.
fn exists_sync(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(exists(view, args, context)?.into())
}

fn exists(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<bool> {
    let Ok(path) = path_of(&arg(args, 0), "path", context) else {
        return Ok(false);
    };
    Ok(crate::permission::read(context, &path).is_ok() && sys::exists(view, Path::new(&path)))
}

fn exists_callback(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (callback, rest) = callback_of(args, context)?;
    let found = exists(view, rest, context)?;
    crate::microtask(context, callback, vec![found.into()]);
    Ok(JsValue::undefined())
}

fn read_sync(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(read(args, context)?.0.into())
}

fn read_callback(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (callback, rest) = callback_of(args, context)?;
    let callback_args = match read(rest, context) {
        Ok((n, buffer)) => vec![JsValue::null(), n.into(), buffer],
        Err(e) => vec![e.into_opaque(context)?],
    };
    crate::microtask(context, callback, callback_args);
    Ok(JsValue::undefined())
}

fn write_sync(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(write(args, context)?.0.into())
}

fn write_callback(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (callback, rest) = callback_of(args, context)?;
    let callback_args = match write(rest, context) {
        Ok((n, data)) => vec![JsValue::null(), n.into(), data],
        Err(e) => vec![e.into_opaque(context)?],
    };
    crate::microtask(context, callback, callback_args);
    Ok(JsValue::undefined())
}

fn watch(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let watched = path_of(&arg(args, 0), "filename", context)?;
    crate::permission::read(context, &watched)?;
    let listener = args.iter().skip(1).find(|a| a.is_callable());
    watcher(context, "FSWatcher", "change", listener)
}

fn watch_file(_: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let watched = path_of(&arg(args, 0), "filename", context)?;
    crate::permission::read(context, &watched)?;
    let listener = args.iter().skip(1).find(|a| a.is_callable());
    watcher(context, "StatWatcher", "change", listener)
}

/// The operations, each as `fs` has it synchronously; the callback and promise forms are made
/// from these. The last field says whether `fs.promises` has it.
const OPERATIONS: &[(&str, usize, Operation, bool)] = &[
    (
        "readFile",
        2,
        |view, args, cx| read_file(view, args, cx),
        true,
    ),
    (
        "writeFile",
        3,
        |view, args, cx| write_file(view, args, cx, false),
        true,
    ),
    (
        "appendFile",
        3,
        |view, args, cx| write_file(view, args, cx, true),
        true,
    ),
    (
        "stat",
        2,
        |view, args, cx| stat_at(view, args, cx, true),
        true,
    ),
    (
        "lstat",
        2,
        |view, args, cx| stat_at(view, args, cx, false),
        true,
    ),
    ("statfs", 2, |view, args, cx| statfs(view, args, cx), true),
    (
        "readdir",
        2,
        |view, args, cx| read_dir(view, args, cx),
        true,
    ),
    ("mkdir", 2, |view, args, cx| make_dir(view, args, cx), true),
    (
        "mkdtemp",
        2,
        |view, args, cx| make_temp_dir(view, args, cx),
        true,
    ),
    ("rm", 2, |view, args, cx| remove(view, args, cx), true),
    (
        "rmdir",
        2,
        |view, args, cx| remove_dir(view, args, cx),
        true,
    ),
    (
        "unlink",
        1,
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            may_write(view, cx, &path, "unlink", Scope::Here)?;
            std::fs::remove_file(&path)
                .map_err(|e| io_error(view, cx, e, "unlink", &path, None))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "rename",
        2,
        |view, args, cx| {
            let from = path_arg(args, 0, cx)?;
            let to = path_arg(args, 1, cx)?;
            may_write_all(
                view,
                cx,
                &[(&from, Scope::Tree), (&to, Scope::Tree)],
                "rename",
            )?;
            std::fs::rename(&from, &to)
                .map_err(|e| io_error(view, cx, e, "rename", &from, Some(&to)))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "copyFile",
        3,
        |view, args, cx| {
            let from = path_arg(args, 0, cx)?;
            let to = path_arg(args, 1, cx)?;
            let mode = arg(args, 2).as_number().unwrap_or(0.) as i64;
            crate::permission::read(cx, &from)?;
            may_write(view, cx, &to, "copyfile", Scope::Path)?;
            if mode & 1 != 0 && sys::exists(view, Path::new(&to)) {
                return Err(fs_error(cx, "EEXIST", "copyfile", Some(&from), Some(&to)));
            }
            sys::copy(view, Path::new(&from), Path::new(&to))
                .map_err(|e| io_error(view, cx, e, "copyfile", &from, Some(&to)))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    ("cp", 3, |view, args, cx| copy(view, args, cx), true),
    ("access", 2, |view, args, cx| access(view, args, cx), true),
    (
        "realpath",
        2,
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::read(cx, &path)?;
            let real = sys::realpath(view, Path::new(&path))
                .map_err(|e| io_error(view, cx, e, "realpath", &path, None))?;
            bytes_or_text(&plain(&real), &arg(args, 1), cx)
        },
        true,
    ),
    (
        "readlink",
        2,
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            crate::permission::read_here(cx, &path)?;
            let target = std::fs::read_link(&path)
                .map_err(|e| io_error(view, cx, e, "readlink", &path, None))?;
            bytes_or_text(&plain(&target), &arg(args, 1), cx)
        },
        true,
    ),
    ("symlink", 3, |view, args, cx| symlink(view, args, cx), true),
    (
        "link",
        2,
        |view, args, cx| {
            let from = path_arg(args, 0, cx)?;
            let to = path_arg(args, 1, cx)?;
            // The new name reaches the same file: whoever may use it must already have been
            // allowed to read and write the file.
            crate::permission::read(cx, &from)?;
            may_write_all(
                view,
                cx,
                &[(&from, Scope::Path), (&to, Scope::Here)],
                "link",
            )?;
            std::fs::hard_link(&from, &to)
                .map_err(|e| io_error(view, cx, e, "link", &from, Some(&to)))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "truncate",
        2,
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            let length = arg(args, 1).as_number().unwrap_or(0.).max(0.) as u64;
            may_write(view, cx, &path, "open", Scope::Path)?;
            OpenOptions::new()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_len(length))
                .map_err(|e| io_error(view, cx, e, "open", &path, None))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "utimes",
        3,
        |view, args, cx| utimes(view, args, cx, true),
        true,
    ),
    (
        "lutimes",
        3,
        |view, args, cx| utimes(view, args, cx, false),
        true,
    ),
    (
        "chmod",
        2,
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            let mode = mode_arg(&arg(args, 1), cx)?;
            may_write(view, cx, &path, "chmod", Scope::Path)?;
            chmod_path(&path, mode).map_err(|e| io_error(view, cx, e, "chmod", &path, None))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "chown",
        3,
        // Ownership is not this runtime's to change: the file must be there, and stays as it is.
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            may_write(view, cx, &path, "chown", Scope::Path)?;
            sys::stat(view, Path::new(&path), true)
                .map_err(|e| io_error(view, cx, e, "chown", &path, None))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    (
        "lchown",
        3,
        |view, args, cx| {
            let path = path_arg(args, 0, cx)?;
            may_write(view, cx, &path, "lchown", Scope::Here)?;
            sys::stat(view, Path::new(&path), false)
                .map_err(|e| io_error(view, cx, e, "lchown", &path, None))?;
            Ok(JsValue::undefined())
        },
        true,
    ),
    ("open", 3, |view, args, cx| open(view, args, cx), false),
    (
        "close",
        1,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            fd::close(cx, fd)?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fstat",
        2,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            let meta = with_fd(cx, fd, "fstat", |open| open.handle.metadata())?;
            stats_object(&meta, flag(&arg(args, 1), "bigint", cx)?, cx)
        },
        false,
    ),
    (
        "fsync",
        1,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            with_fd(cx, fd, "fsync", |open| open.handle.sync())?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fdatasync",
        1,
        |_, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            with_fd(cx, fd, "fdatasync", |open| open.handle.sync_data())?;
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
            with_fd(cx, fd, "ftruncate", |open| open.handle.set_len(length))?;
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
            with_fd(cx, fd, "futime", |open| match &mut open.handle {
                Handle::Disk(file) => set_times(file, accessed, modified),
                Handle::Overlay { .. } => Ok(()),
            })?;
            Ok(JsValue::undefined())
        },
        false,
    ),
    (
        "fchmod",
        2,
        |view, args, cx| {
            let fd = fd_of(&arg(args, 0), cx)?;
            let mode = mode_arg(&arg(args, 1), cx)?;
            let (virtually, path) = with_fd(cx, fd, "fchmod", |open| {
                Ok((open.handle.is_overlay(), open.path.clone()))
            })?;
            if !virtually {
                chmod_path(&path, mode)
                    .map_err(|e| io_error(view, cx, e, "fchmod", &path, None))?;
            }
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
    (
        "opendir",
        2,
        |view, args, cx| open_dir(view, args, cx),
        true,
    ),
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
fn open_dir(view: &View, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = path_arg(args, 0, context)?;
    crate::permission::read(context, &path)?;
    let options = object(context);
    options.set(js_string!("withFileTypes"), true, false, context)?;
    let entries = read_dir(view, &[js_text(&path), options.into()], context)?;
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

pub(super) const DIR_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
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

pub(super) fn emitter_class(context: &mut Context) -> JsResult<JsObject> {
    crate::modules::node_module(context, "events")?
        .as_object()
        .ok_or_else(|| crate::native::type_error("EventEmitter is not a class"))
}

pub(super) const WATCHER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
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

pub(super) const FILE_HANDLE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
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

/// `fs` and `original-fs`, made by one table: the same exports, the second's reading the disk
/// alone. `fs`'s maker is [`module`], `original-fs`'s is [`original::original_module`].
pub(crate) fn build(context: &mut Context, original: bool) -> JsResult<JsValue> {
    let fs = object(context);
    let promises = object(context);
    table_exports(context, original, &fs, &promises)?;
    other_exports(context, original, &fs, &promises)?;
    if original {
        // One set of classes for both modules, so `original.statSync(x) instanceof fs.Stats`.
        share_classes(context, &fs)?;
    } else {
        make_classes(context, &fs)?;
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
    stream_exports(context, original, &fs)?;
    Ok(fs.into())
}

/// Each of [`OPERATIONS`] as `xSync`, `x` with a callback and, where it has one, `promises.x`.
fn table_exports(
    context: &mut Context,
    original: bool,
    fs: &JsObject,
    promises: &JsObject,
) -> JsResult<()> {
    for (name, length, operation, in_promises) in OPERATIONS {
        let op = entry(
            context,
            original,
            &format!("{name}Sync"),
            *length,
            *operation,
        );
        let op: JsValue = op.into();
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

    Ok(())
}

/// What does not fit the table's pattern.
fn other_exports(
    context: &mut Context,
    original: bool,
    fs: &JsObject,
    promises: &JsObject,
) -> JsResult<()> {
    // `exists`, `read` and `write`, and `open` as a promise.
    for (name, length, operation) in [
        // A path the realm may not read does not exist, as far as it can tell.
        ("existsSync", 1, exists_sync as Operation),
        ("exists", 2, exists_callback),
        ("readSync", 5, read_sync),
        ("read", 6, read_callback),
        ("writeSync", 5, write_sync),
        ("write", 6, write_callback),
        ("watch", 3, watch),
        ("watchFile", 3, watch_file),
        ("unwatchFile", 2, unwatch),
    ] {
        let found = entry(context, original, name, length, operation);
        fs.set(JsString::from(name), found, false, context)?;
    }
    let open_handle = entry(context, original, "open", 3, |view, args, cx| {
        let outcome = (|| {
            let fd = open(view, args, cx)?;
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
    let watch = entry(context, original, "watch", 2, watched_directory);
    promises.set(js_string!("watch"), watch, false, context)?;

    Ok(())
}

/// The file streams are the `stream` module's `Readable` and `Writable`, made when first used:
/// requiring `fs` does not make the stream classes.
fn stream_exports(context: &mut Context, original: bool, fs: &JsObject) -> JsResult<()> {
    for (name, operation) in [
        (
            "createReadStream",
            (|view, args, cx| crate::stream::create_read_stream(view, cx, args)) as Operation,
        ),
        (
            "createWriteStream",
            (|view, args, cx| crate::stream::create_write_stream(view, cx, args)) as Operation,
        ),
    ] {
        let found = entry(context, original, name, 2, operation);
        fs.set(JsString::from(name), found, false, context)?;
    }
    crate::native::accessors(
        context,
        fs,
        &[
            ("ReadStream", |_, _, cx| stream_class(cx, "Readable"), None),
            ("WriteStream", |_, _, cx| stream_class(cx, "Writable"), None),
        ],
    )?;
    Ok(())
}

/// `require('fs')`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    build(context, false)
}

/// `fs.ReadStream` and `fs.WriteStream`: the `stream` classes the file streams are made of.
fn stream_class(context: &mut Context, name: &str) -> JsResult<JsValue> {
    let stream = crate::modules::node_module(context, "stream")?;
    get(&stream, name, context)
}

/// The `EventEmitter` part of a watcher or a handle, set up as `EventEmitter.call(this)` would.
pub(super) fn emitter_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    let emitter = emitter_class(context)?;
    crate::call(context, &emitter.into(), &this.clone().into(), &[])?;
    Ok(())
}

/// `require('fs/promises')`.
pub(crate) fn promises_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let fs = crate::modules::node_module(context, "fs")?;
    get(&fs, "promises", context)
}
