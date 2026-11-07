//! What a laid-out page is drawn from: the items in paint order.
//!
//! The list is flat, and a replay keeps a stack of clips, opacity groups and scroll frames. A
//! scroll container a person can scroll is a *frame*: [`PaintItem::PushScroll`] opens it, and the
//! items until [`PaintItem::PopScroll`] are placed as if that container, and every frame outside
//! it, were not scrolled. The replay moves them by minus the container's offset — the shell's own
//! when it has scrolled since the list was made, otherwise the one the [`PaintItem::Scroller`] of
//! the container holds — so a scroll changes that offset and nothing else in the list. Sticky
//! boxes and `transform` translations, and the scrolling of containers that cannot be scrolled by
//! hand, are already in the positions. Colours are straight (not premultiplied) sRGB
//! `[r, g, b, a]` in `0..=1`.

use std::collections::HashMap;
use std::sync::Arc;

use verve_dom::{NodeId, Rect};

use crate::controls::{ControlKind, ControlLook};
use crate::gradient::Gradient;
use crate::measure::{FontKey, PlacedGlyph};

/// A colour as red, green, blue and alpha, each in `0..=1`.
pub type Rgba = [f32; 4];

/// The items to draw, back to front.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    /// Different for every list a process makes, so two lists with one number are one list.
    pub revision: u64,
    pub items: Vec<PaintItem>,
    /// The gradients that [`Fill::Raster`] refers to, for the renderer to rasterise.
    pub rasters: Vec<RasterSpec>,
    /// The page's own scroll container, where a wheel ends when nothing under the pointer takes
    /// it. `None` when the page does not scroll.
    pub root: Option<NodeId>,
    /// The SVG text that the pictures of vector images ([`PaintItem::Image`], [`Fill::Picture`])
    /// are drawn from, for the shell to rasterise at its scale (P35, appended).
    pub vectors: Vec<VectorSpec>,
}

/// Sets the frame a picture item shows to what `frame_of` says of its key.
fn reframe(item: &mut PaintItem, frame_of: &dyn Fn(ImageKey) -> Option<u32>) {
    match item {
        PaintItem::Image { image, frame, .. } => {
            if let Some(at) = frame_of(*image) {
                *frame = at;
            }
        }
        PaintItem::Box { background, .. } => background.iter_mut().for_each(|fill| {
            if let Fill::Picture { image, frame, .. } = fill
                && let Some(at) = frame_of(*image)
            {
                *frame = at;
            }
        }),
        _ => {}
    }
}

impl DisplayList {
    /// The list with the pictures in `frames` showing the frame they are given, under a revision
    /// of its own: what the renderer makes of a list when an animation moves on.
    pub fn with_frames(&self, frames: &std::collections::HashMap<ImageKey, u32>) -> DisplayList {
        let mut list = self.clone();
        list.revision = crate::engine::next_revision();
        for item in &mut list.items {
            reframe(item, &|key| frames.get(&key).copied());
        }
        list
    }

    /// Whether `other` is this list but for the frames of animated pictures: what a screenshot
    /// waits for to stop changing, a page with an animation that never ends never being still
    /// otherwise.
    pub fn same_but_frames(&self, other: &DisplayList) -> bool {
        let still = |item: &PaintItem| {
            let mut item = item.clone();
            reframe(&mut item, &|_| Some(0));
            item
        };
        self.items.len() == other.items.len()
            && self.rasters == other.rasters
            && self.vectors == other.vectors
            && self.root == other.root
            && self
                .items
                .iter()
                .zip(&other.items)
                .all(|(a, b)| a == b || still(a) == still(b))
    }

    /// The node whose [`PaintItem::Hitbox`] is topmost at `(x, y)` in the window. `offset_of`
    /// answers the offset the shell has scrolled a container to, if it is not the list's.
    pub fn hit_test(
        &self,
        x: f32,
        y: f32,
        offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
    ) -> Option<NodeId> {
        self.resolved_hitboxes(offset_of)
            .into_iter()
            .rev()
            .find(|hitbox| hitbox.bounds.contains(x, y))
            .map(|hitbox| hitbox.node)
    }

