//! `Animation` (Web Animations 1) and its two CSS subclasses, `CSSAnimation` and
//! `CSSTransition`: what `element.animate()` returns and `getAnimations()` lists, an
//! `EventTarget` that stands for one animation of the document's store.
//!
//! The object is a host target numbered with [`TAG`] and the animation's id, so nothing of it is
//! a property a script can see; what a script may ask of it — its state, its time, its
//! `finished` promise — is read from the store when asked, and what it may tell it to do
//! (`play`, `pause`, `reverse`, `finish`, `cancel`, `commitStyles`) is done to that one
//! animation. `persist()` is not supported and says so.
//!
//! R1: `class` and `ClassSpec` are the legacy ones of `bind/mod.rs` (they register the class in
//! the document's table, which `verve::native::class` does not); the rest comes from
//! `verve::native`.

use std::sync::Arc;

use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue, NativeFunction};
use verve::native::{arg, array, coded_error, get, js_text, list_of, text_of, type_error};

use super::node::with_dom;
use super::{Accessor, ClassSpec, Method, class, illegal_constructor, node_of, queue_microtask};
use crate::css::{
    AnimationOutcome, Declaration, Direction, Easing, Fill, Keyframes, PlayState, Timing,
};
use crate::state::{AnimationRecord, Pending, shared};

/// What distinguishes an animation's number among host targets, as a node's does: `NODE_TAG`
/// is bit 40 in `bind/node.rs`, this is bit 41.
const TAG: u64 = 1 << 41;

/// Makes `Animation`, and the `CSSAnimation` and `CSSTransition` that extend it.
pub fn install(context: &mut Context) -> JsResult<()> {
    let spec = |name, parent, methods, accessors| ClassSpec {
        name,
        length: 0,
        construct: illegal_constructor,
        parent,
        methods,
        accessors,
        statics: &[],
        global: true,
    };
    let (_, prototype) = class(
        context,
        spec("Animation", Some("EventTarget"), METHODS, ACCESSORS),
    )?;
    verve::dom_events::handler_accessors(context, &prototype, &["finish", "cancel", "remove"])?;
    class(
        context,
        spec("CSSAnimation", Some("Animation"), &[], NAME_OF_ANIMATION),
    )?;
    class(
        context,
        spec("CSSTransition", Some("Animation"), &[], NAME_OF_TRANSITION),
    )?;
    Ok(())
}

/// The animation an object stands for.
fn id_of(this: &JsValue) -> JsResult<u32> {
    this.as_object()
        .and_then(|object| verve::dom_events::host_id(&object))
        .filter(|host| host & TAG != 0)
        .map(|host| (host & !TAG) as u32)
        .ok_or_else(|| type_error("Illegal invocation: not an Animation"))
}

/// Gives `promise` a handler that does nothing, so that its rejection is not reported as one
/// nobody handled. The promise `catch` returns is of no use to anyone, so it is dropped.
fn mark_handled(promise: &JsPromise, context: &mut Context) {
    let ignore = NativeFunction::from_fn_ptr(|_, _, _| Ok(JsValue::undefined()))
        .to_js_function(context.realm());
    let _ = promise.catch(ignore, context);
}

/// A `finished` promise, and how to settle it. Cancelling rejects it, which is no mistake of a
/// page that never looks at it: it is marked handled.
fn pending(context: &mut Context) -> Pending {
    let (promise, resolvers) = JsPromise::new_pending(context);
    mark_handled(&promise, context);
    Pending {
        promise: promise.into(),
        resolve: resolvers.resolve,
        reject: resolvers.reject,
    }
}

/// The object that stands for animation `id`: the one a script already has, or a new one of the
/// class its kind is.
fn object_of(context: &mut Context, id: u32) -> JsObject {
    if let Some(object) = crate::animation::object_of(context, id) {
        return object;
    }
    let class = shared(context)
        .borrow()
        .animations
        .find(id)
        .map_or("Animation", |running| {
            if running.is_script() {
                "Animation"
            } else if running.is_transition() {
                "CSSTransition"
            } else {
                "CSSAnimation"
            }
        });
    let prototype = crate::state::class_prototype(context, class);
    let object = verve::dom_events::host_target(prototype, TAG | u64::from(id));
    let finished = pending(context);
    shared(context).borrow_mut().animation_records.insert(
        id,
        AnimationRecord {
            object: object.downgrade(),
            held: Some(object.clone()),
            finished: Some(finished),
            outcome: None,
            ended_at: 0.,
        },
    );
    object
}

