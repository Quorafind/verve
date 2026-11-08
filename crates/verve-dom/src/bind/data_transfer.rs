//! `DataTransfer`, `DataTransferItemList` and `DataTransferItem`: what a drag, a copy or a paste
//! carries.
//!
//! The data lives in Rust ([`Store`]); the objects are views of it. A transfer is made for each
//! event that has one, in the mode that event allows ([`Mode`]): `dragstart`, `copy` and `cut`
//! may be written; `drop` and `paste` are read only; `dragenter` and `dragover` list what is
//! being dragged (`types`, `items`) but hide its contents and the files, as a browser does until
//! the drop. The rules are the HTML standard's "drag data store"; the values the drag events show
//! were observed on Electron 39.8.10 (see `tests/drag.rs`).

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsSymbol, JsValue, Trace, js_string,
};
use boa_gc::GcRefCell;
use verve::native::{self, arg, arg_text, illegal_constructor, js_text, type_error};

use super::{Accessor, Method, WebIdlClass, prototype_of_target, set, webidl_class};
use crate::state::class_prototype;

/// What a script may do with a transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `setData`, `clearData` and `items.add` work (`dragstart`, `copy`, `cut`).
    Write,
    /// Only reading works (`drop`, `paste`).
    Read,
    /// The kinds and types are listed, the data and the files are hidden (`dragenter`,
    /// `dragover`).
    Protected,
}

/// One thing a transfer holds: a string under a type, or a file.
#[derive(Clone, Trace, Finalize)]
pub enum Entry {
    Text {
        #[unsafe_ignore_trace]
        kind: String,
        #[unsafe_ignore_trace]
        data: String,
    },
    /// A `File`.
    File(JsObject),
}

impl Entry {
    /// A string held under `kind`.
    pub fn text(kind: &str, data: &str) -> Self {
        Self::Text {
            kind: normal(kind),
            data: data.to_owned(),
        }
    }
}

#[derive(Trace, Finalize)]
struct Store {
    entries: Vec<Entry>,
    #[unsafe_ignore_trace]
    mode: Mode,
    #[unsafe_ignore_trace]
    drop_effect: String,
    /// Whether a script has set `dropEffect`: an explicit `none` is not the same as leaving it.
    #[unsafe_ignore_trace]
    drop_effect_set: bool,
    #[unsafe_ignore_trace]
    effect_allowed: String,
    /// Where `setDragImage` said the image's hot spot is: recorded, never drawn.
    #[unsafe_ignore_trace]
    image: Option<(i32, i32)>,
    items: Option<JsObject>,
}

#[derive(Trace, Finalize, JsData)]
struct Transfer(GcRefCell<Store>);

#[derive(Trace, Finalize, JsData)]
struct ItemList(JsObject);

#[derive(Trace, Finalize, JsData)]
struct Item {
    transfer: JsObject,
    entry: Entry,
}

/// The type `kind` is stored under: lower case, with the legacy names mapped.
fn normal(kind: &str) -> String {
    let lower = kind.trim().to_ascii_lowercase();
    match lower.as_str() {
        "text" => "text/plain".to_owned(),
        "url" => "text/uri-list".to_owned(),
        _ => lower,
    }
}

const EFFECTS_ALLOWED: &[&str] = &[
    "none",
    "copy",
    "copyLink",
    "copyMove",
    "link",
    "linkMove",
    "move",
    "all",
    "uninitialized",
];

fn new_store(mode: Mode, entries: Vec<Entry>, effect_allowed: &str) -> Store {
    Store {
        entries,
        mode,
        drop_effect: "none".to_owned(),
        drop_effect_set: false,
        effect_allowed: effect_allowed.to_owned(),
        image: None,
        items: None,
    }
}

/// A transfer holding `entries`, in `mode`, with this `effectAllowed`.
pub fn create(
    context: &mut Context,
    mode: Mode,
    entries: Vec<Entry>,
    effect_allowed: &str,
) -> JsObject {
    let prototype = class_prototype(context, "DataTransfer");
    JsObject::from_proto_and_data(
        prototype,
        Transfer(GcRefCell::new(new_store(mode, entries, effect_allowed))),
    )
}

fn with<R>(this: &JsValue, f: impl FnOnce(&mut Store) -> R) -> JsResult<R> {
    let object = this.as_object();
    match object.as_ref().and_then(|o| o.downcast_ref::<Transfer>()) {
        Some(transfer) => Ok(f(&mut transfer.0.borrow_mut())),
        None => Err(type_error("Illegal invocation: not a DataTransfer")),
    }
}

