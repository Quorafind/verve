//! A page drawn into a picture without a window: `--screenshot <path>`, and the tests that compare
//! what the layout makes of a page with what Chromium drew.
//!
//! It replays the same [plan](crate::plan) the GPUI element does, with tiny-skia. Its edges are its
//! own (anti-aliasing, the blur of a shadow), so a comparison with another renderer allows a
//! little difference. Inset shadows and dashed borders are not drawn as such: an inset shadow is
//! left out and a dashed border is solid.

use std::path::Path;
use std::sync::Arc;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, LinearGradient, Mask, Paint, Path as SkiaPath,
    PathBuilder, Pattern, Pixmap, PixmapPaint, Point, SpreadMode, Transform,
};
use verve_dom::canvas::{blur_pixels, unpremultiply};
use verve_dom::{NodeId, Rect};
use verve_layout::{DisplayList, ImageKey, ImageTable, Rgba, gradient_line};

use crate::plan::{self, Command, Paint as Fill, Shape};
use crate::raster;

/// Draws `list` into a `width` x `height` picture over `background` (`None` is transparent), with
/// the containers scrolled to the offsets the list holds except where `offset_of` says otherwise.
pub fn render(
    list: &DisplayList,
    size: (u32, u32),
    background: Option<Rgba>,
    offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
) -> Option<Pixmap> {
    render_with(list, &ImageTable::default(), size, background, offset_of)
}

/// [`render`] with the pictures the page has decoded.
pub fn render_with(
    list: &DisplayList,
    images: &ImageTable,
    (width, height): (u32, u32),
    background: Option<Rgba>,
    offset_of: &dyn Fn(NodeId) -> Option<[f32; 2]>,
) -> Option<Pixmap> {
    let mut picture = Pixmap::new(width.max(1), height.max(1))?;
    if let Some([r, g, b, a]) = background {
        picture.fill(Color::from_rgba(r, g, b, a)?);
    }
    let mut painter = Painter {
        picture,
        list,
        images,
        pages: Vec::new(),
        vectors: Vec::new(),
        rasters: Vec::new(),
        mask: None,
    };
    for command in &plan::plan(list, offset_of).commands {
        painter.draw(command);
    }
    Some(painter.picture)
}

