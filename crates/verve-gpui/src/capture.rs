//! `--screenshot <path>`: the first window's page, drawn into a PNG once it has stopped changing,
//! and then the app ends.
//!
//! The picture is the display list drawn by the software painter ([`crate::screenshot`]) at the
//! size the window's page was last drawn at, in CSS pixels (a scale of 1, so it can be compared
//! with a `capturePage()` of Electron at `--force-device-scale-factor=1`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::AsyncApp;

use crate::screenshot;
use crate::window::Window;

/// How long the page's list has to stay the same before it is taken for finished.
const SETTLED: Duration = Duration::from_millis(600);
/// How long to wait for a window that draws at all.
const GIVE_UP: Duration = Duration::from_secs(30);

/// A screenshot that is being waited for.
pub(crate) struct Capture {
    path: PathBuf,
    started: Instant,
    /// The revision of the list last seen, and since when.
    seen: Option<(u64, Instant)>,
    /// Set when the picture could not be made, for the process to leave with 1.
    failed: Arc<AtomicBool>,
}

impl Capture {
    pub(crate) fn new(path: PathBuf, failed: Arc<AtomicBool>) -> Self {
        Self {
            path,
            started: Instant::now(),
            seen: None,
            failed,
        }
    }

    /// Looks at `window`; answers whether the picture was taken (or given up on), so the app ends.
    pub(crate) fn poll(&mut self, window: Option<&Window>, cx: &mut AsyncApp) -> bool {
        let look = window.and_then(|window| window.look(cx));
        let Some((list, viewport, background, images)) = look.filter(|look| look.1.0 > 0.) else {
            return self.give_up_after_a_while("no window drew a page");
        };
        let now = Instant::now();
        match self.seen {
            Some((revision, _)) if revision == list.revision => {}
            _ => self.seen = Some((list.revision, now)),
        }
        let settled = self.seen.is_some_and(|(_, since)| now - since >= SETTLED);
        if !settled {
            return self.give_up_after_a_while("the page kept changing");
        }
        let size = (viewport.0.round() as u32, viewport.1.round() as u32);
        let color = background.map(|c| [c.r, c.g, c.b, c.a]);
        if let Err(error) = screenshot::write(&self.path, &list, &images, size, color) {
            eprintln!("electron: no screenshot: {error}");
            self.failed.store(true, Ordering::SeqCst);
        }
        true
    }

    fn give_up_after_a_while(&self, why: &str) -> bool {
        let gave_up = self.started.elapsed() >= GIVE_UP;
        if gave_up {
            eprintln!("electron: no screenshot after {GIVE_UP:?}: {why}");
            self.failed.store(true, Ordering::SeqCst);
        }
        gave_up
    }
}