    /// Every hitbox in paint order (later covers earlier), in window coordinates: moved by the
    /// scroll of the frames it is in and cut to the clips it is under. This is what the cursor, the
    /// drag regions and hit testing read, so none of them follows the stack of frames itself.
    pub fn resolved_hitboxes(
        &self,
        offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
    ) -> Vec<ResolvedHitbox> {
        self.placed(offset_of)
            .iter()
            .filter_map(Placed::hitbox)
            .collect()
    }

    /// The items that draw or hit (everything but the pushes and pops), each with the scroll, the
    /// clip and the opacity it is under, in paint order. This is the one place the stack of frames,
    /// clips and opacity groups is followed: the replay, the hitboxes and the wheel read it.
    /// `offset_of` answers the offset the shell has scrolled a container to, if it is not the one
    /// in the list.
    pub fn placed(&self, offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>) -> Vec<Placed<'_>> {
        self.walk(offset_of).placed
    }

    /// The caret of the text field `node` in the window, as wide as the caret is: where the
    /// shell puts a candidate window. `None` when the field has none (it has no focus).
    pub fn caret_of(&self, node: NodeId) -> Option<Rect> {
        let is_it = |item: &PaintItem| matches!(item, PaintItem::Control { node: n, kind: ControlKind::TextField { caret: Some(_), .. }, .. } if *n == node);
        if !self.items.iter().any(is_it) {
            return None;
        }
        self.placed(&|_| None)
            .into_iter()
            .find_map(|placed| match placed.item {
                PaintItem::Control {
                    kind:
                        ControlKind::TextField {
                            caret: Some(caret), ..
                        },
                    ..
                } if is_it(placed.item) => Some(placed.in_window(*caret)),
                _ => None,
            })
    }

    /// [`DisplayList::placed`], and the table of the frames the items are in.
    fn walk(&self, offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>) -> Walk<'_> {
        let offsets: HashMap<NodeId, [f32; 2]> = self
            .items
            .iter()
            .filter_map(|item| match item {
                PaintItem::Scroller { node, offset, .. } => Some((*node, *offset)),
                _ => None,
            })
            .collect();
        let mut walk = Walk {
            placed: Vec::new(),
            frames: Vec::new(),
        };
        // What each open effect had before it.
        let mut open: Vec<Under> = Vec::new();
        let mut now = Under::OUTSIDE;
        for item in &self.items {
            match item {
                PaintItem::PushClip { bounds, .. } => {
                    open.push(now);
                    let seen = Rect::new(
                        bounds.x - now.shift[0],
                        bounds.y - now.shift[1],
                        bounds.width,
                        bounds.height,
                    );
                    now.clip = Some(now.clip.map_or(seen, |outer| meet(outer, seen)));
                }
                PaintItem::PushScroll { node } => {
                    open.push(now);
                    let offset = offset_of(*node)
                        .or_else(|| offsets.get(node).copied())
                        .unwrap_or([0.; 2]);
                    now.shift = [now.shift[0] + offset[0], now.shift[1] + offset[1]];
                    walk.frames.push(Frame {
                        node: *node,
                        parent: now.frame,
                    });
                    now.frame = Some(walk.frames.len() - 1);
                }
                PaintItem::PushOpacity(alpha) => {
                    open.push(now);
                    now.opacity *= alpha;
                }
                PaintItem::PopClip | PaintItem::PopScroll | PaintItem::PopOpacity => {
                    now = open.pop().unwrap_or(Under::OUTSIDE);
                }
                item => walk.placed.push(Placed {
                    item,
                    shift: now.shift,
                    clip: now.clip,
                    opacity: now.opacity,
                    frame: now.frame,
                }),
            }
        }
        walk
    }

    /// Every scroll container with its viewport in window coordinates, in paint order.
    pub fn resolved_scrollers(
        &self,
        offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
    ) -> Vec<ResolvedScroller> {
        scrollers_of(&self.placed(offset_of), offset_of)
    }

    /// What the wheel does at `(x, y)` turned by `delta` (the amount the content moves up and to
    /// the left: positive scrolls down and right): the container to scroll and where it comes to.
    /// The wheel goes to the element on top at the point and up its scroll chain: the element
    /// itself if it scrolls, then the frames it is in from the innermost out, and last the page.
    /// The first of them that can move that way takes it. `None` when none can.
    pub fn wheel(
        &self,
        x: f32,
        y: f32,
        delta: [f32; 2],
        offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
    ) -> Option<(NodeId, [f32; 2])> {
        let walk = self.walk(offset_of);
        let scrollers = scrollers_of(&walk.placed, offset_of);
        let top = walk.placed.iter().rev().find_map(|placed| {
            let hitbox = placed.hitbox()?;
            hitbox
                .bounds
                .contains(x, y)
                .then_some((hitbox.node, placed.frame))
        });
        let mut chain = Vec::new();
        if let Some((node, mut frame)) = top {
            chain.push(node);
            while let Some(index) = frame {
                chain.push(walk.frames[index].node);
                frame = walk.frames[index].parent;
            }
        }
        chain.extend(self.root);
        chain.into_iter().find_map(|node| {
            let scroller = scrollers.iter().find(|scroller| scroller.node == node)?;
            let to = [
                (scroller.offset[0] + delta[0]).clamp(0., scroller.most[0]),
                (scroller.offset[1] + delta[1]).clamp(0., scroller.most[1]),
            ];
            (to != scroller.offset).then_some((node, to))
        })
    }
}

