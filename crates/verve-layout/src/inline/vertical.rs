//! A line's height and where each box sits on it: `line-height`, `vertical-align` and the
//! baselines of text and atomic inlines.
//!
//! Each inline box has a line-height box: its font's ascent and descent grown or shrunk by half
//! the difference between its `line-height` and their sum above and below (CSS 2 §10.8.1). The
//! line is as tall as the boxes of all of them together once each is aligned to its parent's
//! baseline, and a box aligned `top` or `bottom` is put at the line's edge afterwards, making the
//! line taller if it does not fit.

use std::collections::HashMap;

use super::chunks::Piece;
use super::ifc::{Ifc, Item};
use super::lines::Acc;
use super::style::{LineHeight, TextStyle, VerticalAlign};

/// `line-height` in pixels for text in `style`.
pub(super) fn line_height(style: &TextStyle) -> f32 {
    let metrics = &style.metrics;
    match style.line_height {
        LineHeight::Normal => metrics.ascent + metrics.descent + metrics.line_gap,
        LineHeight::Number(factor) => factor * style.size,
        LineHeight::Length(px) => px,
    }
}

/// How far a line-height box reaches above its baseline and below it.
pub(super) fn extents(style: &TextStyle) -> (f32, f32) {
    let metrics = &style.metrics;
    let half = (line_height(style) - (metrics.ascent + metrics.descent)) / 2.;
    (metrics.ascent + half, metrics.descent + half)
}

/// Where each box on a line is.
#[derive(Debug, Default)]
pub(super) struct Solved {
    pub height: f32,
    /// The baseline's distance from the top of the line.
    pub baseline: f32,
    /// How far below the line's baseline the baseline of each inline element, by index, is.
    pub spans: HashMap<usize, f32>,
    /// The same for each atomic inline's baseline, by index.
    pub atoms: HashMap<usize, f32>,
}

impl Solved {
    /// How far below the line's baseline the baseline of text in `span` is.
    pub(super) fn of_span(&self, span: Option<usize>) -> f32 {
        span.and_then(|span| self.spans.get(&span).copied())
            .unwrap_or(0.)
    }
}

/// What `vertical-align` asks of a box.
enum Shift {
    /// Its baseline this far below the parent's.
    By(f32),
    Top,
    Bottom,
}

fn shift(align: VerticalAlign, parent: &TextStyle, (above, below): (f32, f32), lh: f32) -> Shift {
    let metrics = &parent.metrics;
    match align {
        VerticalAlign::Baseline => Shift::By(0.),
        VerticalAlign::Sub => Shift::By(parent.size / 5. + 1.),
        VerticalAlign::Super => Shift::By(-(parent.size / 3. + 1.)),
        VerticalAlign::TextTop => Shift::By(above - metrics.ascent),
        VerticalAlign::TextBottom => Shift::By(metrics.descent - below),
        VerticalAlign::Middle => Shift::By(-metrics.x_height / 2. - (below - above) / 2.),
        VerticalAlign::Length(px) => Shift::By(-px),
        VerticalAlign::Percent(share) => Shift::By(-share * lh),
        VerticalAlign::Top => Shift::Top,
        VerticalAlign::Bottom => Shift::Bottom,
    }
}

/// Where a baseline is before the boxes aligned to the line's edges are known: this far from the
/// baseline of the nearest such box (`anchor`), or of the line.
#[derive(Copy, Clone, Debug, Default)]
struct Pos {
    from: f32,
    anchor: Option<usize>,
}

/// A box aligned to the line's top or bottom edge.
struct Edge {
    id: usize,
    top: bool,
    above: f32,
    below: f32,
}

/// What stands on a line: the inline elements with content, the text and the atomic inlines, the
/// last two with the element they are in.
struct Boxes {
    spans: Vec<usize>,
    texts: Vec<(Option<usize>, usize)>,
    atoms: Vec<(Option<usize>, usize)>,
}

fn boxes(acc: &Acc) -> Boxes {
    let mut spans: Vec<usize> = Vec::new();
    let mut texts = Vec::new();
    let mut atoms = Vec::new();
    let mut live = acc.carried.clone();
    for placed in &acc.placed {
        match placed.piece {
            // An element with nothing in it and no edges takes no part in the line's height.
            Piece::Open(span) => {
                live.push(span);
                if placed.width > 0. {
                    mark(&mut spans, &[span]);
                }
            }
            Piece::Close(span) => {
                if placed.width > 0. {
                    mark(&mut spans, &[span]);
                }
                live.retain(|open| *open != span);
            }
            Piece::Text { item, .. } => {
                mark(&mut spans, &live);
                texts.push((placed.span, item));
            }
            Piece::Atomic(slot) => {
                mark(&mut spans, &live);
                atoms.push((placed.span, slot));
            }
            Piece::Break => {}
        }
    }
    // The elements open at the start come before the ones opened on the line.
    let mut ordered = acc.carried.clone();
    ordered.extend(spans.iter().filter(|span| !acc.carried.contains(span)));
    Boxes {
        spans: ordered,
        texts,
        atoms,
    }
}

