//! Geometry a script can read at once: the layout provider.
//!
//! A drawer that lays the page out on its own thread reports the boxes with
//! [`Document::set_layout`], and a script that reads `getBoundingClientRect` right after it
//! changed the document sees the boxes of before. A page whose layout runs on the script's
//! thread installs a provider instead: the first geometry read after the document changed (or
//! the viewport did) asks it for the boxes, the scroll offsets and the scroll extents of the
//! document as it is now.
//!
//! The document is stale when it has changes the provider has not seen (nodes changed or
//! restyled, which includes what an animation moved) or when its revision or viewport is not the
//! one the provider laid out.

use std::cell::{Ref, RefCell};
use std::rc::Rc;

use boa_engine::Context;

use crate::state::{Shared, State, shared};
use crate::{Document, NodeId, Rect, Update};

/// What a layout comes to.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    /// Every rendered element's border box, in viewport coordinates.
    pub rects: Vec<(NodeId, Rect)>,
    /// How far each scroll container (and the page, by the root element) is scrolled, clamped to
    /// what it can reach: what `scrollTop` and `scrollLeft` answer.
    pub scroll_offsets: Vec<(NodeId, [f32; 2])>,
    /// `[scrollWidth, scrollHeight]` of every rendered element.
    pub scroll_extents: Vec<(NodeId, [f32; 2])>,
    /// The border boxes of every inline element that has content on a line, one for each line it
    /// has content on, in viewport coordinates: what `Element.getClientRects` answers for it
    /// (`Document::set_fragments` sets the same for a drawer that lays the page out itself).
    pub fragments: Vec<(NodeId, Vec<Rect>)>,
    /// The box of each element inside an inline `<svg>` in the drawing's own pixels (its content
    /// box's top left is the origin): what `getBBox` answers.
    pub svg_boxes: Vec<(NodeId, Rect)>,
}

/// Lays the document out for an [`Update`].
pub type LayoutProvider = Box<dyn FnMut(&Update) -> Layout>;

/// A boundary point of a range: a node and an offset in it (UTF-16 units in a text node, the
/// index of a child in an element).
pub type Boundary = (NodeId, usize);

/// The boxes of the text between two boundary points, in viewport coordinates.
pub type RangeProvider = Box<dyn Fn(Boundary, Boundary) -> Vec<Rect>>;

pub(crate) type RangeHook = Rc<dyn Fn(Boundary, Boundary) -> Vec<Rect>>;

/// The text position nearest a point of the viewport, if there is text to be near.
pub type PositionProvider = Box<dyn Fn(f32, f32) -> Option<Boundary>>;

pub(crate) type PositionHook = Rc<dyn Fn(f32, f32) -> Option<Boundary>>;

/// The bytes of a text field's value nearest a point of the viewport, and over it: where a click
/// puts the caret, and the character a double click takes the word of.
pub type FieldPositionProvider = Box<dyn Fn(NodeId, f32, f32) -> Option<(usize, usize)>>;

pub(crate) type FieldHook = Rc<dyn Fn(NodeId, f32, f32) -> Option<(usize, usize)>>;

impl Document {
    /// Makes `provider` the one that works the geometry out whenever a script reads it
    /// (`getBoundingClientRect`, `offset*`, `client*`, `scroll*`, `elementFromPoint`) or a
    /// pointer is aimed, and the document has changed since it last did. Its answer replaces the
    /// boxes, the scroll offsets and the extents, as [`Document::set_layout`] does for the boxes,
    /// but nobody is told on the spot: the observers hear at [`Document::deliver_layout`], and
    /// `scroll` is the scroll request's own. What changed stays in the next [`Document::update`].
    /// `None` takes the provider away; the boxes are then the ones the drawer reports.
    pub fn set_layout_provider(&self, provider: Option<LayoutProvider>) {
        let hook = provider.map(|provider| {
            // The hook lives in the state it works on: it holds the document weakly.
            let state = Rc::downgrade(&self.state);
            let view = Rc::downgrade(&self.view);
            let provider = RefCell::new(provider);
            Rc::new(move || {
                let (Some(state), Some(view)) = (state.upgrade(), view.upgrade()) else {
                    return;
                };
                // A provider that reads geometry itself is not asked again.
                if let Ok(mut provider) = provider.try_borrow_mut() {
                    Document { state, view }.lay_out(&mut **provider);
                }
            }) as Rc<dyn Fn()>
        });
        let mut state = self.state.borrow_mut();
        state.layout_hook = hook;
        state.laid = None;
    }

