//! A virtual filesystem laid over the real one: the paths an [`Overlay`] holds are answered by
//! it, and every other path by the disk, so `fs` and `require` read a packed application's files
//! where they are packed.
//!
//! Electron does this for `app.asar`: a folder packed into one file that `fs` reads as a folder.
//! An embedder sets its overlay with [`crate::Runtime::set_vfs`], `process.noAsar` puts it aside,
//! and `original-fs` never reads through it.
//!
//! What an overlay answers is a reading of a path, not the file itself: a stat, bytes, names in
//! a folder. Writes are the overlay's to refuse — `app.asar` is read-only — and the error it
//! refuses with is the one `fs` throws. An overlay is always asked about the path made absolute
//! against the realm's own working folder and cleaned, never the string the caller wrote.
//!
//! Which overlay a call reads through is decided by the export it came through, not by anything
//! set while it runs: `fs`'s exports make a [`View`] of the realm's layers, `original-fs`'s make
//! one of the disk alone, and each passes it down. JavaScript that `fs` runs meanwhile — a path
//! object's `toString` — reads through the realm's own layers again.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use boa_engine::Context;

/// What a path inside an overlay is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A file, whose bytes [`Overlay::read`] answers.
    File,
    /// A folder, whose names [`Overlay::read_dir`] answers.
    Dir,
    /// A link, which [`Overlay::realpath`] follows.
    Symlink,
}

impl Kind {
    /// `fs.Stats.mode`'s kind bits for this kind: `S_IFREG`, `S_IFDIR` or `S_IFLNK`.
    pub(crate) fn mode(self) -> u32 {
        match self {
            Self::File => 0o100000,
            Self::Dir => 0o040000,
            Self::Symlink => 0o120000,
        }
    }
}

/// What an overlay's `stat` answers: what a packed file has, and nothing of a device's.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Metadata {
    /// Whether the path is a file, a folder or a link.
    pub kind: Kind,
    /// Its length in bytes; 0 for a folder.
    pub size: u64,
    /// Modified time, in milliseconds since the Unix epoch.
    pub mtime_ms: f64,
    /// The file's mode, as `fs.Stats.mode` has it: kind bits and permissions.
    pub mode: u32,
}

impl Metadata {
    /// A `Metadata` of `kind`, `size` bytes long, last written at `mtime_ms`, with `mode`.
    pub fn new(kind: Kind, size: u64, mtime_ms: f64, mode: u32) -> Self {
        Self {
            kind,
            size,
            mtime_ms,
            mode,
        }
    }
}

/// One name in an overlay's folder.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DirEntry {
    /// The name alone, as `fs.Dirent.name` has it.
    pub name: String,
    /// Whether it is a file, a folder or a link.
    pub kind: Kind,
}

impl DirEntry {
    /// A `DirEntry` named `name` of `kind`.
    pub fn new(name: String, kind: Kind) -> Self {
        Self { name, kind }
    }
}

/// A virtual filesystem an embedder reads through: `app.asar`, unpacked or not.
///
/// Every question is asked of the path as `fs` was given it, made absolute against the realm's
/// working folder and clean; `None` answers "not mine", and the disk is read instead.
pub trait Overlay: Send + Sync {
    /// What `path` is, when it is this overlay's.
    fn stat(&self, path: &Path) -> Option<io::Result<Metadata>>;

    /// What `path` is itself, a link at its end not followed — what `lstat` asks. An overlay
    /// without links answers as [`Overlay::stat`] does.
    fn lstat(&self, path: &Path) -> Option<io::Result<Metadata>> {
        self.stat(path)
    }

    /// The bytes of `path`, when it is this overlay's file.
    fn read(&self, path: &Path) -> Option<io::Result<Cow<'static, [u8]>>>;

    /// The names in `path`, when it is this overlay's folder.
    fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>>;

    /// Where `path` really is, when it is this overlay's: a packed file is where it is packed.
    fn realpath(&self, path: &Path) -> Option<io::Result<PathBuf>>;

    /// The error a write to `path` fails with, when the overlay holds it and keeps it as it is
    /// — a path inside `app.asar` fails as a path through a file does, with `ENOTDIR`.
    fn write_denied(&self, _path: &Path) -> Option<io::Error> {
        None
    }

