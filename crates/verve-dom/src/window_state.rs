//! What the window around the document tells it and what the document asks of it besides
//! drawing: whether it is in sight or has the keyboard, where the lines of inline boxes were
//! painted, how a font the page asked for fared, and the flat tree a drawer walks.

use boa_engine::{JsValue, js_string};

use crate::bind;
use crate::dom::NodeId;
use crate::{Document, Rect, Snapshot};

/// A scroll a script asked for, for the drawer to do (and then report with
/// [`Document::set_scroll_offsets`]).
#[derive(Clone, Debug, PartialEq)]
pub enum ScrollRequest {
    /// `scrollTo`, or `scrollTop`/`scrollLeft` set: to this offset, on the axes given. The
    /// document's root element stands for the window.
    To {
        node: NodeId,
        left: Option<f32>,
        top: Option<f32>,
    },
    /// `scrollBy`: this much further.
    By { node: NodeId, left: f32, top: f32 },
    /// `scrollIntoView`: whatever scrolls `node`'s box, until `node` is in sight, lined up on
    /// each axis as asked; `smooth` when `behavior` said so.
    Reveal {
        node: NodeId,
        block: Align,
        inline: Align,
        smooth: bool,
    },
}

/// Where `scrollIntoView` puts a box along an axis: `block` and `inline` of its options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// The box's start edge at the scrolling box's.
    Start,
    /// The box in the middle.
    Center,
    /// The box's end edge at the scrolling box's.
    End,
    /// The least scrolling that brings it into sight, none if it already is.
    Nearest,
}

/// The number the host knows a face by while it fetches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFaceId(pub(crate) u32);

/// How the host's fetch of a face ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontOutcome {
    /// The font was fetched and is usable.
    Loaded,
    /// It could not be fetched or parsed.
    Failed,
}

impl Document {
    /// Where the lines of inline boxes were painted, each box's border boxes one per line, in
    /// viewport coordinates: what `getClientRects` answers, replacing what was reported before.
    /// A box not reported has the one rectangle of [`Document::set_layout`].
    pub fn set_fragments(&self, fragments: impl IntoIterator<Item = (NodeId, Vec<Rect>)>) {
        let mut state = self.state.borrow_mut();
        state.fragments.clear();
        state.fragments.extend(fragments);
    }

    /// A font the host was asked for ([`crate::Request::LoadFont`]) was fetched, or failed: the
    /// face's `status` and `loaded`, and the events and `ready` of `document.fonts`, follow.
    pub fn settle_font(
        &self,
        runtime: &mut verve::Runtime,
        face: FontFaceId,
        outcome: FontOutcome,
    ) {
        let context = runtime.context();
        if let Err(error) = bind::fonts::settle_face(context, face, outcome) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }

    /// Whether the window is out of sight: `document.hidden` and `visibilityState`, and
    /// `visibilitychange` when that moves.
    pub fn set_visibility(&self, runtime: &mut verve::Runtime, hidden: bool) {
        if std::mem::replace(&mut self.state.borrow_mut().hidden, hidden) == hidden {
            return;
        }
        let context = runtime.context();
        let document = self.state.borrow().dom.document;
        let target = bind::node::wrap(context, document);
        if let Err(error) = bind::events::fire(context, &target, "Event", "visibilitychange", &[]) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }

    /// The window is about to close: `beforeunload` is dispatched at it. Answers whether the page
    /// let it go ahead — a handler that called `preventDefault()` or set `returnValue` to text
    /// kept it.
    pub fn before_unload(&self, runtime: &mut verve::Runtime) -> bool {
        let context = runtime.context();
        let window: JsValue = context.global_object().into();
        let fired = bind::events::fire_event(
            context,
            &window,
            "BeforeUnloadEvent",
            "beforeunload",
            &[],
            false,
        );
        let proceed = match fired {
            Ok((allowed, event)) => {
                let value = event
                    .get(js_string!("returnValue"), context)
                    .unwrap_or_default();
                allowed && value.as_string().is_none_or(|text| text.is_empty())
            }
            Err(error) => {
                verve::report(context, &error);
                true
            }
        };
        runtime.run_microtasks();
        proceed
    }

    /// The window closes: `pagehide`, the document going hidden, then `unload`.
    pub fn unload(&self, runtime: &mut verve::Runtime) {
        let persisted = [("persisted", JsValue::from(false))];
        self.fire_at_window(runtime, "PageTransitionEvent", "pagehide", &persisted);
        self.set_visibility(runtime, true);
        self.fire_at_window(runtime, "Event", "unload", &[]);
        runtime.run_microtasks();
    }

    fn fire_at_window(
        &self,
        runtime: &mut verve::Runtime,
        class: &str,
        kind: &str,
        init: &[(&str, JsValue)],
    ) {
        let context = runtime.context();
        let window: JsValue = context.global_object().into();
        if let Err(error) = bind::events::fire(context, &window, class, kind, init) {
            verve::report(context, &error);
        }
    }

    /// Whether the window has the keyboard: `document.hasFocus()`, and `focus` or `blur` at the
    /// window when that moves.
    pub fn set_window_focus(&self, runtime: &mut verve::Runtime, focused: bool) {
        if std::mem::replace(&mut self.state.borrow_mut().window_focused, focused) == focused {
            return;
        }
        let context = runtime.context();
        let window: JsValue = context.global_object().into();
        let kind = if focused { "focus" } else { "blur" };
        if let Err(error) = bind::events::fire(context, &window, "FocusEvent", kind, &[]) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }
}

impl Snapshot {
    /// The children a box is drawn with: see [`crate::dom::Dom::flat_children`]. A drawer that
    /// walks the document to paint it walks these, not `children`, or shadow trees are never
    /// drawn.
    pub fn flat_children(&self, node: NodeId) -> Vec<NodeId> {
        self.dom.flat_children(node)
    }

    /// The parent in the flat tree: see [`crate::dom::Dom::flat_parent`].
    pub fn flat_parent(&self, node: NodeId) -> Option<NodeId> {
        self.dom.flat_parent(node)
    }
}
