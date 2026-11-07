//! Finishing a line: taking the spaces off its end, justifying and aligning it, giving its boxes
//! their heights, and turning what it holds into the fragments and span boxes a caller reads.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use verve_dom::Rect;

use super::breaks::is_cjk;
use super::chunks::Piece;
use super::ifc::{Ifc, TextItem};
use super::lines::{Acc, Placed, item_text, piece_width, trims};
use super::space::WhiteSpace;
use super::style::TextAlign;
use super::vertical::{self, Solved};
use super::{Fragment, InlineLayout, LineBox, SpanBox, Stop, TextFragment};
use crate::measure::{PlacedGlyph, ShapedRun};

const EPSILON: f32 = 0.01;

/// Where a line may be stretched by `text-align: justify`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Gap {
    None,
    /// After each space.
    Space,
    /// Between two characters of Chinese, Japanese or Korean text.
    Cjk,
}

/// A line's alignment, in the direction of the text.
#[derive(Copy, Clone, Debug)]
struct Align {
    /// How far the content starts from the start edge.
    offset: f32,
    /// What each gap grows by.
    extra: f32,
    gap: Gap,
}

/// The lines of an inline formatting context, laid out.
pub(super) fn all(ifc: &Ifc, mut lines: Vec<Acc>, width: f32) -> InlineLayout {
    let clamped = clamp(ifc, &mut lines);
    let count = lines.len();
    let mut out: Vec<LineBox> = Vec::with_capacity(count);
    let mut y = 0.;
    let mut reach = 0f32;
    for (at, acc) in lines.into_iter().enumerate() {
        let last = at + 1 == count;
        let ellipsis = clamped && last;
        let (line, end) = one(ifc, acc, width, y, (last, ellipsis));
        y += line.rect.height;
        reach = reach.max(end);
        out.push(line);
    }
    InlineLayout {
        height: y,
        first_baseline: out.first().map_or(0., |line| line.baseline),
        last_baseline: out.last().map_or(0., |line| line.baseline),
        width: reach,
        lines: out,
    }
}

/// `-webkit-line-clamp`: the lines after the n-th are dropped. Answers whether any were.
fn clamp(ifc: &Ifc, lines: &mut Vec<Acc>) -> bool {
    let Some(max) = ifc.root.line_clamp else {
        return false;
    };
    if !(ifc.webkit_box && ifc.clips) || lines.len() <= max as usize {
        return false;
    }
    lines.truncate(max as usize);
    true
}

/// One line as a box, and how far its content reaches.
fn one(
    ifc: &Ifc,
    mut acc: Acc,
    width: f32,
    y: f32,
    (last, clamped): (bool, bool),
) -> (LineBox, f32) {
    let start = start_of(ifc, &acc, width);
    trim(ifc, &mut acc, start, width);
    let overflow = acc.x > width + EPSILON && width.is_finite();
    if clamped || (overflow && ifc.clips && ifc.root.ellipsis) {
        ellipsize(ifc, &mut acc, width, start, clamped);
    }
    let end = acc.x;
    let align = align_of(ifc, &acc, width, last);
    let widths = widths_of(ifc, &acc.placed, &align, width);
    // A justified line is as long as its block; the boxes of its elements end there.
    let end = widths.last().map_or(end, |(x, w)| x + w);
    let basis = super::lines::basis(width);
    let solved = vertical::solve(ifc, &acc, basis);
    let place = Geometry {
        ifc,
        width,
        offset: align.offset,
        baseline: y + solved.baseline,
    };
    let line_width = if width.is_finite() { width } else { end };
    let mut line = LineBox {
        rect: Rect::new(0., y, line_width, solved.height),
        baseline: y + solved.baseline,
        fragments: Vec::new(),
        spans: Vec::new(),
    };
    place.fragments(&acc, &widths, &align, &solved, &mut line);
    place.spans(&acc, &widths, &solved, end, &mut line);
    (line, end)
}

/// Where content starts on the line: the indent on the first.
fn start_of(ifc: &Ifc, acc: &Acc, width: f32) -> f32 {
    if acc.first {
        ifc.root.indent.resolve(width)
    } else {
        0.
    }
}

