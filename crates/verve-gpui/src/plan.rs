//! What a display list comes to when it is replayed: a flat list of shapes in window coordinates,
//! each cut to one rectangle, with opacity multiplied into the colors, and the hitboxes of the
//! page where they are now.
//!
//! Both painters read it: the GPUI element draws it in a window ([`crate::dom_element`]) and the
//! software painter draws it into a picture ([`crate::screenshot`]). The frames, clips and
//! opacity groups of the list are followed by [`DisplayList::placed`], once per plan, not here;
//! this decides what each item is made of (shadows, backgrounds, a border, the scrollbars).
//!
//! Deviations, all of them v1:
//! - a clip is a rectangle (the rounded corners of an `overflow: hidden` box do not round its
//!   contents);
//! - opacity is applied to each shape, so overlapping shapes of one group blend separately;
//! - a border of several colors is drawn as four plain strips, also on a rounded box (the paint
//!   rule says one quad in the top color);
//! - a gradient of more than two stops is drawn as one quad per pair of stops, and the strips
//!   lose the box's radii;
//! - the outer shadow of a box is drawn whole, not cut out of the box, so it shows through a
//!   translucent one;
//! - `dotted` borders are dashed, and `double`, `groove`, `ridge`, `inset` and `outset` are solid;
//! - an emoji is drawn in its own colours with no opacity: GPUI's `paint_emoji` takes no colour and
//!   `with_element_opacity` is `pub(crate)` in the fork, so one that is partly faded is drawn in
//!   full (one that is fully faded is left out). The fork patch that makes `with_element_opacity`
//!   public lifts it;
//! - a glyph's vertical offset from the shaper (`ShapedGlyph.position.y`, which marks such as
//!   Arabic vowel signs use) is dropped: every glyph sits on the baseline.

use std::sync::Arc;

use verve_dom::{NodeId, Rect};
use verve_layout::{
    BorderLine, DecorationKind, DisplayList, Fill, ImageKey, PaintItem, Placed, PlacedGlyph,
    ResolvedHitbox, ResolvedScroller, Rgba,
};

/// The width of the thin scrollbar a scroll container shows, and the gap to its edge.
const THUMB: f32 = 6.;
const GAP: f32 = 2.;
/// The shortest a scrollbar's thumb gets.
const SHORTEST: f32 = 24.;
const THUMB_COLOR: Rgba = [0.3, 0.3, 0.3, 0.55];

/// What a list comes to under a set of scroll offsets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// The shapes to draw, in paint order.
    pub commands: Vec<Command>,
    /// The page's hitboxes, in paint order (later covers earlier).
    pub hitboxes: Vec<ResolvedHitbox>,
}

/// One shape to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// Nothing is drawn outside this, if it is set.
    pub clip: Option<Rect>,
    pub shape: Shape,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A `box-shadow` of the box at `bounds`; the shadow's own offset, blur and spread are
    /// applied by the painter.
    Shadow {
        bounds: Rect,
        radii: [f32; 4],
        offset: [f32; 2],
        blur: f32,
        spread: f32,
        color: Rgba,
        inset: bool,
    },
    Fill {
        bounds: Rect,
        radii: [f32; 4],
        paint: Paint,
    },
    /// Shaped text: each glyph in the font that draws it, from `origin` (the left end of the
    /// baseline). `bounds` is where all of it is, for skipping what is out of the window.
    Glyphs {
        origin: [f32; 2],
        size: f32,
        color: Rgba,
        glyphs: Arc<[PlacedGlyph]>,
        bounds: Rect,
    },
    /// An underline, overline or line-through: `bounds` is the line itself.
    Decoration {
        bounds: Rect,
        color: Rgba,
        wavy: bool,
        /// A line through the text, which GPUI draws as a strikethrough.
        through: bool,
    },
    /// A decoded picture, the whole of it drawn into `bounds` and only `visible` of that shown;
    /// `alpha` is the opacity, and the corners of `visible` are rounded by `radii`.
    Picture {
        bounds: Rect,
        visible: Rect,
        radii: [f32; 4],
        image: ImageKey,
        frame: u32,
        alpha: f32,
    },
    /// The border of a box that has one color all round, drawn in the box's own edge.
    Border {
        bounds: Rect,
        radii: [f32; 4],
        /// Top, right, bottom, left.
        widths: [f32; 4],
        color: Rgba,
        dashed: bool,
    },
}

