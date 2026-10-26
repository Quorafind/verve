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
    pack: PathBuf,
    /// 已解析的受保护路径；无论当前执行环境是否启用缓存，都禁止 JavaScript 访问。
    protection: Protection,
    secret: [u8; 32],
    fingerprint: [u8; 32],
    state: RefCell<Pack>,
    stats: std::cell::Cell<CodeCacheStats>,
}

/// 跨 Worker 边界只传递禁止访问的路径，不传递密钥、缓存包句柄或缓存状态。
#[derive(Clone, Default)]
pub(crate) struct Protection {
    roots: Vec<PathBuf>,
}

impl Protection {
    pub(crate) fn inherit(&mut self, parent: Self) {
        for root in parent.roots {
            if !self.roots.contains(&root) {
                self.roots.push(root);
            }
        }
    }

    fn protects(&self, path: &Path, subtree: bool) -> bool {
        self.roots
            .iter()
            .any(|root| within(path, root) || (subtree && within(root, path)))
    }
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
        check_private(dir, false)?;
        let fingerprint = build_fingerprint();
        let mut name = hex(&fingerprint[..8]);
        name.push_str(".pack");
        let dir = crate::permission::real(dir, true);
        Ok(Self {
            pack: dir.join(name),
            protection: Protection {
                roots: std::iter::once(dir)
                    .chain(
                        protected
                            .iter()
                            .map(|path| crate::permission::real(path, true)),
                    )
                    .collect(),
            },
            secret,
            fingerprint,
            state: RefCell::default(),
            stats: std::cell::Cell::default(),
        })
    }

    /// 仅继承访问拒绝规则，不会启用缓存或共享密钥。
    pub(crate) fn protection(&self) -> Protection {
        self.protection.clone()
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
#[cfg(not(windows))]
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

#[cfg(windows)]
fn private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        },
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    with_private_security(|attributes| {
        // SAFETY: 路径以 NUL 结尾，安全描述符在创建期间有效；CREATE_NEW 不打开已有对象。
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 创建成功的句柄只交给 File 一次，由 File 负责关闭。
        Ok(unsafe { fs::File::from_raw_handle(handle) })
    })
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
    check_private(&base, false)?;
    let path = base.join("code-cache-secret");
    Ok((private_secret(&path)?, path))
}

/// The secret in the file `path`, made at random the first time: written whole and synced
/// beside it, then linked into place only if no other process has made one meanwhile, so the
/// first secret made is the one every process uses.
fn private_secret(path: &Path) -> io::Result<[u8; 32]> {
    let read = |path: &Path| -> io::Result<[u8; 32]> {
        let mut file = open_no_follow(path)?;
        check_private_file(&file, path, true)?;
        read_secret_bytes(&mut file)
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
            // 即使文件系统不支持硬链接，也不能替换并发创建成功的密钥。
            // 无法安全创建密钥时禁用缓存，而不是更换已有密钥。
            other => other,
        }
    })();
    let _ = fs::remove_file(&temp);
    made?;
    read(path)
}

/// 最多读取 32 字节密钥及 1 字节超长探测数据，即使读取期间文件增长也不会多读。
fn read_secret_bytes(reader: impl io::Read) -> io::Result<[u8; 32]> {
    use std::io::Read as _;
    let mut bytes = Vec::with_capacity(33);
    reader.take(33).read_to_end(&mut bytes)?;
    bytes.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the code cache secret is not 32 bytes",
        )
    })
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
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
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
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    with_private_security(|attributes| {
        // Safety: `wide` is a NUL-terminated wide path and `attributes` points at a valid
        // descriptor for the length of the call.
        // SAFETY: 路径和安全描述符在此调用期间有效，所有者和 DACL 随目录原子创建。
        let made = unsafe { CreateDirectoryW(wide.as_ptr(), attributes) };
        if made == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })
}