/// Takes the spaces off the end of the line (they take no room) and sets every piece's place
/// again. With `pre-wrap` the spaces are kept as the line's hanging spaces; a line a newline
/// ends keeps them in its text instead, unless they overflow it (CSS Text 3 §4.1.3).
fn trim(ifc: &Ifc, acc: &mut Acc, start: f32, width: f32) {
    let overflow = acc.x > width + EPSILON && width.is_finite();
    let forced = acc.forced;
    let mut hung: Vec<Placed> = Vec::new();
    for placed in acc.placed.iter_mut().rev() {
        match placed.piece {
            Piece::Text { item, a, b } => {
                let Some(text) = item_text(ifc, item) else {
                    break;
                };
                let mode = text.style.white_space;
                if !trims(mode) || (mode == WhiteSpace::PreWrap && forced && !overflow) {
                    break;
                }
                let mut kept = b;
                while kept > a && text.chars[kept - 1] == ' ' {
                    kept -= 1;
                }
                if mode == WhiteSpace::PreWrap && kept < b {
                    hung.push(Placed {
                        piece: Piece::Text { item, a: kept, b },
                        x: 0.,
                        width: text.width(kept, b),
                        span: placed.span,
                    });
                }
                placed.piece = Piece::Text { item, a, b: kept };
                placed.width = text.width(a, kept);
                if kept > a || kept == b {
                    break;
                }
            }
            Piece::Close(_) | Piece::Open(_) | Piece::Break if placed.width == 0. => {}
            _ => break,
        }
    }
    acc.placed
        .retain(|placed| !matches!(placed.piece, Piece::Text { a, b, .. } if a == b));
    hung.reverse();
    acc.hung = hung;
    reflow(&mut acc.placed, start);
    acc.x = acc.placed.last().map_or(start, |last| last.x + last.width);
}

fn reflow(placed: &mut [Placed], start: f32) {
    let mut x = start;
    for piece in placed {
        piece.x = x;
        x += piece.width;
    }
}

/// Ends the line with an `…` when its content overflows (or `force`, for the last line of a
/// clamped block), cutting what does not leave room for it.
fn ellipsize(ifc: &Ifc, acc: &mut Acc, limit: f32, start: f32, force: bool) {
    let root = &ifc.root;
    let run = ifc.measurer.shape("\u{2026}", root.font, root.size);
    if acc.x + run.width <= limit + EPSILON {
        if force {
            acc.ellipsis = Some((acc.x, run));
        }
        return;
    }
    let target = limit - run.width;
    let mut kept: Vec<Placed> = Vec::new();
    let mut x = start;
    for placed in std::mem::take(&mut acc.placed) {
        if x + placed.width <= target + EPSILON {
            x += placed.width;
            kept.push(placed);
            continue;
        }
        if let Piece::Text { item, a, b } = placed.piece
            && let Some(text) = item_text(ifc, item)
        {
            let mut cut = a;
            while cut < b && x + text.width(a, cut + 1) <= target + EPSILON {
                cut += 1;
            }
            if cut > a {
                x += text.width(a, cut);
                kept.push(Placed {
                    piece: Piece::Text { item, a, b: cut },
                    width: text.width(a, cut),
                    ..placed
                });
            }
        }
        break;
    }
    acc.placed = kept;
    acc.x = x;
    acc.ellipsis = Some((x, run));
}

/// How far the line starts from the edge, and what each gap grows by.
fn align_of(ifc: &Ifc, acc: &Acc, width: f32, last: bool) -> Align {
    let none = Align {
        offset: 0.,
        extra: 0.,
        gap: Gap::None,
    };
    if !width.is_finite() {
        return none;
    }
    let free = (width - acc.x).max(0.);
    let rtl = ifc.root.rtl;
    let (start, end) = (0., free);
    let offset = match ifc.root.align {
        TextAlign::Center => free / 2.,
        TextAlign::End => end,
        TextAlign::Left if rtl => end,
        TextAlign::Right if !rtl => end,
        _ => start,
    };
    let justify = ifc.root.align == TextAlign::Justify
        && !last
        && !acc.forced
        && ifc.root.white_space.wraps()
        && acc.ellipsis.is_none();
    if !justify {
        return Align { offset, ..none };
    }
    let (gap, count) = count_gaps(ifc, &acc.placed);
    if count == 0 || width - acc.x <= EPSILON {
        return Align { offset, ..none };
    }
    Align {
        offset: 0.,
        extra: (width - acc.x) / count as f32,
        gap,
    }
}

/// Whether the gap after character `at` of `text[..end]` is one text-align: justify stretches.
fn is_gap(gap: Gap, text: &TextItem, at: usize, end: usize) -> bool {
    match gap {
        Gap::None => false,
        Gap::Space => matches!(text.chars[at], ' ' | '\u{A0}'),
        Gap::Cjk => at + 1 < end && is_cjk(text.chars[at]) && is_cjk(text.chars[at + 1]),
    }
}

fn count_gaps(ifc: &Ifc, placed: &[Placed]) -> (Gap, usize) {
    for gap in [Gap::Space, Gap::Cjk] {
        let count: usize = placed
            .iter()
            .filter_map(|placed| match placed.piece {
                Piece::Text { item, a, b } => {
                    let text = item_text(ifc, item)?;
                    Some((a..b).filter(|at| is_gap(gap, text, *at, b)).count())
                }
                _ => None,
            })
            .sum();
        if count > 0 {
            return (gap, count);
        }
    }
    (Gap::None, 0)
}

