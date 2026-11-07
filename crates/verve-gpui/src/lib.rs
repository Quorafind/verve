//! The shell: the OS's thread, the app's windows, and the wake that tells it there is news.
//!
//! The Hub is what the app's JavaScript threads use; this side owns none of them. A message
//! lands in a queue from whichever thread asked for it, the queue's owner is woken through a
//! channel of its own, and the OS thread turns what it finds into GPUI windows: so a script
//! that never returns holds its own thread and nothing else.
//!
//! The windows are [`window`]'s (one per `BrowserWindow`: frame, limits, drag areas, owner and
//! modal windows, what they report), the system's part of them [`platform`]'s, and
//! [`shell`] only says which window a message is for. What a window draws is the display list
//! `verve-layout` makes on the page's own thread ([`DomElement`](dom_element)), and the
//! [`AppRegion`]s the window is dragged by are the list's own hitboxes.

mod app_region;
mod capture;
mod dialog;
mod dom_element;
mod dom_view;
mod frame_stats;
mod menu;
mod native;
mod pictures;
mod plan;
mod platform;
mod raster;
mod report;
pub mod screenshot;
mod shell;
mod shortcut;
mod tray;
mod wheel;
mod window;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{App, QuitMode};
use verve_electron::Hub;
use verve_electron::hub::{Program, ShellPort};
use verve_electron::ids::WindowId;
use verve_electron::msg::{AppEvent, ToShell};

pub use app_region::{AppRegion, drag_area};

use crate::capture::Capture;
use crate::shell::{Inbox, Item, Shell};

/// What an embedder can do to the shell besides what the Hub sends it.
#[derive(Clone)]
pub struct Control(Inbox);

impl Control {
    /// The page of `window` has these `-webkit-app-region` boxes now.
    pub fn set_app_regions(&self, window: WindowId, regions: Vec<AppRegion>) {
        self.0.push_item(Item::Regions(window, regions));
    }

    /// The menu item that `key`, pressed in `window`, chooses by its accelerator, if any: the window's
    /// menu then the application menu, as `MenuBook::item_for` reads them.
    ///
    /// The shell does not forward keys to pages (P32's input bridge does), so the bridge calls this
    /// on every key down *before* it sends the key to the page, and, for an item found, says
    /// `FromShell::MenuClicked { menu_item, window: Some(window), triggered_by_accelerator: true,
    /// modifiers }` to the Hub and does not send the key on. A page whose `webContents` has a
    /// `'before-input-event'` listener (`Hub::intercepts_keys`) gets the key sent to the main
    /// process as `FromShell::KeyForContents` instead, which looks the menu up itself after the
    /// listeners.
    pub fn menu_item_for_key(&self, window: WindowId, key: &verve_dom::Key) -> Option<u32> {
        let book = self.0.book();
        let book = book.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        book.item_for(window, key)
    }
}

/// Runs an app: the OS thread's GPUI application, the windows the main process asks it for, and
/// the app's script, which starts once the shell is listening. Answers the code the process
/// leaves with.
pub fn run(hub: Arc<Hub>, program: Program) -> i32 {
    run_with(hub, move |hub, _| hub.start(program))
}

/// [`run`] that takes a picture of the first window's page once it stops changing, writes it to
/// `path` as a PNG and ends: `electron --screenshot <path>`. It leaves with 1 when no picture could
/// be made, whatever the app's exit code is.
pub fn run_screenshot(hub: Arc<Hub>, program: Program, path: PathBuf) -> i32 {
    run_inner(hub, move |hub, _| hub.start(program), Some(path))
}

/// [`run`] with `start` in place of the app's script: it is called once, on the OS thread, when
/// the shell is listening, and may start threads that talk to the Hub.
pub fn run_with(hub: Arc<Hub>, start: impl FnOnce(&Arc<Hub>, Control) + 'static) -> i32 {
    run_inner(hub, start, None)
}