/// What a [`Shape::Fill`] is filled with.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid(Rgba),
    /// A gradient of two stops, each a color and its place along the line in `0..=1`; the angle
    /// is CSS's, `0` up and `90` right.
    Linear {
        angle: f32,
        from: (Rgba, f32),
        to: (Rgba, f32),
    },
    /// A gradient the renderer rasterised, stretched over the box; `alpha` is the opacity.
    Raster {
        key: ImageKey,
        alpha: f32,
    },
}

/// The shapes and hitboxes of `list`. `offset_of` answers the offset the shell has scrolled a
/// container to, if it is not the one in the list.
pub fn plan(list: &DisplayList, offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>) -> Plan {
    let mut out = Plan::default();
    for placed in list.placed(offset_of) {
        match placed.item {
            PaintItem::Box { .. } => lower_box(&placed, &mut out.commands),
            PaintItem::Scroller { .. } => {
                if let Some(scroller) = placed.scroller(offset_of) {
                    lower_scrollbars(&scroller, &mut out.commands);
                }
            }
            PaintItem::Glyphs { .. } => lower_glyphs(&placed, &mut out.commands),
            PaintItem::Image { .. } => lower_image(&placed, &mut out.commands),
            PaintItem::Decoration { .. } => lower_decoration(&placed, &mut out.commands),
            PaintItem::Hitbox { .. } => out.hitboxes.extend(placed.hitbox()),
            _ => {}
        }
    }
    out
}

fn fade(color: Rgba, opacity: f32) -> Rgba {
    [color[0], color[1], color[2], color[3] * opacity]
}

fn lower_box(placed: &Placed, out: &mut Vec<Command>) {
    let PaintItem::Box {
        bounds,
        radii,
        background,
        border,
        shadows,
        ..
    } = placed.item
    else {
        return;
    };
    let bounds = placed.in_window(*bounds);
    if bounds.width <= 0. || bounds.height <= 0. {
        return;
    }
    let opacity = placed.opacity;
    let mut push = |shape: Shape| {
        out.push(Command {
            clip: placed.clip,
            shape,
        });
    };
    // The first shadow is on top, so the last is drawn first.
    for shadow in shadows.iter().rev().filter(|shadow| !shadow.inset) {
        push(shadow_of(shadow, bounds, *radii, opacity));
    }
    for fill in background {
        lower_fill(fill, bounds, *radii, opacity, &mut push);
    }
    for shadow in shadows.iter().rev().filter(|shadow| shadow.inset) {
        push(shadow_of(shadow, bounds, *radii, opacity));
    }
    if border.is_visible() {
        lower_border(border, bounds, *radii, opacity, &mut push);
    }
}

fn lower_image(placed: &Placed, out: &mut Vec<Command>) {
    let PaintItem::Image {
        bounds,
        clip,
        image,
        frame,
        ..
    } = placed.item
    else {
        return;
    };
    let visible = placed.in_window(*clip);
    let bounds = placed.in_window(*bounds);
    if visible.width <= 0. || visible.height <= 0. || bounds.width <= 0. || bounds.height <= 0. {
        return;
    }
    out.push(Command {
        clip: placed.clip,
        shape: Shape::Picture {
            bounds,
            visible,
            radii: [0.; 4],
            image: *image,
            frame: *frame,
            alpha: placed.opacity,
        },
    });
}