/// [`render`] as PNG bytes, with the list's own scroll offsets.
pub fn png(
    list: &DisplayList,
    images: &ImageTable,
    size: (u32, u32),
    background: Option<Rgba>,
) -> Result<Vec<u8>, String> {
    let picture =
        render_with(list, images, size, background, &|_| None).ok_or("the picture has no size")?;
    let (width, height) = (picture.width(), picture.height());
    // The picture is premultiplied; a PNG is straight.
    let mut pixels = picture.take();
    unpremultiply(&mut pixels);
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(&pixels, width, height, ExtendedColorType::Rgba8)
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// Writes [`png`] to `path`.
pub fn write(
    path: &Path,
    list: &DisplayList,
    images: &ImageTable,
    size: (u32, u32),
    background: Option<Rgba>,
) -> Result<(), String> {
    let bytes = png(list, images, size, background)?;
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

struct Painter<'a> {
    picture: Pixmap,
    list: &'a DisplayList,
    images: &'a ImageTable,
    /// The page's pictures made into pixmaps so far, by key and frame.
    pages: Vec<((ImageKey, u32), Arc<Pixmap>)>,
    /// The SVG pictures drawn so far, by key and size.
    vectors: Vec<((ImageKey, u32, u32), Arc<Pixmap>)>,
    /// The gradients rasterised so far.
    rasters: Vec<(ImageKey, Pixmap)>,
    /// The mask of the clip the last shape was drawn under.
    mask: Option<(Rect, Mask)>,
}

impl Painter<'_> {
    fn draw(&mut self, command: &Command) {
        let mask = command.clip.and_then(|clip| self.mask_of(clip));
        match &command.shape {
            Shape::Fill {
                bounds,
                radii,
                paint,
            } => {
                let Some(path) = rounded(*bounds, *radii) else {
                    return;
                };
                self.fill(&path, *bounds, paint, mask.as_ref());
            }
            Shape::Border {
                bounds,
                radii,
                widths,
                color,
                ..
            } => {
                if let Some(ring) = ring(*bounds, *radii, *widths) {
                    let paint = solid(*color);
                    self.picture.fill_path(
                        &ring,
                        &paint,
                        FillRule::EvenOdd,
                        Transform::identity(),
                        mask.as_ref(),
                    );
                }
            }
            Shape::Shadow {
                inset: false,
                bounds,
                radii,
                offset,
                blur,
                spread,
                color,
            } => self.shadow(
                *bounds,
                *radii,
                [*offset, [*blur, *spread]],
                *color,
                mask.as_ref(),
            ),
            Shape::Shadow { .. } => {}
            // The software painter has no fonts: a line through text is a plain bar, and the
            // text itself is left out.
            Shape::Decoration { bounds, color, .. } => {
                if let Some(path) = rounded(*bounds, [0.; 4]) {
                    self.fill(&path, *bounds, &Fill::Solid(*color), mask.as_ref());
                }
            }
            Shape::Glyphs { .. } => {}
            Shape::Caret { bounds, color } => {
                if let Some(path) = rounded(*bounds, [0.; 4]) {
                    self.fill(&path, *bounds, &Fill::Solid(*color), mask.as_ref());
                }
            }
            Shape::Picture {
                bounds,
                visible,
                radii,
                image,
                frame,
                alpha,
            } => self.page_picture(
                *bounds,
                *visible,
                *radii,
                (*image, *frame, *alpha),
                mask.as_ref(),
            ),
        }
    }

    /// A picture the page loaded, the whole of it stretched over `bounds` and cut to `visible`.
    fn page_picture(
        &mut self,
        bounds: Rect,
        visible: Rect,
        radii: [f32; 4],
        (key, frame, alpha): (ImageKey, u32, f32),
        mask: Option<&Mask>,
    ) {
        let Some(pixmap) = self
            .page_pixmap(key, frame)
            .or_else(|| self.vector_pixmap(key, bounds))
        else {
            return;
        };
        // Only where the picture is: GPUI draws the part of `visible` inside `bounds`, and the
        // pad of the pattern must not stretch the picture's edge pixels into the rest.
        let Some(shown) = visible.intersection(&bounds) else {
            return;
        };
        let (Some(path), true) = (rounded(shown, radii), pixmap.width() > 0) else {
            return;
        };
        let paint = Paint {
            shader: Pattern::new(
                Pixmap::as_ref(&pixmap),
                SpreadMode::Pad,
                FilterQuality::Bilinear,
                alpha,
                Transform::from_row(
                    bounds.width / pixmap.width() as f32,
                    0.,
                    0.,
                    bounds.height / pixmap.height() as f32,
                    bounds.x,
                    bounds.y,
                ),
            ),
            anti_alias: true,
            ..Paint::default()
        };
        self.picture.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            mask,
        );
    }

    /// The SVG under `key` rasterised to fill `bounds` (one pixel to a CSS pixel), made once.
    fn vector_pixmap(&mut self, key: ImageKey, bounds: Rect) -> Option<Arc<Pixmap>> {
        let width = bounds.width.round().clamp(1., 8192.) as u32;
        let height = bounds.height.round().clamp(1., 8192.) as u32;
        if let Some((_, pixmap)) = self
            .vectors
            .iter()
            .find(|(seen, _)| *seen == (key, width, height))
        {
            return Some(Arc::clone(pixmap));
        }
        let source = self
            .list
            .vectors
            .iter()
            .find(|spec| spec.key == key)
            .map(|spec| &spec.source)
            .or_else(|| self.images.vector(key))?;
        let mut rgba = verve_layout::rasterize_svg(source, width, height)?;
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        verve_dom::canvas::premultiply(&mut rgba);
        let size = tiny_skia::IntSize::from_wh(width, height)?;
        let pixmap = Arc::new(Pixmap::from_vec(rgba, size)?);
        self.vectors
            .push(((key, width, height), Arc::clone(&pixmap)));
        Some(pixmap)
    }

    /// The frame of a page picture as a premultiplied pixmap, made once.
    fn page_pixmap(&mut self, key: ImageKey, frame: u32) -> Option<Arc<Pixmap>> {
        if let Some((_, pixmap)) = self.pages.iter().find(|(seen, _)| *seen == (key, frame)) {
            return Some(Arc::clone(pixmap));
        }
        let frames = self.images.get(key)?;
        let (bytes, _) = frames.frames.get(frame as usize)?;
        // The table is straight-alpha BGRA; tiny-skia wants premultiplied RGBA.
        let mut rgba = bytes.to_vec();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        verve_dom::canvas::premultiply(&mut rgba);
        let size = tiny_skia::IntSize::from_wh(frames.width, frames.height)?;
        let pixmap = Arc::new(Pixmap::from_vec(rgba, size)?);
        self.pages.push(((key, frame), Arc::clone(&pixmap)));
        Some(pixmap)
    }

    fn mask_of(&mut self, clip: Rect) -> Option<Mask> {
        if let Some((seen, mask)) = &self.mask
            && *seen == clip
        {
            return Some(mask.clone());
        }
        let mut mask = Mask::new(self.picture.width(), self.picture.height())?;
        let path = PathBuilder::from_rect(tiny_skia::Rect::from_xywh(
            clip.x,
            clip.y,
            clip.width.max(0.),
            clip.height.max(0.),
        )?);
        mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
        self.mask = Some((clip, mask.clone()));
        Some(mask)
    }

    fn fill(&mut self, path: &SkiaPath, bounds: Rect, with: &Fill, mask: Option<&Mask>) {
        let plain = Transform::identity();
        match with {
            Fill::Solid(color) => {
                self.picture
                    .fill_path(path, &solid(*color), FillRule::Winding, plain, mask);
            }
            Fill::Linear { angle, from, to } => {
                let Some(shader) = linear(bounds, *angle, *from, *to) else {
                    return;
                };
                let paint = Paint {
                    shader,
                    anti_alias: true,
                    ..Paint::default()
                };
                self.picture
                    .fill_path(path, &paint, FillRule::Winding, plain, mask);
            }
            Fill::Raster { key, alpha } => {
                let Some(picture) = self.raster(*key) else {
                    return;
                };
                let paint = Paint {
                    shader: Pattern::new(
                        picture.as_ref(),
                        SpreadMode::Pad,
                        FilterQuality::Bilinear,
                        *alpha,
                        // The picture is capped in size: it is stretched over the box.
                        Transform::from_row(
                            bounds.width / picture.width() as f32,
                            0.,
                            0.,
                            bounds.height / picture.height() as f32,
                            bounds.x,
                            bounds.y,
                        ),
                    ),
                    anti_alias: true,
                    ..Paint::default()
                };
                self.picture
                    .fill_path(path, &paint, FillRule::Winding, plain, mask);
            }
        }
    }

    /// The gradient `key` names, rasterised once.
    fn raster(&mut self, key: ImageKey) -> Option<Pixmap> {
        if let Some((_, picture)) = self.rasters.iter().find(|(seen, _)| *seen == key) {
            return Some(picture.clone());
        }
        let spec = self.list.rasters.iter().find(|spec| spec.key == key)?;
        let drawn = raster::rasterize(spec, 1.);
        let size = tiny_skia::IntSize::from_wh(drawn.width, drawn.height)?;
        let picture = Pixmap::from_vec(drawn.rgba, size)?;
        self.rasters.push((key, picture.clone()));
        Some(picture)
    }

    /// A blurred shadow: the box's shape grown by the spread and moved by the offset, drawn on a
    /// layer of its own, blurred, and laid on the picture.
    fn shadow(
        &mut self,
        bounds: Rect,
        radii: [f32; 4],
        [offset, [blur, spread]]: [[f32; 2]; 2],
        color: Rgba,
        mask: Option<&Mask>,
    ) {
        let reach = blur.max(0.).ceil() as i32 + 1;
        let shape = Rect::new(
            bounds.x + offset[0] - spread,
            bounds.y + offset[1] - spread,
            bounds.width + 2. * spread,
            bounds.height + 2. * spread,
        );
        let radii = radii.map(|radius| (radius + spread).max(0.));
        let left = shape.x.floor() as i32 - reach;
        let top = shape.y.floor() as i32 - reach;
        let width = (shape.width.ceil() as i32 + 2 * reach + 1).max(1) as u32;
        let height = (shape.height.ceil() as i32 + 2 * reach + 1).max(1) as u32;
        let (Some(mut layer), Some(path)) = (
            Pixmap::new(width, height),
            rounded(
                Rect::new(
                    shape.x - left as f32,
                    shape.y - top as f32,
                    shape.width,
                    shape.height,
                ),
                radii,
            ),
        ) else {
            return;
        };
        layer.fill_path(
            &path,
            &solid(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        blur_pixels(&mut layer, blur / 2.);
        self.picture.draw_pixmap(
            left,
            top,
            layer.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            mask,
        );
    }
}

fn solid([r, g, b, a]: Rgba) -> Paint<'static> {
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(Color::from_rgba(r, g, b, a).unwrap_or(Color::TRANSPARENT));
    paint
}

