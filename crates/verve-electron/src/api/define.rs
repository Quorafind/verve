//! What Electron's classes add to `verve::native`: the id Rust knows an object by, the checks a
//! method makes before it acts, and the argument errors of Electron's two dialects.
//!
//! A class is declared as `verve::native` declares any: a [`ClassSpec`] with its constructor's
//! set-up (`init`), its method table, accessors and statics, extending `EventEmitter` through
//! [`emitter`]. Electron's API is a few dozen classes whose methods "check the arguments, send a
//! message, answer a cached value", so what is left for this module is those steps:
//!
//! - [`bind`] and [`live`] map an object to the `WindowId`/`ContentsId` Rust holds it by, per
//!   class ([`Kind`]), in a table of the realm's own: nothing about it is visible to a script,
//!   so it cannot be copied onto another object, read back, or passed through a `Proxy`.
//!   `live` throws Electron's `Object has been destroyed` once what the id stood for is gone.
//! - [`Args`] and [`Fields`] read an argument or an options field. Electron's native methods
//!   (gin) fail as `TypeError: Error processing argument at index 0, conversion failure from
//!   undefined`; the methods Electron writes in JavaScript fail as Node's do
//!   (`ERR_INVALID_ARG_TYPE`); and a constructor's options object is lenient — a field of the
//!   wrong type is left at its default. Each is a way of constructing the reader.
//! - [`emit`] fires an event on an instance.
//!
//! The messages are frozen from Electron 39.8.10 (`tests/api.rs` and `tests/pipeline.rs` name the
//! probe each came from). `class X extends BrowserWindow` works because `native::class` makes it
//! so; a class Electron makes native refuses a call without `new` (`ClassSpec::new_only`).
//!
//! [`ClassSpec`]: verve::native::ClassSpec

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::{
    Context, JsError, JsObject, JsResult, JsString, JsValue, js_string, object::WeakJsObject,
};
use verve::native::{self, arg, invalid_arg_type, invalid_arg_value};

/// The class an Electron class extends to emit events: the realm's `EventEmitter`.
pub use verve::events::emitter_class as emitter;

/// Which class an object stands for an instance of, in the table [`bind`] fills: a window, a
/// `webContents`. A package declares its own as a `const`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kind(pub &'static str);

/// What the realm knows of the objects that stand for something in Rust.
type Table = Rc<RefCell<Vec<(WeakJsObject, Kind, u32)>>>;

fn table(context: &mut Context) -> Table {
    if let Some(table) = context.get_data::<Table>() {
        return Rc::clone(table);
    }
    let made = Table::default();
    context.insert_data(Rc::clone(&made));
    made
}

/// Emits `event` on `object` — an `EventEmitter` — with `args`.
///
/// # Errors
///
/// What a listener threw, or an unheard `error`.
pub fn emit(
    context: &mut Context,
    object: &JsObject,
    event: &str,
    args: &[JsValue],
) -> JsResult<()> {
    verve::events::emit_event(context, &object.clone().into(), event, args)?;
    Ok(())
}

/// An Electron `Event`: an object whose `preventDefault()` sets `defaultPrevented`, which
/// neither shows when the event is listed. Most events of the app and the windows are heard as
/// `(event, ...)`; the IPC events add their own fields to this one.
///
/// # Errors
///
/// A property that could not be defined.
pub fn event(context: &mut Context) -> JsResult<JsObject> {
    let event = native::object(context);
    native::hidden(&event, js_string!("defaultPrevented"), false, context)?;
    let prevent = native::function(context, "preventDefault", 0, |this, _, cx| {
        if let Some(event) = this.as_object() {
            native::hidden(&event, js_string!("defaultPrevented"), true, cx)?;
        }
        Ok(JsValue::undefined())
    });
    native::hidden(&event, js_string!("preventDefault"), prevent, context)?;
    Ok(event)
}

/// Emits `event` on `object` with an Electron `Event` before `args`, and answers whether a
/// listener called `event.preventDefault()`.
///
/// # Errors
///
/// What a listener threw, or an unheard `error`.
pub fn emit_cancelable(
    context: &mut Context,
    object: &JsObject,
    event_name: &str,
    args: &[JsValue],
) -> JsResult<bool> {
    let cancel = event(context)?;
    emit_with_event(context, object, event_name, &cancel, args)
}

