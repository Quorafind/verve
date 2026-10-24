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

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    native_function::NativeFunctionPointer,
    object::{FunctionObjectBuilder, builtins::JsArray},
    property::{Attribute, PropertyDescriptor},
};

pub(crate) fn arg(args: &[JsValue], at: usize) -> JsValue {
    args.get(at).cloned().unwrap_or_default()
}

pub(crate) fn text_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

pub(crate) fn js_text(text: &str) -> JsValue {
    JsString::from(text).into()
}

pub(crate) fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

pub(crate) fn range_error(message: impl Into<String>) -> JsError {
    JsNativeError::range().with_message(message.into()).into()
}

/// An `Error` with Node's `code` on it: `ERR_INVALID_ARG_TYPE`, `ENOENT`, `ABORT_ERR`.
pub(crate) fn coded_error(context: &mut Context, message: &str, code: &str) -> JsError {
    let error = JsNativeError::error()
        .with_message(message.to_owned())
        .into_opaque(context);
    let _ = error.set(js_string!("code"), JsString::from(code), false, context);
    JsError::from_opaque(error.into())
}

/// The same, as a value: what a callback or an `error` event is handed.
pub(crate) fn coded_error_value(context: &mut Context, message: &str, code: &str) -> JsValue {
    coded_error(context, message, code)
        .into_opaque(context)
        .unwrap_or_default()
}

pub(crate) fn object(context: &mut Context) -> JsObject {
    JsObject::with_object_proto(context.intrinsics())
}

pub(crate) fn function(
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
pub(crate) fn closure<T: boa_gc::Trace + 'static>(
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

/// A property that is there but not listed: a method, or state kept on an object.
pub(crate) fn hidden(
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
pub(crate) fn methods(
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
pub(crate) fn exports(
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
pub(crate) fn accessors(
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
pub(crate) fn get(value: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    match value.as_object() {
        Some(object) => object.get(JsString::from(key), context),
        None => Ok(JsValue::undefined()),
    }
}

/// Calls the method `name` of `target` with `args`.
pub(crate) fn invoke(
    target: &JsValue,
    name: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let method = get(target, name, context)?;
    crate::call(context, &method, target, args)
}

/// The items of an array (or anything with a `length` and indices), as a list.
pub(crate) fn list_of(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
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

pub(crate) fn array(context: &mut Context, items: Vec<JsValue>) -> JsValue {
    JsArray::from_iter(items, context).into()
}

/// A global by name.
pub(crate) fn global(context: &mut Context, name: &str) -> JsResult<JsValue> {
    context.global_object().get(JsString::from(name), context)
}

/// `new Name(...args)`, for the global class `Name`.
pub(crate) fn construct_global(
    context: &mut Context,
    name: &str,
    args: &[JsValue],
) -> JsResult<JsObject> {
    let class = global(context, name)?;
    let class = class
        .as_object()
        .filter(|c| c.is_constructor())
        .ok_or_else(|| type_error(format!("{name} is not a constructor")))?;
    class.construct(args, None, context)
}

/// Whether `prototype` is `ancestor` or inherits it.
pub(crate) fn inherits(prototype: &JsObject, ancestor: &JsObject) -> bool {
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
pub(crate) fn is_instance(value: &JsValue, prototype: &JsObject) -> bool {
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
pub(crate) type Init = fn(&JsObject, &[JsValue], &mut Context) -> JsResult<()>;

/// A class to make with [`class`].
pub(crate) struct ClassSpec<'a> {
    pub name: &'a str,
    pub length: usize,
    pub init: Init,
    /// The class it extends: its constructor. The prototype inherits the parent's, and the
    /// constructor the parent constructor, so statics are inherited as with `class … extends`.
    pub parent: Option<&'a JsObject>,
    pub methods: &'a [(&'a str, usize, NativeFunctionPointer)],
    pub accessors: &'a [(
        &'a str,
        NativeFunctionPointer,
        Option<NativeFunctionPointer>,
    )],
    pub statics: &'a [(&'a str, usize, NativeFunctionPointer)],
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
pub(crate) fn class(context: &mut Context, spec: ClassSpec<'_>) -> JsResult<(JsObject, JsObject)> {
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
        .apply(class_apply)
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

/// `value` as the global `name`, writable and configurable so an embedder can put its own in
/// its place.
pub(crate) fn define_global(
    context: &mut Context,
    name: &str,
    value: impl Into<JsValue>,
) -> JsResult<()> {
    context.register_global_property(
        JsString::from(name),
        value,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )
}

/// What makes a global, the first time it is read.
pub(crate) type GlobalMaker = fn(&mut Context) -> JsResult<JsValue>;

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
pub(crate) fn lazy_global(context: &mut Context, name: &str, make: GlobalMaker) -> JsResult<()> {
    let key = JsString::from(name);
    let getter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, _, (key, make): &(JsString, MakerBox), context| {
                let value = (make.0)(context)?;
                settle_global(context, key, value.clone())?;
                Ok(value)
            },
            (key.clone(), MakerBox(make)),
        ),
    )
    .name(JsString::from(format!("get {name}")))
    .build();
    let setter = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |_, args, key: &JsString, context| {
                settle_global(context, key, arg(args, 0))?;
                Ok(JsValue::undefined())
            },
            key.clone(),
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

fn settle_global(context: &mut Context, key: &JsString, value: JsValue) -> JsResult<()> {
    context.global_object().define_property_or_throw(
        key.clone(),
        PropertyDescriptor::builder()
            .value(value)
            .writable(true)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    Ok(())
}

/// A promise already settled with `outcome`: resolved with its value, or rejected with its error.
pub(crate) fn settled(outcome: JsResult<JsValue>, context: &mut Context) -> JsResult<JsValue> {
    use boa_engine::object::builtins::JsPromise;
    match outcome {
        Ok(value) => Ok(JsPromise::resolve(value, context)?.into()),
        Err(error) => Ok(JsPromise::reject(error, context)?.into()),
    }
}
