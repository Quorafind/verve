//! From the document to boxes: which elements make a box, which box each one sits in, and what
//! taffy is given.
//!
//! The boxes follow the flat tree (a shadow host's shadow root, a slot's assigned nodes). An
//! element with `display: none` and its subtree make no box; `contents`, an inline element and
//! the rows and row groups of a table make none of their own and their children stand in their
//! place. A block whose content is inline (text, inline elements, atomic inlines) is a leaf that
//! lays its lines out (see `crate::inline` and `flow.rs`); text makes no box of its own.
//!
//! Every box has two parents. Its *flat parent* is the box it is painted inside, which fixes the
//! stacking order. Its *layout parent* is the box taffy lays it out in, which differs for an
//! absolute or fixed box: it hangs under its containing block (taffy only knows its parent), and
//! a stand-in with the same insets stays under the flat parent to find where the box would have
//! stood, for the insets the style leaves `auto` on both sides.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use taffy::style_helpers::{auto, length};
use taffy::{LengthPercentage, Size, TaffyTree};
use verve_dom::{NodeId, Snapshot};

use crate::fonts::FontRegistry;
use crate::grid::Areas;
use crate::inline::{Ifc, InlineLayout, Parts};
use crate::measure::TextMeasurer;
use crate::props::{BoxProps, Display, Position, ReadContext, read};
use crate::value::{Units, pixels, words};

/// One box of the page.
pub(crate) struct BoxNode {
    pub node: NodeId,
    pub props: BoxProps,
    pub taffy: taffy::NodeId,
    /// The box taffy lays this one out in; `None` is the initial containing block.
    pub parent: Option<usize>,
    /// The box this one is painted inside, in the flat tree.
    pub flat_parent: Option<usize>,
    /// The boxes painted inside this one, in tree order (`order` applied for flex and grid).
    pub children: Vec<usize>,
    /// Where the box would be without its insets, for an absolute box laid out elsewhere.
    pub ghost: Option<taffy::NodeId>,
    /// Which axes (x, y) take their position from the ghost.
    pub follows_ghost: [bool; 2],
    /// A box that holds a run of inline content beside block boxes: it stands for no element
    /// (`node` is the element it is in) and draws nothing.
    pub anonymous: bool,
    /// An atomic inline: a box in a line, laid out on its own and placed by the line.
    pub atomic: bool,
}

/// An atomic inline, for the sizing that takes place before the lines are laid out.
pub(crate) struct AtomRef {
    pub box_index: usize,
    /// The box of the inline formatting context the atom is in.
    pub holder: usize,
    /// Its place among that context's atoms.
    pub slot: usize,
    /// How deep in the tree: the deepest are sized first.
    pub depth: usize,
}

/// The boxes of a page, in an order where a parent always comes before its children.
pub(crate) struct BoxTree {
    pub boxes: Vec<BoxNode>,
    /// The tree taffy lays out; a block of inline content is a leaf holding its [`Ifc`].
    pub taffy: TaffyTree<Ifc>,
    /// The initial containing block: the viewport at the origin.
    pub root: taffy::NodeId,
    pub viewport: (f32, f32),
    /// The box of the root element.
    pub html: Option<usize>,
    /// The box of `<body>`, whose background the page may take over.
    pub body: Option<usize>,
    pub by_node: HashMap<NodeId, usize>,
    /// How many style values fell back to their initial value.
    pub unsupported: u32,
    /// Every atomic inline.
    pub atoms: Vec<AtomRef>,
    /// The boxes that hold inline content, in tree order.
    pub holders: Vec<usize>,
    /// The lines of each holder, once laid out (see `atoms.rs`).
    pub inline: Vec<Option<Arc<InlineLayout>>>,
    /// Where each atomic inline is in its holder's content box.
    pub atom_at: Vec<Option<[f32; 2]>>,
}

/// Where a table cell goes in the grid a table becomes.
#[derive(Clone, Copy)]
struct Cell {
    row: u16,
    column: u16,
    row_span: u16,
    column_span: u16,
}

