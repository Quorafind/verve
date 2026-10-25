//! The web's events, without a document: `EventTarget`, `Event` and the events derived from it
//! (`CustomEvent`, `MessageEvent`, `ErrorEvent`, `CloseEvent`, `ProgressEvent`,
//! `PromiseRejectionEvent`), `DOMException`, `AbortController` and `AbortSignal`, and
//! `reportError`.
//!
//! Each class keeps its state in Rust, on the object itself, so `class Bus extends EventTarget`
//! works as it does in a browser: `super()` makes the object on the subclass's prototype, with
//! the state on it. As in a browser, they cannot be called without `new`.
//!
//! There is no tree here, so an event is heard only by the target it is dispatched to: its
//! capturing listeners first, then the rest, as at the target in a page.
//!
//! A host with a document builds its tree on these classes rather than beside them: its nodes
//! are targets made by [`host_target`], [`set_event_parent`] tells a dispatch what each one is
//! in — so an event is captured down that path, heard at its target and bubbles back up, as in a
//! page — and [`install_window_target`] makes the global object the target at the top of it.
//! The runtime's own targets — `AbortSignal`, `MessagePort`, `WebSocket` — are the same classes,
//! so `signal instanceof EventTarget` holds in a document as anywhere. A host may also put
//! classes of its own in place of the globals; a target here hears an event of those by its
//! `type`.

use std::cell::Cell;
use std::rc::Rc;

use boa_engine::{
    Context, Finalize, JsData, JsError, JsObject, JsResult, JsString, JsSymbol, JsValue,
    NativeFunction, Trace, js_string,
    native_function::NativeFunctionPointer,
    object::FunctionObjectBuilder,
    property::{Attribute, PropertyDescriptor},
};
use boa_gc::{Gc, GcRefCell};

use crate::native::{arg, closure, function, get, hidden, js_text, object, text_of, type_error};

// ─── The state kept on each object ──────────────────────────────────────────────────────────

/// One listener added to a target. Shared between the target's list and a dispatch going on,
/// so one taken off mid-dispatch is not called after.
#[derive(Trace, Finalize)]
pub(crate) struct Listener {
    kind: String,
    callback: JsValue,
    capture: bool,
    once: bool,
    passive: bool,
    /// An `on…` attribute's place in the list: what it calls is the attribute's value then.
    handler: bool,
    removed: Cell<bool>,
}

/// What a target is, beyond its listeners.
#[derive(Trace, Finalize)]
pub(crate) enum Kind {
    Plain,
    Signal {
        aborted: bool,
        reason: JsValue,
        /// The listeners added with this signal: taken off their targets when it aborts.
        removals: Vec<(JsObject, Gc<Listener>)>,
        /// The signals `AbortSignal.any` made from this one.
        dependents: Vec<JsObject>,
    },
    Port {
        other: Option<JsObject>,
        started: bool,
        closed: bool,
        /// Messages that came before the port was started.
        queue: Vec<JsValue>,
    },
    Channel {
        name: String,
        closed: bool,
    },
    Socket {
        url: String,
        state: u16,
        binary_type: String,
        buffered: f64,
    },
    /// A host's own target — a node of its document — by the number the host knows it by, with
    /// the objects the host keeps alive with it: a node's parent and children, so a tree a
    /// script holds any part of stays whole.
    Host { id: u64, held: Vec<JsObject> },
}

#[derive(Trace, Finalize, JsData)]
pub(crate) struct TargetData {
    listeners: GcRefCell<Vec<Gc<Listener>>>,
    /// The `on…` attributes set, by event type.
    handlers: GcRefCell<Vec<(String, JsValue)>>,
    pub(crate) kind: GcRefCell<Kind>,
}

impl TargetData {
    pub(crate) fn new(kind: Kind) -> Self {
        TargetData {
            listeners: GcRefCell::new(Vec::new()),
            handlers: GcRefCell::new(Vec::new()),
            kind: GcRefCell::new(kind),
        }
    }
}

#[derive(Trace, Finalize)]
struct EventState {
    kind: String,
    bubbles: bool,
    cancelable: bool,
    composed: bool,
    canceled: bool,
    stop: bool,
    stop_now: bool,
    target: JsValue,
    current: JsValue,
    phase: u8,
    time: f64,
    dispatching: bool,
    /// In a passive listener, whose `preventDefault` does nothing.
    passive: bool,
    /// Dispatched by the host for something a person did, not by a script.
    trusted: bool,
    /// The targets the event goes through while it is dispatched, target first.
    path: Vec<JsValue>,
    /// What an event derived from `Event` adds: `detail`, `data`, `code`…
    fields: Vec<(String, JsValue)>,
}

#[derive(Trace, Finalize, JsData)]
struct EventData(GcRefCell<EventState>);

#[derive(Trace, Finalize, JsData)]
pub(crate) struct DomExceptionData {
    pub(crate) name: String,
    pub(crate) message: String,
}

/// The realm's own classes, held apart from the globals the host may replace.
pub(crate) struct Dom {
    pub(crate) event_target: JsObject,
    pub(crate) event: JsObject,
    pub(crate) custom_event: JsObject,
    pub(crate) message_event: JsObject,
    pub(crate) error_event: JsObject,
    pub(crate) close_event: JsObject,
    pub(crate) progress_event: JsObject,
    pub(crate) rejection_event: JsObject,
    pub(crate) dom_exception: JsObject,
    pub(crate) abort_controller: JsObject,
    pub(crate) abort_signal: JsObject,
    pub(crate) abort_signal_proto: JsObject,
    /// What `crate::messaging` adds: `MessageChannel`, `MessagePort`, `BroadcastChannel`,
    /// `WebSocket`, and the open broadcast channels.
    pub(crate) messaging: crate::messaging::Messaging,
}

/// The realm's classes, made the first time any of them is needed.
pub(crate) fn dom(context: &mut Context) -> JsResult<Rc<Dom>> {
    if let Some(dom) = context.get_data::<Rc<Dom>>() {
        return Ok(dom.clone());
    }
    let dom = Rc::new(make_classes(context)?);
    context.insert_data(dom.clone());
    Ok(dom)
}

// ─── Making classes ─────────────────────────────────────────────────────────────────────────

/// What a web class's constructor does with the prototype the object is to have (the
/// subclass's, under `extends`) and the arguments: makes the object.
pub type Make = fn(&JsObject, &[JsValue], &mut Context) -> JsResult<JsObject>;

