//! The inline content of a block, as paint items: the backgrounds and borders of inline elements,
//! the selection, the glyphs of each piece of text and the lines drawn through them.
//!
//! Everything is placed from the block's content origin, in the space of the frames the block's
//! content is in (which, for a block that scrolls, includes its own frame).

use std::collections::HashMap;
use std::sync::Arc;

use verve_dom::Rect;

use super::{Effect, Painter};
use crate::display_list::{Border, DecorationKind, Fill, PaintItem, Rgba};
use crate::inline::{DecorationStyle, Fragment, LineBox, SpanBox, SpanPaint, TextFragment};
use crate::measure::TextMeasurer;
use crate::place::{Around, clips_in_frame, padding_box};

/// What painting text needs besides the boxes.
pub(crate) struct Text<'a> {
    pub measurer: &'a dyn TextMeasurer,
    /// The selected parts of each block's lines, by box, from the block's content origin.
    pub selection: &'a HashMap<usize, Vec<Rect>>,
}

/// What a selection is drawn in: Chromium's highlight blue, half see-through so the text shows.
pub(crate) const SELECTION: Rgba = [0.2, 0.565, 1., 0.45];

impl Painter<'_> {
    /// The items for the inline content of box `index`, if it holds any.
    pub(super) fn inline_content(&mut self, index: usize) {
        let tree = self.tree;
        let Some(layout) = tree.inline[index].clone() else {
            return;
        };
        let node = &tree.boxes[index];
        let owner = if node.anonymous {
            node.parent.unwrap_or(index)
        } else {
            index
        };
        if !tree.boxes[owner].props.input.visible {
            return;
        }
        let around = self.content_around(index);
        let paint = self.placed.paint_rect[index];
        let [top, _, _, left] = self.laid.border[index];
        let [pad_top, _, _, pad_left] = self.laid.padding[index];
        let origin = [paint.x + left + pad_left, paint.y + top + pad_top];
        let wanted = self.wanted(&around);
        let selected = self.text.selection.get(&index);
        let mut opened = false;
        let clipped = clips_in_frame(&around).next().is_some();
        for line in &layout.lines {
            if clipped && !reaches(line, origin, &around) {
                continue;
            }
            if !opened {
                self.enter(&wanted);
                opened = true;
            }
            for span in &line.spans {
                if let Some(paint) = &span.paint {
                    self.items.push(span_box(span, paint, origin));
                }
            }
            let parts = selected.into_iter().flatten().filter(|part| {
                part.y >= line.rect.y - 0.01 && part.y < line.rect.y + line.rect.height - 0.01
            });
            for part in parts {
                self.selection(shifted(*part, origin));
            }
            for fragment in &line.fragments {
                if let Fragment::Text(text) = fragment {
                    self.glyphs(text, origin);
                }
            }
        }
    }

    /// What the content of box `index` is painted inside: what the box itself is, and its own
    /// clip and scroll frame when it has them.
    fn content_around(&self, index: usize) -> Vec<Around> {
        let tree = self.tree;
        let node = &tree.boxes[index];
        let mut around = crate::place::around(tree, self.laid, self.placed, index);
        if node.anonymous {
            return around;
        }
        if Some(index) == tree.html {
            if self.placed.frame[index] {
                around.push(Around::Scroll(node.node));
            }
        } else if node.props.clips() {
            let (inner, radii) = padding_box(tree, self.laid, self.placed.paint_rect[index], index);
            around.push(Around::Clip(inner, radii));
            if self.placed.frame[index] {
                around.push(Around::Scroll(node.node));
            }
        }
        around
    }

    /// The effects to have open around `around`: the opacity groups being painted, then it.
    fn wanted(&self, around: &[Around]) -> Vec<Effect> {
        let mut wanted: Vec<Effect> = self
            .groups
            .iter()
            .map(|(opacity, owner)| Effect::Opacity(*opacity, *owner))
            .collect();
        wanted.extend(around.iter().map(|entry| match entry {
            Around::Clip(rect, radii) => Effect::Clip(*rect, *radii),
            Around::Scroll(node) => Effect::Scroll(*node),
        }));
        wanted
    }

    fn selection(&mut self, bounds: Rect) {
        if bounds.width <= 0. || bounds.height <= 0. {
            return;
        }
        self.items.push(PaintItem::Box {
            node: 0,
            bounds,
            radii: [0.; 4],
            background: vec![Fill::Color(SELECTION)],
            border: Border::NONE,
            shadows: Vec::new(),
        });
    }

    /// One piece of text: its shadow, its glyphs, then the lines drawn through it.
    fn glyphs(&mut self, text: &TextFragment, origin: [f32; 2]) {
        if text.run.glyphs.is_empty() {
            return;
        }
        let at = [origin[0] + text.x, origin[1] + text.baseline];
        let glyphs = &text.run.glyphs;
        if let Some(shadow) = text.paint.shadow {
            // A shadow is the text's shape in one colour: an emoji has none of its own.
            let shaped = if glyphs.iter().any(|glyph| glyph.is_emoji) {
                glyphs
                    .iter()
                    .filter(|glyph| !glyph.is_emoji)
                    .copied()
                    .collect()
            } else {
                Arc::clone(glyphs)
            };
            self.items.push(PaintItem::Glyphs {
                node: text.node,
                origin: [at[0] + shadow.offset[0], at[1] + shadow.offset[1]],
                font: text.run.font,
                size: text.run.size,
                color: shadow.color,
                glyphs: shaped,
            });
        }
        // Underlines and overlines go under the glyphs, a line through them over (CSS 2.1
        // Appendix E, 7.2.1.4.1).
        self.decorations(text, at, true);
        self.items.push(PaintItem::Glyphs {
            node: text.node,
            origin: at,
            font: text.run.font,
            size: text.run.size,
            color: text.paint.color,
            glyphs: Arc::clone(glyphs),
        });
        self.decorations(text, at, false);
    }

    /// The decorations of `text` that go under its glyphs (`under`) or over them.
    fn decorations(&mut self, text: &TextFragment, at: [f32; 2], under: bool) {
        let size = text.run.size;
        let metrics = self.text.measurer.metrics(text.run.font, size);
        for decoration in &text.paint.decorations {
            let below = decoration.kind != DecorationKind::LineThrough;
            if decoration.color[3] <= 0. || below != under {
                continue;
            }
            let thickness = decoration
                .thickness
                .unwrap_or_else(|| (size / 16.).max(1.))
                .max(0.5);
            let top = match decoration.kind {
                DecorationKind::Underline => at[1] + size * 0.1 + decoration.offset.unwrap_or(0.),
                DecorationKind::Overline => at[1] - metrics.ascent,
                DecorationKind::LineThrough => at[1] - metrics.x_height * 0.5 - thickness / 2.,
            };
            self.items.push(PaintItem::Decoration {
                kind: decoration.kind,
                bounds: Rect::new(at[0], top, text.width, thickness),
                color: decoration.color,
                wavy: decoration.style == DecorationStyle::Wavy,
            });
        }
    }
}

