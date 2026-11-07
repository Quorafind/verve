//! Laying the tree out when blocks of inline content and the atomic inlines in them depend on
//! each other.
//!
//! A line is as wide as its block, and an atomic inline (an `inline-block`, an `<img>`) in it is as
//! wide as its own content wants within that, so each needs the other's size. Taffy cannot call
//! into itself from a measure function, so the atomic inlines are laid out on their own, outside
//! the tree of the page (they are no taffy children of the leaf that holds their line), at the
//! width their block last had; the lines are laid out with those sizes; and when a block's width
//! differs from the one its atoms were sized for, they are sized again, deepest first, until the
//! numbers stop moving.

use std::sync::Arc;

use taffy::{
    AvailableSpace, CompactLength, Dimension, LengthPercentage, LengthPercentageAuto, Size,
};

use crate::inline::{Atom, Fragment};
use crate::props::BaselineRule;
use crate::tree::BoxTree;

/// Whether a box takes the width it is offered rather than the width its content wants: it has a
/// width of its own, or it is a block in flow, which stretches. Taffy offers a block the width it
/// will end up with as a definite space, and the lines have to be broken at that width.
fn fills_room(style: &taffy::Style) -> bool {
    let tag = style.size.width.into_raw().tag();
    tag == CompactLength::LENGTH_TAG
        || tag == CompactLength::PERCENT_TAG
        || (style.display == taffy::Display::Block
            && style.float == taffy::Float::None
            && style.position != taffy::Position::Absolute)
}

/// Whether any of the sizes that `resolve_percentages` reads is a percentage.
fn has_percentages(style: &taffy::Style) -> bool {
    let percent = |value: CompactLength| value.tag() == CompactLength::PERCENT_TAG;
    percent(style.size.width.into_raw())
        || percent(style.min_size.width.into_raw())
        || percent(style.max_size.width.into_raw())
        || [
            style.padding.left,
            style.padding.right,
            style.padding.top,
            style.padding.bottom,
        ]
        .iter()
        .any(|value| percent(value.into_raw()))
        || [
            style.margin.left,
            style.margin.right,
            style.margin.top,
            style.margin.bottom,
        ]
        .iter()
        .any(|value| percent(value.into_raw()))
}

/// A percentage, as the length it is of `basis`; a length and `auto` are not touched.
fn of_basis(value: CompactLength, basis: f32) -> Option<f32> {
    (value.tag() == CompactLength::PERCENT_TAG).then(|| value.value() * basis)
}

/// Makes the percentages of a box's width, margins and padding lengths of `basis`, the width of
/// the content of the block the box is in. Heights are left alone: a percentage height of a block
/// whose height is not known is `auto`, as taffy takes it.
fn resolve_percentages(style: &mut taffy::Style, basis: f32) {
    let dimension = |value: &mut Dimension| {
        if let Some(px) = of_basis(value.into_raw(), basis) {
            *value = Dimension::length(px);
        }
    };
    dimension(&mut style.size.width);
    dimension(&mut style.min_size.width);
    dimension(&mut style.max_size.width);
    let pad = |value: &mut LengthPercentage| {
        if let Some(px) = of_basis(value.into_raw(), basis) {
            *value = LengthPercentage::length(px);
        }
    };
    for value in [
        &mut style.padding.left,
        &mut style.padding.right,
        &mut style.padding.top,
        &mut style.padding.bottom,
    ] {
        pad(value);
    }
    for value in [
        &mut style.margin.left,
        &mut style.margin.right,
        &mut style.margin.top,
        &mut style.margin.bottom,
    ] {
        if let Some(px) = of_basis(value.into_raw(), basis) {
            *value = LengthPercentageAuto::length(px);
        }
    }
}

/// How many times the atoms and the lines are laid out against each other at most.
const ROUNDS: usize = 6;

/// A width counts as the same width within this.
const SAME: f32 = 0.01;

impl BoxTree {
    /// Lays the tree out in the viewport.
    pub(crate) fn compute(&mut self) {
        // The min- and max-content widths of an atom that has no percentages and no atoms in
        // it do not depend on the room it is offered: they are worked out in the first round.
        let mut known: Vec<Option<(f32, f32)>> = vec![None; self.atoms.len()];
        for _ in 0..ROUNDS {
            self.size_atoms(&mut known);
            self.compute_page();
            if !self.retarget_atoms() {
                break;
            }
        }
        self.settle_inline();
    }