/// [`emit_cancelable`] with an `event` the caller made, to give properties of its own: emits
/// `event_name` with `event` before `args` and answers whether a listener prevented it.
///
/// # Errors
///
/// What a listener threw, or an unheard `error`.
pub fn emit_with_event(
    context: &mut Context,
    object: &JsObject,
    event_name: &str,
    cancel: &JsObject,
    args: &[JsValue],
) -> JsResult<bool> {
    let mut all = vec![JsValue::from(cancel.clone())];
    all.extend_from_slice(args);
    emit(context, object, event_name, &all)?;
    Ok(cancel
        .get(js_string!("defaultPrevented"), context)?
        .to_boolean())
}

/// What `result` holds; a throw is reported like any uncaught error of the process that runs this
/// (the main process, or a page), and is `None`.
pub fn report<T>(context: &mut Context, result: JsResult<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            verve::report(context, &error);
            None
        }
    }
}

/// Settles the promise `resolvers` belong to: `Ok` resolves it, `Err` rejects it.
///
/// # Errors
///
/// What a resolving function threw, which a promise's own do not.
pub fn settle(
    context: &mut Context,
    resolvers: &ResolvingFunctions,
    outcome: Result<JsValue, JsValue>,
) -> JsResult<()> {
    let (function, value) = match outcome {
        Ok(value) => (&resolvers.resolve, value),
        Err(reason) => (&resolvers.reject, reason),
    };
    function.call(&JsValue::undefined(), &[value], context)?;
    Ok(())
}

/// Emits `event` on the main process's `process` with `args`; a listener's throw is
/// [`report`]ed.
pub fn emit_process(context: &mut Context, event: &str, args: &[JsValue]) {
    let process = native::global(context, "process").unwrap_or_default();
    let mut all = vec![native::js_text(event)];
    all.extend_from_slice(args);
    let heard = native::invoke(&process, "emit", &all, context);
    report(context, heard);
}

/// [`emit_cancelable`] for an event nobody can answer for, with a listener's throw [`report`]ed
/// and the event taken as not prevented.
pub fn emit_reporting(
    context: &mut Context,
    object: &JsObject,
    event_name: &str,
    args: &[JsValue],
) -> bool {
    let result = emit_cancelable(context, object, event_name, args);
    report(context, result).unwrap_or(false)
}

/// Marks `object` — the one a constructor's `init` was handed — as an instance of `kind`
/// standing for `id`.
pub fn bind(object: &JsObject, kind: Kind, id: u32, context: &mut Context) {
    let table = table(context);
    let mut entries = table.borrow_mut();
    entries.retain(|(weak, _, _)| weak.upgrade().is_some());
    entries.push((object.downgrade(), kind, id));
}

/// What `value` stands for, when it is an object [`bind`] marked.
fn entry_of(value: &JsValue, context: &mut Context) -> Option<(Kind, u32)> {
    let object = value.as_object()?;
    let table = table(context);
    let entries = table.borrow();
    entries
        .iter()
        .find(|(weak, _, _)| {
            weak.upgrade()
                .is_some_and(|o| JsObject::equals(&o, &object))
        })
        .map(|(_, kind, id)| (*kind, *id))
}

/// The id the object `value` stands for, when it is an instance of `kind`.
pub fn id_of(value: &JsValue, kind: Kind, context: &mut Context) -> Option<u32> {
    entry_of(value, context).and_then(|(found, id)| (found == kind).then_some(id))
}

/// What a method of a class starts with: the id `this` stands for and what `lookup` finds for
/// it. As Electron's native methods answer: `TypeError: Illegal invocation` when `this` is an
/// instance of another class, `TypeError: Object has been destroyed` when it is no instance at
/// all, or `lookup` finds nothing — the window, or page, is gone.
///
/// # Errors
///
/// Those two.
pub fn live<T>(
    this: &JsValue,
    kind: Kind,
    context: &mut Context,
    lookup: impl FnOnce(u32) -> Option<T>,
) -> JsResult<(u32, T)> {
    match entry_of(this, context) {
        Some((found, _)) if found != kind => Err(native::type_error("Illegal invocation")),
        Some((_, id)) => lookup(id).map(|found| (id, found)).ok_or_else(destroyed),
        None => Err(destroyed()),
    }
}

/// What a method answers for something Verve does not do: `ERR_NOT_SUPPORTED`, as the
/// conventions say. [`not_supported`] throws it, [`rejected`] is for the methods that answer a
/// promise.
pub fn unsupported(context: &mut Context, api: &str) -> JsError {
    native::coded_error(
        context,
        &format!("{api} is not supported by Verve"),
        "ERR_NOT_SUPPORTED",
    )
}

