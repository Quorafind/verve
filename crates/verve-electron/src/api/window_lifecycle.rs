//! The end of a window, in Electron 39's order (observed on 39.8.10, Windows).
//!
//! `win.close()`, a person's click on the close button and `app.quit()` all come to
//! [`begin_close`]: the window emits `'close'`, which a listener may cancel; then the page is
//! asked for its `beforeunload`, which it may refuse — the page's `webContents` then emits
//! `'will-prevent-unload'`, and only a listener that calls `preventDefault()` there overrules the
//! page; then the page unloads (`pagehide`, `unload`) and the window is closed: its `webContents`
//! says `'destroyed'`, then the window says `'closed'`. `win.destroy()` skips all of that and says
//! `'closed'` first, then `'destroyed'`. A window closed takes its children with it, which are
//! destroyed without being asked.

use std::time::Instant;

use boa_engine::Context;

use super::windows::{CLOSE_PATIENCE, Closing, WindowRecord};
use super::{MainState, app, contents_ipc, define, navigation, shared};
use crate::ids::{ContentsId, WindowId};
use crate::msg::{ToRenderer, WindowOp};

/// The window is asked to close. A close already under way is not asked again, though its
/// `'close'` is heard once more, as Electron's is — and a listener cannot cancel that second
/// one: the round trip under way is not undone, though a quit waiting on the window is.
pub(crate) fn begin_close(context: &mut Context, window: WindowId) {
    let state = shared(context);
    let Some(record) = state.windows.live(window) else {
        return;
    };
    let prevented = define::emit_reporting(context, &record.object, "close", &[]);
    if prevented {
        if record.closing.get() == Closing::Idle {
            return refuse(context, &record);
        }
        // The round trip under way finishes, but a quit that asked this window is off. (Not
        // observed on Electron: Verve's reading of `WindowCloseCancelled`.)
        return app::quit::close_prevented(context);
    }
    ask_page(context, &record);
}

/// Asks the window's page for its `beforeunload`, unless that is under way: the page answers
/// [`before_unload_reply`]. A page that is not there closes the window at once.
pub(crate) fn ask_page(context: &mut Context, record: &WindowRecord) {
    if record.closing.get() != Closing::Idle {
        return;
    }
    let state = shared(context);
    if state.hub.has_renderer(record.page.contents) {
        let call = state.windows.next_call();
        record.closing.set(Closing::Asking(call));
        record.asked_at.set(Some(Instant::now()));
        record.page.to_renderer(ToRenderer::BeforeUnload { call });
    } else {
        finish_close(context, record);
    }
}

/// The window stays: whoever waited for it to close is told, and so is the shell.
fn refuse(context: &mut Context, record: &WindowRecord) {
    record.closing.set(Closing::Idle);
    record.asked_at.set(None);
    app::quit::close_prevented(context);
    shared(context)
        .hub
        .to_window(record.id, WindowOp::CloseAnswer { prevented: true });
}

/// The page answered `beforeunload`: `proceed` is `false` when a handler of its refused.
pub(crate) fn before_unload_reply(
    context: &mut Context,
    contents: ContentsId,
    call: u64,
    proceed: bool,
) {
    let Some(record) = shared(context).showing(contents) else {
        return;
    };
    if record.closing.get() != Closing::Asking(call) {
        return;
    }
    let overruled = !proceed && {
        let object = contents_ipc::contents_object(context, contents);
        object.is_ok_and(|object| {
            define::emit_reporting(context, &object, "will-prevent-unload", &[])
        })
    };
    if proceed || overruled {
        record.closing.set(Closing::Unloading(call));
        record.asked_at.set(Some(Instant::now()));
        record.page.to_renderer(ToRenderer::Unload { call });
    } else {
        refuse(context, &record);
    }
}

/// The page has unloaded: the window goes.
pub(crate) fn unload_reply(context: &mut Context, contents: ContentsId, call: u64) {
    if let Some(record) = shared(context).showing(contents)
        && record.closing.get() == Closing::Unloading(call)
    {
        finish_close(context, &record);
    }
}

/// The page of a window that is closing is gone: there is nothing left to ask, and the window goes.
pub(crate) fn page_gone(context: &mut Context, contents: ContentsId) {
    if let Some(record) = shared(context).showing(contents)
        && record.closing.get() != Closing::Idle
    {
        finish_close(context, &record);
    }
}

/// Closes the windows whose page has not answered a close in time: the page hangs, and the
/// window goes without it. Answers whether it closed any, for the caller to run what their
/// listeners left to do.
pub(crate) fn close_overdue(context: &mut Context) -> bool {
    let state = shared(context);
    let overdue = state.windows.overdue(Instant::now());
    for record in &overdue {
        eprintln!(
            "electron: the page of window {} did not answer a close in {:?}; closing it",
            record.id, CLOSE_PATIENCE
        );
        finish_close(context, record);
    }
    !overdue.is_empty()
}

