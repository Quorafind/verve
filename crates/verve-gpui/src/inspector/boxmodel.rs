//! The box model of a node: where the layout put its border box, and the margin, border and
//! padding around the content inside it.
//!
//! The border box comes from the page's display list — the box the layout painted, or else the
//! hitbox it made — so it is where the page is drawn, scrolled as the shell has scrolled it. The
//! widths come from the node's computed style; a border takes its widths from the painted box
//! when there is one, since a border with no style has no width however the style is written.
//! A percentage or `auto` is not a length the shell can resolve: that side has no width (`None`)
//! and keeps the style's own text, which the diagram shows and the highlight leaves undrawn.

use verve_dom::{NodeId, Rect, Snapshot};
use verve_layout::{DisplayList, PaintItem};

/// A value for each of the four sides of a box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sides<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

/// The widths of the four sides of a box; a side whose width the layout would have to resolve
/// (`auto`, a percentage) has none.
pub type Edges = Sides<Option<f32>>;

impl Edges {
    /// The widths as numbers, an unresolved side counting as zero.
    pub fn px(&self) -> Sides<f32> {
        let width = |side: Option<f32>| side.unwrap_or(0.);
        Sides {
            top: width(self.top),
            right: width(self.right),
            bottom: width(self.bottom),
            left: width(self.left),
        }
    }

    /// Whether every side has a width.
    pub fn is_resolved(&self) -> bool {
        [self.top, self.right, self.bottom, self.left]
            .iter()
            .all(Option::is_some)
    }

    pub fn horizontal(&self) -> f32 {
        let px = self.px();
        px.left + px.right
    }

    pub fn vertical(&self) -> f32 {
        let px = self.px();
        px.top + px.bottom
    }
}

/// Which widths of the box model a style names.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Margin,
    Padding,
    Border,
}

/// A node's four boxes, from the outside in.
#[derive(Clone, Debug, PartialEq)]
pub struct BoxModel {
    pub margin: Edges,
    pub border: Edges,
    pub padding: Edges,
    /// The border box, in the window.
    pub border_box: Rect,
    /// What the style says of each side of the margin, border and padding: shown where a side has
    /// no width.
    pub words: [Sides<String>; 3],
}

impl BoxModel {
    /// The padding box: inside the border.
    pub fn padding_box(&self) -> Rect {
        inset(self.border_box, &self.border)
    }

    /// The content box: inside the padding. Not known while a side of the padding is one only the
    /// layout can resolve.
    pub fn content_box(&self) -> Option<Rect> {
        self.padding
            .is_resolved()
            .then(|| inset(self.padding_box(), &self.padding))
    }

    /// The margin box: outside the border box, an unresolved side adding nothing.
    pub fn margin_box(&self) -> Rect {
        let m = self.margin.px();
        Rect::new(
            self.border_box.x - m.left,
            self.border_box.y - m.top,
            self.border_box.width + self.margin.horizontal(),
            self.border_box.height + self.margin.vertical(),
        )
    }
}

/// `rect` taken in by `edges`; a side that would cross the other is left at zero.
fn inset(rect: Rect, edges: &Edges) -> Rect {
    let px = edges.px();
    Rect::new(
        rect.x + px.left,
        rect.y + px.top,
        (rect.width - edges.horizontal()).max(0.),
        (rect.height - edges.vertical()).max(0.),
    )
}

/// The border box of `node` in the window: the box the layout painted for it (whole, not cut to
/// the clips) or else the part of it a pointer can reach. `offset_of` is how far the shell has
/// scrolled a container, if it is not as the list has it.
pub fn border_box(
    list: &DisplayList,
    node: NodeId,
    offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
) -> Option<Rect> {
    let placed = list.placed(offset_of);
    let painted = placed.iter().find_map(|item| match item.item {
        PaintItem::Box {
            node: n, bounds, ..
        } if *n == node => Some(item.in_window(*bounds)),
        _ => None,
    });
    painted.or_else(|| {
        placed
            .iter()
            .filter_map(|item| match item.item {
                PaintItem::Hitbox {
                    node: n, bounds, ..
                } if *n == node => Some(item.in_window(*bounds)),
                _ => None,
            })
            .reduce(|a, b| a.union(&b))
    })
}

