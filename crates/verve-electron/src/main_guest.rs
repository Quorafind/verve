//! The browser process: one isolate, running the app's own script with `require('electron')`
//! answering the browser-process API.
//!
//! Nothing here waits for anything. What the shell and the pages say arrives as jobs on this
//! isolate's thread, and is turned into the events the API's objects emit; what the script asks
//! of them leaves through the [`Hub`]. The app ends the way Electron's does: when it is told to
//! (`app.quit()`, `app.exit()`, `process.exit()`, the last window closing with nobody to say
//! otherwise) — an idle main process with no window stays up, waiting for events.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use verve::isolate::{Guest, InterruptHandle, Outbox};
use verve::native;
use verve::{Level, ProcessInfo, Runtime, UncaughtBehavior};

use crate::api::{self, MainState, app, define, window_events, window_lifecycle};
use crate::beat;
use crate::hub::{Hub, Program};
use crate::ids::{ContentsId, WindowId};
use crate::launcher::Target;
use crate::msg::{
    AppEvent, FromRenderer, FromShell, LoadOptions, PageEvent, ToRenderer, WindowEvent,
};
use crate::probe;

/// The browser process: its runtime, the API the app's script calls, and what ends the app.
pub struct MainGuest {
    runtime: Runtime,
    hub: Arc<Hub>,
    api: Rc<MainState>,
    /// The code the app ends with when its script could not run at all.
    failed: Option<i32>,
    /// When the collector next looks for `sendSync` events let go without an answer.
    sweep_at: Option<Instant>,
}

impl MainGuest {
    /// The browser process, ready to run `program` — the first job the Hub sends it ([`run`]),
    /// so the isolate has its interrupt handle and the Hub its handle on the isolate before the
    /// script's first line.
    ///
    /// [`run`]: MainGuest::run
    pub fn new(hub: Arc<Hub>) -> Self {
        let mut runtime = Runtime::new();
        set_up_process(&mut runtime, &hub);
        runtime.set_vfs(hub.overlay());
        let api = api::install_state(&mut runtime, Arc::clone(&hub));
        if let Err(error) = api::process::install(runtime.context()) {
            eprintln!("electron: process could not be set up: {error}");
        }
        Self {
            runtime,
            hub,
            api,
            failed: None,
            sweep_at: None,
        }
    }

    /// Lets work finished on another thread — a socket, a worker — wake the isolate.
    pub fn set_waker(&mut self, waker: verve::Waker) {
        api::message_port_main::set_waker(self.runtime.context(), waker.clone());
        self.runtime.set_waker(Some(waker));
    }

    /// Runs the program, then tells the app it has finished launching. A script that cannot be
    /// found ends the app with code 1, as Electron's `MODULE_NOT_FOUND` does; one that throws is
    /// reported the way an uncaught error is (to an `uncaughtException` listener, else the error
    /// box) and the app goes on, as Electron's does.
    pub fn run(&mut self, program: Program) {
        self.emit_process("loaded");
        match program {
            Program::Script(entry) => self.run_script(&entry),
            Program::DefaultApp(url) => {
                if let Err(error) = self.run_default_app(url) {
                    eprintln!("electron: {error}");
                }
            }
        }
        app::will_finish_launching(self.runtime.context());
    }

    fn run_script(&mut self, entry: &Path) {
        for module in self.hub.launch().requires.clone() {
            if let Err(error) = self.runtime.require(&module) {
                return self.cannot_start(&error);
            }
        }
        let source = match self.hub.read_text(entry) {
            Ok(source) => source,
            Err(error) => {
                let message = format!("Cannot find module '{}': {error}", entry.display());
                return self.cannot_start(&message);
            }
        };
        if let Err(error) = self.runtime.run_module(entry, &source) {
            eprintln!("App threw an error during load");
            let context = self.runtime.context();
            let missing = error_code(context, &error).as_deref() == Some("MODULE_NOT_FOUND");
            verve::report(context, &error);
            if missing {
                self.failed = Some(1);
            }
        }
    }

    /// The app cannot start at all: said on standard error and in the error box, and it ends with 1.
    fn cannot_start(&mut self, message: &str) {
        eprintln!(
            "App threw an error during load
{message}"
        );
        self.hub.error_box(
            "A JavaScript error occurred in the main process".into(),
            format!(
                "Uncaught Exception:
{message}"
            ),
        );
        self.failed = Some(1);
    }