/// The clip, scroll, opacity and frame the items being walked are under.
#[derive(Clone, Copy)]
struct Under {
    clip: Option<Rect>,
    shift: [f32; 2],
    opacity: f32,
    frame: Option<usize>,
}

impl Under {
    /// Under nothing: the page before any effect opens.
    const OUTSIDE: Under = Under {
        clip: None,
        shift: [0.; 2],
        opacity: 1.,
        frame: None,
    };
}

/// A scroll frame of a walk, and the frame it is in.
struct Frame {
    node: NodeId,
    parent: Option<usize>,
}

/// The placed items of a list and the frames they are in.
struct Walk<'a> {
    placed: Vec<Placed<'a>>,
    frames: Vec<Frame>,
}

fn scrollers_of(
    placed: &[Placed<'_>],
    offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
) -> Vec<ResolvedScroller> {
    placed
        .iter()
        .filter_map(|placed| placed.scroller(offset_of))
        .collect()
}

/// A drawn or hit item and what it is under: see [`DisplayList::placed`].
#[derive(Clone, Copy, Debug)]
pub struct Placed<'a> {
    pub item: &'a PaintItem,
    /// How far the frames it is in are scrolled: the item is at its bounds minus this.
    pub shift: [f32; 2],
    /// The clips it is under, met, in window coordinates.
    pub clip: Option<Rect>,
    /// The opacity of the groups it is in, multiplied.
    pub opacity: f32,
    /// The innermost scroll frame it is in, as an index into the walk's frame table.
    frame: Option<usize>,
}

impl Placed<'_> {
    /// The hitbox this item is, placed in the window and cut to its clips; `None` for any other
    /// item, and for a hitbox nothing of which shows.
    pub fn hitbox(&self) -> Option<ResolvedHitbox> {
        let PaintItem::Hitbox {
            node,
            bounds,
            cursor,
            app_region,
        } = self.item
        else {
            return None;
        };
        let seen = self.in_window(*bounds);
        let seen = self.clip.map_or(seen, |clip| meet(clip, seen));
        (seen.width > 0. && seen.height > 0.).then_some(ResolvedHitbox {
            node: *node,
            bounds: seen,
            cursor: *cursor,
            app_region: *app_region,
        })
    }

    /// The scroll container this item is, with its viewport in the window; `None` for any other
    /// item. `offset_of` answers the offset the shell has scrolled a container to, if it is not
    /// the list's.
    pub fn scroller(
        &self,
        offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
    ) -> Option<ResolvedScroller> {
        let PaintItem::Scroller {
            node,
            viewport,
            content,
            offset,
            scrollbar,
        } = self.item
        else {
            return None;
        };
        let most = [
            (content[0] - viewport.width).max(0.),
            (content[1] - viewport.height).max(0.),
        ];
        let wanted = offset_of(*node).unwrap_or(*offset);
        Some(ResolvedScroller {
            node: *node,
            viewport: self.in_window(*viewport),
            clip: self.clip,
            content: *content,
            offset: [wanted[0].clamp(0., most[0]), wanted[1].clamp(0., most[1])],
            most,
            scrollbar: *scrollbar,
            opacity: self.opacity,
        })
    }

    /// `rect`, a rectangle in the item's frame, in the window.
    pub fn in_window(&self, rect: Rect) -> Rect {
        Rect::new(
            rect.x - self.shift[0],
            rect.y - self.shift[1],
            rect.width,
            rect.height,
        )
    }
}

