//! The objects a script holds, built natively in Boa over [`crate::dom`].
//!
//! Every class — `Node`, `Element`, `HTMLInputElement`, `Document`, `MouseEvent`… — is a
//! constructor made here from Rust functions, with Rust functions on its prototype. A node's
//! object is one of Verve's event targets (`verve::dom_events::host_target`), so `Node` extends
//! the realm's own `EventTarget` and an event dispatched at a node travels the tree: Verve's
//! dispatch asks [`events::parent`] what each target is in.
//!
//! An object that stands for something else the document keeps — a node's `classList`, its
//! `style`, a `<style>`'s sheet — carries a [`Handle`]: what kind of thing, and its number.

pub mod document;
pub mod element;
pub mod events;
pub mod fetch;
pub mod node;
pub mod observers;
pub mod platform;
pub mod traversal;
pub mod window;
pub mod xhr;

use boa_engine::{
    Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue,
    NativeFunction, Trace, js_string,
    native_function::NativeFunctionPointer,
    object::{FunctionObjectBuilder, builtins::JsFunction},
    property::{Attribute, PropertyDescriptor, PropertyKey},
};

use crate::dom::NodeId;
use crate::state::shared;

/// A method: its name, how many arguments it declares, and the native.
pub type Method = (&'static str, usize, NativeFunctionPointer);
/// An accessor: its name, its getter and its setter.
pub type Accessor = (
    &'static str,
    Option<NativeFunctionPointer>,
    Option<NativeFunctionPointer>,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ClassList,
    Style,
    /// `getComputedStyle`'s answer, read-only.
    Computed,
    Dataset,
    /// The sheet of a `<style>` element, by the element's id.
    Sheet,
    /// A `new CSSStyleSheet()`, by its number.
    Constructed,
    /// An instance whose state is all in its own properties.
    Plain,
}

#[derive(Debug, Trace, Finalize, JsData)]
pub struct Handle {
    #[unsafe_ignore_trace]
    pub kind: Kind,
    #[unsafe_ignore_trace]
    pub id: u32,
}

pub fn handle(value: &JsValue) -> Option<(Kind, u32)> {
    let object = value.as_object()?;
    let handle = object.downcast_ref::<Handle>()?;
    Some((handle.kind, handle.id))
}

pub fn handle_of(value: &JsValue, kind: Kind) -> Option<u32> {
    handle(value).filter(|(k, _)| *k == kind).map(|(_, id)| id)
}

pub fn type_error(message: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(message.into()).into()
}

/// A `DOMException` of `name` (`NotFoundError`, `HierarchyRequestError`, `SyntaxError`…).
pub fn dom_error(context: &mut Context, message: &str, name: &str) -> JsError {
    verve::dom_events::exception(context, message, name)
}

/// The node `value` is the object of, if it is a node's.
pub fn node_id(value: &JsValue) -> Option<NodeId> {
    let object = value.as_object()?;
    verve::dom_events::host_id(&object).and_then(node::node_of_host)
}

pub fn node_of(value: &JsValue) -> JsResult<NodeId> {
    node_id(value).ok_or_else(|| type_error("Illegal invocation: not a Node"))
}

/// A node argument: a `TypeError` naming the method when it is not one.
pub fn node_arg(args: &[JsValue], index: usize, method: &str, class: &str) -> JsResult<NodeId> {
    node_id(&arg(args, index)).ok_or_else(|| {
        type_error(format!(
            "Failed to execute '{method}' on '{class}': parameter {} is not of type 'Node'.",
            index + 1
        ))
    })
}

pub fn arg(args: &[JsValue], index: usize) -> JsValue {
    args.get(index).cloned().unwrap_or_default()
}

pub fn string(value: &JsValue, context: &mut Context) -> JsResult<String> {
    Ok(value.to_string(context)?.to_std_string_escaped())
}

pub fn arg_string(args: &[JsValue], index: usize, context: &mut Context) -> JsResult<String> {
    string(&arg(args, index), context)
}

/// The argument as a string, or `None` when it is `undefined` or `null`.
pub fn opt_string(
    args: &[JsValue],
    index: usize,
    context: &mut Context,
) -> JsResult<Option<String>> {
    let value = arg(args, index);
    if value.is_null_or_undefined() {
        return Ok(None);
    }
    Ok(Some(string(&value, context)?))
}

pub fn text(s: &str) -> JsValue {
    JsString::from(s).into()
}

pub fn get(object: &JsValue, key: &str, context: &mut Context) -> JsResult<JsValue> {
    match object.as_object() {
        Some(object) => object.get(JsString::from(key), context),
        None => Ok(JsValue::undefined()),
    }
}

pub fn set(
    object: &JsObject,
    key: &str,
    value: impl Into<JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    object.set(JsString::from(key), value.into(), true, context)?;
    Ok(())
}

/// A property that is there but not listed.
pub fn hidden(
    object: &JsObject,
    key: impl Into<PropertyKey>,
    value: impl Into<JsValue>,
    context: &mut Context,
) -> JsResult<()> {
    object.define_property_or_throw(
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

/// `object[name](...args)`, looked up the way JavaScript looks it up. `undefined` when there is
/// no such method.
pub fn call_method(
    object: &JsValue,
    name: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let method = get(object, name, context)?;
    match method.as_callable() {
        Some(method) => method.call(object, args, context),
        None => Ok(JsValue::undefined()),
    }
}

pub fn function(
    context: &mut Context,
    name: &str,
    length: usize,
    f: NativeFunctionPointer,
) -> JsFunction {
    FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(f))
        .name(JsString::from(name))
        .length(length)
        .build()
}

/// A function that carries `captures` with it.
pub fn closure<T: Trace + 'static>(
    context: &mut Context,
    name: &str,
    f: fn(&JsValue, &[JsValue], &T, &mut Context) -> JsResult<JsValue>,
    captures: T,
) -> JsFunction {
    FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(f, captures),
    )
    .name(JsString::from(name))
    .build()
}