/// `element.getAnimations()`: the animations running on it, as the objects a script may
/// already hold.
pub fn element_animations(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this)?;
    let ids = shared(context).borrow().animations.of_node(Some(node));
    objects(ids, context)
}

/// `document.getAnimations()`: all of them.
pub fn document_animations(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ids = shared(context).borrow().animations.of_node(None);
    objects(ids, context)
}

/// The objects of the animations `ids`, as an array.
fn objects(ids: Vec<u32>, context: &mut Context) -> JsResult<JsValue> {
    let list = ids
        .into_iter()
        .map(|id| object_of(context, id).into())
        .collect();
    Ok(array(context, list))
}

/// How animation `id` stands: its state, how far in it is (`None` when idle), and how fast it
/// goes. An animation the store let go of — a transition that ended — is finished, unless it was
/// cancelled.
fn standing(context: &Context, id: u32) -> (PlayState, Option<f64>, f64) {
    let state = shared(context);
    let state = state.borrow();
    match state.animations.find(id) {
        Some(running) if running.state == PlayState::Idle => (PlayState::Idle, None, running.rate),
        Some(running) => (running.state, Some(running.time(state.now)), running.rate),
        None => {
            let record = state.animation_records.get(&id);
            let idle = record.is_some_and(|r| r.outcome == Some(AnimationOutcome::Cancelled));
            let ended = record.map_or(0., |r| r.ended_at);
            if idle {
                (PlayState::Idle, None, 1.)
            } else {
                (PlayState::Finished, Some(ended), 1.)
            }
        }
    }
}

/// Makes `finished` a new promise when the animation goes on after settling it, and holds the
/// object again until it does.
fn renew_finished(context: &mut Context, id: u32) {
    let settled = shared(context)
        .borrow()
        .animation_records
        .get(&id)
        .is_some_and(|record| record.outcome.is_some());
    if settled {
        let finished = pending(context);
        if let Some(record) = shared(context).borrow_mut().animation_records.get_mut(&id) {
            record.held = record.object.upgrade();
            (record.finished, record.outcome) = (Some(finished), None);
        }
    }
}

/// Delivers the events the call queued once the script that made it is done, so that a handler
/// set after `finish()` or `cancel()` still hears.
fn tell_later(context: &mut Context) {
    queue_microtask(context, "animation events", |_, _, cx| {
        crate::animation::flush_events(cx);
        Ok(JsValue::undefined())
    });
}

const METHODS: &[Method] = &[
    ("play", 0, |this, _, cx| {
        let id = id_of(this)?;
        shared(cx).borrow_mut().animations.play(id);
        renew_finished(cx, id);
        super::window::schedule_frame(cx);
        Ok(JsValue::undefined())
    }),
    ("pause", 0, |this, _, cx| {
        let id = id_of(this)?;
        shared(cx).borrow_mut().animations.pause(id);
        Ok(JsValue::undefined())
    }),
    ("cancel", 0, |this, _, cx| {
        let id = id_of(this)?;
        shared(cx).borrow_mut().animations.cancel(id);
        tell_later(cx);
        Ok(JsValue::undefined())
    }),
    ("finish", 0, |this, _, cx| {
        let id = id_of(this)?;
        let finishable = {
            let state = shared(cx);
            let mut state = state.borrow_mut();
            state.animations.find(id).is_none() || state.animations.finish(id)
        };
        if !finishable {
            return Err(verve::dom_events::exception(
                cx,
                "Cannot finish an animation with an infinite target effect.",
                "InvalidStateError",
            ));
        }
        tell_later(cx);
        Ok(JsValue::undefined())
    }),
    ("reverse", 0, |this, _, cx| {
        let id = id_of(this)?;
        let (_, _, rate) = standing(cx, id);
        {
            let state = shared(cx);
            let mut state = state.borrow_mut();
            state.animations.update_rate(id, -rate);
            state.animations.play(id);
        }
        renew_finished(cx, id);
        super::window::schedule_frame(cx);
        Ok(JsValue::undefined())
    }),
    ("updatePlaybackRate", 1, |this, args, cx| {
        let id = id_of(this)?;
        let rate = arg(args, 0).to_number(cx)?;
        shared(cx).borrow_mut().animations.update_rate(id, rate);
        Ok(JsValue::undefined())
    }),
    ("commitStyles", 0, |this, _, cx| {
        let id = id_of(this)?;
        let committed = shared(cx).borrow().animations.committed(id);
        if let Some((node, style)) = committed {
            with_dom(cx, |dom| {
                for (property, value) in &style {
                    dom.set_style(node, property, value);
                }
            });
        }
        Ok(JsValue::undefined())
    }),
    ("persist", 0, |_, _, cx| {
        Err(coded_error(
            cx,
            "Animation.persist() is not supported by Verve",
            "ERR_NOT_SUPPORTED",
        ))
    }),
];

