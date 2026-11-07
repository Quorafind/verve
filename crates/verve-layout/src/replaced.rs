//! Where a picture goes in its box: `object-fit` and `object-position` for replaced elements,
//! and the sizes, positions and tiles of `background-image` layers.
//!
//! Everything here is geometry in CSS pixels. The shell is given the finished rectangles, so it
//! needs no CSS of its own to draw a picture.

use verve_dom::{NodeId, Rect, Snapshot};

use crate::display_list::{ImageKey, ObjectFit};
use crate::images::{canvas_key, key_for_url};
use crate::value::{Dim, Units, length_percent, words};

/// The picture a replaced element shows: an `<img>`'s, or a `<video>`'s poster.
#[derive(Clone, Debug)]
pub(crate) struct Replaced {
    pub key: ImageKey,
    /// The picture's size in CSS pixels (its pixels over its density).
    pub natural: [f32; 2],
    pub fit: ObjectFit,
    pub position: [Pos; 2],
}

/// The picture of the replaced element `node` once its bytes have arrived.
pub(crate) fn read_picture(
    snapshot: &Snapshot,
    node: NodeId,
    tag: Option<&str>,
    units: &Units,
) -> Option<Replaced> {
    // The address is borrowed: a `data:` one can be megabytes and this runs for every layout.
    let poster;
    let (url, density) = match tag? {
        "img" => {
            let (url, density) = snapshot.image_sources.get(&node)?;
            (url.as_str(), *density)
        }
        "video" => {
            let source = snapshot.dom.get(node)?.attr("poster")?;
            poster = verve_dom::canvas::sources::resolve_against(&snapshot.base_url, source)?;
            (poster.as_str(), 1.)
        }
        "canvas" => return read_surface(snapshot, node, units),
        _ => return None,
    };
    let (width, height) = snapshot.images.get(url)?.natural;
    let density = density.max(0.01);
    let style = |property: &str| snapshot.style(node, property);
    Some(Replaced {
        key: key_for_url(url),
        natural: [width as f32 / density, height as f32 / density],
        fit: object_fit(style("object-fit").unwrap_or("fill")),
        position: style("object-position")
            .and_then(|text| position(text, units))
            .unwrap_or([Pos::CENTER; 2]),
    })
}

/// Whether `node` shows a picture and nothing of what is inside it: a drawing, or a canvas (its
/// children are the fallback of a browser with no canvas). Elements that draw their own content
/// (P32's form controls) are added here, not beside the caller.
pub(crate) fn has_no_inner_boxes(snapshot: &Snapshot, node: NodeId) -> bool {
    crate::svg::is_root(snapshot, node)
        || snapshot.dom.get(node).and_then(|n| n.tag()) == Some("canvas")
}

/// The surface a `<canvas>` has painted: its pixels at the generation the document last
/// committed, as big in CSS pixels as the canvas has pixels.
fn read_surface(snapshot: &Snapshot, node: NodeId, units: &Units) -> Option<Replaced> {
    let frame = snapshot.canvases.get(&node)?;
    let style = |property: &str| snapshot.style(node, property);
    Some(Replaced {
        key: canvas_key(node, frame.generation),
        natural: [frame.width as f32, frame.height as f32],
        fit: object_fit(style("object-fit").unwrap_or("fill")),
        position: style("object-position")
            .and_then(|text| position(text, units))
            .unwrap_or([Pos::CENTER; 2]),
    })
}

/// How far along an axis a picture sits in the room it has: `percent` of the free space, then
/// `px` more.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Pos {
    pub percent: f32,
    pub px: f32,
}

impl Pos {
    pub(crate) const CENTER: Pos = Pos {
        percent: 0.5,
        px: 0.,
    };
    pub(crate) const START: Pos = Pos {
        percent: 0.,
        px: 0.,
    };

    fn at(self, free: f32) -> f32 {
        free * self.percent + self.px
    }
}

fn edge(word: &str) -> Option<(usize, f32)> {
    Some(match word {
        "left" => (0, 0.),
        "right" => (0, 1.),
        "top" => (1, 0.),
        "bottom" => (1, 1.),
        _ => return None,
    })
}

fn value(word: &str, units: &Units) -> Option<Pos> {
    Some(match word {
        "center" => Pos::CENTER,
        "left" | "top" => Pos::START,
        "right" | "bottom" => Pos {
            percent: 1.,
            px: 0.,
        },
        _ => match length_percent(word, units)? {
            Dim::Px(px) => Pos { percent: 0., px },
            Dim::Percent(percent) => Pos { percent, px: 0. },
            Dim::Auto => return None,
        },
    })
}

