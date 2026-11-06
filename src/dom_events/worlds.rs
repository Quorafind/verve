//! An event heard in more than one world. A node's objects in every world share one list of
//! listeners, in the order they were added whichever world added them; a dispatch calls each
//! listener in the world that added it, with that world's object for the target and an event of
//! that world's own, which carries what the listener did to it (`preventDefault`,
//! `stopPropagation`) back to the dispatch.

use boa_engine::realm::Realm;
use boa_engine::{Context, Finalize, JsObject, JsResult, JsValue, Trace, js_string};
use boa_gc::{Gc, GcRefCell};

use super::{EventData, EventState, Listener, call_listener, dom, holder};
use crate::clone::Cloned;
use crate::world::{
    current_world, in_realm, in_world, realm_data, set_realm_data, world_global, world_of_global,
};

/// What a host answers for one of its targets, given the world that is running: its object for
/// the same thing in this world, or `None` when this world has none to give. See
/// [`set_world_counterpart`].
pub type Counterpart = fn(target: &JsValue, context: &mut Context) -> JsResult<Option<JsValue>>;

#[derive(Clone, Copy)]
struct CounterpartHook(Counterpart);

/// Tells a dispatch how to find a host target's object in another world: it enters the world
/// of the listener and asks `counterpart` for the object there. The window needs no telling.
pub fn set_world_counterpart(context: &mut Context, counterpart: Counterpart) {
    context.insert_data(CounterpartHook(counterpart));
}

/// `target`'s object in the world that is running.
fn counterpart(target: &JsValue, context: &mut Context) -> JsResult<Option<JsValue>> {
    if let Some(object) = target.as_object()
        && world_of_global(context, &object).is_some()
    {
        return Ok(world_global(context, current_world(context)).map(JsValue::from));
    }
    if target
        .as_object()
        .is_none_or(|o| super::host_id(&o).is_none())
    {
        return Ok(None);
    }
    match context.get_data::<CounterpartHook>().copied() {
        Some(CounterpartHook(counterpart)) => counterpart(target, context),
        None => Ok(None),
    }
}

// ---------- the classes of a world, by name ----------

#[derive(Trace, Finalize)]
struct Named {
    prototype: JsObject,
    #[unsafe_ignore_trace]
    name: &'static str,
}

/// The prototypes of the web classes a realm made, with their names: what tells an event's class
/// without asking the page, whose `constructor` and globals are its own to change.
#[derive(Clone, Trace, Finalize)]
struct ClassNames(Gc<GcRefCell<Vec<Named>>>);

/// Notes that `prototype` is the web class `name`'s in the running realm.
pub(super) fn register_class(context: &Context, prototype: &JsObject, name: &'static str) {
    let names = realm_data::<ClassNames>(context).unwrap_or_else(|| {
        let names = ClassNames(Gc::new(GcRefCell::new(Vec::new())));
        set_realm_data(context, names.clone());
        names
    });
    names.0.borrow_mut().push(Named {
        prototype: prototype.clone(),
        name,
    });
}

/// The web class of the running realm that `event` is an instance of, nearest first.
fn class_name(event: &JsObject, context: &Context) -> Option<&'static str> {
    let names = realm_data::<ClassNames>(context)?;
    let names = names.0.borrow();
    let mut at = event.prototype();
    while let Some(prototype) = at {
        if let Some(found) = names
            .iter()
            .find(|n| JsObject::equals(&n.prototype, &prototype))
        {
            return Some(found.name);
        }
        at = prototype.prototype();
    }
    None
}

/// The prototype of the web class `name` of the running realm.
fn class_prototype(name: &str, context: &Context) -> Option<JsObject> {
    let names = realm_data::<ClassNames>(context)?;
    let names = names.0.borrow();
    names
        .iter()
        .find(|n| n.name == name)
        .map(|n| n.prototype.clone())
}

