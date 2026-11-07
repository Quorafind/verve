//! The console of the inspector: the page's lines, what was typed into the REPL and what came
//! back, and the line editor that takes the typing.

use std::collections::VecDeque;

use verve::Level;
use verve_electron::hub::ConsoleEntry;

/// The most lines the console keeps; older ones go first.
pub const MOST_LINES: usize = 1000;

/// The most lines the REPL remembers to walk back through.
const MOST_HISTORY: usize = 200;

/// What a line of the console is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// A line the page wrote, at the level of the console method.
    Page(Level),
    /// What was typed into the REPL.
    Input,
    /// What the REPL's code gave.
    Output,
    /// What the REPL's code threw.
    Failure,
}

/// One line of the console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
    /// `file:line` of a page's line, else empty.
    pub origin: String,
}

/// The lines of one page's console and the REPL's questions waiting for an answer.
#[derive(Default)]
pub struct Console {
    lines: VecDeque<Line>,
    /// The newest line of the page that was taken from the Hub.
    seen: u64,
    /// The calls not yet answered.
    waiting: Vec<u64>,
}

impl Console {
    /// The lines, oldest first.
    pub fn lines(&self) -> &VecDeque<Line> {
        &self.lines
    }

    /// The number of the newest page line taken: what to ask the Hub for lines newer than.
    pub fn seen(&self) -> u64 {
        self.seen
    }

    /// Takes the page's lines that are new; the ones already taken are skipped.
    pub fn take(&mut self, entries: Vec<ConsoleEntry>) {
        let seen = self.seen;
        for entry in entries.into_iter().filter(|entry| entry.seq > seen) {
            self.seen = entry.seq;
            let file = entry.source.rsplit(['/', '\\']).next().unwrap_or("");
            let origin = match (file, entry.line) {
                ("", _) => String::new(),
                (file, 0) => file.to_owned(),
                (file, line) => format!("{file}:{line}"),
            };
            self.push(Line {
                kind: LineKind::Page(entry.level),
                text: entry.message,
                origin,
            });
        }
    }

    /// Shows `code` as typed and waits for the answer to `call`, a number the Hub gave out
    /// (`Hub::next_inspector_call`), so that no other window's call is the same.
    pub fn ask(&mut self, call: u64, code: &str) {
        self.waiting.push(call);
        self.push(Line {
            kind: LineKind::Input,
            text: code.to_owned(),
            origin: String::new(),
        });
    }

    /// Shows the answer to `call`; a call that is not waiting (an answer that came twice, or from
    /// before the console was cleared by a navigation) shows nothing. Answers whether it was shown.
    pub fn answer(&mut self, call: u64, text: String, is_error: bool) -> bool {
        let Some(at) = self.waiting.iter().position(|waiting| *waiting == call) else {
            return false;
        };
        self.waiting.remove(at);
        let kind = if is_error {
            LineKind::Failure
        } else {
            LineKind::Output
        };
        self.push(Line {
            kind,
            text,
            origin: String::new(),
        });
        true
    }

    /// Empties the console, as a navigation does. Questions still waiting are forgotten with it.
    pub fn clear(&mut self) {
        self.lines.clear();
        self.waiting.clear();
    }

    /// Adds `line`, a row for each line of its text: the first has the origin, the others none.
    fn push(&mut self, line: Line) {
        let Line { kind, text, origin } = line;
        let mut rows = text.lines().map(str::to_owned).collect::<Vec<_>>();
        if rows.is_empty() {
            rows.push(String::new());
        }
        for (at, text) in rows.into_iter().enumerate() {
            if self.lines.len() == MOST_LINES {
                self.lines.pop_front();
            }
            let origin = if at == 0 {
                origin.clone()
            } else {
                String::new()
            };
            self.lines.push_back(Line { kind, text, origin });
        }
    }
}