pub(crate) struct Builder<'a> {
    pub snapshot: &'a Snapshot,
    pub root_font_size: f32,
    pub boxes: Vec<BoxNode>,
    /// The ancestors that can contain absolute boxes, innermost last: a box, whether it is
    /// positioned and whether it has a transform.
    pub containers: Vec<(usize, bool, bool)>,
    cell: Option<Cell>,
    pub unsupported: u32,
    pub measurer: Arc<dyn TextMeasurer>,
    pub fonts: &'a FontRegistry,
    /// The inline content of the boxes that hold it, by box.
    pub ifcs: Vec<(usize, Parts)>,
}

/// The boxes of the document and a taffy tree holding their styles, ready to be laid out. Text is
/// measured with `measurer`; `fonts` are the faces the page registered.
pub(crate) fn build(
    snapshot: &Snapshot,
    measurer: &Arc<dyn TextMeasurer>,
    fonts: &FontRegistry,
) -> BoxTree {
    let root_font_size = snapshot
        .style(snapshot.dom.html, "font-size")
        .and_then(|text| pixels(text, &Units::new(16., 16., snapshot.viewport)))
        .unwrap_or(16.);
    let mut builder = Builder {
        snapshot,
        root_font_size,
        boxes: Vec::new(),
        containers: Vec::new(),
        cell: None,
        unsupported: 0,
        measurer: measurer.clone(),
        fonts,
        ifcs: Vec::new(),
    };
    let mut top = Vec::new();
    builder.visit(snapshot.dom.html, None, None, &mut top);
    builder.finish(snapshot.viewport)
}

