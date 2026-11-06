//! How the app ends, in Electron 39's order (observed on 39.8.10, Windows):
//!
//! `app.quit()` emits `before-quit`, then asks every window to close — each may refuse, which
//! cancels the quit — and when the last is gone emits `will-quit`; when that is not prevented
//! the process's `exit` is heard, then `quit`, and the process ends with code 0. `app.exit(code)`
//! and `process.exit(code)` skip the questions: the windows are destroyed (`closed`, no
//! `close`), `exit` and `quit` follow. A window closing on its own that was the last one ends the
//! app unless a `window-all-closed` listener takes the decision.

use std::path::Path;
use std::process::{Command, Stdio};

use boa_engine::{Context, JsValue};
use verve::native::{self, js_text};

use super::{emit_quietly, is_heard};
use crate::api::{define, shared, window_lifecycle};

/// Where the app is in ending. A listener may call `app.quit()` or `app.exit()` while an event
/// is being emitted, so the state is entered before the emit and read again after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// Running: the app has not been asked to quit.
    Running,
    /// `before-quit` is being heard.
    BeforeQuit,
    /// The windows were asked to close; `will-quit` waits for the last of them.
    ClosingWindows,
    /// `will-quit` is being heard.
    WillQuit,
    /// The app is ending with this code; nothing more is emitted, and nothing leaves this state.
    Exiting(i32),
}

/// `app.quit()`. A quit already under way is left to finish: asked again, nothing is emitted.
/// A listener that throws is reported and has not prevented anything, as in Electron.
pub(crate) fn quit(context: &mut Context) {
    let state = shared(context);
    if state.app.quit.get() != Ending::Running {
        return;
    }
    state.app.quit.set(Ending::BeforeQuit);
    let prevented = emit_quietly(context, "before-quit", &[]);
    if state.app.quit.get() != Ending::BeforeQuit {
        return;
    }
    if prevented {
        state.app.quit.set(Ending::Running);
        return;
    }
    let windows = state.hub.windows();
    if windows.is_empty() {
        return will_quit(context);
    }
    state.app.quit.set(Ending::ClosingWindows);
    // Every window is asked, in this call, as Electron's `CloseAllWindows` does; one that refuses
    // (a `'close'` listener, or a page that keeps its `beforeunload`) cancels the quit through
    // `close_prevented`, and the others close all the same.
    for window in windows {
        window_lifecycle::begin_close(context, window);
    }
}

/// A window refused to close (its `'close'` listener called `preventDefault()`, or its page kept
/// the `beforeunload`): a quit that was waiting on it is off.
pub(crate) fn close_prevented(context: &mut Context) {
    let quit = &shared(context).app.quit;
    if quit.get() == Ending::ClosingWindows {
        quit.set(Ending::Running);
    }
}

/// A window is gone and its `'closed'` listeners have run: if it was the last, the quit that
/// waited for it goes on, or the app is told that no window is left.
pub(crate) fn window_closed(context: &mut Context) {
    let state = shared(context);
    if !state.hub.windows().is_empty() {
        return;
    }
    match state.app.quit.get() {
        Ending::ClosingWindows => will_quit(context),
        // After what a `'closed'` listener queued — a `process.exit` — has run.
        Ending::Running if state.app.is_ready() => state.hub.to_main(|guest| guest.all_closed()),
        _ => {}
    }
}

/// The last window is gone and nothing has ended the app meanwhile: `window-all-closed`, or the
/// quit it stands for when nobody listens.
pub(crate) fn all_closed(context: &mut Context) {
    let state = shared(context);
    if state.app.quit.get() != Ending::Running || !state.hub.windows().is_empty() {
        return;
    }
    if is_heard(context, "window-all-closed") {
        emit_quietly(context, "window-all-closed", &[]);
    } else {
        quit(context);
    }
}

fn will_quit(context: &mut Context) {
    let state = shared(context);
    state.app.quit.set(Ending::WillQuit);
    let prevented = emit_quietly(context, "will-quit", &[]);
    if state.app.quit.get() != Ending::WillQuit {
        return;
    }
    if prevented {
        state.app.quit.set(Ending::Running);
    } else {
        finish(context, 0, true);
    }
}

/// The end: windows destroyed, `process`'s `exit` heard (unless `process.exit` already told it),
/// the app's `quit`, the app started again if it asked, and the process ended with `code`.
pub(crate) fn finish(context: &mut Context, code: i32, emit_exit: bool) {
    let state = shared(context);
    if matches!(
        state.app.quit.replace(Ending::Exiting(code)),
        Ending::Exiting(_)
    ) {
        return;
    }
    for window in state.hub.windows() {
        state.window_closed(context, window);
    }
    if emit_exit {
        let process = native::global(context, "process").unwrap_or_default();
        let heard = native::invoke(&process, "emit", &[js_text("exit"), code.into()], context);
        define::report(context, heard);
    }
    emit_quietly(context, "quit", &[JsValue::from(code)]);
    // The lock goes first: the program started again may be this app, asking for it.
    state.app.single.borrow_mut().take();
    start_again(context);
    // The script that asked goes on to the end of its turn, as it does in Electron; the process
    // ends after it, not in the middle of it. A quit in the entry waits for `ready` too, which
    // Electron still emits (observed on 39.8.10: `will-finish-launching`, `ready`, then the end).
    if state.app.is_ready() {
        let hub = std::sync::Arc::clone(&state.hub);
        state.hub.to_main(move |_| hub.shutdown(code));
    } else {
        state.app.pending_exit.set(Some(code));
    }
}

/// Starts the app again if `app.relaunch()` asked: the same executable with the same arguments,
/// or the ones it was given. It is not waited for.
fn start_again(context: &mut Context) {
    let state = shared(context);
    let Some(again) = state.app.relaunch.borrow_mut().take() else {
        return;
    };
    let exec = again.exec_path.unwrap_or_else(crate::launcher::exe_path);
    let args = again
        .args
        .unwrap_or_else(|| state.hub.launch().argv.clone());
    spawn_detached(&exec, &args);
}

fn spawn_detached(exec: &Path, args: &[String]) {
    let mut command = Command::new(exec);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Err(error) = command.spawn() {
        eprintln!("electron: could not relaunch {}: {error}", exec.display());
    }
}
