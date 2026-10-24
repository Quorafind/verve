//! IndexedDB, native: databases of object stores, each a sorted map from key to a value
//! cloned as `structuredClone` clones, with indexes, cursors, key ranges and transactions —
//! what `localforage`, `idb` and Dexie are written against.
//!
//! An operation is done at once, in Rust, in the order it was asked for; its `success` (or
//! `error`) is heard as a task of its own, after the script that asked. A transaction commits
//! when a task finds it with nothing pending — so one that `await`s between its requests, as
//! the `idb` library does, lives on — and an aborted one is put back as it was. A database is
//! kept on disk, one JSON file each, in the folder the host names
//! ([`crate::RuntimeX::set_storage_dir`]); with none, in memory for the life of the runtime.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{
        FunctionObjectBuilder,
        builtins::{JsArray, JsArrayBuffer, JsDate, JsMap, JsSet, JsUint8Array},
    },
    property::{Attribute, PropertyDescriptor},
};
use serde::{Deserialize, Serialize};

use crate::shared;

// ─── Keys and values, in Rust ───────────────────────────────────────────────────────────────

/// A key, ordered as IndexedDB orders them: numbers, then dates, strings, binary, arrays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Key {
    Number(f64),
    Date(f64),
    String(String),
    Binary(Vec<u8>),
    Array(Vec<Key>),
}

impl Key {
    fn rank(&self) -> u8 {
        match self {
            Key::Number(_) => 0,
            Key::Date(_) => 1,
            Key::String(_) => 2,
            Key::Binary(_) => 3,
            Key::Array(_) => 4,
        }
    }
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Key::Number(a), Key::Number(b)) | (Key::Date(a), Key::Date(b)) => a.total_cmp(b),
            // Strings compare by their UTF-16 code units, as JavaScript's do.
            (Key::String(a), Key::String(b)) => a.encode_utf16().cmp(b.encode_utf16()),
            (Key::Binary(a), Key::Binary(b)) => a.cmp(b),
            (Key::Array(a), Key::Array(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

/// A value as it is kept: a structured clone, in Rust.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Stored {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    BigInt(String),
    Date(f64),
    Array(Vec<Stored>),
    Object(Vec<(String, Stored)>),
    Map(Vec<(Stored, Stored)>),
    Set(Vec<Stored>),
    /// Bytes, and what held them: `ArrayBuffer`, `Uint8Array`…
    Bytes(Vec<u8>, String),
}

fn data_error(message: &str) -> JsError {
    dom_error("DataError", message)
}

fn dom_error(name: &str, message: &str) -> JsError {
    JsNativeError::error().with_message(format!("{name}: {message}")).into()
}

fn dom_exception(context: &mut Context, name: &str, message: &str) -> JsValue {
    let error = JsNativeError::error().with_message(message.to_owned()).into_opaque(context);
    let _ = error.set(js_string!("name"), JsString::from(name), false, context);
    error.into()
}

fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

fn text_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

fn js_text(text: &str) -> JsValue {
    JsString::from(text).into()
}

fn list_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let list = crate::call(context, &from, &JsValue::undefined(), std::slice::from_ref(value))?;
    let Some(list) = list.as_object() else { return Ok(Vec::new()) };
    let list = JsArray::from_object(list)?;
    (0..list.length(context)?).map(|at| list.get(at, context)).collect()
}

/// A value as a key, or `None` when it is not a valid one.
fn key_of(value: &JsValue, context: &mut Context) -> JsResult<Option<Key>> {
    if let Some(n) = value.as_number() {
        return Ok((!n.is_nan()).then_some(Key::Number(n)));
    }
    if let Some(s) = value.as_string() {
        return Ok(Some(Key::String(s.to_std_string_escaped())));
    }
    let Some(object) = value.as_object() else { return Ok(None) };
    if let Ok(date) = JsDate::from_object(object.clone()) {
        let time = date.get_time(context)?.to_number(context)?;
        return Ok((!time.is_nan()).then_some(Key::Date(time)));
    }
    if object.is_array() {
        let mut keys = Vec::new();
        for item in list_of(value, context)? {
            match key_of(&item, context)? {
                Some(key) => keys.push(key),
                None => return Ok(None),
            }
        }
        return Ok(Some(Key::Array(keys)));
    }
    if let Some(bytes) = crate::encoding::bytes_of(value, context)? {
        return Ok(Some(Key::Binary(bytes)));
    }
    Ok(None)
}

fn require_key(value: &JsValue, context: &mut Context) -> JsResult<Key> {
    key_of(value, context)?.ok_or_else(|| data_error("The parameter is not a valid key."))
}

fn key_value(key: &Key, context: &mut Context) -> JsResult<JsValue> {
    Ok(match key {
        Key::Number(n) => (*n).into(),
        Key::String(s) => js_text(s),
        Key::Date(t) => {
            let date = JsDate::new(context);
            date.set_time(*t, context)?;
            date.into()
        }
        Key::Binary(bytes) => JsUint8Array::from_iter(bytes.clone(), context)?.buffer(context)?,
        Key::Array(keys) => {
            let mut items = Vec::new();
            for key in keys {
                items.push(key_value(key, context)?);
            }
            JsArray::from_iter(items, context).into()
        }
    })
}

/// A value as it is kept — the clone `put` takes.
fn store_value(value: &JsValue, context: &mut Context, depth: usize) -> JsResult<Stored> {
    if depth > 200 {
        return Err(dom_error("DataCloneError", "The value is nested too deeply (or holds a cycle)."));
    }
    if value.is_undefined() {
        return Ok(Stored::Undefined);
    }
    if value.is_null() {
        return Ok(Stored::Null);
    }
    if let Some(b) = value.as_boolean() {
        return Ok(Stored::Bool(b));
    }
    if let Some(n) = value.as_number() {
        return Ok(Stored::Number(n));
    }
    if let Some(s) = value.as_string() {
        return Ok(Stored::String(s.to_std_string_escaped()));
    }
    if value.is_bigint() {
        return Ok(Stored::BigInt(text_of(value, context)?));
    }
    let Some(object) = value.as_object() else {
        return Err(dom_error("DataCloneError", "The value could not be cloned."));
    };
    if object.is_callable() {
        return Err(dom_error("DataCloneError", "A function could not be cloned."));
    }
    if let Ok(date) = JsDate::from_object(object.clone()) {
        return Ok(Stored::Date(date.get_time(context)?.to_number(context)?));
    }
    if JsArrayBuffer::from_object(object.clone()).is_ok() {
        let bytes = crate::encoding::bytes_of(value, context)?.unwrap_or_default();
        return Ok(Stored::Bytes(bytes, "ArrayBuffer".into()));
    }
    if let Some(bytes) = crate::encoding::bytes_of(value, context)? {
        let kind = object
            .get(js_string!("constructor"), context)
            .ok()
            .and_then(|c| c.as_object())
            .and_then(|c| c.get(js_string!("name"), context).ok())
            .and_then(|n| n.as_string().map(|s| s.to_std_string_escaped()))
            .unwrap_or_else(|| "Uint8Array".into());
        return Ok(Stored::Bytes(bytes, kind));
    }
    if let Ok(map) = JsMap::from_object(object.clone()) {
        let mut pairs = Vec::new();
        for pair in list_of(&map.entries(context)?.into(), context)? {
            let pair = list_of(&pair, context)?;
            let k = store_value(pair.first().unwrap_or(&JsValue::undefined()), context, depth + 1)?;
            let v = store_value(pair.get(1).unwrap_or(&JsValue::undefined()), context, depth + 1)?;
            pairs.push((k, v));
        }
        return Ok(Stored::Map(pairs));
    }
    if let Ok(set) = JsSet::from_object(object.clone()) {
        let mut items = Vec::new();
        for item in list_of(&set.values(context)?.into(), context)? {
            items.push(store_value(&item, context, depth + 1)?);
        }
        return Ok(Stored::Set(items));
    }
    if object.is_array() {
        let mut items = Vec::new();
        for item in list_of(value, context)? {
            items.push(store_value(&item, context, depth + 1)?);
        }
        return Ok(Stored::Array(items));
    }
    let keys_fn = context.intrinsics().constructors().object().constructor().get(js_string!("keys"), context)?;
    let keys = crate::call(context, &keys_fn, &JsValue::undefined(), std::slice::from_ref(value))?;
    let mut fields = Vec::new();
    for key in list_of(&keys, context)? {
        let name = text_of(&key, context)?;
        let item = object.get(JsString::from(name.as_str()), context)?;
        fields.push((name, store_value(&item, context, depth + 1)?));
    }
    Ok(Stored::Object(fields))
}