/// What the transfer holds now.
pub fn entries_of(transfer: &JsObject) -> Vec<Entry> {
    with(&transfer.clone().into(), |s| s.entries.clone()).unwrap_or_default()
}

/// The transfer's `effectAllowed`.
pub fn effect_allowed(transfer: &JsObject) -> String {
    with(&transfer.clone().into(), |s| s.effect_allowed.clone()).unwrap_or_default()
}

/// The `dropEffect` a script gave the transfer; `None` when it left it alone (a handler that
/// cancels `dragover` and sets nothing gets the default effect, one that sets `none` gets no drop).
pub fn drop_effect(transfer: &JsObject) -> Option<String> {
    with(&transfer.clone().into(), |s| {
        s.drop_effect_set.then(|| s.drop_effect.clone())
    })
    .ok()
    .flatten()
}

/// Sets `dropEffect` as the page's handler would have.
pub fn put_drop_effect(transfer: &JsObject, effect: &str) {
    let _ = with(&transfer.clone().into(), |s| {
        s.drop_effect = effect.to_owned();
    });
}

/// Where `setDragImage` put the image's hot spot, if it was called.
pub fn image_hot_spot(transfer: &JsObject) -> Option<(i32, i32)> {
    with(&transfer.clone().into(), |s| s.image).ok().flatten()
}

fn construct(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let prototype = prototype_of_target(new_target, context)?;
    let store = new_store(Mode::Write, Vec::new(), "none");
    Ok(JsObject::from_proto_and_data(prototype, Transfer(GcRefCell::new(store))).into())
}

fn file_type(file: &JsObject, context: &mut Context) -> JsResult<String> {
    let kind = native::get(&file.clone().into(), "type", context)?;
    native::text_of(&kind, context)
}

/// The `DataTransferItem` for `entry`.
fn item_object(context: &mut Context, transfer: &JsObject, entry: Entry) -> JsObject {
    let prototype = class_prototype(context, "DataTransferItem");
    JsObject::from_proto_and_data(
        prototype,
        Item {
            transfer: transfer.clone(),
            entry,
        },
    )
}

/// Brings the `items` list (when a script has asked for it) in line with the entries.
fn rebuild(context: &mut Context, transfer: &JsObject) -> JsResult<()> {
    let (list, entries) = with(&transfer.clone().into(), |s| {
        (s.items.clone(), s.entries.clone())
    })?;
    let Some(list) = list else {
        return Ok(());
    };
    let before = list.get(js_string!("length"), context)?.to_u32(context)? as usize;
    for (at, entry) in entries.iter().enumerate() {
        let item = item_object(context, transfer, entry.clone());
        list.set(at as u32, item, true, context)?;
    }
    for at in entries.len()..before {
        list.delete_property_or_throw(at as u32, context)?;
    }
    set(&list, "length", entries.len() as u32, context)
}

fn writable(this: &JsValue) -> JsResult<bool> {
    with(this, |s| s.mode == Mode::Write)
}

fn set_data(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let kind = normal(&arg_text(args, 0, cx)?);
    let data = arg_text(args, 1, cx)?;
    if writable(this)? {
        with(this, |s| {
            s.entries
                .retain(|e| !matches!(e, Entry::Text { kind: k, .. } if *k == kind));
            s.entries.push(Entry::Text { kind, data });
        })?;
        if let Some(object) = this.as_object() {
            rebuild(cx, &object)?;
        }
    }
    Ok(JsValue::undefined())
}

fn get_data(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let kind = normal(&arg_text(args, 0, cx)?);
    let found = with(this, |s| {
        if s.mode == Mode::Protected {
            return None;
        }
        s.entries.iter().find_map(|e| match e {
            Entry::Text { kind: k, data } if *k == kind => Some(data.clone()),
            _ => None,
        })
    })?;
    let text = found.unwrap_or_default();
    // A list of addresses answers its first, comments skipped.
    let text = if kind == "text/uri-list" {
        text.lines()
            .find(|line| !line.starts_with('#') && !line.is_empty())
            .unwrap_or("")
            .to_owned()
    } else {
        text
    };
    Ok(js_text(&text))
}

fn clear_data(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let only = match arg(args, 0) {
        value if value.is_undefined() => None,
        value => Some(normal(&native::text_of(&value, cx)?)),
    };
    if writable(this)? {
        with(this, |s| {
            s.entries.retain(|e| match (e, &only) {
                (Entry::Text { kind, .. }, Some(only)) => kind != only,
                (Entry::Text { .. }, None) => false,
                (Entry::File(_), _) => true,
            });
        })?;
        if let Some(object) = this.as_object() {
            rebuild(cx, &object)?;
        }
    }
    Ok(JsValue::undefined())
}

