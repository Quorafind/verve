//! Cutting the content into chunks: the pieces between the places a line may break.
//!
//! The content is read as one string (text as it is, an atomic inline as U+FFFC, a `<br>` as a
//! newline, a `<wbr>` as a zero width space), UAX #14 says where a line may break in it, and
//! `word-break` and `white-space` add to those places and take from them. A chunk is what lies
//! between two places: the line builder puts it on a line whole, or starts a new line for it.

use super::breaks::{WordBreak, opportunities, splits_letters};
use super::ifc::{Item, Span, TextItem};
use super::space::WhiteSpace;
use super::style::TextStyle;

/// A piece of a chunk.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Piece {
    /// Characters `a..b` of the text item with this index.
    Text {
        item: usize,
        a: usize,
        b: usize,
    },
    Open(usize),
    Close(usize),
    Atomic(usize),
    Break,
}

/// What lies between two places a line may break.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Chunk {
    pub pieces: Vec<Piece>,
    /// A forced break ends the chunk.
    pub forced: bool,
}

/// The character before a place, and what decides whether a line may break after it.
#[derive(Copy, Clone)]
struct Before {
    ch: char,
    wraps: bool,
    word_break: WordBreak,
    keeps_spaces: bool,
}

/// What reading the content has come to.
struct Builder<'a> {
    /// The elements of the content, and the style of the block that holds it.
    spans: &'a [Span],
    root: &'a TextStyle,
    /// The elements open now, innermost last.
    open: Vec<usize>,
    out: Vec<Chunk>,
    current: Chunk,
    /// Starts of elements that wait for the next character to say which chunk they open.
    pending: Vec<Piece>,
    /// Where the string read so far ends, and which opportunity is next.
    at: usize,
    next: usize,
    before: Option<Before>,
    /// A `<wbr>` was just read.
    word_break: bool,
    opportunities: Vec<usize>,
}

/// The chunks of `items`.
pub(crate) fn build(items: &[Item], spans: &[Span], root: &TextStyle) -> Vec<Chunk> {
    let mut text = String::new();
    for item in items {
        match item {
            Item::Text(t) => text.push_str(&t.processed.text),
            Item::Atomic(_) => text.push('\u{FFFC}'),
            Item::Break => text.push('\n'),
            Item::WordBreak => text.push('\u{200B}'),
            Item::Open(_) | Item::Close(_) => {}
        }
    }
    let mut builder = Builder {
        spans,
        root,
        open: Vec::new(),
        out: Vec::new(),
        current: Chunk::default(),
        pending: Vec::new(),
        at: 0,
        next: 0,
        before: None,
        word_break: false,
        opportunities: opportunities(&text),
    };
    for (index, item) in items.iter().enumerate() {
        builder.read(index, item);
    }
    builder.finish()
}

