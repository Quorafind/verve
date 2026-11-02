//! One `.asar` file: its header parsed once, its contents read where they lie.
//!
//! The layout is Chromium's two Pickles, little-endian: the size of the first (4), then the
//! second, whose payload is the length of a JSON string and the string, padded to four bytes.
//! File contents follow the header, and a file's `offset` — a decimal string, which can pass
//! 2^53 — counts from there. A file marked `unpacked` has no offset: it is a real file in
//! `<archive>.unpacked`. A link names another entry by its path from the archive's root.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use verve::encoding::hex;
use verve::vfs::Kind;

use super::integrity::Integrity;
use super::overlay::AsarOptions;

/// The largest header Verve reads: a guard of its own against a header that claims the memory.
const MAX_HEADER: u64 = 256 << 20;
/// How many links a path may pass through before it is taken for a loop.
const MAX_LINKS: usize = 40;
/// What separates the names of a path in a header: Electron splits on both on Windows, where
/// `@electron/asar` writes a link with the separators `path.relative` gave it.
const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
/// Where the header's JSON starts: past the two Pickles' size words.
const JSON_AT: u64 = 16;

/// What went wrong inside an archive, in Electron's words. It rides in an [`io::Error`] whose
/// kind is the closest `fs` knows, and `Overlay::error_message` hands its words to `fs`.
#[derive(Debug)]
pub struct AsarError {
    failure: Failure,
    message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    NotFound,
    NotDir,
    NoAccess,
    Invalid,
    Integrity,
}

impl AsarError {
    fn raise(failure: Failure, message: String) -> io::Error {
        let kind = match failure {
            Failure::NotFound => io::ErrorKind::NotFound,
            Failure::NotDir => io::ErrorKind::NotADirectory,
            Failure::NoAccess => io::ErrorKind::PermissionDenied,
            Failure::Invalid | Failure::Integrity => io::ErrorKind::InvalidData,
        };
        io::Error::new(kind, Self { failure, message })
    }

    /// `ENOENT, <inner> not found in <archive>`: Electron's `AsarError.NOT_FOUND`.
    pub(super) fn not_found(archive: &Path, inner: &str) -> io::Error {
        let message = format!("ENOENT, {inner} not found in {}", archive.display());
        Self::raise(Failure::NotFound, message)
    }

    /// `ENOTDIR, not a directory`: `AsarError.NOT_DIR`.
    pub(super) fn not_dir() -> io::Error {
        Self::raise(Failure::NotDir, "ENOTDIR, not a directory".to_owned())
    }

    /// `EACCES: permission denied, access '<inner>'`: `AsarError.NO_ACCESS`.
    pub(super) fn no_access(inner: &str) -> io::Error {
        let message = format!("EACCES: permission denied, access '{inner}'");
        Self::raise(Failure::NoAccess, message)
    }

    /// `Invalid package <archive>`: `AsarError.INVALID_ARCHIVE`.
    pub(super) fn invalid(archive: &Path) -> io::Error {
        let message = format!("Invalid package {}", archive.display());
        Self::raise(Failure::Invalid, message)
    }

    fn integrity(archive: &Path, inner: &str) -> io::Error {
        let message = format!(
            "Integrity check failed for asar archive {}: {inner}",
            archive.display()
        );
        Self::raise(Failure::Integrity, message)
    }

    /// The archive's error `error` carries, if it carries one.
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// The words Electron writes it in.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Whether a file's contents did not match the digests its header holds.
    pub fn is_integrity(&self) -> bool {
        self.failure == Failure::Integrity
    }
}

impl std::fmt::Display for AsarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AsarError {}

/// An entry of the archive's tree.
#[derive(Debug)]
pub(crate) enum Entry {
    /// A folder, its entries by name.
    Dir(BTreeMap<String, Entry>),
    /// A file.
    File(FileEntry),
    /// A link, to a path from the archive's root.
    Link(String),
}

impl Entry {
    /// What `fs` calls this entry.
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Dir(_) => Kind::Dir,
            Self::File(_) => Kind::File,
            Self::Link(_) => Kind::Symlink,
        }
    }
}

/// A file's header record.
#[derive(Debug)]
pub(crate) struct FileEntry {
    /// Its length in bytes.
    pub(crate) size: u64,
    /// Where its bytes are.
    pub(crate) place: Place,
    /// Whether it is marked executable.
    pub(crate) executable: bool,
    integrity: Option<Integrity>,
}

/// Where a file's bytes are.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Place {
    /// In the archive, this far past the header.
    Packed(u64),
    /// On the disk, in `<archive>.unpacked`.
    Unpacked,
}

/// An entry found, and the path it is at once the links on the way are followed.
pub(crate) struct Resolved<'a> {
    /// The entry.
    pub(crate) entry: &'a Entry,
    /// Its path from the archive's root, `/`-separated, without links.
    pub(crate) path: String,
}