    /// Electron's default app: a window, on `url` or on the welcome page.
    fn run_default_app(&mut self, url: Option<String>) -> Result<(), String> {
        let url = url.unwrap_or_else(crate::launcher::welcome_page_url);
        let electron = self.runtime.require("electron")?;
        let context = self.runtime.context();
        let open = |context: &mut boa_engine::Context| -> boa_engine::JsResult<()> {
            let class = native::get(&electron, "BrowserWindow", context)?;
            let options =
                define::record(context, &[("width", 800.into()), ("height", 600.into())])?;
            let window = class
                .as_constructor()
                .ok_or_else(|| native::type_error("BrowserWindow is not a constructor"))?
                .construct(&[options.into()], None, context)?;
            native::invoke(&window.into(), "loadURL", &[native::js_text(&url)], context)?;
            Ok(())
        };
        open(context).map_err(|error| error.to_string())
    }

    fn emit_process(&mut self, event: &str) {
        let context = self.runtime.context();
        let process = native::global(context, "process").unwrap_or_default();
        let heard = native::invoke(&process, "emit", &[native::js_text(event)], context);
        define::report(context, heard);
    }

    pub(crate) fn all_closed(&mut self) {
        app::quit::all_closed(self.runtime.context());
    }

    /// Evaluates `code` in the main process's realm, answering the result as JSON.
    pub fn eval(&mut self, code: &str) -> Result<String, String> {
        probe::evaluate(&mut self.runtime, code)
    }

    /// What the shell saw, told to the window it is about. The app's own events, menus and the
    /// displays are P10/P14/P15's.
    pub fn from_shell(&mut self, msg: FromShell) {
        match msg {
            FromShell::Window { window, event } => self.window_event(window, event),
            FromShell::App(AppEvent::Ready) => app::ready(self.runtime.context()),
            FromShell::App(AppEvent::SecondInstance { argv, cwd, data }) => {
                app::second_instance(self.runtime.context(), &argv, &cwd, &data);
            }
            FromShell::App(event) => app::platform_event(self.runtime.context(), event),
            _ => {}
        }
    }

    /// What a page said, told to the window that shows it. What `webContents` reports of a page's
    /// lifecycle is P13's second half (the window's title and `ready-to-show` are heard now); its
    /// messages are heard as events at once, each with the microtask checkpoint a task of
    /// Electron's main process ends with.
    pub fn from_renderer(&mut self, contents: ContentsId, event: FromRenderer) {
        match event {
            FromRenderer::Ipc { .. }
            | FromRenderer::Invoke { .. }
            | FromRenderer::SendSync { .. }
            | FromRenderer::Post { .. } => {
                api::ipc_main::receive(self.runtime.context(), contents, event);
                self.runtime.run_microtasks();
                self.sweep_at = api::ipc_main::watch_unanswered(self.runtime.context());
            }
            // The window is titled by its page, as in Electron, unless a listener of the window's
            // 'page-title-updated' says otherwise.
            FromRenderer::Dom(verve_dom::Request::Title(title)) => {
                self.page_event(contents, PageEvent::TitleUpdated(title));
            }
            FromRenderer::Lifecycle(event) => self.page_event(contents, event),
            FromRenderer::BeforeUnloadReply { call, proceed } => {
                let context = self.runtime.context();
                window_lifecycle::before_unload_reply(context, contents, call, proceed);
                self.runtime.run_microtasks();
            }
            FromRenderer::UnloadReply { call } => {
                window_lifecycle::unload_reply(self.runtime.context(), contents, call);
                self.runtime.run_microtasks();
            }
            // A page that goes to another address (`location.href`, a link, a form) asks the
            // main process, which lets it.
            FromRenderer::Reload { url } => self.page_reloads(contents, url),
            FromRenderer::Dom(verve_dom::Request::Navigate { url, .. }) => {
                self.page_navigates(contents, url);
            }
            // `'render-process-gone'` is `webContents`' event, P13's second half; a window that was
            // closing and has nobody left to ask closes.
            FromRenderer::Gone { reason } => {
                eprintln!("electron: the page of webContents {contents} is gone: {reason:?}");
                window_lifecycle::page_gone(self.runtime.context(), contents);
            }
            _ => {}
        }
    }

    /// Page `contents` asks to go to `url`: the main process's say in it. P13 wraps this in
    /// `will-frame-navigate` and `will-navigate`, which a listener may cancel; until then every
    /// navigation goes through, and the page is given a document of its own.
    fn page_navigates(&mut self, contents: ContentsId, url: String) {
        self.load_for_page(contents, url);
    }

    /// Page `contents` asks to be loaded again (`location.reload()`): no `will-navigate`, which
    /// P13 leaves out for a reload as Electron does, and a document of its own all the same.
    fn page_reloads(&mut self, contents: ContentsId, url: String) {
        self.load_for_page(contents, url);
    }