/// A kept value, made again in the realm.
fn restore_value(stored: &Stored, context: &mut Context) -> JsResult<JsValue> {
    Ok(match stored {
        Stored::Undefined => JsValue::undefined(),
        Stored::Null => JsValue::null(),
        Stored::Bool(b) => (*b).into(),
        Stored::Number(n) => (*n).into(),
        Stored::String(s) => js_text(s),
        Stored::BigInt(s) => boa_engine::JsBigInt::from_string(s).map_or(JsValue::undefined(), JsValue::from),
        Stored::Date(t) => {
            let date = JsDate::new(context);
            date.set_time(*t, context)?;
            date.into()
        }
        Stored::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                out.push(restore_value(item, context)?);
            }
            JsArray::from_iter(out, context).into()
        }
        Stored::Object(fields) => {
            let object = JsObject::with_object_proto(context.intrinsics());
            for (name, item) in fields {
                let item = restore_value(item, context)?;
                object.set(JsString::from(name.as_str()), item, false, context)?;
            }
            object.into()
        }
        Stored::Map(pairs) => {
            let map = JsMap::new(context);
            for (k, v) in pairs {
                let k = restore_value(k, context)?;
                let v = restore_value(v, context)?;
                map.set(k, v, context)?;
            }
            map.into()
        }
        Stored::Set(items) => {
            let set = JsSet::new(context);
            for item in items {
                let item = restore_value(item, context)?;
                set.add(item, context)?;
            }
            set.into()
        }
        Stored::Bytes(bytes, kind) => {
            let array = JsUint8Array::from_iter(bytes.clone(), context)?;
            if kind == "ArrayBuffer" {
                array.buffer(context)?
            } else if kind == "Uint8Array" {
                array.into()
            } else {
                // Another view: made by its constructor over the bytes' buffer.
                let buffer = array.buffer(context)?;
                let class = context.global_object().get(JsString::from(kind.as_str()), context)?;
                match class.as_object() {
                    Some(class) if class.is_constructor() => class.construct(&[buffer], None, context)?.into(),
                    _ => array.into(),
                }
            }
        }
    })
}

// ─── Key paths ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum KeyPath {
    One(String),
    Many(Vec<String>),
}

fn key_path_of(value: &JsValue, context: &mut Context) -> JsResult<Option<KeyPath>> {
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    if value.as_object().is_some_and(|o| o.is_array()) {
        let mut paths = Vec::new();
        for item in list_of(value, context)? {
            paths.push(text_of(&item, context)?);
        }
        return Ok(Some(KeyPath::Many(paths)));
    }
    Ok(Some(KeyPath::One(text_of(value, context)?)))
}

fn key_path_value(path: &Option<KeyPath>, context: &mut Context) -> JsResult<JsValue> {
    Ok(match path {
        None => JsValue::null(),
        Some(KeyPath::One(p)) => js_text(p),
        Some(KeyPath::Many(ps)) => {
            let items: Vec<JsValue> = ps.iter().map(|p| js_text(p)).collect();
            JsArray::from_iter(items, context).into()
        }
    })
}

/// What `path` reads in a kept value: `a.b` field by field; `""` the value itself.
fn stored_at<'a>(value: &'a Stored, path: &str) -> Option<&'a Stored> {
    if path.is_empty() {
        return Some(value);
    }
    let mut at = value;
    for part in path.split('.') {
        at = match at {
            Stored::Object(fields) => &fields.iter().find(|(n, _)| n == part)?.1,
            Stored::Array(items) if part == "length" => return None,
            _ => return None,
        };
        let _ = &at;
    }
    Some(at)
}

fn stored_key(value: &Stored) -> Option<Key> {
    Some(match value {
        Stored::Number(n) if !n.is_nan() => Key::Number(*n),
        Stored::String(s) => Key::String(s.clone()),
        Stored::Date(t) if !t.is_nan() => Key::Date(*t),
        Stored::Bytes(bytes, _) => Key::Binary(bytes.clone()),
        Stored::Array(items) => Key::Array(items.iter().map(stored_key).collect::<Option<Vec<_>>>()?),
        _ => return None,
    })
}

fn extract_key(value: &Stored, path: &KeyPath) -> Option<Key> {
    match path {
        KeyPath::One(p) => stored_key(stored_at(value, p)?),
        KeyPath::Many(ps) => Some(Key::Array(
            ps.iter().map(|p| stored_at(value, p).and_then(stored_key)).collect::<Option<Vec<_>>>()?,
        )),
    }
}

/// Writes `key` into the value at `path`, for a store that makes its own keys.
fn inject_key(value: &mut Stored, path: &str, key: &Key) {
    let parts: Vec<&str> = path.split('.').collect();
    let mut at = value;
    for (i, part) in parts.iter().enumerate() {
        let Stored::Object(fields) = at else { return };
        if i + 1 == parts.len() {
            let stored = match key {
                Key::Number(n) => Stored::Number(*n),
                Key::String(s) => Stored::String(s.clone()),
                _ => return,
            };
            fields.retain(|(n, _)| n != part);
            fields.push((part.to_string(), stored));
            return;
        }
        if !fields.iter().any(|(n, _)| n == part) {
            fields.push((part.to_string(), Stored::Object(Vec::new())));
        }
        at = &mut fields.iter_mut().find(|(n, _)| n == part).expect("just made").1;
    }
}

// ─── Databases ──────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    key_path: Option<KeyPath>,
    auto_increment: bool,
    next_key: f64,
    #[serde(with = "pairs")]
    records: BTreeMap<Key, Stored>,
    indexes: BTreeMap<String, Index>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Index {
    key_path: KeyPath,
    unique: bool,
    multi_entry: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Database {
    version: u64,
    stores: BTreeMap<String, Store>,
}

impl Store {
    /// The index's entries: each record's index key (each item of it, for a multi-entry
    /// index) and its primary key, in order.
    fn index_entries(&self, index: &Index) -> Vec<(Key, Key)> {
        let mut out = Vec::new();
        for (primary, value) in &self.records {
            let Some(key) = extract_key(value, &index.key_path) else { continue };
            match (index.multi_entry, key) {
                (true, Key::Array(items)) => {
                    let mut items = items;
                    items.sort();
                    items.dedup();
                    for item in items {
                        out.push((item, primary.clone()));
                    }
                }
                (_, key) => out.push((key, primary.clone())),
            }
        }
        out.sort();
        out
    }

    /// Whether putting `value` at `primary` would break a unique index.
    fn breaks_unique(&self, primary: &Key, value: &Stored) -> Option<String> {
        for (name, index) in &self.indexes {
            if !index.unique {
                continue;
            }
            let Some(key) = extract_key(value, &index.key_path) else { continue };
            let clash = self.records.iter().any(|(pk, other)| {
                pk != primary && extract_key(other, &index.key_path).as_ref() == Some(&key)
            });
            if clash {
                return Some(name.clone());
            }
        }
        None
    }
}

/// Records kept as a list of pairs: JSON's object keys are only strings.
mod pairs {
    use super::{Key, Stored};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(records: &BTreeMap<Key, Stored>, s: S) -> Result<S::Ok, S::Error> {
        let list: Vec<(&Key, &Stored)> = records.iter().collect();
        list.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<Key, Stored>, D::Error> {
        Ok(Vec::<(Key, Stored)>::deserialize(d)?.into_iter().collect())
    }
}

/// A key range: `lower`/`upper`, each open or not.
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    lower: Option<Key>,
    upper: Option<Key>,
    lower_open: bool,
    upper_open: bool,
}

impl Range {
    fn all() -> Self {
        Range { lower: None, upper: None, lower_open: false, upper_open: false }
    }

    fn only(key: Key) -> Self {
        Range { lower: Some(key.clone()), upper: Some(key), lower_open: false, upper_open: false }
    }

    fn contains(&self, key: &Key) -> bool {
        let above = match &self.lower {
            None => true,
            Some(l) => if self.lower_open { key > l } else { key >= l },
        };
        let below = match &self.upper {
            None => true,
            Some(u) => if self.upper_open { key < u } else { key <= u },
        };
        above && below
    }
}

/// What a request was asked with: a key range, a key, or nothing.
fn range_of(value: &JsValue, context: &mut Context) -> JsResult<Range> {
    if value.is_null_or_undefined() {
        return Ok(Range::all());
    }
    if let Some(object) = value.as_object()
        && object.has_own_property(js_string!("__range"), context)?
    {
        let lower = object.get(js_string!("lower"), context)?;
        let upper = object.get(js_string!("upper"), context)?;
        return Ok(Range {
            lower: if lower.is_undefined() { None } else { key_of(&lower, context)? },
            upper: if upper.is_undefined() { None } else { key_of(&upper, context)? },
            lower_open: object.get(js_string!("lowerOpen"), context)?.to_boolean(),
            upper_open: object.get(js_string!("upperOpen"), context)?.to_boolean(),
        });
    }
    Ok(Range::only(require_key(value, context)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    ReadOnly,
    ReadWrite,
    VersionChange,
}

/// One write a transaction made, as what it replaced.
enum Undo {
    /// The record at `key` was `was` (`None`: there was none).
    Record { store: String, key: Key, was: Option<Stored> },
    /// The store's next generated key was `was`.
    NextKey { store: String, was: f64 },
    /// The store's records were all of these (`clear`).
    Records { store: String, was: BTreeMap<Key, Stored> },
}

struct Transaction {
    db: String,
    mode: Mode,
    stores: Vec<String>,
    object: JsObject,
    pending: usize,
    finished: bool,
    /// The database as it was when a version change began, to put back on abort.
    before: Option<Database>,
    /// What a read-write transaction's writes replaced, oldest first, to put back on abort. A
    /// copy of the whole database for each transaction made a cache written note by note copy
    /// everything cached so far once a note.
    undo: Vec<Undo>,
    /// The open request a version change belongs to: it succeeds once the change is done.
    open_request: Option<JsObject>,
    /// Whether its `complete` has been looked for once already this time round.
    checking: bool,
}

struct Cursor {
    tx: u32,
    store: String,
    request: JsObject,
    object: JsObject,
    /// What it walks: (key, primary key), in its direction.
    entries: Vec<(Key, Key)>,
    at: usize,
    keys_only: bool,

}

/// Everything IndexedDB keeps for a realm.
#[derive(Default)]
pub(crate) struct IdbState {
    databases: HashMap<String, Database>,
    transactions: HashMap<u32, Transaction>,
    cursors: HashMap<u32, Cursor>,
    next_id: u32,
    dir: Option<PathBuf>,
    /// Databases changed since they were last written, by name.
    dirty: std::collections::BTreeSet<String>,
    /// When dirty databases were last written, on the realm's clock.
    flushed_at: f64,
    /// The thread that writes databases to disk, once one has been written.
    writer: Option<Writer>,
}

/// A thread that serialises databases and writes them, in the order they are handed to it, so
/// the realm's own thread spends a copy on a write rather than the write.
struct Writer {
    to: std::sync::mpsc::Sender<Job>,
    thread: std::thread::JoinHandle<()>,
}

/// What the writer is asked to do with a database's file.
enum Job {
    Write(PathBuf, Database),
    /// Removed — after any write of it handed over before.
    Remove(PathBuf),
}

impl Writer {
    fn start() -> Option<Self> {
        let (to, from) = std::sync::mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("indexeddb writer".into())
            .spawn(move || {
                for job in from {
                    match job {
                        Job::Write(file, db) => write_database(&file, &db),
                        Job::Remove(file) => {
                            let _ = std::fs::remove_file(file);
                        }
                    }
                }
            })
            .ok()?;
        Some(Self { to, thread })
    }
}

fn write_database(file: &std::path::Path, db: &Database) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(db) {
        let _ = std::fs::write(file, text);
    }
}

/// How often changed databases are written, at most, in milliseconds. A database is one file,
/// written whole: writing it at every commit made a cache that grows note by note cost a write
/// of everything cached so far for each note — the window's beat spent on the disk.
const FLUSH_EVERY: f64 = 3_000.;

impl Drop for IdbState {
    /// Whatever has not been written yet is, as the realm goes: what the writer has, then the
    /// rest.
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            drop(writer.to);
            let _ = writer.thread.join();
        }
        self.flush();
    }
}

