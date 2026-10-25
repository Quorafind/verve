//! The code cache: the compiled code of the ES modules a runtime loads, kept on disk so the next
//! run reads it back instead of parsing and compiling the same source again. Off until an
//! embedder gives it a directory and a secret ([`crate::Runtime::set_code_cache_dir_with_secret`];
//! `verve run` takes `--code-cache=<dir>` or `VERVE_CODE_CACHE`, with the user's own secret, see
//! [`user_secret`]). See `docs/startup.md`.
//!
//! The entries of one build live in one file, `<dir>/<build>.pack`: opening a file costs more
//! than reading one (on Windows, with a scanner looking at each, about half a millisecond), so a
//! run opens the pack once, reads its index, and reads each module's entry from it when the
//! module is loaded; nothing else of it stays in memory. What a run compiles is added to the
//! pack when its loading settles (see [`flush`]), when the process exits and when the runtime
//! is dropped.
//!
//! # Trust
//!
//! What is read back is run as code, so an entry is only read if it is authentic: it carries an
//! HMAC-SHA256 of its key and code under a 32-byte secret that is **not** in the directory —
//! the embedder's, or for `verve run` the user's, kept in a private file of their own. Anyone
//! who can write the directory but not read the secret can make modules compile again, nothing
//! more. Its key, a SHA-256 of this build's fingerprint, the realm's global scope and the
//! whole source text, means an entry is only found for the very source, build and global scope
//! it was compiled for.
//!
//! An entry that passes is still checked to be well formed by Boa before it runs (see
//! `boa_engine::code_cache`), and anything refused falls back to compiling the source. The pack
//! is written to a temporary file renamed into place. The directory is made private to the user
//! (owner-only on Windows, `0700` elsewhere, where one owned by someone else or writable by
//! others is refused), and JavaScript the runtime runs can neither read nor write it, nor the
//! user's secret, whatever its file permissions grant.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

use boa_engine::{Context, JsResult, Module, Source};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"VERVEPK\x02";
const TAG_DOMAIN: &[u8] = b"verve code cache entry v1";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 32;
/// The most a pack holds; past it, the entries no run has read lately go first. A larger file
/// is not a pack this build wrote, and is not read.
const MAX_PACK: usize = 64 << 20;
/// How long new entries wait for more to join them before the pack is written.
const SETTLE: Duration = Duration::from_millis(100);

/// The length of an entry in a pack's index: its key, where its code is and how long, its tag.
const INDEX_ENTRY: usize = KEY_LEN + 8 + 8 + TAG_LEN;

type Key = [u8; KEY_LEN];
type Tag = [u8; TAG_LEN];

/// Where an entry's code is in a pack (from the start of the code), how long it is, and its
/// tag.
#[derive(Clone, Copy)]
struct Located {
    offset: u64,
    len: u64,
    tag: Tag,
}

/// A directory of compiled modules, and the secret its entries are signed with.
pub(crate) struct CodeCache {
    dir: PathBuf,
    pack: PathBuf,
    /// Files JavaScript may not reach besides the directory: the secret's, for `verve run`.
    protected: Vec<PathBuf>,
    secret: [u8; 32],
    fingerprint: [u8; 32],
    state: RefCell<Pack>,
    stats: std::cell::Cell<CodeCacheStats>,
}

/// What a runtime's code cache has done since it was set: see
/// [`crate::Runtime::code_cache_stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodeCacheStats {
    /// Modules read back from the cache instead of parsed.
    pub hits: u64,
    /// Modules with no entry, parsed and compiled.
    pub misses: u64,
    /// Modules whose entry was there but not read: not authentic, or refused by Boa as not
    /// well formed for this source, build and realm. They were parsed and compiled.
    pub refused: u64,
    /// Modules compiled and kept for later runs.
    pub stored: u64,
}