/// A scroll container placed in the window: see [`DisplayList::resolved_scrollers`].
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedScroller {
    pub node: NodeId,
    /// The visible part of the box, in the window.
    pub viewport: Rect,
    pub clip: Option<Rect>,
    pub content: [f32; 2],
    /// How far it is scrolled now: the shell's if it has one, else the list's.
    pub offset: [f32; 2],
    /// How far it can be scrolled.
    pub most: [f32; 2],
    pub scrollbar: bool,
    pub opacity: f32,
}

/// The part two rectangles share; zero-sized when they share none.
fn meet(a: Rect, b: Rect) -> Rect {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    Rect::new(x, y, (right - x).max(0.), (bottom - y).max(0.))
}

/// A [`PaintItem::Hitbox`] placed in the window: where a pointer reaches `node` now.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedHitbox {
    pub node: NodeId,
    pub bounds: Rect,
    pub cursor: Cursor,
    pub app_region: Option<AppRegionKind>,
}

/// One thing to draw, or a change to how the items after it are drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum PaintItem {
    /// A box: its shadows, then its backgrounds, then its border.
    Box {
        node: NodeId,
        bounds: Rect,
        /// The radius of each corner (top left, top right, bottom right, bottom left).
        radii: [f32; 4],
        /// Back to front.
        background: Vec<Fill>,
        border: Border,
        /// First in the list is on top, as in CSS.
        shadows: Vec<Shadow>,
    },
    /// Text in one font and colour (P31).
    Glyphs {
        node: NodeId,
        origin: [f32; 2],
        font: FontKey,
        size: f32,
        color: Rgba,
        glyphs: Arc<[PlacedGlyph]>,
    },
    /// An underline, overline or line-through (P31).
    Decoration {
        kind: DecorationKind,
        bounds: Rect,
        color: Rgba,
        wavy: bool,
    },
    /// A picture, the whole of it drawn into `bounds` (already fitted to its box) and cut to
    /// `clip` (P35). `fit` is the style's keyword, kept for the record; `frame` is the frame of
    /// an animation to show.
    Image {
        node: NodeId,
        bounds: Rect,
        clip: Rect,
        image: ImageKey,
        fit: ObjectFit,
        frame: u32,
    },
    /// A form control the shell draws in the platform's way (P32).
    Control {
        node: NodeId,
        bounds: Rect,
        kind: ControlKind,
        look: ControlLook,
    },
    /// Everything up to the matching [`PaintItem::PopClip`] is cut to `bounds` with rounded corners.
    PushClip {
        bounds: Rect,
        radii: [f32; 4],
    },
    PopClip,
    /// The items up to the matching [`PaintItem::PopScroll`] are in the content space of the
    /// scroll container `node`: where they are with it, and every container outside it, scrolled
    /// to zero. The replay translates them by minus its offset.
    PushScroll {
        node: NodeId,
    },
    PopScroll,
    /// Everything up to the matching [`PaintItem::PopOpacity`] is drawn as one group at this opacity.
    PushOpacity(f32),
    PopOpacity,
    /// Where the pointer reaches `node`. `bounds` is the border box in the space of the frames it
    /// is in, cut to the clips in the same frame; [`DisplayList::resolved_hitboxes`] places it.
    Hitbox {
        node: NodeId,
        bounds: Rect,
        cursor: Cursor,
        app_region: Option<AppRegionKind>,
    },
    /// A box that scrolls, outside its own frame: where its viewport is, how big its content is, and
    /// how far it is scrolled. The wheel scrolls the nearest one above the element under the
    /// pointer that can move that way, and the next one out when it cannot.
    Scroller {
        node: NodeId,
        viewport: Rect,
        content: [f32; 2],
        offset: [f32; 2],
        /// Whether the thin scrollbars are drawn (`scrollbar-width: none` leaves them out).
        scrollbar: bool,
    },
}

