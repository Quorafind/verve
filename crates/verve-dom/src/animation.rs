//! The frame driver of transitions and animations: it moves the store of [`crate::css`] along
//! with the document's frames, puts the values it gives over the cascade's, and tells whoever
//! listens — the element for a CSS animation, the `Animation` object for a script's — what
//! happened, settling `finished` as it does.
//!
//! Frames come from the embedder ([`crate::FrameClock::Host`]: [`crate::Document::frame`]) or
//! from the realm's own timers ([`crate::FrameClock::Timer`]), which only run while there is
//! something to move or the page asked for transitions and then changed. A frame updates the
//! animations and sends their events before the `requestAnimationFrame` callbacks run, so a
//! callback reads the values of the frame it is in.

use std::collections::HashMap;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsValue};

use crate::bind;
use crate::css::{AnimationEvent, AnimationOutcome, Computed, Style, Styler, is_inherited};
use crate::dom::{Dom, NodeId};
use crate::state::{FrameClock, State, shared};
use crate::{Document, Request};

/// Starts and stops the transitions and animations a restyle of `restyled` asks for, given the
/// styles `before`, and puts the values they give over the cascade's. Answers the nodes whose
/// shown style that changed.
pub(crate) fn restyle(
    state: &mut State,
    computed: &mut Computed,
    styler: &Styler,
    before: &Computed,
    restyled: &[NodeId],
) -> Vec<NodeId> {
    let was_empty = state.animations.is_empty();
    state
        .animations
        .restart(styler, (before, computed), restyled);
    // The embedder is asked for a frame when there is something to move, or to tell: what a
    // restyle cancelled is told on the next frame.
    let wanted = (was_empty && !state.animations.is_empty()) || state.animations.has_events();
    if wanted && state.frame_clock == FrameClock::Host && !state.requests.contains(&Request::Frame)
    {
        state.requests.push(Request::Frame);
    }
    overlay(state, computed)
}

/// The style of every node an animation drives at the time of the last frame, and of the
/// descendants that inherit what it animates.
fn overlay(state: &State, computed: &mut Computed) -> Vec<NodeId> {
    let mut shown = state
        .animations
        .styles(state.now, |node| computed.cascaded(node));
    let animated: Vec<NodeId> = shown.keys().copied().collect();
    for node in animated {
        pass_down(state, computed, node, &mut shown);
    }
    computed.set_overlay(shown)
}

/// What `node`'s animations make of the properties its descendants inherit, given to the
/// descendants that take them from it: a child of an element that is changing color is changing
/// color too.
fn pass_down(
    state: &State,
    computed: &Computed,
    node: NodeId,
    shown: &mut HashMap<NodeId, Arc<Style>>,
) {
    let (Some(values), Some(base)) = (
        state.animations.animated(node, state.now),
        computed.cascaded(node),
    ) else {
        return;
    };
    for (property, value) in values.iter().filter(|(p, _)| is_inherited(p)) {
        let animates = |n| {
            state
                .animations
                .animated(n, state.now)
                .is_some_and(|a| a.contains_key(property))
        };
        let mut stack = state.dom.flat_children(node);
        while let Some(child) = stack.pop() {
            let Some(style) = computed.cascaded(child) else {
                continue;
            };
            // A child with a value of its own, or one an animation drives, is not inheriting.
            if style.get(property) != base.get(property) || animates(child) {
                continue;
            }
            let entry = shown.entry(child).or_insert_with(|| style.clone());
            Arc::make_mut(entry).insert(property.clone(), value.clone());
            stack.extend(state.dom.flat_children(child));
        }
    }
}

/// What an ancestor's animation makes of the properties `node` inherits from it, for the style
/// `fresh`: the values the last frame showed on it, where `fresh` still agrees with the cascade
/// that frame was made from.
pub(crate) fn inherited(context: &Context, node: NodeId, fresh: &Style) -> Vec<(String, String)> {
    let Some(document) = Document::at(context) else {
        return Vec::new();
    };
    let view = document.view.borrow();
    let (Some(shown), Some(cascaded)) = (view.computed.style(node), view.computed.cascaded(node))
    else {
        return Vec::new();
    };
    shown
        .iter()
        .filter(|(property, value)| {
            is_inherited(property)
                && cascaded.get(*property) != Some(*value)
                && fresh.get(*property) == cascaded.get(*property)
        })
        .map(|(property, value)| (property.clone(), value.clone()))
        .collect()
}