    /// Has page `contents` load `url` in a document of its own.
    fn load_for_page(&mut self, contents: ContentsId, url: String) {
        let load = ToRenderer::Load {
            url,
            options: LoadOptions::default(),
        };
        self.hub.to_renderer(contents, load);
    }

    /// What a page's lifecycle says, heard as the events of its window and `webContents`.
    fn page_event(&mut self, contents: ContentsId, event: PageEvent) {
        window_events::page_event(self.runtime.context(), contents, event);
        self.runtime.run_microtasks();
    }

    /// A window `destroy()` was called on is gone: its `'closed'` is heard, after the turn of the
    /// script that destroyed it.
    pub(crate) fn window_gone(&mut self, window: WindowId) {
        self.api.window_closed(self.runtime.context(), window);
        self.runtime.run_microtasks();
    }

    /// What the shell saw of a window, heard as its events.
    fn window_event(&mut self, window: WindowId, event: WindowEvent) {
        window_events::shell_event(self.runtime.context(), window, event);
        self.runtime.run_microtasks();
    }
}

/// The `code` of a thrown error, as `err.code` reads it.
fn error_code(context: &mut boa_engine::Context, error: &boa_engine::JsError) -> Option<String> {
    let value = error.clone().into_opaque(context).ok()?;
    let code = native::get(&value, "code", context).ok()?;
    code.as_string().map(|code| code.to_std_string_escaped())
}

/// Makes the realm's `process` the browser process's, and its console the app's own.
///
/// `process.exit` is `app.exit` (see [`api::process::install`]). Console lines go where Electron's main
/// process writes them: standard output, and standard error for warnings and errors. An error
/// nobody caught is told to the shell for its error box, and the app goes on until it quits.
fn set_up_process(runtime: &mut Runtime, hub: &Arc<Hub>) {
    let launch = hub.launch();
    let error_hub = Arc::clone(hub);
    let default = ProcessInfo::default();
    let mut argv = vec![default.exec_path.clone()];
    argv.extend(launch.argv.iter().cloned());
    let info = ProcessInfo {
        // Electron's main process reports Node's own title, whatever the app is called.
        title: "node".into(),
        process_type: Some("browser".into()),
        argv,
        argv0: std::env::args_os()
            .next()
            .map_or(default.argv0.clone(), |arg| {
                arg.to_string_lossy().into_owned()
            }),
        resources_path: Some(launch.resources_path.to_string_lossy().into_owned()),
        default_app: !matches!(launch.target, Target::Packaged { .. }),
        on_uncaught: UncaughtBehavior::Report(Arc::new(move |line| {
            error_hub.error_box(
                "A JavaScript error occurred in the main process".into(),
                format!("Uncaught Exception:\n{line}"),
            );
        })),
        ..default
    };
    if let Err(error) = runtime.set_process_info(info) {
        eprintln!("electron: process could not be set up: {error}");
    }
    let log = Arc::downgrade(hub);
    runtime.set_console_sink(Some(Box::new(move |level, text, _| {
        if let Some(hub) = log.upgrade() {
            hub.log_console(None, text);
        }
        match level {
            Level::Warn | Level::Error => eprintln!("{text}"),
            _ => println!("{text}"),
        }
    })));
}

impl Guest for MainGuest {
    type View = ();
    type Event = ();

    fn tick(&mut self) {
        self.hub.count_beat();
        beat::run(&mut self.runtime);
        if self.sweep_at.is_some() {
            self.sweep_at = api::ipc_main::sweep(self.runtime.context());
        }
        let closed = window_lifecycle::close_overdue(self.runtime.context());
        let pumped = api::message_port_main::pump(self.runtime.context());
        if closed || pumped {
            self.runtime.run_microtasks();
        }
        if let Some(code) = self.failed {
            self.hub.shutdown(code);
        }
    }

    fn next_due(&self) -> Option<Duration> {
        let sweep = self
            .sweep_at
            .map(|at| at.saturating_duration_since(Instant::now()));
        let closing = self
            .api
            .windows
            .next_overdue()
            .map(|at| at.saturating_duration_since(Instant::now()));
        [beat::next_due(&self.runtime), sweep, closing]
            .into_iter()
            .flatten()
            .min()
    }

    fn drain(&mut self, _out: &mut Outbox<()>) {}

    fn publish(&mut self, _applied: u64, _ran: bool) -> Option<()> {
        None
    }

    fn interrupt_handle(&mut self) -> Option<InterruptHandle> {
        Some(self.runtime.interrupt_handle())
    }
}