#[derive(Clone, Copy)]
struct MakeBox(Make, &'static str);

impl Finalize for MakeBox {}
// SAFETY: a function pointer and a static string hold no garbage-collected value.
unsafe impl Trace for MakeBox {
    boa_gc::empty_trace!();
}

/// A class as the web has them: constructed only with `new`, made by `make` on the prototype
/// `new.target` names. Answers its constructor and prototype.
pub fn web_class(
    context: &mut Context,
    name: &'static str,
    length: usize,
    make: Make,
    parent: Option<(&JsObject, &JsObject)>,
    methods: &[(&str, usize, NativeFunctionPointer)],
    accessors: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)],
) -> JsResult<(JsObject, JsObject)> {
    let prototype = object(context);
    if let Some((_, parent_proto)) = parent {
        prototype.set_prototype(Some(parent_proto.clone()));
    }
    let constructor: JsObject = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, args, (prototype, make): &(JsObject, MakeBox), context| {
                let Some(new_target) = this.as_object() else {
                    return Err(type_error(format!(
                        "Failed to construct '{}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
                        make.1
                    )));
                };
                let own = new_target.get(js_string!("prototype"), context)?;
                let own = own.as_object().unwrap_or_else(|| prototype.clone());
                Ok((make.0)(&own, args, context)?.into())
            },
            (prototype.clone(), MakeBox(make, name)),
        ),
    )
    .name(JsString::from(name))
    .length(length)
    .constructor(true)
    .build()
    .into();
    if let Some((parent, _)) = parent {
        constructor.set_prototype(Some(parent.clone()));
    }
    crate::native::methods(context, &prototype, methods)?;
    crate::native::accessors(context, &prototype, accessors)?;
    hidden(
        &prototype,
        js_string!("constructor"),
        constructor.clone(),
        context,
    )?;
    prototype.define_property_or_throw(
        JsSymbol::to_string_tag(),
        PropertyDescriptor::builder()
            .value(js_text(name))
            .writable(false)
            .enumerable(false)
            .configurable(true),
        context,
    )?;
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    Ok((constructor, prototype))
}

fn illegal(_: &JsObject, _: &[JsValue], _: &mut Context) -> JsResult<JsObject> {
    Err(type_error("Illegal constructor"))
}

/// Constants on a class and its prototype, as `Event.AT_TARGET` and `event.AT_TARGET` both are.
fn constants(context: &mut Context, targets: &[&JsObject], list: &[(&str, u32)]) -> JsResult<()> {
    for target in targets {
        for (name, value) in list {
            target.define_property_or_throw(
                JsString::from(*name),
                PropertyDescriptor::builder()
                    .value(*value)
                    .writable(false)
                    .enumerable(true)
                    .configurable(false),
                context,
            )?;
        }
    }
    Ok(())
}

fn make_classes(context: &mut Context) -> JsResult<Dom> {
    let (event_target, event_target_proto) = web_class(
        context,
        "EventTarget",
        0,
        |prototype, _, _| {
            Ok(JsObject::from_proto_and_data(
                prototype.clone(),
                TargetData::new(Kind::Plain),
            ))
        },
        None,
        TARGET_METHODS,
        &[],
    )?;
    let (event, event_proto) = web_class(
        context,
        "Event",
        1,
        |p, a, cx| make_event(p, a, &[], cx),
        None,
        EVENT_METHODS,
        EVENT_ACCESSORS,
    )?;
    constants(
        context,
        &[&event, &event_proto],
        &[
            ("NONE", 0),
            ("CAPTURING_PHASE", 1),
            ("AT_TARGET", 2),
            ("BUBBLING_PHASE", 3),
        ],
    )?;
    let base = Some((&event, &event_proto));
    let (custom_event, _) = web_class(
        context,
        "CustomEvent",
        1,
        |p, a, cx| make_event(p, a, &[("detail", JsValue::null())], cx),
        base,
        &[("initCustomEvent", 4, |this, args, cx| {
            let kind = text_of(&arg(args, 0), cx)?;
            with_event(this, |state| {
                if !state.dispatching {
                    state.kind = kind;
                    state.bubbles = arg(args, 1).to_boolean();
                    state.cancelable = arg(args, 2).to_boolean();
                    set_field(state, "detail", arg(args, 3));
                }
            })?;
            Ok(JsValue::undefined())
        })],
        &[("detail", |this, _, _| field(this, "detail"), None)],
    )?;
    let (message_event, _) = web_class(
        context,
        "MessageEvent",
        1,
        |p, a, cx| {
            let ports = crate::native::array(cx, Vec::new());
            make_event(
                p,
                a,
                &[
                    ("data", JsValue::null()),
                    ("origin", js_text("")),
                    ("lastEventId", js_text("")),
                    ("source", JsValue::null()),
                    ("ports", ports),
                ],
                cx,
            )
        },
        base,
        &[],
        &[
            ("data", |this, _, _| field(this, "data"), None),
            ("origin", |this, _, _| field(this, "origin"), None),
            ("lastEventId", |this, _, _| field(this, "lastEventId"), None),
            ("source", |this, _, _| field(this, "source"), None),
            ("ports", |this, _, _| field(this, "ports"), None),
        ],
    )?;
    let (error_event, _) = web_class(
        context,
        "ErrorEvent",
        1,
        |p, a, cx| {
            make_event(
                p,
                a,
                &[
                    ("message", js_text("")),
                    ("filename", js_text("")),
                    ("lineno", 0.into()),
                    ("colno", 0.into()),
                    ("error", JsValue::undefined()),
                ],
                cx,
            )
        },
        base,
        &[],
        &[
            ("message", |this, _, _| field(this, "message"), None),
            ("filename", |this, _, _| field(this, "filename"), None),
            ("lineno", |this, _, _| field(this, "lineno"), None),
            ("colno", |this, _, _| field(this, "colno"), None),
            ("error", |this, _, _| field(this, "error"), None),
        ],
    )?;
    let (close_event, _) = web_class(
        context,
        "CloseEvent",
        1,
        |p, a, cx| {
            make_event(
                p,
                a,
                &[
                    ("wasClean", false.into()),
                    ("code", 0.into()),
                    ("reason", js_text("")),
                ],
                cx,
            )
        },
        base,
        &[],
        &[
            ("wasClean", |this, _, _| field(this, "wasClean"), None),
            ("code", |this, _, _| field(this, "code"), None),
            ("reason", |this, _, _| field(this, "reason"), None),
        ],
    )?;
    let (progress_event, _) = web_class(
        context,
        "ProgressEvent",
        1,
        |p, a, cx| {
            make_event(
                p,
                a,
                &[
                    ("lengthComputable", false.into()),
                    ("loaded", 0.into()),
                    ("total", 0.into()),
                ],
                cx,
            )
        },
        base,
        &[],
        &[
            (
                "lengthComputable",
                |this, _, _| field(this, "lengthComputable"),
                None,
            ),
            ("loaded", |this, _, _| field(this, "loaded"), None),
            ("total", |this, _, _| field(this, "total"), None),
        ],
    )?;
    let (rejection_event, _) = web_class(
        context,
        "PromiseRejectionEvent",
        2,
        |p, a, cx| {
            make_event(
                p,
                a,
                &[
                    ("promise", JsValue::undefined()),
                    ("reason", JsValue::undefined()),
                ],
                cx,
            )
        },
        base,
        &[],
        &[
            ("promise", |this, _, _| field(this, "promise"), None),
            ("reason", |this, _, _| field(this, "reason"), None),
        ],
    )?;

    // DOMException: an Error, as far as `instanceof` and the console can tell.
    let error = context.intrinsics().constructors().error().constructor();
    let error_proto = context.intrinsics().constructors().error().prototype();
    let (dom_exception, dom_exception_proto) = web_class(
        context,
        "DOMException",
        0,
        make_dom_exception,
        Some((&error, &error_proto)),
        &[],
        &[
            (
                "name",
                |this, _, _| exception_part(this, |d| js_text(&d.name)),
                None,
            ),
            (
                "message",
                |this, _, _| exception_part(this, |d| js_text(&d.message)),
                None,
            ),
            (
                "code",
                |this, _, _| exception_part(this, |d| exception_code(&d.name).into()),
                None,
            ),
        ],
    )?;
    let codes: Vec<(&str, u32)> = LEGACY_CONSTANTS.to_vec();
    constants(context, &[&dom_exception, &dom_exception_proto], &codes)?;

    let target = Some((&event_target, &event_target_proto));
    let (abort_signal, abort_signal_proto) = web_class(
        context,
        "AbortSignal",
        0,
        illegal,
        target,
        SIGNAL_METHODS,
        SIGNAL_ACCESSORS,
    )?;
    handler_accessors(context, &abort_signal_proto, &["abort"])?;
    crate::native::methods(context, &abort_signal, SIGNAL_STATICS)?;
    let (abort_controller, _) = web_class(
        context,
        "AbortController",
        0,
        |prototype, _, cx| {
            let controller = object(cx);
            controller.set_prototype(Some(prototype.clone()));
            let signal = new_signal(cx)?;
            hidden(&controller, js_string!("__signal"), signal, cx)?;
            Ok(controller)
        },
        None,
        &[("abort", 1, |this, args, cx| {
            if let Some(signal) = get(this, "__signal", cx)?.as_object() {
                abort(&signal, arg(args, 0), cx)?;
            }
            Ok(JsValue::undefined())
        })],
        &[("signal", |this, _, cx| get(this, "__signal", cx), None)],
    )?;
    let messaging = crate::messaging::make_classes(context, &event_target, &event_target_proto)?;
    Ok(Dom {
        event_target,
        event,
        custom_event,
        message_event,
        error_event,
        close_event,
        progress_event,
        rejection_event,
        dom_exception,
        abort_controller,
        abort_signal,
        abort_signal_proto,
        messaging,
    })
}