/// Calls `listener` in its own world for `event`, dispatched in another, at `this`.
pub(super) fn call_in_world(
    listener: &Listener,
    this: &JsValue,
    kind: &str,
    event: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let source = Source {
        realm: context.realm().clone(),
        class: class_name(event, context),
    };
    in_world(context, listener.world, |cx| {
        call_mirrored(listener, this, kind, event, &source, cx)
    })
}

/// Where an event comes from.
struct Source {
    realm: Realm,
    /// The web class it is an instance of there.
    class: Option<&'static str>,
}

fn call_mirrored(
    listener: &Listener,
    this: &JsValue,
    kind: &str,
    event: &JsObject,
    source: &Source,
    context: &mut Context,
) -> JsResult<JsValue> {
    // A target that is no node and no window is its world's own: the listener is in that world.
    let here = match counterpart(this, context)? {
        Some(here) => here,
        None if this
            .as_object()
            .is_some_and(|o| super::host_id(&o).is_none()) =>
        {
            this.clone()
        }
        None => return Ok(JsValue::undefined()),
    };
    let Some(holder) = holder(&here, context) else {
        return Ok(JsValue::undefined());
    };
    let mirror = mirror_of(event, source, context)?;
    let outcome = call_listener(listener, &holder, &here, kind, &mirror, context);
    let (Some(from), Some(to)) = (
        mirror.downcast_ref::<EventData>(),
        event.downcast_ref::<EventData>(),
    ) else {
        return outcome;
    };
    let (from, mut to) = (from.0.borrow(), to.0.borrow_mut());
    to.canceled |= from.canceled;
    to.stop |= from.stop;
    to.stop_now |= from.stop_now;
    outcome
}

/// `event` as the running world has it: an event of the same class of this world, in the same
/// state, whose targets are this world's objects for them.
fn mirror_of(event: &JsObject, source: &Source, context: &mut Context) -> JsResult<JsObject> {
    let prototype = match source.class.and_then(|name| class_prototype(name, context)) {
        Some(prototype) => prototype,
        None => dom(context)?
            .event
            .get(js_string!("prototype"), context)?
            .as_object()
            .expect("the Event class has a prototype"),
    };
    let mut state = event
        .downcast_ref::<EventData>()
        .map(|data| data.0.borrow().clone())
        .ok_or_else(|| crate::native::type_error("Illegal invocation"))?;
    translate(&mut state, &source.realm, context)?;
    Ok(JsObject::from_proto_and_data(
        prototype,
        EventData(GcRefCell::new(state)),
    ))
}

/// What a field of an event is in the running world: the node or window of the world it comes
/// from as this world has it (a target that is neither is its own world's already), a primitive as it is, and anything else as a structured clone, or
/// `null` where it cannot be cloned: no object of one world is ever handed to the other.
fn translate(state: &mut EventState, from: &Realm, context: &mut Context) -> JsResult<()> {
    let same = |value: &JsValue, context: &mut Context| -> JsResult<JsValue> {
        Ok(counterpart(value, context)?.unwrap_or_else(|| value.clone()))
    };
    let mine = |value: &JsValue, context: &mut Context| -> JsResult<JsValue> {
        if let Some(there) = counterpart(value, context)? {
            return Ok(there);
        }
        if value.as_object().is_none() {
            return Ok(value.clone());
        }
        let cloned = in_realm(context, from, |cx| Cloned::from_value(value, cx));
        match cloned.and_then(|cloned| cloned.to_value(context)) {
            Ok(copy) => Ok(copy),
            Err(_) => Ok(JsValue::null()),
        }
    };
    state.target = same(&state.target, context)?;
    state.current = same(&state.current, context)?;
    let mut path = Vec::with_capacity(state.path.len());
    for item in &state.path {
        if let Some(item) = counterpart(item, context)? {
            path.push(item);
        }
    }
    state.path = path;
    for (_, value) in &mut state.fields {
        *value = mine(value, context)?;
    }
    Ok(())
}
