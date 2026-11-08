//! White space and `text-transform`: what a text node says is turned into what is laid out.
//!
//! CSS Text 3 §4: in the modes that collapse, a run of spaces, tabs and segment breaks is one
//! space, and a space at the start of a line is dropped (the end of a line is the line builder's);
//! in the modes that preserve newlines, a newline is a forced break. Every character that comes
//! out remembers the byte of the text node it came from, so a selection in the layout can be
//! turned back into an offset in the node.

use super::breaks::{is_cjk, is_hangul};

/// CSS's `white-space`, reduced to the six values that tell layout apart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum WhiteSpace {
    Normal,
    Nowrap,
    Pre,
    PreWrap,
    PreLine,
    BreakSpaces,
}

impl WhiteSpace {
    pub(crate) fn parse(text: &str) -> Self {
        match text.trim() {
            "nowrap" => Self::Nowrap,
            "pre" => Self::Pre,
            "pre-wrap" => Self::PreWrap,
            "pre-line" => Self::PreLine,
            "break-spaces" => Self::BreakSpaces,
            _ => Self::Normal,
        }
    }

    /// Whether a run of spaces is one space.
    pub(crate) fn collapses(self) -> bool {
        matches!(self, Self::Normal | Self::Nowrap | Self::PreLine)
    }

    /// Whether a newline in the text ends the line.
    pub(crate) fn keeps_newlines(self) -> bool {
        !matches!(self, Self::Normal | Self::Nowrap)
    }

    /// Whether lines break at soft wrap opportunities.
    pub(crate) fn wraps(self) -> bool {
        !matches!(self, Self::Nowrap | Self::Pre)
    }
}

/// CSS's `text-transform`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Transform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

impl Transform {
    pub(crate) fn parse(text: &str) -> Self {
        match text.split_whitespace().next() {
            Some("uppercase") => Self::Uppercase,
            Some("lowercase") => Self::Lowercase,
            Some("capitalize") => Self::Capitalize,
            _ => Self::None,
        }
    }
}

/// Where in a line the text so far leaves off, carried from one text node to the next.
#[derive(Clone, Debug)]
pub(crate) struct Flow {
    /// Nothing but collapsed space has been put on the line.
    pub at_line_start: bool,
    /// The last thing put was a collapsible space.
    pub after_space: bool,
    /// The column a tab stop is counted from.
    pub column: usize,
    /// The last character was a letter, for `capitalize`.
    pub after_letter: bool,
    /// The last character put, for the rule of segment breaks between wide characters.
    pub last: Option<char>,
}

impl Flow {
    pub(crate) fn new() -> Self {
        Self {
            at_line_start: true,
            after_space: false,
            column: 0,
            after_letter: false,
            last: None,
        }
    }

    /// A forced break has been put: the next text starts a line.
    pub(crate) fn newline(&mut self) {
        self.at_line_start = true;
        self.after_space = false;
        self.column = 0;
        self.after_letter = false;
        self.last = None;
    }

    /// An atomic inline has been put.
    pub(crate) fn atomic(&mut self) {
        self.at_line_start = false;
        self.after_space = false;
        self.after_letter = false;
        self.last = Some('\u{FFFC}');
    }
}

/// One character of laid-out text and where it came from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct CharMap {
    /// Its first byte in the processed text.
    pub byte: u32,
    /// The byte of the text node it stands for.
    pub source: u32,
}

/// The text of one line of a text node, after white space and `text-transform`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Processed {
    pub text: String,
    pub map: Vec<CharMap>,
}

impl Processed {
    fn push(&mut self, ch: char, source: usize) {
        self.map.push(CharMap {
            byte: self.text.len() as u32,
            source: source as u32,
        });
        self.text.push(ch);
    }

    /// Takes off the last character if it is a space; answers whether it did.
    fn pop_space(&mut self) -> bool {
        if self.text.ends_with(' ') {
            self.text.pop();
            self.map.pop();
            true
        } else {
            false
        }
    }
}

