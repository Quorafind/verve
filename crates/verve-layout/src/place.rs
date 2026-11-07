//! From document space to the screen: scroll offsets, sticky boxes, `transform` translations and
//! the clips of the boxes above.
//!
//! The layout is done once per document change; this runs again whenever something scrolls, which
//! moves boxes and nothing else.
//!
//! Two sets of rectangles come out. `rect` is where a box is on the screen, scrolling included:
//! what a script reads. `paint_rect` is where it is in the *frame* it is painted in. A scroll
//! container that a person can scroll (a *frame*) is painted once for any offset: what is inside
//! it is placed as if it, and every frame outside it, were not scrolled, and the replay moves it by
//! the offset. A container that cannot be scrolled by hand has its offset in the positions.

use std::collections::HashMap;

use verve_dom::{NodeId, Rect};

use crate::layout::{Laid, page_extent};
use crate::props::{Clip, Position};
use crate::tree::BoxTree;

/// Where every box is on the screen, and how far each scroll container is scrolled.
pub(crate) struct Placed {
    /// The border box of each box, in viewport coordinates.
    pub rect: Vec<Rect>,
    /// The border box of each box as the display list has it: in the space of the frames it is
    /// inside, each taken at offset zero.
    pub paint_rect: Vec<Rect>,
    /// Whether the box is a frame: a scroll container the display list lets the shell scroll.
    pub frame: Vec<bool>,
    /// How far each box is scrolled (`[0, 0]` for one that does not scroll), after clamping.
    pub offset: Vec<[f32; 2]>,
    /// The size of the scrolled content of each box.
    pub content: Vec<[f32; 2]>,
}

impl Placed {
    /// The scroll offsets in effect, by node, for the scroll containers.
    pub(crate) fn offsets(&self, tree: &BoxTree) -> HashMap<NodeId, [f32; 2]> {
        tree.boxes
            .iter()
            .enumerate()
            .filter(|(index, _)| self.scrolls(tree, *index))
            .map(|(index, node)| (node.node, self.offset[index]))
            .collect()
    }

    /// Whether the box has a scroll offset of its own: a clipping box, or the page.
    pub(crate) fn scrolls(&self, tree: &BoxTree, index: usize) -> bool {
        tree.boxes[index].props.clips() || Some(index) == tree.html
    }
}

/// Places the boxes with the scroll offsets `wanted` (by node, clamped to what each box can
/// scroll to).
pub(crate) fn place(tree: &BoxTree, laid: &Laid, wanted: &HashMap<NodeId, [f32; 2]>) -> Placed {
    let count = tree.boxes.len();
    let ScrollState {
        offset,
        max_offset,
        content,
    } = scroll_state(tree, laid, wanted);
    let frame: Vec<bool> = (0..count)
        .map(|index| is_frame(tree, index, max_offset[index]))
        .collect();
    let mut placed = Placed {
        rect: Vec::with_capacity(count),
        paint_rect: Vec::with_capacity(count),
        frame,
        offset,
        content,
    };
    let mut inside: Vec<[f32; 2]> = Vec::with_capacity(count);
    let mut screen: Vec<[f32; 2]> = Vec::with_capacity(count);
    for (index, node) in tree.boxes.iter().enumerate() {
        let origin = laid.origin[index];
        let natural = match node.parent {
            Some(parent) => {
                let p = laid.origin[parent];
                // The page's scroll is already in where the root element is.
                let moved = if Some(parent) == tree.html {
                    [0.; 2]
                } else {
                    placed.offset[parent]
                };
                [
                    screen[parent][0] + origin[0] - p[0] - moved[0],
                    screen[parent][1] + origin[1] - p[1] - moved[1],
                ]
            }
            None if node.props.position == Position::Fixed => origin,
            None => {
                let page = tree.html.map_or([0.; 2], |html| placed.offset[html]);
                [origin[0] - page[0], origin[1] - page[1]]
            }
        };
        let mut at = natural;
        let size = laid.size[index];
        if node.props.position == Position::Sticky {
            let shift = sticky_shift(tree, laid, &placed, &screen, index, natural);
            at = [at[0] + shift[0], at[1] + shift[1]];
        }
        if let Some(translation) = &node.props.translation {
            let moved = translation.resolve(size[0], size[1]);
            at = [at[0] + moved[0], at[1] + moved[1]];
        }
        screen.push(at);
        placed.rect.push(Rect::new(at[0], at[1], size[0], size[1]));
        let within = frames_within(tree, &placed, &inside, index);
        inside.push(within);
        placed.paint_rect.push(Rect::new(
            at[0] + within[0],
            at[1] + within[1],
            size[0],
            size[1],
        ));
    }
    placed
}

