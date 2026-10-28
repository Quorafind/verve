//! What is typed into an `<input>` or a `<textarea>`: the text, the caret standing in it, and the
//! selection it can drag out — lifted from su-editor's own fields (`su_editor_api::field`).
//!
//! The selection is one range spelled as two offsets — the caret, and the anchor the caret was
//! dragged away from — so a shift-arrow knows which end it moves. Every offset is a byte offset
//! on a grapheme boundary; what JavaScript sees (`selectionStart`, `setSelectionRange`) is in
//! UTF-16, converted at the edge.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// A name being typed, the caret typing it, and what is selected around it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    text: String,
    /// Byte offset into `text`, on a grapheme boundary, never past its end.
    caret: usize,
    /// The other end of the selection, on the same terms. Equal to `caret` when nothing is
    /// selected, which is every field nobody has dragged across or told to select all.
    anchor: usize,
    /// Where an input method is composing, while one is: the preview spelling it has put in
    /// the text, which its next update replaces and its commit writes over.
    ///
    /// Kept here because the platform does not say where: an update arrives as "replace what
    /// you are marking", with no range at all. A field that did not remember would insert
    /// every intermediate spelling instead of replacing the one before it — typing 加载 into
    /// the search box left `jjiajia'zjia'zai加载` standing in it.
    marked: Option<Range<usize>>,
}

impl Draft {
    /// A field opened over a name that already exists, with the caret at the end of it — where
    /// a writer who means to add to it would put it, and one press of Home from the other end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            caret: text.len(),
            anchor: text.len(),
            text,
            marked: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// What is selected, as a range into the text. Empty when nothing is.
    pub fn selection(&self) -> Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    /// Whether the caret is at the near end of the selection — dragged leftward. The platform
    /// asks, because it is what tells an input method which end it is extending.
    pub fn is_reversed(&self) -> bool {
        self.caret < self.anchor
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    /// What is drawn either side of the caret. Both halves can be empty, and a field with
    /// nothing in it is two empty ones.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.caret)
    }

    pub fn into_text(self) -> String {
        self.text
    }

    /// Puts the caret where a click landed, with nothing selected. `at` is clamped onto the
    /// nearest boundary at or before it, so a hit test that answered mid-character cannot leave
    /// an offset every later operation would panic on.
    pub fn place(&mut self, at: usize) {
        self.marked = None;
        self.caret = self.boundary(at);
        self.anchor = self.caret;
    }

    /// Drags the caret to `at` and leaves the anchor where it is: a selection being pulled out
    /// by the pointer, or by a shift-arrow.
    pub fn extend_to(&mut self, at: usize) {
        self.marked = None;
        self.caret = self.boundary(at);
    }

    /// Selects the word `at` falls in — a double click. A click on a run of spaces takes the
    /// spaces, so every position in the field selects something.
    pub fn select_word_at(&mut self, at: usize) {
        self.marked = None;
        let at = self.boundary(at);
        self.anchor = self.word_start_before(at);
        self.caret = self.word_end_after(at);
    }

    /// Ctrl+A. Nothing is selected when there is nothing there: an empty field would otherwise
    /// show a selection with no text inside it.
    pub fn select_all(&mut self) -> bool {
        self.marked = None;
        if self.text.is_empty() {
            return false;
        }
        self.anchor = 0;
        self.caret = self.text.len();
        true
    }

    /// Takes committed text at the caret — a keystroke, or what an IME finished composing, or a
    /// paste. A selection is what it replaces.
    pub fn insert(&mut self, text: &str) {
        self.take_selection();
        if text.is_empty() {
            return;
        }
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
        self.anchor = self.caret;
    }

    /// Takes the selection away, if there is one. Answers whether there was.
    pub fn take_selection(&mut self) -> bool {
        self.marked = None;
        let selection = self.selection();
        if selection.is_empty() {
            return false;
        }
        self.text.replace_range(selection.clone(), "");
        self.caret = selection.start;
        self.anchor = self.caret;
        true
    }

