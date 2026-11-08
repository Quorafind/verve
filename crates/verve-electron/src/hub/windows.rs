//! The registry of open windows and the page each shows.

use super::{ContentsId, Hub, WindowId, lock, stop};
use crate::msg::{ToShell, WindowOp};

impl Hub {
    /// The ids of a window about to be made and of the page it shows.
    pub fn new_window_ids(&self) -> (WindowId, ContentsId) {
        let mut ids = lock(&self.ids);
        (ids.window(), ids.contents())
    }

    /// Records a window the main process made, with the page it shows.
    pub fn add_window(&self, window: WindowId, contents: ContentsId) {
        lock(&self.windows).insert(window, contents);
    }

    /// Asks the shell to do `op` to `window`.
    pub fn to_window(&self, window: WindowId, op: WindowOp) {
        self.to_shell(ToShell::Window { window, op });
    }

    /// The windows still open.
    pub fn windows(&self) -> Vec<WindowId> {
        lock(&self.windows).keys().copied().collect()
    }

    /// The page `window` shows, while the window is open.
    pub fn contents_of(&self, window: WindowId) -> Option<ContentsId> {
        lock(&self.windows).get(&window).copied()
    }

    /// The window that shows page `contents`, while it is open.
    pub fn window_of(&self, contents: ContentsId) -> Option<WindowId> {
        lock(&self.windows)
            .iter()
            .find(|(_, shown)| **shown == contents)
            .map(|(window, _)| *window)
    }

    /// A window the main process is done with, and the page it showed: the page's script is
    /// interrupted and its isolate dropped, which ends its thread. Called on the main process's
    /// thread after the window's `'closed'` listeners ran, so a listener that made another
    /// window has made it before the app asks whether any is left.
    pub fn remove_window(&self, window: WindowId) {
        let Some(contents) = lock(&self.windows).remove(&window) else {
            return;
        };
        let page = lock(&self.renderers).remove(&contents);
        if let Some(page) = page {
            stop(&page);
        }
        self.forget_inspector(contents);
        self.set_devtools_open(contents, false);
    }
}