/// Whether `node` is drawn: in the document, and no element on the way up from it is
/// `display: none`.
fn is_drawn(dom: &Dom, computed: &Computed, node: NodeId) -> bool {
    let mut at = Some(node);
    while let Some(id) = at {
        if computed.get(id, "display") == Some("none") {
            return false;
        }
        at = dom.parent(id);
    }
    dom.is_connected(node)
}

/// Whether the document wants a frame for what moves in it: an animation on something drawn, or
/// an event that waits to be told.
pub(crate) fn wants(state: &State, computed: &Computed) -> bool {
    state.animations.has_events()
        || state
            .animations
            .is_animating(|node| is_drawn(&state.dom, computed, node))
}

/// The style the cascade gives `node`, animations aside: what a script animation starts from.
pub(crate) fn cascaded(context: &Context, node: NodeId) -> Arc<Style> {
    Document::at(context)
        .and_then(|document| {
            document.absorb();
            document.view.borrow().computed.cascaded(node)
        })
        .unwrap_or_default()
}

/// Moves the transitions and animations to `now`: brings the styles up to date, which begins
/// what the cascade now asks for, advances the store, and delivers the events it reached.
/// Answers whether anything is still moving.
pub(crate) fn advance(context: &mut Context, now: f64) -> bool {
    let Some(document) = Document::at(context) else {
        return false;
    };
    document.absorb();
    let (events, animating) = {
        let mut state = document.state.borrow_mut();
        let state = &mut *state;
        let mut view = document.view.borrow_mut();
        state.now = now;
        let dom = &state.dom;
        state.animations.advance(now, |node| dom.is_connected(node));
        for node in overlay(state, &mut view.computed) {
            view.restyled.insert(node);
            view.partitions.insert(dom.partition_of(node));
        }
        let animating = state
            .animations
            .is_animating(|node| is_drawn(dom, &view.computed, node));
        (state.animations.take_events(), animating)
    };
    deliver(context, events);
    animating
}

/// Delivers what the store queued outside a frame: a script's `cancel()` or `finish()`.
pub(crate) fn flush_events(context: &mut Context) {
    let events = shared(context).borrow_mut().animations.take_events();
    deliver(context, events);
}

/// One tick of the realm's own clock: the frame's work for animations, and the next frame if
/// something still moves.
pub(crate) fn tick(context: &mut Context, now: f64) {
    if advance(context, now) {
        bind::window::schedule_frame(context);
    }
}

/// Asks for a frame because the page changed and its sheets animate: on the realm's own clock,
/// so a transition begins with no `requestAnimationFrame` loop to carry it.
pub(crate) fn wake(context: &mut Context) {
    let due = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let changed = state.dom.revision != state.woken;
        state.woken = state.dom.revision;
        if changed && state.frame_clock == FrameClock::Timer && state.frame_timer.is_none() {
            // The sheets as they are now, which is what says whether the page animates.
            state.styler();
            state.animates
        } else {
            false
        }
    };
    if due {
        bind::window::schedule_frame(context);
    }
}

/// Lets go of what a sweep freed: the animations of nodes that are gone — whoever waits on one
/// hears it was cancelled — and the objects, records and finished script animations nothing
/// holds any more.
pub(crate) fn swept(context: &mut Context, freed: &[NodeId]) {
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.animations.forget(freed);
        state
            .animation_records
            .retain(|_, record| record.held.is_some() || record.object.upgrade().is_some());
        let State {
            animations,
            animation_records,
            ..
        } = &mut *state;
        animations.release_finished(|id| animation_records.contains_key(&id));
    }
    flush_events(context);
}

/// Tells the listeners of each event, and settles `finished` for the ones that end or cancel.
fn deliver(context: &mut Context, events: Vec<AnimationEvent>) {
    for event in events {
        let kind = event.kind;
        let fired = if event.silent {
            None
        } else if event.script {
            object_of(context, event.id).map(|object| {
                let target = JsValue::from(object);
                bind::events::fire(context, &target, kind.class(), kind.name(), &[])
            })
        } else {
            let name_key = if kind.is_transition() {
                "propertyName"
            } else {
                "animationName"
            };
            let init = [
                ("bubbles", true.into()),
                (name_key, verve::native::js_text(&event.name)),
                ("elapsedTime", (event.elapsed / 1000.).into()),
                ("pseudoElement", verve::native::js_text("")),
            ];
            Some(bind::events::fire_at(
                context,
                event.node,
                kind.class(),
                kind.name(),
                &init,
                true,
            ))
        };
        if let Some(Err(error)) = fired {
            verve::report(context, &error);
        }
        if let Some(outcome) = kind.settles() {
            settle(context, event.id, outcome, event.time);
        }
    }
}