pub fn plain_object(context: &mut Context) -> JsObject {
    JsObject::with_object_proto(context.intrinsics())
}

pub fn array(values: Vec<JsValue>, context: &mut Context) -> JsValue {
    boa_engine::object::builtins::JsArray::from_iter(values, context).into()
}

/// The items of an array-like.
pub fn list(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let Some(object) = value.as_object() else {
        return Ok(Vec::new());
    };
    let length = object
        .get(js_string!("length"), context)?
        .to_length(context)?;
    let mut out = Vec::with_capacity(length as usize);
    for at in 0..length {
        out.push(object.get(at, context)?);
    }
    Ok(out)
}

/// A promise already settled with `value`.
pub fn resolved(value: JsValue, context: &mut Context) -> JsResult<JsValue> {
    Ok(boa_engine::object::builtins::JsPromise::resolve(value, context)?.into())
}

pub fn rejected(error: JsError, context: &mut Context) -> JsResult<JsValue> {
    Ok(boa_engine::object::builtins::JsPromise::reject(error, context)?.into())
}

pub fn illegal_constructor(_: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Err(type_error("Illegal constructor"))
}

/// One native class: its constructor, what it extends, and what its prototype holds.
pub struct ClassSpec<'a> {
    pub name: &'static str,
    pub length: usize,
    pub construct: NativeFunctionPointer,
    /// The class it extends, by name (one made here, or `EventTarget`, `Event`…).
    pub parent: Option<&'a str>,
    pub methods: &'a [Method],
    pub accessors: &'a [Accessor],
    pub statics: &'a [Method],
    /// Whether the constructor is a global of the window's.
    pub global: bool,
}