// ─── Event ──────────────────────────────────────────────────────────────────────────────────

fn flag(init: &JsValue, name: &str, context: &mut Context) -> JsResult<bool> {
    Ok(get(init, name, context)?.to_boolean())
}

/// `new Event(type, init)` — and the events derived from it, whose own `fields` are read from
/// the same `init`, each defaulting to what is given.
fn make_event(
    prototype: &JsObject,
    args: &[JsValue],
    fields: &[(&str, JsValue)],
    context: &mut Context,
) -> JsResult<JsObject> {
    if args.is_empty() {
        return Err(type_error(
            "Failed to construct 'Event': 1 argument required, but only 0 present.",
        ));
    }
    let kind = text_of(&arg(args, 0), context)?;
    let init = arg(args, 1);
    let mut own = Vec::with_capacity(fields.len());
    for (name, default) in fields {
        let given = get(&init, name, context)?;
        own.push((
            (*name).to_owned(),
            if given.is_undefined() {
                default.clone()
            } else {
                given
            },
        ));
    }
    let state = EventState {
        kind,
        bubbles: flag(&init, "bubbles", context)?,
        cancelable: flag(&init, "cancelable", context)?,
        composed: flag(&init, "composed", context)?,
        canceled: false,
        stop: false,
        stop_now: false,
        target: JsValue::null(),
        current: JsValue::null(),
        phase: 0,
        time: crate::now(context),
        dispatching: false,
        passive: false,
        trusted: false,
        path: Vec::new(),
        fields: own,
    };
    Ok(JsObject::from_proto_and_data(
        prototype.clone(),
        EventData(GcRefCell::new(state)),
    ))
}

fn with_event<R>(this: &JsValue, f: impl FnOnce(&mut EventState) -> R) -> JsResult<R> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    let data = object
        .downcast_ref::<EventData>()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    let mut state = data.0.borrow_mut();
    Ok(f(&mut state))
}

fn field(this: &JsValue, name: &str) -> JsResult<JsValue> {
    with_event(this, |state| {
        state
            .fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    })
}

fn set_field(state: &mut EventState, name: &str, value: JsValue) {
    match state.fields.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = value,
        None => state.fields.push((name.to_owned(), value)),
    }
}

fn cancel(state: &mut EventState) {
    if state.cancelable && !state.passive {
        state.canceled = true;
    }
}

const EVENT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("preventDefault", 0, |this, _, _| {
        with_event(this, cancel)?;
        Ok(JsValue::undefined())
    }),
    ("stopPropagation", 0, |this, _, _| {
        with_event(this, |s| s.stop = true)?;
        Ok(JsValue::undefined())
    }),
    ("stopImmediatePropagation", 0, |this, _, _| {
        with_event(this, |s| {
            s.stop = true;
            s.stop_now = true;
        })?;
        Ok(JsValue::undefined())
    }),
    ("composedPath", 0, |this, _, cx| {
        let items = with_event(this, |s| {
            if s.dispatching {
                s.path.clone()
            } else {
                Vec::new()
            }
        })?;
        Ok(crate::native::array(cx, items))
    }),
    ("initEvent", 3, |this, args, cx| {
        let kind = text_of(&arg(args, 0), cx)?;
        with_event(this, |s| {
            if !s.dispatching {
                s.kind = kind;
                s.bubbles = arg(args, 1).to_boolean();
                s.cancelable = arg(args, 2).to_boolean();
                s.canceled = false;
                s.stop = false;
                s.stop_now = false;
            }
        })?;
        Ok(JsValue::undefined())
    }),
];