#[cfg(windows)]
fn with_private_security<T>(
    create: impl FnOnce(&windows_sys::Win32::Security::SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            SECURITY_ATTRIBUTES,
        },
    };
    with_current_user_sid(|sid| {
        let mut text = std::ptr::null_mut();
        // SAFETY: SID 属于尚未释放的令牌缓冲区；系统分配的字符串在复制后释放。
        if unsafe { ConvertSidToStringSidW(sid, &raw mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0;
        // SAFETY: 转换成功返回以 NUL 结尾的 UTF-16 字符串。
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: 已确定字符串长度，复制后不再引用这块系统分配的内存。
        let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        // SAFETY: 字符串由 ConvertSidToStringSidW 分配，且仅释放一次。
        unsafe { LocalFree(text.cast()) };
        // 默认 TokenOwner 可能是 Administrators，而校验要求 TokenUser；必须在创建时
        // 显式指定用户所有者，不能先暴露对象再改 owner，也不能继承父目录的宽权限。
        // Protected (no inherited entries), full access for the folder's owner, inherited by what
        // is made in it.
        let sddl: Vec<u16> = format!("O:{sid}D:P(A;OICI;FA;;;{sid})\0")
            .encode_utf16()
            .collect();
        let mut descriptor = std::ptr::null_mut();
        // Safety: `sddl` is a NUL-terminated wide string; `descriptor` receives a descriptor
        // allocated by the system, freed below.
        // SAFETY: 描述符含独立的 SID 副本，存活至文件或目录创建调用结束。
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
        // 创建路径先保存系统错误为 io::Error，再释放描述符，避免清理覆盖错误码。
        let result = create(&attributes);
        // Safety: `descriptor` was allocated by the conversion above.
        // SAFETY: 创建调用已结束，描述符及其中的 SID 不再被引用，且仅释放一次。
        unsafe { LocalFree(descriptor) };
        result
    })
}

#[cfg(not(any(unix, windows)))]
fn make_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// 校验已打开的对象，确保密钥权限与读取的字节来自同一文件。
fn check_private(path: &Path, secret: bool) -> io::Result<()> {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?
    };
    #[cfg(not(windows))]
    let file = open_no_follow(path)?;
    check_private_file(&file, path, secret)
}

fn not_private(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("{} is not private to this user", path.display()),
    )
}