/// The pack as this run knows it.
#[derive(Default)]
struct Pack {
    /// Whether the pack has been opened (whether or not there was one).
    opened: bool,
    /// The pack as it was when it was opened: a pack written since replaces the file, not what
    /// this handle reads.
    file: Option<fs::File>,
    /// Where the code of the pack's entries starts in it.
    code_start: u64,
    /// Each entry of the pack, checked when it is read.
    index: HashMap<Key, Located>,
    /// The entries this run read, which stay ahead of those it did not.
    used: HashSet<Key>,
    /// What this run compiled and has not written yet, with its tag.
    pending: Vec<(Key, Vec<u8>, Tag)>,
    /// When the first pending entry came.
    pending_since: Option<Instant>,
}

impl std::fmt::Debug for CodeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeCache")
            .field("pack", &self.pack)
            .finish_non_exhaustive()
    }
}

impl CodeCache {
    /// The cache in `dir`, made private to the user if it is not there, its entries signed
    /// with `secret`. `protected` are files of the secret's that JavaScript may not reach.
    ///
    /// # Errors
    ///
    /// If the directory cannot be made, or is not private to the user.
    pub(crate) fn open(dir: &Path, secret: [u8; 32], protected: Vec<PathBuf>) -> io::Result<Self> {
        create_private_dir(dir)?;
        check_private(dir)?;
        let fingerprint = build_fingerprint();
        let mut name = hex(&fingerprint[..8]);
        name.push_str(".pack");
        let dir = crate::permission::real(dir, true);
        Ok(Self {
            pack: dir.join(name),
            dir,
            protected: protected
                .iter()
                .map(|path| crate::permission::real(path, true))
                .collect(),
            secret,
            fingerprint,
            state: RefCell::default(),
            stats: std::cell::Cell::default(),
        })
    }

    /// Whether `path` (resolved as the permission model resolves it) is the cache directory,
    /// inside it, or a file of its secret.
    fn protects(&self, path: &Path) -> bool {
        within(path, &self.dir) || self.protected.iter().any(|file| within(path, file))
    }

