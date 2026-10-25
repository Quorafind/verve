//! The code cache: the compiled code of the ES modules a runtime loads, kept on disk so the next
//! run reads it back instead of parsing and compiling the same source again. Off until an
//! embedder gives it a directory ([`crate::Runtime::set_code_cache_dir`]; `verve run` takes
//! `--code-cache=<dir>` or `VERVE_CODE_CACHE`). See `docs/startup.md`.
//!
//! The entries of one build live in one file, `<dir>/<build>.pack`: opening a file costs more
//! than reading one (on Windows, with a scanner looking at each, about half a millisecond), so a
//! run reads the pack once and finds every module's entry in it. What a run compiles is added
//! to the pack when its loading settles (see [`flush`]), when the process exits and when the
//! runtime is dropped.
//!
//! # Trust
//!
//! What is read back is run as code, so an entry is only read if it is authentic:
//!
//! - its key is a SHA-256 of this build's fingerprint (Boa's format and opcodes, Verve's
//!   version, the executable's path, size and modification time) and of the whole source text,
//!   so an entry is only ever found for the very source and build it was compiled by and for;
//! - it ends with an HMAC-SHA256 of its key and code under a 32-byte secret: the embedder's, or
//!   one made at random in `<dir>/key` the first time. An entry anyone without the secret
//!   wrote, or that was damaged, fails it and is compiled again.
//!
//! An entry that passes is still checked to be well formed by Boa before it runs (see
//! `boa_engine::code_cache`), and anything refused falls back to compiling the source. The pack
//! is written to a temporary file renamed into place, so a reader sees a whole pack or the one
//! before it.
//!
//! The secret is only as safe as the directory: whoever can read `<dir>/key` can write entries
//! that run as the code of any module this runtime loads. The directory must be private to the
//! user the runtime runs as, and out of reach of code it runs that is less trusted than that:
//! Rebon's Code Mode does not use one.

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

const MAGIC: &[u8; 8] = b"VERVEPK\x01";
const TAG_DOMAIN: &[u8] = b"verve code cache entry v1";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 32;
/// The most a pack holds; past it, the entries no run has read lately go first.
const MAX_PACK: usize = 64 << 20;
/// How long new entries wait for more to join them before the pack is written.
const SETTLE: Duration = Duration::from_millis(100);

type Key = [u8; KEY_LEN];
/// The entries of a pack, by key: each one's code and tag.
type Entries = HashMap<Key, (Vec<u8>, [u8; TAG_LEN])>;

/// A directory of compiled modules, and the secret its entries are signed with.
pub(crate) struct CodeCache {
    pack: PathBuf,
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
    read: bool,
    /// Each entry read from disk: its code and tag, checked when it is used.
    entries: Entries,
    /// The keys on disk, in the pack's order: most recently written first.
    order: Vec<Key>,
    /// The entries this run read, which stay ahead of those it did not.
    used: HashSet<Key>,
    /// What this run compiled and has not written yet, with its tag.
    pending: Vec<(Key, Vec<u8>, [u8; TAG_LEN])>,
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
    /// The cache in `dir`, made if it is not there, signed with `secret` or, without one, with
    /// the directory's own (made at random the first time).
    pub(crate) fn open(dir: &Path, secret: Option<[u8; 32]>) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let secret = match secret {
            Some(secret) => secret,
            None => directory_secret(dir)?,
        };
        let fingerprint = build_fingerprint();
        let mut name = hex(&fingerprint[..8]);
        name.push_str(".pack");
        Ok(Self {
            pack: dir.join(name),
            secret,
            fingerprint,
            state: RefCell::default(),
            stats: std::cell::Cell::default(),
        })
    }

    /// The key of the entry for `source`.
    fn key(&self, source: &str) -> Key {
        let mut hash = Sha256::new();
        hash.update(b"verve code cache key v1\0module\0");
        hash.update(self.fingerprint);
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
    pub(crate) fn load(&self, source: &str) -> Result<Option<Vec<u8>>, ()> {
        let key = self.key(source);
        let mut state = self.state.borrow_mut();
        if !state.read {
            state.read = true;
            if let Ok(bytes) = fs::read(&self.pack) {
                let (entries, order) = parse_pack(&bytes);
                state.entries = entries;
                state.order = order;
            }
        }
        let Some((payload, tag)) = state.entries.get(&key) else {
            return Ok(None);
        };
        // Constant time: how much of a forged tag matched says nothing.
        self.mac(&key, payload).verify_slice(tag).map_err(|_| ())?;
        let payload = payload.clone();
        state.used.insert(key);
        Ok(Some(payload))
    }

    /// Keeps `payload`, compiled from `source`, for the next run.
    pub(crate) fn store(&self, source: &str, payload: Vec<u8>) {
        let key = self.key(source);
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
        // What another process wrote since this one read the pack is kept too.
        let (mut on_disk, disk_order) = fs::read(&self.pack)
            .map(|bytes| parse_pack(&bytes))
            .unwrap_or_default();
        for key in &state.order {
            if let Some(entry) = state.entries.get(key) {
                on_disk.entry(*key).or_insert_with(|| entry.clone());
            }
        }
        let mut out = PackWriter::default();
        // Newest first, then what this run read, then the rest as they were.
        for (key, payload, tag) in state.pending.iter().rev() {
            out.put(key, payload, tag);
        }
        for key in disk_order.iter().chain(state.order.iter()) {
            if state.used.contains(key)
                && let Some((payload, tag)) = on_disk.get(key)
            {
                out.put(key, payload, tag);
            }
        }
        for key in disk_order.iter().chain(state.order.iter()) {
            if let Some((payload, tag)) = on_disk.get(key) {
                out.put(key, payload, tag);
            }
        }
        let PackWriter { bytes, order, .. } = out;
        let _ = write_atomically(&self.pack, &bytes);
        let pending = std::mem::take(&mut state.pending);
        state.pending_since = None;
        for (key, payload, tag) in pending {
            state.used.insert(key);
            state.entries.insert(key, (payload, tag));
        }
        state.order = order;
    }
}