impl IdbState {
    /// Writes every changed database now.
    fn flush(&mut self) {
        for name in std::mem::take(&mut self.dirty) {
            self.save(&name);
        }
    }

    /// Hands the changed databases to the writer if they have waited long enough.
    pub(crate) fn flush_due(&mut self, now: f64) {
        if self.dirty.is_empty() {
            self.flushed_at = now;
            return;
        }
        if now - self.flushed_at < FLUSH_EVERY {
            return;
        }
        self.flushed_at = now;
        if self.writer.is_none() {
            self.writer = Writer::start();
        }
        for name in std::mem::take(&mut self.dirty) {
            let (Some(file), Some(db)) = (self.file(&name), self.databases.get(&name)) else {
                continue;
            };
            let copy = db.clone();
            let sent = self
                .writer
                .as_ref()
                .is_some_and(|writer| writer.to.send(Job::Write(file.clone(), copy)).is_ok());
            if !sent {
                // No thread to write it on: written here.
                self.save(&name);
            }
        }
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    fn file(&self, name: &str) -> Option<PathBuf> {
        let dir = self.dir.as_ref()?;
        let safe: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || "-_.".contains(c) { c } else { '_' })
            .collect();
        Some(dir.join(format!("{safe}.idb.json")))
    }

    /// The database, read from disk the first time it is asked for.
    fn database(&mut self, name: &str) -> Option<&mut Database> {
        if !self.databases.contains_key(name)
            && let Some(file) = self.file(name)
            && let Ok(text) = std::fs::read_to_string(&file)
            && let Ok(db) = serde_json::from_str::<Database>(&text)
        {
            self.databases.insert(name.to_owned(), db);
        }
        self.databases.get_mut(name)
    }

    fn save(&self, name: &str) {
        let (Some(file), Some(db)) = (self.file(name), self.databases.get(name)) else { return };
        write_database(&file, db);
    }
}

fn idb(context: &Context) -> std::rc::Rc<crate::Shared> {
    shared(context)
}

// ─── Events ─────────────────────────────────────────────────────────────────────────────────

fn object(context: &mut Context) -> JsObject {
    JsObject::with_object_proto(context.intrinsics())
}

fn with_proto(context: &mut Context, class: &str) -> JsObject {
    let made = object(context);
    if let Ok(class) = context.global_object().get(JsString::from(class), context)
        && let Some(class) = class.as_object()
        && let Ok(prototype) = class.get(js_string!("prototype"), context)
    {
        made.set_prototype(prototype.as_object());
    }
    made
}

fn hidden(target: &JsObject, name: &str, value: impl Into<JsValue>, context: &mut Context) -> JsResult<()> {
    target.define_property_or_throw(
        JsString::from(name),
        PropertyDescriptor::builder().value(value.into()).writable(true).enumerable(false).configurable(true),
        context,
    )?;
    Ok(())
}

fn hidden_number(target: &JsValue, name: &str, context: &mut Context) -> Option<u32> {
    target.as_object()?.get(JsString::from(name), context).ok()?.as_number().map(|n| n as u32)
}

fn hidden_text(target: &JsValue, name: &str, context: &mut Context) -> Option<String> {
    let value = target.as_object()?.get(JsString::from(name), context).ok()?;
    value.as_string().map(|s| s.to_std_string_escaped())
}

/// An event target's listeners and `on…` handler hear `kind`. Answers whether a listener
/// called `preventDefault`.
fn fire(target: &JsObject, kind: &str, extra: &[(&str, JsValue)], context: &mut Context) -> bool {
    let event = object(context);
    let _ = event.set(js_string!("type"), js_text(kind), false, context);
    let _ = event.set(js_string!("target"), target.clone(), false, context);
    let _ = event.set(js_string!("currentTarget"), target.clone(), false, context);
    for (key, value) in extra {
        let _ = event.set(JsString::from(*key), value.clone(), false, context);
    }
    let _ = hidden(&event, "__prevented", false, context);
    let prevent = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|this, _, cx| {
            if let Some(event) = this.as_object() {
                event.set(js_string!("__prevented"), true, false, cx)?;
                event.set(js_string!("defaultPrevented"), true, false, cx)?;
            }
            Ok(JsValue::undefined())
        }),
    )
    .build();
    let _ = event.set(js_string!("preventDefault"), prevent, false, context);
    let stop = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()))).build();
    let _ = event.set(js_string!("stopPropagation"), stop, false, context);
    let mut handlers = Vec::new();
    if let Ok(handler) = target.get(JsString::from(format!("on{kind}")), context)
        && handler.is_callable()
    {
        handlers.push(handler);
    }
    if let Ok(listeners) = target.get(js_string!("__listeners"), context)
        && let Some(listeners) = listeners.as_object()
        && let Ok(list) = listeners.get(JsString::from(kind), context)
        && let Ok(list) = list_of(&list, context)
    {
        handlers.extend(list);
    }
    for handler in handlers {
        if let Err(error) = crate::call(context, &handler, &target.clone().into(), &[event.clone().into()]) {
            crate::report(context, &error);
        }
    }
    event.get(js_string!("__prevented"), context).is_ok_and(|p| p.to_boolean())
}

const TARGET_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("addEventListener", 2, |this, args, cx| {
        let Some(target) = this.as_object() else { return Ok(JsValue::undefined()) };
        let kind = text_of(&arg(args, 0), cx)?;
        let listener = arg(args, 1);
        if !listener.is_callable() {
            return Ok(JsValue::undefined());
        }
        let mut table = target.get(js_string!("__listeners"), cx)?;
        if !table.is_object() {
            table = object(cx).into();
            hidden(&target, "__listeners", table.clone(), cx)?;
        }
        let table = table.as_object().expect("an object");
        let mut list = table.get(JsString::from(kind.as_str()), cx)?;
        if !list.is_object() {
            list = JsArray::new(cx)?.into();
            table.set(JsString::from(kind.as_str()), list.clone(), false, cx)?;
        }
        JsArray::from_object(list.as_object().expect("an array"))?.push(listener, cx)?;
        Ok(JsValue::undefined())
    }),
    ("removeEventListener", 2, |this, args, cx| {
        let Some(target) = this.as_object() else { return Ok(JsValue::undefined()) };
        let kind = text_of(&arg(args, 0), cx)?;
        let listener = arg(args, 1);
        let table = target.get(js_string!("__listeners"), cx)?;
        if let Some(table) = table.as_object() {
            let list = table.get(JsString::from(kind.as_str()), cx)?;
            let kept: Vec<JsValue> = list_of(&list, cx)?.into_iter().filter(|l| *l != listener).collect();
            let fresh = JsArray::from_iter(kept, cx);
            table.set(JsString::from(kind.as_str()), fresh, false, cx)?;
        }
        Ok(JsValue::undefined())
    }),
];

