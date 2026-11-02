//! What the tests share: a folder of a test's own, and a writer of archives, so no test depends
//! on `@electron/asar`. The writer lays an archive out as the format describes it — two
//! Pickles, a JSON header, the contents, `offset`s as decimal strings — and puts what is marked
//! `unpacked` in `<archive>.unpacked` instead.

#![allow(dead_code, clippy::unwrap_used)] // Not every test file uses every helper.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use verve::encoding::hex;

/// A folder of the test's own, removed when it is done; `.0` is its path.
pub struct Dir(pub PathBuf);

impl Dir {
    /// An empty folder for `test`, at its real path and without Windows' `\\?\` prefix.
    pub fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("verve-asar-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let real = std::fs::canonicalize(&dir).unwrap_or(dir);
        Self(match real.to_string_lossy().strip_prefix(r"\\?\") {
            Some(rest) => PathBuf::from(rest),
            None => real,
        })
    }

    /// The path of `name` in the folder.
    pub fn at(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

enum Item {
    Dir(BTreeMap<String, Item>),
    File {
        bytes: Vec<u8>,
        unpacked: bool,
        executable: bool,
    },
    Link(String),
}

/// An archive to be written.
pub struct Pack {
    root: BTreeMap<String, Item>,
    block_size: Option<usize>,
}

impl Pack {
    pub fn new() -> Self {
        Self {
            root: BTreeMap::new(),
            block_size: None,
        }
    }

    /// A packed file at `path`.
    pub fn file(self, path: &str, bytes: &[u8]) -> Self {
        self.put(path, bytes, false, false)
    }

    /// A packed file marked executable.
    pub fn executable(self, path: &str, bytes: &[u8]) -> Self {
        self.put(path, bytes, false, true)
    }

    /// A file kept outside the archive, in `<archive>.unpacked`.
    pub fn unpacked(self, path: &str, bytes: &[u8]) -> Self {
        self.put(path, bytes, true, false)
    }

    /// A link at `path` to `target`, a path from the archive's root.
    pub fn link(mut self, path: &str, target: &str) -> Self {
        place(&mut self.root, path, Item::Link(target.to_owned()));
        self
    }

    /// An empty folder.
    pub fn dir(mut self, path: &str) -> Self {
        place(&mut self.root, path, Item::Dir(BTreeMap::new()));
        self
    }

    /// Records each packed file's digests, `block_size` bytes to a block.
    pub fn with_integrity(mut self, block_size: usize) -> Self {
        self.block_size = Some(block_size);
        self
    }

    fn put(mut self, path: &str, bytes: &[u8], unpacked: bool, executable: bool) -> Self {
        let item = Item::File {
            bytes: bytes.to_vec(),
            unpacked,
            executable,
        };
        place(&mut self.root, path, item);
        self
    }

    /// Writes the archive to `at`, and the unpacked files beside it.
    pub fn write(&self, at: &Path) {
        let mut contents = Vec::new();
        let header = self.header(&self.root, "", at, &mut contents);
        let json = serde_json::to_vec(&json!({ "files": header })).unwrap();
        let bytes = raw(&json, &contents);
        std::fs::write(at, bytes).unwrap();
    }

    fn header(
        &self,
        items: &BTreeMap<String, Item>,
        prefix: &str,
        at: &Path,
        contents: &mut Vec<u8>,
    ) -> Map<String, Value> {
        let mut files = Map::new();
        for (name, item) in items {
            let path = format!("{prefix}{name}");
            let record = match item {
                Item::Dir(children) => {
                    json!({ "files": self.header(children, &format!("{path}/"), at, contents) })
                }
                Item::Link(target) => json!({ "link": target }),
                Item::File {
                    bytes,
                    unpacked,
                    executable,
                } => self.file_record(&path, bytes, *unpacked, *executable, at, contents),
            };
            files.insert(name.clone(), record);
        }
        files
    }

    fn file_record(
        &self,
        path: &str,
        bytes: &[u8],
        unpacked: bool,
        executable: bool,
        at: &Path,
        contents: &mut Vec<u8>,
    ) -> Value {
        let mut record = json!({ "size": bytes.len() });
        if unpacked {
            let mut name = at.as_os_str().to_owned();
            name.push(".unpacked");
            let real = PathBuf::from(name).join(path);
            std::fs::create_dir_all(real.parent().unwrap()).unwrap();
            std::fs::write(real, bytes).unwrap();
            record["unpacked"] = json!(true);
        } else {
            record["offset"] = json!(contents.len().to_string());
            contents.extend(bytes);
        }
        if executable {
            record["executable"] = json!(true);
        }
        if let Some(size) = self.block_size {
            record["integrity"] = json!({
                "algorithm": "SHA256",
                "hash": hex(&Sha256::digest(bytes)),
                "blockSize": size,
                "blocks": bytes.chunks(size).map(|b| hex(&Sha256::digest(b))).collect::<Vec<_>>(),
            });
        }
        record
    }
}

/// An archive made of `json` as its header and `contents` after it.
pub fn raw(json: &[u8], contents: &[u8]) -> Vec<u8> {
    let padded = json.len().div_ceil(4) * 4;
    let header_size = 8 + padded as u32;
    let mut bytes = Vec::new();
    for word in [4, header_size, header_size - 4, json.len() as u32] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(json);
    bytes.resize(16 + padded, 0);
    bytes.extend(contents);
    bytes
}

fn place(root: &mut BTreeMap<String, Item>, path: &str, item: Item) {
    let mut names: Vec<&str> = path.split('/').collect();
    let last = names.pop().unwrap();
    let mut folder = root;
    for name in names {
        let Item::Dir(children) = folder
            .entry(name.to_owned())
            .or_insert_with(|| Item::Dir(BTreeMap::new()))
        else {
            panic!("{name} is not a folder");
        };
        folder = children;
    }
    folder.insert(last.to_owned(), item);
}
