//! The overlay that lays archives over the disk. A path is inside an archive when one of its
//! names ends in `.asar` and the path up to there is a file that is an archive; the rest is the
//! path inside. Anything else is not the overlay's, and the disk answers.
//!
//! `process.noAsar` is the kernel's (`Runtime::set_no_asar`), and `original-fs` never asks an
//! overlay. Electron also ignores asar when `ELECTRON_NO_ASAR` is set, but only in processes
//! that are neither the browser nor a renderer; that is the host's to decide ([`AsarOptions`]).

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};
use verve::vfs::{DirEntry, Kind, Metadata, Overlay};

use super::archive::{Archive, AsarError, Entry, FileEntry, Place};
use super::extract::Extracted;

/// What a host is told when a file does not match its archive's digest.
pub type IntegrityFailure = Arc<dyn Fn(&AsarError) + Send + Sync>;

/// How an [`AsarOverlay`] treats the archives it opens.
#[derive(Clone, Default)]
pub struct AsarOptions {
    /// Check each packed file against the digest in its archive's header every time it is
    /// read — what Electron does when `EnableEmbeddedAsarIntegrityValidation` is on. A file
    /// that fails is an error ([`AsarError::is_integrity`]), reported to `on_integrity_failure`
    /// first.
    pub validate_integrity: bool,
    /// Called, on the thread that read the file, before the read fails. Electron treats this as
    /// fatal (`LOG(FATAL)`); whether to print and exit is the host's to decide.
    pub on_integrity_failure: Option<IntegrityFailure>,
    /// Claim no path, so the disk answers all: what Electron does under `ELECTRON_NO_ASAR` in
    /// a process whose `process.type` is neither `browser` nor `renderer`.
    pub disabled: bool,
}

/// What a name ending in `.asar` turned out to be.
#[derive(Clone)]
enum Slot {
    Archive(Arc<Archive>),
    /// A file whose header is not an archive's.
    Invalid,
    /// A folder, or anything else that is not an archive.
    Plain,
}

/// Archives laid over the disk. Shared by every realm of a process: an archive is opened once.
pub struct AsarOverlay {
    options: AsarOptions,
    enabled: bool,
    archives: Mutex<HashMap<PathBuf, Slot>>,
    extracted: Extracted,
}

impl AsarOverlay {
    /// An overlay with no archive opened yet.
    pub fn new(options: AsarOptions) -> Self {
        Self {
            enabled: !options.disabled,
            options,
            archives: Mutex::default(),
            extracted: Extracted::new(),
        }
    }

    fn slots(&self) -> MutexGuard<'_, HashMap<PathBuf, Slot>> {
        self.archives.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `result`, once the host has been told if it is an integrity failure.
    fn report<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if let (Err(error), Some(tell)) = (&result, &self.options.on_integrity_failure)
            && let Some(failure) = AsarError::of(error).filter(|e| e.is_integrity())
        {
            tell(failure);
        }
        result
    }

    /// What `prefix`, a path that ends in `.asar`, is. A name that does not exist yet is not
    /// remembered, since the archive may be put there later.
    fn slot(&self, prefix: &Path) -> Slot {
        if let Some(slot) = self.slots().get(prefix) {
            return slot.clone();
        }
        let slot = match std::fs::metadata(prefix) {
            Ok(meta) if meta.is_file() => match Archive::open(prefix, &self.options) {
                Ok(archive) => Slot::Archive(archive),
                Err(error) if AsarError::of(&error).is_some() => Slot::Invalid,
                Err(_) => return Slot::Plain,
            },
            Ok(_) => Slot::Plain,
            Err(_) => return Slot::Plain,
        };
        self.slots()
            .entry(prefix.to_path_buf())
            .or_insert(slot)
            .clone()
    }

    /// The archive `path` is in and the path inside it; `None` when it is in none.
    fn locate(&self, path: &Path) -> Option<io::Result<(Arc<Archive>, String)>> {
        if !self.enabled {
            return None;
        }
        let mut prefix = PathBuf::new();
        let mut components = path.components();
        while let Some(component) = components.next() {
            prefix.push(component);
            let Component::Normal(name) = component else {
                continue;
            };
            if !name.to_str().is_some_and(is_archive_name) {
                continue;
            }
            match self.slot(&prefix) {
                Slot::Archive(archive) => {
                    return Some(Ok((archive, inner_path(components.as_path()))));
                }
                // A file that fails to be an archive is still a file: it reads as one.
                Slot::Invalid if components.as_path().as_os_str().is_empty() => {}
                Slot::Invalid => return Some(Err(AsarError::invalid(&prefix))),
                Slot::Plain => {}
            }
        }
        None
    }

    /// Runs `ask` on the archive `path` is in, when it is in one.
    fn within<T>(
        &self,
        path: &Path,
        ask: impl FnOnce(&Archive, &str) -> io::Result<T>,
    ) -> Option<io::Result<T>> {
        let (archive, inner) = match self.locate(path)? {
            Ok(found) => found,
            Err(error) => return Some(Err(error)),
        };
        Some(ask(&archive, &inner))
    }

    fn metadata(archive: &Archive, inner: &str, follow: bool) -> io::Result<Metadata> {
        let found = archive.resolve(inner, follow)?;
        let (size, executable) = match found.entry {
            Entry::Dir(_) => (0, false),
            Entry::File(file) => (file.size, file.executable),
            Entry::Link(target) => (target.len() as u64, false),
        };
        let kind = found.entry.kind();
        // Electron's `statMode`: `0o644` and the type, `0o111` more for a folder, a link or an
        // executable, and `0o022` for a link.
        let mut permissions = 0o644;
        if executable || kind != Kind::File {
            permissions |= 0o111;
        }
        if kind == Kind::Symlink {
            permissions |= 0o022;
        }
        Ok(Metadata::new(
            kind,
            size,
            archive.mtime_ms(),
            kind.mode() | permissions,
        ))
    }

    /// The real file a packed file is extracted to, or an unpacked file or folder where it is.
    fn real_path(&self, archive: &Archive, inner: &str) -> io::Result<PathBuf> {
        let found = archive.resolve(inner, true)?;
        match found.entry {
            Entry::File(file) => match file.place {
                Place::Unpacked => Ok(archive.unpacked_path(&found.path)),
                Place::Packed(_) => self.report(self.extracted.file(archive, &found)),
            },
            Entry::Dir(_) => {
                let folder = archive.unpacked_path(&found.path);
                if folder.is_dir() {
                    Ok(folder)
                } else {
                    Err(AsarError::not_dir())
                }
            }
            Entry::Link(_) => Err(AsarError::not_found(archive.path(), inner)),
        }
    }
}