const ACCESSORS: &[Accessor] = &[
    (
        "playState",
        Some(|this, _, cx| Ok(js_text(standing(cx, id_of(this)?).0.name()))),
        None,
    ),
    (
        "currentTime",
        Some(|this, _, cx| {
            Ok(standing(cx, id_of(this)?)
                .1
                .map_or_else(JsValue::null, JsValue::from))
        }),
        Some(|this, args, cx| {
            let id = id_of(this)?;
            let time = arg(args, 0).to_number(cx)?;
            shared(cx).borrow_mut().animations.seek(id, time);
            Ok(JsValue::undefined())
        }),
    ),
    (
        "playbackRate",
        Some(|this, _, cx| Ok(standing(cx, id_of(this)?).2.into())),
        Some(|this, args, cx| {
            let id = id_of(this)?;
            let rate = arg(args, 0).to_number(cx)?;
            shared(cx).borrow_mut().animations.set_rate(id, rate);
            Ok(JsValue::undefined())
        }),
    ),
    ("startTime", Some(|_, _, _| Ok(JsValue::null())), None),
    ("pending", Some(|_, _, _| Ok(false.into())), None),
    ("effect", Some(|_, _, _| Ok(JsValue::null())), None),
    ("timeline", Some(|_, _, _| Ok(JsValue::null())), None),
    ("id", Some(|_, _, _| Ok(js_text(""))), None),
    ("replaceState", Some(|_, _, _| Ok(js_text("active"))), None),
    ("finished", Some(|this, _, cx| finished(this, cx)), None),
    (
        "ready",
        Some(|this, _, cx| super::resolved(this.clone(), cx)),
        None,
    ),
];

const NAME_OF_ANIMATION: &[Accessor] =
    &[("animationName", Some(|this, _, cx| named(this, cx)), None)];

const NAME_OF_TRANSITION: &[Accessor] = &[(
    "transitionProperty",
    Some(|this, _, cx| named(this, cx)),
    None,
)];

/// The name a CSS animation or transition goes by, while the store still holds it.
fn named(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = id_of(this)?;
    let state = shared(context);
    let state = state.borrow();
    Ok(state
        .animations
        .find(id)
        .map_or_else(JsValue::undefined, |running| js_text(running.name())))
}

/// The promise `finished` answers: the one that waits, or — once it was settled and the record
/// let go of it — one that already is, resolved or rejected as it was.
fn finished(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = id_of(this)?;
    let (waiting, outcome) = {
        let state = shared(context);
        let state = state.borrow();
        let record = state.animation_records.get(&id);
        (
            record.and_then(|r| r.finished.as_ref().map(|p| p.promise.clone())),
            record.and_then(|r| r.outcome),
        )
    };
    if let Some(promise) = waiting {
        return Ok(promise.into());
    }
    if outcome == Some(AnimationOutcome::Cancelled) {
        let error =
            verve::dom_events::exception(context, "The animation was aborted", "AbortError");
        let rejected = super::rejected(error, context)?;
        if let Some(object) = rejected
            .as_object()
            .and_then(|o| JsPromise::from_object(o).ok())
        {
            mark_handled(&object, context);
        }
        return Ok(rejected);
    }
    super::resolved(this.clone(), context)
}

/// `element.animate(keyframes, options)`: an animation on the document's timeline, driven by
/// the same frames as a transition, whose `finished` settles when it ends.
pub fn animate_element(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let node = node_of(this)?;
    let keyframes = keyframes_from(&arg(args, 0), context)?;
    let timing = timing_from(&arg(args, 1), context)?;
    let base = crate::animation::cascaded(context, node);
    let id = shared(context).borrow_mut().animations.start_script(
        node,
        Arc::new(keyframes),
        base,
        timing,
    );
    let object = object_of(context, id);
    super::window::schedule_frame(context);
    Ok(object.into())
}