/// Throws [`unsupported`].
///
/// # Errors
///
/// Always.
pub fn not_supported(context: &mut Context, api: &str) -> JsResult<JsValue> {
    Err(unsupported(context, api))
}

/// A promise rejected with [`unsupported`].
///
/// # Errors
///
/// A promise that could not be made.
pub fn rejected(context: &mut Context, api: &str) -> JsResult<JsValue> {
    let error = unsupported(context, api);
    native::settled(Err(error), context)
}

/// A plain object with `fields` set in order, as the objects Electron's methods answer are.
pub fn record(context: &mut Context, fields: &[(&str, JsValue)]) -> JsResult<JsObject> {
    let object = native::object(context);
    for (key, value) in fields {
        object.set(JsString::from(*key), value.clone(), false, context)?;
    }
    Ok(object)
}

/// A plain `Error`: what Electron's native methods throw for a failure that is not a bad
/// argument (`Failed to get 'x' path`).
pub fn error(message: &str) -> JsError {
    boa_engine::JsNativeError::error()
        .with_message(message.to_owned())
        .into()
}

/// `TypeError: Object has been destroyed`.
pub fn destroyed() -> JsError {
    native::type_error("Object has been destroyed")
}

/// Which dialect an [`Args`] reads in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// Electron's native methods: `Error processing argument at index N, conversion failure
    /// from X`, and `Insufficient number of arguments.` for one that was not given.
    Gin,
    /// The methods Electron writes in JavaScript, and Node's own: `ERR_INVALID_ARG_TYPE`.
    Node,
}

/// The arguments of a native method, read and checked as the method's kind of API checks them.
pub struct Args<'a> {
    values: &'a [JsValue],
    dialect: Dialect,
}

impl<'a> Args<'a> {
    /// The arguments of one of Electron's native (gin) methods.
    pub fn new(values: &'a [JsValue]) -> Self {
        Self {
            values,
            dialect: Dialect::Gin,
        }
    }

    /// The arguments of a method that fails as Node's do.
    pub fn node(values: &'a [JsValue]) -> Self {
        Self {
            values,
            dialect: Dialect::Node,
        }
    }

    /// The error for argument `at`, named `name` in Node's dialect, that was not an `expected`.
    fn mismatch(&self, at: usize, name: &str, expected: &str, context: &mut Context) -> JsError {
        let value = arg(self.values, at);
        match self.dialect {
            Dialect::Node => {
                invalid_arg_type(context, name, &format!("of type {expected}"), &value)
            }
            Dialect::Gin => self.conversion_failure(at),
        }
    }

    /// Electron's gin error for argument `at`: `Insufficient number of arguments.` when it was not
    /// given, else a conversion failure that names the value only for `null`, `undefined` and
    /// strings.
    pub fn conversion_failure(&self, at: usize) -> JsError {
        if at >= self.values.len() {
            return native::type_error("Insufficient number of arguments.");
        }
        let value = arg(self.values, at);
        let from = if value.is_null_or_undefined() || value.is_string() {
            verve::print(&value)
        } else {
            String::new()
        };
        native::type_error(format!(
            "Error processing argument at index {at}, conversion failure from {from}"
        ))
    }

    /// `Insufficient number of arguments.` unless `count` arguments were given.
    pub fn require(&self, count: usize) -> JsResult<()> {
        if self.values.len() < count {
            return Err(native::type_error("Insufficient number of arguments."));
        }
        Ok(())
    }

    /// A string.
    pub fn string(&self, at: usize, name: &str, context: &mut Context) -> JsResult<String> {
        match arg(self.values, at).as_string() {
            Some(text) => Ok(text.to_std_string_escaped()),
            None => Err(self.mismatch(at, name, "string", context)),
        }
    }

    /// A number.
    pub fn f64(&self, at: usize, name: &str, context: &mut Context) -> JsResult<f64> {
        match arg(self.values, at).as_number() {
            Some(number) => Ok(number),
            None => Err(self.mismatch(at, name, "number", context)),
        }
    }

    /// A number, or `default` when the argument is left out.
    pub fn f64_or(
        &self,
        at: usize,
        name: &str,
        default: f64,
        context: &mut Context,
    ) -> JsResult<f64> {
        if arg(self.values, at).is_undefined() {
            return Ok(default);
        }
        self.f64(at, name, context)
    }