/// The style's text for one side of `edge` of `node`.
fn style_text(snapshot: &Snapshot, node: NodeId, edge: Edge, side: &str) -> Option<String> {
    let property = match edge {
        Edge::Margin => format!("margin-{side}"),
        Edge::Padding => format!("padding-{side}"),
        Edge::Border => format!("border-{side}-width"),
    };
    snapshot.style(node, &property).map(str::to_owned)
}

/// The widths of `edge` of `node`: a side with no style is zero, one that is not a length is
/// unresolved.
fn edges(snapshot: &Snapshot, node: NodeId, edge: Edge) -> Edges {
    let size = snapshot
        .style(node, "font-size")
        .and_then(|text| verve_dom::css::length(text, 16.))
        .unwrap_or(16.);
    let side = |side: &str| match style_text(snapshot, node, edge, side) {
        None => Some(0.),
        Some(text) => verve_dom::css::length(&text, size),
    };
    Sides {
        top: side("top"),
        right: side("right"),
        bottom: side("bottom"),
        left: side("left"),
    }
}

/// What the style says of each side of `edge` of `node`, `0` for a side it says nothing of.
fn words_of(snapshot: &Snapshot, node: NodeId, edge: Edge) -> Sides<String> {
    let side = |side: &str| style_text(snapshot, node, edge, side).unwrap_or_else(|| "0".into());
    Sides {
        top: side("top"),
        right: side("right"),
        bottom: side("bottom"),
        left: side("left"),
    }
}

/// The widths the painted border of `node` has, if it painted one.
fn painted_border(list: &DisplayList, node: NodeId) -> Option<Edges> {
    list.items.iter().find_map(|item| match item {
        PaintItem::Box {
            node: n, border, ..
        } if *n == node => Some(Sides {
            top: Some(border.widths[0]),
            right: Some(border.widths[1]),
            bottom: Some(border.widths[2]),
            left: Some(border.widths[3]),
        }),
        _ => None,
    })
}

/// The margin, border and padding widths of `node`, without a place: what a highlight is drawn
/// from, once a window knows where the border box is.
pub fn widths(snapshot: &Snapshot, list: Option<&DisplayList>, node: NodeId) -> [Edges; 3] {
    let border = list
        .and_then(|list| painted_border(list, node))
        .unwrap_or_else(|| edges(snapshot, node, Edge::Border));
    [
        edges(snapshot, node, Edge::Margin),
        border,
        edges(snapshot, node, Edge::Padding),
    ]
}

/// The box model of `node`, if the layout gave it a box.
pub fn of(
    snapshot: &Snapshot,
    list: &DisplayList,
    node: NodeId,
    offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
) -> Option<BoxModel> {
    let border_box = border_box(list, node, offset_of)?;
    let [margin, border, padding] = widths(snapshot, Some(list), node);
    Some(BoxModel {
        margin,
        border,
        padding,
        border_box,
        words: [
            words_of(snapshot, node, Edge::Margin),
            words_of(snapshot, node, Edge::Border),
            words_of(snapshot, node, Edge::Padding),
        ],
    })
}

#[cfg(test)]
mod tests {
    use verve_layout::Border;

    use super::*;
    use crate::inspector::tree::tests::page;

    fn nothing(_: NodeId) -> Option<[f32; 2]> {
        None
    }

    fn list_with(items: Vec<PaintItem>) -> DisplayList {
        DisplayList {
            revision: 1,
            items,
            ..DisplayList::default()
        }
    }

    fn hitbox(node: NodeId, bounds: Rect) -> PaintItem {
        PaintItem::Hitbox {
            node,
            bounds,
            cursor: verve_layout::Cursor::Auto,
            app_region: None,
        }
    }