/// The tiles of a background picture of the box at `bounds`.
fn lower_tiles(
    fill: &Fill,
    bounds: Rect,
    radii: [f32; 4],
    opacity: f32,
    push: &mut impl FnMut(Shape),
) {
    let Fill::Picture {
        image,
        area,
        tile,
        step,
        count,
        frame,
    } = fill
    else {
        return;
    };
    let visible = Rect::new(
        bounds.x + area.x,
        bounds.y + area.y,
        area.width,
        area.height,
    );
    // The corners of the box round its background only when the picture covers the whole box,
    // and a tile takes the radius of the corners of the area it touches, not its own.
    let round = if area.width >= bounds.width && area.height >= bounds.height {
        radii
    } else {
        [0.; 4]
    };
    for j in 0..count[1] {
        for i in 0..count[0] {
            let at = Rect::new(
                bounds.x + tile.x + i as f32 * step[0],
                bounds.y + tile.y + j as f32 * step[1],
                tile.width,
                tile.height,
            );
            if let Some(cut) = at.intersection(&visible) {
                push(Shape::Picture {
                    bounds: at,
                    visible,
                    radii: touched_corners(round, cut, visible),
                    image: *image,
                    frame: *frame,
                    alpha: opacity,
                });
            }
        }
    }
}

/// The radii of the corners of `area` that the rectangle `cut` inside it touches, and nothing at
/// the others: a tile in the middle of a rounded box is square.
fn touched_corners(radii: [f32; 4], cut: Rect, area: Rect) -> [f32; 4] {
    const NEAR: f32 = 0.01;
    let left = (cut.x - area.x).abs() < NEAR;
    let top = (cut.y - area.y).abs() < NEAR;
    let right = (cut.x + cut.width - area.x - area.width).abs() < NEAR;
    let bottom = (cut.y + cut.height - area.y - area.height).abs() < NEAR;
    let [top_left, top_right, bottom_right, bottom_left] = radii;
    [
        if left && top { top_left } else { 0. },
        if right && top { top_right } else { 0. },
        if right && bottom { bottom_right } else { 0. },
        if left && bottom { bottom_left } else { 0. },
    ]
}

fn lower_glyphs(placed: &Placed, out: &mut Vec<Command>) {
    let PaintItem::Glyphs {
        origin,
        size,
        color,
        glyphs,
        ..
    } = placed.item
    else {
        return;
    };
    let at = placed.in_window(Rect::new(origin[0], origin[1], 0., 0.));
    let reach = glyphs.last().map_or(0., |glyph| glyph.x) + size * 1.5;
    out.push(Command {
        clip: placed.clip,
        shape: Shape::Glyphs {
            origin: [at.x, at.y],
            size: *size,
            color: fade(*color, placed.opacity),
            glyphs: Arc::clone(glyphs),
            bounds: Rect::new(at.x - size * 0.5, at.y - size * 1.2, reach, size * 1.8),
        },
    });
}

fn lower_decoration(placed: &Placed, out: &mut Vec<Command>) {
    let PaintItem::Decoration {
        kind,
        bounds,
        color,
        wavy,
    } = placed.item
    else {
        return;
    };
    out.push(Command {
        clip: placed.clip,
        shape: Shape::Decoration {
            bounds: placed.in_window(*bounds),
            color: fade(*color, placed.opacity),
            wavy: *wavy,
            through: *kind == DecorationKind::LineThrough,
        },
    });
}

fn shadow_of(shadow: &verve_layout::Shadow, bounds: Rect, radii: [f32; 4], opacity: f32) -> Shape {
    Shape::Shadow {
        bounds,
        radii,
        offset: shadow.offset,
        blur: shadow.blur,
        spread: shadow.spread,
        color: fade(shadow.color, opacity),
        inset: shadow.inset,
    }
}

fn filled(bounds: Rect, radii: [f32; 4], paint: Paint) -> Shape {
    Shape::Fill {
        bounds,
        radii,
        paint,
    }
}

/// The fills a background layer comes to: one, or the strips of a gradient of more than two stops
/// (which the layout only makes along an axis).
fn lower_fill(
    fill: &Fill,
    bounds: Rect,
    radii: [f32; 4],
    opacity: f32,
    push: &mut impl FnMut(Shape),
) {
    match fill {
        Fill::Color(color) => push(filled(bounds, radii, Paint::Solid(fade(*color, opacity)))),
        Fill::Picture { .. } => lower_tiles(fill, bounds, radii, opacity, push),
        Fill::Raster(key) => push(filled(
            bounds,
            radii,
            Paint::Raster {
                key: *key,
                alpha: opacity,
            },
        )),
        Fill::Linear { angle, stops } => match stops.as_slice() {
            [] => {}
            [(only, _)] => push(filled(bounds, radii, Paint::Solid(fade(*only, opacity)))),
            [(from, at), (to, then)] => push(filled(
                bounds,
                radii,
                Paint::Linear {
                    angle: *angle,
                    from: (fade(*from, opacity), *at),
                    to: (fade(*to, opacity), *then),
                },
            )),
            many => lower_strips(*angle, many, bounds, opacity, push),
        },
    }
}

