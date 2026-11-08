//! Undo and redo in the text of an `<input>` and a `<textarea>` (P32).
//!
//! A field keeps the drafts it had before each edit. Typing joins into one step as long as each
//! character lands where the one before it left the caret, so one Ctrl+Z takes back a run of
//! typing. Electron 39.8.10 (the coordinator's `probe-keys` probe, `keys2`,
//! `type-x-then-ctrl-z-in-input`): Ctrl+Z after typing undoes, and the key is then handled.

use boa_engine::JsValue;

use crate::field::Draft;
use crate::{Document, NodeId, bind};

/// The most steps a field keeps, as a host's history does (`editing::LIMIT`).
const LIMIT: usize = 1000;

/// What kind of edit was made, which decides whether the next one joins its step.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Edit {
    /// A character typed: it joins the typing before it, and the typing after it joins.
    Typing,
    /// An input method's commit: it joins the typing before it, but the next edit starts a step
    /// (Electron 39.8.10: "ab", 加, 载, Ctrl+Z gives "ab加", then "").
    Commit,
    /// A deletion, a paste or a cut: a step of its own.
    Other,
    /// Backspace or Delete: they join each other, not typing.
    Delete,
}

/// The drafts a field can go back to and forward to.
#[derive(Default)]
pub(crate) struct FieldHistory {
    undo: Vec<Draft>,
    redo: Vec<Draft>,
    /// Where the caret was left by the edit that the next one may join, and which kind it was.
    joins_at: Option<(usize, Edit)>,
    /// The text the field has after the last edit or step. A script that sets the value changes
    /// it, and the history before is then of another text.
    text: String,
}

impl FieldHistory {
    /// Notes an edit that changed `before` into `after`.
    fn record(&mut self, before: Draft, after: &Draft, kind: Edit) {
        if before.text() != self.text {
            *self = Self::default();
        }
        let joinable = match kind {
            Edit::Typing | Edit::Commit => matches!(self.joins_at, Some((_, Edit::Typing))),
            Edit::Delete => matches!(self.joins_at, Some((_, Edit::Delete))),
            Edit::Other => false,
        };
        let joins = joinable
            && !before.has_selection()
            && self.joins_at.is_some_and(|(at, _)| at == before.caret())
            && !self.undo.is_empty();
        if !joins {
            self.undo.push(before);
            if self.undo.len() > LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.joins_at = match kind {
            Edit::Typing | Edit::Delete => Some((after.caret(), kind)),
            Edit::Commit | Edit::Other => None,
        };
        self.text = after.text().to_owned();
    }

    /// The draft to go to when going back (or forward), given the one now.
    fn travel(&mut self, now: Draft, forward: bool) -> Option<Draft> {
        self.joins_at = None;
        let (from, to) = if forward {
            (&mut self.redo, &mut self.undo)
        } else {
            (&mut self.undo, &mut self.redo)
        };
        let step = from.pop()?;
        to.push(now);
        self.text = step.text().to_owned();
        Some(step)
    }

    fn has(&self, redo: bool) -> bool {
        if redo {
            !self.redo.is_empty()
        } else {
            !self.undo.is_empty()
        }
    }
}

impl Document {
    /// A copy of the field's draft, to give to [`Document::edited`] with the edit done.
    pub(crate) fn draft_before(&self, field: NodeId) -> Option<Draft> {
        self.state.borrow().dom.get(field)?.draft.clone()
    }

    /// The field was edited from `before`: remembers it for Ctrl+Z.
    pub(crate) fn edited(&self, field: NodeId, before: Option<Draft>, kind: Edit) {
        let Some(before) = before else { return };
        let mut state = self.state.borrow_mut();
        let Some(after) = state.dom.get(field).and_then(|n| n.draft.clone()) else {
            return;
        };
        state
            .field_history
            .entry(field)
            .or_default()
            .record(before, &after, kind);
    }

