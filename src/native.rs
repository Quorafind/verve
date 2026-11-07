//! What the runtime's native modules share: reading arguments, making functions, and making
//! classes that behave as Node's and the web's do when JavaScript extends them.
//!
//! A class here is a native constructor and a prototype of native methods. It can be extended
//! three ways, and each works:
//!
//! - `new Readable(options)`: the constructor makes the object on its own prototype.
//! - `class Parser extends Transform { constructor() { super() } }`: `super()` constructs with the
//!   subclass as `new.target`, and the object is made on the subclass's prototype.
//! - `function Parser() { Transform.call(this) } util.inherits(Parser, Transform)`: older npm code,
//!   which calls the constructor as a plain function on an object it already made. The
//!   constructor then sets up that object and answers it, as Node's own ES5 constructors do.
//!
//! Boa calls a native constructor called without `new` with no `this` at all, so the class
//! JavaScript sees is a `Proxy` of it, whose traps are native too: `construct` hands `new.target`
//! on, and `apply` hands on the `this` the caller gave, which the constructor then sets up.
//!
//! This module is also the embedder's toolkit: the native functions, classes and objects a host
//! defining `obsidian`, `electron` or a document's own API builds its modules out of. Every
//! item here is public for that; the runtime's own modules use the same ones.

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor},
};

/// The argument at `at`, or `undefined` when the call gave fewer.
pub fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

/// `value` converted to a string as JavaScript's `String(value)` does, which may run its
/// `toString` and so may throw.
pub fn text_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

/// The argument at `at` converted to a string as `String(value)` does: [`text_of`] of [`arg`].
pub fn arg_text(args: &[JsValue], at: usize, context: &mut Context) -> JsResult<String> {
    text_of(&arg(args, at), context)
}

/// `text` as a JavaScript string value.
pub fn js_text(text: &str) -> JsValue {
    JsString::from(text).into()
}

/// A `TypeError` with `message`.
pub fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

/// A native constructor that refuses to construct: `new X` is a `TypeError`, as for an interface
/// with no constructor.
pub fn illegal_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(type_error("Illegal constructor"))
}

/// An `Error` with `message`.
pub fn error(message: impl Into<String>) -> JsError {
    JsNativeError::error().with_message(message.into()).into()
}

/// A `RangeError` with `message`.
pub fn range_error(message: impl Into<String>) -> JsError {
    JsNativeError::range().with_message(message.into()).into()
}

/// An `Error` with Node's `code` on it: `ERR_INVALID_ARG_TYPE`, `ENOENT`, `ABORT_ERR`.
pub fn coded_error(context: &mut Context, message: &str, code: &str) -> JsError {
    let error = JsNativeError::error()
        .with_message(message.to_owned())
        .into_opaque(context);
    let _ = error.set(js_string!("code"), JsString::from(code), false, context);
    JsError::from_opaque(error.into())
}

/// A `TypeError` with Node's `code` on it: `ERR_INVALID_URL` of `new URL('nonsense')`.
pub fn coded_type_error(context: &mut Context, message: &str, code: &str) -> JsError {
    crate::events::coded_type_error(context, message, code)
}

/// The same, as a value: what a callback or an `error` event is handed.
pub fn coded_error_value(context: &mut Context, message: &str, code: &str) -> JsValue {
    coded_error(context, message, code)
        .into_opaque(context)
        .unwrap_or_default()
}

/// Node's `ERR_INVALID_ARG_TYPE`: a `TypeError` saying `name` must be `expected` and what it
/// was instead, as `The "file" argument must be of type string. Received undefined`.
pub fn invalid_arg_type(
    context: &mut Context,
    name: &str,
    expected: &str,
    value: &JsValue,
) -> JsError {
    let kind = if name.contains('.') {
        "property"
    } else {
        "argument"
    };
    let message = format!(
        "The \"{name}\" {kind} must be {expected}. Received {}",
        received(value, context)
    );
    crate::events::coded_type_error(context, &message, "ERR_INVALID_ARG_TYPE")
}