/// Runs `f` as a task of its own, after the script that asked and its microtasks.
fn task(context: &mut Context, f: NativeFunction) -> JsResult<()> {
    let function = FunctionObjectBuilder::new(context.realm(), f).build();
    let set_timeout = context.global_object().get(js_string!("setTimeout"), context)?;
    crate::call(context, &set_timeout, &JsValue::undefined(), &[function.into(), 0.into()])?;
    Ok(())
}

// ─── Requests ───────────────────────────────────────────────────────────────────────────────

fn new_request(context: &mut Context, source: JsValue, tx: Option<u32>, class: &str) -> JsResult<JsObject> {
    let request = with_proto(context, class);
    request.set(js_string!("result"), JsValue::undefined(), false, context)?;
    request.set(js_string!("error"), JsValue::null(), false, context)?;
    request.set(js_string!("readyState"), js_string!("pending"), false, context)?;
    request.set(js_string!("source"), source, false, context)?;
    let tx_object = tx
        .and_then(|id| idb(context).idb.borrow().transactions.get(&id).map(|t| t.object.clone()))
        .map_or(JsValue::null(), JsValue::from);
    request.set(js_string!("transaction"), tx_object, false, context)?;
    for handler in ["onsuccess", "onerror"] {
        request.set(JsString::from(handler), JsValue::null(), false, context)?;
    }
    if let Some(id) = tx {
        if let Some(t) = idb(context).idb.borrow_mut().transactions.get_mut(&id) {
            t.pending += 1;
        }
    }
    Ok(request)
}

/// The request's answer heard as a task: `success` with `result`, or `error`. The
/// transaction it belongs to is then looked at to see whether it is done.
fn answer(context: &mut Context, request: &JsObject, tx: Option<u32>, outcome: Result<JsValue, (String, String)>) -> JsResult<()> {
    let captures = (request.clone(), tx.map_or(-1., f64::from), match &outcome {
        Ok(value) => (true, value.clone(), JsValue::undefined(), JsValue::undefined()),
        Err((name, message)) => (false, JsValue::undefined(), js_text(name), js_text(message)),
    });
    task(
        context,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, (request, tx, (ok, value, name, message)): &(JsObject, f64, (bool, JsValue, JsValue, JsValue)), cx| {
                let tx = (*tx >= 0.).then_some(*tx as u32);
                request.set(js_string!("readyState"), js_string!("done"), false, cx)?;
                if *ok {
                    request.set(js_string!("result"), value.clone(), false, cx)?;
                    fire(request, "success", &[], cx);
                } else {
                    let (name, message) = (text_of(name, cx)?, text_of(message, cx)?);
                    let error = dom_exception(cx, &name, &message);
                    request.set(js_string!("error"), error.clone(), false, cx)?;
                    let prevented = fire(request, "error", &[], cx);
                    if let Some(tx) = tx {
                        let tx_object = idb(cx).idb.borrow().transactions.get(&tx).map(|t| t.object.clone());
                        if let Some(tx_object) = tx_object {
                            tx_object.set(js_string!("error"), error, false, cx)?;
                            let prevented_too = fire(&tx_object, "error", &[], cx);
                            if !prevented && !prevented_too {
                                abort(cx, tx)?;
                            }
                        }
                    }
                }
                if let Some(tx) = tx {
                    if let Some(t) = idb(cx).idb.borrow_mut().transactions.get_mut(&tx) {
                        t.pending = t.pending.saturating_sub(1);
                    }
                    check_done(cx, tx)?;
                }
                Ok(JsValue::undefined())
            },
            captures,
        ),
    )
}

/// A transaction with nothing pending is looked at again a task later — so what an answer's
/// handler or its promise asked of it in the meantime keeps it alive — and committed if still
/// idle then.
fn check_done(context: &mut Context, tx: u32) -> JsResult<()> {
    let idle = idb(context)
        .idb
        .borrow()
        .transactions
        .get(&tx)
        .is_some_and(|t| t.pending == 0 && !t.finished && !t.checking);
    if !idle {
        return Ok(());
    }
    if let Some(t) = idb(context).idb.borrow_mut().transactions.get_mut(&tx) {
        t.checking = true;
    }
    task(
        context,
        NativeFunction::from_copy_closure(move |_, _, cx| {
            let still = {
                let state = idb(cx);
                let mut state = state.idb.borrow_mut();
                match state.transactions.get_mut(&tx) {
                    Some(t) => {
                        t.checking = false;
                        t.pending == 0 && !t.finished
                    }
                    None => false,
                }
            };
            if still {
                commit(cx, tx)?;
            }
            Ok(JsValue::undefined())
        }),
    )
}

fn commit(context: &mut Context, tx: u32) -> JsResult<()> {
    let (object, db, mode, open_request) = {
        let state = idb(context);
        let mut state = state.idb.borrow_mut();
        let Some(t) = state.transactions.get_mut(&tx) else { return Ok(()) };
        t.finished = true;
        let out = (t.object.clone(), t.db.clone(), t.mode, t.open_request.clone());
        if mode_writes(out.2) {
            state.dirty.insert(out.1.clone());
        }
        state.transactions.remove(&tx);
        out
    };
    let _ = db;
    let _ = mode;
    fire(&object, "complete", &[], context);
    // A version change done: the database is open.
    if let Some(request) = open_request {
        request.set(js_string!("readyState"), js_string!("done"), false, context)?;
        request.set(js_string!("transaction"), JsValue::null(), false, context)?;
        fire(&request, "success", &[], context);
    }
    Ok(())
}

fn mode_writes(mode: Mode) -> bool {
    matches!(mode, Mode::ReadWrite | Mode::VersionChange)
}

fn abort(context: &mut Context, tx: u32) -> JsResult<()> {
    let (object, open_request) = {
        let state = idb(context);
        let mut state = state.idb.borrow_mut();
        let Some(t) = state.transactions.remove(&tx) else { return Ok(()) };
        if let Some(before) = t.before {
            state.databases.insert(t.db.clone(), before);
        } else if let Some(db) = state.database(&t.db) {
            // Each write taken back, newest first.
            for undo in t.undo.into_iter().rev() {
                match undo {
                    Undo::Record { store, key, was } => {
                        if let Some(s) = db.stores.get_mut(&store) {
                            match was {
                                Some(value) => {
                                    s.records.insert(key, value);
                                }
                                None => {
                                    s.records.remove(&key);
                                }
                            }
                        }
                    }
                    Undo::NextKey { store, was } => {
                        if let Some(s) = db.stores.get_mut(&store) {
                            s.next_key = was;
                        }
                    }
                    Undo::Records { store, was } => {
                        if let Some(s) = db.stores.get_mut(&store) {
                            s.records = was;
                        }
                    }
                }
            }
        }
        (t.object, t.open_request)
    };
    // Put back at once; heard as a task of its own, so a handler set after `abort()` — or
    // in the `catch` of a request that failed — hears it.
    task(
        context,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, (object, open_request): &(JsObject, JsValue), cx| {
                fire(object, "abort", &[], cx);
                if let Some(request) = open_request.as_object() {
                    let error = dom_exception(cx, "AbortError", "The version change was aborted.");
                    request.set(js_string!("error"), error, false, cx)?;
                    fire(&request, "error", &[], cx);
                }
                Ok(JsValue::undefined())
            },
            (object, open_request.map_or(JsValue::null(), JsValue::from)),
        ),
    )
}

// ─── indexedDB ──────────────────────────────────────────────────────────────────────────────

fn open(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = text_of(&arg(args, 0), context)?;
    let asked = arg(args, 1);
    let asked = if asked.is_undefined() { None } else { Some(asked.to_number(context)? as u64) };
    let request = new_request(context, JsValue::null(), None, "IDBOpenDBRequest")?;
    for handler in ["onupgradeneeded", "onblocked"] {
        request.set(JsString::from(handler), JsValue::null(), false, context)?;
    }
    let current = idb(context).idb.borrow_mut().database(&name).map(|db| db.version);
    let version = asked.unwrap_or(current.unwrap_or(1)).max(1);
    if current.is_some_and(|c| version < c) {
        answer(context, &request, None, Err(("VersionError".into(), format!("The requested version ({version}) is less than the existing version ({}).", current.unwrap_or(0)))))?;
        return Ok(request.into());
    }
    let upgrade = current.is_none_or(|c| version > c);
    let old = current.unwrap_or(0);
    let db_object = database_object(context, &name)?;
    request.set(js_string!("result"), db_object.clone(), false, context)?;
    if !upgrade {
        answer(context, &request, None, Ok(db_object.into()))?;
        return Ok(request.into());
    }
    // A version change: the database made or brought up, inside an upgrade transaction.
    let tx = {
        let state = idb(context);
        let mut state = state.idb.borrow_mut();
        let before = state.database(&name).cloned();
        let db = state.databases.entry(name.clone()).or_default();
        db.version = version;
        let id = state.id();
        (id, before)
    };
    let (tx_id, before) = tx;
    let tx_object = transaction_object(context, tx_id, &db_object, "versionchange")?;
    idb(context).idb.borrow_mut().transactions.insert(
        tx_id,
        Transaction {
            db: name.clone(),
            mode: Mode::VersionChange,
            stores: Vec::new(),
            object: tx_object.clone(),
            pending: 1,
            finished: false,
            before: Some(before.unwrap_or_default()),
            undo: Vec::new(),
            open_request: Some(request.clone()),
            checking: false,
        },
    );
    request.set(js_string!("transaction"), tx_object, false, context)?;
    task(
        context,
        NativeFunction::from_copy_closure_with_captures(
            |_, _, (request, tx, old, new): &(JsObject, f64, f64, f64), cx| {
                let tx = *tx as u32;
                request.set(js_string!("readyState"), js_string!("done"), false, cx)?;
                fire(request, "upgradeneeded", &[("oldVersion", (*old).into()), ("newVersion", (*new).into())], cx);
                if let Some(t) = idb(cx).idb.borrow_mut().transactions.get_mut(&tx) {
                    t.pending = t.pending.saturating_sub(1);
                }
                check_done(cx, tx)
                    .map(|_| JsValue::undefined())
            },
            (request.clone(), f64::from(tx_id), old as f64, version as f64),
        ),
    )?;
    Ok(request.into())
}

