//! The pack's bytes: one file per build holding the compiled code of the modules a run loaded,
//! as a header, an index of (key, offset, length, tag) entries, then the code itself.
//!
//! Split out of [`crate::code_cache`], which keeps what a run decides with them: the format is
//! parsed and written here, and never trusted — every offset and count is checked against the
//! file before anything is read (`docs/startup.md`, "Verve: the pack on disk").

use std::{
    collections::{HashMap, HashSet},
    fs,
};

/// What a pack starts with: the format's name and version. A larger number is not read.
pub(super) const MAGIC: &[u8; 8] = b"VERVEPK\x02";

/// The most a pack holds; past it, the entries no run has read lately go first. A larger file
/// is not a pack this build wrote, and is not read.
pub(super) const MAX_PACK: usize = 64 << 20;

pub(super) const KEY_LEN: usize = 32;
pub(super) const TAG_LEN: usize = 32;

/// The length of an entry in a pack's index: its key, where its code is and how long, its tag.
pub(super) const INDEX_ENTRY: usize = KEY_LEN + 8 + 8 + TAG_LEN;

pub(super) type Key = [u8; KEY_LEN];
pub(super) type Tag = [u8; TAG_LEN];

/// Where an entry's code is in a pack (from the start of the code), how long it is, and its
/// tag.
#[derive(Clone, Copy)]
pub(super) struct Located {
    pub(super) offset: u64,
    pub(super) len: u64,
    pub(super) tag: Tag,
}

/// A pack being written: its entries in order, until it is full.
#[derive(Default)]
pub(super) struct PackWriter {
    entries: Vec<(Key, Vec<u8>, Tag)>,
    size: usize,
    written: HashSet<Key>,
}

impl PackWriter {
    /// Adds an entry, unless one with its key is in already or the pack is full.
    pub(super) fn put(&mut self, key: &Key, payload: &[u8], tag: &Tag) {
        let size = INDEX_ENTRY + payload.len();
        if self.size + size > MAX_PACK || !self.written.insert(*key) {
            return;
        }
        self.size += size;
        self.entries.push((*key, payload.to_vec(), *tag));
    }

    /// Whether an entry with `key` is in already.
    pub(super) fn has(&self, key: &Key) -> bool {
        self.written.contains(key)
    }

    /// The pack's bytes: the header, the index, then the code of each entry.
    pub(super) fn finish(self) -> Vec<u8> {
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
pub(super) fn read_index(file: &mut fs::File) -> Option<(HashMap<Key, Located>, u64)> {
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
pub(super) fn read_code(file: &mut fs::File, code_start: u64, located: Located) -> Option<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(code_start.checked_add(located.offset)?))
        .ok()?;
    let mut code = vec![0u8; usize::try_from(located.len).ok()?];
    file.read_exact(&mut code).ok()?;
    Some(code)
}

/// The entries of a pack read whole, as far as its index holds, by key and in order.
#[derive(Default)]
pub(super) struct OnDisk<'a> {
    pub(super) entries: HashMap<Key, (&'a [u8], Tag)>,
    pub(super) order: Vec<Key>,
}

/// Reads a whole pack's bytes into its entries, in the order it holds them.
pub(super) fn parse_pack(bytes: &[u8]) -> OnDisk<'_> {
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