/// Node's `ERR_INVALID_ARG_VALUE`: a `TypeError` for a `name` of the right type but a value
/// that will not do, `reason` saying why: `The argument 'file' cannot be empty. Received ''`.
pub fn invalid_arg_value(
    context: &mut Context,
    name: &str,
    value: &JsValue,
    reason: &str,
) -> JsError {
    let shown = match value.to_string(context) {
        Ok(text) if !value.is_object() => format!("'{}'", text.to_std_string_escaped()),
        _ => received(value, context),
    };
    let message = format!("The argument '{name}' {reason}. Received {shown}");
    crate::events::coded_type_error(context, &message, "ERR_INVALID_ARG_VALUE")
}

/// How Node's argument errors name what they were given.
pub(crate) fn received(value: &JsValue, context: &mut Context) -> String {
    if value.is_null_or_undefined() {
        return crate::print(value);
    }
    if value.is_callable() {
        return "function".into();
    }
    if let Some(object) = value.as_object() {
        let constructor = object.get(js_string!("constructor"), context).ok();
        let name = get(
            constructor.as_ref().unwrap_or(&JsValue::undefined()),
            "name",
            context,
        )
        .map(|n| crate::print(&n))
        .unwrap_or_default();
        return if name.is_empty() {
            "an instance of Object".into()
        } else {
            format!("an instance of {name}")
        };
    }
    if value.is_string() {
        return format!("type string ('{}')", crate::print(value));
    }
    format!("type {} ({})", value.type_of(), crate::print(value))
}

/// A new empty object on `Object.prototype`.
pub fn object(context: &mut Context) -> JsObject {
    JsObject::with_object_proto(context.intrinsics())
}

/// A native function `name` of `length` parameters, running `f`.
pub fn function(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
        .into()
}

/// A function holding `captures`, which it is handed each time it is called.
pub fn closure<T: boa_gc::Trace + 'static>(
    context: &mut Context,
    name: &str,
    length: usize,
    f: fn(&JsValue, &[JsValue], &T, &mut Context) -> JsResult<JsValue>,
    captures: T,
) -> JsObject {
    FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(f, captures),
    )
    .name(JsString::from(name))
    .length(length)
    .build()
    .into()
}

/// A function a [`closure`] runs: the receiver, the arguments, its captures, the context.
pub type ClosureFn<T> = fn(&JsValue, &[JsValue], &T, &mut Context) -> JsResult<JsValue>;

/// `promise.then(ok, err)` where both reactions hold `captures`: `ok` runs with the value the
/// promise is fulfilled with, `err` with the reason it is rejected with. Answers the promise
/// `then` made.
pub fn then<T: boa_gc::Trace + Clone + 'static>(
    context: &mut Context,
    promise: &JsValue,
    ok: ClosureFn<T>,
    err: ClosureFn<T>,
    captures: T,
) -> JsResult<JsValue> {
    let ok = closure(context, "", 1, ok, captures.clone());
    let err = closure(context, "", 1, err, captures);
    invoke(promise, "then", &[ok.into(), err.into()], context)
}