fn set_drag_image(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let x = arg(args, 1).to_i32(cx)?;
    let y = arg(args, 2).to_i32(cx)?;
    if writable(this)? {
        with(this, |s| s.image = Some((x, y)))?;
    }
    Ok(JsValue::undefined())
}

const METHODS: &[Method] = &[
    ("setData", 2, set_data),
    ("getData", 1, get_data),
    ("clearData", 0, clear_data),
    ("setDragImage", 3, set_drag_image),
];

fn types(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let (mut kinds, files) = with(this, |s| {
        let mut kinds: Vec<String> = Vec::new();
        for entry in &s.entries {
            if let Entry::Text { kind, .. } = entry
                && !kinds.contains(kind)
            {
                kinds.push(kind.clone());
            }
        }
        (kinds, s.entries.iter().any(|e| matches!(e, Entry::File(_))))
    })?;
    if files {
        kinds.push("Files".to_owned());
    }
    let list = kinds.iter().map(|k| js_text(k)).collect();
    Ok(native::array(cx, list))
}

fn files(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let list = with(this, |s| match s.mode {
        Mode::Protected => Vec::new(),
        _ => s
            .entries
            .iter()
            .filter_map(|e| match e {
                Entry::File(file) => Some(JsValue::from(file.clone())),
                Entry::Text { .. } => None,
            })
            .collect(),
    })?;
    Ok(native::array(cx, list))
}

fn items(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some(transfer) = this.as_object() else {
        return Err(type_error("Illegal invocation"));
    };
    if let Some(list) = with(this, |s| s.items.clone())? {
        return Ok(list.into());
    }
    let prototype = class_prototype(cx, "DataTransferItemList");
    let list = JsObject::from_proto_and_data(prototype, ItemList(transfer.clone()));
    set(&list, "length", 0, cx)?;
    with(this, |s| s.items = Some(list.clone()))?;
    rebuild(cx, &transfer)?;
    Ok(list.into())
}

fn effect_allowed_get(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_text(&with(this, |s| s.effect_allowed.clone())?))
}

fn effect_allowed_set(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let value = arg_text(args, 0, cx)?;
    // Only a handler of `dragstart` (or a constructed transfer) may choose; a value that is no
    // effect is ignored.
    if writable(this)? && EFFECTS_ALLOWED.contains(&value.as_str()) {
        with(this, |s| s.effect_allowed = value)?;
    }
    Ok(JsValue::undefined())
}

fn drop_effect_get(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_text(&with(this, |s| s.drop_effect.clone())?))
}

fn drop_effect_set(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let value = arg_text(args, 0, cx)?;
    if ["none", "copy", "link", "move"].contains(&value.as_str()) {
        with(this, |s| {
            s.drop_effect = value;
            s.drop_effect_set = true;
        })?;
    }
    Ok(JsValue::undefined())
}

const ACCESSORS: &[Accessor] = &[
    ("types", Some(types), None),
    ("files", Some(files), None),
    ("items", Some(items), None),
    (
        "effectAllowed",
        Some(effect_allowed_get),
        Some(effect_allowed_set),
    ),
    ("dropEffect", Some(drop_effect_get), Some(drop_effect_set)),
];

fn list_transfer(this: &JsValue) -> JsResult<JsObject> {
    let object = this.as_object();
    object
        .as_ref()
        .and_then(|o| o.downcast_ref::<ItemList>().map(|l| l.0.clone()))
        .ok_or_else(|| type_error("Illegal invocation: not a DataTransferItemList"))
}

/// `items.add(string, type)` or `items.add(file)`.
fn list_add(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let transfer = list_transfer(this)?;
    if !with(&transfer.clone().into(), |s| s.mode == Mode::Write)? {
        return Ok(JsValue::null());
    }
    let first = arg(args, 0);
    let entry = if verve::blob::is_blob(&first, cx) {
        let file = first.as_object().ok_or_else(|| type_error("not a File"))?;
        Entry::File(file)
    } else {
        let data = native::text_of(&first, cx)?;
        let kind = normal(&arg_text(args, 1, cx)?);
        let taken = with(&transfer.clone().into(), |s| {
            s.entries
                .iter()
                .any(|e| matches!(e, Entry::Text { kind: k, .. } if *k == kind))
        })?;
        if taken {
            return Err(super::dom_error(
                cx,
                "An item already exists for the type.",
                "NotSupportedError",
            ));
        }
        Entry::Text { kind, data }
    };
    with(&transfer.clone().into(), |s| s.entries.push(entry.clone()))?;
    rebuild(cx, &transfer)?;
    Ok(item_object(cx, &transfer, entry).into())
}