/// How far each scroll container is scrolled (the wanted offset clamped to what it can reach), how
/// far it can scroll, and how big its content is.
fn scroll_state(tree: &BoxTree, laid: &Laid, wanted: &HashMap<NodeId, [f32; 2]>) -> ScrollState {
    let count = tree.boxes.len();
    let page = page_extent(tree, laid);
    let (mut offset, mut max_offset, mut content) = (
        vec![[0.; 2]; count],
        vec![[0.; 2]; count],
        vec![[0.; 2]; count],
    );
    for (index, node) in tree.boxes.iter().enumerate() {
        let is_page = Some(index) == tree.html;
        if !node.props.clips() && !is_page {
            continue;
        }
        let (size, client) = if is_page {
            (page, [tree.viewport.0, tree.viewport.1])
        } else {
            (laid.scroll_size(index), laid.client(index))
        };
        let most = [(size[0] - client[0]).max(0.), (size[1] - client[1]).max(0.)];
        let wants = wanted.get(&node.node).copied().unwrap_or([0.; 2]);
        let axes_scroll = if is_page {
            [true; 2]
        } else {
            [
                node.props.overflow[0].clips(),
                node.props.overflow[1].clips(),
            ]
        };
        let clamp = |axis: usize| {
            if axes_scroll[axis] {
                wants[axis].clamp(0., most[axis])
            } else {
                0.
            }
        };
        offset[index] = [clamp(0), clamp(1)];
        max_offset[index] = most;
        content[index] = size;
    }
    ScrollState {
        offset,
        max_offset,
        content,
    }
}

/// Per box: how far it is scrolled, how far it can scroll, and how big its content is.
struct ScrollState {
    offset: Vec<[f32; 2]>,
    max_offset: Vec<[f32; 2]>,
    content: Vec<[f32; 2]>,
}

/// Whether a box is a frame: a container with `overflow: auto` or `scroll` that has something to
/// scroll to (or always shows its scrollbar), or the page unless the root element clips it.
fn is_frame(tree: &BoxTree, index: usize, most: [f32; 2]) -> bool {
    let props = &tree.boxes[index].props;
    let scrollable = |clip: Clip| matches!(clip, Clip::Scroll | Clip::Auto);
    let movable = most[0] > 0. || most[1] > 0.;
    if Some(index) == tree.html {
        return movable && !props.overflow.contains(&Clip::Hidden);
    }
    props.overflow.into_iter().any(scrollable)
        && (movable || props.overflow.contains(&Clip::Scroll))
}

/// How much the frames a box is inside have scrolled: what the screen position of the box has
/// had taken off for them, and its painted position has put back.
fn frames_within(tree: &BoxTree, placed: &Placed, inside: &[[f32; 2]], index: usize) -> [f32; 2] {
    let node = &tree.boxes[index];
    let (base, frame) = match node.parent {
        Some(parent) => (inside[parent], parent),
        None if node.props.position == Position::Fixed || Some(index) == tree.html => {
            return [0.; 2];
        }
        None => match tree.html {
            Some(html) => ([0.; 2], html),
            None => return [0.; 2],
        },
    };
    if placed.frame[frame] {
        [
            base[0] + placed.offset[frame][0],
            base[1] + placed.offset[frame][1],
        ]
    } else {
        base
    }
}

/// How far a sticky box is pushed from where its place in the flow puts it: far enough to stay
/// `top`/`bottom`/`left`/`right` inside the scrollport of the nearest scroll container, and no
/// further than the edges of its parent.
fn sticky_shift(
    tree: &BoxTree,
    laid: &Laid,
    placed: &Placed,
    screen: &[[f32; 2]],
    index: usize,
    natural: [f32; 2],
) -> [f32; 2] {
    let node = &tree.boxes[index];
    let [top, right, bottom, left] = node.props.sticky;
    let port = scrollport(tree, laid, screen, node.parent);
    let Some(parent) = node.parent else {
        return [0.; 2];
    };
    let [bt, br, bb, bl] = laid.border[parent];
    let [pt, pr, pb, pl] = laid.padding[parent];
    let outer = laid.size[parent];
    let scrolled = if Some(parent) == tree.html {
        [0.; 2]
    } else {
        placed.offset[parent]
    };
    let start = [
        screen[parent][0] + bl + pl - scrolled[0],
        screen[parent][1] + bt + pt - scrolled[1],
    ];
    // What the parent holds: all of its scrollable content when it scrolls, else its content box.
    let span = if placed.scrolls(tree, parent) && Some(parent) != tree.html {
        [
            placed.content[parent][0] - pl - pr,
            placed.content[parent][1] - pt - pb,
        ]
    } else {
        [outer[0] - bl - br - pl - pr, outer[1] - bt - bb - pt - pb]
    };
    let size = laid.size[index];
    [
        stick(
            natural[0],
            size[0],
            [left, right],
            [port.x, port.x + port.width],
            [start[0], start[0] + span[0].max(0.)],
        ),
        stick(
            natural[1],
            size[1],
            [top, bottom],
            [port.y, port.y + port.height],
            [start[1], start[1] + span[1].max(0.)],
        ),
    ]
}