/// A property that is there but not listed: a method, or state kept on an object.
pub fn hidden(
    target: &JsObject,
    key: impl Into<boa_engine::property::PropertyKey>,
    value: impl Into<JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    target.define_property_or_throw(
        key,
        PropertyDescriptor::builder()
            .value(value)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    Ok(())
}

/// Methods on `target`, listed nowhere, as a class's own are.
pub fn methods(
    context: &mut Context,
    target: &JsObject,
    list: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<()> {
    for (name, length, f) in list {
        let method = function(context, name, *length, *f);
        hidden(target, JsString::from(*name), method, context)?;
    }
    Ok(())
}

/// Functions on `target` as plain properties: a module's exports.
pub fn exports(
    context: &mut Context,
    target: &JsObject,
    list: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<()> {
    for (name, length, f) in list {
        let method = function(context, name, *length, *f);
        target.set(JsString::from(*name), method, false, context)?;
    }
    Ok(())
}

/// Accessors on `target`: a getter each, and a setter where there is one.
pub fn accessors(
    context: &mut Context,
    target: &JsObject,
    list: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)],
) -> JsResult<()> {
    for (name, get, set) in list {
        let getter = function(context, &format!("get {name}"), 0, *get);
        let mut descriptor = PropertyDescriptor::builder()
            .get(getter)
            .enumerable(false)
            .configurable(true);
        if let Some(set) = set {
            descriptor = descriptor.set(function(context, &format!("set {name}"), 1, *set));
        }
        target.define_property_or_throw(JsString::from(*name), descriptor, context)?;
    }
    Ok(())
}

/// A property of `value` when it is an object; `undefined` otherwise.
pub fn get(value: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    match value.as_object() {
        Some(object) => object.get(JsString::from(key), context),
        None => Ok(JsValue::undefined()),
    }
}

/// Calls the method `name` of `target` with `args`.
pub fn invoke(
    target: &JsValue,
    name: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let method = get(target, name, context)?;
    crate::call(context, &method, target, args)
}

/// The items of an array (or anything with a `length` and indices), as a list.
pub fn list_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let Some(object) = value.as_object() else {
        return Ok(Vec::new());
    };
    if let Ok(array) = JsArray::from_object(object.clone()) {
        return (0..array.length(context)?)
            .map(|at| array.get(at, context))
            .collect();
    }
    let length = object
        .get(js_string!("length"), context)?
        .to_length(context)?;
    (0..length).map(|at| object.get(at, context)).collect()
}

/// The items of anything `Array.from` takes (an iterable such as a `Set` or a `Map`'s entries
/// as well as an array-like), as a list. Where [`list_of`] reads only indices, this runs the
/// iterator.
pub fn list_from(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let list = crate::call(
        context,
        &from,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    let Some(list) = list.as_object() else {
        return Ok(Vec::new());
    };
    let list = JsArray::from_object(list)?;
    (0..list.length(context)?)
        .map(|at| list.get(at, context))
        .collect()
}

/// A JavaScript array of `items`.
pub fn array(context: &mut Context, items: Vec<JsValue>) -> JsValue {
    JsArray::from_iter(items, context).into()
}

/// A global by name.
pub fn global(context: &mut Context, name: &str) -> JsResult<JsValue> {
    context.global_object().get(JsString::from(name), context)
}

/// `new Name(...args)`, for the global class `Name`.
pub fn construct_global(context: &mut Context, name: &str, args: &[JsValue]) -> JsResult<JsObject> {
    let class = global(context, name)?;
    let class = class
        .as_object()
        .filter(|c| c.is_constructor())
        .ok_or_else(|| type_error(format!("{name} is not a constructor")))?;
    class.construct(args, None, context)
}

/// Whether `prototype` is `ancestor` or inherits it.
pub fn inherits(prototype: &JsObject, ancestor: &JsObject) -> bool {
    let mut at = Some(prototype.clone());
    // Bounded, against a prototype chain made circular by a proxy.
    for _ in 0..10_000 {
        match at {
            None => return false,
            Some(object) if JsObject::equals(&object, ancestor) => return true,
            Some(object) => at = object.prototype(),
        }
    }
    false
}

/// Whether `value` is an instance of the class whose prototype is `prototype`.
pub fn is_instance(value: &JsValue, prototype: &JsObject) -> bool {
    value
        .as_object()
        .and_then(|o| o.prototype())
        .is_some_and(|p| inherits(&p, prototype))
}

/// The object a class's constructor sets up (see the module's comment). Constructed, Boa hands
/// it `new.target` as `this`, and the object is made on that one's prototype. Called, it is
/// handed nothing, and the class's `apply` trap has left the caller's `this` in the runtime's
/// state: that object is set up, unless it is nothing or the global object — then, as
/// `Readable()` without `new` in Node, a fresh one is made.
///
/// Not part of the embedder's toolkit: it depends on the handshake with [`class`]'s `apply` trap,
/// which only classes made here keep.
pub(crate) fn instance(
    this: &JsValue,
    prototype: &JsObject,
    context: &mut Context,
) -> JsResult<JsObject> {
    if let Some(new_target) = this.as_object() {
        let own = new_target.get(js_string!("prototype"), context)?;
        let made = object(context);
        made.set_prototype(Some(own.as_object().unwrap_or_else(|| prototype.clone())));
        return Ok(made);
    }
    let given = crate::shared(context).called_on.borrow_mut().take();
    if let Some(target) = given.as_ref().and_then(JsValue::as_object) {
        let global = context.global_object();
        if !JsObject::equals(&target, &global) {
            return Ok(target);
        }
    }
    let made = object(context);
    made.set_prototype(Some(prototype.clone()));
    Ok(made)
}

/// A class's `apply` trap — `Class.call(self, …)` — with `[target, this, arguments]`: the
/// constructor called with the caller's `this` left where [`instance`] finds it.
fn class_apply(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let list = list_of(&arg(args, 2), context)?;
    let previous = crate::shared(context).called_on.replace(Some(arg(args, 1)));
    let outcome = crate::call(context, &target, &JsValue::undefined(), &list);
    *crate::shared(context).called_on.borrow_mut() = previous;
    outcome
}

/// The `apply` trap of a class that is only ever constructed, as an ES `class` is:
/// `Class(…)` and `Class.call(self, …)` throw V8's `TypeError`.
fn class_apply_refused(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = get(&arg(args, 0), "name", context)?;
    Err(type_error(format!(
        "Class constructor {} cannot be invoked without 'new'",
        crate::print(&name)
    )))
}

/// The same for a class a host makes native (V8 `FunctionTemplate`): a plain `Error`.
fn native_apply_refused(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(JsNativeError::error()
        .with_message("Requires constructor call")
        .into())
}

/// A class's `construct` trap — `new Class(…)`, `super(…)` — with
/// `[target, arguments, new.target]`.
fn class_construct(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0);
    let target = target
        .as_object()
        .ok_or_else(|| type_error("not a class"))?;
    let list = list_of(&arg(args, 1), context)?;
    let new_target = arg(args, 2).as_object();
    Ok(target
        .construct(&list, new_target.as_ref(), context)?
        .into())
}