/// How many gaps there are before each character of `a..b`, and after the last.
fn gap_prefix(gap: Gap, text: &TextItem, a: usize, b: usize) -> Vec<u32> {
    let mut prefix = Vec::with_capacity(b - a + 1);
    let mut seen = 0;
    for at in a..b {
        prefix.push(seen);
        seen += u32::from(is_gap(gap, text, at, b));
    }
    prefix.push(seen);
    prefix
}

/// The width of each placed piece once the gaps have grown, and where it starts.
fn widths_of(ifc: &Ifc, placed: &[Placed], align: &Align, width: f32) -> Vec<(f32, f32)> {
    let mut x = placed.first().map_or(0., |first| first.x);
    placed
        .iter()
        .map(|placed| {
            let natural = piece_width(ifc, &placed.piece, width);
            let grown = match placed.piece {
                Piece::Text { item, a, b } => item_text(ifc, item).map_or(0., |text| {
                    align.extra * *gap_prefix(align.gap, text, a, b).last().unwrap_or(&0) as f32
                }),
                _ => 0.,
            };
            let at = x;
            x += natural + grown;
            (at, natural + grown)
        })
        .collect()
}

/// Where a line's boxes are, in the block's content box.
struct Geometry<'a> {
    ifc: &'a Ifc,
    width: f32,
    offset: f32,
    /// The line's baseline, from the top of the block's content.
    baseline: f32,
}

impl Geometry<'_> {
    /// The left edge of something that starts `x` from the start edge and is `w` wide.
    fn left(&self, x: f32, w: f32) -> f32 {
        if self.ifc.root.rtl && self.width.is_finite() {
            self.width - (x + self.offset) - w
        } else {
            x + self.offset
        }
    }

    fn fragments(
        &self,
        acc: &Acc,
        places: &[(f32, f32)],
        align: &Align,
        solved: &Solved,
        line: &mut LineBox,
    ) {
        let ifc = self.ifc;
        let last = acc.placed.len().saturating_sub(1);
        for (index, (placed, (x, w))) in acc.placed.iter().zip(places).enumerate() {
            match placed.piece {
                Piece::Text { item, a, b } => {
                    let Some(text) = item_text(ifc, item) else {
                        continue;
                    };
                    // The spaces that hang after the text are in its fragment.
                    let b = match joined_hang(acc) {
                        Some(end) if index == last => end,
                        _ => b,
                    };
                    let baseline = self.baseline + solved.of_span(text.span);
                    let at = self.left(*x, *w);
                    line.fragments.push(Fragment::Text(text_fragment(
                        text,
                        a..b,
                        align,
                        at,
                        baseline,
                    )));
                }
                Piece::Atomic(slot) => {
                    let atom = &ifc.atoms[slot];
                    let basis = super::lines::basis(self.width);
                    let left = atom.margin[3].resolve(basis);
                    let top = atom.baseline_from_top(basis);
                    let above =
                        self.baseline + solved.atoms.get(&slot).copied().unwrap_or(0.) - top;
                    line.fragments.push(Fragment::Atomic {
                        node: atom.node,
                        rect: Rect::new(
                            self.left(*x, *w) + left,
                            above,
                            atom.size[0],
                            atom.size[1],
                        ),
                    });
                }
                _ => {}
            }
        }
        self.hung(acc, places, solved, line);
        if let Some((x, run)) = &acc.ellipsis {
            line.fragments
                .push(Fragment::Text(self.ellipsis(*x, run, line.baseline)));
        }
    }

    /// The spaces that hang past the end of the line, after the text.
    fn hung(&self, acc: &Acc, places: &[(f32, f32)], solved: &Solved, line: &mut LineBox) {
        let none = Align {
            offset: 0.,
            extra: 0.,
            gap: Gap::None,
        };
        let mut at = places.last().map_or(acc.x, |(x, w)| x + w);
        let joined = usize::from(joined_hang(acc).is_some());
        if joined == 1 {
            at += acc.hung[0].width;
        }
        for hung in acc.hung.iter().skip(joined) {
            let Piece::Text { item, a, b } = hung.piece else {
                continue;
            };
            let Some(text) = item_text(self.ifc, item) else {
                continue;
            };
            let baseline = self.baseline + solved.of_span(text.span);
            let left = self.left(at, hung.width);
            line.fragments.push(Fragment::Text(text_fragment(
                text,
                a..b,
                &none,
                left,
                baseline,
            )));
            at += hung.width;
        }
    }

    fn ellipsis(&self, x: f32, run: &Arc<ShapedRun>, baseline: f32) -> TextFragment {
        let root = &self.ifc.root;
        TextFragment {
            node: self.ifc.node,
            range: 0..0,
            run: run.clone(),
            x: self.left(x, run.width),
            width: run.width,
            baseline,
            paint: Arc::new(root.paint(&[])),
            stops: vec![
                Stop { byte: 0, x: 0. },
                Stop {
                    byte: 0,
                    x: run.width,
                },
            ],
            generated: true,
            select_none: true,
        }
    }

    /// The border box of each inline element with content on the line.
    fn spans(
        &self,
        acc: &Acc,
        places: &[(f32, f32)],
        solved: &Solved,
        end: f32,
        line: &mut LineBox,
    ) {
        let mut extents: HashMap<usize, (f32, f32, bool, bool)> = HashMap::new();
        for span in &acc.carried {
            extents.insert(*span, (0., end, false, false));
        }
        let basis = super::lines::basis(self.width);
        for (placed, (x, w)) in acc.placed.iter().zip(places) {
            match placed.piece {
                Piece::Open(span) => {
                    let margin = self.ifc.spans[span].edges.margin[3].resolve(basis);
                    extents.insert(span, (x + margin, end, true, false));
                }
                Piece::Close(span) => {
                    let margin = self.ifc.spans[span].edges.margin[1].resolve(basis);
                    if let Some(entry) = extents.get_mut(&span) {
                        entry.1 = x + w - margin;
                        entry.3 = true;
                    }
                }
                _ => {}
            }
        }
        let mut spans: Vec<usize> = solved.spans.keys().copied().collect();
        spans.sort_unstable();
        for span in spans {
            let Some((x0, x1, starts, ends)) = extents.get(&span).copied() else {
                continue;
            };
            let rect = self.span_rect(span, (x0, x1), solved);
            line.spans.push(SpanBox {
                node: self.ifc.spans[span].node,
                rect,
                starts,
                ends,
                paint: self.ifc.spans[span].paint.clone(),
            });
        }
    }

    fn span_rect(&self, span: usize, (x0, x1): (f32, f32), solved: &Solved) -> Rect {
        let info = &self.ifc.spans[span];
        let basis = super::lines::basis(self.width);
        let metrics = &info.style.metrics;
        let [top, _, bottom, _] = info.edges.padding.map(|len| len.resolve(basis));
        let [border_top, _, border_bottom, _] = info.edges.border;
        let base = self.baseline + solved.of_span(Some(span));
        let y0 = base - metrics.ascent - top - border_top;
        let y1 = base + metrics.descent + bottom + border_bottom;
        let left = self.left(x0, x1 - x0);
        Rect::new(left, y0, x1 - x0, y1 - y0)
    }
}