/// What a background layer is painted with.
#[derive(Clone, Debug, PartialEq)]
pub enum Fill {
    Color(Rgba),
    /// A linear gradient with two or more stops: the angle in degrees (CSS: `0` points up, `90`
    /// right), each stop a colour and its place along the line in `0..=1`.
    Linear {
        angle: f32,
        stops: Vec<(Rgba, f32)>,
    },
    /// A gradient too complex for the shell to paint as it is: the renderer rasterises it
    /// ([`DisplayList::rasters`]) and the picture is stretched over the box.
    Raster(ImageKey),
    /// A `background-image: url()` layer: the tiles of the picture, cut to `area`. Rectangles are
    /// relative to the top left of the box's border box. The tile at `(i, j)` is `tile` moved by
    /// `(i * step[0], j * step[1])`, for `i` below `count[0]` and `j` below `count[1]`.
    Picture {
        image: ImageKey,
        area: Rect,
        tile: Rect,
        step: [f32; 2],
        count: [u32; 2],
        frame: u32,
    },
}

/// A picture the renderer owns, by number. The top two bits tell the kind: `1x` for a rasterised
/// gradient (the top bit set, the next one arbitrary), `01` for a canvas surface (P35: bits
/// 61–32 are the canvas's slot, bits 31–0 its generation, see `canvas_key`) and `00` for a
/// picture the page loads or an inline SVG (P35), whose other 62 bits are a hash of its address
/// or of its text, so two of them share a picture with a chance of 2^-62.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct ImageKey(pub u64);

/// The text of an SVG picture, for the shell to rasterise at the size it draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorSpec {
    pub key: ImageKey,
    pub source: std::sync::Arc<[u8]>,
}

/// A gradient the renderer rasterises for a [`Fill::Raster`].
#[derive(Clone, Debug, PartialEq)]
pub struct RasterSpec {
    pub key: ImageKey,
    pub gradient: Gradient,
    /// The box it fills, in CSS pixels.
    pub size: [f32; 2],
}

/// The four sides of a border, top first and then clockwise.
#[derive(Clone, Debug, PartialEq)]
pub struct Border {
    pub widths: [f32; 4],
    pub colors: [Rgba; 4],
    pub styles: [BorderLine; 4],
}

impl Border {
    /// A border that is not there.
    pub const NONE: Border = Border {
        widths: [0.; 4],
        colors: [[0.; 4]; 4],
        styles: [BorderLine::None; 4],
    };

    /// Whether any side draws anything.
    pub fn is_visible(&self) -> bool {
        (0..4).any(|side| {
            self.widths[side] > 0.
                && self.colors[side][3] > 0.
                && !matches!(self.styles[side], BorderLine::None | BorderLine::Hidden)
        })
    }
}

/// How a side of a border is drawn: CSS's `border-style`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BorderLine {
    None,
    Hidden,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

/// One `box-shadow`.
#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    pub offset: [f32; 2],
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
    pub inset: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DecorationKind {
    Underline,
    Overline,
    LineThrough,
}

/// How a picture fills its box: CSS's `object-fit`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// A CSS `cursor`, inherited as the style is. A keyword this does not list is `Auto`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Cursor {
    Auto,
    Default,
    None,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    AllScroll,
    ColResize,
    RowResize,
    NResize,
    EResize,
    SResize,
    WResize,
    NeResize,
    NwResize,
    SeResize,
    SwResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
}