/// How a class sets up an object made by, or handed to, its constructor.
pub type Init = fn(&JsObject, &[JsValue], &mut Context) -> JsResult<()>;

/// A class to make with [`class`].
pub struct ClassSpec<'a> {
    /// The class's name, as `Class.name` and a stack trace show it.
    pub name: &'a str,
    /// The constructor's `length`: how many parameters it declares.
    pub length: usize,
    /// Sets up the object the constructor was called on or made.
    pub init: Init,
    /// The class it extends: its constructor. The prototype inherits the parent's, and the
    /// constructor the parent constructor, so statics are inherited as with `class … extends`.
    pub parent: Option<&'a JsObject>,
    /// The prototype's methods: name, `length` and function.
    pub methods: &'a [(&'a str, usize, NativeFunctionPointer)],
    /// The prototype's accessors: name, getter and optional setter.
    pub accessors: &'a [(
        &'a str,
        NativeFunctionPointer,
        Option<NativeFunctionPointer>,
    )],
    /// The constructor's own methods: name, `length` and function.
    pub statics: &'a [(&'a str, usize, NativeFunctionPointer)],
    /// Whether, and how, the class refuses to be called without `new`. `Class()` and
    /// `Class.call(this)` then throw, and `util.inherits` cannot make a function-style subclass
    /// of it. Node's own classes (`Readable`, `EventEmitter`) allow the call, so the default is
    /// `None`.
    pub new_only: Option<NewOnly>,
}

/// What a class that refuses a call without `new` throws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewOnly {
    /// V8's own, for an ES `class`: `TypeError: Class constructor X cannot be invoked without
    /// 'new'`.
    Class,
    /// Electron's native classes (`BrowserWindow` in 39): `Error: Requires constructor call`.
    Native,
}

