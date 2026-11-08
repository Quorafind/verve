//! The keys of a focused drop-down `<select>` that is not open: they change the choice at once,
//! with `input` and then `change`, and some of them ask for the pop-up.
//!
//! Observed in Electron 39.8.10 (a hidden window, keys by `sendInputEvent`; probes `p32c/sel` and
//! `p32c-review/sel3-out.json`, a fresh window for each case): Down moves to the next option that
//! can be chosen and Up to the one before, Home and End to the first and the last, Page Down and
//! Page Up three options on (Apple, Cherry, Date; End, Page Up, Bandana: the count is of all the
//! options, and a disabled one it lands on is skipped), and a typed letter to the next option
//! whose label starts with it (the same letter again goes round, and letters typed within a
//! second make a longer prefix: `b a n d` reaches Bandana). The disabled option is skipped. The
//! page hears each key's `keydown` first, and a character's `keypress`; the change comes after.
//!
//! The pop-up opens on a press of the primary button that `mousedown` did not prevent, on F4
//! without Alt or Ctrl, on Alt+Up and Alt+Down (each after the page heard its `keydown`, and none
//! of them when that was prevented), and on Space (when no letters were typed in the last second)
//! and Enter, after their `keypress` (`p32c-review/sel3-out.json`, `sel4-out.json`; whether it
//! opened is told from whether the next Down reached the page). The document only decides that;
//! the shell draws the pop-up and takes the keys and presses while it is open
//! ([`Document::take_open_select`]).

use std::time::{Duration, Instant};

use crate::dom::NodeId;
use crate::{Document, Key};

/// How long a typed letter waits for the next one to make a longer prefix.
pub const TYPE_AHEAD: Duration = Duration::from_millis(1000);

/// The letters typed at a list of options in the last second.
#[derive(Clone, Debug, Default)]
pub struct TypeAhead {
    typed: String,
    at: Option<Instant>,
}

impl TypeAhead {
    /// Whether letters were typed within the last second as of `now`.
    pub fn is_typing(&self, now: Instant) -> bool {
        self.at
            .is_some_and(|at| !self.typed.is_empty() && now.duration_since(at) <= TYPE_AHEAD)
    }

    /// `text` was typed at `now` with option `current` chosen: the option to go to, if there is
    /// one. `options` are the labels and whether each can be chosen. A letter typed again goes
    /// to the next option that starts with it, several different ones to the first option
    /// (from the current one) that starts with all of them.
    pub fn find(
        &mut self,
        text: &str,
        now: Instant,
        current: Option<usize>,
        options: &[(&str, bool)],
    ) -> Option<usize> {
        if !self.is_typing(now) {
            self.typed.clear();
        }
        self.typed.push_str(&text.to_lowercase());
        self.at = Some(now);
        let first = self.typed.chars().next();
        let again = self.typed.chars().all(|c| Some(c) == first);
        let (prefix, start) = if again {
            (
                first.map(String::from).unwrap_or_default(),
                current.map_or(0, |at| at + 1),
            )
        } else {
            (self.typed.clone(), current.unwrap_or(0))
        };
        let count = options.len();
        (0..count).map(|k| (start + k) % count).find(|index| {
            let (label, open) = options[*index];
            open && label.trim_start().to_lowercase().starts_with(&prefix)
        })
    }
}

/// What the document keeps for the selects: the letters typed at the focused one, and the pop-up
/// the shell is to open.
#[derive(Default)]
pub(crate) struct SelectState {
    typing: Option<(NodeId, TypeAhead)>,
    open: Option<NodeId>,
}

impl Document {
    /// Whether `node` is a `<select>` that shows one option at a time and is not disabled.
    pub(crate) fn is_dropdown(&self, node: NodeId) -> bool {
        let state = self.state.borrow();
        let Some(found) = state.dom.get(node) else {
            return false;
        };
        let rows = found
            .attr("size")
            .and_then(|s| s.trim().parse::<u32>().ok());
        found.tag() == Some("select")
            && found.attr("multiple").is_none()
            && rows.is_none_or(|rows| rows <= 1)
            && found.attr("disabled").is_none()
    }

    /// The drop-down a person asked to open since this was last called: the shell opens its
    /// pop-up (the decision is the document's, as it is Chromium's).
    pub fn take_open_select(&self) -> Option<NodeId> {
        self.view.borrow_mut().select.open.take()
    }

    /// Asks for the pop-up of the drop-down `select`.
    pub(crate) fn open_select(&self, select: NodeId) {
        self.view.borrow_mut().select.open = Some(select);
    }