/// A piece of a text node: text up to a preserved newline, or the newline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Segment {
    Text(Processed),
    /// A forced break; the byte is where the newline is in the node.
    Newline(u32),
}

/// What `text-transform` does to one character.
fn transformed(ch: char, transform: Transform, after_letter: bool, out: &mut Vec<char>) {
    match transform {
        Transform::None => out.push(ch),
        Transform::Uppercase => out.extend(ch.to_uppercase()),
        Transform::Lowercase => out.extend(ch.to_lowercase()),
        Transform::Capitalize if !after_letter && ch.is_alphanumeric() => {
            out.extend(ch.to_uppercase())
        }
        Transform::Capitalize => out.push(ch),
    }
}

/// How a text node's text is laid out in `mode`: spaces collapsed or kept, newlines made forced
/// breaks where the mode says, tabs widened, `transform` applied. `flow` carries what came before
/// in the same block, so a space after a space in the element before is dropped.
pub(crate) fn process(
    source: &str,
    mode: WhiteSpace,
    transform: Transform,
    tab_size: usize,
    flow: &mut Flow,
) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut line = Processed::default();
    let chars: Vec<(usize, char)> = source.char_indices().collect();
    let mut at = 0;
    let mut scratch: Vec<char> = Vec::new();
    while at < chars.len() {
        let (byte, ch) = chars[at];
        at += 1;
        let newline = ch == '\n' || ch == '\r';
        if ch == '\r' && chars.get(at).is_some_and(|(_, next)| *next == '\n') {
            at += 1;
        }
        if newline && mode.keeps_newlines() {
            if mode == WhiteSpace::PreLine && line.pop_space() {
                flow.after_space = false;
            }
            if !line.text.is_empty() {
                out.push(Segment::Text(std::mem::take(&mut line)));
            }
            out.push(Segment::Newline(byte as u32));
            flow.newline();
            continue;
        }
        if newline || ch == ' ' || ch == '\t' || ch == '\u{000C}' {
            flow.after_letter = false;
            if mode.collapses() {
                let drop = flow.after_space
                    || flow.at_line_start
                    || (newline && joins_wide(flow.last, &chars[at..]));
                if !drop {
                    line.push(' ', byte);
                    flow.after_space = true;
                }
            } else {
                let width = if ch == '\t' {
                    tab_size - flow.column % tab_size.max(1)
                } else {
                    1
                };
                for _ in 0..width {
                    line.push(' ', byte);
                }
                flow.column += width;
                flow.at_line_start = false;
                flow.last = Some(' ');
            }
            continue;
        }
        scratch.clear();
        transformed(ch, transform, flow.after_letter, &mut scratch);
        for made in &scratch {
            line.push(*made, byte);
        }
        flow.after_letter = ch.is_alphanumeric() || ch == '\'';
        flow.at_line_start = false;
        flow.after_space = false;
        flow.column += 1;
        flow.last = Some(ch);
    }
    if !line.text.is_empty() {
        out.push(Segment::Text(line));
    }
    out
}