fn run_inner(
    hub: Arc<Hub>,
    start: impl FnOnce(&Arc<Hub>, Control) + 'static,
    screenshot: Option<PathBuf>,
) -> i32 {
    let failed = Arc::new(AtomicBool::new(false));
    let capture_failed = Arc::clone(&failed);
    let (wake, woken) = async_channel::bounded(1);
    let inbox = Inbox::new(wake);
    // The system's own error box when no window exists, unless the environment says no (the
    // tests of this crate do, so none puts a box on the desktop).
    if native::touches_desktop() {
        hub.allow_system_boxes();
    }
    hub.set_shell(Arc::new(Port {
        inbox: inbox.clone(),
    }));
    let app = Arc::clone(&hub);
    // The app ends when the main process says so (`ToShell::Quit`, or the Hub's exit code), never
    // because the last window closed: GPUI's default quits then on Windows and Linux, and that
    // would end an app whose `'closed'` listener is about to open another window, with code 0.
    let application = gpui_platform::application().with_quit_mode(QuitMode::Explicit);
    let reopened = Arc::clone(&hub);
    application.on_reopen(move |cx| {
        let has_visible_windows = !cx.windows().is_empty();
        let event = AppEvent::Activate {
            has_visible_windows,
        };
        reopened.from_shell(verve_electron::msg::FromShell::App(event));
    });
    let opened = Arc::clone(&hub);
    application.on_open_urls(move |urls| {
        for url in urls {
            let event = match file_of(&url) {
                Some(path) => AppEvent::OpenFile(path),
                None => AppEvent::OpenUrl(url),
            };
            opened.from_shell(verve_electron::msg::FromShell::App(event));
        }
    });
    application.run(move |cx: &mut App| {
        let mut shell = Shell::new(Arc::clone(&app), inbox.clone());
        if let Some(path) = screenshot {
            shell.capture(Capture::new(path, capture_failed));
            // Nothing wakes the shell when a page has been still for long enough.
            wake_every(&inbox, Duration::from_millis(150), cx);
        }
        let pump = inbox.clone();
        cx.spawn(async move |cx| {
            let mut cx = cx.clone();
            while woken.recv().await.is_ok() {
                shell.process(&mut cx);
            }
        })
        .detach();
        let displays = inbox.clone();
        cx.observe_displays(move |_, _| displays.push_item(Item::Displays))
            .detach();
        quit_through_the_main_process(&app, cx);
        pump.push_item(Item::Displays);
        start(&app, Control(inbox));
    });
    // A picture that could not be made is a failure, whatever the app's own code says.
    if failed.load(Ordering::SeqCst) {
        return 1;
    }
    hub.exit_code().unwrap_or(0)
}

/// Wakes the shell every `period`, for what it waits for without anything telling it.
fn wake_every(inbox: &Inbox, period: Duration, cx: &mut App) {
    let waker = inbox.clone();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(period).await;
            waker.wake();
        }
    })
    .detach();
}

/// A system that asks the app to quit (a shutdown, a Cmd+Q the menu did not take) is told to the
/// main process as `app.quit()`, which asks the windows and says `will-quit`.
///
/// GPUI calls this observer, then drops every window, then waits `SHUTDOWN_TIMEOUT` (200 ms) for
/// the futures it returned without ever polling this shell's pump, and on Windows leaves with
/// code 0 after `WM_ENDSESSION`. So the message is sent at once and nothing waits. GPUI's
/// `WindowsWindow::drop` only hides a window and leaves its destruction to a task that never
/// runs, so no window says `Closed` on this path either: the main process hears of the quit and
/// of nothing after it, and what it would do in answer (a `before-quit` veto, an exit code) is
/// lost. Catching `WM_QUERYENDSESSION` before GPUI shuts down is the way to more, and is not done.
fn quit_through_the_main_process(hub: &Arc<Hub>, cx: &mut App) {
    let hub = Arc::clone(hub);
    cx.on_app_quit(move |_| {
        if hub.exit_code().is_none() {
            hub.from_shell(verve_electron::msg::FromShell::App(AppEvent::QuitRequested));
        }
        std::future::ready(())
    })
    .detach();
}

/// The path a `file://` URL names, or `None` for any other address (and for a host that is not
/// this machine, off Windows, where it is a UNC share).
fn file_of(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let (host, path) = rest.split_at(rest.find('/')?);
    let path = String::from_utf8(verve::encoding::percent_decode(path)).ok()?;
    match host {
        "" | "localhost" => {
            // A drive letter's path begins `/C:/`: the slash is the URL's.
            let drive = path.as_bytes().get(2) == Some(&b':') && cfg!(windows);
            Some(
                path.strip_prefix('/')
                    .filter(|_| drive)
                    .unwrap_or(&path)
                    .into(),
            )
        }
        _ if cfg!(windows) => Some(format!("//{host}{path}").into()),
        _ => None,
    }
}

/// The Hub's end of the shell: messages land in the shell's queue from whichever thread sent
/// them, and the OS thread is woken to take them.
struct Port {
    inbox: Inbox,
}

impl ShellPort for Port {
    fn send(&self, msg: ToShell) {
        self.inbox.queue(msg);
    }

    fn wake(&self) {
        self.inbox.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::file_of;

    #[test]
    fn a_file_url_names_its_decoded_path_and_other_urls_name_none() {
        let path = file_of("file:///tmp/a%20b.txt").expect("a path");
        assert!(path.ends_with("a b.txt"));
        assert!(file_of("https://example.com/").is_none());
        let share = file_of("file://server/share/x");
        assert_eq!(share.is_some(), cfg!(windows));
        assert!(share.is_none_or(|path| path.to_string_lossy().ends_with("share/x")));
    }
}
