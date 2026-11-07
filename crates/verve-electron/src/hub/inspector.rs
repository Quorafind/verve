//! What the Hub keeps for the inspector window (P43): the last lines of each page's console, which
//! the window shows, who has the DevTools' focus, and the page's REPL answers on their way to the
//! shell.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use verve::Level;

use super::{ContentsId, Hub, lock};
use crate::msg::{FromRenderer, ToShell};

/// The most console lines the Hub keeps for one page. DevTools keeps more; an inspector this small
/// shows the end of a console, and a page that logs in a loop cannot grow the Hub without bound.
const KEPT_CONSOLE_LINES: usize = 500;

/// The most characters of one line the Hub keeps: a page that logs a megabyte string in a loop
/// cannot hold hundreds of them.
const KEPT_LINE_CHARS: usize = 4000;

/// A line of a page's console, as the inspector shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleEntry {
    /// Counts up from 1 for each page: [`Hub::console_since`] asks for what is newer than one.
    pub seq: u64,
    pub level: Level,
    pub message: String,
    /// The file the line was written in, if the page said.
    pub source: String,
    pub line: u32,
}

/// One page's lines.
#[derive(Default)]
pub(super) struct Kept {
    last: u64,
    lines: VecDeque<ConsoleEntry>,
}

impl Hub {
    /// Keeps a console line of page `contents` for the inspector.
    pub(super) fn keep_console(
        &self,
        contents: ContentsId,
        level: Level,
        at: (&str, u32),
        message: &str,
    ) {
        let message = match message.char_indices().nth(KEPT_LINE_CHARS) {
            Some((end, _)) => format!("{}…", &message[..end]),
            None => message.to_owned(),
        };
        let mut pages = lock(&self.kept_console);
        // A page that is gone has no ring; a batch still on its way does not make one.
        let Some(kept) = pages.get_mut(&contents) else {
            return;
        };
        kept.last += 1;
        if kept.lines.len() == KEPT_CONSOLE_LINES {
            kept.lines.pop_front();
        }
        kept.lines.push_back(ConsoleEntry {
            seq: kept.last,
            level,
            message,
            source: at.0.to_owned(),
            line: at.1,
        });
    }

    /// Page `contents` was added: it gets the ring its console lines are kept in.
    pub(super) fn start_kept_console(&self, contents: ContentsId) {
        lock(&self.kept_console).entry(contents).or_default();
    }

    /// The lines page `contents` wrote since line `after` (`0` for all it still has), oldest first.
    /// A navigation empties the console, as it does in a browser: the numbers go on, so what the
    /// inspector already has is never mistaken for new.
    pub fn console_since(&self, contents: ContentsId, after: u64) -> Vec<ConsoleEntry> {
        let pages = lock(&self.kept_console);
        let Some(kept) = pages.get(&contents) else {
            return Vec::new();
        };
        kept.lines
            .iter()
            .filter(|entry| entry.seq > after)
            .cloned()
            .collect()
    }

