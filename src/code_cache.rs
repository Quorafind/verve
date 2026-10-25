//! The code cache: the compiled code of the ES modules a runtime loads, kept on disk so the next
//! run reads it back instead of parsing and compiling the same source again. Off until an
//! embedder gives it a directory ([`crate::Runtime::set_code_cache_dir`]; `verve run` takes
//! `--code-cache=<dir>` or `VERVE_CODE_CACHE`). See `docs/startup.md`.
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

const MAGIC: &[u8; 8] = b"VERVEPK\x02";
const TAG_DOMAIN: &[u8] = b"verve code cache entry v1";
const KEY_LEN: usize = 32;
const TAG_LEN: usize = 32;
/// The most a pack holds; past it, the entries no run has read lately go first.
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
        if !state.opened {
            state.opened = true;
            if let Ok(mut file) = fs::File::open(&self.pack)
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
        // What another process wrote since this one opened the pack is kept too, and what this
        // run read from the pack it opened, if the pack on disk no longer has it.
        let disk = fs::read(&self.pack).unwrap_or_default();
        let on_disk = parse_pack(&disk);
        let mut out = PackWriter::default();
        // Newest first, then what this run read, then the rest as they were.
        for (key, payload, tag) in state.pending.iter().rev() {
            out.put(key, payload, tag);
        }
        let used: Vec<Key> = state.used.iter().copied().collect();
        for key in &used {
            if let Some((payload, tag)) = on_disk.entries.get(key) {
                out.put(key, payload, tag);
            } else if let Some(located) = state.index.get(key).copied() {
                let code_start = state.code_start;
                if let Some(file) = state.file.as_mut()
                    && let Some(payload) = read_code(file, code_start, located)
                {
                    out.put(key, &payload, &located.tag);
                }
            }
        }
        for key in &on_disk.order {
            if let Some((payload, tag)) = on_disk.entries.get(key) {
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
    // Written whole beside it, then linked into place only if no other process has made one
    // meanwhile: the first secret made is the one every process uses.
    let mut nonce = [0u8; 8];
    let _ = getrandom::fill(&mut nonce);
    let temp = dir.join(format!("key.{}.{}.tmp", std::process::id(), hex(&nonce)));
    let made = (|| {
        let mut file = private_file(&temp)?;
        file.write_all(&secret)?;
        drop(file);
        match fs::hard_link(&temp, &path) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            // Where links cannot be made, the last secret renamed into place wins.
            Err(_) => fs::rename(&temp, &path),
            Ok(()) => Ok(()),
        }
    })();
    let _ = fs::remove_file(&temp);
    made?;
    read(fs::read(&path)?)
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
