//! The two numbers Electron gives everything: `BrowserWindow.id` and `webContents.id`, each from
//! 1, and the counter of each that an app hands out.

/// `BrowserWindow.id`: from 1, counting the windows this app made.
pub type WindowId = u32;

/// `webContents.id`: from 1 too, counted apart from the windows'.
pub type ContentsId = u32;

/// The next id of each kind. One per [`crate::Hub`] — a Hub is one app.
#[derive(Default)]
pub(crate) struct Ids {
    windows: WindowId,
    contents: ContentsId,
}

impl Ids {
    /// The next window id.
    pub(crate) fn window(&mut self) -> WindowId {
        self.windows += 1;
        self.windows
    }

    /// The next `webContents` id.
    pub(crate) fn contents(&mut self) -> ContentsId {
        self.contents += 1;
        self.contents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_counts_from_one_on_its_own() {
        let mut ids = Ids::default();
        assert_eq!(ids.window(), 1);
        assert_eq!(ids.contents(), 1);
        assert_eq!(ids.window(), 2);
    }
}