fn delete_database(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = text_of(&arg(args, 0), context)?;
    let request = new_request(context, JsValue::null(), None, "IDBOpenDBRequest")?;
    {
        let state = idb(context);
        let mut state = state.idb.borrow_mut();
        state.databases.remove(&name);
        state.dirty.remove(&name);
        if let Some(file) = state.file(&name) {
            // Through the writer when there is one, so a write of it it has yet to do does not
            // put the file back.
            let queued = state
                .writer
                .as_ref()
                .is_some_and(|writer| writer.to.send(Job::Remove(file.clone())).is_ok());
            if !queued {
                let _ = std::fs::remove_file(file);
            }
        }
    }
    answer(context, &request, None, Ok(JsValue::undefined()))?;
    Ok(request.into())
}

fn database_object(context: &mut Context, name: &str) -> JsResult<JsObject> {
    let db = with_proto(context, "IDBDatabase");
    db.set(js_string!("name"), js_text(name), false, context)?;
    hidden(&db, "__db", js_text(name), context)?;
    for handler in ["onversionchange", "onclose", "onerror", "onabort"] {
        db.set(JsString::from(handler), JsValue::null(), false, context)?;
    }
    Ok(db)
}

fn string_list(context: &mut Context, names: Vec<String>) -> JsValue {
    let items: Vec<JsValue> = names.iter().map(|n| js_text(n)).collect();
    let list = JsArray::from_iter(items, context);
    let contains = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|this, args, cx| {
            let wanted = text_of(&arg(args, 0), cx)?;
            Ok(list_of(this, cx)?.iter().any(|v| v.as_string().is_some_and(|s| s.to_std_string_escaped() == wanted)).into())
        }),
    )
    .build();
    let item = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|this, args, cx| {
            let at = arg(args, 0).to_number(cx)? as usize;
            Ok(list_of(this, cx)?.get(at).cloned().unwrap_or(JsValue::null()))
        }),
    )
    .build();
    let _ = list.set(js_string!("contains"), contains, false, context);
    let _ = list.set(js_string!("item"), item, false, context);
    list.into()
}

fn db_name(this: &JsValue, context: &mut Context) -> JsResult<String> {
    hidden_text(this, "__db", context).ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation: not an IDBDatabase").into())
}

const DATABASE_GETTERS: &[(&str, NativeFunctionPointer)] = &[
    ("version", |this, _, cx| {
        let name = db_name(this, cx)?;
        Ok((idb(cx).idb.borrow_mut().database(&name).map_or(0, |d| d.version) as f64).into())
    }),
    ("objectStoreNames", |this, _, cx| {
        let name = db_name(this, cx)?;
        let names: Vec<String> = idb(cx).idb.borrow_mut().database(&name).map(|d| d.stores.keys().cloned().collect()).unwrap_or_default();
        Ok(string_list(cx, names))
    }),
];

/// The version change transaction open on database `name`, if one is.
fn upgrading(context: &Context, name: &str) -> Option<u32> {
    idb(context)
        .idb
        .borrow()
        .transactions
        .iter()
        .find(|(_, t)| t.db == name && t.mode == Mode::VersionChange && !t.finished)
        .map(|(id, _)| *id)
}

const DATABASE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("createObjectStore", 2, |this, args, cx| {
        let name = db_name(this, cx)?;
        let tx = upgrading(cx, &name).ok_or_else(|| dom_error("InvalidStateError", "createObjectStore is only allowed in a version change."))?;
        let store = text_of(&arg(args, 0), cx)?;
        let options = arg(args, 1);
        let (key_path, auto) = match options.as_object() {
            Some(o) => (key_path_of(&o.get(js_string!("keyPath"), cx)?, cx)?, o.get(js_string!("autoIncrement"), cx)?.to_boolean()),
            None => (None, false),
        };
        {
            let state = idb(cx);
            let mut state = state.idb.borrow_mut();
            let db = state.database(&name).ok_or_else(|| dom_error("InvalidStateError", "The database is not open."))?;
            if db.stores.contains_key(&store) {
                return Err(dom_error("ConstraintError", "An object store with that name already exists."));
            }
            db.stores.insert(store.clone(), Store { key_path, auto_increment: auto, next_key: 1., ..Store::default() });
        }
        Ok(store_object(cx, tx, &store, None)?.into())
    }),
    ("deleteObjectStore", 1, |this, args, cx| {
        let name = db_name(this, cx)?;
        upgrading(cx, &name).ok_or_else(|| dom_error("InvalidStateError", "deleteObjectStore is only allowed in a version change."))?;
        let store = text_of(&arg(args, 0), cx)?;
        if let Some(db) = idb(cx).idb.borrow_mut().database(&name) {
            db.stores.remove(&store);
        }
        Ok(JsValue::undefined())
    }),
    ("transaction", 2, |this, args, cx| {
        let name = db_name(this, cx)?;
        let names = arg(args, 0);
        let stores: Vec<String> = if names.is_string() {
            vec![text_of(&names, cx)?]
        } else {
            list_of(&names, cx)?.iter().map(|n| text_of(n, cx)).collect::<JsResult<_>>()?
        };
        let mode = match arg(args, 1) {
            m if m.is_undefined() => Mode::ReadOnly,
            m => match text_of(&m, cx)?.as_str() {
                "readwrite" => Mode::ReadWrite,
                _ => Mode::ReadOnly,
            },
        };
        {
            let state = idb(cx);
            let mut state = state.idb.borrow_mut();
            let db = state.database(&name).ok_or_else(|| dom_error("InvalidStateError", "The database is not open."))?;
            if let Some(missing) = stores.iter().find(|s| !db.stores.contains_key(*s)) {
                return Err(dom_error("NotFoundError", &format!("No object store named {missing}.")));
            }
        }
        let id = idb(cx).idb.borrow_mut().id();
        let db_object = this.as_object().expect("a database");
        let object = transaction_object(cx, id, &db_object, if mode == Mode::ReadWrite { "readwrite" } else { "readonly" })?;
        idb(cx).idb.borrow_mut().transactions.insert(
            id,
            Transaction { db: name, mode, stores, object: object.clone(), pending: 0, finished: false, before: None, undo: Vec::new(), open_request: None, checking: false },
        );
        // One that is never asked anything commits on its own.
        check_done(cx, id)?;
        Ok(object.into())
    }),
    ("close", 0, |_, _, _| Ok(JsValue::undefined())),
];

fn transaction_object(context: &mut Context, id: u32, db: &JsObject, mode: &str) -> JsResult<JsObject> {
    let tx = with_proto(context, "IDBTransaction");
    hidden(&tx, "__tx", id, context)?;
    tx.set(js_string!("db"), db.clone(), false, context)?;
    tx.set(js_string!("mode"), js_text(mode), false, context)?;
    tx.set(js_string!("error"), JsValue::null(), false, context)?;
    tx.set(js_string!("durability"), js_string!("default"), false, context)?;
    for handler in ["oncomplete", "onerror", "onabort"] {
        tx.set(JsString::from(handler), JsValue::null(), false, context)?;
    }
    Ok(tx)
}

fn tx_of(this: &JsValue, context: &mut Context) -> JsResult<u32> {
    hidden_number(this, "__tx", context).ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation").into())
}

/// The transaction, its database, and whether it may still be asked things.
fn tx_info(context: &Context, tx: u32) -> JsResult<(String, Mode, Vec<String>)> {
    let state = idb(context);
    let state = state.idb.borrow();
    let t = state
        .transactions
        .get(&tx)
        .filter(|t| !t.finished)
        .ok_or_else(|| dom_error("TransactionInactiveError", "The transaction has finished."))?;
    Ok((t.db.clone(), t.mode, t.stores.clone()))
}