impl Builder<'_> {
    /// Makes the boxes for `node`, appended to `out`: none for text, `display: none` and the
    /// like, one for an ordinary element, and its children's for a transparent one.
    fn visit(
        &mut self,
        node: NodeId,
        parent: Option<usize>,
        areas: Option<&Areas>,
        out: &mut Vec<usize>,
    ) {
        let Some(found) = self.snapshot.dom.get(node) else {
            return;
        };
        if !found.is_element() {
            return;
        }
        let context = ReadContext {
            snapshot: self.snapshot,
            root_font_size: self.root_font_size,
            parent_areas: areas,
            blockify: parent.is_some_and(|p| self.boxes[p].props.display.is_flex_or_grid()),
        };
        let mut props = read(&context, node);
        if found.tag() == Some("slot") && self.snapshot.style(node, "display").is_none() {
            props.display = Display::Contents;
        }
        self.unsupported += props.unsupported;
        if props.display.is_transparent() {
            self.visit_children(node, parent, areas, out);
        } else if props.display.has_box() {
            self.make_box(node, props, parent, out);
        }
    }

    pub(crate) fn visit_children(
        &mut self,
        node: NodeId,
        parent: Option<usize>,
        areas: Option<&Areas>,
        out: &mut Vec<usize>,
    ) {
        for child in self.snapshot.dom.flat_children(node) {
            self.visit(child, parent, areas, out);
        }
    }

    pub(crate) fn make_box(
        &mut self,
        node: NodeId,
        mut props: BoxProps,
        parent: Option<usize>,
        out: &mut Vec<usize>,
    ) {
        if let Some(cell) = self.cell.take() {
            place_cell(&mut props, cell);
        }
        let index = self.boxes.len();
        let layout_parent = self.layout_parent(&props, parent);
        let ghost_axes = ghost_axes(&props, parent != layout_parent);
        self.boxes.push(BoxNode {
            node,
            taffy: taffy::NodeId::new(0),
            parent: layout_parent,
            flat_parent: parent,
            children: Vec::new(),
            ghost: None,
            follows_ghost: ghost_axes,
            anonymous: false,
            atomic: false,
            props,
        });
        let props = &self.boxes[index].props;
        let display = props.display;
        let areas = props.grid_areas.clone();
        let container = (index, props.position.is_positioned(), props.contains_fixed);
        self.containers.push(container);
        let mut children = Vec::new();
        if display == Display::Table {
            self.visit_table(node, index, &mut children);
        } else {
            let areas = (!areas.is_empty()).then_some(&areas);
            self.build_content(node, index, areas, &mut children);
        }
        self.containers.pop();
        if self.boxes[index].props.display.is_flex_or_grid() {
            let boxes = &self.boxes;
            children.sort_by_key(|child| boxes[*child].props.order);
        }
        self.boxes[index].children = children;
        out.push(index);
    }

    /// The box taffy lays a box out in: its flat parent, except for an absolute box (its
    /// containing block: the nearest positioned or transformed ancestor) and a fixed box (the
    /// nearest transformed one, else the viewport).
    fn layout_parent(&self, props: &BoxProps, flat_parent: Option<usize>) -> Option<usize> {
        let nearest = |wants: fn(&(usize, bool, bool)) -> bool| {
            self.containers
                .iter()
                .rev()
                .find(|entry| wants(entry))
                .map(|entry| entry.0)
        };
        match props.position {
            Position::Absolute => {
                nearest(|(_, positioned, transformed)| *positioned || *transformed)
            }
            Position::Fixed => nearest(|(_, _, transformed)| *transformed),
            _ => flat_parent,
        }
    }

    /// A table becomes a grid: a column per cell of the widest row, each cell in the row and
    /// column its place in the table says, `colspan` and `rowspan` as spans.
    fn visit_table(&mut self, table: NodeId, index: usize, out: &mut Vec<usize>) {
        let mut rows: Vec<Vec<NodeId>> = Vec::new();
        let mut captions: Vec<NodeId> = Vec::new();
        self.collect_rows(table, &mut rows, &mut captions);
        let (placed, columns) = self.place_cells(&rows, u16::from(!captions.is_empty()));
        self.style_table(index, columns);
        for caption in captions {
            self.cell = Some(Cell {
                row: 1,
                column: 1,
                row_span: 1,
                column_span: columns.max(1),
            });
            self.visit(caption, Some(index), None, out);
            self.cell = None;
        }
        for (cell, slot) in placed {
            self.cell = Some(slot);
            self.visit(cell, Some(index), None, out);
            self.cell = None;
        }
    }

    /// Where each cell of `rows` goes, `offset` rows down, and how many columns the table has.
    fn place_cells(&self, rows: &[Vec<NodeId>], offset: u16) -> (Vec<(NodeId, Cell)>, u16) {
        let mut occupied: HashSet<(u16, u16)> = HashSet::new();
        let mut columns = 0_u16;
        let mut placed = Vec::new();
        for (row, cells) in (0_u16..).zip(rows) {
            let mut column = 1_u16;
            for cell in cells {
                while occupied.contains(&(row, column)) {
                    column += 1;
                }
                let column_span = self.span_of(*cell, "colspan");
                let row_span = self.span_of(*cell, "rowspan");
                occupy(&mut occupied, (row, column), (row_span, column_span));
                let slot = Cell {
                    row: row + 1 + offset,
                    column,
                    row_span,
                    column_span,
                };
                placed.push((*cell, slot));
                column += column_span;
                columns = columns.max(column - 1);
            }
        }
        (placed, columns)
    }

    fn span_of(&self, cell: NodeId, attribute: &str) -> u16 {
        self.snapshot
            .dom
            .get(cell)
            .and_then(|node| node.attr(attribute))
            .and_then(|value| value.trim().parse::<u16>().ok())
            .map_or(1, |span| span.clamp(1, 1000))
    }

    /// The rows of a table in order, row groups opened, and its captions.
    fn collect_rows(
        &self,
        parent: NodeId,
        rows: &mut Vec<Vec<NodeId>>,
        captions: &mut Vec<NodeId>,
    ) {
        for child in self.snapshot.dom.flat_children(parent) {
            match self.display_of(child) {
                Display::TableRowGroup => self.collect_rows(child, rows, captions),
                Display::TableRow => rows.push(
                    self.snapshot
                        .dom
                        .flat_children(child)
                        .into_iter()
                        .filter(|cell| self.display_of(*cell) == Display::TableCell)
                        .collect(),
                ),
                Display::TableCaption => captions.push(child),
                _ => {}
            }
        }
    }

    fn display_of(&self, node: NodeId) -> Display {
        let tag = self.snapshot.dom.get(node).and_then(|n| n.tag());
        Display::parse(
            self.snapshot
                .style(node, "display")
                .or_else(|| verve_dom::css::initial_value("display", tag))
                .unwrap_or("inline"),
        )
    }

    /// Gives a table its columns and the gap between its cells (`border-spacing`, none when the
    /// borders collapse).
    fn style_table(&mut self, index: usize, columns: u16) {
        let node = self.boxes[index].node;
        let units = Units::new(16., self.root_font_size, self.snapshot.viewport);
        let spacing = if self.snapshot.style(node, "border-collapse") == Some("collapse") {
            Vec::new()
        } else {
            words(self.snapshot.style(node, "border-spacing").unwrap_or("2px"))
                .into_iter()
                .filter_map(|part| pixels(part, &units))
                .collect()
        };
        let node_padding = |side: &str| match self.snapshot.style(node, &format!("padding-{side}"))
        {
            None => Some(0.),
            Some(text) => pixels(text, &units),
        };
        let [top, right, bottom, left] = ["top", "right", "bottom", "left"].map(node_padding);
        let column_gap = spacing.first().copied().unwrap_or(0.);
        let row_gap = spacing.get(1).copied().unwrap_or(column_gap);
        let style = &mut self.boxes[index].props.style;
        // A table's `width` counts its border and the spacing round the cells, as border-box does.
        style.box_sizing = taffy::BoxSizing::BorderBox;
        // The spacing runs round the outside of the cells too, which a grid has as padding.
        if let (Some(top), Some(right), Some(bottom), Some(left)) = (top, right, bottom, left) {
            style.padding = taffy::Rect {
                top: LengthPercentage::length(top + row_gap),
                right: LengthPercentage::length(right + column_gap),
                bottom: LengthPercentage::length(bottom + row_gap),
                left: LengthPercentage::length(left + column_gap),
            };
        }
        style.grid_template_columns = (0..columns.max(1))
            .map(|_| taffy::GridTemplateComponent::Single(taffy::style_helpers::auto()))
            .collect();
        style.gap = Size {
            width: LengthPercentage::length(column_gap),
            height: LengthPercentage::length(row_gap),
        };
    }

    /// Creates the taffy nodes and joins them as the boxes are joined.
    fn finish(mut self, viewport: (f32, f32)) -> BoxTree {
        let mut taffy: TaffyTree<Ifc> = TaffyTree::new();
        taffy.disable_rounding();
        let html_node = self.snapshot.dom.html;
        let html = self.boxes.iter().position(|b| b.node == html_node);
        // The root element starts a block formatting context: nothing collapses through it.
        if let Some(root) = html.map(|i| &mut self.boxes[i])
            && root.props.style.display == taffy::Display::Block
        {
            root.props.style.display = taffy::Display::FlowRoot;
        }
        let (atoms, holders) = self.make_nodes(&mut taffy);
        join_children(&mut self.boxes, &mut taffy);
        let (width, height) = viewport;
        let root_style = taffy::Style {
            display: taffy::Display::Block,
            size: Size {
                width: length(width),
                height: length(height),
            },
            ..taffy::Style::default()
        };
        let top: Vec<taffy::NodeId> = self
            .boxes
            .iter()
            .filter(|node| node.parent.is_none())
            .map(|node| node.taffy)
            .collect();
        let root = taffy
            .new_with_children(root_style, &top)
            .expect("the root holds nodes of this tree");
        let by_node = self
            .boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| !b.anonymous)
            .map(|(i, b)| (b.node, i))
            .collect();
        let body = self
            .boxes
            .iter()
            .position(|b| b.node == self.snapshot.dom.body && !b.anonymous);
        let count = self.boxes.len();
        BoxTree {
            boxes: self.boxes,
            taffy,
            root,
            viewport,
            html,
            body,
            by_node,
            unsupported: self.unsupported,
            atoms,
            holders,
            inline: vec![None; count],
            atom_at: vec![None; count],
        }
    }

    /// Creates the taffy node of every box: a leaf from its style, holding the inline content if
    /// the box has any. Answers the atomic inlines and the boxes that hold content.
    fn make_nodes(&mut self, taffy: &mut TaffyTree<Ifc>) -> (Vec<AtomRef>, Vec<usize>) {
        let mut contents: HashMap<usize, Parts> =
            std::mem::take(&mut self.ifcs).into_iter().collect();
        let mut atoms = Vec::new();
        let mut holders = Vec::new();
        for index in 0..self.boxes.len() {
            // A taffy node is made from the style and holds no copy the boxes could drift from.
            let style = self.boxes[index].props.style.clone();
            let node = match contents.remove(&index) {
                Some(parts) => {
                    for (slot, atom) in parts.atoms.iter().enumerate() {
                        atoms.push(AtomRef {
                            box_index: atom.box_index,
                            holder: index,
                            slot,
                            depth: self.depth_of(atom.box_index),
                        });
                    }
                    holders.push(index);
                    let ifc = Ifc::new(self.measurer.clone(), parts);
                    taffy.new_leaf_with_context(style, ifc)
                }
                None => taffy.new_leaf(style),
            };
            self.boxes[index].taffy = node.expect("a taffy leaf is made from a style alone");
        }
        (atoms, holders)
    }

    fn depth_of(&self, index: usize) -> usize {
        let mut depth = 0;
        let mut at = self.boxes[index].parent;
        while let Some(parent) = at {
            depth += 1;
            at = self.boxes[parent].parent;
        }
        depth
    }
}