    /// The `N` numbers the arguments start with, as gin reads them: a missing one is
    /// `Insufficient number of arguments.`, and a bad one is named by the last of them.
    pub fn numbers<const N: usize>(&self, context: &mut Context) -> JsResult<[f64; N]> {
        let mut read = [0.; N];
        let mut failed = None;
        for (at, slot) in read.iter_mut().enumerate() {
            match arg(self.values, at).as_number() {
                Some(number) => *slot = number,
                None => failed = Some(at),
            }
        }
        match failed {
            Some(at) => Err(self.mismatch(at, "", "number", context)),
            None => Ok(read),
        }
    }

    /// A boolean, or `default` when the argument is left out. Gin reads any value by its
    /// truthiness; Node's dialect wants a boolean.
    pub fn bool_or(
        &self,
        at: usize,
        name: &str,
        default: bool,
        context: &mut Context,
    ) -> JsResult<bool> {
        let value = arg(self.values, at);
        match (value.as_boolean(), self.dialect) {
            _ if value.is_undefined() => Ok(default),
            (Some(flag), _) => Ok(flag),
            (None, Dialect::Gin) => Ok(value.to_boolean()),
            (None, Dialect::Node) => Err(self.mismatch(at, name, "boolean", context)),
        }
    }

    /// An object, or `None` when the argument is left out or `null`.
    pub fn object_opt(
        &self,
        at: usize,
        name: &str,
        context: &mut Context,
    ) -> JsResult<Option<JsObject>> {
        let value = arg(self.values, at);
        if value.is_null_or_undefined() {
            return Ok(None);
        }
        match value.as_object() {
            Some(object) => Ok(Some(object)),
            None => Err(self.mismatch(at, name, "object", context)),
        }
    }

    /// A function.
    pub fn function(&self, at: usize, name: &str, context: &mut Context) -> JsResult<JsObject> {
        match arg(self.values, at).as_callable() {
            Some(function) => Ok(function.clone()),
            None => Err(self.mismatch(at, name, "function", context)),
        }
    }
}

/// The fields of an options object, read for a constructor or a method that takes one.
///
/// [`Fields::new`] reads as Electron's constructors do: a field that is the wrong type is left at
/// its default, silently — `new BrowserWindow({ width: 'abc' })` is an 800 pixel window.
/// [`Fields::strict`] reads as Node's JavaScript does, failing in the property form: `The
/// "options.width" property must be of type number`.
pub struct Fields<'a> {
    object: Option<&'a JsObject>,
    /// What the object is called in a strict error: `options` in `options.width`.
    strict: Option<&'a str>,
}

impl<'a> Fields<'a> {
    /// The fields of `object`, when the script gave one; every wrong-typed one is its default.
    pub fn new(object: Option<&'a JsObject>) -> Self {
        Self {
            object,
            strict: None,
        }
    }

    /// The same, failing as Node's dialect does on a field of the wrong type; `name` is what the
    /// object is called in the message.
    pub fn strict(object: Option<&'a JsObject>, name: &'a str) -> Self {
        Self {
            object,
            strict: Some(name),
        }
    }

    fn value(&self, field: &str, context: &mut Context) -> JsResult<JsValue> {
        match self.object {
            Some(object) => object.get(JsString::from(field), context),
            None => Ok(JsValue::undefined()),
        }
    }

    /// What a field of the wrong type gives: nothing in the lenient reading, Node's error in the
    /// strict one.
    fn wrong<T>(
        &self,
        field: &str,
        expected: &str,
        value: &JsValue,
        context: &mut Context,
    ) -> JsResult<Option<T>> {
        match self.strict {
            None => Ok(None),
            Some(object) => Err(invalid_arg_type(
                context,
                &format!("{object}.{field}"),
                &format!("of type {expected}"),
                value,
            )),
        }
    }

    /// The field's number, `None` when it is absent or the wrong type.
    pub fn f64_opt(&self, field: &str, context: &mut Context) -> JsResult<Option<f64>> {
        let value = self.value(field, context)?;
        if value.is_null_or_undefined() {
            return Ok(None);
        }
        match value.as_number() {
            Some(number) => Ok(Some(number)),
            None => self.wrong(field, "number", &value, context),
        }
    }

    /// The field's number, or `default`.
    pub fn f64(&self, field: &str, default: f64, context: &mut Context) -> JsResult<f64> {
        Ok(self.f64_opt(field, context)?.unwrap_or(default))
    }

