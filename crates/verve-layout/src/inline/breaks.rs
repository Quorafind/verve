//! Where a line may break: UAX #14's opportunities, the properties that add to them or take them
//! away (`word-break`, `overflow-wrap`), and the character classes the rules lean on.

use unicode_linebreak::linebreaks;

/// CSS's `word-break`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum WordBreak {
    Normal,
    /// A break is allowed between any two letters.
    BreakAll,
    /// No break between characters of Chinese, Japanese or Korean text.
    KeepAll,
}

impl WordBreak {
    pub(crate) fn parse(text: &str) -> Self {
        match text.trim() {
            "break-all" => Self::BreakAll,
            "keep-all" => Self::KeepAll,
            _ => Self::Normal,
        }
    }
}

/// CSS's `overflow-wrap` (and its old name `word-wrap`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum OverflowWrap {
    Normal,
    /// A word too long for a line breaks inside, and that does not shorten its minimum width.
    BreakWord,
    /// A word breaks anywhere when it has to, and its minimum width is one character.
    Anywhere,
}

impl OverflowWrap {
    pub(crate) fn parse(text: &str) -> Self {
        match text.trim() {
            "break-word" => Self::BreakWord,
            "anywhere" => Self::Anywhere,
            _ => Self::Normal,
        }
    }

    /// Whether a word that does not fit a line on its own may be broken inside.
    pub(crate) fn breaks_words(self) -> bool {
        self != Self::Normal
    }
}

/// The byte positions of `text` where UAX #14 allows a break (before the character that starts
/// there), ascending, without the end of the text. A break the standard demands is in the list
/// too.
pub(crate) fn opportunities(text: &str) -> Vec<usize> {
    linebreaks(text)
        .map(|(at, _)| at)
        .filter(|at| *at > 0 && *at < text.len())
        .collect()
}

/// Han, kana, CJK punctuation, fullwidth forms and Hangul: text that breaks between characters.
pub(crate) fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x11FF
            | 0x2E80..=0x2FDF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3100..=0x312F
            | 0x3130..=0x318F
            | 0x31A0..=0x31FF
            | 0x3200..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0x20000..=0x2FFFF
            | 0x30000..=0x3FFFF
    )
}

pub(crate) fn is_hangul(ch: char) -> bool {
    matches!(ch as u32, 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF)
}

/// A character that is part of the one before it: a combining mark, a joiner, a variation
/// selector, an emoji modifier. A break never comes before it.
pub(crate) fn is_combining(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200C
            | 0x200D
            | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF
            | 0xE0100..=0xE01EF
    )
}

/// Whether a break may be put between `before` and `after` by `word-break: break-all`: between
/// any two letters, numbers and ideographs.
pub(crate) fn splits_letters(before: char, after: char) -> bool {
    let word = |ch: char| !ch.is_whitespace() && !is_combining(ch) && ch != '\u{FFFC}';
    word(before) && word(after) && !is_closing_punctuation(after) && !is_opening_punctuation(before)
}

/// Punctuation that must not start a line.
fn is_closing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ','
            | ';'
            | ':'
            | '!'
            | '?'
            | ')'
            | ']'
            | '}'
            | '。'
            | '，'
            | '、'
            | '）'
            | '」'
            | '』'
            | '！'
            | '？'
            | '；'
            | '：'
    )
}

/// Punctuation that must not end a line.
fn is_opening_punctuation(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '（' | '「' | '『' | '【')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_break_follows_each_space() {
        assert_eq!(opportunities("hello big world"), vec![6, 10]);
        assert!(opportunities("unbreakable").is_empty());
    }

    #[test]
    fn a_break_is_allowed_between_ideographs() {
        assert_eq!(opportunities("中文字"), vec![3, 6]);
    }

    #[test]
    fn closing_punctuation_does_not_start_a_line() {
        // `。` is a closing punctuation mark: no break before it.
        assert_eq!(opportunities("中。文"), vec![6]);
    }

    #[test]
    fn a_hyphen_lets_the_line_break_after_it() {
        assert_eq!(opportunities("well-known"), vec![5]);
    }

    #[test]
    fn a_no_break_space_does_not_break() {
        assert!(opportunities("a\u{A0}b").is_empty());
    }

    #[test]
    fn an_object_replacement_character_breaks_on_both_sides() {
        assert_eq!(opportunities("a\u{FFFC}b"), vec![1, 4]);
    }

    #[test]
    fn the_character_classes() {
        assert!(is_cjk('中') && is_cjk('あ') && is_cjk('한') && is_cjk('，'));
        assert!(!is_cjk('a') && !is_cjk('é'));
        assert!(is_hangul('한') && !is_hangul('中'));
        assert!(is_combining('\u{0301}') && is_combining('\u{200D}'));
        assert!(!is_combining('a'));
    }

    #[test]
    fn break_all_splits_letters_but_not_before_closing_marks() {
        assert!(splits_letters('a', 'b'));
        assert!(!splits_letters('a', ' '));
        assert!(!splits_letters(' ', 'a'));
        assert!(!splits_letters('a', '.'));
        assert!(!splits_letters('(', 'a'));
    }

    #[test]
    fn the_keywords_parse() {
        assert_eq!(WordBreak::parse("break-all"), WordBreak::BreakAll);
        assert_eq!(WordBreak::parse("keep-all"), WordBreak::KeepAll);
        assert_eq!(WordBreak::parse("normal"), WordBreak::Normal);
        assert_eq!(OverflowWrap::parse("anywhere"), OverflowWrap::Anywhere);
        assert!(OverflowWrap::parse("break-word").breaks_words());
        assert!(!OverflowWrap::parse("normal").breaks_words());
    }
}