/// Makes a class and remembers it by name.
pub fn class(context: &mut Context, spec: ClassSpec<'_>) -> JsResult<(JsObject, JsObject)> {
    let parent = spec.parent.map(|name| {
        shared(context)
            .borrow()
            .class(name)
            .unwrap_or_else(|| panic!("the parent class {name} is made first"))
    });
    let mut builder = boa_engine::object::ConstructorBuilder::new(
        context,
        NativeFunction::from_fn_ptr(spec.construct),
    );
    builder
        .name(spec.name)
        .length(spec.length)
        .constructor(true);
    if let Some((constructor, prototype)) = &parent {
        builder.inherit(prototype.clone());
        builder.custom_prototype(constructor.clone());
    }
    for (name, length, f) in spec.statics {
        builder.static_method(
            NativeFunction::from_fn_ptr(*f),
            JsString::from(*name),
            *length,
        );
    }
    let built = builder.build();
    let (constructor, prototype) = (built.constructor(), built.prototype());
    extend_prototype(context, &prototype, spec.methods, spec.accessors)?;
    prototype.define_property_or_throw(
        boa_engine::JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(text(spec.name))
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    shared(context)
        .borrow_mut()
        .classes
        .insert(spec.name, (constructor.clone(), prototype.clone()));
    if spec.global {
        context.register_global_property(
            JsString::from(spec.name),
            constructor.clone(),
            Attribute::WRITABLE | Attribute::CONFIGURABLE,
        )?;
    }
    Ok((constructor, prototype))
}

/// Puts `methods` and `accessors` on `prototype`, over what is there.
pub fn extend_prototype(
    context: &mut Context,
    prototype: &JsObject,
    methods: &[Method],
    accessors: &[Accessor],
) -> JsResult<()> {
    for (name, length, f) in methods {
        let made = function(context, name, *length, *f);
        prototype.define_property_or_throw(
            JsString::from(*name),
            PropertyDescriptor::builder()
                .value(made)
                .writable(true)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    for (name, get, set) in accessors {
        let mut descriptor = PropertyDescriptor::builder()
            .enumerable(true)
            .configurable(true);
        if let Some(get) = get {
            descriptor = descriptor.get(function(context, &format!("get {name}"), 0, *get));
        }
        if let Some(set) = set {
            descriptor = descriptor.set(function(context, &format!("set {name}"), 1, *set));
        }
        prototype.define_property_or_throw(JsString::from(*name), descriptor, context)?;
    }
    Ok(())
}

/// The object a native constructor builds for something that is no node: on `new_target`'s
/// prototype, so a subclass's methods are found on it, holding `kind` and `id`.
pub fn instance(
    new_target: &JsValue,
    kind: Kind,
    id: u32,
    context: &mut Context,
) -> JsResult<JsObject> {
    let prototype = prototype_of_target(new_target, context)?;
    Ok(JsObject::from_proto_and_data(
        prototype,
        Handle { kind, id },
    ))
}

/// The prototype an object made with `new` takes: `new.target`'s.
pub fn prototype_of_target(new_target: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let Some(target) = new_target.as_object() else {
        return Err(type_error(
            "Class constructor cannot be invoked without 'new'",
        ));
    };
    Ok(target
        .get(js_string!("prototype"), context)?
        .as_object()
        .unwrap_or_else(|| context.intrinsics().constructors().object().prototype()))
}

/// An instance of one of the classes made here, made by the host itself.
pub fn native_instance(context: &Context, class: &str, kind: Kind, id: u32) -> JsObject {
    let prototype = shared(context).borrow().prototype(class);
    JsObject::from_proto_and_data(prototype, Handle { kind, id })
}

/// The constructor of a class made here, by name.
pub fn constructor_of(context: &Context, name: &str) -> Option<JsObject> {
    shared(context).borrow().class(name).map(|(c, _)| c)
}

/// Sets `(key, value)` pairs on a fresh object: an event's `init`, a rect.
pub fn dictionary(context: &mut Context, pairs: &[(&str, JsValue)]) -> JsResult<JsObject> {
    let object = plain_object(context);
    for (key, value) in pairs {
        object.set(JsString::from(*key), value.clone(), false, context)?;
    }
    Ok(object)
}