/// Whether anything of `line` can show through the clips it is in: its box, its inline boxes or
/// the ink of its text, which overflows a line box that is short (`line-height: 0`) or narrow.
/// The test is a plain intersection, so a box with no area still counts.
fn reaches(line: &LineBox, origin: [f32; 2], around: &[Around]) -> bool {
    let shows =
        |rect: Rect| clips_in_frame(around).all(|(clip, _)| rect.intersection(&clip).is_some());
    shows(shifted(line.rect, origin))
        || line
            .spans
            .iter()
            .any(|span| shows(shifted(span.rect, origin)))
        || line.fragments.iter().any(|fragment| match fragment {
            Fragment::Text(text) => shows(ink(text, origin)),
            Fragment::Atomic { .. } => false,
        })
}

/// Where the glyphs and decorations of `text` can reach: from about 1.2 em above the baseline to
/// 0.6 em below it, and the shadow's offset on top of that.
fn ink(text: &TextFragment, origin: [f32; 2]) -> Rect {
    let size = text.run.size;
    let [dx, dy] = text.paint.shadow.map_or([0.; 2], |shadow| shadow.offset);
    let baseline = origin[1] + text.baseline;
    Rect::new(
        origin[0] + text.x + dx.min(0.),
        baseline - 1.2 * size + dy.min(0.),
        text.width + dx.abs(),
        1.8 * size + dy.abs(),
    )
}

fn shifted(rect: Rect, origin: [f32; 2]) -> Rect {
    Rect::new(
        rect.x + origin[0],
        rect.y + origin[1],
        rect.width,
        rect.height,
    )
}

/// The box of an inline element on one line: a side that belongs to another line has no border
/// and no rounded corners (`box-decoration-break: slice`).
fn span_box(span: &SpanBox, paint: &SpanPaint, origin: [f32; 2]) -> PaintItem {
    let mut border = paint.border.clone();
    let mut radii = paint.radii;
    if !span.starts {
        border.widths[3] = 0.;
        radii[0] = 0.;
        radii[3] = 0.;
    }
    if !span.ends {
        border.widths[1] = 0.;
        radii[1] = 0.;
        radii[2] = 0.;
    }
    let background = if paint.background[3] > 0. {
        vec![Fill::Color(paint.background)]
    } else {
        Vec::new()
    };
    PaintItem::Box {
        node: span.node,
        bounds: shifted(span.rect, origin),
        radii,
        background,
        border,
        shadows: Vec::new(),
    }
}