    /// The key of the entry for `source`, compiled against a global scope of digest `global`.
    fn key(&self, source: &str, global: u64) -> Key {
        let mut hash = Sha256::new();
        hash.update(b"verve code cache key v2\0module\0");
        hash.update(self.fingerprint);
        hash.update(global.to_le_bytes());
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source.as_bytes());
        hash.finalize().into()
    }

    fn mac(&self, key: &Key, payload: &[u8]) -> Hmac<Sha256> {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&self.secret).expect("HMAC takes a key of any size");
        mac.update(TAG_DOMAIN);
        mac.update(key);
        mac.update(&(payload.len() as u64).to_le_bytes());
        mac.update(payload);
        mac
    }

    fn count(&self, update: impl FnOnce(&mut CodeCacheStats)) {
        let mut stats = self.stats.get();
        update(&mut stats);
        self.stats.set(stats);
    }

    /// What the cache has done so far.
    pub(crate) fn stats(&self) -> CodeCacheStats {
        self.stats.get()
    }

    /// The compiled code kept for `source`, if there is an authentic entry for it; `Err` if
    /// there is an entry that is not.
    pub(crate) fn load(&self, source: &str, global: u64) -> Result<Option<Vec<u8>>, ()> {
        let key = self.key(source, global);
        let mut state = self.state.borrow_mut();
        if !state.opened {
            state.opened = true;
            if let Ok(mut file) = open_no_follow(&self.pack)
                && let Some((index, code_start)) = read_index(&mut file)
            {
                state.index = index;
                state.code_start = code_start;
                state.file = Some(file);
            }
        }
        let Some(located) = state.index.get(&key).copied() else {
            return Ok(None);
        };
        let code_start = state.code_start;
        let file = state.file.as_mut().ok_or(())?;
        let payload = read_code(file, code_start, located).ok_or(())?;
        // Constant time: how much of a forged tag matched says nothing.
        self.mac(&key, &payload)
            .verify_slice(&located.tag)
            .map_err(|_| ())?;
        state.used.insert(key);
        Ok(Some(payload))
    }

    /// Keeps `payload`, compiled from `source`, for the next run.
    pub(crate) fn store(&self, source: &str, global: u64, payload: Vec<u8>) {
        let key = self.key(source, global);
        let tag = self.mac(&key, &payload).finalize().into_bytes().into();
        let mut state = self.state.borrow_mut();
        state.pending.push((key, payload, tag));
        state.pending_since.get_or_insert_with(Instant::now);
    }

    /// Writes what this run compiled into the pack, with what is there already, if there is
    /// anything to write and `settled` (or it has waited long enough for more to join it).
    /// Failing to is no error: the next run compiles again.
    fn write(&self, settled: bool) {
        let mut state = self.state.borrow_mut();
        let Some(since) = state.pending_since else {
            return;
        };
        if !settled && since.elapsed() < SETTLE {
            return;
        }
        // What another process wrote since this one opened the pack is kept too, if it is
        // authentic: an entry written without the secret goes. What this run read comes from
        // the pack it opened and checked, not from whatever is on disk now. A file too large to
        // be a pack is not read.
        let disk = open_no_follow(&self.pack)
            .and_then(|mut file| {
                use std::io::Read as _;
                let len = file.metadata()?.len();
                if len > (MAX_PACK + 12) as u64 {
                    return Ok(Vec::new());
                }
                let mut bytes = Vec::with_capacity(len as usize);
                file.read_to_end(&mut bytes)?;
                Ok(bytes)
            })
            .unwrap_or_default();
        let on_disk = parse_pack(&disk);
        let authentic =
            |key: &Key, payload: &[u8], tag: &Tag| self.mac(key, payload).verify_slice(tag).is_ok();
        let mut out = PackWriter::default();
        // Newest first, then what this run read, then the rest as they were.
        for (key, payload, tag) in state.pending.iter().rev() {
            out.put(key, payload, tag);
        }
        let used: Vec<Key> = state.used.iter().copied().collect();
        for key in &used {
            let code_start = state.code_start;
            let from_opened = state.index.get(key).copied().and_then(|located| {
                let file = state.file.as_mut()?;
                let payload = read_code(file, code_start, located)?;
                authentic(key, &payload, &located.tag).then_some((payload, located.tag))
            });
            if let Some((payload, tag)) = from_opened {
                out.put(key, &payload, &tag);
            } else if let Some((payload, tag)) = on_disk.entries.get(key)
                && authentic(key, payload, tag)
            {
                out.put(key, payload, tag);
            }
        }
        for key in &on_disk.order {
            if let Some((payload, tag)) = on_disk.entries.get(key)
                && !out.written.contains(key)
                && authentic(key, payload, tag)
            {
                out.put(key, payload, tag);
            }
        }
        let _ = write_atomically(&self.pack, &out.finish());
        // The next load reads the pack just written.
        *state = Pack {
            used: state.used.clone(),
            ..Pack::default()
        };
    }
}

/// A pack being written: its entries in order, until it is full.
#[derive(Default)]
struct PackWriter {
    entries: Vec<(Key, Vec<u8>, Tag)>,
    size: usize,
    written: HashSet<Key>,
}

impl PackWriter {
    /// Adds an entry, unless one with its key is in already or the pack is full.
    fn put(&mut self, key: &Key, payload: &[u8], tag: &Tag) {
        let size = INDEX_ENTRY + payload.len();
        if self.size + size > MAX_PACK || !self.written.insert(*key) {
            return;
        }
        self.size += size;
        self.entries.push((*key, payload.to_vec(), *tag));
    }

    /// The pack's bytes: the header, the index, then the code of each entry.
    fn finish(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + self.size);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        let mut offset = 0u64;
        for (key, payload, tag) in &self.entries {
            bytes.extend_from_slice(key);
            bytes.extend_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
            bytes.extend_from_slice(tag);
            offset += payload.len() as u64;
        }
        for (_, payload, _) in &self.entries {
            bytes.extend_from_slice(payload);
        }
        bytes
    }
}

