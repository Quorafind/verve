//! The layers of `background-image`: gradients, and pictures from `url()` placed by
//! `background-size`, `-position`, `-repeat`, `-origin` and `-clip`.
//!
//! A picture layer becomes a [`Fill::Picture`] once the box has its final size: the tiles are
//! worked out here (see [`crate::replaced`]) and the shell draws them as they are.

use verve_dom::canvas::sources::{resolve_against, url_of_layer};
use verve_dom::{NodeId, Rect, Snapshot};

use crate::display_list::{Fill, ImageKey, Rgba};
use crate::gradient::{self, Gradient};
use crate::images::key_for_url;
use crate::replaced::{self, MOST_TILES, Pos, Repeat, Size};
use crate::value::{Units, split_outside};

/// One layer of `background-image`.
#[derive(Clone, Debug)]
pub(crate) enum Layer {
    Gradient(Gradient),
    Picture(Box<BgPicture>),
}

/// Which edge of the box a background is placed in or cut to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Border,
    Padding,
    Content,
}

impl Edge {
    fn of(word: &str) -> Edge {
        match word.trim() {
            "padding-box" => Edge::Padding,
            "content-box" => Edge::Content,
            _ => Edge::Border,
        }
    }
}

/// A `url()` layer whose picture has arrived.
#[derive(Clone, Debug)]
pub(crate) struct BgPicture {
    key: ImageKey,
    natural: [f32; 2],
    size: Size,
    position: [Pos; 2],
    repeat: [Repeat; 2],
    origin: Edge,
    clip: Edge,
}

/// The `index`th entry of a comma list, which repeats to cover as many layers as there are.
fn nth<'a>(from: &[&'a str], index: usize, default: &'a str) -> &'a str {
    if from.is_empty() {
        default
    } else {
        from[index % from.len()]
    }
}

/// The layers of `node`'s `background-image`, the first on top. A picture that has not arrived
/// is left out; a layer that is neither is `none`.
pub(crate) fn read_layers(
    snapshot: &Snapshot,
    node: NodeId,
    color: Rgba,
    units: &Units,
) -> Vec<Layer> {
    let Some(value) = snapshot.style(node, "background-image") else {
        return Vec::new();
    };
    if !value.contains("gradient(") && !value.contains("url(") {
        return Vec::new();
    }
    let list = |property: &str| -> Vec<&str> {
        snapshot
            .style(node, property)
            .map(|value| split_outside(value, ','))
            .unwrap_or_default()
    };
    let (sizes, positions, repeats) = (
        list("background-size"),
        list("background-position"),
        list("background-repeat"),
    );
    let (origins, clips) = (list("background-origin"), list("background-clip"));
    let at = nth;
    let mut layers = Vec::new();
    for (index, layer) in split_outside(value, ',').into_iter().enumerate() {
        if let Some(gradient) = gradient::parse(layer, color, units) {
            layers.push(Layer::Gradient(gradient));
            continue;
        }
        let Some(url) =
            url_of_layer(layer).and_then(|raw| resolve_against(&snapshot.base_url, raw))
        else {
            continue;
        };
        let Some(image) = snapshot.images.get(&url) else {
            continue;
        };
        let natural = [image.natural.0 as f32, image.natural.1 as f32];
        layers.push(Layer::Picture(Box::new(BgPicture {
            key: key_for_url(&url),
            natural,
            size: replaced::size(at(&sizes, index, "auto"), units),
            position: replaced::position(at(&positions, index, "0% 0%"), units)
                .unwrap_or([Pos::START; 2]),
            repeat: replaced::repeat(at(&repeats, index, "repeat")),
            origin: Edge::of(at(&origins, index, "padding-box")),
            clip: Edge::of(at(&clips, index, "border-box")),
        })));
    }
    layers
}

impl BgPicture {
    /// The tiles of this layer in a box of `size`, with borders and paddings `[top, right, bottom,
    /// left]` as given. `None` when nothing of the layer shows.
    pub(crate) fn fill(&self, size: [f32; 2], border: [f32; 4], padding: [f32; 4]) -> Option<Fill> {
        let inset = |edge: Edge| -> [f32; 4] {
            match edge {
                Edge::Border => [0.; 4],
                Edge::Padding => border,
                Edge::Content => std::array::from_fn(|side| border[side] + padding[side]),
            }
        };
        let within = |edge: Edge| {
            let i = inset(edge);
            Rect::new(i[3], i[0], size[0] - i[1] - i[3], size[1] - i[0] - i[2])
        };
        let (clip, origin) = (within(self.clip), within(self.origin));
        if clip.width <= 0. || clip.height <= 0. || origin.width <= 0. || origin.height <= 0. {
            return None;
        }
        let tile = replaced::tile_size(self.size, self.natural, [origin.width, origin.height]);
        let x = replaced::run(
            self.repeat[0],
            tile[0],
            (origin.x, origin.width),
            self.position[0],
            (clip.x, clip.width),
        );
        let y = replaced::run(
            self.repeat[1],
            tile[1],
            (origin.y, origin.height),
            self.position[1],
            (clip.y, clip.height),
        );
        if x.count == 0 || y.count == 0 {
            return None;
        }
        // A layer cut into more tiles than this shows the first of them only.
        let across = x.count.min(MOST_TILES);
        let down = y.count.min((MOST_TILES / across).max(1));
        Some(Fill::Picture {
            image: self.key,
            area: clip,
            tile: Rect::new(x.start, y.start, x.size, y.size),
            step: [x.step, y.step],
            count: [across, down],
            frame: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(size: Size, repeat: [Repeat; 2], position: [Pos; 2]) -> BgPicture {
        BgPicture {
            key: ImageKey(7),
            natural: [20., 10.],
            size,
            position,
            repeat,
            origin: Edge::Padding,
            clip: Edge::Border,
        }
    }

    #[test]
    fn a_repeating_layer_covers_the_border_box_from_the_padding_box() {
        let auto = Size::Lengths(crate::value::Dim::Auto, crate::value::Dim::Auto);
        let picture = layer(auto, [Repeat::Tile; 2], [Pos::START; 2]);
        let fill = picture.fill([100., 50.], [2.; 4], [0.; 4]).expect("a fill");
        let Fill::Picture {
            area, tile, count, ..
        } = fill
        else {
            panic!("a picture");
        };
        assert_eq!(area, Rect::new(0., 0., 100., 50.));
        // The first tile starts in the padding box, and one more starts before it to cover the border.
        assert_eq!((tile.x, tile.y), (-18., -8.));
        assert_eq!(count, [6, 6]);
    }

    #[test]
    fn a_layer_that_does_not_repeat_is_one_tile_where_it_is_placed() {
        let auto = Size::Lengths(crate::value::Dim::Auto, crate::value::Dim::Auto);
        let picture = layer(auto, [Repeat::Single; 2], [Pos::CENTER; 2]);
        let Some(Fill::Picture { tile, count, .. }) = picture.fill([100., 50.], [0.; 4], [0.; 4])
        else {
            panic!("a picture");
        };
        assert_eq!(tile, Rect::new(40., 20., 20., 10.));
        assert_eq!(count, [1, 1]);
    }
}