    /// Makes `provider` the one `Range.getClientRects` and `Range.getBoundingClientRect` ask for
    /// the boxes of the text between a range's ends, after the layout is brought up to date.
    /// Without one a range answers the box of the element it starts in. `None` takes it away.
    pub fn set_range_provider(&self, provider: Option<RangeProvider>) {
        self.state.borrow_mut().range_hook =
            provider.map(|provider| Rc::from(provider) as RangeHook);
    }

    /// Makes `provider` the one [`Document::text_position_at`] asks, after the layout is brought
    /// up to date. `None` takes it away.
    pub fn set_position_provider(&self, provider: Option<PositionProvider>) {
        self.state.borrow_mut().position_hook =
            provider.map(|provider| Rc::from(provider) as PositionHook);
    }

    /// Makes `provider` the one [`Document::field_offset_at`] asks. `None` takes it away.
    pub fn set_field_position_provider(&self, provider: Option<FieldPositionProvider>) {
        self.state.borrow_mut().field_hook =
            provider.map(|provider| Rc::from(provider) as FieldHook);
    }

    /// The bytes of the value of the text field `field` that the point `(x, y)` of the viewport
    /// is nearest to and over, after the layout is brought up to date. `None` without a field
    /// position provider, or for a field with no box.
    pub fn field_offset_at(&self, field: NodeId, x: f32, y: f32) -> Option<(usize, usize)> {
        refresh(&self.state);
        let hook = self.state.borrow().field_hook.clone()?;
        hook(field, x, y)
    }

    /// The text position nearest the point `(x, y)` of the viewport: a text node and a UTF-16
    /// offset in it. `None` where there is no text to be near, and without a position provider.
    pub fn text_position_at(&self, x: f32, y: f32) -> Option<Boundary> {
        refresh(&self.state);
        let hook = self.state.borrow().position_hook.clone()?;
        hook(x, y)
    }

    /// Lays the document out now if it changed since the provider last did. Nothing happens
    /// without a provider.
    pub fn ensure_layout(&self) {
        refresh(&self.state);
    }

    /// The state with its geometry up to date: what a read of geometry goes through.
    pub(crate) fn laid(&self) -> Ref<'_, State> {
        refresh(&self.state);
        self.state.borrow()
    }

    fn lay_out(&self, provider: &mut dyn FnMut(&Update) -> Layout) {
        {
            let state = self.state.borrow();
            let view = self.view.borrow();
            let unseen = !view.changed.is_empty() || !view.restyled.is_empty() || view.restyled_all;
            if !unseen && state.laid == Some(state.layout_key()) {
                return;
            }
        }
        let update = self.take_update();
        let key = (update.snapshot.revision, update.snapshot.viewport);
        let layout = provider(&update);
        {
            let mut state = self.state.borrow_mut();
            state.rects = layout.rects.into_iter().collect();
            state.scroll_offsets = layout.scroll_offsets.into_iter().collect();
            state.scroll_extents = layout.scroll_extents.into_iter().collect();
            state.fragments = layout.fragments.into_iter().collect();
            state.svg_boxes = layout.svg_boxes.into_iter().collect();
            state.laid = Some(key);
        }
        // The drawer has yet to hear what changed.
        self.view.borrow_mut().carried.merge(update.changes);
    }
}

/// Asks the layout provider, if there is one, to lay the document out when it is out of date.
pub(crate) fn refresh(state: &Shared) {
    let hook = state.borrow().layout_hook.clone();
    if let Some(hook) = hook {
        hook();
    }
}

/// The boxes an inline element has on each of its lines, from a layout brought up to date.
pub(crate) fn fragments_of(context: &Context, node: NodeId) -> Option<Vec<Rect>> {
    fresh(context).borrow().fragments.get(&node).cloned()
}

/// The state of `context`'s document with its geometry up to date.
pub(crate) fn fresh(context: &Context) -> Shared {
    let state = shared(context);
    refresh(&state);
    state
}
