//! The disk behind an overlay: every read `fs` makes of a path goes through here, so an
//! overlay answers for the paths it holds — a packed `app.asar` — and the disk for the rest.
//! See [`crate::vfs`].
//!
//! A call's [`View`] is made by the export it came through and passed down, so `original-fs`'s
//! operations read the disk alone while `fs`'s read through the realm's layers. Paths reach an
//! overlay made absolute against the realm's working folder and clean, never as the caller wrote
//! them. Writes pass one gate here: the permission first, then the overlay's refusal.

use std::io;
use std::path::{Path, PathBuf};

use boa_engine::{Context, JsResult};

use crate::vfs::{self, View};

use super::fd::Handle;
use super::stat::{
    DIRENT_DIR, DIRENT_FILE, DIRENT_LINK, S_IFMT, StatNumbers, dirent_kind, stat_numbers,
};
use super::{Flags, io_error, open_with};

/// Whether a write reaches one name, the path and what is behind it, or the tree under it.
#[derive(Clone, Copy)]
pub(crate) enum Scope {
    /// The name itself, not what it leads to: `unlink`, `lutimes`.
    Here,
    /// The path and what it leads to: `writeFile`, `chmod`.
    Path,
    /// Everything under it: `rm -r`, `mv`.
    Tree,
}

/// Throws unless the realm may write `path`: the permission first, then the overlay's own
/// refusal. That order is Electron's — an asar path is granted as the folder it sits in — and it
/// is kept in this one place so no entry point can get it wrong.
pub(crate) fn may_write(
    view: &View,
    context: &mut Context,
    path: &str,
    syscall: &str,
    scope: Scope,
) -> JsResult<()> {
    may_write_all(view, context, &[(path, scope)], syscall)
}

/// The same for a call that writes two names, `rename` and `link`: both permissions are asked
/// before the overlay is asked about either, so a refused name is never answered by the
/// overlay's words.
pub(crate) fn may_write_all(
    view: &View,
    context: &mut Context,
    paths: &[(&str, Scope)],
    syscall: &str,
) -> JsResult<()> {
    for (path, scope) in paths {
        match scope {
            Scope::Here => crate::permission::write_here(context, path)?,
            Scope::Path => crate::permission::write(context, path)?,
            Scope::Tree => crate::permission::write_tree(context, path)?,
        }
    }
    for (path, _) in paths {
        if let Some(error) = view.write_denied(Path::new(path)) {
            return Err(io_error(view, context, error, syscall, path, None));
        }
    }
    Ok(())
}

/// What a path is, from the overlay that holds it or from the disk.
pub(crate) enum Meta {
    Disk(std::fs::Metadata),
    Overlay(vfs::Metadata),
}

impl Meta {
    pub(super) fn numbers(&self) -> StatNumbers {
        match self {
            Self::Disk(meta) => stat_numbers(meta),
            Self::Overlay(meta) => {
                let mode = if meta.mode & S_IFMT == 0 {
                    meta.kind.mode() | meta.mode
                } else {
                    meta.mode
                };
                let size = meta.size as f64;
                StatNumbers {
                    dev: 0.,
                    mode: f64::from(mode),
                    nlink: 1.,
                    uid: 0.,
                    gid: 0.,
                    rdev: 0.,
                    blksize: 4096.,
                    ino: 0.,
                    size,
                    blocks: (size / 4096.).ceil() * 8.,
                    atime: meta.mtime_ms,
                    mtime: meta.mtime_ms,
                    ctime: meta.mtime_ms,
                    birthtime: meta.mtime_ms,
                }
            }
        }
    }

    pub(super) fn is_dir(&self) -> bool {
        match self {
            Self::Disk(meta) => meta.is_dir(),
            Self::Overlay(meta) => meta.kind == vfs::Kind::Dir,
        }
    }

    pub(super) fn is_file(&self) -> bool {
        match self {
            Self::Disk(meta) => meta.is_file(),
            Self::Overlay(meta) => meta.kind == vfs::Kind::File,
        }
    }

    pub(super) fn is_readonly(&self) -> bool {
        match self {
            Self::Disk(meta) => meta.permissions().readonly(),
            Self::Overlay(meta) => meta.mode & 0o200 == 0,
        }
    }
}

/// What `path` is, following a link at its end when `follow` says so.
pub(crate) fn stat(view: &View, path: &Path, follow: bool) -> io::Result<Meta> {
    if let Some(found) = view.stat(path) {
        return found.map(Meta::Overlay);
    }
    let meta = if follow {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    }?;
    Ok(Meta::Disk(meta))
}

pub(crate) fn read(view: &View, path: &Path) -> io::Result<Vec<u8>> {
    match view.read(path) {
        Some(bytes) => bytes.map(|bytes| bytes.into_owned()),
        None => std::fs::read(path),
    }
}

