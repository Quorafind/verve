//! What the shell and the page say, heard as the events of a `BrowserWindow` and its
//! `webContents`: each of the shell's [`WindowEvent`]s corrects the window's cached state and
//! becomes the event of its name, the page's title becomes `'page-title-updated'` and its first paint `'ready-to-show'`
//! (the rest of a page's life is [`page_events`](super::page_events)).
//!
//! The order of what is emitted is Electron 39.8.10's (observed on Windows): a window's `'focus'`
//! is heard by the app's `'browser-window-focus'` first; `'page-title-updated'` is the window's,
//! whose listener may keep the title, and then the `webContents`'; and `'ready-to-show'` follows
//! the page's `'did-stop-loading'`.

use boa_engine::Context;
use verve::native::js_text;

use super::define;
use super::window_lifecycle::begin_close;
use super::windows::WindowRecord;
use super::{app, contents_ipc, shared};
use crate::ids::WindowId;
use crate::msg::{WindowEvent, WindowOp};

/// What the shell says happened to `window`.
pub(crate) fn shell_event(context: &mut Context, window: WindowId, event: WindowEvent) {
    let state = shared(context);
    match event {
        WindowEvent::CloseRequested => return begin_close(context, window),
        WindowEvent::Closed => return state.window_closed(context, window),
        _ => {}
    }
    let Some(record) = state.windows.live(window) else {
        return;
    };
    record.state.borrow_mut().observe(&event);
    state.follow_modal(&record);
    if matches!(event, WindowEvent::FirstPaint) {
        update_ready(&record, |ready| ready.painted = true);
    }
    if let Some(name) = event_name(&event) {
        announce(context, &record, name);
    }
    ready_to_show_if_due(context, &record);
}

/// The event a window says for what the shell reported, if it says one.
fn event_name(event: &WindowEvent) -> Option<&'static str> {
    Some(match event {
        WindowEvent::Shown => "show",
        WindowEvent::Hidden => "hide",
        WindowEvent::Focus => "focus",
        WindowEvent::Blur => "blur",
        WindowEvent::Resized { .. } => "resize",
        WindowEvent::Moved { .. } => "move",
        WindowEvent::Maximized => "maximize",
        WindowEvent::Unmaximized => "unmaximize",
        WindowEvent::Minimized => "minimize",
        WindowEvent::Restored => "restore",
        WindowEvent::EnterFullScreen => "enter-full-screen",
        WindowEvent::LeaveFullScreen => "leave-full-screen",
        _ => return None,
    })
}

/// Emits `name` on the window, after the app heard of a window gaining or losing the focus.
fn announce(context: &mut Context, record: &WindowRecord, name: &str) {
    let object = record.object.clone();
    if let Some(app_event) = match name {
        "focus" => Some("browser-window-focus"),
        "blur" => Some("browser-window-blur"),
        _ => None,
    } {
        app::emit_quietly(context, app_event, &[object.clone().into()]);
    }
    define::emit_reporting(context, &object, name, &[]);
}

/// The page named its title: the window says `'page-title-updated'` and, unless a listener
/// prevents it, takes the title; the page's `webContents` says it too.
pub(crate) fn page_title(context: &mut Context, record: &WindowRecord, title: &str) {
    let shared = shared(context);
    let args = [js_text(title), true.into()];
    let prevented = define::emit_reporting(context, &record.object, "page-title-updated", &args);
    if !prevented {
        record.state.borrow_mut().title = title.to_owned();
        shared
            .hub
            .to_window(record.id, WindowOp::SetTitle(title.to_owned()));
    }
    if let Ok(object) = contents_ipc::contents_object(context, record.page.contents) {
        define::emit_reporting(context, &object, "page-title-updated", &args);
    }
}

/// Changes the conditions `ready-to-show` waits for.
pub(crate) fn update_ready(record: &WindowRecord, change: impl FnOnce(&mut super::windows::Ready)) {
    let mut ready = record.ready.get();
    change(&mut ready);
    record.ready.set(ready);
}

/// Says `'ready-to-show'`, once, when the page has something to paint and has finished loading,
/// and the shell has drawn it if the window can be seen. A window made hidden with
/// `paintWhenInitiallyHidden: false` waits until it is shown.
pub(crate) fn ready_to_show_if_due(context: &mut Context, record: &WindowRecord) {
    let visible = record.state.borrow().is_visible();
    update_ready(record, |ready| ready.shown |= visible);
    let ready = record.ready.get();
    let held = !record.paint_when_hidden && !ready.shown;
    if ready.said || held || !(ready.paintable && ready.finished) || (visible && !ready.painted) {
        return;
    }
    update_ready(record, |ready| ready.said = true);
    let heard = define::emit(context, &record.object, "ready-to-show", &[]);
    define::report(context, heard);
}

/// An `'always-on-top-changed'`-style event with the flag: `(event, flag)`.
pub(crate) fn flag_changed(context: &mut Context, record: &WindowRecord, name: &str, flag: bool) {
    define::emit_reporting(context, &record.object, name, &[flag.into()]);
}