/// The text field of the REPL: what is typed, where the caret is, and the lines sent before.
#[derive(Default)]
pub struct LineEditor {
    text: String,
    /// The caret, in characters from the start.
    caret: usize,
    history: Vec<String>,
    /// Which line of the history is shown, if one is.
    shown: Option<usize>,
    /// What was typed before the history was walked.
    draft: String,
}

impl LineEditor {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The caret, in characters from the start.
    pub fn caret(&self) -> usize {
        self.caret
    }

    fn byte_at(&self, chars: usize) -> usize {
        let mut at = self.text.char_indices().map(|(at, _)| at);
        at.nth(chars).unwrap_or(self.text.len())
    }

    fn length(&self) -> usize {
        self.text.chars().count()
    }

    /// Types `typed` at the caret. A control character other than a tab is not text.
    pub fn insert(&mut self, typed: &str) {
        let keeps = |c: &char| !c.is_control() || *c == '\t';
        let typed: String = typed.chars().filter(keeps).collect();
        let at = self.byte_at(self.caret);
        self.text.insert_str(at, &typed);
        self.caret += typed.chars().count();
        self.shown = None;
    }

    /// Takes away the character before the caret.
    pub fn backspace(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            let at = self.byte_at(self.caret);
            self.text.remove(at);
        }
        self.shown = None;
    }

    /// Takes away the character at the caret.
    pub fn delete(&mut self) {
        if self.caret < self.length() {
            let at = self.byte_at(self.caret);
            self.text.remove(at);
        }
        self.shown = None;
    }

    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.length());
    }

    pub fn home(&mut self) {
        self.caret = 0;
    }

    pub fn end(&mut self) {
        self.caret = self.length();
    }

    /// Shows the line sent before the one shown (the newest, if none is shown).
    pub fn older(&mut self) {
        let at = match self.shown {
            None if self.history.is_empty() => return,
            None => {
                self.draft = std::mem::take(&mut self.text);
                self.history.len() - 1
            }
            Some(at) => at.saturating_sub(1),
        };
        self.show(at);
    }

    /// Shows the line sent after the one shown, and what was typed before the history was walked
    /// after the newest.
    pub fn newer(&mut self) {
        match self.shown {
            None => {}
            Some(at) if at + 1 < self.history.len() => self.show(at + 1),
            Some(_) => {
                self.shown = None;
                self.text = std::mem::take(&mut self.draft);
                self.end();
            }
        }
    }

    fn show(&mut self, at: usize) {
        self.shown = Some(at);
        self.text.clone_from(&self.history[at]);
        self.end();
    }

    /// Sends the line: answers it (unless it is only white space) and clears the field. A line
    /// the same as the one sent last is not kept twice in the history.
    pub fn submit(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.text);
        self.caret = 0;
        self.shown = None;
        self.draft.clear();
        if line.trim().is_empty() {
            return None;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > MOST_HISTORY {
                self.history.remove(0);
            }
        }
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u64, level: Level, message: &str, source: &str, line: u32) -> ConsoleEntry {
        ConsoleEntry {
            seq,
            level,
            message: message.to_owned(),
            source: source.to_owned(),
            line,
        }
    }

    #[test]
    fn the_page_lines_are_taken_once_with_the_file_and_line_they_came_from() {
        let mut console = Console::default();
        let first = entry(1, Level::Log, "hello", "file:///app/page.html", 7);
        console.take(vec![first.clone(), entry(2, Level::Error, "bad", "", 0)]);
        console.take(vec![first, entry(3, Level::Warn, "w", "x.js", 0)]);
        let lines: Vec<(LineKind, &str, &str)> = (console.lines().iter())
            .map(|l| (l.kind, l.text.as_str(), l.origin.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (LineKind::Page(Level::Log), "hello", "page.html:7"),
                (LineKind::Page(Level::Error), "bad", ""),
                (LineKind::Page(Level::Warn), "w", "x.js"),
            ]
        );
        assert_eq!(console.seen(), 3);
    }

    #[test]
    fn a_question_shows_as_typed_and_its_answer_follows_once() {
        let mut console = Console::default();
        console.ask(1, "document.title");
        assert!(console.answer(1, "'Hello'".into(), false));
        assert!(!console.answer(1, "'Hello'".into(), false));
        console.ask(2, "boom()");
        assert!(console.answer(2, "ReferenceError".into(), true));
        let kinds: Vec<LineKind> = console.lines().iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                LineKind::Input,
                LineKind::Output,
                LineKind::Input,
                LineKind::Failure
            ]
        );
        console.ask(3, "x");
        console.clear();
        assert!(console.lines().is_empty());
        assert!(!console.answer(3, "late".into(), false));
    }

    #[test]
    fn a_value_of_several_lines_is_a_row_for_each() {
        let mut console = Console::default();
        console.ask(1, "x");
        assert!(console.answer(1, "{\n  a: 1,\n  b: 2\n}".into(), false));
        let rows: Vec<&str> = console.lines().iter().map(|l| l.text.as_str()).collect();
        assert_eq!(rows, ["x", "{", "  a: 1,", "  b: 2", "}"]);
        assert!(
            console
                .lines()
                .iter()
                .skip(1)
                .all(|l| l.kind == LineKind::Output)
        );
    }

    #[test]
    fn the_console_keeps_its_newest_lines() {
        let mut console = Console::default();
        let entries = (1..=(MOST_LINES as u64 + 5))
            .map(|n| entry(n, Level::Log, &format!("line {n}"), "", 0))
            .collect();
        console.take(entries);
        assert_eq!(console.lines().len(), MOST_LINES);
        assert_eq!(console.lines()[0].text, "line 6");
    }

    #[test]
    fn typing_moves_the_caret_and_edits_in_the_middle() {
        let mut editor = LineEditor::default();
        editor.insert("héllo");
        assert_eq!(editor.caret(), 5);
        editor.left();
        editor.left();
        editor.insert("XY");
        assert_eq!(editor.text(), "hélXYlo");
        editor.backspace();
        editor.delete();
        assert_eq!(editor.text(), "hélXo");
        editor.home();
        editor.backspace();
        assert_eq!(editor.text(), "hélXo");
        editor.end();
        editor.delete();
        assert_eq!(editor.caret(), 5);
        editor.insert("\n\u{7}z");
        assert_eq!(editor.text(), "hélXoz");
    }

    #[test]
    fn the_history_is_walked_with_up_and_down_and_the_draft_comes_back() {
        let mut editor = LineEditor::default();
        for line in ["one", "two", "two", "three"] {
            editor.insert(line);
            assert_eq!(editor.submit().as_deref(), Some(line));
        }
        editor.insert("dra");
        editor.older();
        assert_eq!(editor.text(), "three");
        editor.older();
        editor.older();
        assert_eq!(editor.text(), "one");
        editor.older();
        assert_eq!(editor.text(), "one");
        editor.newer();
        assert_eq!(editor.text(), "two");
        editor.newer();
        editor.newer();
        assert_eq!(editor.text(), "dra");
        assert_eq!(editor.caret(), 3);
    }

    #[test]
    fn the_history_keeps_the_newest_lines() {
        let mut editor = LineEditor::default();
        for n in 0..(MOST_HISTORY + 5) {
            editor.insert(&format!("line {n}"));
            editor.submit();
        }
        for _ in 0..(MOST_HISTORY + 5) {
            editor.older();
        }
        assert_eq!(editor.text(), "line 5");
    }

    #[test]
    fn a_blank_line_is_not_sent() {
        let mut editor = LineEditor::default();
        editor.insert("   ");
        assert_eq!(editor.submit(), None);
        assert_eq!(editor.text(), "");
        editor.older();
        assert_eq!(editor.text(), "");
    }
}