pub(crate) fn read_to_string(view: &View, path: &Path) -> io::Result<String> {
    let bytes = read(view, path)?;
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The names in `path`, each with the kind Node's `Dirent` codes it as. An entry the disk
/// fails to read is left out, as `readdir` leaves it out; [`read_dir_whole`] fails on it.
pub(crate) fn read_dir(view: &View, path: &Path) -> io::Result<Vec<(String, u32)>> {
    read_entries(view, path, false)
}

/// The same, but an entry that cannot be read is the error: what `cp` copies must not be
/// silently short.
pub(crate) fn read_dir_whole(view: &View, path: &Path) -> io::Result<Vec<(String, u32)>> {
    read_entries(view, path, true)
}

fn read_entries(view: &View, path: &Path, whole: bool) -> io::Result<Vec<(String, u32)>> {
    if let Some(entries) = view.read_dir(path) {
        return Ok(entries?
            .into_iter()
            .map(|entry| (entry.name, dirent_code(entry.kind)))
            .collect());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)? {
        match entry {
            Ok(entry) => names.push((
                entry.file_name().to_string_lossy().into_owned(),
                dirent_kind(entry.file_type()),
            )),
            Err(error) if whole => return Err(error),
            Err(_) => {}
        }
    }
    Ok(names)
}

fn dirent_code(kind: vfs::Kind) -> u32 {
    match kind {
        vfs::Kind::File => DIRENT_FILE,
        vfs::Kind::Dir => DIRENT_DIR,
        vfs::Kind::Symlink => DIRENT_LINK,
    }
}

pub(crate) fn realpath(view: &View, path: &Path) -> io::Result<PathBuf> {
    match view.realpath(path) {
        Some(found) => found,
        None => std::fs::canonicalize(path),
    }
}

pub(crate) fn exists(view: &View, path: &Path) -> bool {
    stat(view, path, true).is_ok()
}

pub(crate) fn is_file(view: &View, path: &Path) -> bool {
    stat(view, path, true).is_ok_and(|meta| meta.is_file())
}

pub(crate) fn is_dir(view: &View, path: &Path) -> bool {
    stat(view, path, true).is_ok_and(|meta| meta.is_dir())
}

/// Opens `path` as `flags` would. Only the overlay is asked first, so a disk open pays no stat
/// it did not pay before; a file it holds is its bytes kept here.
pub(crate) fn open(view: &View, path: &Path, flags: Flags) -> io::Result<Handle> {
    let reading = flags.read && !flags.write && !flags.append;
    if reading
        && let Some(Ok(meta)) = view.stat(path)
        && meta.kind == vfs::Kind::File
    {
        let bytes = view
            .read(path)
            .expect("the overlay just answered for this file")?;
        return Ok(Handle::Overlay {
            bytes: bytes.into_owned(),
            position: 0,
            meta,
        });
    }
    open_with(&path.to_string_lossy(), flags).map(Handle::Disk)
}

/// Whether `path` could be opened for reading: the disk really opens it, an overlay's file is
/// asked about. What a file stream decides its `open` and `ready` on.
pub(crate) fn probe_open(view: &View, path: &Path) -> io::Result<()> {
    match view.stat(path) {
        Some(found) => found.and_then(|meta| match meta.kind {
            vfs::Kind::Dir => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            _ => Ok(()),
        }),
        None => std::fs::File::open(path).map(|_| ()),
    }
}

/// Copies `from` to `to`: a file only an overlay holds, from its bytes; the disk's, as the disk
/// copies it. `copyFile` and `cp -r` both come here.
pub(crate) fn copy(view: &View, from: &Path, to: &Path) -> io::Result<()> {
    match view.read(from) {
        Some(bytes) => std::fs::write(to, bytes?),
        None => std::fs::copy(from, to).map(|_| ()),
    }
}

/// What a stream keeps of a file only an overlay holds, so it asks the overlay once and slices
/// from there for every chunk after: the path it was read under, and its bytes.
pub(crate) type Held = Option<(PathBuf, Vec<u8>)>;

/// The bytes of `path` from `position` on, at most `size` of them. A file an overlay holds is
/// read whole into `held` the first time and sliced from it after; the disk's is read where it
/// is wanted.
pub(crate) fn read_range(
    view: &View,
    path: &Path,
    held: &mut Held,
    position: u64,
    size: usize,
) -> io::Result<Vec<u8>> {
    if held.as_ref().is_none_or(|(kept, _)| kept != path) {
        *held = match view.read(path) {
            Some(bytes) => Some((path.to_path_buf(), bytes?.into_owned())),
            None => None,
        };
    }
    if let Some((_, bytes)) = held {
        return Ok(slice(bytes, position, size));
    }
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(position))?;
    let mut bytes = vec![0u8; size];
    let mut filled = 0;
    while filled < size {
        match file.read(&mut bytes[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    bytes.truncate(filled);
    Ok(bytes)
}

fn slice(bytes: &[u8], position: u64, size: usize) -> Vec<u8> {
    let start = (position as usize).min(bytes.len());
    let end = (start + size).min(bytes.len());
    bytes[start..end].to_vec()
}