impl Cursor {
    /// The cursor a `cursor` value names: the last of its list, the keyword after any `url()`s.
    pub fn parse(list: &str) -> Cursor {
        let keyword = list.rsplit(',').next().unwrap_or(list).trim();
        match keyword {
            "default" => Cursor::Default,
            "none" => Cursor::None,
            "context-menu" => Cursor::ContextMenu,
            "help" => Cursor::Help,
            "pointer" => Cursor::Pointer,
            "progress" => Cursor::Progress,
            "wait" => Cursor::Wait,
            "cell" => Cursor::Cell,
            "crosshair" => Cursor::Crosshair,
            "text" => Cursor::Text,
            "vertical-text" => Cursor::VerticalText,
            "alias" => Cursor::Alias,
            "copy" => Cursor::Copy,
            "move" => Cursor::Move,
            "no-drop" => Cursor::NoDrop,
            "not-allowed" => Cursor::NotAllowed,
            "grab" => Cursor::Grab,
            "grabbing" => Cursor::Grabbing,
            "all-scroll" => Cursor::AllScroll,
            "col-resize" => Cursor::ColResize,
            "row-resize" => Cursor::RowResize,
            "n-resize" => Cursor::NResize,
            "e-resize" => Cursor::EResize,
            "s-resize" => Cursor::SResize,
            "w-resize" => Cursor::WResize,
            "ne-resize" => Cursor::NeResize,
            "nw-resize" => Cursor::NwResize,
            "se-resize" => Cursor::SeResize,
            "sw-resize" => Cursor::SwResize,
            "ew-resize" => Cursor::EwResize,
            "ns-resize" => Cursor::NsResize,
            "nesw-resize" => Cursor::NeswResize,
            "nwse-resize" => Cursor::NwseResize,
            "zoom-in" => Cursor::ZoomIn,
            "zoom-out" => Cursor::ZoomOut,
            _ => Cursor::Auto,
        }
    }
}