/// Reads a pack's index from `file`: each entry's place, and where their code starts. `None`
/// if the file is not a pack, or its index is cut short.
fn read_index(file: &mut fs::File) -> Option<(HashMap<Key, Located>, u64)> {
    use std::io::Read as _;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    if &header[..8] != MAGIC.as_slice() {
        return None;
    }
    let count = u32::from_le_bytes(header[8..].try_into().ok()?) as usize;
    let len = file.metadata().ok()?.len();
    // A pack this build wrote is no larger than `MAX_PACK`, nor its index: what claims more is
    // not read, so a file made to look large costs nothing.
    if len > (MAX_PACK + 12) as u64 || count > MAX_PACK / INDEX_ENTRY {
        return None;
    }
    let index_len = count.checked_mul(INDEX_ENTRY)?;
    let code_start = 12u64.checked_add(index_len as u64)?;
    if code_start > len {
        return None;
    }
    let mut bytes = vec![0u8; index_len];
    file.read_exact(&mut bytes).ok()?;
    let code_len = len - code_start;
    let mut index = HashMap::with_capacity(count);
    for entry in bytes.chunks_exact(INDEX_ENTRY) {
        let (key, rest) = entry.split_at(KEY_LEN);
        let (offset, rest) = rest.split_at(8);
        let (code, tag) = rest.split_at(8);
        let located = Located {
            offset: u64::from_le_bytes(offset.try_into().ok()?),
            len: u64::from_le_bytes(code.try_into().ok()?),
            tag: tag.try_into().ok()?,
        };
        // An entry whose code is not inside the pack is left out.
        if located
            .offset
            .checked_add(located.len)
            .is_some_and(|end| end <= code_len)
        {
            index.entry(key.try_into().ok()?).or_insert(located);
        }
    }
    Some((index, code_start))
}

/// Reads an entry's code from the pack `file`.
fn read_code(file: &mut fs::File, code_start: u64, located: Located) -> Option<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(code_start.checked_add(located.offset)?))
        .ok()?;
    let mut code = vec![0u8; usize::try_from(located.len).ok()?];
    file.read_exact(&mut code).ok()?;
    Some(code)
}

/// The entries of a pack read whole, as far as its index holds, by key and in order.
#[derive(Default)]
struct OnDisk<'a> {
    entries: HashMap<Key, (&'a [u8], Tag)>,
    order: Vec<Key>,
}

fn parse_pack(bytes: &[u8]) -> OnDisk<'_> {
    let mut pack = OnDisk::default();
    let Some(rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return pack;
    };
    let Some((count, rest)) = rest.split_first_chunk::<4>() else {
        return pack;
    };
    let count = u32::from_le_bytes(*count) as usize;
    if count > MAX_PACK / INDEX_ENTRY {
        return pack;
    }
    let Some(index_len) = count.checked_mul(INDEX_ENTRY) else {
        return pack;
    };
    if rest.len() < index_len {
        return pack;
    }
    let (index, code) = rest.split_at(index_len);
    for entry in index.chunks_exact(INDEX_ENTRY) {
        let (key, rest) = entry.split_at(KEY_LEN);
        let (offset, rest) = rest.split_at(8);
        let (len, tag) = rest.split_at(8);
        let (Ok(key), Ok(offset), Ok(len), Ok(tag)) = (
            <Key>::try_from(key),
            <[u8; 8]>::try_from(offset),
            <[u8; 8]>::try_from(len),
            <Tag>::try_from(tag),
        ) else {
            continue;
        };
        let (Ok(offset), Ok(len)) = (
            usize::try_from(u64::from_le_bytes(offset)),
            usize::try_from(u64::from_le_bytes(len)),
        ) else {
            continue;
        };
        let Some(payload) = offset
            .checked_add(len)
            .and_then(|end| code.get(offset..end))
        else {
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(slot) = pack.entries.entry(key) {
            pack.order.push(key);
            slot.insert((payload, tag));
        }
    }
    pack
}