impl Builder<'_> {
    fn read(&mut self, index: usize, item: &Item) {
        match item {
            Item::Open(span) => {
                self.open.push(*span);
                self.pending.push(Piece::Open(*span));
            }
            Item::Close(span) => {
                self.open.pop();
                let pending = std::mem::take(&mut self.pending);
                self.current.pieces.extend(pending);
                self.current.pieces.push(Piece::Close(*span));
            }
            Item::Break => {
                let pending = std::mem::take(&mut self.pending);
                self.current.pieces.extend(pending);
                self.current.pieces.push(Piece::Break);
                self.current.forced = true;
                self.out.push(std::mem::take(&mut self.current));
                self.at += 1;
                self.before = None;
            }
            Item::WordBreak => {
                self.word_break = true;
                self.at += '\u{200B}'.len_utf8();
            }
            Item::Atomic(atom) => {
                let breaks = self.boundary('\u{FFFC}');
                self.start(breaks, Piece::Atomic(*atom));
                self.at += '\u{FFFC}'.len_utf8();
                let holder = self
                    .open
                    .last()
                    .map_or(self.root, |span| &*self.spans[*span].style);
                self.before = Some(Before {
                    ch: '\u{FFFC}',
                    wraps: holder.white_space.wraps(),
                    word_break: holder.word_break,
                    keeps_spaces: false,
                });
            }
            Item::Text(text) => self.text(index, text),
        }
    }

    fn text(&mut self, index: usize, text: &TextItem) {
        let style = &text.style;
        for (at, ch) in text.chars.iter().enumerate() {
            let breaks = self.boundary(*ch);
            let piece = Piece::Text {
                item: index,
                a: at,
                b: at + 1,
            };
            if !breaks && self.extend_text(index, at) {
                self.at += ch.len_utf8();
                self.note(*ch, style.white_space, style.word_break);
                continue;
            }
            self.start(breaks, piece);
            self.at += ch.len_utf8();
            self.note(*ch, style.white_space, style.word_break);
        }
    }

    /// Remembers the character just read as the one before the next place.
    fn note(&mut self, ch: char, white_space: WhiteSpace, word_break: WordBreak) {
        self.before = Some(Before {
            ch,
            wraps: white_space.wraps(),
            word_break,
            keeps_spaces: white_space == WhiteSpace::BreakSpaces,
        });
    }

    /// Whether a line may break before `ch`, the next character of the content.
    fn boundary(&mut self, ch: char) -> bool {
        while self
            .opportunities
            .get(self.next)
            .is_some_and(|place| *place < self.at)
        {
            self.next += 1;
        }
        let mut allowed = self.opportunities.get(self.next) == Some(&self.at);
        let wbr = std::mem::take(&mut self.word_break);
        let Some(before) = self.before else {
            return false;
        };
        if wbr && !ch.is_whitespace() {
            allowed = true;
        }
        if before.word_break == WordBreak::BreakAll && splits_letters(before.ch, ch) {
            allowed = true;
        }
        if before.word_break == WordBreak::KeepAll
            && before.ch.is_alphanumeric()
            && ch.is_alphanumeric()
        {
            allowed = false;
        }
        if before.keeps_spaces && before.ch == ' ' {
            allowed = true;
        }
        allowed && before.wraps
    }

    /// Adds the character to the text piece before it when that piece is of the same item and
    /// ends where this character starts. Answers whether it did.
    fn extend_text(&mut self, index: usize, at: usize) -> bool {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            self.current.pieces.extend(pending);
        }
        match self.current.pieces.last_mut() {
            Some(Piece::Text { item, b, .. }) if *item == index && *b == at => {
                *b = at + 1;
                true
            }
            _ => false,
        }
    }

    /// Puts `piece` at the start of a new chunk when `breaks`, and in the current one otherwise.
    fn start(&mut self, breaks: bool, piece: Piece) {
        if breaks && !self.current.pieces.is_empty() {
            self.out.push(std::mem::take(&mut self.current));
        }
        let pending = std::mem::take(&mut self.pending);
        self.current.pieces.extend(pending);
        self.current.pieces.push(piece);
    }

    fn finish(mut self) -> Vec<Chunk> {
        let pending = std::mem::take(&mut self.pending);
        self.current.pieces.extend(pending);
        if !self.current.pieces.is_empty() {
            self.out.push(self.current);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inline::ifc::TextItem;
    use crate::inline::space::{Flow, Segment, Transform, process};
    use crate::inline::style::{LineHeight, TextAlign, TextPaint, TextStyle, VerticalAlign};
    use crate::measure::{FontKey, HeadlessMeasurer, TextMeasurer};
    use std::sync::Arc;

    fn style(white_space: WhiteSpace, word_break: WordBreak) -> Arc<TextStyle> {
        Arc::new(TextStyle {
            size: 10.,
            font: FontKey(0),
            metrics: HeadlessMeasurer.metrics(FontKey(0), 10.),
            line_height: LineHeight::Normal,
            white_space,
            word_break,
            overflow_wrap: crate::inline::breaks::OverflowWrap::Normal,
            align: TextAlign::Start,
            indent: crate::value::Dim::Px(0.),
            transform: Transform::None,
            letter_spacing: 0.,
            word_spacing: 0.,
            vertical_align: VerticalAlign::Baseline,
            color: [0., 0., 0., 1.],
            rtl: false,
            tab_size: 8,
            ellipsis: false,
            line_clamp: None,
            decorations: Vec::new(),
            shadow: None,
            select_none: false,
        })
    }

    fn text_item(text: &str, style: &Arc<TextStyle>, flow: &mut Flow) -> Vec<Item> {
        process(text, style.white_space, Transform::None, 8, flow)
            .into_iter()
            .map(|segment| match segment {
                Segment::Newline(_) => Item::Break,
                Segment::Text(processed) => {
                    let run = HeadlessMeasurer.shape(&processed.text, style.font, style.size);
                    let chars: Vec<char> = processed.text.chars().collect();
                    let (stops, shifts) =
                        crate::inline::ifc::stops_of(&run, &processed, &chars, 0., 0.);
                    Item::Text(TextItem {
                        node: 1,
                        generated: false,
                        span: None,
                        style: style.clone(),
                        paint: Arc::new(TextPaint {
                            color: [0., 0., 0., 1.],
                            decorations: Vec::new(),
                            shadow: None,
                        }),
                        processed,
                        chars,
                        order: crate::inline::ifc::glyph_order(&run),
                        run,
                        stops,
                        shifts,
                        source_len: text.len() as u32,
                    })
                }
            })
            .collect()
    }

    fn texts(items: &[Item], chunks: &[Chunk]) -> Vec<String> {
        chunks
            .iter()
            .map(|chunk| {
                chunk
                    .pieces
                    .iter()
                    .map(|piece| match piece {
                        Piece::Text { item, a, b } => {
                            let Item::Text(text) = &items[*item] else {
                                return String::new();
                            };
                            text.chars[*a..*b].iter().collect()
                        }
                        Piece::Break => "<br>".to_owned(),
                        Piece::Atomic(_) => "[]".to_owned(),
                        Piece::Open(_) => "<".to_owned(),
                        Piece::Close(_) => ">".to_owned(),
                    })
                    .collect::<String>()
            })
            .collect()
    }

    fn chunked(text: &str, white_space: WhiteSpace, word_break: WordBreak) -> Vec<String> {
        let style = style(white_space, word_break);
        let items = text_item(text, &style, &mut Flow::new());
        let chunks = build(&items, &[], &style);
        texts(&items, &chunks)
    }

    #[test]
    fn a_chunk_is_a_word_with_the_space_after_it() {
        assert_eq!(
            chunked("one two  three", WhiteSpace::Normal, WordBreak::Normal),
            vec!["one ", "two ", "three"]
        );
    }

    #[test]
    fn nowrap_is_one_chunk() {
        assert_eq!(
            chunked("one two three", WhiteSpace::Nowrap, WordBreak::Normal),
            vec!["one two three"]
        );
    }

    #[test]
    fn pre_breaks_only_at_newlines() {
        assert_eq!(
            chunked("a b\nc d", WhiteSpace::Pre, WordBreak::Normal),
            vec!["a b<br>", "c d"]
        );
    }

    #[test]
    fn break_all_cuts_between_letters() {
        assert_eq!(
            chunked("abc de", WhiteSpace::Normal, WordBreak::BreakAll),
            vec!["a", "b", "c ", "d", "e"]
        );
    }

    #[test]
    fn keep_all_joins_cjk_text() {
        assert_eq!(
            chunked("中文 字", WhiteSpace::Normal, WordBreak::KeepAll),
            vec!["中文 ", "字"]
        );
        assert_eq!(
            chunked("中文字", WhiteSpace::Normal, WordBreak::Normal),
            vec!["中", "文", "字"]
        );
    }

    #[test]
    fn break_spaces_lets_every_space_be_a_place() {
        assert_eq!(
            chunked("a  b", WhiteSpace::BreakSpaces, WordBreak::Normal),
            vec!["a ", " ", "b"]
        );
    }

    #[test]
    fn an_element_boundary_without_a_space_is_no_place() {
        let plain = style(WhiteSpace::Normal, WordBreak::Normal);
        let mut flow = Flow::new();
        let mut items = text_item("foo", &plain, &mut flow);
        items.push(Item::Open(0));
        items.extend(text_item("bar baz", &plain, &mut flow));
        items.push(Item::Close(0));
        let chunks = build(&items, &[], &plain);
        assert_eq!(texts(&items, &chunks), vec!["foo<bar ", "baz>"]);
    }

    #[test]
    fn a_closing_boundary_stays_with_the_text_before_it() {
        let plain = style(WhiteSpace::Normal, WordBreak::Normal);
        let mut flow = Flow::new();
        let mut items = vec![Item::Open(0)];
        items.extend(text_item("foo ", &plain, &mut flow));
        items.push(Item::Close(0));
        items.extend(text_item("bar", &plain, &mut flow));
        let chunks = build(&items, &[], &plain);
        assert_eq!(texts(&items, &chunks), vec!["<foo >", "bar"]);
    }

    #[test]
    fn an_atomic_inline_is_a_chunk_of_its_own() {
        let plain = style(WhiteSpace::Normal, WordBreak::Normal);
        let mut flow = Flow::new();
        let mut items = text_item("a", &plain, &mut flow);
        items.push(Item::Atomic(0));
        items.extend(text_item("b", &plain, &mut flow));
        let chunks = build(&items, &[], &plain);
        assert_eq!(texts(&items, &chunks), vec!["a", "[]", "b"]);
    }

    #[test]
    fn a_word_break_opportunity_cuts_a_word() {
        let plain = style(WhiteSpace::Normal, WordBreak::Normal);
        let mut flow = Flow::new();
        let mut items = text_item("ab", &plain, &mut flow);
        items.push(Item::WordBreak);
        items.extend(text_item("cd", &plain, &mut flow));
        let chunks = build(&items, &[], &plain);
        assert_eq!(texts(&items, &chunks), vec!["ab", "cd"]);
    }

    #[test]
    fn atomic_inlines_break_only_where_the_block_wraps() {
        for (mode, want) in [
            (WhiteSpace::Normal, vec!["[]", "[]", "a"]),
            (WhiteSpace::Nowrap, vec!["[][]a"]),
            (WhiteSpace::Pre, vec!["[][]a"]),
        ] {
            let style = style(mode, WordBreak::Normal);
            let mut items = vec![Item::Atomic(0), Item::Atomic(1)];
            items.extend(text_item("a", &style, &mut Flow::new()));
            let chunks = build(&items, &[], &style);
            let got = texts(&items, &chunks);
            assert_eq!(got.len(), want.len(), "{mode:?}: {got:?}");
            assert_eq!(got.concat(), want.concat());
        }
    }
}