    /// What an input method is composing here, when one is.
    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// A composition's next spelling: `text` goes over `range`, and stands there as the
    /// composition until the one after it. `selected_in_replacement` is where the caret sits
    /// inside what was just written, in UTF-16, which is how the platform counts.
    ///
    /// A `range` of `None` means "over what you are marking", and with nothing marked, "over
    /// the selection" — the same reading the document gives it.
    pub fn replace_and_mark_utf16(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected_in_replacement: Option<Range<usize>>,
    ) {
        let range = self.target(range);
        let start = range.start;
        self.text.replace_range(range, text);
        self.marked = (!text.is_empty()).then_some(start..start + text.len());
        let selected = selected_in_replacement
            .map(|range| utf16_range_to_utf8(text, range))
            .unwrap_or(text.len()..text.len());
        self.anchor = start + selected.start;
        self.caret = start + selected.end;
    }

    /// The composition, finished: what the input method settled on goes over what it was
    /// composing, and nothing is being composed any more.
    pub fn replace_utf16(&mut self, range: Option<Range<usize>>, text: &str) {
        let range = self.target(range);
        self.text.replace_range(range.clone(), text);
        self.caret = range.start + text.len();
        self.anchor = self.caret;
        self.marked = None;
    }

    /// The composition, abandoned. The preview stays — that is what every platform means by
    /// unmarking — but nothing is standing over it any more.
    pub fn unmark(&mut self) {
        self.marked = None;
    }

