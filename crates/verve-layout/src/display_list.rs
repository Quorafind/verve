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
}

impl DisplayList {
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
        let offsets: HashMap<NodeId, [f32; 2]> = self
            .items
            .iter()
            .filter_map(|item| match item {
                PaintItem::Scroller { node, offset, .. } => Some((*node, *offset)),
                _ => None,
            })
            .collect();
        let mut found = Vec::new();
        // What each open effect had before it: the clip so far and the scroll so far.
        let mut open: Vec<(Option<Rect>, [f32; 2])> = Vec::new();
        let (mut clip, mut shift): (Option<Rect>, [f32; 2]) = (None, [0.; 2]);
        for item in &self.items {
            match item {
                PaintItem::PushClip { bounds, .. } => {
                    open.push((clip, shift));
                    let seen = Rect::new(
                        bounds.x - shift[0],
                        bounds.y - shift[1],
                        bounds.width,
                        bounds.height,
                    );
                    clip = Some(clip.map_or(seen, |outer| meet(outer, seen)));
                }
                PaintItem::PushScroll { node } => {
                    open.push((clip, shift));
                    let offset = offset_of(*node)
                        .or_else(|| offsets.get(node).copied())
                        .unwrap_or([0.; 2]);
                    shift = [shift[0] + offset[0], shift[1] + offset[1]];
                }
                PaintItem::PushOpacity(_) => open.push((clip, shift)),
                PaintItem::PopClip | PaintItem::PopScroll | PaintItem::PopOpacity => {
                    (clip, shift) = open.pop().unwrap_or((None, [0.; 2]));
                }
                PaintItem::Hitbox {
                    node,
                    bounds,
                    cursor,
                    app_region,
                } => {
                    let seen = Rect::new(
                        bounds.x - shift[0],
                        bounds.y - shift[1],
                        bounds.width,
                        bounds.height,
                    );
                    let seen = clip.map_or(seen, |clip| meet(clip, seen));
                    if seen.width > 0. && seen.height > 0. {
                        found.push(ResolvedHitbox {
                            node: *node,
                            bounds: seen,
                            cursor: *cursor,
                            app_region: *app_region,
                        });
                    }
                }
                _ => {}
            }
        }
        found
    }
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
    /// A picture, drawn into `bounds` and cut to `clip` (P35).
    Image {
        node: NodeId,
        bounds: Rect,
        clip: Rect,
        image: ImageKey,
        fit: ObjectFit,
    },
    /// A form control the shell draws in the platform's way (P32).
    Control {
        node: NodeId,
        bounds: Rect,
        kind: ControlKind,
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
}

/// A picture the renderer owns, by number. The keys of rasterised gradients have the top bit set;
/// pictures the page loads (P35) leave it clear.
#[derive(Copy, Clone, Debug, Hash, Eq, PartialEq)]
pub struct ImageKey(pub u64);

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

/// The form controls the shell draws itself (P32 gives it its variants).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ControlKind {}

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