impl Overlay for AsarOverlay {
    fn stat(&self, path: &Path) -> Option<io::Result<Metadata>> {
        self.within(path, |archive, inner| Self::metadata(archive, inner, true))
    }

    fn lstat(&self, path: &Path) -> Option<io::Result<Metadata>> {
        self.within(path, |archive, inner| Self::metadata(archive, inner, false))
    }

    fn read(&self, path: &Path) -> Option<io::Result<Cow<'static, [u8]>>> {
        self.within(path, |archive, inner| {
            self.report(archive.read(inner)).map(Cow::Owned)
        })
    }

    fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>> {
        self.within(path, |archive, inner| {
            let names = archive.list(inner)?;
            Ok(names
                .into_iter()
                .map(|(name, kind)| DirEntry::new(name, kind))
                .collect())
        })
    }

    fn realpath(&self, path: &Path) -> Option<io::Result<PathBuf>> {
        self.within(path, |archive, inner| {
            let found = archive.resolve(inner, true)?;
            let mut real = archive.real_path().to_path_buf();
            real.extend(found.path.split('/').filter(|part| !part.is_empty()));
            Ok(real)
        })
    }

    /// Anything inside an archive is refused as the operating system refuses a path through a
    /// file: `ENOTDIR`. The archive itself is a file on the disk, and the disk's to answer.
    fn write_denied(&self, path: &Path) -> Option<io::Error> {
        let (_, inner) = self.locate(path)?.ok()?;
        (!inner.is_empty()).then(|| io::ErrorKind::NotADirectory.into())
    }

    fn access_denied(&self, path: &Path) -> Option<io::Error> {
        let (archive, inner) = self.locate(path)?.ok()?;
        let found = archive.resolve(&inner, true).ok()?;
        match found.entry {
            // Electron asks the real file about an unpacked one.
            Entry::File(FileEntry {
                place: Place::Unpacked,
                ..
            }) => {
                let real = std::fs::metadata(archive.unpacked_path(&found.path)).ok()?;
                real.permissions()
                    .readonly()
                    .then(|| io::ErrorKind::PermissionDenied.into())
            }
            _ => Some(AsarError::no_access(&inner)),
        }
    }

    fn materialize(&self, path: &Path) -> Option<io::Result<PathBuf>> {
        self.within(path, |archive, inner| self.real_path(archive, inner))
    }

    fn error_message(&self, err: &io::Error, _: &Path) -> Option<String> {
        AsarError::of(err).map(|error| error.message().to_owned())
    }

    /// A packed file is named by the archive's path and time, and where in it the file is and
    /// how long: a new archive that still holds the file where it was is the same file.
    fn cache_key(&self, path: &Path) -> Option<[u8; 32]> {
        let (archive, inner) = self.locate(path)?.ok()?;
        let found = archive.resolve(&inner, true).ok()?;
        let Entry::File(FileEntry {
            size,
            place: Place::Packed(offset),
            ..
        }) = found.entry
        else {
            return None;
        };
        let mut hash = Sha256::new();
        hash.update(archive.path().to_string_lossy().as_bytes());
        hash.update(offset.to_le_bytes());
        hash.update(size.to_le_bytes());
        hash.update(archive.mtime_ms().to_bits().to_le_bytes());
        Some(hash.finalize().into())
    }
}

/// Whether a name is an archive's: it ends in `.asar`, in any case, as Electron matches it.
fn is_archive_name(name: &str) -> bool {
    name.len()
        .checked_sub(5)
        .and_then(|at| name.get(at..))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(".asar"))
}

/// The path inside an archive: the names of `rest`, `/`-separated.
fn inner_path(rest: &Path) -> String {
    let names: Vec<_> = rest
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect();
    names.join("/")
}