/// Where the hanging spaces of a line end in the text item of its last piece when they follow it
/// directly: they are then part of that piece's fragment.
fn joined_hang(acc: &Acc) -> Option<usize> {
    let (Some(last), Some(hung)) = (acc.placed.last(), acc.hung.first()) else {
        return None;
    };
    match (last.piece, hung.piece) {
        (
            Piece::Text { item, b, .. },
            Piece::Text {
                item: other,
                a,
                b: end,
            },
        ) if item == other && a == b => Some(end),
        _ => None,
    }
}

/// The fragment for characters `range` of a text item: its glyphs and caret positions from the
/// fragment's own left edge.
fn text_fragment(
    text: &TextItem,
    range: Range<usize>,
    align: &Align,
    x: f32,
    baseline: f32,
) -> TextFragment {
    let (a, b) = (range.start, range.end);
    let prefix = gap_prefix(align.gap, text, a, b);
    let base = text.stops[a];
    let first_byte = text.processed.map[a].byte;
    let grown = |at: usize| align.extra * prefix[at - a] as f32;
    let glyphs: Vec<PlacedGlyph> = text
        .glyphs_of(a, b)
        .map(|glyph| {
            let at = text.char_of(glyph.byte);
            PlacedGlyph {
                x: glyph.x + text.shifts[at] - base + grown(at),
                byte: glyph.byte - first_byte,
                ..*glyph
            }
        })
        .collect();
    let width = text.width(a, b) + align.extra * prefix[b - a] as f32;
    let stops = (a..=b)
        .map(|at| Stop {
            byte: if text.generated { 0 } else { text.source(at) },
            x: text.stops[at] - base + grown(at),
        })
        .collect();
    TextFragment {
        node: text.node,
        range: if text.generated {
            0..0
        } else {
            text.source(a) as usize..text.source(b) as usize
        },
        run: Arc::new(ShapedRun {
            font: text.run.font,
            size: text.run.size,
            glyphs: glyphs.into(),
            width,
        }),
        x,
        width,
        baseline,
        paint: text.paint.clone(),
        stops,
        generated: text.generated,
        select_none: text.style.select_none,
    }
}
