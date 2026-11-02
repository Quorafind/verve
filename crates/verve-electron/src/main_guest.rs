//! The browser process: one isolate, running the app's own script with `require('electron')`
//! answering the browser-process API.
//!
//! Nothing here waits for anything. What the shell and the pages say arrives as jobs on this
//! isolate's thread, and is turned into the events the API's objects emit; what the script asks
//! of them leaves through the [`Hub`]. The app ends the way Electron's does — the script done,
//! nothing left to run, no window open.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use verve::isolate::{Guest, InterruptHandle, Outbox};
use verve::{ExitBehavior, Level, ProcessInfo, Runtime};

use crate::api::{self, MainState, define};
use crate::beat;
use crate::hub::Hub;
use crate::ids::{ContentsId, WindowId};
use crate::msg::{FromRenderer, FromShell, ToShell, WindowEvent, WindowOp};

/// The browser process: its runtime, the API the app's script calls, and what ends the app.
pub struct MainGuest {
    runtime: Runtime,
    hub: Arc<Hub>,
    api: Rc<MainState>,
    /// The code the app ends with when its script could not run at all.
    failed: Option<i32>,
    /// Whether the app's script has run: until it has, an empty beat is not an app with nothing
    /// left to do.
    started: bool,
}

impl MainGuest {
    /// The browser process, ready to run `entry` — the first job the Hub sends it ([`run`]),
    /// so the isolate has its interrupt handle and the Hub its handle on the isolate before the
    /// script's first line.
    ///
    /// [`run`]: MainGuest::run
    pub fn new(hub: Arc<Hub>, entry: PathBuf) -> Self {
        let mut runtime = Runtime::new();
        set_up_process(&mut runtime, &hub, &entry);
        let api = api::install_state(&mut runtime, Arc::clone(&hub));
        Self {
            runtime,
            hub,
            api,
            failed: None,
            started: false,
        }
    }

    /// Lets work finished on another thread — a socket, a worker — wake the isolate.
    pub fn set_waker(&mut self, waker: verve::Waker) {
        self.runtime.set_waker(Some(waker));
    }

    /// Runs the entry. A script that cannot be loaded, or that throws as it runs, is printed and
    /// ends the app with code 1 — what Electron does when there is no dialog to show it in. One
    /// that called `process.exit` has already said how the app ends, and is not reported.
    pub fn run(&mut self, entry: &Path) {
        self.started = true;
        let source = match std::fs::read_to_string(entry) {
            Ok(source) => source,
            Err(error) => {
                eprintln!(
                    "electron: Cannot find module '{}': {error}",
                    entry.display()
                );
                self.failed = Some(1);
                return;
            }
        };
        if let Err(error) = self.runtime.load_module(entry, &source)
            && self.hub.exit_code().is_none()
        {
            eprintln!("electron: {error}");
            self.failed = Some(1);
        }
    }

    /// What the shell saw, told to the window it is about. The app's own events, menus and the
    /// displays are P10/P14/P15's.
    pub fn from_shell(&mut self, msg: FromShell) {
        if let FromShell::Window { window, event } = msg {
            self.window_event(window, event);
        }
    }

    /// What a page said, told to the window that shows it. A page's messages are P12's, and what
    /// `webContents` reports of it is P13's.
    pub fn from_renderer(&mut self, contents: ContentsId, event: FromRenderer) {
        match event {
            FromRenderer::Dom(verve_dom::Request::Title(title)) => {
                if let Some(window) = self.hub.window_of(contents) {
                    self.hub.to_shell(ToShell::Window {
                        window,
                        op: WindowOp::SetTitle(title),
                    });
                }
            }
            // `'render-process-gone'` is `webContents`' event, P13's; until then it is said.
            FromRenderer::Gone { reason } => {
                eprintln!("electron: the page of webContents {contents} is gone: {reason:?}");
            }
            _ => {}
        }
    }

    fn window_event(&mut self, window: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                // A `'close'` listener may cancel it; P02 has no event object to
                // `preventDefault`, so the answer is always "not prevented" (P13 asks JS).
                self.emit(window, "close");
                self.hub.to_shell(ToShell::Window {
                    window,
                    op: WindowOp::CloseAnswer { prevented: false },
                });
            }
            WindowEvent::Closed => {
                // The window is gone before its `'closed'` listeners hear of it, as in Electron:
                // there the native window is already destroyed, so a listener that still holds
                // the object gets `Object has been destroyed` from any method of it. The object
                // itself stays until the listeners are done — they are what is being emitted on.
                self.hub.remove_window(window);
                self.emit(window, "closed");
                self.api.forget(window);
            }
            // Shown, focused, resized and the rest are `BrowserWindow`'s events in P13.
            _ => {}
        }
    }

    fn emit(&mut self, window: WindowId, event: &str) {
        let Some(object) = self.api.object(window) else {
            return;
        };
        if let Err(error) = define::emit(self.runtime.context(), &object, event, &[]) {
            verve::report(self.runtime.context(), &error);
        }
    }
}

/// Makes the realm's `process` the browser process's, and its console the app's own.
///
/// `process.exit` ends the app with the code it was given. Console lines go where Electron's main
/// process writes them: standard output, and standard error for warnings and errors — an error
/// nobody caught among them, after which the app goes on, as Electron's does until its dialog is
/// closed.
fn set_up_process(runtime: &mut Runtime, hub: &Arc<Hub>, entry: &Path) {
    let exit_hub = Arc::clone(hub);
    let info = ProcessInfo {
        title: "electron".into(),
        process_type: Some("browser".into()),
        argv: vec![
            ProcessInfo::default().exec_path,
            entry.display().to_string(),
        ],
        exit: ExitBehavior::Host(Arc::new(move |code| exit_hub.shutdown(code))),
        ..ProcessInfo::default()
    };
    if let Err(error) = runtime.set_process_info(info) {
        eprintln!("electron: process could not be set up: {error}");
    }
    runtime.set_console_sink(Some(Box::new(|level, text, _| match level {
        Level::Warn | Level::Error => eprintln!("{text}"),
        _ => println!("{text}"),
    })));
}

impl Guest for MainGuest {
    type View = ();
    type Event = ();

    fn tick(&mut self) {
        beat::run(&mut self.runtime);
        if let Some(code) = self.failed {
            self.hub.shutdown(code);
        } else if self.started && self.hub.windows().is_empty() && self.runtime.next_due().is_none()
        {
            self.hub.shutdown(0);
        }
    }

    fn next_due(&self) -> Option<Duration> {
        beat::next_due(&self.runtime)
    }

    fn drain(&mut self, _out: &mut Outbox<()>) {}

    fn publish(&mut self, _applied: u64, _ran: bool) -> Option<()> {
        None
    }

    fn interrupt_handle(&mut self) -> Option<InterruptHandle> {
        Some(self.runtime.interrupt_handle())
    }
}
