//! What the main process keeps for each window it made: the object a script holds, the page it
//! shows, the cached [state](crate::window_state) its getters answer from, and where it is in
//! closing and in saying `ready-to-show`.
//!
//! The records live from `new BrowserWindow` until the window's `'closed'` listeners have run. A
//! record whose window is `dead` is already destroyed as far as a script can tell — every method of
//! the object throws — though its `'closed'` is still to be heard.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use boa_engine::JsObject;

use super::page_state::PageState;
use crate::hub::RendererHandle;
use crate::ids::WindowId;
use crate::window_state::WindowState;

/// Where a window is in the round trip that closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Closing {
    /// Not closing, or `'close'` was heard and nothing is waited for.
    Idle,
    /// The page was asked for its `beforeunload`; this is the call it answers.
    Asking(u64),
    /// The page was told to unload; this is the call it answers.
    Unloading(u64),
}

/// How long a window waits for its page to answer a close before closing without it, as
/// Chromium does for a page that hangs (a script that never returns, a `sendSync` nobody answers).
pub(crate) const CLOSE_PATIENCE: Duration = Duration::from_secs(3);

/// What has to hold before `ready-to-show` is said, once.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ready {
    /// The page has something to paint.
    pub paintable: bool,
    /// The page has finished loading.
    pub finished: bool,
    /// The shell drew a frame of the window while it was visible.
    pub painted: bool,
    /// The window has been visible at some time.
    pub shown: bool,
    /// It was said.
    pub said: bool,
}

/// One window.
pub(crate) struct WindowRecord {
    pub id: WindowId,
    pub object: JsObject,
    pub page: RendererHandle,
    pub state: RefCell<WindowState>,
    /// `paintWhenInitiallyHidden`.
    pub paint_when_hidden: bool,
    /// Whether the window is modal to its parent.
    pub modal: bool,
    pub parent: Cell<Option<WindowId>>,
    /// Destroyed, or closing for good: scripts can no longer use it.
    pub dead: Cell<bool>,
    pub closing: Cell<Closing>,
    /// When the page was first asked about closing: a page that has not answered within
    /// [`CLOSE_PATIENCE`] is not going to.
    pub asked_at: Cell<Option<Instant>>,
    /// Whether the page's `webContents` has said `'destroyed'`.
    pub contents_destroyed: Cell<bool>,
    pub ready: Cell<Ready>,
    /// The page: where it is, its history and the load it is in.
    pub page_state: PageState,
    /// `accessibleTitle`, when the window's own is not the title.
    pub accessible_title: RefCell<Option<String>>,
}

/// The windows of the app, by id, which are also their order of creation.
#[derive(Default)]
pub(crate) struct Windows {
    records: RefCell<BTreeMap<WindowId, Rc<WindowRecord>>>,
    calls: Cell<u64>,
}

impl Windows {
    pub(crate) fn insert(&self, record: WindowRecord) {
        self.records.borrow_mut().insert(record.id, Rc::new(record));
    }

    /// The record of `window`, until it is forgotten.
    pub(crate) fn get(&self, window: WindowId) -> Option<Rc<WindowRecord>> {
        self.records.borrow().get(&window).cloned()
    }

    /// The record of `window` while a script can still use it.
    pub(crate) fn live(&self, window: WindowId) -> Option<Rc<WindowRecord>> {
        self.get(window).filter(|record| !record.dead.get())
    }

    /// Forgets `window`, answering its record; a window forgotten is not closed again.
    pub(crate) fn remove(&self, window: WindowId) -> Option<Rc<WindowRecord>> {
        self.records.borrow_mut().remove(&window)
    }

    /// Whether `window` is none, or is gone as far as a script can tell: what `isDestroyed()`
    /// answers.
    pub(crate) fn is_gone(&self, window: Option<WindowId>) -> bool {
        window.is_none_or(|window| self.live(window).is_none())
    }

    /// Whether `window` is `other` or has it among its ancestors. A chain longer than the
    /// windows there are is a loop, and ends the walk.
    pub(crate) fn descends_from(&self, window: WindowId, other: WindowId) -> bool {
        let mut at = Some(window);
        for _ in 0..=self.records.borrow().len() {
            let Some(id) = at else {
                return false;
            };
            if id == other {
                return true;
            }
            at = self.get(id).and_then(|record| record.parent.get());
        }
        false
    }

    /// The windows a script can use, the newest first: the order `getAllWindows()` answers in.
    pub(crate) fn newest_first(&self) -> Vec<Rc<WindowRecord>> {
        let records = self.records.borrow();
        let live = records.values().rev();
        live.filter(|record| !record.dead.get()).cloned().collect()
    }

    /// The windows whose parent is `window`, the newest first, destroyed ones included: they
    /// are still to be closed with it.
    pub(crate) fn children_of(&self, window: WindowId) -> Vec<Rc<WindowRecord>> {
        let records = self.records.borrow();
        let own = records.values().rev();
        own.filter(|r| r.parent.get() == Some(window))
            .cloned()
            .collect()
    }

    /// The windows whose page has had [`CLOSE_PATIENCE`] to answer a close and has not.
    pub(crate) fn overdue(&self, now: Instant) -> Vec<Rc<WindowRecord>> {
        let records = self.records.borrow();
        let due = |r: &&Rc<WindowRecord>| {
            let asked = r.asked_at.get();
            r.closing.get() != Closing::Idle && asked.is_some_and(|at| now >= at + CLOSE_PATIENCE)
        };
        records.values().filter(due).cloned().collect()
    }

    /// When the first page still asked about closing runs out of patience.
    pub(crate) fn next_overdue(&self) -> Option<Instant> {
        let records = self.records.borrow();
        let waiting = records
            .values()
            .filter(|r| r.closing.get() != Closing::Idle);
        waiting
            .filter_map(|r| r.asked_at.get())
            .min()
            .map(|at| at + CLOSE_PATIENCE)
    }

    /// A number for a call to a page that has not been used.
    pub(crate) fn next_call(&self) -> u64 {
        self.calls.set(self.calls.get() + 1);
        self.calls.get()
    }
}
