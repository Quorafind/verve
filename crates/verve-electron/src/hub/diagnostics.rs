//! What the Hub says on the terminal: the app's [`Log`], and which pages' consoles go to it.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use verve::Level;

use super::{ContentsId, Hub, lock};
use crate::log::{self, Log};

impl Hub {
    /// The terminal output of this app.
    pub fn log(&self) -> &Arc<Log> {
        self.log.get_or_init(|| Arc::clone(log::process_log()))
    }

    /// Gives the app a log of its own, which a test reads. Before anything else uses the Hub: a
    /// log that has been used stays.
    pub fn set_log(&self, log: Arc<Log>) {
        // A second answer is the one the first user already got.
        let _ = self.log.set(log);
    }

    /// Page `contents` has its DevTools open, or has not: its console goes to the terminal while
    /// it does.
    pub(crate) fn set_devtools_open(&self, contents: ContentsId, open: bool) {
        let mut pages = lock(&self.devtools);
        if open {
            pages.insert(contents);
        } else {
            pages.remove(&contents);
        }
        self.any_devtools
            .store(!pages.is_empty(), Ordering::Relaxed);
    }

    /// Whether page `contents` has its DevTools open.
    pub(crate) fn is_devtools_open(&self, contents: ContentsId) -> bool {
        lock(&self.devtools).contains(&contents)
    }

    /// Writes a line of page `contents`' console to the terminal, if logging is on or its
    /// DevTools are open; Electron writes nothing otherwise.
    pub(super) fn forward_console(
        &self,
        contents: ContentsId,
        level: Level,
        at: (&str, u32),
        text: &str,
    ) {
        let log = self.log();
        let devtools = self.any_devtools.load(Ordering::Relaxed) && self.is_devtools_open(contents);
        if log.console() || devtools {
            log.console_line(contents, level, at, text);
        }
    }
}
