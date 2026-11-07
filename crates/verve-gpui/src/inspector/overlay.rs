//! What the inspector draws over its page's window: the box of the node it points at, in DevTools'
//! colours, and the layer that turns the pointer into a pick. Both are elements of their own
//! above the page's, drawn from the shell's own display list, never from the renderer.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, Bounds, CursorStyle, DispatchPhase, HitboxBehavior, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, Pixels, Rgba, Styled, Window, canvas, fill, point, px, rgba,
    size,
};
use verve_dom::{NodeId, Rect};

use super::boxmodel::{self, BoxModel, Edges};
use super::model::Highlight;
use crate::dom_view::DomState;

/// What the pointer did over a page window that is being picked from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickEvent {
    /// The pointer is over this node, or over none.
    Hover(Option<NodeId>),
    /// A click on this node, or on none.
    Click(Option<NodeId>),
}

/// What a page window calls with what the pointer did.
pub type Picker = Rc<dyn Fn(PickEvent)>;

/// DevTools' colours for the four boxes of a highlighted node.
const CONTENT: u32 = 0x6fa8_dcaa;
const PADDING: u32 = 0x93c4_7d8c;
const BORDER: u32 = 0xffe5_99a8;
const MARGIN: u32 = 0xf6b2_6ba8;

/// The four bands between `outer` and the `inner` box inside it: above, below, left and right.
/// A band with no width is a rectangle with no size.
pub fn bands(outer: Rect, inner: Rect) -> [Rect; 4] {
    let (outer_right, outer_bottom) = (outer.x + outer.width, outer.y + outer.height);
    let (inner_right, inner_bottom) = (inner.x + inner.width, inner.y + inner.height);
    [
        Rect::new(outer.x, outer.y, outer.width, (inner.y - outer.y).max(0.)),
        Rect::new(
            outer.x,
            inner_bottom,
            outer.width,
            (outer_bottom - inner_bottom).max(0.),
        ),
        Rect::new(outer.x, inner.y, (inner.x - outer.x).max(0.), inner.height),
        Rect::new(
            inner_right,
            inner.y,
            (outer_right - inner_right).max(0.),
            inner.height,
        ),
    ]
}

/// The bands of `outer` that `edges` name, for the sides that have a width: what is drawn of a
/// box whose inside is not known.
fn known_bands(outer: Rect, edges: &Edges) -> Vec<Rect> {
    let Edges {
        top,
        right,
        bottom,
        left,
    } = *edges;
    let (width, height) = (outer.width, outer.height);
    let mut bands = Vec::new();
    bands.extend(top.map(|t| Rect::new(outer.x, outer.y, width, t.min(height))));
    bands.extend(bottom.map(|b| {
        let b = b.min(height);
        Rect::new(outer.x, outer.y + height - b, width, b)
    }));
    bands.extend(left.map(|l| Rect::new(outer.x, outer.y, l.min(width), height)));
    bands.extend(right.map(|r| {
        let r = r.min(width);
        Rect::new(outer.x + width - r, outer.y, r, height)
    }));
    bands
}

/// What to paint for a node's box model, back to front: each box as the colour it is drawn in. A
/// side whose width only the layout can resolve (`auto`, a percentage) is not drawn, and with
/// such a padding neither is the content, whose size is not known.
pub fn layers(model: &BoxModel) -> Vec<(Rect, u32)> {
    let mut layers = Vec::new();
    layers.extend(bands(model.margin_box(), model.border_box).map(|band| (band, MARGIN)));
    layers.extend(bands(model.border_box, model.padding_box()).map(|band| (band, BORDER)));
    match model.content_box() {
        Some(content) => {
            layers.extend(bands(model.padding_box(), content).map(|band| (band, PADDING)));
            layers.push((content, CONTENT));
        }
        None => {
            let known = known_bands(model.padding_box(), &model.padding);
            layers.extend(known.into_iter().map(|band| (band, PADDING)));
        }
    }
    layers
}

fn bounds_in(origin: gpui::Point<Pixels>, rect: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(origin.x + px(rect.x), origin.y + px(rect.y)),
        size(px(rect.width), px(rect.height)),
    )
}

/// The node under `(x, y)` of the page, as the window has it laid out and scrolled.
fn node_under(state: &Rc<RefCell<DomState>>, x: f32, y: f32) -> Option<NodeId> {
    let state = state.borrow();
    let predicted = &state.predicted;
    state
        .list
        .as_ref()?
        .hit_test(x, y, &|node| predicted.get(&node).copied())
}

