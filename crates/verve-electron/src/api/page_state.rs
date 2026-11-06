//! What the main process keeps of the page a window shows, apart from the window: where it is,
//! how it got there, whether it is loading and what `loadURL` is waiting for.
//!
//! The history is `webContents.navigationHistory`'s. Every document the page commits is an entry,
//! and a failed load too (Electron 39.8.10 keeps the failed address as the current entry); what
//! the page does with it — a new entry, a replaced one, a move through the list or nothing for a
//! reload — is the [`Intent`] the load was started with.

use std::cell::{Cell, RefCell};

use boa_engine::builtins::promise::ResolvingFunctions;

/// One entry of the session history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub url: String,
    pub title: String,
}

/// How the document a load commits enters the history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Intent {
    /// A new entry after the current one, dropping any that were ahead of it.
    #[default]
    Push,
    /// The current entry becomes this document (`location.replace`).
    Replace,
    /// The entry at this index becomes current (`goBack`, `goToIndex`).
    Go(usize),
    /// The same document again: the history does not change.
    Reload,
}

/// The session history of one page.
#[derive(Debug, Default)]
pub(crate) struct History {
    entries: Vec<Entry>,
    active: usize,
}

impl History {
    /// A document with `url` was committed the way `intent` says. Loading the address the page
    /// is already at is no new entry, as in Chromium.
    pub(crate) fn commit(&mut self, intent: Intent, url: &str) {
        let same = self
            .entries
            .get(self.active)
            .is_some_and(|entry| entry.url == url);
        match intent {
            Intent::Go(index) if index < self.entries.len() => self.active = index,
            Intent::Reload => {}
            Intent::Replace | Intent::Push if same => {}
            Intent::Replace if !self.entries.is_empty() => {
                self.entries[self.active].url = url.to_owned();
            }
            _ => {
                self.entries.truncate(self.active + 1);
                self.active = self.entries.len();
                self.entries.push(Entry {
                    url: url.to_owned(),
                    title: String::new(),
                });
            }
        }
    }

    /// The current entry's title is `title`.
    pub(crate) fn set_title(&mut self, title: &str) {
        if let Some(entry) = self.entries.get_mut(self.active) {
            title.clone_into(&mut entry.title);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The current entry's index, `-1` for an empty history.
    pub(crate) fn active(&self) -> i32 {
        if self.entries.is_empty() {
            -1
        } else {
            self.active as i32
        }
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The entry `offset` steps from the current one, if there is one.
    pub(crate) fn index_at(&self, offset: i32) -> Option<usize> {
        let index = i64::try_from(self.active).ok()? + i64::from(offset);
        let index = usize::try_from(index).ok()?;
        (index < self.entries.len() && !self.entries.is_empty()).then_some(index)
    }

    /// Takes away the entry at `index`, unless it is the current one. Whether it did.
    pub(crate) fn remove(&mut self, index: usize) -> bool {
        if index >= self.entries.len() || index == self.active {
            return false;
        }
        self.entries.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        true
    }

    /// Takes away every entry but the current one.
    pub(crate) fn clear(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.entries = vec![self.entries.swap_remove(self.active)];
        self.active = 0;
    }
}

/// The page of one window, as the main process knows it.
#[derive(Default)]
pub(crate) struct PageState {
    /// The address of the current document.
    pub url: RefCell<String>,
    pub title: RefCell<String>,
    pub history: RefCell<History>,
    /// How the next committed document enters the history.
    pub intent: Cell<Intent>,
    /// The address the load in progress is for, until it commits.
    pub target: RefCell<String>,
    pub loading: Cell<bool>,
    /// Whether `did-start-navigation` was said for the load in progress.
    pub announced: Cell<bool>,
    /// Whether `frame-created` was said, once.
    pub frame_created: Cell<bool>,
    /// The promise of the `loadURL` that has not finished.
    pub load: RefCell<Option<ResolvingFunctions>>,
    pub user_agent: RefCell<Option<String>>,
    pub zoom_factor: Cell<Option<f64>>,
    /// The page's process ended and no load has made a new one.
    pub crashed: Cell<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages(urls: &[&str]) -> History {
        let mut history = History::default();
        for url in urls {
            history.commit(Intent::Push, url);
        }
        history
    }

    #[test]
    fn a_new_document_cuts_off_what_was_ahead() {
        let mut history = pages(&["a", "b", "c"]);
        history.commit(Intent::Go(0), "a");
        history.commit(Intent::Push, "d");
        let urls: Vec<_> = history.entries().iter().map(|e| e.url.as_str()).collect();
        assert_eq!((urls, history.active()), (vec!["a", "d"], 1));
    }

    #[test]
    fn loading_where_the_page_is_and_reloading_add_nothing() {
        let mut history = pages(&["a", "b"]);
        history.commit(Intent::Push, "b");
        history.commit(Intent::Reload, "b");
        assert_eq!((history.len(), history.active()), (2, 1));
    }

    #[test]
    fn the_current_entry_cannot_be_removed_and_clear_keeps_it() {
        let mut history = pages(&["a", "b", "c"]);
        assert!(!history.remove(2));
        assert!(history.remove(0));
        assert_eq!((history.len(), history.active()), (2, 1));
        history.clear();
        assert_eq!((history.len(), history.active()), (1, 0));
        assert_eq!(history.entries()[0].url, "c");
    }

    #[test]
    fn offsets_reach_only_entries_there_are() {
        let history = pages(&["a", "b", "c"]);
        assert_eq!(history.index_at(-2), Some(0));
        assert_eq!(history.index_at(-3), None);
        assert_eq!(history.index_at(1), None);
    }
}