    /// Where a replacement the platform did not place goes.
    fn target(&self, range: Option<Range<usize>>) -> Range<usize> {
        let range = range
            .map(|range| self.range_from_utf16(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let start = range.start.min(self.text.len());
        let end = range.end.clamp(start, self.text.len());
        start..end
    }

    pub fn offset_to_utf16(&self, offset: usize) -> usize {
        utf8_offset_to_utf16(&self.text, offset)
    }

    pub fn offset_from_utf16(&self, offset: usize) -> usize {
        utf16_offset_to_utf8(&self.text, offset)
    }

    pub fn range_to_utf16(&self, range: Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    pub fn range_from_utf16(&self, range: Range<usize>) -> Range<usize> {
        utf16_range_to_utf8(&self.text, range)
    }

    /// Backspace: the selection, or the grapheme before the caret, or nothing when it is
    /// standing at the start of a field with nothing selected.
    pub fn backspace(&mut self) -> bool {
        self.marked = None;
        if self.take_selection() {
            return true;
        }
        let start = self.before(self.caret);
        if start == self.caret {
            return false;
        }
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.anchor = start;
        true
    }

    /// Delete: the selection, or the grapheme after the caret, which leaves the caret where it
    /// is.
    pub fn delete(&mut self) -> bool {
        self.marked = None;
        if self.take_selection() {
            return true;
        }
        let end = self.after(self.caret);
        if end == self.caret {
            return false;
        }
        self.text.replace_range(self.caret..end, "");
        true
    }

    /// Ctrl+Backspace: the word before the caret, or the selection when there is one.
    pub fn backspace_word(&mut self) -> bool {
        self.marked = None;
        if self.take_selection() {
            return true;
        }
        let start = self.word_start_before(self.caret);
        if start == self.caret {
            return false;
        }
        self.text.replace_range(start..self.caret, "");
        self.caret = start;
        self.anchor = start;
        true
    }

    /// The motions this field has. Each answers whether anything actually changed, so a press at
    /// either end is not a frame redrawn for nothing.
    ///
    /// A selection collapses to the end the motion points at rather than moving past it: ← with
    /// three letters selected puts the caret in front of them, which is where every other field
    /// in every other program puts it.
    pub fn left(&mut self) -> bool {
        if self.collapse_to(self.selection().start) {
            return true;
        }
        self.jump_to(self.before(self.caret))
    }

    pub fn right(&mut self) -> bool {
        if self.collapse_to(self.selection().end) {
            return true;
        }
        self.jump_to(self.after(self.caret))
    }

    pub fn home(&mut self) -> bool {
        self.jump_to(0)
    }

    pub fn end(&mut self) -> bool {
        self.jump_to(self.text.len())
    }

    pub fn word_left(&mut self) -> bool {
        let target = self.word_start_before(self.selection().start);
        self.jump_to(target)
    }

    pub fn word_right(&mut self) -> bool {
        let target = self.word_end_after(self.selection().end);
        self.jump_to(target)
    }

    /// The same four with Shift held: the anchor stays and the selection grows or shrinks.
    pub fn extend_left(&mut self) -> bool {
        self.drag_to(self.before(self.caret))
    }

    pub fn extend_right(&mut self) -> bool {
        self.drag_to(self.after(self.caret))
    }

    pub fn extend_home(&mut self) -> bool {
        self.drag_to(0)
    }

    pub fn extend_end(&mut self) -> bool {
        self.drag_to(self.text.len())
    }

    pub fn extend_word_left(&mut self) -> bool {
        self.drag_to(self.word_start_before(self.caret))
    }

    pub fn extend_word_right(&mut self) -> bool {
        self.drag_to(self.word_end_after(self.caret))
    }

    /// Collapses a selection onto one of its ends. Answers false when there was none, which is
    /// the motion's cue to actually move.
    fn collapse_to(&mut self, at: usize) -> bool {
        self.marked = None;
        if !self.has_selection() {
            return false;
        }
        self.caret = at;
        self.anchor = at;
        true
    }

    /// Moves the caret and drops any selection with it.
    fn jump_to(&mut self, at: usize) -> bool {
        self.marked = None;
        let moved = self.caret != at || self.has_selection();
        self.caret = at;
        self.anchor = at;
        moved
    }

    /// Moves the caret and leaves the anchor, which is what makes a selection.
    fn drag_to(&mut self, at: usize) -> bool {
        self.marked = None;
        std::mem::replace(&mut self.caret, at) != at
    }

    /// The nearest grapheme boundary at or before `at`, and never past the end.
    fn boundary(&self, at: usize) -> usize {
        let at = at.min(self.text.len());
        if at == self.text.len() {
            return at;
        }
        self.text
            .grapheme_indices(true)
            .map(|(start, grapheme)| (start, start + grapheme.len()))
            .find(|(start, end)| at >= *start && at < *end)
            .map_or(at, |(start, _)| start)
    }

    fn before(&self, at: usize) -> usize {
        self.text[..at]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(start, _)| start)
    }

    fn after(&self, at: usize) -> usize {
        self.text[at..]
            .graphemes(true)
            .next()
            .map_or(at, |grapheme| at + grapheme.len())
    }

    /// Where the word before `at` begins: back over any spaces, then back over the run they
    /// were behind. A run is letters and digits together, or a stretch of everything else —
    /// which is what makes `example.com` three steps rather than one, so a host in a URL can be
    /// corrected without retyping the path.
    fn word_start_before(&self, at: usize) -> usize {
        let before = &self.text[..at];
        let trimmed = before.trim_end_matches(char::is_whitespace);
        let Some(last) = trimmed.chars().next_back() else {
            return 0;
        };
        let word = last.is_alphanumeric();
        trimmed
            .char_indices()
            .rev()
            .take_while(|(_, character)| character.is_alphanumeric() == word)
            .last()
            .map_or(0, |(start, _)| start)
    }

    /// Where the word after `at` ends: over the run under the caret, then over the spaces
    /// behind it.
    fn word_end_after(&self, at: usize) -> usize {
        let after = &self.text[at..];
        let Some(first) = after.chars().next() else {
            return at;
        };
        let run: usize = if first.is_whitespace() {
            0
        } else {
            after
                .chars()
                .take_while(|character| {
                    !character.is_whitespace()
                        && character.is_alphanumeric() == first.is_alphanumeric()
                })
                .map(char::len_utf8)
                .sum()
        };
        let spaces: usize = after[run..]
            .chars()
            .take_while(|character| character.is_whitespace())
            .map(char::len_utf8)
            .sum();
        at + run + spaces
    }
}

impl From<&str> for Draft {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Draft {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// UTF-8 byte offset to UTF-16 code unit offset, clamped onto the text and onto a boundary.
fn utf8_offset_to_utf16(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[..offset].encode_utf16().count()
}

/// The way back. An offset past the end lands on the end.
fn utf16_offset_to_utf8(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (index, character) in text.char_indices() {
        if utf16 >= offset {
            return index;
        }
        let next = utf16 + character.len_utf16();
        if offset < next {
            return index;
        }
        utf16 = next;
    }
    text.len()
}

fn utf16_range_to_utf8(text: &str, range: Range<usize>) -> Range<usize> {
    let start = utf16_offset_to_utf8(text, range.start);
    start..utf16_offset_to_utf8(text, range.end).max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_field_opens_with_the_caret_after_the_name_it_was_given() {
        let draft = Draft::new("笔记.md");
        assert_eq!(draft.caret(), "笔记.md".len());
        assert_eq!(draft.split(), ("笔记.md", ""));
        assert!(!draft.has_selection());
        assert!(Draft::default().is_empty());
    }

    /// The complaint this type exists for: a name typed with a mistake in the middle of it,
    /// corrected where the mistake is rather than by retyping the tail.
    #[test]
    fn the_caret_walks_into_the_name_and_edits_there() {
        let mut draft = Draft::new("笔计.md");
        for _ in 0..3 {
            assert!(draft.left());
        }
        assert!(draft.backspace());
        draft.insert("记");
        assert_eq!(draft.text(), "笔记.md");
        // And the caret is still standing where the correction was made, not at the end.
        assert_eq!(draft.split(), ("笔记", ".md"));
    }

    /// Each step is a whole character. A caret that moved by bytes would land inside 记 and
    /// every string operation after it would panic.
    #[test]
    fn a_step_is_a_grapheme_however_many_bytes_that_is() {
        let mut draft = Draft::new("a好👨‍👩‍👧");
        assert!(draft.left());
        assert_eq!(draft.split(), ("a好", "👨‍👩‍👧"));
        assert!(draft.left());
        assert_eq!(draft.split(), ("a", "好👨‍👩‍👧"));
        assert!(draft.right());
        assert_eq!(draft.split(), ("a好", "👨‍👩‍👧"));
    }

    #[test]
    fn neither_end_is_a_step_too_far() {
        let mut draft = Draft::new("ab");
        assert!(draft.home());
        assert!(!draft.home(), "already home");
        assert!(!draft.left(), "nothing before the start");
        assert!(!draft.backspace(), "nothing to take");
        assert!(draft.end());
        assert!(!draft.end(), "already at the end");
        assert!(!draft.right(), "nothing after the end");
        assert!(!draft.delete(), "nothing to take");
        assert_eq!(draft.text(), "ab");
    }

    #[test]
    fn delete_takes_what_is_in_front_and_leaves_the_caret_where_it_is() {
        let mut draft = Draft::new("笔记.md");
        draft.home();
        assert!(draft.delete());
        assert_eq!(draft.text(), "记.md");
        assert_eq!(draft.caret(), 0);
    }

    /// An empty field is a field the caret is in, not a field with no caret: the placeholder is
    /// drawn where the name will be, and what is typed lands at the start of it.
    #[test]
    fn typing_into_an_empty_field_starts_the_name() {
        let mut draft = Draft::default();
        draft.insert("提");
        draft.insert("纲");
        assert_eq!(draft.text(), "提纲");
        assert_eq!(draft.split(), ("提纲", ""));
    }

    /// A click lands the caret where the pointer was, and a drag from there pulls a selection
    /// out. Neither has to arrive on a boundary: a hit test answers in pixels.
    #[test]
    fn a_click_places_the_caret_and_a_drag_selects_from_it() {
        let mut draft = Draft::new("https://例子.com/dav");
        draft.place(8);
        assert_eq!(draft.caret(), 8);
        assert!(!draft.has_selection());
        // Mid-character, from a pointer halfway across 例: the boundary before it.
        draft.place(9);
        assert_eq!(draft.caret(), 8);
        draft.extend_to(8 + "例子".len());
        assert_eq!(draft.selected_text(), "例子");
    }

    #[test]
    fn what_is_typed_over_a_selection_replaces_it() {
        let mut draft = Draft::new("https://old/dav");
        assert!(draft.select_all());
        assert_eq!(draft.selected_text(), "https://old/dav");
        draft.insert("https://new/dav");
        assert_eq!(draft.text(), "https://new/dav");
        assert!(!draft.has_selection());
        // And an empty field has nothing to select.
        assert!(!Draft::default().select_all());
    }

    #[test]
    fn backspace_over_a_selection_takes_all_of_it() {
        let mut draft = Draft::new("abcdef");
        draft.place(2);
        draft.extend_to(5);
        assert!(draft.backspace());
        assert_eq!(draft.text(), "abf");
        assert_eq!(draft.caret(), 2);
    }

    /// An arrow with a selection standing collapses it to the end it points at, rather than
    /// stepping past it — which is what every other text field does, and what a writer who has
    /// just pressed Ctrl+A and changed their mind expects.
    #[test]
    fn an_arrow_collapses_a_selection_to_the_end_it_points_at() {
        let mut draft = Draft::new("abcdef");
        draft.place(2);
        draft.extend_to(5);
        assert!(draft.left());
        assert_eq!(draft.caret(), 2);
        assert!(!draft.has_selection());

        draft.place(2);
        draft.extend_to(5);
        assert!(draft.right());
        assert_eq!(draft.caret(), 5);
    }

    #[test]
    fn shift_arrows_grow_the_selection_from_the_anchor() {
        let mut draft = Draft::new("笔记.md");
        for _ in 0..4 {
            assert!(draft.extend_left());
        }
        assert_eq!(draft.selected_text(), "记.md");
        // And shrink it again from the same end.
        assert!(draft.extend_right());
        assert_eq!(draft.selected_text(), ".md");
        // Shift+Home reaches over the anchor at the far end and takes the whole of it.
        assert!(draft.extend_home());
        assert_eq!(draft.selected_text(), "笔记.md");
    }

    /// A URL is corrected a segment at a time: the host without the scheme, the last folder
    /// without the host.
    #[test]
    fn a_word_is_a_run_of_letters_or_a_run_of_everything_else() {
        let mut draft = Draft::new("https://example.com/dav");
        assert!(draft.word_left());
        assert_eq!(draft.split(), ("https://example.com/", "dav"));
        assert!(draft.word_left());
        assert_eq!(draft.split(), ("https://example.com", "/dav"));
        assert!(draft.backspace_word());
        assert_eq!(draft.text(), "https://example./dav");
        assert!(draft.home());
        assert!(draft.word_right());
        assert_eq!(draft.split(), ("https", "://example./dav"));
    }

    #[test]
    fn a_double_click_takes_the_word_under_it() {
        let mut draft = Draft::new("https://example.com/dav");
        draft.select_word_at("https://exa".len());
        assert_eq!(draft.selected_text(), "example");
    }

    /// The spellings on the way to å è½½, each standing where the last one stood. A field that
    /// took them as words rather than as a composition ended up holding all of them:
    /// `jjiajia'zjia'zaiå è½½`.
    #[test]
    fn a_composition_replaces_itself_and_is_committed_once() {
        let mut draft = Draft::default();
        for spelling in ["j", "jia", "jia'z", "jia'zai"] {
            draft.replace_and_mark_utf16(None, spelling, None);
            assert_eq!(draft.text(), spelling);
        }
        assert_eq!(draft.marked(), Some(0.."jia'zai".len()));
        draft.replace_utf16(None, "å è½½");
        assert_eq!(draft.text(), "å è½½");
        assert_eq!(draft.marked(), None);
        assert_eq!(draft.caret(), "å è½½".len());
    }

    #[test]
    fn a_composition_lands_at_the_caret_and_leaves_the_rest_where_it_was() {
        let mut draft = Draft::new("ç´ ç¼è¾å¨");
        draft.place("ç´ ".len());
        draft.replace_and_mark_utf16(None, "hao", None);
        assert_eq!(draft.text(), "ç´ haoç¼è¾å¨");
        draft.replace_utf16(None, "å¥½");
        assert_eq!(draft.split(), ("ç´ å¥½", "ç¼è¾å¨"));
    }

    /// A caret inside the preview, which is where an input method puts it while a spelling is
    /// still being chosen.
    #[test]
    fn the_caret_sits_where_the_composition_puts_it() {
        let mut draft = Draft::default();
        draft.replace_and_mark_utf16(None, "nihao", Some(2..2));
        assert_eq!(draft.split(), ("ni", "hao"));
    }

    /// Ranges arrive counted the way the platform counts, which is not the way the text is
    /// indexed: the face is two to it and four bytes here.
    #[test]
    fn a_range_from_the_platform_is_counted_in_utf16() {
        let mut draft = Draft::new("🙂ab");
        draft.replace_utf16(Some(2..4), "cd");
        assert_eq!(draft.text(), "🙂cd");
    }

    #[test]
    fn touching_the_field_any_other_way_ends_the_composition() {
        let mut draft = Draft::default();
        draft.replace_and_mark_utf16(None, "jia", None);
        assert!(draft.home());
        assert_eq!(draft.marked(), None);
        draft.replace_and_mark_utf16(None, "z", None);
        assert_eq!(draft.text(), "zjia");
    }
}