impl Default for ClassSpec<'_> {
    fn default() -> Self {
        ClassSpec {
            name: "",
            length: 0,
            init: |_, _, _| Ok(()),
            parent: None,
            methods: &[],
            accessors: &[],
            statics: &[],
            new_only: None,
        }
    }
}

/// What a class's native constructor does: makes or takes the object, and sets it up.
fn construct(
    this: &JsValue,
    args: &[JsValue],
    (prototype, init): &(JsObject, InitBox),
    context: &mut Context,
) -> JsResult<JsValue> {
    let target = instance(this, prototype, context)?;
    (init.0)(&target, args, context)?;
    Ok(target.into())
}

/// An [`Init`] the collector may hold: a function pointer has nothing in it to trace.
#[derive(Clone, Copy)]
struct InitBox(Init);

impl boa_gc::Finalize for InitBox {}
// SAFETY: a function pointer holds no garbage-collected value.
unsafe impl boa_gc::Trace for InitBox {
    boa_gc::empty_trace!();
}

/// Makes a class: answers its constructor — the `Proxy` JavaScript sees — and its prototype.
/// It is not made a global; see [`define_global`].
pub fn class(context: &mut Context, spec: ClassSpec<'_>) -> JsResult<(JsObject, JsObject)> {
    let prototype = object(context);
    let parent_prototype = match spec.parent {
        Some(parent) => parent.get(js_string!("prototype"), context)?.as_object(),
        None => None,
    };
    if let Some(parent_prototype) = parent_prototype {
        prototype.set_prototype(Some(parent_prototype));
    }
    let constructor: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            construct,
            (prototype.clone(), InitBox(spec.init)),
        ),
    )
    .name(JsString::from(spec.name))
    .length(spec.length)
    .constructor(true)
    .build()
    .into();
    if let Some(parent) = spec.parent {
        constructor.set_prototype(Some(parent.clone()));
    }
    methods(context, &prototype, spec.methods)?;
    accessors(context, &prototype, spec.accessors)?;
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    methods(context, &constructor, spec.statics)?;
    let class: JsObject = boa_engine::object::builtins::JsProxy::builder(constructor)
        .apply(match spec.new_only {
            None => class_apply,
            Some(NewOnly::Class) => class_apply_refused,
            Some(NewOnly::Native) => native_apply_refused,
        })
        .construct(class_construct)
        .build(context)?
        .into();
    hidden(
        &prototype,
        js_string!("constructor"),
        class.clone(),
        context,
    )?;
    Ok((class, prototype))
}

/// A native function `name` of `length` parameters that `new` can call.
pub fn constructor(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsObject {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .constructor(true)
        .build()
        .into()
}

/// A plain class: what [`plain_class`] makes from this.
///
/// Unlike [`class`] it is not wrapped in a `Proxy`, so a call without `new` (or
/// `Foo.call(this)`) runs `construct` with no `this`, and `construct` must refuse it itself.
/// Its methods are not listed (`for..in` skips them) and its getters are.
pub struct PlainClass<'a> {
    /// The class's name: the constructor's `name`.
    pub name: &'a str,
    /// What `new` runs; it is handed `new.target` as `this`.
    pub construct: NativeFunctionPointer,
    /// The constructor's `length`.
    pub length: usize,
    /// The prototype the class's own prototype inherits (a prototype, not a constructor as
    /// [`ClassSpec::parent`] is); `Object.prototype` when none.
    pub parent: Option<JsObject>,
    /// The prototype's methods: name, parameter count, function.
    pub methods: &'a [(&'a str, usize, NativeFunctionPointer)],
    /// The prototype's read-only accessors: name and getter.
    pub getters: &'a [(&'a str, NativeFunctionPointer)],
}