/// The edge-and-offset form of a position, `right 10px bottom 4px`.
fn edged(list: &[&str], units: &Units) -> Option<[Option<Pos>; 2]> {
    let mut out = [None; 2];
    let mut at = 0;
    while at < list.len() {
        let (axis, far) = edge(list[at])?;
        let offset = list
            .get(at + 1)
            .filter(|word| edge(word).is_none() && **word != "center");
        let px = match offset {
            Some(word) => pixels_of(word, units)?,
            None => 0.,
        };
        out[axis] = Some(Pos {
            percent: far,
            px: if far > 0. { -px } else { px },
        });
        at += 1 + usize::from(offset.is_some());
    }
    Some(out)
}

fn pixels_of(word: &str, units: &Units) -> Option<f32> {
    match length_percent(word, units)? {
        Dim::Px(px) => Some(px),
        _ => None,
    }
}

/// A `<position>`: one to four words, as `object-position` and `background-position` have them.
pub(crate) fn position(text: &str, units: &Units) -> Option<[Pos; 2]> {
    let list = words(text);
    let mut out = [None; 2];
    match list.as_slice() {
        [one] => {
            let on_y = matches!(*one, "top" | "bottom");
            out[usize::from(on_y)] = Some(value(one, units)?);
        }
        [first, second] => {
            let swapped = matches!(*first, "top" | "bottom") || matches!(*second, "left" | "right");
            let (x, y) = if swapped {
                (second, first)
            } else {
                (first, second)
            };
            out = [Some(value(x, units)?), Some(value(y, units)?)];
        }
        [_, _, _] | [_, _, _, _] => out = edged(&list, units)?,
        _ => return None,
    }
    Some([out[0].unwrap_or(Pos::CENTER), out[1].unwrap_or(Pos::CENTER)])
}

/// `object-fit`; anything else is `fill`.
pub(crate) fn object_fit(text: &str) -> ObjectFit {
    match text.split_whitespace().next().unwrap_or("fill") {
        "contain" => ObjectFit::Contain,
        "cover" => ObjectFit::Cover,
        "none" => ObjectFit::None,
        "scale-down" => ObjectFit::ScaleDown,
        _ => ObjectFit::Fill,
    }
}

/// The rectangle the whole picture of `natural` size is drawn into, to fill `content` as `fit`
/// says and sitting at `at`. The part outside `content` is cut by the caller.
pub(crate) fn fit_rect(fit: ObjectFit, natural: [f32; 2], content: Rect, at: [Pos; 2]) -> Rect {
    let [nw, nh] = natural;
    let contain = || {
        let scale = (content.width / nw).min(content.height / nh);
        [nw * scale, nh * scale]
    };
    let [w, h] = match fit {
        ObjectFit::Fill => [content.width, content.height],
        ObjectFit::None => [nw, nh],
        ObjectFit::Contain => contain(),
        ObjectFit::Cover => {
            let scale = (content.width / nw).max(content.height / nh);
            [nw * scale, nh * scale]
        }
        ObjectFit::ScaleDown => {
            let fitted = contain();
            if fitted[0] < nw { fitted } else { [nw, nh] }
        }
    };
    Rect::new(
        content.x + at[0].at(content.width - w),
        content.y + at[1].at(content.height - h),
        w,
        h,
    )
}

/// How a layer repeats along one axis.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Repeat {
    /// `repeat`.
    Tile,
    Space,
    Round,
    /// `no-repeat`.
    Single,
}

/// `background-repeat` of one layer as the repeat along x and y.
pub(crate) fn repeat(text: &str) -> [Repeat; 2] {
    let word = |w: &str| match w {
        "no-repeat" => Repeat::Single,
        "space" => Repeat::Space,
        "round" => Repeat::Round,
        _ => Repeat::Tile,
    };
    match words(text).as_slice() {
        ["repeat-x"] => [Repeat::Tile, Repeat::Single],
        ["repeat-y"] => [Repeat::Single, Repeat::Tile],
        [one] => [word(one); 2],
        [x, y, ..] => [word(x), word(y)],
        [] => [Repeat::Tile; 2],
    }
}

/// `background-size` of one layer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Size {
    Cover,
    Contain,
    /// A width and a height; `Dim::Auto` is the other one's ratio, or the picture's own size.
    Lengths(Dim, Dim),
}

pub(crate) fn size(text: &str, units: &Units) -> Size {
    let dim = |w: &str| length_percent(w, units).unwrap_or(Dim::Auto);
    match words(text).as_slice() {
        ["cover"] => Size::Cover,
        ["contain"] => Size::Contain,
        [one] => Size::Lengths(dim(one), Dim::Auto),
        [x, y, ..] => Size::Lengths(dim(x), dim(y)),
        [] => Size::Lengths(Dim::Auto, Dim::Auto),
    }
}