    #[test]
    fn the_four_boxes_nest_from_the_border_box_in() {
        let (_runtime, document) = page(
            "<div id=a style='margin: 10px 20px; padding: 4px 6px; border: 2px solid black'>x</div>",
        );
        let snapshot = document.update().snapshot;
        let div = snapshot
            .dom
            .query(snapshot.dom.body, "#a")
            .expect("the div");
        let border = Border {
            widths: [2.; 4],
            ..Border::NONE
        };
        let painted = PaintItem::Box {
            node: div,
            bounds: Rect::new(28., 18., 100., 50.),
            radii: [0.; 4],
            background: Vec::new(),
            border,
            shadows: Vec::new(),
        };
        let model = of(&snapshot, &list_with(vec![painted]), div, &nothing).expect("a box");
        let edges = |top, right, bottom, left| Sides {
            top: Some(top),
            right: Some(right),
            bottom: Some(bottom),
            left: Some(left),
        };
        assert_eq!(model.border_box, Rect::new(28., 18., 100., 50.));
        assert_eq!(model.margin, edges(10., 20., 10., 20.));
        assert_eq!(model.padding, edges(4., 6., 4., 6.));
        assert_eq!(model.border, edges(2., 2., 2., 2.));
        assert_eq!(model.margin_box(), Rect::new(8., 8., 140., 70.));
        assert_eq!(model.padding_box(), Rect::new(30., 20., 96., 46.));
        assert_eq!(model.content_box(), Some(Rect::new(36., 24., 84., 38.)));
    }

    #[test]
    fn a_node_with_only_a_hitbox_has_its_hitbox_for_a_border_box() {
        let (_runtime, document) = page("<p id=a>x</p>");
        let snapshot = document.update().snapshot;
        let p = snapshot.dom.query(snapshot.dom.body, "#a").expect("the p");
        let list = list_with(vec![hitbox(p, Rect::new(8., 8., 784., 16.))]);
        let model = of(&snapshot, &list, p, &nothing).expect("a box");
        assert_eq!(model.border_box, Rect::new(8., 8., 784., 16.));
        assert_eq!(model.content_box(), Some(model.border_box));
        assert!(of(&snapshot, &list, p + 1, &nothing).is_none());
    }

    #[test]
    fn a_container_the_shell_scrolled_moves_the_box_in_it() {
        let (_runtime, document) = page("<p id=a>x</p>");
        let snapshot = document.update().snapshot;
        let p = snapshot.dom.query(snapshot.dom.body, "#a").expect("the p");
        let scroller = snapshot.dom.body;
        let list = list_with(vec![
            PaintItem::PushScroll { node: scroller },
            hitbox(p, Rect::new(8., 100., 50., 20.)),
            PaintItem::PopScroll,
        ]);
        let scrolled = |node: NodeId| (node == scroller).then_some([0., 40.]);
        let model = of(&snapshot, &list, p, &scrolled).expect("a box");
        assert_eq!(model.border_box, Rect::new(8., 60., 50., 20.));
    }

    #[test]
    fn a_length_that_needs_the_layout_has_no_width_and_keeps_its_words() {
        let (_runtime, document) = page("<div id=a style='padding: 10%; margin: 0 auto'>x</div>");
        let snapshot = document.update().snapshot;
        let div = snapshot
            .dom
            .query(snapshot.dom.body, "#a")
            .expect("the div");
        let [margin, _, padding] = widths(&snapshot, None, div);
        assert_eq!(margin.top, Some(0.));
        assert_eq!((margin.left, margin.right), (None, None));
        assert!(!padding.is_resolved());
        let list = list_with(vec![hitbox(div, Rect::new(8., 8., 100., 20.))]);
        let model = of(&snapshot, &list, div, &nothing).expect("a box");
        assert_eq!(model.words[0].left, "auto");
        assert_eq!(model.words[2].top, "10%");
        // The content box is not guessed from a padding nobody resolved.
        assert_eq!(model.content_box(), None);
    }
}