    /// The drop-down a press at `target` is on, if there is one: the select itself or something
    /// in it.
    pub(crate) fn dropdown_under(&self, target: NodeId) -> Option<NodeId> {
        let select = {
            let state = self.state.borrow();
            let dom = &state.dom;
            std::iter::successors(Some(target), |node| dom.parent(*node))
                .find(|node| dom.get(*node).and_then(|n| n.tag()) == Some("select"))
        };
        select.filter(|select| self.is_dropdown(*select))
    }

    /// A key on the focused drop-down `select` that asks for its pop-up: F4 without Alt or Ctrl,
    /// Alt+Up and Alt+Down (on the `keydown`), Enter (after its `keypress`, which this fires).
    /// Answers whether it was one.
    pub(crate) fn select_opener(
        &self,
        runtime: &mut verve::Runtime,
        select: NodeId,
        key: &Key,
    ) -> bool {
        let held = key.modifiers;
        let opens = match key.key.as_str() {
            "F4" => !held.alt && !held.ctrl,
            "ArrowUp" | "ArrowDown" => held.alt && !held.ctrl,
            "Enter" => {
                let init = Self::key_init(key, 13);
                if self.fire(runtime, select, "KeyboardEvent", "keypress", &init) {
                    self.open_select(select);
                }
                return true;
            }
            _ => return false,
        };
        if opens {
            self.open_select(select);
        }
        opens
    }

    /// A character typed at the focused drop-down `select`, after its `keypress`: Space opens the
    /// pop-up unless it follows other letters, anything else goes to the option that starts with
    /// the letters typed in the last second.
    pub(crate) fn select_typed(&self, runtime: &mut verve::Runtime, select: NodeId, text: &str) {
        let now = Instant::now();
        let mut typing = match self.view.borrow_mut().select.typing.take() {
            Some((owner, typing)) if owner == select => typing,
            _ => TypeAhead::default(),
        };
        if text == " " && !typing.is_typing(now) {
            self.open_select(select);
            return;
        }
        let to = {
            let state = self.state.borrow();
            let dom = &state.dom;
            let listed = dom.listed_options(select);
            let current = dom
                .selected_option(select)
                .and_then(|chosen| listed.iter().position(|o| *o == chosen));
            let labels: Vec<(String, bool)> = (listed.iter())
                .map(|o| (dom.text_content(*o), !dom.option_disabled(*o)))
                .collect();
            let options: Vec<(&str, bool)> = labels.iter().map(|(l, o)| (l.as_str(), *o)).collect();
            typing.find(text, now, current, &options)
        };
        self.view.borrow_mut().select.typing = Some((select, typing));
        if let Some(index) = to {
            self.choose_option(runtime, select, index);
        }
    }

    /// A key on the focused drop-down `select`: the choice moves. Answers whether the key was
    /// one that does that.
    pub(crate) fn select_key(
        &self,
        runtime: &mut verve::Runtime,
        select: NodeId,
        key: &str,
    ) -> bool {
        let target = {
            let state = self.state.borrow();
            let dom = &state.dom;
            let listed = dom.listed_options(select);
            let open = |at: usize| !dom.option_disabled(listed[at]);
            let current = dom
                .selected_option(select)
                .and_then(|chosen| listed.iter().position(|o| *o == chosen));
            let after = |from: usize| (from + 1..listed.len()).find(|at| open(*at));
            let before = |from: usize| (0..from).rev().find(|at| open(*at));
            let last = listed.len().saturating_sub(1);
            match key {
                "ArrowDown" | "ArrowRight" => Some(current.map_or_else(|| after(0), after)),
                "ArrowUp" | "ArrowLeft" => Some(current.and_then(before)),
                "Home" => Some((0..listed.len()).find(|at| open(*at))),
                "End" => Some((0..listed.len()).rev().find(|at| open(*at))),
                // Three options on, then the nearest that can be chosen in the direction of the
                // move, and the other way when there is none.
                "PageDown" => Some(current.and_then(|at| {
                    let to = (at + 3).min(last);
                    (to..listed.len()).find(|i| open(*i)).or_else(|| before(to))
                })),
                "PageUp" => Some(current.and_then(|at| {
                    let to = at.saturating_sub(3);
                    (0..=to).rev().find(|i| open(*i)).or_else(|| after(to))
                })),
                _ => None,
            }
        };
        match target {
            None => false,
            Some(to) => {
                if let Some(index) = to {
                    self.choose_option(runtime, select, index);
                }
                true
            }
        }
    }
}