/// A gradient of several stops along an axis, as one two-stop gradient between each pair of stops
/// and a plain color before the first and after the last.
fn lower_strips(
    angle: f32,
    stops: &[(Rgba, f32)],
    bounds: Rect,
    opacity: f32,
    push: &mut impl FnMut(Shape),
) {
    // Along which axis the line runs, and whether it runs from the low edge to the high one.
    let (across_x, forward) = match angle.rem_euclid(360.).round() as i32 {
        90 => (true, true),
        270 => (true, false),
        0 => (false, false),
        _ => (false, true),
    };
    let length = if across_x {
        bounds.width
    } else {
        bounds.height
    };
    // `from..to` along the line, as a rectangle.
    let band = |from: f32, to: f32| {
        let (low, size) = (from.min(to) * length, (to - from).abs() * length);
        let start = if forward { low } else { length - low - size };
        if across_x {
            Rect::new(bounds.x + start, bounds.y, size, bounds.height)
        } else {
            Rect::new(bounds.x, bounds.y + start, bounds.width, size)
        }
    };
    let clamp = |at: f32| at.clamp(0., 1.);
    let color = |c: Rgba| fade(c, opacity);
    let (first, last) = (stops[0], stops[stops.len() - 1]);
    if clamp(first.1) > 0. {
        let rect = band(0., clamp(first.1));
        push(filled(rect, [0.; 4], Paint::Solid(color(first.0))));
    }
    for pair in stops.windows(2) {
        let ((from, at), (to, then)) = (pair[0], pair[1]);
        let (at, then) = (clamp(at), clamp(then));
        if then > at {
            let paint = Paint::Linear {
                angle,
                from: (color(from), 0.),
                to: (color(to), 1.),
            };
            push(filled(band(at, then), [0.; 4], paint));
        }
    }
    if clamp(last.1) < 1. {
        let rect = band(clamp(last.1), 1.);
        push(filled(rect, [0.; 4], Paint::Solid(color(last.0))));
    }
}

fn lower_border(
    border: &verve_layout::Border,
    bounds: Rect,
    radii: [f32; 4],
    opacity: f32,
    push: &mut impl FnMut(Shape),
) {
    let widths = border.widths;
    let colors = border.colors.map(|color| fade(color, opacity));
    let drawn = |side: usize| {
        widths[side] > 0.
            && border.colors[side][3] > 0.
            && !matches!(border.styles[side], BorderLine::None | BorderLine::Hidden)
    };
    let widths_drawn = [0, 1, 2, 3].map(|side| if drawn(side) { widths[side] } else { 0. });
    let Some(first) = (0..4).find(|side| drawn(*side)) else {
        return;
    };
    if (0..4).all(|side| !drawn(side) || colors[side] == colors[first]) {
        let dashed = (0..4).any(|side| {
            drawn(side) && matches!(border.styles[side], BorderLine::Dashed | BorderLine::Dotted)
        });
        push(Shape::Border {
            bounds,
            radii,
            widths: widths_drawn,
            color: colors[first],
            dashed,
        });
        return;
    }
    // Several colors: the sides as strips, the top and the bottom taking the corners.
    let [top, right, bottom, left] = widths_drawn;
    let (x, y, w, h) = (bounds.x, bounds.y, bounds.width, bounds.height);
    let mut strip = |side: usize, rect: Rect| {
        push(filled(rect, [0.; 4], Paint::Solid(colors[side])));
    };
    if top > 0. {
        strip(0, Rect::new(x, y, w, top));
    }
    if right > 0. {
        strip(
            1,
            Rect::new(x + w - right, y + top, right, h - top - bottom),
        );
    }
    if bottom > 0. {
        strip(2, Rect::new(x, y + h - bottom, w, bottom));
    }
    if left > 0. {
        strip(3, Rect::new(x, y + top, left, h - top - bottom));
    }
}