    /// Lays out the tree from its root.
    fn compute_page(&mut self) {
        let (width, height) = self.viewport;
        let space = Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        };
        // A layout that fails leaves every box where it was; the previous result stays.
        let _ = self.taffy.compute_layout_with_measure(
            self.root,
            space,
            |known, available, _, context, style| {
                context.map_or(Size::ZERO, |ifc| {
                    ifc.measure(known, available, fills_room(style))
                })
            },
        );
    }

    /// Lays out `node` as a tree of its own, `width` wide (or as wide as it wants).
    fn layout_alone(&mut self, node: taffy::NodeId, width: AvailableSpace) {
        let space = Size {
            width,
            height: AvailableSpace::MaxContent,
        };
        let _ = self.taffy.mark_dirty(node);
        let _ = self.taffy.compute_layout_with_measure(
            node,
            space,
            |known, available, _, context, style| {
                context.map_or(Size::ZERO, |ifc| {
                    ifc.measure(known, available, fills_room(style))
                })
            },
        );
    }

    /// The width of the border box taffy gave a node.
    fn border_width(&self, node: taffy::NodeId) -> f32 {
        self.taffy
            .layout(node)
            .map_or(0., |layout| layout.size.width)
    }

    /// Sizes every atomic inline for the width its block has now, the deepest first.
    fn size_atoms(&mut self, known: &mut [Option<(f32, f32)>]) {
        let mut order: Vec<usize> = (0..self.atoms.len()).collect();
        order.sort_by_key(|at| std::cmp::Reverse(self.atoms[*at].depth));
        for at in order {
            let (box_index, holder, slot) = {
                let atom = &self.atoms[at];
                (atom.box_index, atom.holder, atom.slot)
            };
            let node = self.boxes[box_index].taffy;
            let holder_node = self.boxes[holder].taffy;
            let Some(before) = self
                .taffy
                .get_node_context(holder_node)
                .map(|ifc| ifc.atoms[slot].clone())
            else {
                continue;
            };
            let sized = self.size_atom(node, box_index, before, &mut known[at]);
            let changed = self
                .taffy
                .get_node_context_mut(holder_node)
                .is_some_and(|ifc| ifc.set_atom(slot, sized));
            if changed {
                let _ = self.taffy.mark_dirty(holder_node);
            }
        }
    }

    /// The atom with the size its box comes to when its block's content is `atom.avail` wide: as
    /// wide as the content wants, no wider than the room and no narrower than it can be.
    ///
    /// Taffy lays the box out as a root, and a root's percentages are of the space it is offered,
    /// which is not its block's width. So for the sizing, the percentages that refer to the width
    /// of the block are made lengths of it.
    fn size_atom(
        &mut self,
        node: taffy::NodeId,
        box_index: usize,
        atom: Atom,
        known: &mut Option<(f32, f32)>,
    ) -> Atom {
        let original = self.taffy.style(node).ok().cloned();
        let fixed = original
            .as_ref()
            .is_some_and(|style| !has_percentages(style) && !self.holds_atoms(box_index));
        if let (Some(style), Some(avail)) = (&original, atom.avail) {
            let mut resolved = style.clone();
            resolve_percentages(&mut resolved, avail);
            let _ = self.taffy.set_style(node, resolved);
        }
        let sized = self.fit_atom(node, box_index, atom, known, fixed);
        if let Some(style) = original {
            let _ = self.taffy.set_style(node, style);
        }
        sized
    }

    fn fit_atom(
        &mut self,
        node: taffy::NodeId,
        box_index: usize,
        mut atom: Atom,
        known: &mut Option<(f32, f32)>,
        fixed: bool,
    ) -> Atom {
        let before = atom.size;
        match *known {
            Some((min, max)) if fixed => (atom.min_width, atom.max_width) = (min, max),
            _ => {
                self.layout_alone(node, AvailableSpace::MinContent);
                atom.min_width = self.border_width(node);
                self.layout_alone(node, AvailableSpace::MaxContent);
                atom.max_width = self.border_width(node);
                *known = fixed.then_some((atom.min_width, atom.max_width));
            }
        }
        let margins = atom.margin[1].resolve(atom.avail.unwrap_or(0.))
            + atom.margin[3].resolve(atom.avail.unwrap_or(0.));
        let room = atom.avail.map_or(f32::INFINITY, |avail| avail - margins);
        let fit = atom.max_width.min(room.max(atom.min_width));
        if known.is_some() && fixed && (before[0] - fit).abs() < SAME && before[1] > 0. {
            // The same width as before, and the box has nothing in it that moved.
            return atom;
        }
        self.layout_alone(node, AvailableSpace::Definite(fit + margins));
        atom.size = self
            .taffy
            .layout(node)
            .map_or([0.; 2], |layout| [layout.size.width, layout.size.height]);
        atom.baseline = self.baseline_of(box_index);
        atom
    }

    /// Whether an atomic inline is somewhere in the box (an atom in an atom).
    fn holds_atoms(&self, index: usize) -> bool {
        self.boxes[index]
            .children
            .iter()
            .any(|child| self.boxes[*child].atomic || self.holds_atoms(*child))
    }

    /// The width of the content box of a box taffy has laid out.
    pub(crate) fn content_width(&self, index: usize) -> f32 {
        let Ok(layout) = self.taffy.layout(self.boxes[index].taffy) else {
            return 0.;
        };
        let (border, padding) = (layout.border, layout.padding);
        (layout.size.width - border.left - border.right - padding.left - padding.right).max(0.)
    }

    /// The distance from the top of a box's border box to the baseline of its last line: an
    /// atomic inline sits on it. `None` for a box with no line in flow, and for one that clips
    /// (its bottom margin edge is its baseline then).
    fn baseline_of(&mut self, index: usize) -> Option<f32> {
        let rule = self.boxes[index].props.baseline;
        if let BaselineRule::FromBottom(up) = rule {
            return self
                .taffy
                .layout(self.boxes[index].taffy)
                .ok()
                .map(|layout| layout.size.height - up);
        }
        if rule == BaselineRule::BorderBottom {
            return self
                .taffy
                .layout(self.boxes[index].taffy)
                .ok()
                .map(|layout| layout.size.height);
        }
        if rule == BaselineRule::Last && self.boxes[index].props.clips() {
            return None;
        }
        let node = self.boxes[index].taffy;
        let width = self.content_width(index);
        if let Some(ifc) = self.taffy.get_node_context_mut(node) {
            let layout = ifc.layout(width);
            // A field with no text stands on the baseline its text would have.
            let baseline = match layout.lines.last() {
                Some(line) => line.baseline,
                None => ifc.empty_baseline()?,
            };
            let offset = self
                .taffy
                .layout(node)
                .map_or(0., |l| l.border.top + l.padding.top);
            return Some(offset + baseline);
        }
        let children: Vec<usize> = self.boxes[index].children.clone();
        for child in children.into_iter().rev() {
            let b = &self.boxes[child];
            if b.atomic || b.props.float || b.props.position.is_out_of_flow() {
                continue;
            }
            if let Some(found) = self.baseline_of(child) {
                let top = self
                    .taffy
                    .layout(self.boxes[child].taffy)
                    .map_or(0., |layout| layout.location.y);
                return Some(top + found);
            }
        }
        None
    }

    /// Tells every atom the width of the block it is in. Answers whether any was not told that
    /// before, which means it has to be sized again.
    fn retarget_atoms(&mut self) -> bool {
        let mut changed = false;
        for at in 0..self.atoms.len() {
            let (holder, slot) = (self.atoms[at].holder, self.atoms[at].slot);
            let width = self.content_width(holder);
            let node = self.boxes[holder].taffy;
            let Some(ifc) = self.taffy.get_node_context_mut(node) else {
                continue;
            };
            let atom = &mut ifc.atoms[slot];
            if atom.avail.is_none_or(|known| (known - width).abs() > SAME) {
                atom.avail = Some(width);
                changed = true;
            }
        }
        changed
    }

    /// Takes the lines of every block as laid out at the width it ended up with, and where each
    /// atomic inline is in its block.
    fn settle_inline(&mut self) {
        for index in self.holders.clone() {
            let width = self.content_width(index);
            let node = self.boxes[index].taffy;
            let Some(ifc) = self.taffy.get_node_context_mut(node) else {
                continue;
            };
            let layout = ifc.layout(width);
            for line in &layout.lines {
                for fragment in &line.fragments {
                    if let Fragment::Atomic { node, rect } = fragment
                        && let Some(&atom) = self.by_node.get(node)
                    {
                        self.atom_at[atom] = Some([rect.x, rect.y]);
                    }
                }
            }
            self.inline[index] = Some(Arc::clone(&layout));
        }
    }
}