fn mark(list: &mut Vec<usize>, spans: &[usize]) {
    for span in spans {
        if !list.contains(span) {
            list.push(*span);
        }
    }
}

/// The line being worked out: how far it reaches above and below the baseline so far.
struct Solver<'a> {
    ifc: &'a Ifc,
    basis: f32,
    top: f32,
    bottom: f32,
    spans: HashMap<usize, Pos>,
    atoms: HashMap<usize, Pos>,
    edges: Vec<Edge>,
}

/// Where each box of the line goes, and how high the line is.
pub(super) fn solve(ifc: &Ifc, acc: &Acc, basis: f32) -> Solved {
    let found = boxes(acc);
    let (above, below) = extents(&ifc.root);
    let mut solver = Solver {
        ifc,
        basis,
        top: -above,
        bottom: below,
        spans: HashMap::new(),
        atoms: HashMap::new(),
        edges: Vec::new(),
    };
    for span in &found.spans {
        solver.span(*span);
    }
    for (span, item) in &found.texts {
        solver.text(*span, *item);
    }
    for (span, slot) in &found.atoms {
        solver.atom(*span, *slot);
    }
    solver.resolve()
}

impl Solver<'_> {
    fn style_of(&self, span: Option<usize>) -> &TextStyle {
        match span {
            Some(span) => &self.ifc.spans[span].style,
            None => &self.ifc.root,
        }
    }

    fn pos_of(&self, span: Option<usize>) -> Pos {
        span.and_then(|span| self.spans.get(&span).copied())
            .unwrap_or_default()
    }

    /// Takes the box into the line's height, unless it waits for the line's edge.
    fn reach(&mut self, pos: Pos, (above, below): (f32, f32)) {
        if pos.anchor.is_none() {
            self.top = self.top.min(pos.from - above);
            self.bottom = self.bottom.max(pos.from + below);
        }
    }

    /// Aligns a box of `style` in `parent`, or sets it aside for the line's edge as `id`.
    fn align(
        &mut self,
        parent: Option<usize>,
        id: usize,
        style: &TextStyle,
        size: (f32, f32),
    ) -> Pos {
        let at = self.pos_of(parent);
        let lh = line_height(style);
        match shift(style.vertical_align, self.style_of(parent), size, lh) {
            Shift::By(by) => Pos {
                from: at.from + by,
                anchor: at.anchor,
            },
            Shift::Top | Shift::Bottom if at.anchor.is_some() => at,
            edge => {
                let top = matches!(edge, Shift::Top);
                self.edges.push(Edge {
                    id,
                    top,
                    above: size.0,
                    below: size.1,
                });
                Pos {
                    from: 0.,
                    anchor: Some(id),
                }
            }
        }
    }

    fn span(&mut self, span: usize) {
        let ifc = self.ifc;
        let info = &ifc.spans[span];
        let style = info.style.clone();
        let size = extents(&style);
        let pos = self.align(info.parent, span, &style, size);
        self.spans.insert(span, pos);
        self.reach(pos, size);
    }

    fn text(&mut self, span: Option<usize>, item: usize) {
        let ifc = self.ifc;
        let Some(Item::Text(text)) = ifc.items.get(item) else {
            return;
        };
        let pos = self.pos_of(span);
        self.reach(pos, extents(&text.style));
    }

    fn atom(&mut self, span: Option<usize>, slot: usize) {
        let ifc = self.ifc;
        let atom = &ifc.atoms[slot];
        let style = atom.style.clone();
        let size = atom.extents(self.basis);
        // The ids of atomic inlines count down from the top, away from the elements'.
        let pos = self.align(span, usize::MAX - slot, &style, size);
        self.atoms.insert(slot, pos);
        self.reach(pos, size);
    }

    /// Grows the line for the boxes aligned to its edges, then reads every box's baseline.
    fn resolve(mut self) -> Solved {
        let tallest = |top: bool| {
            self.edges
                .iter()
                .filter(|edge| edge.top == top)
                .map(|edge| edge.above + edge.below)
                .fold(0., f32::max)
        };
        let (need_top, need_bottom) = (tallest(true), tallest(false));
        if need_top > self.bottom - self.top {
            self.bottom = self.top + need_top;
        }
        if need_bottom > self.bottom - self.top {
            self.top = self.bottom - need_bottom;
        }
        let anchored: HashMap<usize, f32> = self
            .edges
            .iter()
            .map(|edge| {
                let at = if edge.top {
                    self.top + edge.above
                } else {
                    self.bottom - edge.below
                };
                (edge.id, at)
            })
            .collect();
        let absolute =
            |pos: &Pos| pos.from + pos.anchor.and_then(|id| anchored.get(&id)).unwrap_or(&0.);
        Solved {
            height: self.bottom - self.top,
            baseline: -self.top,
            spans: self
                .spans
                .iter()
                .map(|(id, pos)| (*id, absolute(pos)))
                .collect(),
            atoms: self
                .atoms
                .iter()
                .map(|(id, pos)| (*id, absolute(pos)))
                .collect(),
        }
    }
}