/// The thumbs of a scroll container's scrollbars: thin, over the content, at the edges of its
/// viewport, where its scroll offset puts them.
fn lower_scrollbars(scroller: &ResolvedScroller, out: &mut Vec<Command>) {
    if !scroller.scrollbar {
        return;
    }
    let view = scroller.viewport;
    let color = fade(THUMB_COLOR, scroller.opacity);
    for vertical in [true, false] {
        let axis = usize::from(vertical);
        let (port, content) = if vertical {
            (view.height, scroller.content[1])
        } else {
            (view.width, scroller.content[0])
        };
        if scroller.most[axis] <= 0. || content <= 0. {
            continue;
        }
        let length = (port * port / content).clamp(SHORTEST.min(port), port);
        let at = scroller.offset[axis] / scroller.most[axis] * (port - length);
        let bounds = if vertical {
            Rect::new(
                view.x + view.width - THUMB - GAP,
                view.y + at,
                THUMB,
                length,
            )
        } else {
            Rect::new(
                view.x + at,
                view.y + view.height - THUMB - GAP,
                length,
                THUMB,
            )
        };
        out.push(Command {
            clip: scroller.clip,
            shape: filled(bounds, [THUMB / 2.; 4], Paint::Solid(color)),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use verve_layout::{Border, DecorationKind, FontKey, Shadow};

    const RED: Rgba = [1., 0., 0., 1.];
    const BLUE: Rgba = [0., 0., 1., 1.];

    fn boxed(background: Vec<Fill>, border: Border, shadows: Vec<Shadow>) -> PaintItem {
        PaintItem::Box {
            node: 1,
            bounds: Rect::new(10., 20., 100., 50.),
            radii: [4.; 4],
            background,
            border,
            shadows,
        }
    }

    fn list(items: Vec<PaintItem>) -> DisplayList {
        DisplayList {
            revision: 1,
            items,
            rasters: Vec::new(),
            root: None,
        }
    }

    fn shapes(items: Vec<PaintItem>) -> Vec<Shape> {
        plan(&list(items), &|_| None)
            .commands
            .into_iter()
            .map(|cmd| cmd.shape)
            .collect()
    }

    #[test]
    fn a_box_is_its_shadows_then_its_backgrounds_then_its_border() {
        let shadow = |inset| Shadow {
            offset: [1., 2.],
            blur: 3.,
            spread: 0.,
            color: [0., 0., 0., 0.5],
            inset,
        };
        let mut border = Border::NONE;
        border.widths = [1.; 4];
        border.colors = [RED; 4];
        border.styles = [BorderLine::Solid; 4];
        let drawn = shapes(vec![boxed(
            vec![Fill::Color(BLUE)],
            border,
            vec![shadow(true), shadow(false)],
        )]);
        let kinds: Vec<&str> = drawn
            .iter()
            .map(|shape| match shape {
                Shape::Shadow { inset: false, .. } => "drop",
                Shape::Shadow { inset: true, .. } => "inset",
                Shape::Fill { .. } => "fill",
                Shape::Border { .. } => "border",
                Shape::Glyphs { .. } | Shape::Decoration { .. } => "text",
                Shape::Picture { .. } => "picture",
            })
            .collect();
        assert_eq!(kinds, ["drop", "fill", "inset", "border"]);
    }

    #[test]
    fn a_frame_moves_what_is_in_it_and_opacity_fades_it() {
        let items = vec![
            PaintItem::PushScroll { node: 9 },
            PaintItem::PushOpacity(0.5),
            boxed(vec![Fill::Color(RED)], Border::NONE, Vec::new()),
            PaintItem::PopOpacity,
            PaintItem::PopScroll,
            PaintItem::Scroller {
                node: 9,
                viewport: Rect::new(0., 0., 200., 100.),
                content: [200., 400.],
                offset: [0., 15.],
                scrollbar: true,
            },
        ];
        let drawn = shapes(items);
        assert_eq!(
            drawn[0],
            Shape::Fill {
                bounds: Rect::new(10., 5., 100., 50.),
                radii: [4.; 4],
                paint: Paint::Solid([1., 0., 0., 0.5]),
            }
        );
        // The thumb: a quarter of the track, a fifteenth of the way down the 300 it can scroll.
        let Shape::Fill { bounds, .. } = &drawn[1] else {
            panic!("a scrollbar, got {drawn:?}");
        };
        assert_eq!((bounds.height, bounds.y), (25., 15. / 300. * 75.));
        assert_eq!(drawn.len(), 2, "no horizontal bar: the content fits across");
    }

    #[test]
    fn text_in_a_frame_moves_with_it_and_fades_with_its_group() {
        let glyph = |x| PlacedGlyph {
            id: 7,
            x,
            byte: 0,
            is_emoji: false,
            font: FontKey(3),
        };
        let items = vec![
            PaintItem::PushScroll { node: 9 },
            PaintItem::PushOpacity(0.5),
            PaintItem::Glyphs {
                node: 1,
                origin: [10., 30.],
                font: FontKey(3),
                size: 10.,
                color: RED,
                glyphs: vec![glyph(0.), glyph(6.)].into(),
            },
            PaintItem::Decoration {
                kind: DecorationKind::Underline,
                bounds: Rect::new(10., 32., 12., 1.),
                color: BLUE,
                wavy: true,
            },
            PaintItem::PopOpacity,
            PaintItem::PopScroll,
            PaintItem::Scroller {
                node: 9,
                viewport: Rect::new(0., 0., 200., 100.),
                content: [200., 400.],
                offset: [0., 15.],
                scrollbar: false,
            },
        ];
        let drawn = shapes(items);
        let Shape::Glyphs {
            origin,
            color,
            glyphs,
            bounds,
            ..
        } = &drawn[0]
        else {
            panic!("glyphs first, got {drawn:?}");
        };
        assert_eq!((*origin, *color), ([10., 15.], [1., 0., 0., 0.5]));
        assert_eq!(glyphs.len(), 2);
        assert!(bounds.contains(10., 15.) && bounds.contains(21., 15.));
        assert_eq!(
            drawn[1],
            Shape::Decoration {
                bounds: Rect::new(10., 17., 12., 1.),
                color: [0., 0., 1., 0.5],
                wavy: true,
                through: false,
            }
        );
    }

    #[test]
    fn more_than_two_stops_along_an_axis_are_strips_of_two() {
        let fill = Fill::Linear {
            angle: 90.,
            stops: vec![(RED, 0.), (BLUE, 0.5), (RED, 1.)],
        };
        let drawn = shapes(vec![boxed(vec![fill], Border::NONE, Vec::new())]);
        let bounds: Vec<Rect> = drawn
            .iter()
            .map(|shape| match shape {
                Shape::Fill { bounds, .. } => *bounds,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            bounds,
            [Rect::new(10., 20., 50., 50.), Rect::new(60., 20., 50., 50.)]
        );
    }

    #[test]
    fn a_border_of_four_colors_is_four_strips() {
        let mut border = Border::NONE;
        border.widths = [2.; 4];
        border.colors = [RED, BLUE, RED, BLUE];
        border.styles = [BorderLine::Solid; 4];
        let drawn = shapes(vec![boxed(Vec::new(), border, Vec::new())]);
        assert_eq!(drawn.len(), 4);
        assert!(
            drawn
                .iter()
                .all(|shape| matches!(shape, Shape::Fill { .. }))
        );
    }
}

#[cfg(test)]
mod tile_tests {
    use super::*;

    #[test]
    fn a_tile_is_round_only_at_the_corners_of_the_area_it_touches() {
        let area = Rect::new(0., 0., 100., 100.);
        let radii = [10., 20., 30., 40.];
        let tile = |x, y| touched_corners(radii, Rect::new(x, y, 20., 20.), area);
        assert_eq!(tile(0., 0.), [10., 0., 0., 0.]);
        assert_eq!(tile(80., 0.), [0., 20., 0., 0.]);
        assert_eq!(tile(80., 80.), [0., 0., 30., 0.]);
        assert_eq!(tile(0., 80.), [0., 0., 0., 40.]);
        assert_eq!(tile(40., 40.), [0.; 4]);
        assert_eq!(touched_corners(radii, area, area), radii);
    }
}