/// The constructor and prototype of a [`PlainClass`], linked both ways. Putting the constructor
/// somewhere (a global, a namespace) is the caller's.
pub fn plain_class(context: &mut Context, spec: PlainClass<'_>) -> JsResult<(JsObject, JsObject)> {
    let class = constructor(context, spec.name, spec.length, spec.construct);
    let prototype = object(context);
    if let Some(parent) = spec.parent {
        prototype.set_prototype(Some(parent));
    }
    methods(context, &prototype, spec.methods)?;
    for (name, f) in spec.getters {
        let getter = function(context, &format!("get {name}"), 0, *f);
        prototype.define_property_or_throw(
            JsString::from(*name),
            PropertyDescriptor::builder()
                .get(getter)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    hidden(
        &prototype,
        js_string!("constructor"),
        class.clone(),
        context,
    )?;
    class.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    Ok((class, prototype))
}

/// `value` as the global `name`, writable and configurable so an embedder can put its own in
/// its place.
pub fn define_global(context: &mut Context, name: &str, value: impl Into<JsValue>) -> JsResult<()> {
    context.register_global_property(
        JsString::from(name),
        value,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )
}

/// What makes a global, the first time it is read.
pub type GlobalMaker = fn(&mut Context) -> JsResult<JsValue>;

#[derive(Clone, Copy)]
struct MakerBox(GlobalMaker);

impl boa_gc::Finalize for MakerBox {}
// SAFETY: a function pointer holds no garbage-collected value.
unsafe impl boa_gc::Trace for MakerBox {
    boa_gc::empty_trace!();
}

/// A global made the first time it is read — or written, which simply puts the new value in its
/// place. Every realm (each worker's too) starts with all of them, and makes only the ones its
/// scripts touch.
///
/// The global stays an accessor, its value kept in a cell the getter and setter share: turning
/// it into a data property once made would be the plain way, but Boa's lookup of a global by
/// name, cached by an earlier read, then calls the next `new Name()` without its `new.target`.
pub fn lazy_global(context: &mut Context, name: &str, make: GlobalMaker) -> JsResult<()> {
    let key = JsString::from(name);
    let cell = JsObject::with_null_proto();
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, _, (cell, make, name): &(JsObject, MakerBox, String), context| {
                if cell.has_own_property(js_string!("value"), context)? {
                    return cell.get(js_string!("value"), context);
                }
                let value = (make.0)(context)?;
                cell.set(js_string!("value"), value.clone(), false, context)?;
                // The record is Verve's own; a `Context` that is not a Verve runtime has none.
                if let Some(state) = context.get_data::<std::rc::Rc<crate::Shared>>() {
                    state
                        .lazy_values
                        .borrow_mut()
                        .insert(name.clone(), value.clone());
                }
                Ok(value)
            },
            (cell.clone(), MakerBox(make), name.to_owned()),
        ),
    )
    .name(JsString::from(format!("get {name}")))
    .build();
    let setter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, cell: &JsObject, context| {
                cell.set(js_string!("value"), arg(args, 0), false, context)?;
                Ok(JsValue::undefined())
            },
            cell,
        ),
    )
    .name(JsString::from(format!("set {name}")))
    .length(1)
    .build();
    context.global_object().define_property_or_throw(
        key,
        PropertyDescriptor::builder()
            .get(getter)
            .set(setter)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    Ok(())
}

/// The lazy global `name`'s value, when something has already read it: a hook that mends one
/// in place asks this, so it never makes a global nothing asked for.
pub(crate) fn lazy_value(context: &Context, name: &str) -> Option<JsValue> {
    crate::shared(context)
        .lazy_values
        .borrow()
        .get(name)
        .cloned()
}

/// A promise already settled with `outcome`: resolved with its value, or rejected with its error.
pub fn settled(outcome: JsResult<JsValue>, context: &mut Context) -> JsResult<JsValue> {
    use boa_engine::object::builtins::JsPromise;
    match outcome {
        Ok(value) => Ok(JsPromise::resolve(value, context)?.into()),
        Err(error) => Ok(JsPromise::reject(error, context)?.into()),
    }
}