/// The shader of a two-stop linear gradient over `bounds`: CSS's gradient line, centred in the
/// box, as long as the box is along it.
fn linear(
    bounds: Rect,
    angle: f32,
    (from, at): (Rgba, f32),
    (to, then): (Rgba, f32),
) -> Option<tiny_skia::Shader<'static>> {
    let ([dx, dy], length) = gradient_line(angle, [bounds.width, bounds.height]);
    let (cx, cy) = (bounds.x + bounds.width / 2., bounds.y + bounds.height / 2.);
    let start = Point::from_xy(cx - dx * length / 2., cy - dy * length / 2.);
    let end = Point::from_xy(cx + dx * length / 2., cy + dy * length / 2.);
    let color = |[r, g, b, a]: Rgba| Color::from_rgba(r, g, b, a);
    LinearGradient::new(
        start,
        end,
        vec![
            GradientStop::new(at.clamp(0., 1.), color(from)?),
            GradientStop::new(then.clamp(0., 1.), color(to)?),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
}

/// A rectangle with rounded corners (top left, top right, bottom right, bottom left).
fn rounded(rect: Rect, radii: [f32; 4]) -> Option<SkiaPath> {
    let limit = rect.width.min(rect.height) / 2.;
    let [tl, tr, br, bl] = radii.map(|radius| radius.clamp(0., limit));
    // The control distance of a cubic that draws a quarter of a circle.
    let k = 0.552_284_8;
    let (x, y, right, bottom) = (rect.x, rect.y, rect.x + rect.width, rect.y + rect.height);
    let mut path = PathBuilder::new();
    path.move_to(x + tl, y);
    path.line_to(right - tr, y);
    path.cubic_to(
        right - tr * (1. - k),
        y,
        right,
        y + tr * (1. - k),
        right,
        y + tr,
    );
    path.line_to(right, bottom - br);
    path.cubic_to(
        right,
        bottom - br * (1. - k),
        right - br * (1. - k),
        bottom,
        right - br,
        bottom,
    );
    path.line_to(x + bl, bottom);
    path.cubic_to(
        x + bl * (1. - k),
        bottom,
        x,
        bottom - bl * (1. - k),
        x,
        bottom - bl,
    );
    path.line_to(x, y + tl);
    path.cubic_to(x, y + tl * (1. - k), x + tl * (1. - k), y, x + tl, y);
    path.close();
    path.finish()
}

/// The border: the box less its padding box, whose corners are rounded less by the widths.
fn ring(bounds: Rect, radii: [f32; 4], [top, right, bottom, left]: [f32; 4]) -> Option<SkiaPath> {
    let inner = Rect::new(
        bounds.x + left,
        bounds.y + top,
        (bounds.width - left - right).max(0.),
        (bounds.height - top - bottom).max(0.),
    );
    let [tl, tr, br, bl] = radii;
    let inside = [
        (tl - left.max(top)).max(0.),
        (tr - right.max(top)).max(0.),
        (br - right.max(bottom)).max(0.),
        (bl - left.max(bottom)).max(0.),
    ];
    let mut path = PathBuilder::new();
    path.push_path(&rounded(bounds, radii)?);
    path.push_path(&rounded(inner, inside)?);
    path.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use verve_layout::{Border, BorderLine, Fill as ListFill, PaintItem};

    fn list(items: Vec<PaintItem>) -> DisplayList {
        DisplayList {
            revision: 1,
            items,
            rasters: Vec::new(),
            vectors: Vec::new(),
            root: None,
        }
    }

    fn pixel(picture: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let pixel = picture.pixel(x, y).expect("a pixel");
        [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
    }

    #[test]
    fn a_picture_is_stretched_over_its_rectangle_and_cut_to_its_clip() {
        // Two pixels side by side, straight-alpha BGRA: red, then blue.
        let bytes: std::sync::Arc<[u8]> =
            std::sync::Arc::from(vec![0, 0, 255, 255, 255, 0, 0, 255]);
        let frames = verve_layout::ImageFrames {
            width: 2,
            height: 1,
            frames: vec![(bytes, std::time::Duration::ZERO)],
            loops: 1,
        };
        let key = ImageKey(5);
        let table = ImageTable::new([(key, std::sync::Arc::new(frames))].into());
        let page = list(vec![PaintItem::Image {
            node: 1,
            bounds: Rect::new(0., 0., 40., 10.),
            clip: Rect::new(0., 0., 35., 10.),
            image: key,
            fit: verve_layout::ObjectFit::Fill,
            frame: 0,
        }]);
        let picture = render_with(&page, &table, (40, 10), Some([1.; 4]), &|_| None).unwrap();
        // The texels' centers are at 10 and 30: the sampling blends between them, and is the
        // texel's color on either side.
        assert_eq!(pixel(&picture, 5, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&picture, 33, 5), [0, 0, 255, 255]);
        // Past the clip the page shows through.
        assert_eq!(pixel(&picture, 37, 5), [255, 255, 255, 255]);
    }

    fn table_of(key: ImageKey, width: u32, bgra: &[u8]) -> ImageTable {
        let frames = verve_layout::ImageFrames {
            width,
            height: (bgra.len() / 4) as u32 / width,
            frames: vec![(std::sync::Arc::from(bgra), std::time::Duration::ZERO)],
            loops: 1,
        };
        ImageTable::new([(key, std::sync::Arc::new(frames))].into())
    }

    #[test]
    fn a_picture_smaller_than_its_clip_leaves_the_rest_to_the_page() {
        // `object-fit: contain` of a 2:1 picture in a 40x10 box: it fills the middle only.
        let key = ImageKey(6);
        let table = table_of(key, 2, &[0, 0, 255, 255, 255, 0, 0, 255]);
        let page = list(vec![PaintItem::Image {
            node: 1,
            bounds: Rect::new(10., 0., 20., 10.),
            clip: Rect::new(0., 0., 40., 10.),
            image: key,
            fit: verve_layout::ObjectFit::Contain,
            frame: 0,
        }]);
        let picture = render_with(&page, &table, (40, 10), Some([1.; 4]), &|_| None).unwrap();
        assert_eq!(pixel(&picture, 5, 5), [255, 255, 255, 255]);
        assert_eq!(pixel(&picture, 12, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&picture, 28, 5), [0, 0, 255, 255]);
        assert_eq!(pixel(&picture, 35, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn background_tiles_are_painted_where_they_are_and_not_over_the_gaps() {
        // A 1x1 green picture as 5x5 tiles every 10px, 2x2 of them, in a 20x20 area.
        let key = ImageKey(7);
        let table = table_of(key, 1, &[0, 255, 0, 255]);
        let page = list(vec![PaintItem::Box {
            node: 1,
            bounds: Rect::new(0., 0., 20., 20.),
            radii: [0.; 4],
            background: vec![ListFill::Picture {
                image: key,
                area: Rect::new(0., 0., 20., 20.),
                tile: Rect::new(0., 0., 5., 5.),
                step: [10., 10.],
                count: [2, 2],
                frame: 0,
            }],
            border: Border::NONE,
            shadows: Vec::new(),
        }]);
        let picture = render_with(&page, &table, (20, 20), Some([1.; 4]), &|_| None).unwrap();
        for (x, y) in [(2, 2), (12, 2), (2, 12), (12, 12)] {
            assert_eq!(pixel(&picture, x, y), [0, 255, 0, 255], "tile at {x},{y}");
        }
        for (x, y) in [(7, 7), (17, 7), (7, 17), (17, 17), (7, 2)] {
            assert_eq!(
                pixel(&picture, x, y),
                [255, 255, 255, 255],
                "gap at {x},{y}"
            );
        }
    }

    #[test]
    fn a_box_with_a_border_is_drawn_where_the_list_says() {
        let mut border = Border::NONE;
        border.widths = [2.; 4];
        border.colors = [[0., 0., 1., 1.]; 4];
        border.styles = [BorderLine::Solid; 4];
        let page = list(vec![PaintItem::Box {
            node: 1,
            bounds: Rect::new(10., 10., 30., 20.),
            radii: [0.; 4],
            background: vec![ListFill::Color([1., 0., 0., 1.])],
            border,
            shadows: Vec::new(),
        }]);
        let picture = render(&page, (60, 40), Some([1.; 4]), &|_| None).unwrap();
        assert_eq!(pixel(&picture, 5, 5), [255, 255, 255, 255]);
        assert_eq!(pixel(&picture, 11, 20), [0, 0, 255, 255]);
        assert_eq!(pixel(&picture, 25, 20), [255, 0, 0, 255]);
        assert_eq!(pixel(&picture, 45, 20), [255, 255, 255, 255]);
    }

    #[test]
    fn a_clip_keeps_a_scrolled_box_inside_its_container() {
        let port = Rect::new(0., 0., 20., 20.);
        let page = list(vec![
            PaintItem::PushClip {
                bounds: port,
                radii: [0.; 4],
            },
            PaintItem::PushScroll { node: 9 },
            PaintItem::Box {
                node: 2,
                bounds: Rect::new(0., 30., 40., 40.),
                radii: [0.; 4],
                background: vec![ListFill::Color([0., 0., 0., 1.])],
                border: Border::NONE,
                shadows: Vec::new(),
            },
            PaintItem::PopScroll,
            PaintItem::PopClip,
            PaintItem::Scroller {
                node: 9,
                viewport: port,
                content: [40., 100.],
                offset: [0., 20.],
                scrollbar: false,
            },
        ]);
        // Scrolled 20: the box is at 10..50, clipped to the first 20 rows.
        let picture = render(&page, (40, 40), None, &|_| None).unwrap();
        assert_eq!(pixel(&picture, 5, 15), [0, 0, 0, 255]);
        assert_eq!(pixel(&picture, 5, 25)[3], 0);
        assert_eq!(pixel(&picture, 30, 15)[3], 0);
        // The shell has scrolled on to 30: the box starts at 0.
        let picture = render(&page, (40, 40), None, &|node| {
            (node == 9).then_some([0., 30.])
        })
        .unwrap();
        assert_eq!(pixel(&picture, 5, 1), [0, 0, 0, 255]);
    }

    #[test]
    fn an_svg_is_drawn_at_the_size_it_is_shown_at() {
        let key = ImageKey(8);
        let source = br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><rect width="1" height="1" fill="red"/><rect x="1" width="1" height="1" fill="blue"/></svg>"#;
        let mut page = list(vec![PaintItem::Image {
            node: 1,
            bounds: Rect::new(0., 0., 40., 20.),
            clip: Rect::new(0., 0., 40., 20.),
            image: key,
            fit: verve_layout::ObjectFit::Fill,
            frame: 0,
        }]);
        page.vectors.push(verve_layout::VectorSpec {
            key,
            source: std::sync::Arc::from(&source[..]),
        });
        let picture = render_with(
            &page,
            &ImageTable::default(),
            (40, 20),
            Some([1.; 4]),
            &|_| None,
        )
        .unwrap();
        // Two halves, with the edge between them where it falls at 40 pixels, not blurred from 2.
        assert_eq!(pixel(&picture, 5, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&picture, 19, 5), [255, 0, 0, 255]);
        assert_eq!(pixel(&picture, 21, 5), [0, 0, 255, 255]);
    }
}
