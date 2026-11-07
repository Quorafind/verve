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

pub mod animation;
pub mod caret;
pub mod computed;
pub mod custom;
pub mod document;
pub mod element;
pub mod events;
pub mod fetch;
pub mod fonts;
pub mod live;
pub mod node;
pub mod observers;
pub mod platform;
pub mod range_rects;
pub mod reveal;
pub mod slot;
pub mod svg;
pub mod template;
pub mod traversal;
pub mod window;
pub mod xhr;

use boa_engine::{
    Context, Finalize, JsData, JsError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    Trace, js_string,
    native_function::NativeFunctionPointer,
    property::{Attribute, PropertyDescriptor},
};
use verve::native::{self, arg, function, get, js_text, text_of, type_error};

use crate::dom::NodeId;
use crate::state::shared;

/// A method: its name, how many arguments it declares, and the native.
pub type Method = (&'static str, usize, NativeFunctionPointer);
/// Runs the native `f` in a microtask, through the global `queueMicrotask`. If a script has
/// broken that there is no one to tell, and nothing else to do: the work is dropped.
/// The runtime's own microtask queue is not reachable from here, so a script that has replaced
/// `queueMicrotask` is what decides whether this runs.
pub fn queue_microtask(context: &mut Context, name: &str, f: NativeFunctionPointer) {
    let task = function(context, name, 0, f);
    let queue = context
        .global_object()
        .get(js_string!("queueMicrotask"), context);
    if let Ok(queue) = queue {
        let _ = verve::call(context, &queue, &JsValue::undefined(), &[task.into()]);
    }
}

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
    /// A `CanvasRenderingContext2D`, by the canvas it draws into.
    #[cfg(feature = "canvas")]
    Context2d,
    /// A `CanvasGradient`, by its number.
    #[cfg(feature = "canvas")]
    Gradient,
    /// A `CanvasPattern`, by its number.
    #[cfg(feature = "canvas")]
    Pattern,
    /// A `Path2D`, by its number.
    #[cfg(feature = "canvas")]
    Path,
    /// An `ImageBitmap`, by its number.
    #[cfg(feature = "canvas")]
    Bitmap,
    /// An `OffscreenCanvas`, by its canvas's number.
    #[cfg(feature = "canvas")]
    Offscreen,
    /// An `ImageBitmapRenderingContext`, by the canvas it shows.
    #[cfg(feature = "canvas")]
    BitmapRenderer,
    /// An instance whose state is all in its own properties.
    Plain,
}

#[derive(Debug, Trace, Finalize, JsData)]
pub struct Handle {
    #[unsafe_ignore_trace]
    pub kind: Kind,
    #[unsafe_ignore_trace]
    pub id: u32,
    /// For a computed style: the part of the element, if any, that it is about.
    #[unsafe_ignore_trace]
    pub target: computed::Target,
}

pub fn handle(value: &JsValue) -> Option<(Kind, u32)> {
    let object = value.as_object()?;
    let handle = object.downcast_ref::<Handle>()?;
    Some((handle.kind, handle.id))
}

pub fn handle_of(value: &JsValue, kind: Kind) -> Option<u32> {
    handle(value).filter(|(k, _)| *k == kind).map(|(_, id)| id)
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
    Ok(Some(text_of(&value, context)?))
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

/// One WebIDL interface as the DOM has it: unlike `verve::native::PlainClass` its methods and
/// accessors are listed (enumerable), it carries a `Symbol.toStringTag`, it extends a class made
/// here by name, and it is remembered in the document's registry.
pub struct WebIdlClass<'a> {
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

/// Makes an interface and remembers it by name.
pub fn webidl_class(
    context: &mut Context,
    spec: WebIdlClass<'_>,
) -> JsResult<(JsObject, JsObject)> {
    let parent = spec.parent.map(|name| {
        crate::state::class_of(context, name)
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
            .value(js_text(spec.name))
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    let world = verve::current_world(context);
    shared(context)
        .borrow_mut()
        .classes
        .insert((world, spec.name), (constructor.clone(), prototype.clone()));
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
pub fn handle_instance(
    new_target: &JsValue,
    kind: Kind,
    id: u32,
    context: &mut Context,
) -> JsResult<JsObject> {
    let prototype = prototype_of_target(new_target, context)?;
    Ok(JsObject::from_proto_and_data(
        prototype,
        Handle {
            kind,
            id,
            target: computed::Target::default(),
        },
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
    let prototype = crate::state::class_prototype(context, class);
    JsObject::from_proto_and_data(
        prototype,
        Handle {
            kind,
            id,
            target: computed::Target::default(),
        },
    )
}

/// The constructor of a class made here, by name.
pub fn constructor_of(context: &Context, name: &str) -> Option<JsObject> {
    crate::state::class_of(context, name).map(|(c, _)| c)
}

/// Sets `(key, value)` pairs on a fresh object: an event's `init`, a rect.
pub fn dictionary(context: &mut Context, pairs: &[(&str, JsValue)]) -> JsResult<JsObject> {
    let object = native::object(context);
    for (key, value) in pairs {
        object.set(JsString::from(*key), value.clone(), false, context)?;
    }
    Ok(object)
}