/// Sets each taffy node's children: its flat children that it lays out, in paint order, and then
/// the absolute boxes hung under it from elsewhere. A hung box's ghost stays in its flat parent.
fn join_children(boxes: &mut [BoxNode], taffy: &mut TaffyTree<Ifc>) {
    for node in boxes.iter_mut() {
        if node.follows_ghost != [false; 2] {
            node.ghost = taffy.new_leaf(node.props.style.clone()).ok();
        }
    }
    let mut lists: Vec<Vec<taffy::NodeId>> = vec![Vec::new(); boxes.len()];
    for (index, node) in boxes.iter().enumerate() {
        for &child in &node.children {
            let child = &boxes[child];
            // An atomic inline is placed by its line, not by taffy: it is no child of the leaf.
            if child.parent == Some(index) && !child.atomic {
                lists[index].push(child.taffy);
            }
            if let Some(ghost) = child.ghost {
                lists[index].push(ghost);
            }
        }
    }
    for node in boxes.iter() {
        if let Some(parent) = node.parent
            && node.flat_parent != Some(parent)
        {
            lists[parent].push(node.taffy);
        }
    }
    for (node, list) in boxes.iter().zip(lists) {
        if !list.is_empty() {
            let _ = taffy.set_children(node.taffy, &list);
        }
    }
}

/// Which axes need a ghost: the box hangs under a containing block that is not its flat parent,
/// and the style gives neither inset of the axis.
fn ghost_axes(props: &BoxProps, reparented: bool) -> [bool; 2] {
    if !reparented {
        return [false; 2];
    }
    let inset = &props.style.inset;
    [
        inset.left == auto() && inset.right == auto(),
        inset.top == auto() && inset.bottom == auto(),
    ]
}

/// Marks the cells a spanning cell covers, `(row, column)` first, as taken.
fn occupy(taken: &mut HashSet<(u16, u16)>, from: (u16, u16), spans: (u16, u16)) {
    for r in 0..spans.0 {
        for c in 0..spans.1 {
            taken.insert((from.0 + r, from.1 + c));
        }
    }
}

fn place_cell(props: &mut BoxProps, cell: Cell) {
    props.style.grid_row = taffy::Line {
        start: taffy::style_helpers::line(i16::try_from(cell.row).unwrap_or(1)),
        end: taffy::style_helpers::span(cell.row_span),
    };
    props.style.grid_column = taffy::Line {
        start: taffy::style_helpers::line(i16::try_from(cell.column).unwrap_or(1)),
        end: taffy::style_helpers::span(cell.column_span),
    };
}
