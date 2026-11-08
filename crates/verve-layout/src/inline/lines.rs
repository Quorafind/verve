//! Filling lines: the chunks of an inline formatting context put on lines one after the other.
//!
//! This is the greedy algorithm every browser uses: a chunk goes on the line when it fits, on the
//! next one when it does not, and a chunk that is too wide for a line of its own is broken inside
//! when `overflow-wrap` allows. A finished line is handed to [`super::finish`], which trims it,
//! aligns it and gives its boxes their heights.

use super::InlineLayout;
use super::breaks::{OverflowWrap, is_combining};
use super::chunks::{Chunk, Piece};
use super::finish;
use super::ifc::{Ifc, Item, TextItem};
use super::space::WhiteSpace;
use crate::measure::ShapedRun;
use std::sync::Arc;

/// How much a chunk may overhang a line and still fit (floating point's, not the page's).
const EPSILON: f32 = 0.01;

/// A piece put on a line.
#[derive(Clone, Debug)]
pub(super) struct Placed {
    pub piece: Piece,
    /// From the start edge of the line, indent included.
    pub x: f32,
    pub width: f32,
    /// The innermost inline element the piece is in.
    pub span: Option<usize>,
}

/// A line being filled.
#[derive(Clone, Debug, Default)]
pub(super) struct Acc {
    pub placed: Vec<Placed>,
    /// The spaces `white-space: pre-wrap` lets hang past the end of the line: they stay in the
    /// line (a caret can stand in them) but take no room.
    pub hung: Vec<Placed>,
    /// Where the next piece goes.
    pub x: f32,
    /// A piece that takes room (text or an atomic inline) has been put.
    pub content: bool,
    /// The inline elements that were open when the line began.
    pub carried: Vec<usize>,
    /// A forced break ends the line.
    pub forced: bool,
    /// The first line of the block: it takes the indent.
    pub first: bool,
    /// The ellipsis that ends the line, and where it goes.
    pub ellipsis: Option<(f32, Arc<ShapedRun>)>,
}

/// The lines so far.
struct Filler<'a> {
    ifc: &'a Ifc,
    width: f32,
    lines: Vec<Acc>,
    current: Acc,
    /// The inline elements open now, outermost first.
    stack: Vec<usize>,
}

/// The lines of `ifc` at a content width of `width`.
pub(super) fn build(ifc: &Ifc, width: f32) -> InlineLayout {
    let indent = ifc.root.indent.resolve(width);
    let mut filler = Filler {
        ifc,
        width,
        lines: Vec::new(),
        current: Acc {
            x: indent,
            first: true,
            ..Acc::default()
        },
        stack: Vec::new(),
    };
    for chunk in &ifc.chunks {
        filler.place(chunk.clone());
    }
    let lines = filler.close();
    finish::all(ifc, lines, width)
}