const TRANSACTION_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("objectStore", 1, |this, args, cx| {
        let tx = tx_of(this, cx)?;
        let name = text_of(&arg(args, 0), cx)?;
        let (db, mode, stores) = tx_info(cx, tx)?;
        let known = idb(cx).idb.borrow_mut().database(&db).is_some_and(|d| d.stores.contains_key(&name));
        if !known || (mode != Mode::VersionChange && !stores.contains(&name)) {
            return Err(dom_error("NotFoundError", &format!("No object store named {name} in this transaction.")));
        }
        Ok(store_object(cx, tx, &name, None)?.into())
    }),
    ("abort", 0, |this, _, cx| {
        let tx = tx_of(this, cx)?;
        abort(cx, tx)?;
        Ok(JsValue::undefined())
    }),
    ("commit", 0, |this, _, cx| {
        let tx = tx_of(this, cx)?;
        check_done(cx, tx)?;
        Ok(JsValue::undefined())
    }),
];

const TRANSACTION_GETTERS: &[(&str, NativeFunctionPointer)] = &[("objectStoreNames", |this, _, cx| {
    let tx = tx_of(this, cx)?;
    let names = idb(cx).idb.borrow().transactions.get(&tx).map(|t| t.stores.clone()).unwrap_or_default();
    Ok(string_list(cx, names))
})];

// ─── Object stores and indexes ──────────────────────────────────────────────────────────────

fn store_object(context: &mut Context, tx: u32, store: &str, index: Option<&str>) -> JsResult<JsObject> {
    let object = with_proto(context, if index.is_some() { "IDBIndex" } else { "IDBObjectStore" });
    hidden(&object, "__tx", tx, context)?;
    hidden(&object, "__store", js_text(store), context)?;
    let tx_object = idb(context).idb.borrow().transactions.get(&tx).map(|t| t.object.clone());
    let (db, _, _) = tx_info(context, tx)?;
    let (key_path, auto, index_info) = {
        let state = idb(context);
        let mut state = state.idb.borrow_mut();
        let s = state.database(&db).and_then(|d| d.stores.get(store).cloned()).unwrap_or_default();
        let info = index.and_then(|i| s.indexes.get(i).cloned());
        (s.key_path.clone(), s.auto_increment, info)
    };
    match (index, index_info) {
        (Some(name), Some(info)) => {
            hidden(&object, "__index", js_text(name), context)?;
            object.set(js_string!("name"), js_text(name), false, context)?;
            let path = key_path_value(&Some(info.key_path.clone()), context)?;
            object.set(js_string!("keyPath"), path, false, context)?;
            object.set(js_string!("unique"), info.unique, false, context)?;
            object.set(js_string!("multiEntry"), info.multi_entry, false, context)?;
            let parent = store_object(context, tx, store, None)?;
            object.set(js_string!("objectStore"), parent, false, context)?;
        }
        _ => {
            object.set(js_string!("name"), js_text(store), false, context)?;
            let path = key_path_value(&key_path, context)?;
            object.set(js_string!("keyPath"), path, false, context)?;
            object.set(js_string!("autoIncrement"), auto, false, context)?;
            if let Some(tx_object) = tx_object {
                object.set(js_string!("transaction"), tx_object, false, context)?;
            }
        }
    }
    Ok(object)
}

/// The store `this` is (or an index of), its transaction, its database — checked usable.
fn target_of(this: &JsValue, context: &mut Context, writes: bool) -> JsResult<(u32, String, String, Option<String>)> {
    let tx = tx_of(this, context)?;
    let store = hidden_text(this, "__store", context).ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation"))?;
    let index = hidden_text(this, "__index", context);
    let (db, mode, _) = tx_info(context, tx)?;
    if writes && mode == Mode::ReadOnly {
        return Err(dom_error("ReadOnlyError", "The transaction is read-only."));
    }
    Ok((tx, db, store, index))
}

/// Keeps what a write replaced on its transaction, to put back if it aborts.
fn remember(context: &Context, tx: u32, undo: impl IntoIterator<Item = Undo>) {
    let state = idb(context);
    let mut state = state.idb.borrow_mut();
    if let Some(t) = state.transactions.get_mut(&tx)
        && t.before.is_none()
    {
        t.undo.extend(undo);
    }
}

fn with_store<R>(context: &Context, db: &str, store: &str, f: impl FnOnce(&mut Store) -> R) -> JsResult<R> {
    let state = idb(context);
    let mut state = state.idb.borrow_mut();
    let store = state
        .database(db)
        .and_then(|d| d.stores.get_mut(store))
        .ok_or_else(|| dom_error("NotFoundError", "The object store is gone."))?;
    Ok(f(store))
}

fn put(this: &JsValue, args: &[JsValue], context: &mut Context, no_overwrite: bool) -> JsResult<JsValue> {
    let (tx, db, store, _) = target_of(this, context, true)?;
    let mut value = store_value(&arg(args, 0), context, 0)?;
    let given = arg(args, 1);
    let given = if given.is_undefined() { None } else { Some(require_key(&given, context)?) };
    let outcome = with_store(context, &db, &store, |s| -> Result<(Key, Option<Stored>, f64), (String, String)> {
        let next_key = s.next_key;
        let key = match (&s.key_path, given) {
            (Some(_), Some(_)) => return Err(("DataError".into(), "A key was given to a store that takes its keys from its values.".into())),
            (Some(path), None) => match extract_key(&value, path) {
                Some(key) => key,
                None if s.auto_increment => {
                    let key = Key::Number(s.next_key);
                    if let KeyPath::One(p) = path {
                        inject_key(&mut value, p, &key);
                    }
                    key
                }
                None => return Err(("DataError".into(), "The value has no key at the store's key path.".into())),
            },
            (None, Some(key)) => key,
            (None, None) if s.auto_increment => Key::Number(s.next_key),
            (None, None) => return Err(("DataError".into(), "The store needs a key for this value.".into())),
        };
        if let Key::Number(n) = key
            && s.auto_increment
            && n >= s.next_key
        {
            s.next_key = n.floor() + 1.;
        }
        if no_overwrite && s.records.contains_key(&key) {
            return Err(("ConstraintError".into(), "A record with that key already exists.".into()));
        }
        if let Some(index) = s.breaks_unique(&key, &value) {
            return Err(("ConstraintError".into(), format!("The value breaks the unique index {index}.")));
        }
        let was = s.records.insert(key.clone(), value);
        Ok((key, was, next_key))
    })?;
    if let Ok((key, was, next_key)) = &outcome {
        remember(
            context,
            tx,
            [
                Undo::NextKey { store: store.clone(), was: *next_key },
                Undo::Record { store: store.clone(), key: key.clone(), was: was.clone() },
            ],
        );
    }
    let request = new_request(context, this.clone(), Some(tx), "IDBRequest")?;
    let outcome = match outcome {
        Ok((key, ..)) => Ok(key_value(&key, context)?),
        Err(error) => Err(error),
    };
    answer(context, &request, Some(tx), outcome)?;
    Ok(request.into())
}

/// The entries a read walks: (key, primary key) in order, of the store or the index.
fn entries_for(context: &Context, db: &str, store: &str, index: Option<&str>, range: &Range) -> JsResult<Vec<(Key, Key)>> {
    with_store(context, db, store, |s| match index.and_then(|i| s.indexes.get(i)) {
        Some(index) => s.index_entries(index).into_iter().filter(|(k, _)| range.contains(k)).collect(),
        None => s.records.keys().filter(|k| range.contains(k)).map(|k| (k.clone(), k.clone())).collect(),
    })
}

fn record(context: &Context, db: &str, store: &str, primary: &Key) -> JsResult<Option<Stored>> {
    with_store(context, db, store, |s| s.records.get(primary).cloned())
}

/// One read: the request answered with what `read` made of the entries in range.
fn read(this: &JsValue, args: &[JsValue], context: &mut Context, read: fn(&mut Context, &str, &str, Vec<(Key, Key)>, Option<usize>) -> JsResult<JsValue>) -> JsResult<JsValue> {
    let (tx, db, store, index) = target_of(this, context, false)?;
    let range = range_of(&arg(args, 0), context)?;
    let count = match arg(args, 1) {
        c if c.is_undefined() => None,
        c => Some(c.to_number(context)? as usize).filter(|c| *c > 0),
    };
    let entries = entries_for(context, &db, &store, index.as_deref(), &range)?;
    let result = read(context, &db, &store, entries, count)?;
    let request = new_request(context, this.clone(), Some(tx), "IDBRequest")?;
    answer(context, &request, Some(tx), Ok(result))?;
    Ok(request.into())
}

