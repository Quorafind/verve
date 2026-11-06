//! What the threads of one Electron app say to each other, and nothing else: the main process's
//! isolate, the isolate of each rendered page, and the shell — the OS thread — that draws their
//! windows.
//!
//! These types are the contract every other package compiles against, so they live here, in one
//! module. A package that needs a variant adds it at the end of a message and records it in
//! `docs/electron/packages/P02-skeleton-and-contracts.md`.

pub use crate::ids::{ContentsId, WindowId};

/// A JavaScript value copied out of its realm, to cross to another thread: the kernel's
/// structured clone. `Clone`, because [`ToShell`] and the pages' replies are copied by whoever
/// observes them.
pub use verve::clone::Cloned;

mod dialog;
mod input;
mod page;
mod preferences;
mod shell;
mod window;

pub use dialog::*;
pub use input::*;
pub use page::*;
pub use preferences::*;
pub use shell::*;
pub use window::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_reply_answers_once_and_never_waits() {
        let (reply, page) = SyncReply::channel();
        assert!(reply.send(Cloned::undefined()));
        assert!(page.try_recv().is_ok());
    }

    #[test]
    fn a_sync_reply_to_a_page_that_left_is_not_an_error() {
        let (reply, page) = SyncReply::channel();
        drop(page);
        assert!(!reply.send(Cloned::undefined()));
    }

    #[test]
    fn a_sync_reply_dropped_unanswered_wakes_the_page() {
        let (reply, page) = SyncReply::channel();
        drop(reply);
        assert!(page.recv().is_err());
    }

    #[test]
    fn a_pages_preferences_default_as_electrons_do() {
        let prefs = WebPreferences::default();
        assert!(prefs.sandbox && prefs.web_security && prefs.javascript && prefs.dev_tools);
        assert!(!prefs.node_integration && !prefs.allow_running_insecure_content);
        assert_eq!(prefs.zoom_factor, 1.0);
    }
}