    /// The number for the next call of an inspector's REPL: one counter for every window, so a
    /// number is never given twice.
    pub fn next_inspector_call(&self) -> u64 {
        self.inspector_calls.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Which document page `contents` is showing: it counts up at each navigation after the
    /// first load (a reload too), and is `0` for a page the Hub does not have. An inspector tells
    /// a new document by it.
    pub fn page_generation(&self, contents: ContentsId) -> u64 {
        lock(&self.renderers)
            .get(&contents)
            .map_or(0, |page| page.generation())
    }

    /// The page is gone: what the Hub kept for its inspector goes with it.
    pub(super) fn forget_inspector(&self, contents: ContentsId) {
        lock(&self.kept_console).remove(&contents);
        lock(&self.devtools_focused).remove(&contents);
    }

    /// The page navigated: its console is empty.
    pub(super) fn clear_kept_console(&self, contents: ContentsId) {
        if let Some(kept) = lock(&self.kept_console).get_mut(&contents) {
            kept.lines.clear();
        }
    }

    /// The inspector window of page `contents` has the focus, or has not.
    pub fn set_devtools_focused(&self, contents: ContentsId, focused: bool) {
        let mut pages = lock(&self.devtools_focused);
        if focused {
            pages.insert(contents);
        } else {
            pages.remove(&contents);
        }
    }

    /// Whether the inspector window of page `contents` has the focus.
    pub fn is_devtools_focused(&self, contents: ContentsId) -> bool {
        lock(&self.devtools_focused).contains(&contents)
    }

    /// Takes the REPL answers out of what page `contents` said: they go to the inspector window,
    /// and the main process hears the rest.
    pub(super) fn take_inspector_replies(
        &self,
        contents: ContentsId,
        events: Vec<FromRenderer>,
    ) -> Vec<FromRenderer> {
        let (replies, rest): (Vec<_>, Vec<_>) = events
            .into_iter()
            .partition(|event| matches!(event, FromRenderer::InspectorEvalReply { .. }));
        for reply in replies {
            if let FromRenderer::InspectorEvalReply {
                call,
                text,
                is_error,
            } = reply
            {
                self.to_shell(ToShell::InspectorReply {
                    contents,
                    call,
                    text,
                    is_error,
                });
            }
        }
        rest
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ElectronOptions;

    fn hub() -> Arc<Hub> {
        Hub::new(ElectronOptions {
            app_path: std::env::temp_dir(),
            dark: false,
        })
    }

    #[test]
    fn a_page_keeps_its_newest_lines_and_numbers_them_on() {
        let hub = hub();
        hub.start_kept_console(7);
        for n in 0..(KEPT_CONSOLE_LINES + 3) {
            hub.keep_console(7, Level::Log, ("a.js", 1), &format!("line {n}"));
        }
        let all = hub.console_since(7, 0);
        assert_eq!(all.len(), KEPT_CONSOLE_LINES);
        assert_eq!(all[0].message, "line 3");
        assert_eq!(all[0].seq, 4);
        assert_eq!(all[all.len() - 1].seq, (KEPT_CONSOLE_LINES + 3) as u64);
        let newer = hub.console_since(7, all[all.len() - 2].seq);
        assert_eq!(newer.len(), 1);
        assert!(hub.console_since(8, 0).is_empty());
    }

    #[test]
    fn a_navigation_empties_the_console_and_the_numbers_go_on() {
        let hub = hub();
        hub.start_kept_console(1);
        hub.keep_console(1, Level::Warn, ("a.js", 2), "before");
        hub.clear_kept_console(1);
        assert!(hub.console_since(1, 0).is_empty());
        hub.keep_console(1, Level::Warn, ("a.js", 2), "after");
        assert_eq!(hub.console_since(1, 0)[0].seq, 2);
    }

    #[test]
    fn a_long_line_is_cut_and_a_gone_page_is_forgotten() {
        let hub = hub();
        hub.start_kept_console(1);
        hub.keep_console(
            1,
            Level::Log,
            ("a.js", 1),
            &"é".repeat(KEPT_LINE_CHARS + 10),
        );
        let kept = hub.console_since(1, 0).remove(0).message;
        assert_eq!(kept.chars().count(), KEPT_LINE_CHARS + 1);
        assert!(kept.ends_with('…'));
        hub.set_devtools_focused(1, true);
        hub.forget_inspector(1);
        assert!(hub.console_since(1, 0).is_empty());
        assert!(!hub.is_devtools_focused(1));
        // A batch still on its way makes no ring for a page that is gone.
        hub.keep_console(1, Level::Log, ("a.js", 1), "late");
        assert!(hub.console_since(1, 0).is_empty());
    }

    #[test]
    fn the_calls_of_the_repl_are_numbered_on_for_every_window() {
        let hub = hub();
        let calls: Vec<u64> = (0..3).map(|_| hub.next_inspector_call()).collect();
        assert_eq!(calls, [1, 2, 3]);
    }

    #[test]
    fn the_inspector_is_focused_until_it_is_not() {
        let hub = hub();
        assert!(!hub.is_devtools_focused(3));
        hub.set_devtools_focused(3, true);
        assert!(hub.is_devtools_focused(3));
        hub.set_devtools_focused(3, false);
        assert!(!hub.is_devtools_focused(3));
    }
}