impl Filler<'_> {
    /// Puts a chunk on the current line, or on the lines after it.
    fn place(&mut self, mut chunk: Chunk) {
        loop {
            let (full, trailing) = widths(self.ifc, &chunk, self.width);
            let fit = full - trailing;
            let room = self.width - self.current.x;
            if fit <= room + EPSILON {
                self.put(&chunk);
                return;
            }
            if self.current.content {
                self.break_line(false);
                continue;
            }
            match split(self.ifc, &chunk, room.max(0.), self.width) {
                Some((head, tail)) => {
                    self.put(&head);
                    self.break_line(false);
                    chunk = tail;
                }
                None => {
                    self.put(&chunk);
                    return;
                }
            }
        }
    }

    /// Adds the pieces of a chunk to the current line, and ends the line if the chunk forces it.
    fn put(&mut self, chunk: &Chunk) {
        for piece in &chunk.pieces {
            let width = piece_width(self.ifc, piece, self.width);
            if !self.join(piece, width) {
                self.current.placed.push(Placed {
                    piece: *piece,
                    x: self.current.x,
                    width,
                    span: self.stack.last().copied(),
                });
            }
            self.current.x += width;
            self.track(piece);
        }
        if chunk.forced {
            self.break_line(true);
        }
    }

    /// Makes a text piece part of the one before it when it carries on from it: a line holds one
    /// piece of a text node, not one for each word. Answers whether it did.
    fn join(&mut self, piece: &Piece, width: f32) -> bool {
        let Piece::Text { item, a, b } = *piece else {
            return false;
        };
        let Some(last) = self.current.placed.last_mut() else {
            return false;
        };
        match last.piece {
            Piece::Text {
                item: before,
                a: from,
                b: until,
            } if before == item && until == a => {
                last.piece = Piece::Text { item, a: from, b };
                last.width += width;
                true
            }
            _ => false,
        }
    }

    /// Keeps the stack of open elements and the line's content up to date.
    fn track(&mut self, piece: &Piece) {
        match piece {
            Piece::Open(span) => self.stack.push(*span),
            Piece::Close(span) => {
                if let Some(at) = self.stack.iter().rposition(|open| open == span) {
                    self.stack.truncate(at);
                }
            }
            Piece::Text { .. } | Piece::Atomic(_) => self.current.content = true,
            Piece::Break => {}
        }
    }

    /// Ends the current line and starts the next one in the elements that are open.
    fn break_line(&mut self, forced: bool) {
        let next = Acc {
            carried: self.stack.clone(),
            ..Acc::default()
        };
        let mut done = std::mem::replace(&mut self.current, next);
        done.forced = forced;
        self.lines.push(done);
    }

    /// The last line is a line when it holds anything.
    fn close(mut self) -> Vec<Acc> {
        if self.current.content || !self.current.placed.is_empty() {
            self.lines.push(self.current);
        }
        self.lines
    }
}

/// The width percentages of a line's boxes are shares of: its block's, none when it is unbounded.
pub(super) fn basis(width: f32) -> f32 {
    if width.is_finite() { width } else { 0. }
}

/// The width of a piece. A text piece is the width of its characters; the edges of an inline
/// element are its margins, borders and paddings; an atomic inline is its margin box.
pub(super) fn piece_width(ifc: &Ifc, piece: &Piece, width: f32) -> f32 {
    match piece {
        Piece::Text { item, a, b } => item_text(ifc, *item).map_or(0., |text| text.width(*a, *b)),
        Piece::Open(span) => {
            let e = &ifc.spans[*span].edges;
            e.margin[3].resolve(width) + e.border[3] + e.padding[3].resolve(width)
        }
        Piece::Close(span) => {
            let e = &ifc.spans[*span].edges;
            e.margin[1].resolve(width) + e.border[1] + e.padding[1].resolve(width)
        }
        Piece::Atomic(slot) => {
            let atom = &ifc.atoms[*slot];
            let own = if width.is_finite() {
                atom.size[0]
            } else {
                atom.max_width
            };
            atom.outer_width(own, width)
        }
        Piece::Break => 0.,
    }
}

pub(super) fn item_text(ifc: &Ifc, item: usize) -> Option<&TextItem> {
    match ifc.items.get(item) {
        Some(Item::Text(text)) => Some(text),
        _ => None,
    }
}

/// The width of a chunk, and of the space at its end that does not count when it ends a line.
fn widths(ifc: &Ifc, chunk: &Chunk, width: f32) -> (f32, f32) {
    let full: f32 = chunk
        .pieces
        .iter()
        .map(|piece| piece_width(ifc, piece, width))
        .sum();
    (full, trailing_space(ifc, chunk, width))
}

/// Whether spaces at the end of a line are taken off it.
pub(super) fn trims(white_space: WhiteSpace) -> bool {
    !matches!(white_space, WhiteSpace::Pre | WhiteSpace::BreakSpaces)
}

/// The width of the spaces at the end of the chunk, skipping what takes no room.
fn trailing_space(ifc: &Ifc, chunk: &Chunk, width: f32) -> f32 {
    let mut total = 0.;
    for piece in chunk.pieces.iter().rev() {
        match piece {
            Piece::Text { item, a, b } => {
                let Some(text) = item_text(ifc, *item) else {
                    return total;
                };
                if !trims(text.style.white_space) {
                    return total;
                }
                let mut start = *b;
                while start > *a && text.chars[start - 1] == ' ' {
                    start -= 1;
                }
                total += text.width(start, *b);
                if start > *a {
                    return total;
                }
            }
            Piece::Break => {}
            other if piece_width(ifc, other, width) == 0. => {}
            _ => return total,
        }
    }
    total
}