const STORE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("put", 2, |this, args, cx| put(this, args, cx, false)),
    ("add", 2, |this, args, cx| put(this, args, cx, true)),
    ("get", 1, |this, args, cx| {
        read(this, args, cx, |cx, db, store, entries, _| match entries.first() {
            Some((_, primary)) => match record(cx, db, store, primary)? {
                Some(value) => restore_value(&value, cx),
                None => Ok(JsValue::undefined()),
            },
            None => Ok(JsValue::undefined()),
        })
    }),
    ("getKey", 1, |this, args, cx| {
        read(this, args, cx, |cx, _, _, entries, _| match entries.first() {
            Some((_, primary)) => key_value(primary, cx),
            None => Ok(JsValue::undefined()),
        })
    }),
    ("getAll", 2, |this, args, cx| {
        read(this, args, cx, |cx, db, store, entries, count| {
            let mut out = Vec::new();
            for (_, primary) in entries.iter().take(count.unwrap_or(usize::MAX)) {
                if let Some(value) = record(cx, db, store, primary)? {
                    out.push(restore_value(&value, cx)?);
                }
            }
            Ok(JsArray::from_iter(out, cx).into())
        })
    }),
    ("getAllKeys", 2, |this, args, cx| {
        read(this, args, cx, |cx, _, _, entries, count| {
            let mut out = Vec::new();
            for (_, primary) in entries.iter().take(count.unwrap_or(usize::MAX)) {
                out.push(key_value(primary, cx)?);
            }
            Ok(JsArray::from_iter(out, cx).into())
        })
    }),
    ("count", 1, |this, args, cx| read(this, args, cx, |_, _, _, entries, _| Ok((entries.len() as f64).into()))),
    ("delete", 1, |this, args, cx| {
        let (tx, db, store, _) = target_of(this, cx, true)?;
        let range = range_of(&arg(args, 0), cx)?;
        let gone: Vec<(Key, Stored)> = with_store(cx, &db, &store, |s| {
            let keys: Vec<Key> = s.records.keys().filter(|k| range.contains(k)).cloned().collect();
            keys.into_iter().filter_map(|k| s.records.remove(&k).map(|v| (k, v))).collect()
        })?;
        remember(cx, tx, gone.into_iter().map(|(key, was)| Undo::Record { store: store.clone(), key, was: Some(was) }));
        let request = new_request(cx, this.clone(), Some(tx), "IDBRequest")?;
        answer(cx, &request, Some(tx), Ok(JsValue::undefined()))?;
        Ok(request.into())
    }),
    ("clear", 0, |this, _, cx| {
        let (tx, db, store, _) = target_of(this, cx, true)?;
        let was = with_store(cx, &db, &store, |s| std::mem::take(&mut s.records))?;
        remember(cx, tx, [Undo::Records { store: store.clone(), was }]);
        let request = new_request(cx, this.clone(), Some(tx), "IDBRequest")?;
        answer(cx, &request, Some(tx), Ok(JsValue::undefined()))?;
        Ok(request.into())
    }),
    ("openCursor", 2, |this, args, cx| open_cursor(this, args, cx, false)),
    ("openKeyCursor", 2, |this, args, cx| open_cursor(this, args, cx, true)),
    ("createIndex", 3, |this, args, cx| {
        let (tx, db, store, _) = target_of(this, cx, true)?;
        let (_, mode, _) = tx_info(cx, tx)?;
        if mode != Mode::VersionChange {
            return Err(dom_error("InvalidStateError", "createIndex is only allowed in a version change."));
        }
        let name = text_of(&arg(args, 0), cx)?;
        let key_path = key_path_of(&arg(args, 1), cx)?.ok_or_else(|| data_error("An index needs a key path."))?;
        let options = arg(args, 2);
        let flag = |key: &str, cx: &mut Context| -> JsResult<bool> {
            Ok(options.as_object().map(|o| o.get(JsString::from(key), cx)).transpose()?.is_some_and(|v| v.to_boolean()))
        };
        let (unique, multi_entry) = (flag("unique", cx)?, flag("multiEntry", cx)?);
        with_store(cx, &db, &store, |s| s.indexes.insert(name.clone(), Index { key_path, unique, multi_entry }))?;
        Ok(store_object(cx, tx, &store, Some(&name))?.into())
    }),
    ("deleteIndex", 1, |this, args, cx| {
        let (_, db, store, _) = target_of(this, cx, true)?;
        let name = text_of(&arg(args, 0), cx)?;
        with_store(cx, &db, &store, |s| s.indexes.remove(&name))?;
        Ok(JsValue::undefined())
    }),
    ("index", 1, |this, args, cx| {
        let (tx, db, store, _) = target_of(this, cx, false)?;
        let name = text_of(&arg(args, 0), cx)?;
        let known = with_store(cx, &db, &store, |s| s.indexes.contains_key(&name))?;
        if !known {
            return Err(dom_error("NotFoundError", &format!("No index named {name}.")));
        }
        Ok(store_object(cx, tx, &store, Some(&name))?.into())
    }),
];

const STORE_GETTERS: &[(&str, NativeFunctionPointer)] = &[("indexNames", |this, _, cx| {
    let (_, db, store, _) = target_of(this, cx, false)?;
    let names = with_store(cx, &db, &store, |s| s.indexes.keys().cloned().collect())?;
    Ok(string_list(cx, names))
})];

// ─── Cursors ────────────────────────────────────────────────────────────────────────────────

fn open_cursor(this: &JsValue, args: &[JsValue], context: &mut Context, keys_only: bool) -> JsResult<JsValue> {
    let (tx, db, store, index) = target_of(this, context, false)?;
    let range = range_of(&arg(args, 0), context)?;
    let direction = match arg(args, 1) {
        d if d.is_undefined() => "next".to_owned(),
        d => text_of(&d, context)?,
    };
    let mut entries = entries_for(context, &db, &store, index.as_deref(), &range)?;
    if direction.starts_with("prev") {
        entries.reverse();
    }
    if direction.ends_with("unique") {
        entries.dedup_by(|a, b| a.0 == b.0);
    }
    let request = new_request(context, this.clone(), Some(tx), "IDBRequest")?;
    let cursor_id = idb(context).idb.borrow_mut().id();
    let cursor = with_proto(context, if keys_only { "IDBCursor" } else { "IDBCursorWithValue" });
    hidden(&cursor, "__cursor", cursor_id, context)?;
    cursor.set(js_string!("direction"), js_text(&direction), false, context)?;
    cursor.set(js_string!("source"), this.clone(), false, context)?;
    cursor.set(js_string!("request"), request.clone(), false, context)?;
    idb(context).idb.borrow_mut().cursors.insert(
        cursor_id,
        Cursor { tx, store, request: request.clone(), object: cursor, entries, at: 0, keys_only },
    );
    step_cursor(context, cursor_id, 0)?;
    Ok(request.into())
}

/// Moves the cursor on by `by` and answers its request: with the cursor at its new place,
/// or `null` past the end.
fn step_cursor(context: &mut Context, id: u32, by: usize) -> JsResult<()> {
    let found = {
        let state = idb(context);
        let mut state = state.idb.borrow_mut();
        let Some(c) = state.cursors.get_mut(&id) else { return Ok(()) };
        c.at += by;
        (c.tx, c.request.clone(), c.object.clone(), c.entries.get(c.at).cloned(), c.store.clone(), c.keys_only)
    };
    let (tx, request, cursor, entry, store, keys_only) = found;
    let result = match entry {
        Some((key, primary)) => {
            let key_js = key_value(&key, context)?;
            let primary_js = key_value(&primary, context)?;
            cursor.set(js_string!("key"), key_js, false, context)?;
            cursor.set(js_string!("primaryKey"), primary_js, false, context)?;
            if !keys_only {
                let db = tx_info(context, tx)?.0;
                let value = match record(context, &db, &store, &primary)? {
                    Some(value) => restore_value(&value, context)?,
                    None => JsValue::undefined(),
                };
                cursor.set(js_string!("value"), value, false, context)?;
            }
            JsValue::from(cursor)
        }
        None => {
            idb(context).idb.borrow_mut().cursors.remove(&id);
            JsValue::null()
        }
    };
    // The request is answered again for each step.
    if by > 0 && let Some(t) = idb(context).idb.borrow_mut().transactions.get_mut(&tx) {
        t.pending += 1;
    }
    request.set(js_string!("readyState"), js_string!("pending"), false, context)?;
    answer(context, &request, Some(tx), Ok(result))
}

fn cursor_of(this: &JsValue, context: &mut Context) -> JsResult<u32> {
    hidden_number(this, "__cursor", context).ok_or_else(|| JsNativeError::typ().with_message("Illegal invocation: not a cursor").into())
}