/// An opened archive. Reads are positional and take no lock, so any thread may read at once.
pub struct Archive {
    path: PathBuf,
    real_path: PathBuf,
    base: u64,
    len: u64,
    json_len: u64,
    root: Entry,
    file: File,
    mtime_ms: f64,
    validate: bool,
}

impl Archive {
    /// Opens the archive at `path`, reading and parsing its header. With
    /// `options.validate_integrity`, every packed file that has a digest is checked against it
    /// each time it is read.
    ///
    /// # Errors
    /// `Invalid package` when the header is not an archive's; the disk's own error when the
    /// file cannot be read.
    pub fn open(path: &Path, options: &AsarOptions) -> io::Result<Arc<Self>> {
        let file = File::open(path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        let invalid = || AsarError::invalid(path);
        let (json_len, header_size) = read_sizes(&file, len)?.ok_or_else(invalid)?;
        let mut json = vec![0; json_len as usize];
        read_at(&file, &mut json, JSON_AT)?;
        let node: Node = serde_json::from_slice(&json).map_err(|_| invalid())?;
        let root = entry(node, false)
            .filter(|root| matches!(root, Entry::Dir(_)))
            .ok_or_else(invalid)?;
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0., |since| since.as_secs_f64() * 1000.);
        Ok(Arc::new(Self {
            path: path.to_path_buf(),
            real_path: plain(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())),
            base: 8 + header_size,
            len,
            json_len,
            root,
            file,
            mtime_ms,
            validate: options.validate_integrity,
        }))
    }

    /// Where the archive is.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the archive really is, found once when it was opened: what `realpath` answers for
    /// the part of a path before the archive's contents.
    pub(crate) fn real_path(&self) -> &Path {
        &self.real_path
    }

    /// When the archive was last written, in milliseconds since the epoch: what every entry's
    /// times are.
    pub(crate) fn mtime_ms(&self) -> f64 {
        self.mtime_ms
    }

    /// The SHA-256 of the header's JSON as lowercase hex: the digest `ElectronAsarIntegrity`
    /// holds, and what names this archive's version.
    ///
    /// # Errors
    /// The disk's error, if the header cannot be read again.
    pub fn header_hash(&self) -> io::Result<String> {
        let mut json = vec![0; self.json_len as usize];
        read_at(&self.file, &mut json, JSON_AT)?;
        Ok(hex(&Sha256::digest(&json)))
    }

    /// The entry at `inner` — a `/`-separated path from the root, empty for the root — with
    /// links on the way followed, and one at its end too when `follow` says so.
    ///
    /// # Errors
    /// `ENOENT, <inner> not found in <archive>` when there is no such entry, a path through a
    /// file, or links that loop (Electron answers a loop as not found too).
    pub(crate) fn resolve(&self, inner: &str, follow: bool) -> io::Result<Resolved<'_>> {
        let missing = || AsarError::not_found(&self.path, inner);
        let mut pending: VecDeque<String> = parts(inner).ok_or_else(missing)?.into();
        let mut real = Vec::new();
        let mut entry = &self.root;
        let mut links = 0;
        while let Some(name) = pending.pop_front() {
            let Entry::Dir(children) = entry else {
                return Err(missing());
            };
            let child = children.get(&name).ok_or_else(missing)?;
            match child {
                Entry::Link(target) if follow || !pending.is_empty() => {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(missing());
                    }
                    for part in parts(target).ok_or_else(missing)?.into_iter().rev() {
                        pending.push_front(part);
                    }
                    real.clear();
                    entry = &self.root;
                }
                _ => {
                    real.push(name);
                    entry = child;
                }
            }
        }
        Ok(Resolved {
            entry,
            path: real.join("/"),
        })
    }

    /// The bytes of the file at `inner`.
    ///
    /// # Errors
    /// Not found, `EISDIR` for a folder, or what [`Archive::read_found`] fails with.
    pub(crate) fn read(&self, inner: &str) -> io::Result<Vec<u8>> {
        self.read_found(&self.resolve(inner, true)?)
    }

    /// The bytes of the file `found` is: from the archive, or from `.unpacked` for a file that
    /// is not packed. A packed file with a digest is checked against it when the archive was
    /// opened to validate.
    ///
    /// # Errors
    /// `EISDIR` for a folder; `Invalid package` when the header puts the file outside the
    /// archive; an integrity failure; the disk's error.
    pub(crate) fn read_found(&self, found: &Resolved<'_>) -> io::Result<Vec<u8>> {
        let Entry::File(file) = found.entry else {
            return Err(io::ErrorKind::IsADirectory.into());
        };
        let Place::Packed(offset) = file.place else {
            return std::fs::read(self.unpacked_path(&found.path));
        };
        let start = self.base.checked_add(offset);
        let within = start
            .and_then(|start| start.checked_add(file.size))
            .is_some_and(|end| end <= self.len);
        let size = usize::try_from(file.size).ok().filter(|_| within);
        let (Some(start), Some(size)) = (start, size) else {
            return Err(AsarError::invalid(&self.path));
        };
        let mut bytes = vec![0; size];
        read_at(&self.file, &mut bytes, start)?;
        if self.validate && file.integrity.as_ref().is_some_and(|i| !i.accepts(&bytes)) {
            return Err(AsarError::integrity(&self.path, &found.path));
        }
        Ok(bytes)
    }

    /// The names in the folder at `inner`, each with its kind.
    ///
    /// # Errors
    /// Not found, or `ENOTDIR, not a directory` for a file.
    pub(crate) fn list(&self, inner: &str) -> io::Result<Vec<(String, Kind)>> {
        match self.resolve(inner, true)?.entry {
            Entry::Dir(children) => Ok(children
                .iter()
                .map(|(name, entry)| (name.clone(), entry.kind()))
                .collect()),
            _ => Err(AsarError::not_dir()),
        }
    }

    /// Where `inner` is when it is unpacked: `<archive>.unpacked/<inner>`.
    pub(crate) fn unpacked_path(&self, inner: &str) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(".unpacked");
        let mut path = PathBuf::from(name);
        path.extend(inner.split('/').filter(|part| !part.is_empty()));
        path
    }
}