/// A segment break between two wide characters (not Hangul) is no space at all.
fn joins_wide(before: Option<char>, rest: &[(usize, char)]) -> bool {
    let after = rest
        .iter()
        .map(|(_, ch)| *ch)
        .find(|ch| !matches!(ch, ' ' | '\t' | '\n' | '\r'));
    match (before, after) {
        (Some(a), Some(b)) => is_cjk(a) && is_cjk(b) && !is_hangul(a) && !is_hangul(b),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, mode: WhiteSpace) -> Vec<Segment> {
        process(text, mode, Transform::None, 8, &mut Flow::new())
    }

    fn texts(segments: &[Segment]) -> Vec<String> {
        segments
            .iter()
            .map(|segment| match segment {
                Segment::Text(p) => p.text.clone(),
                Segment::Newline(_) => "\\n".to_owned(),
            })
            .collect()
    }

    #[test]
    fn normal_collapses_runs_and_drops_the_space_at_the_start() {
        assert_eq!(
            texts(&run("  a \t b\n\n c  ", WhiteSpace::Normal)),
            vec!["a b c "]
        );
    }

    #[test]
    fn nowrap_collapses_like_normal() {
        assert_eq!(texts(&run("a   b", WhiteSpace::Nowrap)), vec!["a b"]);
    }

    #[test]
    fn pre_keeps_everything_and_breaks_at_newlines() {
        assert_eq!(
            texts(&run("a  b\n c", WhiteSpace::Pre)),
            vec!["a  b", "\\n", " c"]
        );
    }

    #[test]
    fn pre_widens_tabs_to_the_next_stop() {
        assert_eq!(texts(&run("ab\tc", WhiteSpace::Pre)), vec!["ab      c"]);
    }

    #[test]
    fn pre_wrap_keeps_spaces_too() {
        assert_eq!(texts(&run("a  b", WhiteSpace::PreWrap)), vec!["a  b"]);
    }

    #[test]
    fn pre_line_collapses_spaces_but_keeps_newlines() {
        assert_eq!(
            texts(&run("a  b \n  c", WhiteSpace::PreLine)),
            vec!["a b", "\\n", "c"]
        );
    }

    #[test]
    fn break_spaces_keeps_spaces_and_newlines() {
        assert_eq!(
            texts(&run("a  \nb", WhiteSpace::BreakSpaces)),
            vec!["a  ", "\\n", "b"]
        );
    }

    #[test]
    fn a_space_after_a_space_in_the_text_before_is_dropped() {
        let mut flow = Flow::new();
        let first = process("a ", WhiteSpace::Normal, Transform::None, 8, &mut flow);
        let second = process(" b", WhiteSpace::Normal, Transform::None, 8, &mut flow);
        assert_eq!(texts(&first), vec!["a "]);
        assert_eq!(texts(&second), vec!["b"]);
    }

    #[test]
    fn a_newline_between_wide_characters_is_no_space() {
        assert_eq!(
            texts(&run("中文\n中文", WhiteSpace::Normal)),
            vec!["中文中文"]
        );
        assert_eq!(texts(&run("a\n中", WhiteSpace::Normal)), vec!["a 中"]);
        assert_eq!(texts(&run("한\n글", WhiteSpace::Normal)), vec!["한 글"]);
    }

    #[test]
    fn characters_remember_the_byte_they_came_from() {
        let segments = run("a  é", WhiteSpace::Normal);
        let [Segment::Text(text)] = segments.as_slice() else {
            panic!("one segment");
        };
        assert_eq!(text.text, "a é");
        let sources: Vec<u32> = text.map.iter().map(|c| c.source).collect();
        assert_eq!(sources, vec![0, 1, 3]);
        let bytes: Vec<u32> = text.map.iter().map(|c| c.byte).collect();
        assert_eq!(bytes, vec![0, 1, 2]);
    }

    #[test]
    fn text_transform_changes_case_and_capitalizes_words() {
        let upper = process(
            "straße",
            WhiteSpace::Normal,
            Transform::Uppercase,
            8,
            &mut Flow::new(),
        );
        assert_eq!(texts(&upper), vec!["STRASSE"]);
        let words = process(
            "hello big world",
            WhiteSpace::Normal,
            Transform::Capitalize,
            8,
            &mut Flow::new(),
        );
        assert_eq!(texts(&words), vec!["Hello Big World"]);
        let lower = process(
            "ABC",
            WhiteSpace::Normal,
            Transform::Lowercase,
            8,
            &mut Flow::new(),
        );
        assert_eq!(texts(&lower), vec!["abc"]);
    }

    #[test]
    fn capitalize_goes_on_across_text_nodes() {
        let mut flow = Flow::new();
        let first = process(
            "wor",
            WhiteSpace::Normal,
            Transform::Capitalize,
            8,
            &mut flow,
        );
        let second = process(
            "ld",
            WhiteSpace::Normal,
            Transform::Capitalize,
            8,
            &mut flow,
        );
        assert_eq!(texts(&first), vec!["Wor"]);
        assert_eq!(texts(&second), vec!["ld"]);
    }
}