/// The close was not refused: the page's `webContents` is destroyed, the shell closes the
/// window, and the window is closed.
pub(crate) fn finish_close(context: &mut Context, record: &WindowRecord) {
    let state = shared(context);
    if let Ok(object) = contents_ipc::contents_object(context, record.page.contents)
        && !record.contents_destroyed.replace(true)
    {
        define::emit_reporting(context, &object, "destroyed", &[]);
    }
    state
        .hub
        .to_window(record.id, WindowOp::CloseAnswer { prevented: false });
    state.window_closed(context, record.id);
}

/// `win.destroy()`: the window and its children are destroyed as far as a script can tell at once;
/// their `'closed'` follows as a task of the main process, so a script that destroyed a window
/// finishes its turn first.
pub(crate) fn destroy(context: &mut Context, window: WindowId) {
    let state = shared(context);
    let Some(record) = state.windows.live(window) else {
        return;
    };
    state.mark_dead(&record);
    state.hub.to_window(window, WindowOp::Destroy);
    state.hub.to_main(move |guest| guest.window_gone(window));
}

impl MainState {
    /// Makes `record` and its children unusable to scripts. A window already marked has had its
    /// children marked with it.
    fn mark_dead(&self, record: &WindowRecord) {
        if record.dead.replace(true) {
            return;
        }
        for child in self.windows.children_of(record.id) {
            self.mark_dead(&child);
        }
    }

    /// A window is gone: its children go first, then it — the Hub forgets it, its object hears
    /// `'closed'` — after the native window is destroyed, as in Electron — and its page's
    /// `webContents` `'destroyed'` unless that has been said. The app is then told whether it
    /// was the last. The record is forgotten first, so that a listener which ends the app, or
    /// closes the windows again, finds this one closed: a window says `'closed'` once.
    pub(crate) fn window_closed(&self, context: &mut Context, window: WindowId) {
        let Some(record) = self.windows.remove(window) else {
            return;
        };
        self.mark_dead(&record);
        self.give_up_on_page(context, &record);
        for child in self.windows.children_of(window) {
            self.hub.to_window(child.id, WindowOp::Destroy);
            self.window_closed(context, child.id);
        }
        self.enable_parent_of(&record);
        let contents = self.hub.contents_of(window);
        for opened in self.windows.opened_by(record.page.contents) {
            if !opened.page_state.outlives_opener.get() {
                self.hub.to_window(opened.id, WindowOp::Destroy);
                self.window_closed(context, opened.id);
            }
        }
        self.hub.remove_window(window);
        super::menu_window::window_gone(context, window);
        define::emit_reporting(context, &record.object, "closed", &[]);
        if let Some(contents) = contents {
            if let Ok(object) = contents_ipc::contents_object(context, contents)
                && !record.contents_destroyed.replace(true)
            {
                define::emit_reporting(context, &object, "destroyed", &[]);
            }
            contents_ipc::forget(context, contents);
        }
        app::quit::window_closed(context);
    }

    /// The window is gone: a `loadURL` still waiting fails as `ERR_FAILED (-2)` (Electron's answer
    /// for a destroyed page), and so does every script asked of the page.
    fn give_up_on_page(&self, context: &mut Context, record: &WindowRecord) {
        let url = record.page_state.target.borrow().clone();
        navigation::settle_load(context, record, Err((-2, "ERR_FAILED".into(), url)));
        self.calls.fail(context, record.page.contents);
    }

    /// A modal window blocks its parent while it can be seen: the parent takes input again once
    /// `modal` is hidden or gone.
    pub(crate) fn follow_modal(&self, modal: &WindowRecord) {
        if !modal.modal {
            return;
        }
        let blocking = !modal.dead.get() && modal.state.borrow().is_visible();
        self.enable_window(modal.parent.get(), !blocking);
    }

    fn enable_parent_of(&self, modal: &WindowRecord) {
        if modal.modal {
            self.enable_window(modal.parent.get(), true);
        }
    }

    /// Enables or disables `window`, if it exists and is not so already.
    pub(crate) fn enable_window(&self, window: Option<WindowId>, enabled: bool) {
        let Some(record) = window.and_then(|id| self.windows.get(id)) else {
            return;
        };
        let mut state = record.state.borrow_mut();
        if state.flag(crate::window_state::Flag::Enabled) != enabled {
            state.set_flag(crate::window_state::Flag::Enabled, enabled);
            self.hub.to_window(record.id, WindowOp::SetEnabled(enabled));
        }
    }
}