impl Drop for CodeCache {
    fn drop(&mut self) {
        self.write(true);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Writes `bytes` to a file of its own beside `path`, then renames it to `path`, so a reader
/// finds the old file or the new one, never part of one.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut nonce = [0u8; 8];
    let _ = getrandom::fill(&mut nonce);
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.{}.tmp", std::process::id(), hex(&nonce)));
    let temp = PathBuf::from(temp);
    let result = (|| {
        let mut file = private_file(&temp)?;
        file.write_all(bytes)?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A new file only its owner can read and write, where the platform says who can.
fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The user's own code cache secret, for `verve run`: 32 random bytes in a file only they can
/// read, outside any cache directory — `%LOCALAPPDATA%\Verve\code-cache-secret` on Windows,
/// `$XDG_CONFIG_HOME/verve/code-cache-secret` (or `~/.config/verve/…`) elsewhere — made the
/// first time. Answers the secret and its file.
///
/// # Errors
///
/// If there is no such place, the file or its folder is not private to the user, or the file is
/// not a secret (the wrong length): the caller runs without a cache then.
pub(crate) fn user_secret() -> io::Result<([u8; 32], PathBuf)> {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("Verve"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(|dir| PathBuf::from(dir).join("verve"))
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config/verve"))
            })
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no per-user folder for the secret"))?;
    create_private_dir(&base)?;
    check_private(&base)?;
    let path = base.join("code-cache-secret");
    Ok((private_secret(&path)?, path))
}

/// The secret in the file `path`, made at random the first time: written whole and synced
/// beside it, then linked into place only if no other process has made one meanwhile, so the
/// first secret made is the one every process uses.
fn private_secret(path: &Path) -> io::Result<[u8; 32]> {
    let read = |path: &Path| -> io::Result<[u8; 32]> {
        use std::io::Read as _;
        check_private(path)?;
        let mut file = open_no_follow(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the code cache secret is not 32 bytes",
            )
        })
    };
    if fs::symlink_metadata(path).is_ok() {
        return read(path);
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| io::Error::other(e.to_string()))?;
    let mut nonce = [0u8; 8];
    let _ = getrandom::fill(&mut nonce);
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".{}.{}.tmp", std::process::id(), hex(&nonce)));
    let temp = PathBuf::from(temp);
    let made = (|| {
        let mut file = private_file(&temp)?;
        file.write_all(&secret)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temp, path) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            // Where links cannot be made, the last secret renamed into place wins.
            Err(_) => fs::rename(&temp, path),
            Ok(()) => Ok(()),
        }
    })();
    let _ = fs::remove_file(&temp);
    made?;
    read(path)
}

/// Whether `path` is `root` or inside it, as paths compare where they are (without case on
/// Windows).
fn within(path: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let path = path.to_string_lossy().to_lowercase().replace('/', "\\");
        let root = root.to_string_lossy().to_lowercase().replace('/', "\\");
        let root = root.trim_end_matches('\\');
        path == root || path.starts_with(&format!("{root}\\"))
    } else {
        path.starts_with(root)
    }
}

/// Opens `path` to read, not following a link at its end.
fn open_no_follow(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Makes the folder `path` (and the folders it is in) if it is not there; the folder itself
/// only its owner can reach: `0700`, or on Windows a protected DACL granting its owner alone.
fn create_private_dir(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let made = make_private_dir(path);
    match made {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        other => other,
    }
}

#[cfg(unix)]
fn make_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(windows)]
fn make_private_dir(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SECURITY_ATTRIBUTES,
        },
        Storage::FileSystem::CreateDirectoryW,
    };
    // Protected (no inherited entries), full access for the folder's owner, inherited by what
    // is made in it.
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)\0".encode_utf16().collect();
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut descriptor = std::ptr::null_mut();
    // Safety: `sddl` is a NUL-terminated wide string; `descriptor` receives a descriptor
    // allocated by the system, freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // Safety: `wide` is a NUL-terminated wide path and `attributes` points at a valid
    // descriptor for the length of the call.
    let made = unsafe { CreateDirectoryW(wide.as_ptr(), &raw const attributes) };
    let error = io::Error::last_os_error();
    // Safety: `descriptor` was allocated by the conversion above.
    unsafe { LocalFree(descriptor) };
    if made == 0 { Err(error) } else { Ok(()) }
}