    /// Undoes (or redoes) the field's last edit, and answers whether there was one to take.
    pub(crate) fn undo_field(
        &self,
        runtime: &mut verve::Runtime,
        field: NodeId,
        redo: bool,
    ) -> bool {
        // A script that has set the value since the last edit took the history's text away.
        let now = self
            .draft_before(field)
            .map(|draft| draft.text().to_owned());
        let available = {
            let mut state = self.state.borrow_mut();
            if let Some(history) = state.field_history.get_mut(&field)
                && now.is_some_and(|now| now != history.text)
            {
                *history = FieldHistory::default();
            }
            (state.field_history.get(&field)).is_some_and(|history| history.has(redo))
        };
        if !available {
            return false;
        }
        let input_type = if redo { "historyRedo" } else { "historyUndo" };
        let init = [
            ("inputType", verve::native::js_text(input_type)),
            ("data", JsValue::null()),
        ];
        if !self.fire(runtime, field, "InputEvent", "beforeinput", &init) {
            return true;
        }
        let now = bind::node::with_dom(runtime.context(), |dom| {
            bind::element::ensure_draft(dom, field);
            dom.get(field).and_then(|n| n.draft.clone())
        });
        let step = now.and_then(|now| {
            let mut state = self.state.borrow_mut();
            (state.field_history.get_mut(&field)).and_then(|history| history.travel(now, redo))
        });
        let Some(step) = step else { return false };
        bind::node::with_dom(runtime.context(), |dom| {
            if let Some(node) = dom.get_mut(field) {
                node.draft = Some(step);
                node.dirty_value = true;
            }
            dom.mark(field);
        });
        self.fire(runtime, field, "InputEvent", "input", &init);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(text: &str) -> Draft {
        Draft::new(text)
    }

    #[test]
    fn typing_in_a_row_is_one_step() {
        let mut history = FieldHistory::default();
        history.record(draft(""), &draft("a"), Edit::Typing);
        history.record(draft("a"), &draft("ab"), Edit::Typing);
        assert_eq!(history.undo.len(), 1);
        assert_eq!(history.travel(draft("ab"), false), Some(draft("")));
        assert_eq!(history.travel(draft(""), true), Some(draft("ab")));
    }

    #[test]
    fn a_delete_starts_a_new_step() {
        let mut history = FieldHistory::default();
        history.record(draft(""), &draft("a"), Edit::Typing);
        history.record(draft("a"), &draft(""), Edit::Other);
        history.record(draft(""), &draft("b"), Edit::Typing);
        assert_eq!(history.undo.len(), 3);
    }

    #[test]
    fn backspaces_in_a_row_are_one_step() {
        // Electron 39.8.10: "abc", Backspace twice, Ctrl+Z gives "abc", then "".
        let mut history = FieldHistory::default();
        history.record(draft(""), &draft("abc"), Edit::Typing);
        history.record(draft("abc"), &draft("ab"), Edit::Delete);
        history.record(draft("ab"), &draft("a"), Edit::Delete);
        assert_eq!(history.undo.len(), 2);
        assert_eq!(history.travel(draft("a"), false), Some(draft("abc")));
    }

    #[test]
    fn a_commit_joins_the_typing_before_it_and_the_next_commit_does_not() {
        // Electron 39.8.10: "ab", then 加, then 载; Ctrl+Z gives "ab加", then "".
        let mut history = FieldHistory::default();
        history.record(draft(""), &draft("ab"), Edit::Typing);
        history.record(draft("ab"), &draft("ab加"), Edit::Commit);
        history.record(draft("ab加"), &draft("ab加载"), Edit::Commit);
        assert_eq!(history.travel(draft("ab加载"), false), Some(draft("ab加")));
        assert_eq!(history.travel(draft("ab加"), false), Some(draft("")));
        assert!(!history.has(false));
    }

    #[test]
    fn a_value_a_script_set_takes_the_history_away() {
        // Electron 39.8.10: type "abc", set the value to "xyz", type Q, and Ctrl+Z gives "xyz".
        let mut history = FieldHistory::default();
        history.record(draft(""), &draft("abc"), Edit::Typing);
        history.record(draft("xyz"), &draft("xyzQ"), Edit::Typing);
        assert_eq!(history.undo.len(), 1);
        assert_eq!(history.travel(draft("xyzQ"), false), Some(draft("xyz")));
    }

    #[test]
    fn an_edit_forgets_what_could_be_redone() {
        let mut history = FieldHistory::default();
        history.record(draft(""), &draft("a"), Edit::Typing);
        history.travel(draft("a"), false);
        assert!(history.has(true));
        history.record(draft(""), &draft("c"), Edit::Typing);
        assert!(!history.has(true));
    }
}
