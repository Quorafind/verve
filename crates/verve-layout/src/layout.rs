//! Laying the boxes out and reading where taffy put them.
//!
//! Positions here are in document space: each box's border box from the origin of the initial
//! containing block, before any scrolling, sticking or `transform`. [`crate::place`] turns them
//! into what is on the screen.

use taffy::style_helpers::length;

use crate::props::Position;
use crate::tree::BoxTree;

/// Where taffy put each box, by box index.
pub(crate) struct Laid {
    pub origin: Vec<[f32; 2]>,
    pub size: Vec<[f32; 2]>,
    /// Top, right, bottom, left.
    pub border: Vec<[f32; 4]>,
    pub padding: Vec<[f32; 4]>,
    /// The size of what a box holds, scrolled to the end: taffy's content size.
    pub extent: Vec<[f32; 2]>,
}

impl Laid {
    /// The padding box's size: what a scroll container shows of its content.
    pub(crate) fn client(&self, index: usize) -> [f32; 2] {
        let [top, right, bottom, left] = self.border[index];
        let [width, height] = self.size[index];
        [
            (width - left - right).max(0.),
            (height - top - bottom).max(0.),
        ]
    }

    /// `scrollWidth` and `scrollHeight` of a box: its content, and never less than what it shows.
    pub(crate) fn scroll_size(&self, index: usize) -> [f32; 2] {
        let client = self.client(index);
        let extent = self.extent[index];
        [extent[0].max(client[0]), extent[1].max(client[1])]
    }
}

/// Lays the tree out, puts right what taffy gets wrong (a percentage `padding-top`), places the
/// boxes that follow a ghost where it stood, and reads the result.
pub(crate) fn settle(tree: &mut BoxTree) -> Laid {
    tree.compute();
    let mut laid = read(tree);
    if resolve_percent_padding(tree, &laid) {
        tree.compute();
        laid = read(tree);
    }
    if follow_ghosts(tree, &laid) {
        tree.compute();
        laid = read(tree);
    }
    laid
}

fn read(tree: &BoxTree) -> Laid {
    let count = tree.boxes.len();
    let mut laid = Laid {
        origin: Vec::with_capacity(count),
        size: Vec::with_capacity(count),
        border: Vec::with_capacity(count),
        padding: Vec::with_capacity(count),
        extent: Vec::with_capacity(count),
    };
    for (index, node) in tree.boxes.iter().enumerate() {
        let Ok(layout) = tree.taffy.layout(node.taffy) else {
            laid.origin.push([0.; 2]);
            laid.size.push([0.; 2]);
            laid.border.push([0.; 4]);
            laid.padding.push([0.; 4]);
            laid.extent.push([0.; 2]);
            continue;
        };
        let at = match (tree.atom_at[index], node.parent) {
            // An atomic inline is where its line put it, in the content box of its block.
            (Some(inside), Some(parent)) => {
                let [top, _, _, left] = laid.border[parent];
                let [pad_top, _, _, pad_left] = laid.padding[parent];
                let from = laid.origin[parent];
                [
                    from[0] + left + pad_left + inside[0],
                    from[1] + top + pad_top + inside[1],
                ]
            }
            _ => {
                let base = node.parent.map_or([0.; 2], |parent| laid.origin[parent]);
                [base[0] + layout.location.x, base[1] + layout.location.y]
            }
        };
        laid.origin.push(at);
        laid.size.push([layout.size.width, layout.size.height]);
        let b = layout.border;
        laid.border.push([b.top, b.right, b.bottom, b.left]);
        let p = layout.padding;
        laid.padding.push([p.top, p.right, p.bottom, p.left]);
        let mut extent = [layout.content_size.width, layout.content_size.height];
        if let Some(lines) = &tree.inline[index] {
            // Lines reach as far as they do, past the box's edge if they overflow it.
            extent[0] = extent[0].max(b.left + p.left + lines.width);
            extent[1] = extent[1].max(b.top + p.top + lines.height);
        }
        laid.extent.push(extent);
    }
    laid
}

/// A percentage `padding-top` or `padding-bottom` is of the containing block's width, which taffy
/// 0.13 takes from its height in a block's list of items. Each such padding is turned into pixels
/// from the width laid out the first time. Answers whether any box changed.
fn resolve_percent_padding(tree: &mut BoxTree, laid: &Laid) -> bool {
    let mut changed = false;
    for (node, shares) in tree
        .boxes
        .iter()
        .map(|node| (node, node.props.padding_percent))
        .filter(|(_, shares)| *shares != [None; 2])
    {
        let basis = node.parent.map_or(tree.viewport.0, |parent| {
            let [_, right, _, left] = laid.border[parent];
            let [_, pad_right, _, pad_left] = laid.padding[parent];
            let inner = laid.size[parent][0] - left - right;
            if node.props.position.is_out_of_flow() {
                inner
            } else {
                inner - pad_left - pad_right
            }
        });
        let Ok(mut style) = tree.taffy.style(node.taffy).cloned() else {
            continue;
        };
        if let Some(share) = shares[0] {
            style.padding.top = length(share * basis);
        }
        if let Some(share) = shares[1] {
            style.padding.bottom = length(share * basis);
        }
        changed |= tree.taffy.set_style(node.taffy, style).is_ok();
    }
    changed
}

/// An absolute box laid out in its containing block, with no inset on an axis, is put where it
/// would have stood in its flat parent: its inset becomes the distance from the containing block's
/// padding box to the ghost it left. Answers whether any box moved.
fn follow_ghosts(tree: &mut BoxTree, laid: &Laid) -> bool {
    let mut moved = false;
    for index in 0..tree.boxes.len() {
        let node = &tree.boxes[index];
        let (Some(ghost), Some(flat)) = (node.ghost, node.flat_parent) else {
            continue;
        };
        let Ok(stand_in) = tree.taffy.layout(ghost) else {
            continue;
        };
        let (location, margin) = (stand_in.location, stand_in.margin);
        let Ok(mut style) = tree.taffy.style(node.taffy).cloned() else {
            continue;
        };
        let at = [
            laid.origin[flat][0] + location.x,
            laid.origin[flat][1] + location.y,
        ];
        let pad_origin = node.parent.map_or([0.; 2], |parent| {
            [
                laid.origin[parent][0] + laid.border[parent][3],
                laid.origin[parent][1] + laid.border[parent][0],
            ]
        });
        if node.follows_ghost[0] {
            style.inset.left = length(at[0] - pad_origin[0] - margin.left);
        }
        if node.follows_ghost[1] {
            style.inset.top = length(at[1] - pad_origin[1] - margin.top);
        }
        let target = node.taffy;
        moved |= tree.taffy.set_style(target, style).is_ok();
    }
    moved
}

/// How far the page scrolls: the viewport, grown to hold every box that is not fixed, not
/// clipped by an ancestor and not inside something that is.
pub(crate) fn page_extent(tree: &BoxTree, laid: &Laid) -> [f32; 2] {
    let mut extent = [tree.viewport.0, tree.viewport.1];
    let mut counts = vec![true; tree.boxes.len()];
    for (index, node) in tree.boxes.iter().enumerate() {
        let inherited = node.parent.is_none_or(|parent| {
            counts[parent] && (!tree.boxes[parent].props.clips() || Some(parent) == tree.html)
        });
        counts[index] = inherited && node.props.position != Position::Fixed;
        if counts[index] {
            extent[0] = extent[0].max(laid.origin[index][0] + laid.size[index][0]);
            extent[1] = extent[1].max(laid.origin[index][1] + laid.size[index][1]);
        }
    }
    extent
}