/// The header JSON's length and the size of the header Pickle, when the first 16 bytes and the
/// file's length say it is an archive.
fn read_sizes(file: &File, len: u64) -> io::Result<Option<(u64, u64)>> {
    let mut prefix = [0u8; 16];
    if len < JSON_AT {
        return Ok(None);
    }
    read_at(file, &mut prefix, 0)?;
    let word = |at: usize| {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&prefix[at..at + 4]);
        u64::from(u32::from_le_bytes(bytes))
    };
    let (first, header_size, payload, json_len) = (word(0), word(4), word(8), word(12));
    let sane = first == 4
        && header_size <= MAX_HEADER
        && payload + 4 == header_size
        && json_len + 8 <= header_size
        && 8 + header_size <= len;
    Ok(sane.then_some((json_len, header_size)))
}

/// Fills `buffer` from `at`, without moving any shared position.
fn read_at(file: &File, buffer: &mut [u8], at: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buffer, at)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut buffer, mut at) = (buffer, at);
        while !buffer.is_empty() {
            match file.seek_read(buffer, at)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                read => {
                    buffer = &mut buffer[read..];
                    at += read as u64;
                }
            }
        }
        Ok(())
    }
}

/// A path's names, with `.` and `..` read lexically; `None` when `..` climbs out of the root.
fn parts(text: &str) -> Option<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for part in text.split(SEPARATORS) {
        match part {
            "" | "." => {}
            ".." => {
                names.pop()?;
            }
            name => names.push(name.to_owned()),
        }
    }
    Some(names)
}

/// A header record, before it is an [`Entry`].
#[derive(Deserialize)]
struct Node {
    files: Option<BTreeMap<String, Node>>,
    size: Option<u64>,
    offset: Option<Offset>,
    #[serde(default)]
    unpacked: bool,
    #[serde(default)]
    executable: bool,
    link: Option<String>,
    integrity: Option<Integrity>,
}

/// An offset is a decimal string, since JSON numbers stop being exact at 2^53; older archives
/// wrote a number.
#[derive(Deserialize)]
#[serde(untagged)]
enum Offset {
    Text(String),
    Number(u64),
}

impl Offset {
    fn value(&self) -> Option<u64> {
        match self {
            Self::Text(text) => text.parse().ok(),
            Self::Number(number) => Some(*number),
        }
    }
}

/// The entry `node` records. Everything under a folder marked `unpacked` is unpacked.
fn entry(node: Node, unpacked: bool) -> Option<Entry> {
    let unpacked = unpacked || node.unpacked;
    if let Some(link) = node.link {
        return Some(Entry::Link(link));
    }
    if let Some(files) = node.files {
        // A name that is a path of its own would reach out of its folder when it is joined to
        // one; Electron's reader does not find such an entry either.
        let children = files
            .into_iter()
            .filter(|(name, _)| is_plain_name(name))
            .map(|(name, child)| Some((name, entry(child, unpacked)?)))
            .collect::<Option<_>>()?;
        return Some(Entry::Dir(children));
    }
    let place = if unpacked {
        Place::Unpacked
    } else {
        Place::Packed(node.offset?.value()?)
    };
    Some(Entry::File(FileEntry {
        size: node.size?,
        place,
        executable: node.executable,
        integrity: node.integrity,
    }))
}

/// Whether `name` can be one name in a folder: not empty, `.` or `..`, and no separator.
fn is_plain_name(name: &str) -> bool {
    !matches!(name, "" | "." | "..") && !name.contains(['/', '\\', '\0'])
}

/// `path` without Windows' verbatim prefix, which Node's `realpath` does not carry.
fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}
