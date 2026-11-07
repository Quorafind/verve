//! The display list: the boxes in the order CSS 2.1 Appendix E paints them.
//!
//! Each stacking context paints its own background and border, then the contexts with a negative
//! `z-index`, the in-flow blocks, the floats, the positioned boxes with `z-index: auto` or `0` in
//! tree order, and the contexts with a positive `z-index`. A positioned box with `z-index: auto`
//! paints like a context of its own, but what stacks inside it belongs to the context above.
//! Text and inline content go between the floats and the positioned boxes.
//!
//! Clips, scroll frames and opacity groups are not nested the way the boxes are: a box is cut by
//! the overflow of its containing blocks, which an absolute box does not share with its parent. So
//! the painter keeps the stack of effects that is open and, before each box, closes and opens the
//! difference. A scroll frame is a `PushScroll` after the clip of the container: the items inside
//! are placed as if it were not scrolled, and the replay moves them.

use verve_dom::Rect;

mod text;

pub(crate) use text::Text;

use crate::display_list::{Border, Fill, PaintItem, RasterSpec};
use crate::layout::Laid;
use crate::place::{Around, Placed, around, clips_in_frame, padding_box};
use crate::props::Position;
use crate::tree::BoxTree;

/// What is open around the items being painted.
#[derive(Clone, Debug, PartialEq)]
enum Effect {
    Clip(Rect, [f32; 4]),
    /// The content of a scroll frame, moved by the container's offset when replayed.
    Scroll(verve_dom::NodeId),
    /// An opacity group, told apart by the stacking context that makes it.
    Opacity(f32, usize),
}

/// How a box takes part in its stacking context.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    /// A stacking context at this `z-index`.
    Context(i32),
    /// A positioned box with `z-index: auto`: painted as a unit in tree order above the floats.
    Positioned,
    Flow,
}

#[derive(Default)]
struct Layers {
    below: Vec<(i32, usize)>,
    middle: Vec<usize>,
    above: Vec<(i32, usize)>,
}

struct Painter<'a> {
    text: &'a Text<'a>,
    tree: &'a BoxTree,
    laid: &'a Laid,
    placed: &'a Placed,
    items: Vec<PaintItem>,
    rasters: Vec<RasterSpec>,
    open: Vec<Effect>,
    groups: Vec<(f32, usize)>,
    outlines: Vec<usize>,
    /// The box whose background was given to the canvas.
    canvas_from: Option<usize>,
}

/// The items for the page, back to front, and the gradients they ask the renderer to rasterise.
pub(crate) fn build(
    tree: &BoxTree,
    laid: &Laid,
    placed: &Placed,
    text: &Text,
) -> (Vec<PaintItem>, Vec<RasterSpec>) {
    let Some(html) = tree.html else {
        return (Vec::new(), Vec::new());
    };
    let mut painter = Painter {
        text,
        tree,
        laid,
        placed,
        items: Vec::new(),
        rasters: Vec::new(),
        open: Vec::new(),
        groups: Vec::new(),
        outlines: Vec::new(),
        canvas_from: None,
    };
    painter.canvas(html);
    painter.context(html);
    painter.enter(&[]);
    (painter.items, painter.rasters)
}