/// The names that are no property of a keyframe.
const NOT_PROPERTIES: [&str; 3] = ["offset", "easing", "composite"];

/// The keyframes `animate()` was given: an array of objects, each with the offset it says or
/// one spread evenly; or one object of arrays, `{ opacity: [0, 1] }`, each property's values
/// spread evenly on their own.
fn keyframes_from(value: &JsValue, context: &mut Context) -> JsResult<Keyframes> {
    let object = value
        .as_object()
        .ok_or_else(|| type_error("keyframes must be an array or an object"))?;
    let mut frames = Vec::new();
    if object.is_array() {
        let list = list_of(value, context)?;
        for (at, frame) in list.iter().enumerate() {
            let offset = match get(frame, "offset", context)? {
                said if said.is_undefined() || said.is_null() => spread(at, list.len()),
                said => said.to_number(context)? as f32,
            };
            let mut declarations = Vec::new();
            for name in properties(frame, context)? {
                let written = get(frame, &name, context)?;
                declarations.push(declaration(&name, &written, context)?);
            }
            frames.push((offset, declarations, easing_of(frame, context)?));
        }
    } else {
        for name in properties(value, context)? {
            let written = get(value, &name, context)?;
            let values = match written.as_object().filter(|o| o.is_array()) {
                Some(_) => list_of(&written, context)?,
                None => vec![written],
            };
            for (at, written) in values.iter().enumerate() {
                let declaration = declaration(&name, written, context)?;
                frames.push((spread(at, values.len()), vec![declaration], None));
            }
        }
    }
    Ok(Keyframes::from_frames(frames))
}

/// The easing a keyframe moves on to the next with, if it says one.
fn easing_of(frame: &JsValue, context: &mut Context) -> JsResult<Option<Easing>> {
    match get(frame, "easing", context)? {
        said if said.is_string() => Ok(Some(Easing::parse(&text_of(&said, context)?))),
        _ => Ok(None),
    }
}

/// Where the `at`th of `count` values falls when nothing says: evenly, the last at the end.
fn spread(at: usize, count: usize) -> f32 {
    if count > 1 {
        at as f32 / (count - 1) as f32
    } else {
        1.
    }
}

/// The names of the properties an object of keyframe values sets.
fn properties(object: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    let Some(object) = object.as_object() else {
        return Ok(Vec::new());
    };
    Ok(object
        .own_property_keys(context)?
        .into_iter()
        .map(|key| key.to_string())
        .filter(|name| !NOT_PROPERTIES.contains(&name.as_str()))
        .collect())
}

fn declaration(name: &str, written: &JsValue, context: &mut Context) -> JsResult<Declaration> {
    Ok(Declaration {
        name: if name.contains('-') {
            name.to_owned()
        } else {
            crate::dom::css_name(name)
        },
        value: text_of(written, context)?,
        important: false,
    })
}

/// The timing `animate()` was given: a duration alone, or an object of the options.
fn timing_from(options: &JsValue, context: &mut Context) -> JsResult<Timing> {
    let mut timing = Timing::default();
    if let Some(duration) = options.as_number() {
        timing.duration = duration;
        return Ok(timing);
    }
    let read = |key: &str, default: f64, context: &mut Context| -> JsResult<f64> {
        match get(options, key, context)? {
            value if value.is_undefined() || value.is_null() => Ok(default),
            // `duration: 'auto'` is no time at all.
            value if value.is_string() => Ok(default),
            value => value.to_number(context),
        }
    };
    timing.duration = read("duration", 0., context)?;
    timing.delay = read("delay", 0., context)?;
    timing.iterations = read("iterations", 1., context)?;
    let word = |key: &str, context: &mut Context| -> JsResult<Option<String>> {
        match get(options, key, context)? {
            value if value.is_string() => Ok(Some(text_of(&value, context)?)),
            _ => Ok(None),
        }
    };
    if let Some(easing) = word("easing", context)? {
        timing.easing = Easing::parse(&easing);
    }
    if let Some(direction) = word("direction", context)? {
        timing.direction = Direction::from_name(&direction);
    }
    if let Some(fill) = word("fill", context)? {
        timing.fill = Fill::from_name(&fill);
    }
    Ok(timing)
}
