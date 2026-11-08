//! What the Hub keeps for a test to read once asked: console lines and pages' lifecycle events.

use super::{ContentsId, Hub, PageEvent, lock};

/// A line a script wrote to its console, as [`Hub::console_lines`] keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleLine {
    /// The page that wrote it; `None` is the main process.
    pub contents: Option<ContentsId>,
    /// The line, formatted as the console prints it.
    pub text: String,
}

impl Hub {
    /// Starts keeping every console line the app writes, from now on.
    pub fn record_console(&self) {
        lock(&self.console).get_or_insert_with(Vec::new);
    }

    /// Starts keeping every lifecycle event the pages report, from now on.
    pub fn record_page_events(&self) {
        lock(&self.page_events).get_or_insert_with(Vec::new);
    }

    /// Notes a lifecycle event, if they are being kept.
    pub(super) fn log_page_event(&self, contents: ContentsId, event: &PageEvent) {
        let log = self.log();
        crate::log::diag!(log, Lifecycle, Info, "page {contents}: {event:?}");
        if let Some(kept) = lock(&self.page_events).as_mut() {
            kept.push((contents, event.clone()));
        }
    }

    /// The lifecycle events page `contents` reported since [`Hub::record_page_events`], in order.
    pub fn page_events(&self, contents: ContentsId) -> Vec<PageEvent> {
        let kept = lock(&self.page_events);
        let all = kept.iter().flatten();
        all.filter(|(page, _)| *page == contents)
            .map(|(_, event)| event.clone())
            .collect()
    }

    /// Notes a console line, if they are being kept.
    pub(crate) fn log_console(&self, contents: Option<ContentsId>, text: &str) {
        if let Some(lines) = lock(&self.console).as_mut() {
            lines.push(ConsoleLine {
                contents,
                text: text.to_owned(),
            });
        }
    }

    /// The console lines kept since [`Hub::record_console`], in the order they were written.
    pub fn console_lines(&self) -> Vec<ConsoleLine> {
        lock(&self.console).clone().unwrap_or_default()
    }
}
