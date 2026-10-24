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
//! The Obsidian host has a document, and may put its own `EventTarget`, `Event` and
//! `CustomEvent` in place of these. The runtime's own targets — `AbortSignal`, `MessagePort`,
//! `WebSocket` — keep using these classes, which the realm holds apart from its globals; and a
//! target here hears an event made by the host's classes too, by its `type`.

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
pub(crate) type Make = fn(&JsObject, &[JsValue], &mut Context) -> JsResult<JsObject>;

#[derive(Clone, Copy)]
struct MakeBox(Make, &'static str);

impl Finalize for MakeBox {}
// SAFETY: a function pointer and a static string hold no garbage-collected value.
unsafe impl Trace for MakeBox {
    boa_gc::empty_trace!();
}

/// A class as the web has them: constructed only with `new`, made by `make` on the prototype
/// `new.target` names. Answers its constructor and prototype.
pub(crate) fn web_class(
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
        let current = with_event(this, |s| {
            if s.dispatching {
                s.current.clone()
            } else {
                JsValue::null()
            }
        })?;
        let items = if current.is_null() {
            Vec::new()
        } else {
            vec![current]
        };
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
    ("isTrusted", |_, _, _| Ok(false.into()), None),
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
    let listener = with_target(this, |data| {
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
        let target = this.as_object().expect("a target");
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
    with_target(this, |data| {
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

/// Dispatches `event` at `target`: its listeners for the event's type, capturing ones first,
/// each one's error reported and the rest still called. Answers whether nothing cancelled it.
pub(crate) fn dispatch(
    target: &JsObject,
    event: &JsObject,
    context: &mut Context,
) -> JsResult<bool> {
    let ours = event.downcast_ref::<EventData>().is_some();
    let kind = if ours {
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
        state.target = target.clone().into();
        state.current = target.clone().into();
        state.phase = 2;
        state.stop_now = false;
        state.kind.clone()
    } else {
        // An event made by the host's classes: heard by its type, its own state its own.
        text_of(&event.get(js_string!("type"), context)?, context)?
    };
    let listeners: Vec<Gc<Listener>> = match target.downcast_ref::<TargetData>() {
        Some(data) => data
            .listeners
            .borrow()
            .iter()
            .filter(|l| l.kind == kind)
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    let this: JsValue = target.clone().into();
    'passes: for capture in [true, false] {
        for listener in listeners.iter().filter(|l| l.capture == capture) {
            if listener.removed.get() {
                continue;
            }
            if listener.once {
                take_off(target, listener);
            }
            if ours {
                let data = event.downcast_ref::<EventData>().expect("an event");
                data.0.borrow_mut().passive = listener.passive;
            }
            let outcome = if listener.handler {
                call_handler(target, &kind, event, context)
            } else if listener.callback.is_callable() {
                crate::call(context, &listener.callback, &this, &[event.clone().into()])
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
            if let Err(error) = outcome {
                crate::report(context, &error);
            }
            if ours {
                let data = event.downcast_ref::<EventData>().expect("an event");
                let mut state = data.0.borrow_mut();
                state.passive = false;
                if state.stop_now {
                    break 'passes;
                }
            }
        }
    }
    if !ours {
        return Ok(!get(&event.clone().into(), "defaultPrevented", context)?.to_boolean());
    }
    let data = event.downcast_ref::<EventData>().expect("an event");
    let mut state = data.0.borrow_mut();
    state.dispatching = false;
    state.current = JsValue::null();
    state.phase = 0;
    state.stop = false;
    state.stop_now = false;
    Ok(!state.canceled)
}

/// An `on…` attribute's turn in a dispatch: its value now called, a `false` it answers
/// cancelling the event.
fn call_handler(
    target: &JsObject,
    kind: &str,
    event: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let handler = match target.downcast_ref::<TargetData>() {
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
    let answer = crate::call(
        context,
        &handler,
        &target.clone().into(),
        &[event.clone().into()],
    )?;
    if answer == JsValue::from(false)
        && let Some(data) = event.downcast_ref::<EventData>()
    {
        cancel(&mut data.0.borrow_mut());
    }
    Ok(answer)
}

/// The `on…` attribute for `kind` on `target`.
pub(crate) fn handler_of(target: &JsValue, kind: &str) -> JsResult<JsValue> {
    with_target(target, |data| {
        let handlers = data.handlers.borrow();
        handlers
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, h)| h.clone())
            .unwrap_or(JsValue::null())
    })
}

/// Sets the `on…` attribute for `kind`: the first time, it takes its place among the listeners.
pub(crate) fn set_handler(target: &JsValue, kind: &str, value: JsValue) -> JsResult<()> {
    let value = if value.is_callable() {
        value
    } else {
        JsValue::null()
    };
    with_target(target, |data| {
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
    })
}

/// `onabort`, `onmessage`… on a prototype: an accessor for each event type in `kinds`.
pub(crate) fn handler_accessors(
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
            |this, _, kind: &JsString, _| handler_of(this, &kind.to_std_string_escaped()),
            key.clone(),
        );
        let setter = closure(
            context,
            &format!("set on{kind}"),
            1,
            |this, args, kind: &JsString, _| {
                set_handler(this, &kind.to_std_string_escaped(), arg(args, 0))?;
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
        let target = this
            .as_object()
            .ok_or_else(|| type_error("Illegal invocation"))?;
        Ok(dispatch(&target, &event, cx)?.into())
    }),
];

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