/// The size of one tile of a picture of `natural` size in a positioning area of `area`.
pub(crate) fn tile_size(size: Size, natural: [f32; 2], area: [f32; 2]) -> [f32; 2] {
    let [nw, nh] = natural;
    match size {
        Size::Cover => {
            let scale = (area[0] / nw).max(area[1] / nh);
            [nw * scale, nh * scale]
        }
        Size::Contain => {
            let scale = (area[0] / nw).min(area[1] / nh);
            [nw * scale, nh * scale]
        }
        Size::Lengths(w, h) => {
            let w = (w != Dim::Auto).then(|| w.resolve(area[0]));
            let h = (h != Dim::Auto).then(|| h.resolve(area[1]));
            match (w, h) {
                (Some(w), Some(h)) => [w, h],
                (Some(w), None) => [w, w * nh / nw],
                (None, Some(h)) => [h * nw / nh, h],
                (None, None) => [nw, nh],
            }
        }
    }
}

/// The most tiles one layer is cut into.
pub(crate) const MOST_TILES: u32 = 4096;

/// The tiles along one axis: where the first starts, how big and how far apart they are, and how
/// many.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub start: f32,
    pub size: f32,
    pub step: f32,
    pub count: u32,
}

/// The tiles of size `tile` along an axis whose positioning area runs `from..from + room`, placed
/// at `at`, repeating as `repeat` says, that cover `show` (a start and a length along the axis).
pub(crate) fn run(
    repeat: Repeat,
    tile: f32,
    (from, room): (f32, f32),
    at: Pos,
    show: (f32, f32),
) -> Run {
    let single = |tile: f32| Run {
        start: from + at.at(room - tile),
        size: tile,
        step: tile.max(1.),
        count: 1,
    };
    if tile <= 0. || !tile.is_finite() {
        return Run {
            count: 0,
            ..single(0.)
        };
    }
    let covering = |start: f32, size: f32| {
        let skipped = ((start - show.0) / size).ceil();
        let first = start - skipped * size;
        let count = ((show.0 + show.1 - first) / size).ceil().max(0.) as u32;
        Run {
            start: first,
            size,
            step: size,
            count,
        }
    };
    match repeat {
        Repeat::Single => single(tile),
        Repeat::Tile => covering(from + at.at(room - tile), tile),
        Repeat::Round => {
            let size = room / (room / tile).round().max(1.);
            covering(from + at.at(room - size), size)
        }
        Repeat::Space => {
            let fit = (room / tile).floor();
            if fit < 2. {
                return single(tile);
            }
            let gap = (room - fit * tile) / (fit - 1.);
            Run {
                start: from,
                size: tile,
                step: tile + gap,
                count: fit as u32,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units() -> Units {
        Units::new(16., 16., (800., 600.))
    }

    // The boxes below are what Electron 39.8.10 (Chromium 142, `--force-device-scale-factor=1`)
    // painted for a 40x20 picture and a 200x100 one in a 100x100 box: the bounding box of the
    // picture's pixels after `capturePage()`, which is the picture's rectangle cut to the box.
    fn seen(fit: ObjectFit, natural: [f32; 2], position_text: &str) -> [i32; 4] {
        let content = Rect::new(0., 0., 100., 100.);
        let at = position(position_text, &units()).expect("a position");
        let drawn = fit_rect(fit, natural, content, at);
        let cut = drawn.intersection(&content).expect("it shows");
        let (x0, y0) = (cut.x.round() as i32, cut.y.round() as i32);
        let x1 = (cut.x + cut.width).round() as i32;
        let y1 = (cut.y + cut.height).round() as i32;
        [x0, y0, x1 - x0, y1 - y0]
    }

    #[test]
    fn object_fit_and_position_land_where_chromium_painted_them() {
        let small = [40., 20.];
        let big = [200., 100.];
        assert_eq!(seen(ObjectFit::Fill, small, "50% 50%"), [0, 0, 100, 100]);
        assert_eq!(seen(ObjectFit::Fill, small, "10px 20px"), [10, 20, 90, 80]);
        assert_eq!(seen(ObjectFit::Contain, small, "50% 50%"), [0, 25, 100, 50]);
        assert_eq!(seen(ObjectFit::Contain, small, "0 0"), [0, 0, 100, 50]);
        assert_eq!(
            seen(ObjectFit::Contain, small, "100% 100%"),
            [0, 50, 100, 50]
        );
        assert_eq!(seen(ObjectFit::Contain, small, "25% 75%"), [0, 38, 100, 50]);
        assert_eq!(
            seen(ObjectFit::Contain, small, "10px 20px"),
            [10, 20, 90, 50]
        );
        assert_eq!(
            seen(ObjectFit::Contain, small, "right bottom"),
            [0, 50, 100, 50]
        );
        assert_eq!(seen(ObjectFit::Cover, small, "25% 75%"), [0, 0, 100, 100]);
        assert_eq!(seen(ObjectFit::Cover, small, "10px 20px"), [10, 20, 90, 80]);
        assert_eq!(seen(ObjectFit::None, small, "50% 50%"), [30, 40, 40, 20]);
        assert_eq!(seen(ObjectFit::None, small, "0 0"), [0, 0, 40, 20]);
        assert_eq!(seen(ObjectFit::None, small, "100% 100%"), [60, 80, 40, 20]);
        assert_eq!(seen(ObjectFit::None, small, "25% 75%"), [15, 60, 40, 20]);
        assert_eq!(
            seen(ObjectFit::None, small, "right bottom"),
            [60, 80, 40, 20]
        );
        assert_eq!(
            seen(ObjectFit::ScaleDown, small, "50% 50%"),
            [30, 40, 40, 20]
        );
        assert_eq!(seen(ObjectFit::None, big, "50% 50%"), [0, 0, 100, 100]);
        assert_eq!(seen(ObjectFit::None, big, "10px 20px"), [10, 20, 90, 80]);
        assert_eq!(seen(ObjectFit::ScaleDown, big, "50% 50%"), [0, 25, 100, 50]);
        assert_eq!(
            seen(ObjectFit::ScaleDown, big, "100% 100%"),
            [0, 50, 100, 50]
        );
        assert_eq!(seen(ObjectFit::ScaleDown, big, "25% 75%"), [0, 38, 100, 50]);
    }

    #[test]
    fn a_position_reads_keywords_edges_and_offsets() {
        let u = units();
        assert_eq!(position("left", &u), Some([Pos::START, Pos::CENTER]));
        assert_eq!(position("bottom", &u).map(|p| p[1].percent), Some(1.));
        let both = position("top left", &u).expect("a position");
        assert_eq!(both, [Pos::START, Pos::START]);
        let edged = position("right 10px bottom 4px", &u).expect("a position");
        assert_eq!(
            edged[0],
            Pos {
                percent: 1.,
                px: -10.
            }
        );
        assert_eq!(
            edged[1],
            Pos {
                percent: 1.,
                px: -4.
            }
        );
        assert_eq!(position("1 2 3 4 5", &u), None);
    }

    #[test]
    fn a_tile_size_follows_the_ratio_of_the_picture() {
        let area = [100., 50.];
        assert_eq!(tile_size(Size::Cover, [20., 20.], area), [100., 100.]);
        assert_eq!(tile_size(Size::Contain, [20., 20.], area), [50., 50.]);
        let w = Size::Lengths(Dim::Px(40.), Dim::Auto);
        assert_eq!(tile_size(w, [20., 10.], area), [40., 20.]);
        let pct = Size::Lengths(Dim::Percent(0.5), Dim::Percent(1.));
        assert_eq!(tile_size(pct, [20., 10.], area), [50., 50.]);
        let auto = Size::Lengths(Dim::Auto, Dim::Auto);
        assert_eq!(tile_size(auto, [20., 10.], area), [20., 10.]);
    }

    #[test]
    fn tiles_cover_the_area_and_start_before_it() {
        let shown = (0., 100.);
        let r = run(Repeat::Tile, 30., (0., 100.), Pos::START, shown);
        assert_eq!((r.start, r.step, r.count), (0., 30., 4));
        // Centered at 35..65, the tile before it straddles the left edge.
        let c = run(Repeat::Tile, 30., (0., 100.), Pos::CENTER, shown);
        assert_eq!((c.start, c.count), (-25., 5));
        let n = run(Repeat::Single, 30., (0., 100.), Pos::CENTER, shown);
        assert_eq!((n.start, n.count), (35., 1));
        let round = run(Repeat::Round, 30., (0., 100.), Pos::START, shown);
        assert_eq!((round.size, round.count), (100. / 3., 3));
        let space = run(Repeat::Space, 30., (0., 100.), Pos::START, shown);
        assert_eq!((space.count, space.step), (3, 35.));
        assert_eq!(repeat("repeat-x"), [Repeat::Tile, Repeat::Single]);
        assert_eq!(repeat("space no-repeat"), [Repeat::Space, Repeat::Single]);
    }
}