/// Settles the `finished` promise of animation `id`: resolved with its object when it finished,
/// rejected with an `AbortError` when it was cancelled. What the record held to keep the object
/// until now is let go of.
fn settle(context: &mut Context, id: u32, outcome: AnimationOutcome, time: f64) {
    let pending = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.animation_records.get_mut(&id).and_then(|record| {
            record.ended_at = time;
            record.outcome = Some(outcome);
            let pending = record.finished.take();
            let object = record.held.take();
            pending.map(|pending| (pending, object))
        })
    };
    let Some((pending, object)) = pending else {
        return;
    };
    let result = if outcome == AnimationOutcome::Finished {
        let value = object.map_or_else(JsValue::undefined, JsValue::from);
        pending
            .resolve
            .call(&JsValue::undefined(), &[value], context)
    } else {
        let error =
            verve::dom_events::exception(context, "The animation was aborted", "AbortError");
        match error.into_opaque(context) {
            Ok(reason) => pending
                .reject
                .call(&JsValue::undefined(), &[reason], context),
            Err(error) => Err(error),
        }
    };
    if let Err(error) = result {
        verve::report(context, &error);
    }
}

/// The object an event of animation `id` is dispatched on, when there is one.
pub(crate) fn object_of(context: &Context, id: u32) -> Option<JsObject> {
    let state = shared(context);
    let state = state.borrow();
    let record = state.animation_records.get(&id)?;
    record.held.clone().or_else(|| record.object.upgrade())
}

#[cfg(test)]
mod tests {
    use crate::Options;

    /// The nodes a restyle looked at for animations, after each of three updates.
    fn examined() -> Vec<usize> {
        const STACK: usize = 64 << 20;
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(|| {
                verve::set_thread_stack(STACK);
                let mut runtime = verve::Runtime::new();
                let document = crate::install(&mut runtime, Options::default()).unwrap();
                document.set_stylesheet("test", "div { transition: opacity 100ms linear }");
                runtime
                    .eval(
                        "document.body.innerHTML = '<div></div>'.repeat(200);
                         window.boxes = document.querySelectorAll('div');
                         boxes.forEach(b => b.style.opacity = '1');",
                    )
                    .unwrap();
                let looked =
                    |document: &crate::Document| document.state.borrow().animations.examined();
                let mut counts = Vec::new();
                document.update();
                counts.push(looked(&document));
                runtime.eval("boxes[7].style.opacity = '0'").unwrap();
                document.update();
                counts.push(looked(&document));
                document.update();
                counts.push(looked(&document));
                counts
            })
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn a_restyle_looks_only_at_the_nodes_it_restyled() {
        let counts = examined();
        // The first update restyles every node there is, the second the one that changed, the
        // third nothing at all: not all the nodes of the document, every time.
        assert!(counts[0] >= 200, "{counts:?}");
        assert_eq!(counts[1] - counts[0], 1, "{counts:?}");
        assert_eq!(counts[2], counts[1], "{counts:?}");
    }

    /// What the realm holds of the animations a script made and let go of, before and after a
    /// sweep, once they have finished.
    fn held_after_a_sweep() -> [(usize, usize); 2] {
        const STACK: usize = 64 << 20;
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(|| {
                verve::set_thread_stack(STACK);
                let mut runtime = verve::Runtime::new();
                let document = crate::install(&mut runtime, Options::default()).unwrap();
                runtime
                    .eval(
                        "const box = document.body.appendChild(document.createElement('div'));
                         for (let i = 0; i < 20; i++) box.animate([{ opacity: '0' }, { opacity: '1' }], 100);",
                    )
                    .unwrap();
                document.frame(&mut runtime, 0.);
                document.frame(&mut runtime, 200.);
                let entries = |document: &crate::Document| {
                    let state = document.state.borrow();
                    (state.animation_records.len(), usize::from(!state.animations.is_empty()))
                };
                let before = entries(&document);
                document.sweep(&mut runtime);
                document.sweep(&mut runtime);
                [before, entries(&document)]
            })
            .unwrap()
            .join()
            .unwrap()
    }

    #[test]
    fn a_finished_script_animation_nothing_holds_is_let_go_of() {
        let [before, after] = held_after_a_sweep();
        // Settled, so only weakly known: gone with its object at a sweep, store entry and all.
        assert_eq!(before, (20, 1));
        assert_eq!(after, (0, 0));
    }
}
