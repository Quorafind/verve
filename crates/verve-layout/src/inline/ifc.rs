//! The content of an inline formatting context, ready to be laid out at any width, and the
//! measure function taffy asks it for.

use std::collections::HashMap;
use std::sync::Arc;

use taffy::{AvailableSpace, Size};
use verve_dom::NodeId;

use super::InlineLayout;
use super::chunks::{self, Chunk};
use super::space::{CharMap, Processed};
use super::style::{Edges, SpanPaint, TextPaint, TextStyle};
use crate::measure::{PlacedGlyph, ShapedRun, TextMeasurer};
use crate::value::Dim;

/// One thing in the content, in document order.
#[derive(Clone, Debug)]
pub(crate) enum Item {
    Text(TextItem),
    /// The start of the inline element with this index in [`Ifc::spans`].
    Open(usize),
    Close(usize),
    /// The atomic inline with this index in [`Ifc::atoms`].
    Atomic(usize),
    /// A forced line break: `<br>`, or a newline where white space is kept.
    Break,
    /// `<wbr>`: a line may break here.
    WordBreak,
}

/// Text of one element, processed and shaped.
#[derive(Clone, Debug)]
pub(crate) struct TextItem {
    /// The text node; for generated text, the element it belongs to.
    pub node: NodeId,
    pub generated: bool,
    /// The innermost inline element the text is in.
    pub span: Option<usize>,
    pub style: Arc<TextStyle>,
    pub paint: Arc<TextPaint>,
    pub processed: Processed,
    /// The characters of the processed text.
    pub chars: Vec<char>,
    /// The shaped text, as the measurer made it: positions without letter and word spacing.
    pub run: Arc<ShapedRun>,
    /// The x of every character boundary, spacing added: one more than there are characters.
    pub stops: Vec<f32>,
    /// What the spacing added before each boundary, so a glyph's position follows its character.
    pub shifts: Vec<f32>,
    /// Where the text ends in the text node: the byte after the last character's, which is a
    /// preserved newline's when one follows, and the end of the node's text otherwise.
    pub source_len: u32,
    /// The indices of `run.glyphs`, ordered by the byte they start, so the glyphs of a range of
    /// characters are found by search.
    pub order: Vec<u32>,
}

impl TextItem {
    /// The width of characters `a..b`.
    pub(crate) fn width(&self, a: usize, b: usize) -> f32 {
        self.stops[b] - self.stops[a]
    }

    /// The byte of the text node that character `at` stands for; one past the last character is
    /// the end of the node's text.
    pub(crate) fn source(&self, at: usize) -> u32 {
        self.processed
            .map
            .get(at)
            .map_or(self.source_len, |entry| entry.source)
    }

    /// The glyphs of characters `a..b`, in the order the run has them.
    pub(crate) fn glyphs_of(&self, a: usize, b: usize) -> impl Iterator<Item = &PlacedGlyph> {
        let low = self.processed.map[a].byte;
        let high = self
            .processed
            .map
            .get(b)
            .map_or(u32::MAX, |entry| entry.byte);
        let glyphs = &self.run.glyphs;
        let from = self
            .order
            .partition_point(|i| glyphs[*i as usize].byte < low);
        let to = self
            .order
            .partition_point(|i| glyphs[*i as usize].byte < high);
        let mut picked: Vec<u32> = self.order[from..to].to_vec();
        picked.sort_unstable();
        picked.into_iter().map(move |i| &glyphs[i as usize])
    }

    /// The index of the character the glyph for processed byte `byte` starts.
    pub(crate) fn char_of(&self, byte: u32) -> usize {
        self.processed
            .map
            .partition_point(|entry: &CharMap| entry.byte <= byte)
            .saturating_sub(1)
    }
}

/// An inline element: what its boxes on a line need.
#[derive(Clone, Debug)]
pub(crate) struct Span {
    pub node: NodeId,
    pub parent: Option<usize>,
    pub style: Arc<TextStyle>,
    pub edges: Edges,
    /// Its background and border, if it has any.
    pub paint: Option<Arc<SpanPaint>>,
}

/// An atomic inline: a box taffy lays out, which a line holds as one unbreakable thing.
#[derive(Clone, Debug)]
pub(crate) struct Atom {
    pub node: NodeId,
    /// The box of the tree this is.
    pub box_index: usize,
    pub style: Arc<TextStyle>,
    pub margin: [Dim; 4],
    /// The border box at the width available to it now.
    pub size: [f32; 2],
    /// From the top of the border box; `None` puts the bottom margin edge on the baseline
    /// (CSS 2.1 §10.8.1).
    pub baseline: Option<f32>,
    /// The border box's width when the line has room for it all, and when it has none.
    pub max_width: f32,
    pub min_width: f32,
    /// The width of the block's content it was sized for.
    pub avail: Option<f32>,
}

