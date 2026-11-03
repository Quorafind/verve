//! The mark a first run leaves for the second: the files it loaded, without their code, so that
//! the run which loads them again is the one that compiles every function once and keeps it
//! (`docs/electron/packages/P67-code-cache-default.md`).
//!
//! The mark is not signed and carries no code: a forged one can only make Verve compile the
//! modules it names, which the trust model already allows whoever can write the cache directory
//! (`docs/startup.md`). A run that writes it is the one run that checks nothing and reads no
//! secret; a pack is only ever opened or written after `CodeCache::open` has made and checked
//! the directory.

use std::{
    collections::{HashMap, VecDeque},
    fs, io,
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use boa_engine::Context;

use crate::code_cache::{self, CodeCache};
use crate::encoding::hex;

/// The source a run must load before the default folder is touched at all: below it the run makes
/// no directory, writes no mark and reads no secret.
///
/// Measured in the first phase of this package (release, `--features jit`, median of 5, whole
/// process, Windows 10 / Ryzen 9 7950X): compiling everything and writing a pack on the first run
/// took 35.8 ms against 27.9 ms uncached for a one-file `console.log('hi')` (+28%; of the extra
/// 10.0 ms compiling is 0.02 and the rest is fixed cost: 3.07 making the directory, 1.57 reading
/// and writing the secret, 4.43 the atomic write), 122.5 against 103.7 for Rebon's combination
/// (+18%), and 1,538.7 against 1,157.2 for `require('typescript')` (+33%). A first run may cost at
/// most 105% of an uncached one, so it does not compile. A script under the threshold has no parsing
/// to save, so a cache is only fixed cost (29.2 warm against 27.9 uncached) and its pack is 402 B.
/// Three workloads above 64 KB write packs of 402 B, 994 KB and 17.8 MB.
const WORTH_CACHING: u64 = 64 << 10;

/// The most files a mark names; past it the earliest recorded go, so the entry and what it imports
/// first stay.
const MAX_ENTRIES: usize = 4096;
/// The most bytes a mark may have: 4,096 paths of a thousand characters fit. A larger mark is not
/// read.
const MAX_BYTES: u64 = 256 << 10;
/// How a mark file starts: its format, and the version of it.
const MAGIC: &[u8] = b"VERVESEN\x01";

/// A source file as it was on disk, by length and modification time, to tell it is unchanged.
#[derive(Clone, Copy)]
struct Entry {
    len: u64,
    modified: u128,
}

/// The files a run loaded, as it leaves them for the next.
#[derive(Default)]
pub(crate) struct Mark {
    entries: HashMap<PathBuf, Entry>,
    /// The order they were recorded in; the earliest go first.
    order: VecDeque<PathBuf>,
}

impl Mark {
    /// The size and modification time each of these files has on disk now.
    fn of(paths: &[PathBuf]) -> Self {
        let mut mark = Self::default();
        for path in paths {
            let Ok(metadata) = fs::metadata(path) else {
                continue;
            };
            mark.insert(
                path.clone(),
                Entry {
                    len: metadata.len(),
                    modified: modified(&metadata),
                },
            );
        }
        mark
    }

    fn insert(&mut self, path: PathBuf, entry: Entry) {
        if self.entries.insert(path.clone(), entry).is_none() {
            self.order.push_back(path);
        }
        while self.order.len() > MAX_ENTRIES {
            let oldest = self.order.pop_front().expect("longer than the limit");
            self.entries.remove(&oldest);
        }
    }

    fn get(&self, path: &Path) -> Option<Entry> {
        self.entries.get(path).copied()
    }

    /// The mark at `path`; one that cannot be read back (missing, damaged, too large) is none, and
    /// the next run writes it again.
    fn read(path: &Path) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        let mut rest = bytes.strip_prefix(MAGIC)?;
        let (count, tail) = rest.split_first_chunk::<4>()?;
        rest = tail;
        let mut mark = Self::default();
        for _ in 0..u32::from_le_bytes(*count) {
            let (len, tail) = rest.split_first_chunk::<8>()?;
            let (changed, tail) = tail.split_first_chunk::<16>()?;
            let (path_len, tail) = tail.split_first_chunk::<4>()?;
            let (path, tail) = tail.split_at_checked(u32::from_le_bytes(*path_len) as usize)?;
            mark.insert(
                PathBuf::from(String::from_utf8_lossy(path).into_owned()),
                Entry {
                    len: u64::from_le_bytes(*len),
                    modified: u128::from_le_bytes(*changed),
                },
            );
            rest = tail;
        }
        Some(mark)
    }

    /// Writes the mark for the next run as one file, in one write.
    fn write(&self, path: &Path) -> io::Result<()> {
        fs::write(path, self.encode())
    }

    /// The mark's bytes; the count in the header is the count written, as entries past
    /// [`MAX_BYTES`] are left out.
    fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let mut count = 0u32;
        for path in &self.order {
            let Some(entry) = self.entries.get(path) else {
                continue;
            };
            let written = path.to_string_lossy();
            let written = written.as_bytes();
            if MAGIC.len() + 4 + body.len() + 28 + written.len() > MAX_BYTES as usize {
                break;
            }
            body.extend_from_slice(&entry.len.to_le_bytes());
            body.extend_from_slice(&entry.modified.to_le_bytes());
            body.extend_from_slice(&(written.len() as u32).to_le_bytes());
            body.extend_from_slice(written);
            count += 1;
        }
        let mut out = Vec::with_capacity(MAGIC.len() + 4 + body.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }
}