#[cfg(not(any(unix, windows)))]
fn make_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// Refuses `path` unless only its owner — the user this runs as — can change it: on Unix, not a
/// link, owned by the effective user, and not writable by its group or others. Windows folders
/// this makes are owner-only by their DACL.
fn check_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)?;
        // Safety: `geteuid` only reads the process's credentials.
        let euid = unsafe { libc::geteuid() };
        if metadata.file_type().is_symlink()
            || metadata.uid() != euid
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is not private to this user (owned by them alone, not a link, and \
                     writable by no one else)",
                    path.display()
                ),
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// What compiled code depends on besides its source: Boa's format and opcodes, Verve's
/// version, and this executable (path, size and modification time), so any rebuild starts a
/// cache afresh.
fn build_fingerprint() -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"verve build v1\0");
    hash.update(boa_engine::code_cache::fingerprint().to_le_bytes());
    hash.update(env!("CARGO_PKG_VERSION").as_bytes());
    hash.update([0]);
    if let Ok(exe) = std::env::current_exe() {
        hash.update(exe.to_string_lossy().as_bytes());
        hash.update([0]);
        if let Ok(metadata) = fs::metadata(&exe) {
            hash.update(metadata.len().to_le_bytes());
            if let Ok(modified) = metadata.modified()
                && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
            {
                hash.update(since.as_nanos().to_le_bytes());
            }
        }
    }
    hash.finalize().into()
}

/// The module `source` (at `path`) is: read back from the runtime's code cache if it keeps
/// one with an entry for it, parsed otherwise, and then kept in the cache.
pub(crate) fn module(source: &str, path: &Path, context: &mut Context) -> JsResult<Module> {
    let cache = crate::shared(context).code_cache.borrow().clone();
    let Some(cache) = cache else {
        return Module::parse(Source::from_bytes(source).with_path(path), None, context);
    };
    let global = boa_engine::code_cache::global_scope_fingerprint(context);
    match cache.load(source, global) {
        Ok(Some(bytes)) => {
            if let Ok(module) = Module::from_code_cache(&bytes, source, Some(path), None, context) {
                cache.count(|stats| stats.hits += 1);
                return Ok(module);
            }
            cache.count(|stats| stats.refused += 1);
        }
        Ok(None) => cache.count(|stats| stats.misses += 1),
        Err(()) => cache.count(|stats| stats.refused += 1),
    }
    let module = Module::parse(Source::from_bytes(source).with_path(path), None, context)?;
    // A module that fails to compile fails when it is linked, as without the cache.
    if let Ok(Some(bytes)) = module.code_cache(context) {
        cache.store(source, global, bytes);
        cache.count(|stats| stats.stored += 1);
    }
    Ok(module)
}

/// Whether the runtime keeps a code cache.
pub(crate) fn is_on(context: &Context) -> bool {
    crate::shared(context).code_cache.borrow().is_some()
}

/// Whether `path` (resolved as the permission model resolves it) is out of JavaScript's reach:
/// the runtime's code cache directory or its secret's file.
pub(crate) fn protects(context: &Context, path: &Path) -> bool {
    crate::shared(context)
        .code_cache
        .borrow()
        .as_ref()
        .is_some_and(|cache| cache.protects(path))
}

/// Writes what the runtime's cache has to write: everything if `settled` (the process is
/// ending), or what has waited long enough for more modules to join it. Called after each
/// round of jobs, so a burst of loading is written once it settles.
pub(crate) fn flush(context: &Context, settled: bool) {
    let cache = crate::shared(context).code_cache.borrow().clone();
    if let Some(cache) = cache {
        cache.write(settled);
    }
}

/// The cache a runtime keeps, shared with the module loader.
pub(crate) type Shared = Option<Rc<CodeCache>>;

/// Writes what the runtime's cache has to write now, whether or not its loading has settled.
pub(crate) fn flush_now(context: &Context) {
    flush(context, true);
}

/// What the runtime's cache has done, if it keeps one.
pub(crate) fn stats(context: &Context) -> Option<CodeCacheStats> {
    crate::shared(context)
        .code_cache
        .borrow()
        .as_ref()
        .map(|cache| cache.stats())
}