    /// The error `access(path, W_OK)` fails with, when the overlay holds the path and it is not
    /// writable. Apart from [`Overlay::write_denied`] because the two differ: Electron asks the
    /// real file about an unpacked one, and words the refusal for a packed one its own way.
    fn access_denied(&self, path: &Path) -> Option<io::Error> {
        self.write_denied(path)
    }

    /// Where a file only the overlay holds can be read by a program that is not this runtime —
    /// a script `child_process` runs, a native module loaded: what it is unpacked to.
    fn materialize(&self, _path: &Path) -> Option<io::Result<PathBuf>> {
        None
    }

    /// The message this overlay writes for `err` on `path`, when the error is its own — the
    /// words of Electron's `fs`.
    fn error_message(&self, _err: &io::Error, _path: &Path) -> Option<String> {
        None
    }

    /// The key this overlay's `path` is cached under by the code cache, when it has one: a
    /// packed file keeps its identity across versions of the archive that holds it.
    fn cache_key(&self, _path: &Path) -> Option<[u8; 32]> {
        None
    }
}

// SAFETY: an overlay holds no garbage-collected value — it is the embedder's own, and `View`
// keeps it only for as long as a call, or a stream, reads through it.
unsafe impl boa_gc::Trace for View {
    boa_gc::empty_trace!();
}

impl boa_gc::Finalize for View {}

/// The overlays a realm reads through, and whether `process.noAsar` puts them aside.
#[derive(Default)]
pub(crate) struct Layers {
    pub(crate) overlay: Option<Arc<dyn Overlay>>,
    pub(crate) no_asar: bool,
}

/// The overlay a call reads through, and the folder its relative paths are made absolute
/// against. Cheap to clone, and made by the export that starts the call, so what `fs` runs
/// meanwhile cannot change what the call sees.
#[derive(Clone, Default)]
pub(crate) struct View {
    overlay: Option<Arc<dyn Overlay>>,
    cwd: PathBuf,
}

impl View {
    /// The overlay `context`'s realm reads through — none while `process.noAsar` is on — or the
    /// disk alone when `original` says so: what `original-fs`'s exports make.
    pub(crate) fn new(context: &Context, original: bool) -> Self {
        let state = crate::shared(context);
        let cwd = PathBuf::from(state.cwd.borrow().clone());
        let layers = state.vfs.borrow();
        let overlay = if original || layers.no_asar {
            None
        } else {
            layers.overlay.clone()
        };
        drop(layers);
        Self { overlay, cwd }
    }

    /// The overlay `context`'s realm reads through, for the modules that are not `fs`'s own:
    /// `require`, the ES module loader, a stream reading on its own.
    pub(crate) fn of(context: &Context) -> Self {
        Self::new(context, false)
    }

    /// The disk alone.
    pub(crate) fn disk() -> Self {
        Self::default()
    }

    /// The path an overlay knows it by: absolute against this realm's folder, and clean. `None`
    /// when the path names nothing an overlay could hold.
    fn absolute(&self, path: &Path) -> Option<PathBuf> {
        if path.as_os_str().is_empty() {
            return None;
        }
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        Some(crate::modules::clean(&joined))
    }

    pub(crate) fn stat(&self, path: &Path) -> Option<io::Result<Metadata>> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.stat(&path)
    }

    pub(crate) fn lstat(&self, path: &Path) -> Option<io::Result<Metadata>> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.lstat(&path)
    }

    pub(crate) fn read(&self, path: &Path) -> Option<io::Result<Cow<'static, [u8]>>> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.read(&path)
    }

    pub(crate) fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.read_dir(&path)
    }

    pub(crate) fn realpath(&self, path: &Path) -> Option<io::Result<PathBuf>> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.realpath(&path)
    }

    pub(crate) fn write_denied(&self, path: &Path) -> Option<io::Error> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.write_denied(&path)
    }

    pub(crate) fn access_denied(&self, path: &Path) -> Option<io::Error> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.access_denied(&path)
    }

    pub(crate) fn materialize(&self, path: &Path) -> Option<io::Result<PathBuf>> {
        let overlay = self.overlay.as_ref()?;
        let path = self.absolute(path)?;
        overlay.materialize(&path)
    }

    pub(crate) fn error_message(&self, err: &io::Error, path: &Path) -> Option<String> {
        let path = self.absolute(path)?;
        self.overlay.as_ref()?.error_message(err, &path)
    }
}