const EVENT_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    (
        "type",
        |this, _, _| with_event(this, |s| js_text(&s.kind)),
        None,
    ),
    (
        "bubbles",
        |this, _, _| with_event(this, |s| s.bubbles.into()),
        None,
    ),
    (
        "cancelable",
        |this, _, _| with_event(this, |s| s.cancelable.into()),
        None,
    ),
    (
        "composed",
        |this, _, _| with_event(this, |s| s.composed.into()),
        None,
    ),
    (
        "defaultPrevented",
        |this, _, _| with_event(this, |s| s.canceled.into()),
        None,
    ),
    (
        "target",
        |this, _, _| with_event(this, |s| s.target.clone()),
        None,
    ),
    (
        "srcElement",
        |this, _, _| with_event(this, |s| s.target.clone()),
        None,
    ),
    (
        "currentTarget",
        |this, _, _| with_event(this, |s| s.current.clone()),
        None,
    ),
    (
        "eventPhase",
        |this, _, _| with_event(this, |s| u32::from(s.phase).into()),
        None,
    ),
    (
        "timeStamp",
        |this, _, _| with_event(this, |s| s.time.into()),
        None,
    ),
    (
        "isTrusted",
        |this, _, _| with_event(this, |s| s.trusted.into()),
        None,
    ),
    (
        "returnValue",
        |this, _, _| with_event(this, |s| (!s.canceled).into()),
        Some(|this, args, _| {
            if !arg(args, 0).to_boolean() {
                with_event(this, cancel)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
    (
        "cancelBubble",
        |this, _, _| with_event(this, |s| s.stop.into()),
        Some(|this, args, _| {
            if arg(args, 0).to_boolean() {
                with_event(this, |s| s.stop = true)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
];

/// An `Event` of the realm's own class: what the runtime's targets fire.
pub(crate) fn new_event(context: &mut Context, kind: &str) -> JsResult<JsObject> {
    let class = dom(context)?.event.clone();
    class.construct(&[js_text(kind)], None, context)
}

/// An event of `class` (one of [`Dom`]'s) with `init`'s fields.
pub(crate) fn new_event_of(
    context: &mut Context,
    class: &JsObject,
    kind: &str,
    init: &[(&str, JsValue)],
) -> JsResult<JsObject> {
    let dict = object(context);
    for (key, value) in init {
        dict.set(JsString::from(*key), value.clone(), false, context)?;
    }
    class.construct(&[js_text(kind), dict.into()], None, context)
}

// ─── EventTarget ────────────────────────────────────────────────────────────────────────────

/// The window's own listeners, once a host made the global object a target; see
/// [`install_window_target`].
struct WindowTarget(JsObject);

/// The object holding `this`'s listeners: `this` itself, or — for the global object, or no
/// `this` at all, as `addEventListener(…)` called bare has — the window's, when there is one.
fn holder(this: &JsValue, context: &Context) -> Option<JsObject> {
    match this.as_object() {
        Some(object) if object.downcast_ref::<TargetData>().is_some() => Some(object.clone()),
        Some(object) => context
            .get_data::<WindowTarget>()
            .filter(|_| JsObject::equals(&object, &context.global_object()))
            .map(|window| window.0.clone()),
        None if this.is_undefined() => context.get_data::<WindowTarget>().map(|w| w.0.clone()),
        None => None,
    }
}

/// [`with_target`], for a `this` that may be the window.
fn with_holder<R>(
    this: &JsValue,
    context: &Context,
    f: impl FnOnce(&TargetData) -> R,
) -> JsResult<R> {
    let object = holder(this, context).ok_or_else(|| type_error("Illegal invocation"))?;
    with_target(&object.into(), f)
}

/// Runs `f` with the state of the target `this`; a `TypeError` when it is not one.
pub(crate) fn with_target<R>(this: &JsValue, f: impl FnOnce(&TargetData) -> R) -> JsResult<R> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    let data = object
        .downcast_ref::<TargetData>()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    Ok(f(&data))
}

/// The `options` of `addEventListener`: a boolean (capture), or `{ capture, once, passive,
/// signal }`.
fn listener_options(
    options: &JsValue,
    context: &mut Context,
) -> JsResult<(bool, bool, bool, JsValue)> {
    if options.is_object() {
        Ok((
            flag(options, "capture", context)?,
            flag(options, "once", context)?,
            flag(options, "passive", context)?,
            get(options, "signal", context)?,
        ))
    } else {
        Ok((options.to_boolean(), false, false, JsValue::undefined()))
    }
}

fn add_listener(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = text_of(&arg(args, 0), context)?;
    let callback = arg(args, 1);
    let (capture, once, passive, signal) = listener_options(&arg(args, 2), context)?;
    if callback.is_null_or_undefined() {
        return Ok(JsValue::undefined());
    }
    if !callback.is_object() {
        return Err(type_error(
            "Failed to execute 'addEventListener' on 'EventTarget': The callback provided as parameter 2 is not an object.",
        ));
    }
    if signal.is_object() && get(&signal, "aborted", context)?.to_boolean() {
        return Ok(JsValue::undefined());
    }
    let listener = with_holder(this, context, |data| {
        let mut listeners = data.listeners.borrow_mut();
        let present = listeners.iter().any(|l| {
            !l.handler && l.kind == kind && l.capture == capture && l.callback == callback
        });
        if present {
            return None;
        }
        let listener = Gc::new(Listener {
            kind,
            callback,
            capture,
            once,
            passive,
            handler: false,
            removed: Cell::new(false),
        });
        listeners.push(listener.clone());
        Some(listener)
    })?;
    let Some(listener) = listener else {
        return Ok(JsValue::undefined());
    };
    if let Some(signal) = signal.as_object() {
        let target = holder(this, context).expect("a target");
        follow_signal(&signal, target, listener, context)?;
    }
    Ok(JsValue::undefined())
}

/// Takes `listener` off `target` when `signal` aborts.
fn follow_signal(
    signal: &JsObject,
    target: JsObject,
    listener: Gc<Listener>,
    context: &mut Context,
) -> JsResult<()> {
    if let Some(data) = signal.downcast_ref::<TargetData>()
        && let Kind::Signal { removals, .. } = &mut *data.kind.borrow_mut()
    {
        removals.push((target, listener));
        return Ok(());
    }
    // A signal of some other kind: heard as anything with `addEventListener` is.
    let remove = closure(
        context,
        "",
        0,
        |_, _, (target, kind, callback, capture): &(JsObject, JsString, JsValue, bool), cx| {
            let options = object(cx);
            options.set(js_string!("capture"), *capture, false, cx)?;
            remove_listener(
                &target.clone().into(),
                &[kind.clone().into(), callback.clone(), options.into()],
                cx,
            )
        },
        (
            target,
            JsString::from(listener.kind.as_str()),
            listener.callback.clone(),
            listener.capture,
        ),
    );
    let once = object(context);
    once.set(js_string!("once"), true, false, context)?;
    crate::native::invoke(
        &signal.clone().into(),
        "addEventListener",
        &[js_text("abort"), remove.into(), once.into()],
        context,
    )?;
    Ok(())
}

fn remove_listener(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let kind = text_of(&arg(args, 0), context)?;
    let callback = arg(args, 1);
    let (capture, ..) = listener_options(&arg(args, 2), context)?;
    with_holder(this, context, |data| {
        data.listeners.borrow_mut().retain(|l| {
            let same =
                !l.handler && l.kind == kind && l.capture == capture && l.callback == callback;
            if same {
                l.removed.set(true);
            }
            !same
        })
    })?;
    Ok(JsValue::undefined())
}

fn take_off(target: &JsObject, listener: &Gc<Listener>) {
    listener.removed.set(true);
    if let Some(data) = target.downcast_ref::<TargetData>() {
        data.listeners
            .borrow_mut()
            .retain(|l| !Gc::ptr_eq(l, listener));
    }
}

/// What a host with a tree says an event target is in: the next target on an event's path
/// (a node's parent; the window, for its document), or `None` at the top. See
/// [`set_event_parent`].
pub type EventParent = fn(&JsValue, &JsObject, &mut Context) -> JsResult<Option<JsValue>>;

#[derive(Clone, Copy)]
struct ParentHook(EventParent);

/// Dispatches `event` at `target`: captured down the path to it from the top (see
/// [`set_event_parent`]; with no tree the path is the target alone), heard at the target —
/// capturing listeners first — and, when it bubbles, back up the path; each listener's error
/// reported and the rest still called. Answers whether nothing cancelled it.
pub(crate) fn dispatch(
    target: &JsObject,
    event: &JsObject,
    context: &mut Context,
) -> JsResult<bool> {
    dispatch_value(&target.clone().into(), event, context)
}

/// [`dispatch`], at a target that may be the window (the global object).
pub fn dispatch_value(target: &JsValue, event: &JsObject, context: &mut Context) -> JsResult<bool> {
    let ours = event.downcast_ref::<EventData>().is_some();
    if !ours {
        // An event made by the host's classes: heard by its type, at its target alone, its own
        // state its own.
        let kind = text_of(&event.get(js_string!("type"), context)?, context)?;
        if let Some(holder) = holder(target, context) {
            invoke(&holder, target, &kind, event, None, false, context)?;
        }
        return Ok(!get(&event.clone().into(), "defaultPrevented", context)?.to_boolean());
    }
    let mut path = vec![target.clone()];
    if let Some(hook) = context.get_data::<ParentHook>().copied() {
        let mut at = target.clone();
        while let Some(parent) = (hook.0)(&at, event, context)? {
            // A path never loops, whatever the host answers.
            if path.iter().any(|p| p == &parent) || path.len() > 4096 {
                break;
            }
            path.push(parent.clone());
            at = parent;
        }
    }
    let (kind, bubbles) = {
        let data = event.downcast_ref::<EventData>().expect("an event");
        let mut state = data.0.borrow_mut();
        if state.dispatching {
            drop(state);
            return Err(dom_exception_error(
                context,
                "The event is already being dispatched.",
                "InvalidStateError",
            ));
        }
        state.dispatching = true;
        state.target = target.clone();
        state.path = path.clone();
        state.stop = false;
        state.stop_now = false;
        (state.kind.clone(), state.bubbles)
    };
    // Capturing, from the top down to the target's parent; then the target; then bubbling.
    let mut steps: Vec<(usize, u8, Option<bool>)> = Vec::with_capacity(path.len() * 2);
    for at in (1..path.len()).rev() {
        steps.push((at, 1, Some(true)));
    }
    steps.push((0, 2, None));
    if bubbles {
        for at in 1..path.len() {
            steps.push((at, 3, Some(false)));
        }
    }
    let mut outcome = Ok(());
    for (at, phase, capture) in steps {
        let this = &path[at];
        let Some(holder) = holder(this, context) else {
            continue;
        };
        {
            let data = event.downcast_ref::<EventData>().expect("an event");
            let mut state = data.0.borrow_mut();
            if state.stop {
                break;
            }
            state.current = this.clone();
            state.phase = phase;
        }
        match invoke(&holder, this, &kind, event, capture, true, context) {
            Ok(false) => {}
            Ok(true) => break,
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    let data = event.downcast_ref::<EventData>().expect("an event");
    let mut state = data.0.borrow_mut();
    state.dispatching = false;
    state.current = JsValue::null();
    state.phase = 0;
    state.stop = false;
    state.stop_now = false;
    state.path.clear();
    outcome?;
    Ok(!state.canceled)
}

/// The listeners for `kind` on `holder` (the object `this` keeps its listeners on) called with
/// `event`: only the capturing ones or only the others when `capture` says, both — capturing
/// first — at the target. Answers whether `stopImmediatePropagation` was called.
fn invoke(
    holder: &JsObject,
    this: &JsValue,
    kind: &str,
    event: &JsObject,
    capture: Option<bool>,
    ours: bool,
    context: &mut Context,
) -> JsResult<bool> {
    let listeners: Vec<Gc<Listener>> = match holder.downcast_ref::<TargetData>() {
        Some(data) => data
            .listeners
            .borrow()
            .iter()
            .filter(|l| l.kind == kind && capture.is_none_or(|c| c == l.capture))
            .cloned()
            .collect(),
        None => return Ok(false),
    };
    if listeners.is_empty() {
        return Ok(false);
    }
    for pass in [true, false] {
        for listener in listeners.iter().filter(|l| l.capture == pass) {
            if listener.removed.get() {
                continue;
            }
            if listener.once {
                take_off(holder, listener);
            }
            if ours {
                let data = event.downcast_ref::<EventData>().expect("an event");
                data.0.borrow_mut().passive = listener.passive;
            }
            let outcome = if listener.handler {
                call_handler(holder, this, kind, event, context)
            } else if listener.callback.is_callable() {
                crate::call(context, &listener.callback, this, &[event.clone().into()])
            } else {
                // An object with `handleEvent`, called on that object.
                match get(&listener.callback, "handleEvent", context) {
                    Ok(handle) if handle.is_callable() => crate::call(
                        context,
                        &handle,
                        &listener.callback,
                        &[event.clone().into()],
                    ),
                    Ok(_) => Err(type_error("The listener's handleEvent is not a function")),
                    Err(error) => Err(error),
                }
            };
            match outcome {
                Err(error) => crate::report(context, &error),
                // An `async` listener that fails says so, as one that throws does.
                Ok(value) => report_rejection(context, &value),
            }
            if ours {
                let data = event.downcast_ref::<EventData>().expect("an event");
                let mut state = data.0.borrow_mut();
                state.passive = false;
                if state.stop_now {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// When `value` is a promise, its rejection reported as an uncaught error.
fn report_rejection(context: &mut Context, value: &JsValue) {
    let Some(object) = value.as_object() else {
        return;
    };
    let Ok(promise) = boa_engine::object::builtins::JsPromise::from_object(object.clone()) else {
        return;
    };
    let report = function(context, "report", 1, |_, args, cx| {
        crate::report(cx, &JsError::from_opaque(arg(args, 0)));
        Ok(JsValue::undefined())
    });
    let _ = promise.then(
        None,
        Some(boa_engine::object::builtins::JsFunction::from_object(report).expect("a function")),
        context,
    );
}

/// An `on…` attribute's turn in a dispatch: its value now called, a `false` it answers
/// cancelling the event.
fn call_handler(
    holder: &JsObject,
    this: &JsValue,
    kind: &str,
    event: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let handler = match holder.downcast_ref::<TargetData>() {
        Some(data) => data
            .handlers
            .borrow()
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, h)| h.clone()),
        None => None,
    };
    let Some(handler) = handler.filter(JsValue::is_callable) else {
        return Ok(JsValue::undefined());
    };
    let answer = crate::call(context, &handler, this, &[event.clone().into()])?;
    if answer == JsValue::from(false)
        && let Some(data) = event.downcast_ref::<EventData>()
    {
        cancel(&mut data.0.borrow_mut());
    }
    Ok(answer)
}

/// The `on…` attribute for `kind` on `target`.
pub(crate) fn handler_of(target: &JsValue, kind: &str) -> JsResult<JsValue> {
    with_target(target, |data| handler_in(data, kind))
}

fn handler_in(data: &TargetData, kind: &str) -> JsValue {
    let handlers = data.handlers.borrow();
    handlers
        .iter()
        .find(|(k, _)| k == kind)
        .map(|(_, h)| h.clone())
        .unwrap_or(JsValue::null())
}

/// Sets the `on…` attribute for `kind`: the first time, it takes its place among the listeners.
pub(crate) fn set_handler(target: &JsValue, kind: &str, value: JsValue) -> JsResult<()> {
    with_target(target, |data| set_handler_in(data, kind, value))
}

fn set_handler_in(data: &TargetData, kind: &str, value: JsValue) {
    let value = if value.is_callable() {
        value
    } else {
        JsValue::null()
    };
    let mut handlers = data.handlers.borrow_mut();
    match handlers.iter_mut().find(|(k, _)| k == kind) {
        Some(slot) => slot.1 = value,
        None => {
            handlers.push((kind.to_owned(), value));
            data.listeners.borrow_mut().push(Gc::new(Listener {
                kind: kind.to_owned(),
                callback: JsValue::undefined(),
                capture: false,
                once: false,
                passive: false,
                handler: true,
                removed: Cell::new(false),
            }));
        }
    }
}

/// `onabort`, `onmessage`… on a prototype — or on the global object, for the window's — an
/// accessor for each event type in `kinds`.
pub fn handler_accessors(
    context: &mut Context,
    prototype: &JsObject,
    kinds: &[&str],
) -> JsResult<()> {
    for kind in kinds {
        let key = JsString::from(*kind);
        let getter = closure(
            context,
            &format!("get on{kind}"),
            0,
            |this, _, kind: &JsString, cx| {
                with_holder(this, cx, |data| {
                    handler_in(data, &kind.to_std_string_escaped())
                })
            },
            key.clone(),
        );
        let setter = closure(
            context,
            &format!("set on{kind}"),
            1,
            |this, args, kind: &JsString, cx| {
                with_holder(this, cx, |data| {
                    set_handler_in(data, &kind.to_std_string_escaped(), arg(args, 0))
                })?;
                Ok(JsValue::undefined())
            },
            key,
        );
        prototype.define_property_or_throw(
            JsString::from(format!("on{kind}")),
            PropertyDescriptor::builder()
                .get(getter)
                .set(setter)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    Ok(())
}

const TARGET_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("addEventListener", 2, add_listener),
    ("removeEventListener", 2, remove_listener),
    ("dispatchEvent", 1, |this, args, cx| {
        let event = arg(args, 0);
        let event = event
            .as_object()
            .ok_or_else(|| type_error("Failed to execute 'dispatchEvent' on 'EventTarget': parameter 1 is not of type 'Event'."))?;
        if holder(this, cx).is_none() {
            return Err(type_error("Illegal invocation"));
        }
        let this = match this.as_object() {
            Some(object) => object.into(),
            None => cx.global_object().into(),
        };
        Ok(dispatch_value(&this, &event, cx)?.into())
    }),
];

// ─── What a host with a document builds on ──────────────────────────────────────────────────

/// The classes a host's own derive from: `EventTarget`, `Event` and `CustomEvent`, as
/// `(constructor, prototype)`. They are the realm's own, whatever the globals now say.
pub struct Classes {
    pub event_target: (JsObject, JsObject),
    pub event: (JsObject, JsObject),
    pub custom_event: (JsObject, JsObject),
}

/// The realm's [`Classes`].
pub fn classes(context: &mut Context) -> JsResult<Classes> {
    let dom = dom(context)?;
    let prototype = |class: &JsObject, cx: &mut Context| -> JsResult<JsObject> {
        class
            .get(js_string!("prototype"), cx)?
            .as_object()
            .ok_or_else(|| type_error("a class without a prototype"))
    };
    Ok(Classes {
        event_target: (dom.event_target.clone(), prototype(&dom.event_target, context)?),
        event: (dom.event.clone(), prototype(&dom.event, context)?),
        custom_event: (dom.custom_event.clone(), prototype(&dom.custom_event, context)?),
    })
}

/// A class derived from `Event` (or from one derived from it: `parent`), as the web's
/// `MouseEvent` is: made with `new Name(type, init)`, each of `fields` read from `init` with its
/// default, and answered by an accessor of its name. `methods` go on its prototype. Answers
/// `(constructor, prototype)`; the host puts the constructor where it wants it.
pub fn define_event_class(
    context: &mut Context,
    name: &'static str,
    parent: (&JsObject, &JsObject),
    fields: &'static [(&'static str, FieldDefault)],
    methods: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<(JsObject, JsObject)> {
    let (constructor, prototype) = web_class(
        context,
        name,
        1,
        |_, _, _| Err(type_error("Illegal constructor")),
        Some(parent),
        methods,
        &[],
    )?;
    // The constructor `web_class` made calls a plain `Make`, which carries nothing; this class's
    // fields are on its constructor instead, read when it makes an event.
    let fields_list = FieldList(fields);
    let make = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure_with_captures(
            |this, args, (prototype, fields, name): &(JsObject, FieldList, JsString), context| {
                let Some(new_target) = this.as_object() else {
                    return Err(type_error(format!(
                        "Failed to construct '{}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
                        name.to_std_string_escaped()
                    )));
                };
                let own = new_target.get(js_string!("prototype"), context)?;
                let own = own.as_object().unwrap_or_else(|| prototype.clone());
                let defaults: Vec<(&str, JsValue)> = fields
                    .0
                    .iter()
                    .map(|(name, default)| (*name, default.value()))
                    .collect();
                Ok(make_event(&own, args, &defaults, context)?.into())
            },
            (prototype.clone(), fields_list, JsString::from(name)),
        ),
    )
    .name(JsString::from(name))
    .length(1)
    .constructor(true)
    .build();
    let constructor_object: JsObject = make.into();
    constructor_object.set_prototype(Some(parent.0.clone()));
    constructor_object.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype.clone())
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    hidden(
        &prototype,
        js_string!("constructor"),
        constructor_object.clone(),
        context,
    )?;
    let _ = constructor;
    for (field, _) in fields {
        let getter = closure(
            context,
            &format!("get {field}"),
            0,
            |this, _, field: &JsString, _| field_value(this, &field.to_std_string_escaped()),
            JsString::from(*field),
        );
        prototype.define_property_or_throw(
            JsString::from(*field),
            PropertyDescriptor::builder()
                .get(getter)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    Ok((constructor_object, prototype))
}

/// A field's default in [`define_event_class`]: values a `static` can hold.
#[derive(Clone, Copy, Debug)]
pub enum FieldDefault {
    Null,
    Undefined,
    Bool(bool),
    Number(f64),
    Text(&'static str),
}

impl FieldDefault {
    fn value(self) -> JsValue {
        match self {
            Self::Null => JsValue::null(),
            Self::Undefined => JsValue::undefined(),
            Self::Bool(value) => value.into(),
            Self::Number(value) => value.into(),
            Self::Text(value) => js_text(value),
        }
    }
}

#[derive(Clone, Copy)]
struct FieldList(&'static [(&'static str, FieldDefault)]);

impl Finalize for FieldList {}
// SAFETY: static names and plain values hold no garbage-collected value.
unsafe impl Trace for FieldList {
    boa_gc::empty_trace!();
}

/// A field of an event made by one of the realm's event classes (`detail`, `clientX`…).
pub fn field_value(event: &JsValue, name: &str) -> JsResult<JsValue> {
    field(event, name)
}

/// Sets a field of such an event, as the host does to one it is about to dispatch.
pub fn set_field_value(event: &JsValue, name: &str, value: JsValue) -> JsResult<()> {
    with_event(event, |state| set_field(state, name, value))
}

/// Whether `event` is one of the realm's events (made by `Event` or a class derived from it).
pub fn is_event(event: &JsObject) -> bool {
    event.downcast_ref::<EventData>().is_some()
}

/// The event's `type`.
pub fn event_type(event: &JsValue) -> JsResult<String> {
    with_event(event, |state| state.kind.clone())
}

/// Marks an event the host is about to dispatch for something a person did: `isTrusted`.
pub fn set_trusted(event: &JsValue, trusted: bool) -> JsResult<()> {
    with_event(event, |state| state.trusted = trusted)
}

/// Whether a listener called `preventDefault` on `event` (when it could).
pub fn default_prevented(event: &JsValue) -> JsResult<bool> {
    with_event(event, |state| state.canceled)
}

/// A new target on `prototype` that a host knows by `id` — a node of its document. See
/// [`host_id`] and [`hold`].
pub fn host_target(prototype: JsObject, id: u64) -> JsObject {
    JsObject::from_proto_and_data(
        prototype,
        TargetData::new(Kind::Host {
            id,
            held: Vec::new(),
        }),
    )
}

/// The number a host target was made with, when `object` is one.
pub fn host_id(object: &JsObject) -> Option<u64> {
    let data = object.downcast_ref::<TargetData>()?;
    match &*data.kind.borrow() {
        Kind::Host { id, .. } => Some(*id),
        _ => None,
    }
}

/// Changes what a host target keeps alive with it. Does nothing for any other object.
pub fn hold(object: &JsObject, f: impl FnOnce(&mut Vec<JsObject>)) {
    if let Some(data) = object.downcast_ref::<TargetData>()
        && let Kind::Host { held, .. } = &mut *data.kind.borrow_mut()
    {
        f(held);
    }
}

/// Tells every dispatch from now on what each target is in: see [`EventParent`].
pub fn set_event_parent(context: &mut Context, parent: EventParent) {
    context.insert_data(ParentHook(parent));
}

/// Makes the global object an event target — the window: `addEventListener` called on it (or
/// bare) keeps its listeners on a target of the realm's own, which a dispatch whose path reaches
/// the global object calls, with `this` and `currentTarget` the global object. Answers that
/// target. The methods themselves are the host's to put where the window finds them (on its
/// prototype, as a browser's `Window.prototype` inherits them from `EventTarget`).
pub fn install_window_target(context: &mut Context) -> JsResult<JsObject> {
    if let Some(window) = context.get_data::<WindowTarget>() {
        return Ok(window.0.clone());
    }
    let prototype = classes(context)?.event_target.1;
    let target = JsObject::from_proto_and_data(prototype, TargetData::new(Kind::Plain));
    context.insert_data(WindowTarget(target.clone()));
    Ok(target)
}

/// The event types `target` (or the window, for the global object) has listeners for, `on…`
/// attributes included, each once.
pub fn listener_kinds(target: &JsValue, context: &Context) -> Vec<String> {
    let Some(holder) = holder(target, context) else {
        return Vec::new();
    };
    let Some(data) = holder.downcast_ref::<TargetData>() else {
        return Vec::new();
    };
    let handlers = data.handlers.borrow();
    let mut kinds: Vec<String> = Vec::new();
    for listener in data.listeners.borrow().iter() {
        if listener.handler
            && handlers
                .iter()
                .find(|(k, _)| *k == listener.kind)
                .is_none_or(|(_, h)| !h.is_callable())
        {
            continue;
        }
        if !kinds.contains(&listener.kind) {
            kinds.push(listener.kind.clone());
        }
    }
    kinds
}

/// Whether `target` has a listener for any of `kinds`.
pub fn listens(target: &JsValue, kinds: &[&str], context: &Context) -> bool {
    listener_kinds(target, context)
        .iter()
        .any(|kind| kinds.contains(&kind.as_str()))
}

/// How many listeners `target` has, `on…` attributes set included.
pub fn listener_count(target: &JsValue, context: &Context) -> usize {
    let Some(holder) = holder(target, context) else {
        return 0;
    };
    holder
        .downcast_ref::<TargetData>()
        .map_or(0, |data| data.listeners.borrow().len())
}

/// A `DOMException` of `name`, to throw.
pub fn exception(context: &mut Context, message: &str, name: &str) -> JsError {
    dom_exception_error(context, message, name)
}

// ─── DOMException ───────────────────────────────────────────────────────────────────────────

/// The legacy codes, by name, that `DOMException.prototype.code` answers.
const CODES: &[(&str, u32)] = &[
    ("IndexSizeError", 1),
    ("HierarchyRequestError", 3),
    ("WrongDocumentError", 4),
    ("InvalidCharacterError", 5),
    ("NoModificationAllowedError", 7),
    ("NotFoundError", 8),
    ("NotSupportedError", 9),
    ("InUseAttributeError", 10),
    ("InvalidStateError", 11),
    ("SyntaxError", 12),
    ("InvalidModificationError", 13),
    ("NamespaceError", 14),
    ("InvalidAccessError", 15),
    ("TypeMismatchError", 17),
    ("SecurityError", 18),
    ("NetworkError", 19),
    ("AbortError", 20),
    ("URLMismatchError", 21),
    ("QuotaExceededError", 22),
    ("TimeoutError", 23),
    ("InvalidNodeTypeError", 24),
    ("DataCloneError", 25),
];

const LEGACY_CONSTANTS: &[(&str, u32)] = &[
    ("INDEX_SIZE_ERR", 1),
    ("DOMSTRING_SIZE_ERR", 2),
    ("HIERARCHY_REQUEST_ERR", 3),
    ("WRONG_DOCUMENT_ERR", 4),
    ("INVALID_CHARACTER_ERR", 5),
    ("NO_DATA_ALLOWED_ERR", 6),
    ("NO_MODIFICATION_ALLOWED_ERR", 7),
    ("NOT_FOUND_ERR", 8),
    ("NOT_SUPPORTED_ERR", 9),
    ("INUSE_ATTRIBUTE_ERR", 10),
    ("INVALID_STATE_ERR", 11),
    ("SYNTAX_ERR", 12),
    ("INVALID_MODIFICATION_ERR", 13),
    ("NAMESPACE_ERR", 14),
    ("INVALID_ACCESS_ERR", 15),
    ("VALIDATION_ERR", 16),
    ("TYPE_MISMATCH_ERR", 17),
    ("SECURITY_ERR", 18),
    ("NETWORK_ERR", 19),
    ("ABORT_ERR", 20),
    ("URL_MISMATCH_ERR", 21),
    ("QUOTA_EXCEEDED_ERR", 22),
    ("TIMEOUT_ERR", 23),
    ("INVALID_NODE_TYPE_ERR", 24),
    ("DATA_CLONE_ERR", 25),
];

fn exception_code(name: &str) -> u32 {
    CODES
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(0, |(_, c)| *c)
}

fn exception_part(
    this: &JsValue,
    f: impl FnOnce(&DomExceptionData) -> JsValue,
) -> JsResult<JsValue> {
    let object = this
        .as_object()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    let data = object
        .downcast_ref::<DomExceptionData>()
        .ok_or_else(|| type_error("Illegal invocation"))?;
    Ok(f(&data))
}

/// `new DOMException(message, name)` or `new DOMException(message, { name, cause })`.
fn make_dom_exception(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    let message = match arg(args, 0) {
        m if m.is_undefined() => String::new(),
        m => text_of(&m, context)?,
    };
    let options = arg(args, 1);
    let (name, cause) = if options.is_object() {
        let name = get(&options, "name", context)?;
        let has_cause = options
            .as_object()
            .expect("an object")
            .has_property(js_string!("cause"), context)?;
        let cause = if has_cause {
            Some(get(&options, "cause", context)?)
        } else {
            None
        };
        (
            if name.is_undefined() {
                "Error".to_owned()
            } else {
                text_of(&name, context)?
            },
            cause,
        )
    } else if options.is_undefined() {
        ("Error".to_owned(), None)
    } else {
        (text_of(&options, context)?, None)
    };
    let exception = JsObject::from_proto_and_data(
        prototype.clone(),
        DomExceptionData {
            name: name.clone(),
            message: message.clone(),
        },
    );
    // A stack, as an `Error` made here has.
    let error = context.intrinsics().constructors().error().constructor();
    let made = error.construct(&[js_text(&message)], None, context)?;
    let stack = made.get(js_string!("stack"), context)?;
    if stack.is_string() {
        hidden(&exception, js_string!("stack"), stack, context)?;
    }
    if let Some(cause) = cause {
        hidden(&exception, js_string!("cause"), cause, context)?;
    }
    Ok(exception)
}

/// A `DOMException` of the realm's own class.
pub(crate) fn dom_exception(context: &mut Context, message: &str, name: &str) -> JsValue {
    let made = dom(context).and_then(|dom| {
        dom.dom_exception
            .construct(&[js_text(message), js_text(name)], None, context)
    });
    match made {
        Ok(exception) => exception.into(),
        Err(error) => error.into_opaque(context).unwrap_or_default(),
    }
}

/// The same, to throw.
pub(crate) fn dom_exception_error(context: &mut Context, message: &str, name: &str) -> JsError {
    JsError::from_opaque(dom_exception(context, message, name))
}

// ─── AbortController and AbortSignal ────────────────────────────────────────────────────────

/// A signal not aborted, of the realm's own `AbortSignal`.
pub(crate) fn new_signal(context: &mut Context) -> JsResult<JsObject> {
    let prototype = dom(context)?.abort_signal_proto.clone();
    Ok(JsObject::from_proto_and_data(
        prototype,
        TargetData::new(Kind::Signal {
            aborted: false,
            reason: JsValue::undefined(),
            removals: Vec::new(),
            dependents: Vec::new(),
        }),
    ))
}

fn signal_state(this: &JsValue) -> JsResult<(bool, JsValue)> {
    with_target(this, |data| match &*data.kind.borrow() {
        Kind::Signal {
            aborted, reason, ..
        } => Ok((*aborted, reason.clone())),
        _ => Err(type_error("Illegal invocation")),
    })?
}

/// Aborts `signal` with `reason` (an `AbortError` when none is given): the listeners added
/// with it taken off their targets, its `abort` event fired, then the signals made from it by
/// `AbortSignal.any` aborted with the same reason.
pub(crate) fn abort(signal: &JsObject, reason: JsValue, context: &mut Context) -> JsResult<()> {
    let reason = if reason.is_undefined() {
        dom_exception(context, "This operation was aborted", "AbortError")
    } else {
        reason
    };
    let taken = with_target(&signal.clone().into(), |data| {
        let mut kind = data.kind.borrow_mut();
        let Kind::Signal {
            aborted,
            reason: own,
            removals,
            dependents,
        } = &mut *kind
        else {
            return Err(type_error("not an AbortSignal"));
        };
        if *aborted {
            return Ok(None);
        }
        *aborted = true;
        *own = reason.clone();
        Ok(Some((std::mem::take(removals), std::mem::take(dependents))))
    })??;
    let Some((removals, dependents)) = taken else {
        return Ok(());
    };
    for (target, listener) in removals {
        take_off(&target, &listener);
    }
    let event = new_event(context, "abort")?;
    dispatch(signal, &event, context)?;
    for dependent in dependents {
        abort(&dependent, reason.clone(), context)?;
    }
    Ok(())
}

const SIGNAL_METHODS: &[(&str, usize, NativeFunctionPointer)] =
    &[("throwIfAborted", 0, |this, _, _| {
        let (aborted, reason) = signal_state(this)?;
        if aborted {
            return Err(JsError::from_opaque(reason));
        }
        Ok(JsValue::undefined())
    })];

const SIGNAL_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    (
        "aborted",
        |this, _, _| Ok(signal_state(this)?.0.into()),
        None,
    ),
    ("reason", |this, _, _| Ok(signal_state(this)?.1), None),
];

const SIGNAL_STATICS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("abort", 1, |_, args, cx| {
        let signal = new_signal(cx)?;
        abort(&signal, arg(args, 0), cx)?;
        Ok(signal.into())
    }),
    ("timeout", 1, |_, args, cx| {
        let ms = arg(args, 0).to_number(cx)?;
        if !ms.is_finite() || ms < 0. {
            return Err(type_error(
                "AbortSignal.timeout: the delay must be a non-negative number",
            ));
        }
        let signal = new_signal(cx)?;
        let fire = closure(
            cx,
            "",
            0,
            |_, _, signal: &JsObject, cx| {
                let reason = dom_exception(cx, "signal timed out", "TimeoutError");
                abort(signal, reason, cx)?;
                Ok(JsValue::undefined())
            },
            signal.clone(),
        );
        crate::set_timeout(cx, fire.into(), ms, false, Vec::new());
        Ok(signal.into())
    }),
    ("any", 1, |_, args, cx| {
        let signals = iterate(&arg(args, 0), cx)?;
        let result = new_signal(cx)?;
        for source in &signals {
            let (aborted, reason) = signal_state(source)?;
            if aborted {
                abort(&result, reason, cx)?;
                return Ok(result.into());
            }
        }
        for source in signals {
            with_target(&source, |data| {
                if let Kind::Signal { dependents, .. } = &mut *data.kind.borrow_mut() {
                    dependents.push(result.clone());
                }
            })?;
        }
        Ok(result.into())
    }),
];

/// The items of an iterable, as a list: what `AbortSignal.any` is given.
pub(crate) fn iterate(value: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let from = context
        .intrinsics()
        .constructors()
        .array()
        .constructor()
        .get(js_string!("from"), context)?;
    let array = crate::call(
        context,
        &from,
        &JsValue::undefined(),
        std::slice::from_ref(value),
    )?;
    crate::native::list_of(&array, context)
}

// ─── Installing them ────────────────────────────────────────────────────────────────────────

/// The globals, each made the first time it is read.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let globals: &[(&str, crate::native::GlobalMaker)] = &[
        ("EventTarget", |cx| Ok(dom(cx)?.event_target.clone().into())),
        ("Event", |cx| Ok(dom(cx)?.event.clone().into())),
        ("CustomEvent", |cx| Ok(dom(cx)?.custom_event.clone().into())),
        ("MessageEvent", |cx| {
            Ok(dom(cx)?.message_event.clone().into())
        }),
        ("ErrorEvent", |cx| Ok(dom(cx)?.error_event.clone().into())),
        ("CloseEvent", |cx| Ok(dom(cx)?.close_event.clone().into())),
        ("ProgressEvent", |cx| {
            Ok(dom(cx)?.progress_event.clone().into())
        }),
        ("PromiseRejectionEvent", |cx| {
            Ok(dom(cx)?.rejection_event.clone().into())
        }),
        ("DOMException", |cx| {
            Ok(dom(cx)?.dom_exception.clone().into())
        }),
        ("AbortController", |cx| {
            Ok(dom(cx)?.abort_controller.clone().into())
        }),
        ("AbortSignal", |cx| Ok(dom(cx)?.abort_signal.clone().into())),
    ];
    for (name, make) in globals {
        crate::native::lazy_global(context, name, *make)?;
    }
    let report_error = function(context, "reportError", 1, |_, args, cx| {
        crate::report(cx, &JsError::from_opaque(arg(args, 0)));
        Ok(JsValue::undefined())
    });
    context.register_global_property(
        js_string!("reportError"),
        report_error,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    Ok(())
}

/// Whether `object` is a `MessagePort`: what a transfer list may hold besides buffers.
pub(crate) fn is_port(object: &JsObject) -> bool {
    object
        .downcast_ref::<TargetData>()
        .is_some_and(|data| matches!(&*data.kind.borrow(), Kind::Port { .. }))
}

/// Whether `object` is one of the runtime's event targets or events, which, as a browser's,
/// cannot be cloned.
pub(crate) fn is_platform_object(object: &JsObject) -> bool {
    object.downcast_ref::<TargetData>().is_some() || object.downcast_ref::<EventData>().is_some()
}