    /// The field's number, in the `f32` a window's geometry is kept in.
    pub fn f32(&self, field: &str, default: f32, context: &mut Context) -> JsResult<f32> {
        Ok(self.f64(field, f64::from(default), context)? as f32)
    }

    /// The field's integer, or `default`: Electron's options take `Integer`, and a fractional
    /// number is not one — `{ width: 300.7 }` is an 800 pixel window.
    pub fn int(&self, field: &str, default: f32, context: &mut Context) -> JsResult<f32> {
        Ok(self.int_opt(field, context)?.unwrap_or(default))
    }

    /// The field's integer, `None` when it is absent, the wrong type or not whole.
    pub fn int_opt(&self, field: &str, context: &mut Context) -> JsResult<Option<f32>> {
        match self.f64_opt(field, context)? {
            Some(number) if number.fract() == 0. => Ok(Some(number as f32)),
            Some(number) => self.wrong::<f32>(field, "integer", &JsValue::new(number), context),
            None => Ok(None),
        }
    }

    /// The field's boolean, or `default`. The lenient reading is by truthiness, as Electron's:
    /// `{ resizable: 0 }` is `false` and `{ show: 'yes' }` is `true`.
    pub fn bool(&self, field: &str, default: bool, context: &mut Context) -> JsResult<bool> {
        let value = self.value(field, context)?;
        if value.is_null_or_undefined() {
            return Ok(default);
        }
        match (value.as_boolean(), self.strict) {
            (Some(flag), _) => Ok(flag),
            (None, None) => Ok(value.to_boolean()),
            (None, Some(_)) => Ok(self
                .wrong::<bool>(field, "boolean", &value, context)?
                .unwrap_or(default)),
        }
    }

    /// The field's string, `None` when it is absent or the wrong type.
    pub fn string(&self, field: &str, context: &mut Context) -> JsResult<Option<String>> {
        let value = self.value(field, context)?;
        if value.is_null_or_undefined() {
            return Ok(None);
        }
        match value.as_string() {
            Some(text) => Ok(Some(text.to_std_string_escaped())),
            None => self.wrong(field, "string", &value, context),
        }
    }

    /// The field's function, `None` when it is absent or the wrong type.
    pub fn function(&self, field: &str, context: &mut Context) -> JsResult<Option<JsObject>> {
        let value = self.value(field, context)?;
        if value.is_null_or_undefined() {
            return Ok(None);
        }
        match value.as_callable() {
            Some(function) => Ok(Some(function.clone())),
            None => self.wrong(field, "function", &value, context),
        }
    }

    /// The field's object (a nested options object: `webPreferences`), `None` when it is absent
    /// or the wrong type. Read its own fields with [`Fields::new`].
    pub fn object(&self, field: &str, context: &mut Context) -> JsResult<Option<JsObject>> {
        let value = self.value(field, context)?;
        if value.is_null_or_undefined() {
            return Ok(None);
        }
        match value.as_object() {
            Some(object) => Ok(Some(object)),
            None => self.wrong(field, "object", &value, context),
        }
    }

    /// The field's string when it is one of `allowed`, `None` when it is absent or not a string.
    /// A string that is not one of them is `ERR_INVALID_ARG_VALUE` in the strict reading, and
    /// the default in the lenient.
    pub fn one_of(
        &self,
        field: &str,
        allowed: &[&'static str],
        context: &mut Context,
    ) -> JsResult<Option<&'static str>> {
        let Some(text) = self.string(field, context)? else {
            return Ok(None);
        };
        if let Some(found) = allowed.iter().find(|name| **name == text) {
            return Ok(Some(found));
        }
        match self.strict {
            None => Ok(None),
            Some(object) => Err(invalid_arg_value(
                context,
                &format!("{object}.{field}"),
                &JsValue::from(JsString::from(text)),
                &format!("must be one of: {}", allowed.join(", ")),
            )),
        }
    }
}

/// A `Buffer` of `bytes`.
///
/// # Errors
///
/// The realm has no `Buffer`.
pub fn buffer_of(context: &mut Context, bytes: Vec<u8>) -> JsResult<JsValue> {
    let array: JsObject =
        boa_engine::object::builtins::JsUint8Array::from_iter(bytes, context)?.into();
    let class = native::global(context, "Buffer")?;
    let prototype = native::get(&class, "prototype", context)?;
    array.set_prototype(prototype.as_object());
    Ok(array.into())
}