fn modified(metadata: &fs::Metadata) -> u128 {
    metadata
        .modified()
        .ok()
        .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos())
        .unwrap_or_default()
}

/// This build's mark file, beside its pack in the cache directory.
fn mark_path(dir: &Path, fingerprint: &[u8; 32]) -> PathBuf {
    let mut name = hex(&fingerprint[..8]);
    name.push_str(".seen");
    dir.join(name)
}

/// `verve run`'s default cache directory, when neither the flag nor the variable named one.
///
/// This build's pack being there is the whole of what a run looks at before its first module:
/// it opens the cache then, as a run with `--code-cache` does (the directory is made and checked,
/// the secret read, the pack's index read). Without a pack nothing is made and no secret is read;
/// this run's first module load reads the mark, and whether it compiles everything and keeps it is
/// decided there and before the process ends. Nothing here may fail the run or write to the
/// console: there are machines and accounts with no private folder to keep a cache in.
pub(crate) fn start(context: &Context, dir: &Path) {
    let dir = crate::permission::real(dir, true);
    let fingerprint = code_cache::build_fingerprint();
    let shared = crate::shared(context);
    if fs::metadata(code_cache::pack_in(&dir, &fingerprint)).is_ok() {
        activate(&shared, &dir, fingerprint);
        return;
    }
    *shared.code_cache_deferred.borrow_mut() = Some(Deferred::new(dir, fingerprint));
}

/// The default directory's cache, before this run has shown it is worth keeping
/// ([`WORTH_CACHING`]): the files it loaded, and whether the mark it read named them.
pub(crate) struct Deferred {
    dir: PathBuf,
    fingerprint: [u8; 32],
    /// The source this run has loaded in total, and the files it came from.
    loaded: u64,
    paths: Vec<PathBuf>,
    /// The mark read at the first load; `None` until `read` is set.
    mark: Option<Mark>,
    read: bool,
    /// This run has enabled the cache: it writes the pack and no mark.
    activated: bool,
}

impl Deferred {
    fn new(dir: PathBuf, fingerprint: [u8; 32]) -> Self {
        Self {
            dir,
            fingerprint,
            loaded: 0,
            paths: Vec::new(),
            mark: None,
            read: false,
            activated: false,
        }
    }
}

/// Asks, as `path` (`len` bytes of source) loads, whether this run uses the default folder's cache.
///
/// A mark that names the file, unchanged, means the same program is running again: only then is the
/// directory made and its access checked, the secret read and the pack opened, and the run goes on
/// as if the cache had been on from the start, compiling every function and keeping it. Otherwise
/// the source loaded is only counted, and whether to leave a mark is decided before exit.
pub(crate) fn deferred(context: &Context, path: &Path, len: u64) -> Option<Rc<CodeCache>> {
    let shared = crate::shared(context);
    let mut slot = shared.code_cache_deferred.borrow_mut();
    let deferred = slot.as_mut()?;
    if !deferred.read {
        deferred.read = true;
        deferred.mark = Mark::read(&mark_path(&deferred.dir, &deferred.fingerprint));
    }
    if deferred.activated {
        return None;
    }
    let seen = deferred.mark.as_ref().and_then(|mark| mark.get(path));
    if seen.is_some_and(|entry| unchanged(path, entry)) {
        deferred.activated = true;
        let (dir, fingerprint) = (deferred.dir.clone(), deferred.fingerprint);
        drop(slot);
        let cache = activate(&shared, &dir, fingerprint);
        if let Some(cache) = &cache {
            cache.count(|stats| stats.mark_hits += 1);
        }
        return cache;
    }
    deferred.loaded += len;
    deferred.paths.push(path.to_path_buf());
    None
}

