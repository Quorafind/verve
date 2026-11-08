//! `<slot>`: `assignedNodes()`, `assignedElements()`, `assign()`, and `slotchange`, which is
//! told once for each slot whose assigned nodes changed, in a microtask.

use boa_engine::{Context, JsResult, JsValue};
use verve::native::{self, arg, get};

use super::node::{read_dom, with_dom, wrap};
use super::{Method, extend_prototype, node_of, queue_microtask};
use crate::dom::NodeId;
use crate::state::shared;

/// The slots of a shadow tree and what is assigned to each, told to `slotchange`.
#[derive(Default)]
pub struct Slots {
    /// What each slot showed when it was last looked at.
    shown: std::collections::HashMap<NodeId, Vec<NodeId>>,
    /// The slots to tell, once, in a microtask.
    changed: Vec<NodeId>,
    queued: bool,
}

/// The nodes `assignedNodes` or `assignedElements` answers, as objects.
fn assigned(
    this: &JsValue,
    args: &[JsValue],
    elements: bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let slot = node_of(this)?;
    let flatten = get(&arg(args, 0), "flatten", context)?.to_boolean();
    let nodes = read_dom(context, |dom| {
        let is_element = |id: &NodeId| dom.get(*id).is_some_and(|n| n.is_element());
        let nodes = dom.assigned_nodes(slot, flatten);
        nodes
            .into_iter()
            .filter(|id| !elements || is_element(id))
            .collect::<Vec<_>>()
    });
    let objects = nodes.into_iter().map(|id| wrap(context, id)).collect();
    Ok(native::array(context, objects))
}

const SLOT_METHODS: &[Method] = &[
    ("assignedNodes", 1, |this, args, cx| {
        assigned(this, args, false, cx)
    }),
    ("assignedElements", 1, |this, args, cx| {
        assigned(this, args, true, cx)
    }),
    ("assign", 0, |this, args, cx| {
        let slot = node_of(this)?;
        // A slot takes only its host's own children, as a browser's does.
        let host = read_dom(cx, |dom| dom.shadow_host(dom.root_of(slot)));
        let mut nodes = Vec::new();
        for value in args {
            let id = node_of(value)?;
            if host.is_some() && read_dom(cx, |dom| dom.parent(id)) == host {
                nodes.push(id);
            }
        }
        with_dom(cx, |dom| dom.assign_slot(slot, nodes));
        Ok(JsValue::undefined())
    }),
];

/// Puts the slot methods on `HTMLSlotElement`.
pub fn install(context: &mut Context) -> JsResult<()> {
    let prototype = crate::state::class_prototype(context, "HTMLSlotElement");
    extend_prototype(context, &prototype, SLOT_METHODS, &[])
}

/// Notes the slots whose assigned nodes changed since they were last looked at, and tells
/// them `slotchange` in a microtask.
pub(crate) fn slots_changed(context: &mut Context) {
    let queue = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        if !state.dom.take_slot_dirty() {
            return;
        }
        let now = state.dom.slot_assignments();
        for (slot, nodes) in now {
            if state.slots.shown.get(&slot) != Some(&nodes) {
                state.slots.shown.insert(slot, nodes);
                state.slots.changed.push(slot);
                state.dom.mark(slot);
            }
        }
        let queue = !state.slots.changed.is_empty() && !state.slots.queued;
        state.slots.queued = state.slots.queued || queue;
        queue
    };
    if queue {
        queue_microtask(context, "slotchange", tell_slots);
    }
}

/// The microtask that fires `slotchange` at each slot that changed.
fn tell_slots(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let slots = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.slots.queued = false;
        std::mem::take(&mut state.slots.changed)
    };
    for slot in slots {
        let init = [("bubbles", false.into()), ("composed", false.into())];
        let told = super::events::fire_at(context, slot, "Event", "slotchange", &init, false);
        if let Err(error) = told {
            verve::report(context, &error);
        }
    }
    Ok(JsValue::undefined())
}