fn check_private_file(file: &fs::File, path: &Path, secret: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if (secret && !metadata.is_file()) || (!secret && !metadata.is_dir()) {
        return Err(not_private(path));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Safety: `geteuid` only reads the process's credentials.
        let euid = unsafe { libc::geteuid() };
        let forbidden = if secret { 0o077 } else { 0o022 };
        if metadata.uid() != euid || metadata.mode() & forbidden != 0 {
            return Err(not_private(path));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || !windows_private(file, secret)?
        {
            return Err(not_private(path));
        }
    }
    Ok(())
}

/// 空指针或缺失的 DACL 允许所有人访问。对实际的 DACL，拒绝任何授予非所有者
/// 修改对象（含子项）或读取密钥的权限。
/// 遇到未知 ACE 类型时拒绝访问；不能因存在拒绝项而接受过宽的允许项。
#[cfg(windows)]
fn windows_private(file: &fs::File, secret: bool) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::{GENERIC_ALL, GENERIC_EXECUTE, GENERIC_READ, GENERIC_WRITE, LocalFree},
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER,
            Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, IsValidAcl, IsValidSid, IsWellKnownSid,
            OWNER_SECURITY_INFORMATION, WinCreatorOwnerRightsSid,
        },
        Storage::FileSystem::{
            DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_EXECUTE, FILE_READ_DATA,
            FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, WRITE_DAC, WRITE_OWNER,
        },
        System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE},
    };
    let mut owner = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: 文件句柄保持打开；返回的指针指向下方才释放的 `descriptor` 内部。
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            &raw mut dacl,
            std::ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    let result = (|| {
        // SAFETY: 指针由安全 API 返回，且安全描述符尚未释放。
        if owner.is_null()
            || dacl.is_null()
            || unsafe { IsValidSid(owner) == 0 || IsValidAcl(dacl) == 0 }
            || !current_user_owns(owner)?
        {
            return Ok(false);
        }
        let mut forbidden = GENERIC_ALL
            | GENERIC_WRITE
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
            | FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_WRITE_ATTRIBUTES
            | FILE_DELETE_CHILD;
        if secret {
            forbidden |= GENERIC_READ | GENERIC_EXECUTE | FILE_READ_DATA | FILE_EXECUTE;
        }
        // SAFETY: `dacl` 已在上方校验，且其所属的 `descriptor` 尚未释放。
        for index in 0..unsafe { (*dacl).AceCount } {
            let mut ace = std::ptr::null_mut();
            // SAFETY: 索引位于 ACL 范围内；`ace` 指向尚未释放的安全描述符内部。
            if unsafe { GetAce(dacl, u32::from(index), &raw mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: GetAce 返回的是已校验 ACL 中的 ACE。
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            match u32::from(header.AceType) {
                ACCESS_DENIED_ACE_TYPE => continue,
                ACCESS_ALLOWED_ACE_TYPE => {}
                _ => return Ok(false),
            }
            // SAFETY: 普通允许项 ACE 使用此布局，其 SID 从 SidStart 开始。
            let allow = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = (&raw const allow.SidStart).cast_mut().cast();
            // SAFETY: SID 属于此已校验的 ACE；所有者 SID 在整个调用期间有效。
            let is_owner = unsafe {
                IsValidSid(sid) != 0
                    && (EqualSid(sid, owner) != 0
                        || IsWellKnownSid(sid, WinCreatorOwnerRightsSid) != 0)
            };
            if allow.Mask & forbidden != 0 && !is_owner {
                return Ok(false);
            }
        }
        Ok(true)
    })();
    // SAFETY: `descriptor` 由 GetSecurityInfo 分配，且指向其内部的引用不会逃逸此调用。
    unsafe { LocalFree(descriptor) };
    result
}

#[cfg(windows)]
fn current_user_owns(owner: windows_sys::Win32::Security::PSID) -> io::Result<bool> {
    use windows_sys::Win32::Security::EqualSid;
    with_current_user_sid(|sid| {
        // SAFETY: 两个 SID 均来自尚未释放的安全描述符或令牌缓冲区。
        Ok(unsafe { EqualSid(owner, sid) } != 0)
    })
}

/// 创建和校验共用 TokenUser 查询，避免误用可能指向 Administrators 的 TokenOwner。
#[cfg(windows)]
fn with_current_user_sid<T>(
    use_sid: impl FnOnce(windows_sys::Win32::Security::PSID) -> io::Result<T>,
) -> io::Result<T> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token = std::ptr::null_mut();
    // SAFETY: 当前进程句柄有效，`token` 接收的句柄会在下方关闭。
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let result = (|| {
        let mut len = 0;
        // SAFETY: 传入空缓冲区仅查询所需大小。
        unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &raw mut len) };
        if len == 0 {
            return Err(io::Error::last_os_error());
        }
        // 为 TOKEN_USER 及其后的 SID 提供按指针对齐的存储。
        let mut buffer = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
        // SAFETY: 缓冲区至少有 `len` 字节，且在使用 SID 期间始终有效。
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                len,
                &raw mut len,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: 令牌查询在此缓冲区中返回 TOKEN_USER 及紧随其后的 SID。
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        // SID 仅在回调期间使用，不得逃逸令牌缓冲区的生命周期。
        use_sid(user.User.Sid)
    })();
    // SAFETY: 此句柄由 OpenProcessToken 返回，且仅关闭一次。
    unsafe { CloseHandle(token) };
    result
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