impl Atom {
    /// How far the box reaches above its baseline and below it, margins in.
    pub(crate) fn extents(&self, basis: f32) -> (f32, f32) {
        let Some(baseline) = self.baseline else {
            let height = self.size[1];
            let (top, bottom) = (self.margin[0].resolve(basis), self.margin[2].resolve(basis));
            return (top + height + bottom, 0.);
        };
        (
            self.margin[0].resolve(basis) + baseline,
            self.size[1] - baseline + self.margin[2].resolve(basis),
        )
    }

    /// The distance from the top of the border box to the baseline the box sits on.
    pub(crate) fn baseline_from_top(&self, basis: f32) -> f32 {
        self.baseline
            .unwrap_or_else(|| self.size[1] + self.margin[2].resolve(basis))
    }

    /// The width a line gives the box, margins in, when it is `size`, `max_width` or `min_width`.
    pub(crate) fn outer_width(&self, width: f32, basis: f32) -> f32 {
        self.margin[1].resolve(basis) + self.margin[3].resolve(basis) + width
    }
}

/// An inline formatting context.
pub(crate) struct Ifc {
    pub node: NodeId,
    pub root: Arc<TextStyle>,
    pub items: Vec<Item>,
    pub spans: Vec<Span>,
    pub atoms: Vec<Atom>,
    /// The block clips its overflow, so `text-overflow` and `-webkit-line-clamp` apply.
    pub clips: bool,
    /// The block is `display: -webkit-box`.
    pub webkit_box: bool,
    pub measurer: Arc<dyn TextMeasurer>,
    pub(super) chunks: Vec<Chunk>,
    laid: HashMap<u32, Arc<InlineLayout>>,
    min: Option<f32>,
    max: Option<f32>,
}

/// What an [`Ifc`] is made from.
pub(crate) struct Parts {
    pub node: NodeId,
    pub root: Arc<TextStyle>,
    pub items: Vec<Item>,
    pub spans: Vec<Span>,
    pub atoms: Vec<Atom>,
    pub clips: bool,
    pub webkit_box: bool,
}

impl Ifc {
    pub(crate) fn new(measurer: Arc<dyn TextMeasurer>, parts: Parts) -> Self {
        let chunks = chunks::build(&parts.items, &parts.spans, &parts.root);
        Self {
            node: parts.node,
            root: parts.root,
            items: parts.items,
            spans: parts.spans,
            atoms: parts.atoms,
            clips: parts.clips,
            webkit_box: parts.webkit_box,
            measurer,
            chunks,
            laid: HashMap::new(),
            min: None,
            max: None,
        }
    }

    /// The lines at a content width of `width`; `f32::INFINITY` lays out without wrapping.
    pub(crate) fn layout(&mut self, width: f32) -> Arc<InlineLayout> {
        let key = width.to_bits();
        if let Some(found) = self.laid.get(&key) {
            return found.clone();
        }
        let made = Arc::new(super::lines::build(self, width));
        if self.laid.len() > 16 {
            self.laid.clear();
        }
        self.laid.insert(key, made.clone());
        made
    }

    /// The width of the longest line when nothing wraps but forced breaks.
    pub(crate) fn max_content(&mut self) -> f32 {
        if let Some(width) = self.max {
            return width;
        }
        let width = self.layout(f32::INFINITY).width;
        self.max = Some(width);
        width
    }

    /// The width of the longest thing that cannot be broken.
    pub(crate) fn min_content(&mut self) -> f32 {
        if let Some(width) = self.min {
            return width;
        }
        let width = super::lines::min_content(self);
        self.min = Some(width);
        width
    }

    /// Takes the size an atomic inline now has. Answers whether it is not the one it had, which
    /// makes every line laid out so far stale.
    pub(crate) fn set_atom(&mut self, slot: usize, update: Atom) -> bool {
        let before = &self.atoms[slot];
        let same = before.size == update.size
            && before.baseline == update.baseline
            && before.max_width == update.max_width
            && before.min_width == update.min_width;
        self.atoms[slot] = update;
        if same {
            return false;
        }
        self.laid.clear();
        self.min = None;
        self.max = None;
        true
    }

