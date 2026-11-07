//! The text measurer the app's pages are laid out with.

use std::sync::Arc;

use verve_layout::{HeadlessMeasurer, TextMeasurer};

use std::sync::atomic::Ordering;

use super::{Hub, lock};

impl Hub {
    /// Sets the measurer every page made from now on lays its text out with: the shell's, over
    /// its text system. It has to be set before any page is made: a page laid out with the
    /// headless measurer has font keys no window's text system knows, and drawing them would
    /// index past its fonts on the GPUI thread.
    ///
    /// # Panics
    ///
    /// When a page has already been made.
    pub fn set_text_measurer(&self, measurer: Arc<dyn TextMeasurer>) {
        assert!(
            !self.measurer_taken.load(Ordering::Acquire),
            "the text measurer is set before the first page is made"
        );
        *lock(&self.measurer) = Some(measurer);
    }

    /// The measurer for a new page: the shell's, or the headless one when there is no shell.
    pub(crate) fn text_measurer(&self) -> Arc<dyn TextMeasurer> {
        self.measurer_taken.store(true, Ordering::Release);
        lock(&self.measurer)
            .clone()
            .unwrap_or_else(|| Arc::new(HeadlessMeasurer))
    }
}