/// The layer that draws `highlight` over the page: the margin, border, padding and content of its
/// node, where the window's own list has them now.
pub(crate) fn highlight_layer(
    state: Rc<RefCell<DomState>>,
    highlight: Highlight,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window: &mut Window, _: &mut App| {
            let found = {
                let state = state.borrow();
                let Some(list) = state.list.as_ref() else {
                    return;
                };
                let predicted = &state.predicted;
                boxmodel::border_box(list, highlight.node, &|node| predicted.get(&node).copied())
            };
            let Some(border_box) = found else {
                return;
            };
            let model = BoxModel {
                margin: highlight.margin,
                border: highlight.border,
                padding: highlight.padding,
                border_box,
                words: Default::default(),
            };
            for (rect, colour) in layers(&model) {
                if rect.width > 0. && rect.height > 0. {
                    let colour: Rgba = rgba(colour);
                    window.paint_quad(fill(bounds_in(bounds.origin, rect), colour));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

/// The layer that takes the pointer while a person picks a node: it covers the page, shows a
/// crosshair, and tells `picker` the node under the pointer and the click.
pub(crate) fn pick_layer(state: Rc<RefCell<DomState>>, picker: Picker) -> impl IntoElement {
    canvas(
        |bounds, window: &mut Window, _| window.insert_hitbox(bounds, HitboxBehavior::BlockMouse),
        move |bounds, hitbox, window: &mut Window, _: &mut App| {
            window.set_cursor_style(CursorStyle::Crosshair, &hitbox);
            let at = move |position: gpui::Point<Pixels>| {
                let x = f32::from(position.x - bounds.origin.x);
                let y = f32::from(position.y - bounds.origin.y);
                (x, y)
            };
            let (moving, over) = (Rc::clone(&state), Rc::clone(&picker));
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, _| {
                if phase == DispatchPhase::Bubble {
                    let (x, y) = at(event.position);
                    over(PickEvent::Hover(node_under(&moving, x, y)));
                }
            });
            let (clicked, state) = (picker, state.clone());
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx: &mut App| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    let (x, y) = at(event.position);
                    clicked(PickEvent::Click(node_under(&state, x, y)));
                    cx.stop_propagation();
                }
            });
        },
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::boxmodel::Sides;

    fn area(rect: &Rect) -> f32 {
        rect.width * rect.height
    }

    #[test]
    fn the_bands_and_the_inner_box_cover_the_outer_box_exactly() {
        let outer = Rect::new(10., 20., 100., 60.);
        let inner = Rect::new(25., 30., 50., 40.);
        let bands = bands(outer, inner);
        let covered: f32 = bands.iter().map(area).sum::<f32>() + area(&inner);
        assert_eq!(covered, area(&outer));
        assert_eq!(bands[0], Rect::new(10., 20., 100., 10.));
        assert_eq!(bands[2], Rect::new(10., 30., 15., 40.));
    }

    /// A box with a 5px margin, a 1px border and `padding` all round (`None`: not resolved).
    fn model_of(padding: Option<f32>) -> BoxModel {
        let edge = |n| Sides {
            top: n,
            right: n,
            bottom: n,
            left: n,
        };
        BoxModel {
            margin: edge(Some(5.)),
            border: edge(Some(1.)),
            padding: edge(padding),
            border_box: Rect::new(20., 20., 50., 30.),
            words: Default::default(),
        }
    }

    #[test]
    fn a_padding_the_layout_has_to_resolve_is_not_drawn_nor_is_the_content() {
        let layers = layers(&model_of(None));
        assert_eq!(layers.len(), 8);
        assert!(layers.iter().all(|(_, colour)| *colour != CONTENT));
        assert!(layers.iter().all(|(_, colour)| *colour != PADDING));
    }

    #[test]
    fn a_node_is_painted_as_margin_border_padding_and_content_from_the_outside_in() {
        let model = model_of(Some(2.));
        let layers = layers(&model);
        let colours: Vec<u32> = layers.iter().map(|(_, colour)| *colour).collect();
        let mut expected = vec![MARGIN; 4];
        expected.extend([BORDER; 4]);
        expected.extend([PADDING; 4]);
        expected.push(CONTENT);
        assert_eq!(colours, expected);
        let (content, _) = layers[12];
        assert_eq!(content, Rect::new(23., 23., 44., 24.));
        let total: f32 = layers.iter().map(|(rect, _)| area(rect)).sum();
        assert_eq!(total, area(&model.margin_box()));
    }
}
