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

/// A font a page declared, with its bytes: what [`Document::set_font_provider`]'s provider is
/// given.
#[derive(Clone, Debug)]
pub struct FontData {
    /// The family the page calls it by.
    pub family: String,
    /// The weight (the lower end of a range) and slant the face declared.
    pub weight: f32,
    pub italic: bool,
    pub bytes: std::sync::Arc<[u8]>,
}

/// Takes the bytes of a declared font and answers whether the page's text can use them.
pub type FontProvider = Box<dyn Fn(FontData) -> bool>;

pub(crate) type FontHook = std::rc::Rc<dyn Fn(FontData) -> bool>;

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

    /// Makes `provider` the one that takes the bytes of every font the page declares (`new
    /// FontFace(family, buffer)`, and the body of a `url()` face fetched for `load()`): the face
    /// is `loaded` when it answers `true` and `error` when it answers `false`. A layout that
    /// uses a font is laid out again after it. Without one every face with bytes loads.
    pub fn set_font_provider(&self, provider: Option<FontProvider>) {
        self.state.borrow_mut().font_hook =
            provider.map(|provider| std::rc::Rc::from(provider) as FontHook);
    }

    /// The two ends of the first range of the window's selection: what a drawer paints as
    /// selected. `None` when nothing is selected.
    pub fn selection_boundaries(
        &self,
        runtime: &mut verve::Runtime,
    ) -> Option<(crate::Boundary, crate::Boundary)> {
        bind::range::selection_bounds(runtime.context())
    }

    /// How many ranges the document holds: the selection's, and one for each `Range` object not
    /// yet collected.
    pub fn live_ranges(&self) -> usize {
        let mut state = self.state.borrow_mut();
        state.dom.ranges.count()
    }

    /// A face the host was asked for was fetched: these are its bytes. The provider
    /// ([`Document::set_font_provider`]) decides whether it loaded.
    pub fn settle_font_bytes(
        &self,
        runtime: &mut verve::Runtime,
        face: FontFaceId,
        bytes: Vec<u8>,
    ) {
        let context = runtime.context();
        if let Err(error) = bind::fonts::deliver_face(context, face, bytes.into()) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
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

    /// The embedder answered a permission question ([`crate::PermissionBackend::ask`] said it
    /// would later): whatever the page asked is settled.
    pub fn settle_permission(&self, runtime: &mut verve::Runtime, ask: u64, granted: bool) {
        let context = runtime.context();
        if let Err(error) = bind::permissions::settle(context, ask, granted) {
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
        // Frames asked for while it was hidden start now.
        if !hidden && !self.state.borrow().frames.is_empty() {
            bind::window::schedule_frame(context);
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