/// One axis of [`sticky_shift`]: the box at `at` of length `len`, with the insets from the start
/// and end of the scrollport, kept inside the parent's `limits`.
fn stick(at: f32, len: f32, insets: [Option<f32>; 2], port: [f32; 2], limits: [f32; 2]) -> f32 {
    let mut shift = 0.;
    if let Some(start) = insets[0]
        && at < port[0] + start
    {
        shift = port[0] + start - at;
    }
    if let Some(end) = insets[1]
        && at + shift > port[1] - end - len
    {
        shift = port[1] - end - len - at;
    }
    let (low, high) = (limits[0] - at, limits[1] - len - at);
    if high >= low {
        shift.clamp(low, high)
    } else {
        shift
    }
}

/// The part of the screen a scroll container shows its content in: the padding box of the
/// nearest clipping ancestor, or the viewport.
fn scrollport(tree: &BoxTree, laid: &Laid, screen: &[[f32; 2]], from: Option<usize>) -> Rect {
    let mut at = from;
    while let Some(index) = at {
        if tree.boxes[index].props.clips() && Some(index) != tree.html {
            let [top, _, _, left] = laid.border[index];
            let client = laid.client(index);
            return Rect::new(
                screen[index][0] + left,
                screen[index][1] + top,
                client[0],
                client[1],
            );
        }
        at = tree.boxes[index].parent;
    }
    Rect::new(0., 0., tree.viewport.0, tree.viewport.1)
}

/// What a box is painted inside, outermost first: the clip of each clipping container above it,
/// in the space outside that container's frame, and the scroll of each container that is a frame.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Around {
    Clip(Rect, [f32; 4]),
    Scroll(NodeId),
}

/// What box `index` is painted inside. The page is a frame without a clip.
pub(crate) fn around(tree: &BoxTree, laid: &Laid, placed: &Placed, index: usize) -> Vec<Around> {
    let mut list = Vec::new();
    let mut at = tree.boxes[index].parent;
    let mut reached_page = false;
    while let Some(ancestor) = at {
        let node = &tree.boxes[ancestor];
        if Some(ancestor) == tree.html {
            reached_page = true;
            if placed.frame[ancestor] {
                list.push(Around::Scroll(node.node));
            }
        } else if node.props.clips() {
            if placed.frame[ancestor] {
                list.push(Around::Scroll(node.node));
            }
            let (inner, radii) = padding_box(tree, laid, placed.paint_rect[ancestor], ancestor);
            list.push(Around::Clip(inner, radii));
        }
        at = node.parent;
    }
    let fixed_to_viewport =
        tree.boxes[index].props.position == Position::Fixed && tree.boxes[index].parent.is_none();
    if !reached_page
        && Some(index) != tree.html
        && !fixed_to_viewport
        && let Some(html) = tree.html.filter(|html| placed.frame[*html])
    {
        list.push(Around::Scroll(tree.boxes[html].node));
    }
    list.reverse();
    list
}

/// The clips that are in the same frame as the box: those after the last scroll.
pub(crate) fn clips_in_frame(around: &[Around]) -> impl Iterator<Item = (Rect, [f32; 4])> + '_ {
    let from = around
        .iter()
        .rposition(|entry| matches!(entry, Around::Scroll(_)))
        .map_or(0, |at| at + 1);
    around[from..].iter().filter_map(|entry| match entry {
        Around::Clip(rect, radii) => Some((*rect, *radii)),
        Around::Scroll(_) => None,
    })
}

/// The padding box of a box whose border box is `rect`, with its corner radii (the border box's,
/// less the border).
pub(crate) fn padding_box(
    tree: &BoxTree,
    laid: &Laid,
    rect: Rect,
    index: usize,
) -> (Rect, [f32; 4]) {
    let [top, right, bottom, left] = laid.border[index];
    let inner = Rect::new(
        rect.x + left,
        rect.y + top,
        (rect.width - left - right).max(0.),
        (rect.height - top - bottom).max(0.),
    );
    let [tl, tr, br, bl] = tree.boxes[index].props.paint.radii(rect.width, rect.height);
    let radii = [
        (tl - left.max(top)).max(0.),
        (tr - right.max(top)).max(0.),
        (br - right.max(bottom)).max(0.),
        (bl - left.max(bottom)).max(0.),
    ];
    (inner, radii)
}