/// A pack being written: its bytes, and which keys it holds in what order.
struct PackWriter {
    bytes: Vec<u8>,
    order: Vec<Key>,
    written: HashSet<Key>,
}

impl Default for PackWriter {
    fn default() -> Self {
        Self {
            bytes: MAGIC.to_vec(),
            order: Vec::new(),
            written: HashSet::new(),
        }
    }
}

impl PackWriter {
    /// Adds an entry, unless one with its key is in already or the pack is full.
    fn put(&mut self, key: &Key, payload: &[u8], tag: &[u8; TAG_LEN]) {
        if self.bytes.len() + KEY_LEN + 8 + payload.len() + TAG_LEN > MAX_PACK
            || !self.written.insert(*key)
        {
            return;
        }
        self.bytes.extend_from_slice(key);
        self.bytes
            .extend_from_slice(&(payload.len() as u64).to_le_bytes());
        self.bytes.extend_from_slice(payload);
        self.bytes.extend_from_slice(tag);
        self.order.push(*key);
    }
}

impl Drop for CodeCache {
    fn drop(&mut self) {
        self.write(true);
    }
}

/// The entries of a pack, as far as it is whole, and their keys in its order.
fn parse_pack(bytes: &[u8]) -> (Entries, Vec<Key>) {
    let mut entries = HashMap::new();
    let mut order = Vec::new();
    let Some(mut rest) = bytes.strip_prefix(MAGIC.as_slice()) else {
        return (entries, order);
    };
    while rest.len() >= KEY_LEN + 8 + TAG_LEN {
        let (key, after) = rest.split_at(KEY_LEN);
        let (len, after) = after.split_at(8);
        let Ok(len) = usize::try_from(u64::from_le_bytes(len.try_into().unwrap_or_default()))
        else {
            break;
        };
        if after.len() < len.saturating_add(TAG_LEN) {
            break;
        }
        let (payload, after) = after.split_at(len);
        let (tag, after) = after.split_at(TAG_LEN);
        let key: Key = key.try_into().unwrap_or_default();
        if let std::collections::hash_map::Entry::Vacant(entry) = entries.entry(key) {
            order.push(key);
            entry.insert((payload.to_vec(), tag.try_into().unwrap_or_default()));
        }
        rest = after;
    }
    (entries, order)
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

/// The secret of the cache in `dir`: `<dir>/key`, made at random the first time.
fn directory_secret(dir: &Path) -> io::Result<[u8; 32]> {
    let path = dir.join("key");
    let read = |bytes: Vec<u8>| {
        <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "code cache key is not 32 bytes")
        })
    };
    if let Ok(bytes) = fs::read(&path) {
        return read(bytes);
    }
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| io::Error::other(e.to_string()))?;
    write_atomically(&path, &secret)?;
    // Another process may have made one at the same time; the one on disk is the one to use.
    fs::read(&path).map_or(Ok(secret), read)
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
    match cache.load(source) {
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
        cache.store(source, bytes);
        cache.count(|stats| stats.stored += 1);
    }
    Ok(module)
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