/// `-webkit-app-region`: the parts of a frameless window a person drags it by. P34's
/// `AppRegionKind` is a rectangle and a flag: it is `rect` = the resolved hitbox's bounds and
/// `drag` = `kind == AppRegionKind::Drag`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AppRegionKind {
    Drag,
    NoDrag,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hitbox(node: NodeId, x: f32) -> PaintItem {
        PaintItem::Hitbox {
            node,
            bounds: Rect::new(x, 0., 10., 10.),
            cursor: Cursor::Auto,
            app_region: None,
        }
    }

    #[test]
    fn the_last_hitbox_under_the_pointer_wins() {
        let list = DisplayList {
            revision: 1,
            items: vec![hitbox(1, 0.), hitbox(2, 5.), PaintItem::PopClip],
            rasters: Vec::new(),
            vectors: Vec::new(),
            root: None,
        };
        let none = |_: NodeId| None;
        assert_eq!(list.hit_test(2., 2., &none), Some(1));
        assert_eq!(list.hit_test(7., 2., &none), Some(2));
        assert_eq!(list.hit_test(40., 2., &none), None);
        assert_eq!(list.resolved_hitboxes(&none).len(), 2);
    }

    #[test]
    fn a_frame_moves_its_hitboxes_by_the_offset_and_cuts_them_to_the_clip() {
        let port = Rect::new(0., 0., 100., 100.);
        let list = DisplayList {
            revision: 1,
            items: vec![
                PaintItem::PushClip {
                    bounds: port,
                    radii: [0.; 4],
                },
                PaintItem::PushScroll { node: 9 },
                hitbox(1, 150.),
                PaintItem::PopScroll,
                PaintItem::PopClip,
                PaintItem::Scroller {
                    node: 9,
                    viewport: port,
                    content: [300., 100.],
                    offset: [120., 0.],
                    scrollbar: true,
                },
            ],
            rasters: Vec::new(),
            vectors: Vec::new(),
            root: None,
        };
        let own = |_: NodeId| None;
        // At the list's own offset the box (150..160 in the frame) is at 30..40 in the window.
        assert_eq!(list.hit_test(35., 5., &own), Some(1));
        assert_eq!(list.hit_test(155., 5., &own), None);
        // The shell has scrolled on to 145: the box is at 5..15, still in the clip.
        let shell = |node: NodeId| (node == 9).then_some([145., 0.]);
        assert_eq!(list.hit_test(10., 5., &shell), Some(1));
        assert_eq!(list.hit_test(35., 5., &shell), None);
        // Scrolled to zero it is outside the clip: nothing there to hit.
        let zero = |node: NodeId| (node == 9).then_some([0., 0.]);
        assert_eq!(list.hit_test(155., 5., &zero), None);
        assert!(list.resolved_hitboxes(&zero).is_empty());
    }

    #[test]
    fn the_wheel_scrolls_the_nearest_container_that_can_move_and_chains_outwards() {
        let scroller =
            |node: NodeId, size: f32, content: [f32; 2], offset: [f32; 2]| PaintItem::Scroller {
                node,
                viewport: Rect::new(0., 0., size, size),
                content,
                offset,
                scrollbar: true,
            };
        let area = |node: NodeId, size: f32| PaintItem::Hitbox {
            node,
            bounds: Rect::new(0., 0., size, size),
            cursor: Cursor::Auto,
            app_region: None,
        };
        // Container 2 (50 wide, 100 of content, scrolled to its end) is inside container 1 (100
        // wide, 400 of content). Each container's box is a hitbox, its own outside its frame: the
        // wheel starts from the topmost one under the pointer.
        let list = DisplayList {
            revision: 1,
            items: vec![
                area(1, 100.),
                PaintItem::PushScroll { node: 1 },
                area(2, 50.),
                scroller(2, 50., [50., 100.], [0., 50.]),
                PaintItem::PopScroll,
                scroller(1, 100., [100., 400.], [0., 0.]),
            ],
            rasters: Vec::new(),
            vectors: Vec::new(),
            root: None,
        };
        let own = |_: NodeId| None;
        // Down: the inner one is at its end, so the outer takes it.
        assert_eq!(list.wheel(10., 10., [0., 40.], &own), Some((1, [0., 40.])));
        // Up: the inner one can.
        assert_eq!(list.wheel(10., 10., [0., -20.], &own), Some((2, [0., 30.])));
        // Outside the inner one, the outer.
        assert_eq!(
            list.wheel(80., 80., [0., 500.], &own),
            Some((1, [0., 300.]))
        );
        // Nothing sideways in boxes that have no width to scroll, and nothing outside them.
        assert_eq!(list.wheel(10., 10., [30., 0.], &own), None);
        assert_eq!(list.wheel(500., 10., [0., 10.], &own), None);
        // The shell's own offset is the one the wheel moves on from.
        let shell = |node: NodeId| (node == 1).then_some([0., 300.]);
        assert_eq!(list.wheel(80., 80., [0., 500.], &shell), None);
    }

    #[test]
    fn opacity_groups_multiply_and_end() {
        let list = DisplayList {
            revision: 1,
            items: vec![
                PaintItem::PushOpacity(0.5),
                hitbox(1, 0.),
                PaintItem::PushOpacity(0.5),
                hitbox(2, 0.),
                PaintItem::PopOpacity,
                PaintItem::PopOpacity,
                hitbox(3, 0.),
            ],
            rasters: Vec::new(),
            vectors: Vec::new(),
            root: None,
        };
        let seen: Vec<f32> = list
            .placed(&|_| None)
            .iter()
            .map(|placed| placed.opacity)
            .collect();
        assert_eq!(seen, [0.5, 0.25, 1.]);
    }

    #[test]
    fn a_border_is_visible_when_a_side_draws() {
        assert!(!Border::NONE.is_visible());
        let mut border = Border::NONE;
        border.widths[1] = 2.;
        border.styles[1] = BorderLine::Solid;
        assert!(!border.is_visible());
        border.colors[1] = [0., 0., 0., 1.];
        assert!(border.is_visible());
    }
}

#[cfg(test)]
mod frames {
    use super::*;

    fn picture(frame: u32) -> DisplayList {
        DisplayList {
            items: vec![PaintItem::Image {
                node: 1,
                bounds: Rect::new(0., 0., 1., 1.),
                clip: Rect::new(0., 0., 1., 1.),
                image: ImageKey(3),
                fit: ObjectFit::Fill,
                frame,
            }],
            ..DisplayList::default()
        }
    }

    #[test]
    fn a_list_that_differs_only_in_frames_is_the_same_for_a_screenshot() {
        let (first, next) = (picture(0), picture(1));
        assert!(first.same_but_frames(&next));
        let moved = with_bounds(&next);
        assert!(!first.same_but_frames(&moved));
        let frames = [(ImageKey(3), 1)].into();
        assert_eq!(first.with_frames(&frames).items, next.items);
    }

    fn with_bounds(list: &DisplayList) -> DisplayList {
        let mut list = list.clone();
        if let PaintItem::Image { bounds, .. } = &mut list.items[0] {
            bounds.x = 5.;
        }
        list
    }
}