    /// The content size taffy asks for: a width it knows, or the content's own, and the height
    /// that width makes. `fills_room` says the box will be as wide as the room it is offered.
    pub(crate) fn measure(
        &mut self,
        known: Size<Option<f32>>,
        available: Size<AvailableSpace>,
        fills_room: bool,
    ) -> Size<f32> {
        let width = known.width.unwrap_or_else(|| match available.width {
            AvailableSpace::MinContent => self.min_content(),
            AvailableSpace::MaxContent => self.max_content(),
            // A box that fills the room takes all of it, even when a word is wider (it breaks
            // there with `overflow-wrap`); one that shrinks to fit never gets narrower than
            // its longest unbreakable run.
            AvailableSpace::Definite(room) if fills_room => room,
            AvailableSpace::Definite(room) => self.max_content().min(room).max(self.min_content()),
        });
        let height = known.height.unwrap_or_else(|| {
            // A line box wider than the room is still laid out as wide as it is: only an
            // unbounded width makes the lines not wrap.
            self.layout(width).height
        });
        Size { width, height }
    }
}

/// The indices of a run's glyphs ordered by the byte they start.
pub(crate) fn glyph_order(run: &ShapedRun) -> Vec<u32> {
    let mut order: Vec<u32> = (0..run.glyphs.len() as u32).collect();
    order.sort_by_key(|i| run.glyphs[*i as usize].byte);
    order
}

/// The x of every character boundary of `run`'s text, and how far letter and word spacing moved
/// each one. A glyph is for the character its byte starts; the boundaries inside a ligature are
/// spread evenly over it.
pub(crate) fn stops_of(
    run: &ShapedRun,
    processed: &Processed,
    chars: &[char],
    letter_spacing: f32,
    word_spacing: f32,
) -> (Vec<f32>, Vec<f32>) {
    let count = chars.len();
    let mut raw: Vec<Option<f32>> = vec![None; count + 1];
    for glyph in run.glyphs.iter() {
        let at = processed
            .map
            .partition_point(|entry| entry.byte <= glyph.byte)
            .saturating_sub(1);
        if raw[at].is_none() {
            raw[at] = Some(glyph.x);
        }
    }
    raw[0].get_or_insert(0.);
    raw[count] = Some(run.width);
    let mut stops = fill_gaps(&raw);
    let mut shifts = vec![0.; count + 1];
    let mut pushed = 0.;
    for at in 0..=count {
        shifts[at] = pushed;
        stops[at] += pushed;
        if let Some(ch) = chars.get(at) {
            pushed += letter_spacing;
            if matches!(ch, ' ' | '\u{A0}') {
                pushed += word_spacing;
            }
        }
    }
    (stops, shifts)
}

/// The stops that are known, with the ones between them (inside a cluster) spread evenly.
fn fill_gaps(raw: &[Option<f32>]) -> Vec<f32> {
    let mut out = vec![0.; raw.len()];
    let mut last: Option<(usize, f32)> = None;
    for (at, value) in raw.iter().enumerate() {
        let Some(value) = value else { continue };
        if let Some((from, start)) = last {
            let span = (at - from) as f32;
            for (between, slot) in out.iter_mut().enumerate().take(at).skip(from + 1) {
                *slot = start + (value - start) * (between - from) as f32 / span;
            }
        }
        out[at] = *value;
        last = Some((at, *value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measure::{FontKey, HeadlessMeasurer};

    fn processed(text: &str) -> Processed {
        Processed {
            text: text.to_owned(),
            map: text
                .char_indices()
                .map(|(byte, _)| CharMap {
                    byte: byte as u32,
                    source: byte as u32,
                })
                .collect(),
        }
    }

    #[test]
    fn stops_follow_the_glyphs_and_add_spacing() {
        let text = "ab c";
        let chars: Vec<char> = text.chars().collect();
        let run = HeadlessMeasurer.shape(text, FontKey(0), 10.);
        let (stops, shifts) = stops_of(&run, &processed(text), &chars, 1., 2.);
        // Half an em is 5; letter spacing 1 after each character, 2 more after the space.
        assert_eq!(stops, vec![0., 6., 12., 20., 26.]);
        assert_eq!(shifts, vec![0., 1., 2., 5., 6.]);
    }

    #[test]
    fn the_stops_inside_a_cluster_are_spread_evenly() {
        // One glyph for three characters, 30 wide.
        let text = "abc";
        let chars: Vec<char> = text.chars().collect();
        let run = ShapedRun {
            font: FontKey(0),
            size: 10.,
            glyphs: vec![crate::measure::PlacedGlyph {
                id: 1,
                x: 0.,
                byte: 0,
                is_emoji: false,
                font: FontKey(0),
            }]
            .into(),
            width: 30.,
        };
        let (stops, _) = stops_of(&run, &processed(text), &chars, 0., 0.);
        assert_eq!(stops, vec![0., 10., 20., 30.]);
    }
}