const CURSOR_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("continue", 1, |this, args, cx| {
        let id = cursor_of(this, cx)?;
        let target = arg(args, 0);
        if target.is_undefined() {
            step_cursor(cx, id, 1)?;
            return Ok(JsValue::undefined());
        }
        // On to the first entry at or past the key asked for, in the cursor's direction.
        let wanted = require_key(&target, cx)?;
        let by = {
            let state = idb(cx);
            let state = state.idb.borrow();
            let Some(c) = state.cursors.get(&id) else { return Ok(JsValue::undefined()) };
            let backwards = c.entries.len() > 1 && c.entries[0].0 > c.entries[c.entries.len() - 1].0;
            c.entries[c.at + 1..]
                .iter()
                .position(|(k, _)| if backwards { *k <= wanted } else { *k >= wanted })
                .map_or(c.entries.len() - c.at, |p| p + 1)
        };
        step_cursor(cx, id, by)?;
        Ok(JsValue::undefined())
    }),
    ("advance", 1, |this, args, cx| {
        let id = cursor_of(this, cx)?;
        let by = arg(args, 0).to_number(cx)? as usize;
        step_cursor(cx, id, by.max(1))?;
        Ok(JsValue::undefined())
    }),
    ("continuePrimaryKey", 2, |this, _, cx| {
        let id = cursor_of(this, cx)?;
        step_cursor(cx, id, 1)?;
        Ok(JsValue::undefined())
    }),
    ("update", 1, |this, args, cx| {
        let id = cursor_of(this, cx)?;
        let (tx, store, primary) = {
            let state = idb(cx);
            let state = state.idb.borrow();
            let c = state.cursors.get(&id).ok_or_else(|| dom_error("InvalidStateError", "The cursor is done."))?;
            (c.tx, c.store.clone(), c.entries.get(c.at).map(|e| e.1.clone()))
        };
        let primary = primary.ok_or_else(|| dom_error("InvalidStateError", "The cursor is done."))?;
        let (db, mode, _) = tx_info(cx, tx)?;
        if mode == Mode::ReadOnly {
            return Err(dom_error("ReadOnlyError", "The transaction is read-only."));
        }
        let value = store_value(&arg(args, 0), cx, 0)?;
        let was = with_store(cx, &db, &store, |s| s.records.insert(primary.clone(), value))?;
        remember(cx, tx, [Undo::Record { store: store.clone(), key: primary.clone(), was }]);
        let source = this.as_object().map_or(JsValue::null(), JsValue::from);
        let request = new_request(cx, source, Some(tx), "IDBRequest")?;
        let key = key_value(&primary, cx)?;
        answer(cx, &request, Some(tx), Ok(key))?;
        Ok(request.into())
    }),
    ("delete", 0, |this, _, cx| {
        let id = cursor_of(this, cx)?;
        let (tx, store, primary) = {
            let state = idb(cx);
            let state = state.idb.borrow();
            let c = state.cursors.get(&id).ok_or_else(|| dom_error("InvalidStateError", "The cursor is done."))?;
            (c.tx, c.store.clone(), c.entries.get(c.at).map(|e| e.1.clone()))
        };
        let primary = primary.ok_or_else(|| dom_error("InvalidStateError", "The cursor is done."))?;
        let (db, mode, _) = tx_info(cx, tx)?;
        if mode == Mode::ReadOnly {
            return Err(dom_error("ReadOnlyError", "The transaction is read-only."));
        }
        let was = with_store(cx, &db, &store, |s| s.records.remove(&primary))?;
        remember(cx, tx, [Undo::Record { store: store.clone(), key: primary.clone(), was }]);
        let source = this.as_object().map_or(JsValue::null(), JsValue::from);
        let request = new_request(cx, source, Some(tx), "IDBRequest")?;
        answer(cx, &request, Some(tx), Ok(JsValue::undefined()))?;
        Ok(request.into())
    }),
];

// ─── IDBKeyRange ────────────────────────────────────────────────────────────────────────────

fn make_range(context: &mut Context, lower: JsValue, upper: JsValue, lower_open: bool, upper_open: bool) -> JsResult<JsValue> {
    for bound in [&lower, &upper] {
        if !bound.is_undefined() {
            require_key(bound, context)?;
        }
    }
    let range = with_proto(context, "IDBKeyRange");
    hidden(&range, "__range", true, context)?;
    range.set(js_string!("lower"), lower, false, context)?;
    range.set(js_string!("upper"), upper, false, context)?;
    range.set(js_string!("lowerOpen"), lower_open, false, context)?;
    range.set(js_string!("upperOpen"), upper_open, false, context)?;
    Ok(range.into())
}

const RANGE_STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("only", 1, |_, args, cx| make_range(cx, arg(args, 0), arg(args, 0), false, false)),
    ("lowerBound", 2, |_, args, cx| make_range(cx, arg(args, 0), JsValue::undefined(), arg(args, 1).to_boolean(), false)),
    ("upperBound", 2, |_, args, cx| make_range(cx, JsValue::undefined(), arg(args, 0), false, arg(args, 1).to_boolean())),
    ("bound", 4, |_, args, cx| {
        let (lower, upper) = (require_key(&arg(args, 0), cx)?, require_key(&arg(args, 1), cx)?);
        if lower > upper {
            return Err(data_error("The lower bound is greater than the upper bound."));
        }
        make_range(cx, arg(args, 0), arg(args, 1), arg(args, 2).to_boolean(), arg(args, 3).to_boolean())
    }),
];

const RANGE_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[("includes", 1, |this, args, cx| {
    let range = range_of(this, cx)?;
    let key = require_key(&arg(args, 0), cx)?;
    Ok(range.contains(&key).into())
})];

// ─── Installing it ──────────────────────────────────────────────────────────────────────────

fn function(context: &mut Context, name: &str, length: usize, f: NativeFunctionPointer) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

fn class(
    context: &mut Context,
    name: &str,
    parent: Option<&JsObject>,
    methods: &[(&str, usize, NativeFunctionPointer)],
    getters: &[(&str, NativeFunctionPointer)],
) -> JsResult<JsObject> {
    let constructor: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(|_, _, _| Err(JsNativeError::typ().with_message("Illegal constructor").into())),
    )
    .name(JsString::from(name))
    .constructor(true)
    .build()
    .into();
    let prototype = object(context);
    if let Some(parent) = parent {
        prototype.set_prototype(Some(parent.clone()));
    }
    for (method, length, f) in methods {
        let f = function(context, method, *length, *f);
        prototype.define_property_or_throw(
            JsString::from(*method),
            PropertyDescriptor::builder().value(f).writable(true).enumerable(false).configurable(true),
            context,
        )?;
    }
    for (getter, f) in getters {
        let f = function(context, &format!("get {getter}"), 0, *f);
        prototype.define_property_or_throw(
            JsString::from(*getter),
            PropertyDescriptor::builder().get(f).enumerable(true).configurable(true),
            context,
        )?;
    }
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder().value(prototype.clone()).writable(false).enumerable(false).configurable(false),
        context,
    )?;
    context.register_global_property(JsString::from(name), constructor.clone(), Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    Ok(prototype)
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let target = class(context, "IDBEventTarget", None, TARGET_METHODS, &[])?;
    let request = class(context, "IDBRequest", Some(&target), &[], &[])?;
    class(context, "IDBOpenDBRequest", Some(&request), &[], &[])?;
    class(context, "IDBDatabase", Some(&target), DATABASE_METHODS, DATABASE_GETTERS)?;
    class(context, "IDBTransaction", Some(&target), TRANSACTION_METHODS, TRANSACTION_GETTERS)?;
    let store = class(context, "IDBObjectStore", None, STORE_METHODS, STORE_GETTERS)?;
    class(context, "IDBIndex", Some(&store), &[], &[])?;
    let cursor = class(context, "IDBCursor", None, CURSOR_METHODS, &[])?;
    class(context, "IDBCursorWithValue", Some(&cursor), &[], &[])?;
    class(context, "IDBKeyRange", None, RANGE_METHODS, &[])?;
    let range_class = context.global_object().get(js_string!("IDBKeyRange"), context)?;
    if let Some(range_class) = range_class.as_object() {
        for (name, length, f) in RANGE_STATICS {
            let f = function(context, name, *length, *f);
            range_class.set(JsString::from(*name), f, false, context)?;
        }
    }
    let factory = object(context);
    for (name, length, f) in [
        ("open", 2, open as NativeFunctionPointer),
        ("deleteDatabase", 1, delete_database),
        ("cmp", 2, |_, args, cx| {
            let a = require_key(&arg(args, 0), cx)?;
            let b = require_key(&arg(args, 1), cx)?;
            Ok(match a.cmp(&b) {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            }
            .into())
        }),
        ("databases", 0, |_, _, cx| {
            let state = idb(cx);
            let names: Vec<(String, u64)> = {
                let mut state = state.idb.borrow_mut();
                if let Some(dir) = state.dir.clone()
                    && let Ok(entries) = std::fs::read_dir(dir)
                {
                    for entry in entries.flatten() {
                        let file = entry.file_name().to_string_lossy().into_owned();
                        if let Some(name) = file.strip_suffix(".idb.json") {
                            state.database(name);
                        }
                    }
                }
                state.databases.iter().map(|(n, d)| (n.clone(), d.version)).collect()
            };
            let mut out = Vec::new();
            for (name, version) in names {
                let info = object(cx);
                info.set(js_string!("name"), js_text(&name), false, cx)?;
                info.set(js_string!("version"), version as f64, false, cx)?;
                out.push(info.into());
            }
            let list = JsArray::from_iter(out, cx);
            Ok(boa_engine::object::builtins::JsPromise::resolve(list, cx)?.into())
        }),
    ] {
        let f = function(context, name, length, f);
        factory.set(JsString::from(name), f, false, context)?;
    }
    context.register_global_property(js_string!("indexedDB"), factory, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    Ok(())
}

/// Where the databases are kept from now on.
pub(crate) fn set_dir(context: &Context, dir: Option<PathBuf>) {
    let state = idb(context);
    let mut state = state.idb.borrow_mut();
    state.dir = dir;
    state.databases.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_order_as_the_spec_says() {
        let mut keys = vec![
            Key::Array(vec![]),
            Key::String("b".into()),
            Key::Binary(vec![1]),
            Key::Date(5.),
            Key::Number(10.),
            Key::String("a".into()),
            Key::Number(-1.),
        ];
        keys.sort();
        assert_eq!(
            keys,
            [
                Key::Number(-1.),
                Key::Number(10.),
                Key::Date(5.),
                Key::String("a".into()),
                Key::String("b".into()),
                Key::Binary(vec![1]),
                Key::Array(vec![]),
            ]
        );
    }
}