impl Painter<'_> {
    /// The root element's background, or the body's when the root has none, covers the whole
    /// window and is not painted again by the element.
    fn canvas(&mut self, html: usize) {
        let tree = self.tree;
        let source = [Some(html), tree.body].into_iter().flatten().find(|index| {
            let paint = &tree.boxes[*index].props.paint;
            paint.background_color[3] > 0. || !paint.layers.is_empty()
        });
        let Some(source) = source else { return };
        let (width, height) = tree.viewport;
        let background = tree.boxes[source]
            .props
            .paint
            .fills([width, height], &mut self.rasters);
        self.canvas_from = Some(source);
        self.items.push(PaintItem::Box {
            node: tree.boxes[source].node,
            bounds: Rect::new(0., 0., width, height),
            radii: [0.; 4],
            background,
            border: Border::NONE,
            shadows: Vec::new(),
        });
    }

    fn kind(&self, index: usize) -> Kind {
        let node = &self.tree.boxes[index];
        let flex_item = node
            .flat_parent
            .is_some_and(|parent| self.tree.boxes[parent].props.display.is_flex_or_grid());
        let props = &node.props;
        if props.is_stacking_context(flex_item) {
            let stacked = props.position.is_positioned() || flex_item;
            Kind::Context(props.z_index.filter(|_| stacked).unwrap_or(0))
        } else if matches!(props.position, Position::Relative | Position::Absolute) {
            Kind::Positioned
        } else {
            Kind::Flow
        }
    }

    /// Paints a stacking context: `index` and everything stacked inside it.
    fn context(&mut self, index: usize) {
        let opacity = self.tree.boxes[index].props.opacity;
        if opacity < 1. {
            self.groups.push((opacity, index));
        }
        // Outlines wait for the end of the context they are painted in, not of an inner one.
        let outer_outlines = std::mem::take(&mut self.outlines);
        self.paint_box(index);
        let mut layers = Layers::default();
        self.gather(index, &mut layers);
        layers.below.sort_by_key(|(z, _)| *z);
        layers.above.sort_by_key(|(z, _)| *z);
        for (_, below) in layers.below {
            self.layer(below);
        }
        self.flow_with_floats(index);
        for middle in layers.middle {
            self.layer(middle);
        }
        for (_, above) in layers.above {
            self.layer(above);
        }
        self.flush_outlines();
        self.outlines = outer_outlines;
        self.scroller(index);
        if opacity < 1. {
            self.groups.pop();
        }
    }

    fn layer(&mut self, index: usize) {
        match self.kind(index) {
            Kind::Context(_) => self.context(index),
            Kind::Positioned | Kind::Flow => self.unit(index),
        }
    }

    /// Paints a box that is not a stacking context as a unit: itself, its in-flow blocks and its
    /// floats.
    fn unit(&mut self, index: usize) {
        self.paint_box(index);
        self.flow_with_floats(index);
        self.scroller(index);
    }

    fn flow_with_floats(&mut self, index: usize) {
        let mut floats = Vec::new();
        let mut holders = vec![index];
        self.flow(index, &mut floats, &mut holders);
        for float in floats {
            self.unit(float);
        }
        // The text and inline boxes of the blocks go over the floats, as CSS 2.1 Appendix E has it.
        for &holder in &holders {
            self.inline_content(holder);
        }
        // A scroller's bars go over everything inside it, its text too: innermost first.
        for &holder in holders.iter().skip(1).rev() {
            self.scroller(holder);
        }
    }

    /// The in-flow, non-positioned descendants of `parent`, painted in tree order; floats are
    /// set aside for after.
    fn flow(&mut self, parent: usize, floats: &mut Vec<usize>, holders: &mut Vec<usize>) {
        let tree = self.tree;
        for &child in &tree.boxes[parent].children {
            if self.kind(child) != Kind::Flow {
                continue;
            }
            if tree.boxes[child].props.float {
                floats.push(child);
                continue;
            }
            self.paint_box(child);
            holders.push(child);
            self.flow(child, floats, holders);
        }
    }

    /// Sorts the stacked descendants of `parent` into the layers of the context above them.
    fn gather(&self, parent: usize, layers: &mut Layers) {
        for &child in &self.tree.boxes[parent].children {
            match self.kind(child) {
                Kind::Context(z) if z < 0 => layers.below.push((z, child)),
                Kind::Context(z) if z > 0 => layers.above.push((z, child)),
                Kind::Context(_) => layers.middle.push(child),
                Kind::Positioned => {
                    layers.middle.push(child);
                    self.gather(child, layers);
                }
                Kind::Flow => self.gather(child, layers),
            }
        }
    }

    /// Paints one box's own background, border and shadows, and its hitbox.
    fn paint_box(&mut self, index: usize) {
        let tree = self.tree;
        let node = &tree.boxes[index];
        let rect = self.placed.paint_rect[index];
        if node.anonymous || rect.width <= 0. || rect.height <= 0. || !node.props.input.visible {
            return;
        }
        let around = around(tree, self.laid, self.placed, index);
        let Some(bounds) = visible_part(rect, clips_in_frame(&around)) else {
            return;
        };
        self.open_around(&around);
        let paint = &node.props.paint;
        let background = if self.canvas_from == Some(index) {
            Vec::new()
        } else {
            paint.fills([rect.width, rect.height], &mut self.rasters)
        };
        if !background.is_empty() || paint.border.is_visible() || !paint.shadows.is_empty() {
            self.items.push(PaintItem::Box {
                node: node.node,
                bounds: rect,
                radii: paint.radii(rect.width, rect.height),
                background,
                border: paint.border.clone(),
                shadows: paint.shadows.clone(),
            });
        }
        if paint.outline.is_some() {
            self.outlines.push(index);
        }
        if node.props.input.pointer_events {
            self.items.push(PaintItem::Hitbox {
                node: node.node,
                bounds,
                cursor: node.props.input.cursor,
                app_region: node.props.input.app_region,
            });
        }
    }

    /// Paints the outlines that waited for the end of their context.
    fn flush_outlines(&mut self) {
        let tree = self.tree;
        for index in std::mem::take(&mut self.outlines) {
            let node = &tree.boxes[index];
            let Some(outline) = &node.props.paint.outline else {
                continue;
            };
            let around = around(tree, self.laid, self.placed, index);
            self.open_around(&around);
            let rect = self.placed.paint_rect[index];
            let grow = outline.offset + outline.width;
            let line = outline.line;
            let radii = node
                .props
                .paint
                .radii(rect.width, rect.height)
                .map(|radius| if radius > 0. { radius + grow } else { 0. });
            self.items.push(PaintItem::Box {
                node: node.node,
                bounds: Rect::new(
                    rect.x - grow,
                    rect.y - grow,
                    rect.width + 2. * grow,
                    rect.height + 2. * grow,
                ),
                radii,
                background: Vec::<Fill>::new(),
                border: Border {
                    widths: [outline.width; 4],
                    colors: [outline.color; 4],
                    styles: [line; 4],
                },
                shadows: Vec::new(),
            });
        }
    }

    /// The scrollbars' place for a box that is a frame; the scroll offset is read from here.
    fn scroller(&mut self, index: usize) {
        let tree = self.tree;
        let node = &tree.boxes[index];
        if !self.placed.frame[index] || !node.props.input.visible {
            return;
        }
        let around = around(tree, self.laid, self.placed, index);
        self.open_around(&around);
        let viewport = if Some(index) == tree.html {
            Rect::new(0., 0., tree.viewport.0, tree.viewport.1)
        } else {
            padding_box(tree, self.laid, self.placed.paint_rect[index], index).0
        };
        self.items.push(PaintItem::Scroller {
            node: node.node,
            viewport,
            content: self.placed.content[index],
            offset: self.placed.offset[index],
            scrollbar: node.props.input.scrollbar,
        });
    }

    /// Opens what the items of a box need around them: the opacity groups being painted, then
    /// the clips and scroll frames of its containing blocks.
    fn open_around(&mut self, around: &[Around]) {
        let mut wanted: Vec<Effect> = self
            .groups
            .iter()
            .map(|(opacity, owner)| Effect::Opacity(*opacity, *owner))
            .collect();
        wanted.extend(around.iter().map(|entry| match entry {
            Around::Clip(rect, radii) => Effect::Clip(*rect, *radii),
            Around::Scroll(node) => Effect::Scroll(*node),
        }));
        self.enter(&wanted);
    }

    /// Closes the effects that are open and not wanted, and opens the wanted ones that are not.
    fn enter(&mut self, wanted: &[Effect]) {
        let shared = self
            .open
            .iter()
            .zip(wanted)
            .take_while(|(a, b)| a == b)
            .count();
        while self.open.len() > shared {
            match self.open.pop() {
                Some(Effect::Clip(..)) => self.items.push(PaintItem::PopClip),
                Some(Effect::Scroll(_)) => self.items.push(PaintItem::PopScroll),
                Some(Effect::Opacity(..)) => self.items.push(PaintItem::PopOpacity),
                None => break,
            }
        }
        for effect in &wanted[shared..] {
            self.items.push(match effect {
                Effect::Clip(bounds, radii) => PaintItem::PushClip {
                    bounds: *bounds,
                    radii: *radii,
                },
                Effect::Scroll(node) => PaintItem::PushScroll { node: *node },
                Effect::Opacity(opacity, _) => PaintItem::PushOpacity(*opacity),
            });
            self.open.push(effect.clone());
        }
    }
}

/// The part of `rect` that every clip leaves; `None` when nothing of it shows.
fn visible_part(rect: Rect, clips: impl Iterator<Item = (Rect, [f32; 4])>) -> Option<Rect> {
    let mut part = rect;
    for (clip, _) in clips {
        part = part.intersection(&clip)?;
    }
    (part.width > 0. && part.height > 0.).then_some(part)
}