/// Breaks a chunk too wide for a line of its own at a character, if the text allows it: the part
/// that fits in `room` (never less than one character) and the rest.
fn split(ifc: &Ifc, chunk: &Chunk, room: f32, width: f32) -> Option<(Chunk, Chunk)> {
    let mut used = 0.;
    for (at, piece) in chunk.pieces.iter().enumerate() {
        let Piece::Text { item, a, b } = *piece else {
            used += piece_width(ifc, piece, width);
            continue;
        };
        let text = item_text(ifc, item)?;
        let whole = text.width(a, b);
        if !text.style.overflow_wrap.breaks_words() || used + whole <= room + EPSILON {
            used += whole;
            continue;
        }
        let before = chunk.pieces[..at]
            .iter()
            .any(|piece| matches!(piece, Piece::Text { .. } | Piece::Atomic(_)));
        if let Some(cut) = cut_point(text, (a, b), room - used, before) {
            return Some(cut_at(chunk, at, cut));
        }
        used += whole;
    }
    None
}

/// The character of `text[a..b]` to cut before: the last one whose start is within `room`, but
/// at least the second one when nothing is on the line yet, and never in front of a combining
/// mark. `None` when there is no place inside the piece.
fn cut_point(text: &TextItem, (a, b): (usize, usize), room: f32, before: bool) -> Option<usize> {
    let mut cut = (a..b)
        .rev()
        .find(|cut| text.width(a, *cut) <= room + EPSILON)
        .unwrap_or(a);
    if cut == a && !before {
        cut = a + 1;
    }
    while cut < b && cut > a && is_combining(text.chars[cut]) {
        cut -= 1;
    }
    if cut == a && !before {
        cut = a + 1;
        while cut < b && is_combining(text.chars[cut]) {
            cut += 1;
        }
    }
    (cut < b).then_some(cut)
}

/// The chunk cut inside its text piece `at`, so that the first part holds characters up to
/// `cut`.
fn cut_at(chunk: &Chunk, at: usize, cut: usize) -> (Chunk, Chunk) {
    let mut head = Chunk::default();
    let mut tail = Chunk {
        forced: chunk.forced,
        ..Chunk::default()
    };
    for (index, piece) in chunk.pieces.iter().enumerate() {
        match (*piece, index.cmp(&at)) {
            (_, std::cmp::Ordering::Less) => head.pieces.push(*piece),
            (Piece::Text { item, a, b }, std::cmp::Ordering::Equal) => {
                if cut > a {
                    head.pieces.push(Piece::Text { item, a, b: cut });
                }
                tail.pieces.push(Piece::Text { item, a: cut, b });
            }
            _ => tail.pieces.push(*piece),
        }
    }
    (head, tail)
}

/// The width of the longest thing in the content that a line cannot break: the largest chunk,
/// without the space at its end.
pub(super) fn min_content(ifc: &Ifc) -> f32 {
    ifc.chunks
        .iter()
        .map(|chunk| min_of_chunk(ifc, chunk))
        .fold(0., f32::max)
}

fn min_of_chunk(ifc: &Ifc, chunk: &Chunk) -> f32 {
    let mut total = 0.;
    for piece in &chunk.pieces {
        total += match piece {
            Piece::Atomic(slot) => {
                let atom = &ifc.atoms[*slot];
                atom.outer_width(atom.min_width, 0.)
            }
            Piece::Text { item, a, b } => item_text(ifc, *item).map_or(0., |text| {
                if text.style.overflow_wrap == OverflowWrap::Anywhere {
                    (*a..*b).map(|at| text.width(at, at + 1)).fold(0., f32::max)
                } else {
                    text.width(*a, *b)
                }
            }),
            other => piece_width(ifc, other, 0.),
        };
    }
    total - trailing_space(ifc, chunk, 0.)
}