/// 判断路径是否受保护；移动或删除目录树时，也保护其祖先路径。
/// 常规未启用缓存且无继承保护规则的场景无需解析路径。
pub(crate) fn protects(context: &Context, path: &Path, follow: bool, subtree: bool) -> bool {
    let state = crate::shared(context);
    let protection = state.code_cache_protection.borrow();
    !protection.roots.is_empty()
        && protection.protects(&crate::permission::real(path, follow), subtree)
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

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "verve-code-cache-boundary-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            create_private_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn ancestor_probe(name: &str, secret: bool, operation: &str, renamed: bool) -> String {
        let fixture = Fixture::new(name);
        let ancestor = fixture.0.join("ancestor");
        let moved = fixture.0.join("moved");
        let (dir, file, protected) = if secret {
            let base = ancestor.join("nested");
            create_private_dir(&base).unwrap();
            let file = base.join("code-cache-secret");
            private_secret(&file).unwrap();
            (fixture.0.join("cache"), file.clone(), vec![file])
        } else {
            let dir = ancestor.join("nested/cache");
            (dir.clone(), dir.join("entry"), Vec::new())
        };
        let mut runtime = crate::Runtime::new();
        runtime.set_code_cache(&dir, [0x5e; 32], protected).unwrap();
        if !secret {
            fs::write(&file, [0x5e; 32]).unwrap();
        }
        let moved_file = moved.join(file.strip_prefix(&ancestor).unwrap());
        let path = |path: &Path| serde_json::to_string(&path.to_string_lossy()).unwrap();
        let source = format!(
            r#"
            const fs = require('fs');
            const ancestor = {ancestor};
            const moved = {moved};
            globalThis.result = 'pending';
            (async () => {{
                try {{
                    {operation};
                    globalThis.result = {allowed};
                }} catch (e) {{
                    globalThis.result = e.code + ':' + (e.permission || '');
                }}
            }})().catch(e => {{ globalThis.result = 'probe-error:' + e; }});
            "#,
            ancestor = path(&ancestor),
            moved = path(&moved),
            allowed = if renamed {
                format!(
                    "'allowed:read-' + fs.readFileSync({}).length",
                    path(&moved_file)
                )
            } else {
                "'allowed'".to_owned()
            },
        );
        runtime
            .load_module(&fixture.0.join("probe.cjs"), &source)
            .unwrap();
        for _ in 0..100 {
            runtime.run_microtasks();
            runtime.run_due();
            if probe_result(&mut runtime) != "pending" {
                break;
            }
        }
        let result = probe_result(&mut runtime);
        if result == "ERR_ACCESS_DENIED:FileSystemWrite" {
            assert!(
                file.exists(),
                "a refusal must leave the protected file intact"
            );
            assert!(!moved.exists(), "a refusal must not rename the ancestor");
        }
        result
    }

    fn probe_result(runtime: &mut crate::Runtime) -> String {
        crate::print(&runtime.eval("globalThis.result").unwrap())
    }

    fn assert_ancestor_renames_denied(secret: bool) {
        let mut results = Vec::new();
        for (family, operation) in [
            ("sync", "fs.renameSync(ancestor, moved)"),
            ("promise", "await fs.promises.rename(ancestor, moved)"),
            (
                "callback",
                "await new Promise((resolve, reject) => fs.rename(ancestor, moved, e => e ? reject(e) : resolve()))",
            ),
        ] {
            let result = ancestor_probe(
                &format!("rename-{secret}-{family}"),
                secret,
                operation,
                true,
            );
            results.push((family, result));
        }
        assert!(
            results
                .iter()
                .all(|(_, result)| result == "ERR_ACCESS_DENIED:FileSystemWrite"),
            "{results:?}"
        );
    }

    #[test]
    fn javascript_cannot_rename_cache_ancestors() {
        assert_ancestor_renames_denied(false);
    }

    #[test]
    fn javascript_cannot_rename_secret_ancestors() {
        assert_ancestor_renames_denied(true);
    }

    fn assert_ancestor_removals_denied(secret: bool) {
        let mut results = Vec::new();
        for (family, operation) in [
            (
                "rm-sync",
                "fs.rmSync(ancestor, { recursive: true, force: true })",
            ),
            ("rmdir-sync", "fs.rmdirSync(ancestor, { recursive: true })"),
            (
                "rm-promise",
                "await fs.promises.rm(ancestor, { recursive: true, force: true })",
            ),
            (
                "rmdir-promise",
                "await fs.promises.rmdir(ancestor, { recursive: true })",
            ),
            (
                "rm-callback",
                "await new Promise((resolve, reject) => fs.rm(ancestor, { recursive: true, force: true }, e => e ? reject(e) : resolve()))",
            ),
            (
                "rmdir-callback",
                "await new Promise((resolve, reject) => fs.rmdir(ancestor, { recursive: true }, e => e ? reject(e) : resolve()))",
            ),
        ] {
            let result = ancestor_probe(
                &format!("remove-{secret}-{family}"),
                secret,
                operation,
                false,
            );
            results.push((family, result));
        }
        assert!(
            results
                .iter()
                .all(|(_, result)| result == "ERR_ACCESS_DENIED:FileSystemWrite"),
            "{results:?}"
        );
    }

    #[test]
    fn javascript_cannot_recursively_remove_cache_ancestors() {
        assert_ancestor_removals_denied(false);
    }

    #[test]
    fn javascript_cannot_recursively_remove_secret_ancestors() {
        assert_ancestor_removals_denied(true);
    }

    #[test]
    fn workers_and_descendants_inherit_cache_denials() {
        let fixture = Fixture::new("worker-protection");
        let dir = fixture.0.join("cache");
        let secret_file = fixture.0.join("code-cache-secret");
        let secret = private_secret(&secret_file).unwrap();
        let cache_file = dir.join("entry");
        let ordinary_file = fixture.0.join("ordinary");
        let mut runtime = crate::Runtime::new();
        runtime
            .set_code_cache(&dir, secret, vec![secret_file.clone()])
            .unwrap();
        fs::write(&cache_file, b"cache-entry").unwrap();
        fs::write(&ordinary_file, b"ordinary-ok").unwrap();
        // 此处刻意使用 FsAccess::All：访问拒绝规则必须独立于普通文件系统授权继承，
        // 且不能因此在任一 Worker 中启用代码缓存。
        assert_eq!(runtime.permissions().fs_read, crate::FsAccess::All);
        let paths = serde_json::json!({
            "cache": cache_file.to_string_lossy(),
            "secret": secret_file.to_string_lossy(),
            "ordinary": ordinary_file.to_string_lossy(),
        });
        let probe = format!(
            r#"
            function readProbe() {{
                const fs = process.getBuiltinModule('fs');
                const report = {{}};
                for (const [name, path] of Object.entries({paths})) {{
                    try {{ report[name] = 'allowed:read-' + fs.readFileSync(path, 'hex'); }}
                    catch (e) {{ report[name] = e.code + ':' + (e.permission || ''); }}
                    try {{
                        fs.appendFileSync(path, name === 'ordinary' ? '!' : 'forbidden');
                        report[name + 'Write'] = 'allowed';
                    }} catch (e) {{ report[name + 'Write'] = e.code + ':' + (e.permission || ''); }}
                }}
                return report;
            }}
            "#,
        );
        let expected = serde_json::json!({
            "cache": "ERR_ACCESS_DENIED:FileSystemRead",
            "secret": "ERR_ACCESS_DENIED:FileSystemRead",
            "ordinary": format!("allowed:read-{}", hex(b"ordinary-ok")),
            "cacheWrite": "ERR_ACCESS_DENIED:FileSystemWrite",
            "secretWrite": "ERR_ACCESS_DENIED:FileSystemWrite",
            "ordinaryWrite": "allowed",
        });
        // 先确认这个父运行时本身已拒绝读取受保护路径。
        let parent = runtime
            .eval(&format!("{probe}\nJSON.stringify(readProbe())"))
            .unwrap();
        let parent: serde_json::Value = serde_json::from_str(&crate::print(&parent)).unwrap();
        assert_eq!(parent, expected);
        fs::write(&ordinary_file, b"ordinary-ok").unwrap();

        let child =
            format!("{probe}\npostMessage({{ realm: 'child', report: readProbe() }}); close();");
        let worker = format!(
            r#"
            {probe}
            const report = readProbe();
            postMessage({{ realm: 'worker', report }});
            process.getBuiltinModule('fs').writeFileSync({ordinary}, 'ordinary-ok');
            const child = new Worker('data:text/javascript,' + encodeURIComponent({child}));
            child.onmessage = e => {{ postMessage(e.data); child.terminate(); close(); }};
            "#,
            child = serde_json::to_string(&child).unwrap(),
            ordinary = serde_json::to_string(&ordinary_file.to_string_lossy()).unwrap(),
        );
        runtime
            .eval(&format!(
                r#"
                globalThis.result = {{}};
                globalThis.worker = new Worker('data:text/javascript,' + encodeURIComponent({worker}));
                worker.onmessage = e => {{
                    result[e.data.realm] = e.data.report;
                    if (Object.keys(result).length === 2) worker.terminate();
                }};
                "#,
                worker = serde_json::to_string(&worker).unwrap(),
            ))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            runtime.wait_for_workers(Duration::from_millis(20));
            runtime.run_workers();
            if runtime
                .eval("Object.keys(result).length === 2")
                .unwrap()
                .to_boolean()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        runtime.eval("worker.terminate()").unwrap();
        let reports = runtime.eval("JSON.stringify(result)").unwrap();
        let reports: serde_json::Value = serde_json::from_str(&crate::print(&reports)).unwrap();
        let console: Vec<_> = runtime
            .take_console()
            .into_iter()
            .map(|line| line.text)
            .collect();
        assert_eq!(
            reports,
            serde_json::json!({ "worker": expected.clone(), "child": expected }),
            "both workers must answer through postMessage; console: {console:?}"
        );
        assert_eq!(fs::read(&secret_file).unwrap(), secret);
        assert_eq!(fs::read(&cache_file).unwrap(), b"cache-entry");
        assert_eq!(fs::read(&ordinary_file).unwrap(), b"ordinary-ok!");
    }

    #[test]
    fn ordinary_siblings_can_be_written_renamed_and_removed() {
        let fixture = Fixture::new("ordinary-siblings");
        let secret_file = fixture.0.join("secret");
        let secret = private_secret(&secret_file).unwrap();
        let mut runtime = crate::Runtime::new();
        runtime
            .set_code_cache(&fixture.0.join("cache"), secret, vec![secret_file.clone()])
            .unwrap();
        let source = format!(
            r#"
            const fs = require('fs');
            const root = {root};
            globalThis.result = 'pending';
            (async () => {{
                for (const family of ['sync', 'promise', 'callback']) {{
                    const invoke = (name, ...args) => {{
                        if (family === 'sync') return fs[name + 'Sync'](...args);
                        if (family === 'promise') return fs.promises[name](...args);
                        return new Promise((resolve, reject) =>
                            fs[name](...args, e => e ? reject(e) : resolve()));
                    }};
                    const from = root + '/ordinary-' + family;
                    const to = from + '-moved';
                    await invoke('mkdir', from);
                    await invoke('writeFile', from + '/entry', 'ordinary-ok');
                    if (fs.readFileSync(from + '/entry', 'utf8') !== 'ordinary-ok')
                        throw new Error('ordinary read failed');
                    await invoke('rename', from, to);
                    await invoke('rm', to, {{ recursive: true, force: true }});
                    await invoke('mkdir', from);
                    await invoke('writeFile', from + '/entry', 'ordinary-ok');
                    await invoke('rmdir', from, {{ recursive: true }});
                }}
                globalThis.result = 'allowed';
            }})().catch(e => {{ globalThis.result = e.code + ':' + e; }});
            "#,
            root = serde_json::to_string(&fixture.0.to_string_lossy()).unwrap(),
        );
        runtime
            .load_module(&fixture.0.join("ordinary.cjs"), &source)
            .unwrap();
        for _ in 0..100 {
            runtime.run_microtasks();
            runtime.run_due();
            if probe_result(&mut runtime) != "pending" {
                break;
            }
        }
        assert_eq!(probe_result(&mut runtime), "allowed");
        assert_eq!(fs::read(&secret_file).unwrap(), secret);
        assert!(fixture.0.join("cache").is_dir());
        for family in ["sync", "promise", "callback"] {
            assert!(!fixture.0.join(format!("ordinary-{family}")).exists());
            assert!(!fixture.0.join(format!("ordinary-{family}-moved")).exists());
        }
    }

    #[test]
    fn concurrent_first_secret_creation_agrees() {
        let fixture = Fixture::new("secret-concurrent");
        let path = fixture.0.join("secret");
        let start = std::sync::Barrier::new(2);
        let secrets = std::thread::scope(|scope| {
            let create = || {
                start.wait();
                private_secret(&path).unwrap()
            };
            let a = scope.spawn(create);
            let b = scope.spawn(create);
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(secrets[0], secrets[1]);
        assert_eq!(private_secret(&path).unwrap(), secrets[0]);
        assert_eq!(fs::read(&path).unwrap(), secrets[0]);
    }

    #[test]
    fn secret_length_probe_reads_at_most_33_bytes() {
        let mut reader = io::Cursor::new(vec![0x5e; 4096]);
        assert_eq!(
            read_secret_bytes(&mut reader).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
        );
        assert_eq!(reader.position(), 33);
    }

    #[test]
    fn a_private_secret_is_reused_and_must_have_exactly_32_bytes() {
        let fixture = Fixture::new("secret-length");
        let path = fixture.0.join("secret");
        let secret = private_secret(&path).unwrap();
        assert_eq!(private_secret(&path).unwrap(), secret);
        for len in [0, 1, 31, 33, 4096] {
            fs::write(&path, vec![0x5e; len]).unwrap();
            assert_eq!(
                private_secret(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "length {len}"
            );
        }
        fs::write(&path, [0x5e; 32]).unwrap();
        assert_eq!(private_secret(&path).unwrap(), [0x5e; 32]);
    }

    #[cfg(unix)]
    #[test]
    fn a_secret_readable_by_group_or_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new("secret-mode");
        let path = fixture.0.join("secret");
        private_secret(&path).unwrap();
        let mut results = Vec::new();
        for mode in [0o640, 0o604, 0o644] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            results.push((mode, private_secret(&path).err().map(|error| error.kind())));
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(private_secret(&path).is_ok());
        assert!(
            results
                .iter()
                .all(|(_, error)| *error == Some(io::ErrorKind::PermissionDenied)),
            "{results:?}"
        );
    }

    #[cfg(windows)]
    fn replace_dacl(path: &Path, sddl: &str) {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
                DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
            },
        };
        let sddl: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: 两个字符串均以 NUL 结尾，系统分配的描述符在调用后释放。
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(converted, 0, "{}", io::Error::last_os_error());
        // SAFETY: 路径和描述符在本次调用期间有效。
        let changed = unsafe {
            SetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor,
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: 描述符由系统转换函数分配，且仅释放一次。
        unsafe { LocalFree(descriptor) };
        assert_ne!(changed, 0, "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn a_new_private_file_has_the_user_owner_and_no_inherited_access() {
        let fixture = Fixture::new("new-file-owner");
        // 刻意给父目录宽权限，确保文件在创建时自带用户 owner 和受保护 DACL，
        // 而不是依赖父目录恰好私密或创建后再收紧权限。
        replace_dacl(&fixture.0, "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;WD)");
        let path = fixture.0.join("secret");
        let mut file = private_file(&path).unwrap();
        file.write_all(b"private").unwrap();
        check_private_file(&file, &path, true).unwrap();
        drop(file);
        check_private(&path, true).unwrap();
        assert_eq!(
            private_file(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists,
        );
        assert_eq!(fs::read(&path).unwrap(), b"private");
    }

    #[cfg(windows)]
    #[test]
    fn a_new_private_directory_has_the_user_owner_and_no_inherited_access() {
        let fixture = Fixture::new("new-dir-owner");
        replace_dacl(&fixture.0, "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;WD)");
        let path = fixture.0.join("cache");
        make_private_dir(&path).unwrap();
        check_private(&path, false).unwrap();
        assert_eq!(
            make_private_dir(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists,
        );
    }

    #[cfg(windows)]
    fn inherited_dacl_is_user_only(path: &Path) -> io::Result<bool> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::LocalFree,
            Security::{
                ACCESS_ALLOWED_ACE, ACE_HEADER,
                Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
                DACL_SECURITY_INFORMATION, GetAce, INHERITED_ACE, IsValidAcl,
            },
            Storage::FileSystem::FILE_ALL_ACCESS,
            System::SystemServices::ACCESS_ALLOWED_ACE_TYPE,
        };
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut dacl = std::ptr::null_mut();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: 路径以 NUL 结尾；DACL 指向下方才释放的系统描述符内部。
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut dacl,
                std::ptr::null_mut(),
                &raw mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        let result = (|| {
            // SAFETY: DACL 来自系统安全 API，描述符在整个检查期间有效。
            if dacl.is_null() || unsafe { IsValidAcl(dacl) == 0 || (*dacl).AceCount != 1 } {
                return Ok(false);
            }
            let mut ace = std::ptr::null_mut();
            // SAFETY: 已确认 DACL 有且只有一个 ACE，系统返回的指针不逃逸此调用。
            if unsafe { GetAce(dacl, 0, &raw mut ace) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: GetAce 返回已校验 ACL 内的 ACE，先确认类型再读取普通允许项。
            let header = unsafe { &*ace.cast::<ACE_HEADER>() };
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
                || u32::from(header.AceFlags) & INHERITED_ACE == 0
            {
                return Ok(false);
            }
            // SAFETY: ACE 类型已确认，其 SID 与描述符的生命周期一致。
            let allow = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            let sid = (&raw const allow.SidStart).cast_mut().cast();
            Ok(allow.Mask == FILE_ALL_ACCESS && current_user_owns(sid)?)
        })();
        // SAFETY: 描述符由 GetNamedSecurityInfoW 分配，结果不保留内部指针。
        unsafe { LocalFree(descriptor) };
        result
    }

    #[cfg(windows)]
    #[test]
    fn ordinary_children_inherit_access_only_for_the_current_user() {
        let fixture = Fixture::new("inherited-user-access");
        let dir = fixture.0.join("ordinary-dir");
        let file = fixture.0.join("ordinary-file");
        let nested = dir.join("nested-file");
        fs::create_dir(&dir).unwrap();
        fs::write(&file, b"ordinary").unwrap();
        fs::write(&nested, b"nested").unwrap();
        // 普通创建仍可能使用默认 TokenOwner；此处检查继承的授权 SID 必须是用户，
        // 而不是动态 owner、Administrators 或 Everyone，且不能丢失对子目录的继承。
        for path in [&dir, &file, &nested] {
            assert!(inherited_dacl_is_user_only(path).unwrap(), "{path:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn an_existing_cache_directory_writable_by_everyone_is_refused() {
        let fixture = Fixture::new("cache-dacl");
        let dir = fixture.0.join("cache");
        create_private_dir(&dir).unwrap();
        assert!(CodeCache::open(&dir, [0x5e; 32], Vec::new()).is_ok());
        replace_dacl(&dir, "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;WD)");
        assert_eq!(
            CodeCache::open(&dir, [0x5e; 32], Vec::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied,
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_secret_readable_by_everyone_is_refused() {
        let fixture = Fixture::new("secret-dacl");
        let path = fixture.0.join("secret");
        let secret = private_secret(&path).unwrap();
        assert_eq!(private_secret(&path).unwrap(), secret);
        replace_dacl(&path, "D:P(A;;FA;;;OW)(A;;FR;;;WD)");
        assert_eq!(
            private_secret(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_secret_with_a_null_dacl_is_refused() {
        let fixture = Fixture::new("secret-null-dacl");
        let path = fixture.0.join("secret");
        private_secret(&path).unwrap();
        replace_dacl(&path, "D:NO_ACCESS_CONTROL");
        assert_eq!(
            private_secret(&path).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
        );
    }
}