fn list_remove(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let transfer = list_transfer(this)?;
    let at = arg(args, 0).to_u32(cx)? as usize;
    let removed = with(&transfer.clone().into(), |s| {
        (s.mode == Mode::Write && at < s.entries.len()).then(|| s.entries.remove(at))
    })?;
    if removed.is_some() {
        rebuild(cx, &transfer)?;
    }
    Ok(JsValue::undefined())
}

fn list_clear(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let transfer = list_transfer(this)?;
    let cleared = with(&transfer.clone().into(), |s| {
        let write = s.mode == Mode::Write;
        if write {
            s.entries.clear();
        }
        write
    })?;
    if cleared {
        rebuild(cx, &transfer)?;
    }
    Ok(JsValue::undefined())
}

const LIST_METHODS: &[Method] = &[
    ("add", 1, list_add),
    ("remove", 1, list_remove),
    ("clear", 0, list_clear),
];

fn item_of(this: &JsValue) -> JsResult<(JsObject, Entry)> {
    let object = this.as_object();
    object
        .as_ref()
        .and_then(|o| {
            o.downcast_ref::<Item>()
                .map(|i| (i.transfer.clone(), i.entry.clone()))
        })
        .ok_or_else(|| type_error("Illegal invocation: not a DataTransferItem"))
}

fn item_kind(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(js_text(match item_of(this)?.1 {
        Entry::Text { .. } => "string",
        Entry::File(_) => "file",
    }))
}

fn item_type(this: &JsValue, _: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    Ok(match &item_of(this)?.1 {
        Entry::Text { kind, .. } => js_text(kind),
        Entry::File(file) => js_text(&file_type(file, cx)?),
    })
}

fn readable(transfer: &JsObject) -> bool {
    with(&transfer.clone().into(), |s| s.mode != Mode::Protected).unwrap_or(false)
}

/// `item.getAsString(callback)`: the callback gets the string in a task of its own.
fn item_as_string(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let (transfer, entry) = item_of(this)?;
    let callback = arg(args, 0);
    if let (Entry::Text { data, .. }, true, true) =
        (&entry, readable(&transfer), callback.is_callable())
    {
        let task = native::closure(
            cx,
            "",
            0,
            |_, _, (callback, data): &(JsValue, String), cx| {
                verve::call(cx, callback, &JsValue::undefined(), &[js_text(data)])
            },
            (callback, data.clone()),
        );
        let timer = native::global(cx, "setTimeout")?;
        verve::call(cx, &timer, &JsValue::undefined(), &[task.into(), 0.into()])?;
    }
    Ok(JsValue::undefined())
}

fn item_as_file(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    let (transfer, entry) = item_of(this)?;
    Ok(match &entry {
        Entry::File(file) if readable(&transfer) => file.clone().into(),
        _ => JsValue::null(),
    })
}

const ITEM_METHODS: &[Method] = &[
    ("getAsString", 1, item_as_string),
    ("getAsFile", 0, item_as_file),
    ("webkitGetAsEntry", 0, |_, _, _| Ok(JsValue::null())),
];

const ITEM_ACCESSORS: &[Accessor] = &[
    ("kind", Some(item_kind), None),
    ("type", Some(item_type), None),
];

/// Installs the three classes.
pub(super) fn install(context: &mut Context) -> JsResult<()> {
    webidl_class(
        context,
        WebIdlClass {
            name: "DataTransfer",
            length: 0,
            construct,
            parent: None,
            methods: METHODS,
            accessors: ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let (_, list) = webidl_class(
        context,
        WebIdlClass {
            name: "DataTransferItemList",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: LIST_METHODS,
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    // The list is array-like: `for…of` walks it as it walks an array.
    let array = native::global(context, "Array")?;
    let proto = native::get(&array, "prototype", context)?;
    let values = native::get(&proto, "values", context)?;
    list.set(JsSymbol::iterator(), values, false, context)?;
    webidl_class(
        context,
        WebIdlClass {
            name: "DataTransferItem",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: ITEM_METHODS,
            accessors: ITEM_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    Ok(())
}