/// Whether the file on disk is the one the mark names: its length and modification time match. A
/// wrong match only makes Verve compile once more, since an entry still counts only by the key
/// computed from the whole source and its signature.
fn unchanged(path: &Path, entry: Entry) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.len() == entry.len && modified(&metadata) == entry.modified)
}

/// Turns the cache on: makes the directory or checks others cannot write to it, reads the secret
/// and opens this build's pack.
fn activate(shared: &crate::Shared, dir: &Path, fingerprint: [u8; 32]) -> Option<Rc<CodeCache>> {
    let (secret, file) = code_cache::user_secret().ok()?;
    let cache = CodeCache::open_with(dir, secret, vec![file], fingerprint).ok()?;
    *shared.code_cache_protection.borrow_mut() = cache.protection();
    let cache = Rc::new(cache);
    *shared.code_cache.borrow_mut() = Some(Rc::clone(&cache));
    Some(cache)
}

/// Before the process exits, after the flush that writes the pack: a run that loaded enough leaves
/// a mark so the next run compiles and writes the pack. The directory and the mark are written
/// here, off the startup path, and this is the one place the directory's DACL goes unchecked, as
/// all it writes is a mark with no code and no signature.
pub(crate) fn finish(context: &Context) {
    let shared = crate::shared(context);
    let Some(deferred) = shared.code_cache_deferred.borrow_mut().take() else {
        return;
    };
    if deferred.activated || deferred.loaded < WORTH_CACHING || deferred.paths.is_empty() {
        return;
    }
    if code_cache::create_private_dir(&deferred.dir).is_err() {
        return;
    }
    let _ = Mark::of(&deferred.paths).write(&mark_path(&deferred.dir, &deferred.fingerprint));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u64) -> Entry {
        Entry {
            len: n,
            modified: n as u128,
        }
    }

    /// The entry count is capped: past it the earliest recorded go and the latest stay.
    #[test]
    fn a_mark_keeps_the_newest_entries_only() {
        let mut mark = Mark::default();
        for n in 0..MAX_ENTRIES as u64 + 3 {
            mark.insert(PathBuf::from(format!("/m/{n}.cjs")), entry(n));
        }
        assert_eq!(mark.order.len(), MAX_ENTRIES);
        assert!(
            mark.get(Path::new("/m/0.cjs")).is_none(),
            "the earliest went"
        );
        assert!(
            mark.get(Path::new("/m/2.cjs")).is_none(),
            "the earliest went"
        );
        assert!(mark.get(Path::new("/m/3.cjs")).is_some());
        assert!(
            mark.get(&PathBuf::from(format!("/m/{}.cjs", MAX_ENTRIES + 2)))
                .is_some()
        );
    }

    /// The byte size is capped: long paths stop being written once it is reached, and what was
    /// written reads back as exactly those entries.
    #[test]
    fn a_mark_is_written_within_its_byte_limit_and_reads_back_whole() {
        let mut mark = Mark::default();
        let long = "x".repeat(1_000);
        for n in 0..MAX_ENTRIES as u64 {
            mark.insert(PathBuf::from(format!("/m/{n}/{long}.cjs")), entry(n));
        }
        let bytes = mark.encode();
        assert!(
            bytes.len() as u64 <= MAX_BYTES,
            "a mark of {} bytes",
            bytes.len()
        );
        let parsed = Mark::parse(&bytes).expect("a mark it wrote reads back");
        assert!(
            parsed.order.len() < MAX_ENTRIES,
            "{} entries fit, not every one",
            parsed.order.len()
        );
        assert!(
            parsed
                .order
                .iter()
                .eq(mark.order.iter().take(parsed.order.len()))
        );
        assert!(
            parsed.get(Path::new("/m/0.cjs")).is_none(),
            "the earliest went"
        );
        for path in &parsed.order {
            let (read, written) = (parsed.get(path).unwrap(), mark.get(path).unwrap());
            assert_eq!((read.len, read.modified), (written.len, written.modified));
        }
    }

    /// A mark larger than the cap is not read; the next run writes it again.
    #[test]
    fn a_mark_past_the_byte_limit_is_not_read() {
        let path =
            std::env::temp_dir().join(format!("verve-mark-oversized-{}.seen", std::process::id()));
        fs::write(&path, vec![0u8; MAX_BYTES as usize + 1]).unwrap();
        assert!(Mark::read(&path).is_none());
        let _ = fs::remove_file(&path);
    }
}
