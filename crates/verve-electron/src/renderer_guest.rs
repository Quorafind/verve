//! One rendered page: the document, the runtime that changes it, and the frame the shell draws.
//!
//! The page's own thread does everything the page needs: it parses what it is told to load, runs
//! the script that changes the document, and publishes a [`RendererView`] for the shell to draw.
//! Nothing here calls the shell or the main process; what a page wants of them leaves as events
//! through the Hub, which is what keeps the page from ever holding anyone else up.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use verve::isolate::{Guest, InterruptHandle, Outbox as IsolateOutbox};
use verve::{Level, ProcessInfo, Runtime};
use verve_dom::{Document, FrameClock, LoadEvent, Options};

use crate::api::{self, ipc_renderer::Outbox};
use crate::beat;
use crate::hub::Hub;
use crate::ids::ContentsId;
use crate::loader::ElectronLoader;
use crate::msg::{
    FromRenderer, PageEvent, RendererView, ToRenderer, ToRendererInput, WebPreferences,
};
use crate::probe;

/// One page: the runtime its scripts run in, its document, and what it has yet to tell the main
/// process.
pub struct RendererGuest {
    runtime: Runtime,
    document: Document,
    /// Everything the page has to tell the main process, in the order it happened: console lines,
    /// lifecycle, requests and IPC. It leaves as one job after each job the page runs.
    outbox: Outbox,
    contents: ContentsId,
    /// What the page was made with; the Hub makes the isolate of the next document anew with
    /// the same ones.
    prefs: WebPreferences,
    /// What the page's document is loaded through; the other half is the document's.
    loader: ElectronLoader,
}

impl RendererGuest {
    /// Page `contents` of the app `hub` routes, with an empty document, made with `prefs`, in
    /// the system theme the app started in. `about:blank` until something is loaded.
    pub fn new(hub: &Arc<Hub>, contents: ContentsId, prefs: WebPreferences) -> Self {
        let dark = hub.options().dark;
        let mut runtime = Runtime::new();
        runtime.set_vfs(hub.overlay());
        // A page without Node resolves its modules as a browser does.
        runtime.set_web_modules(!prefs.node_integration);
        api::install_page(&mut runtime, Arc::clone(hub), contents, &prefs)
            .expect("a page's electron module installs");
        let outbox = api::ipc_renderer::outbox(runtime.context());
        set_up_process(&mut runtime, &outbox);
        let loader = ElectronLoader::new(hub.overlay(), hub.protocol_calls());
        let document = verve_dom::install(
            &mut runtime,
            Options {
                loader: Some(Box::new(loader.clone())),
                // The shell says when a frame is drawn: there are no timers to drive
                // `requestAnimationFrame` on a page nobody draws.
                frames: FrameClock::Host,
                dark,
                ..Options::default()
            },
        )
        .expect("a page's document installs");
        Self {
            runtime,
            document,
            outbox,
            contents,
            prefs,
            loader,
        }
    }

    /// The page's `webContents.id`.
    pub fn contents(&self) -> ContentsId {
        self.contents
    }

    /// The preferences the page was made with.
    pub fn preferences(&self) -> &WebPreferences {
        &self.prefs
    }

    /// Makes `scheme` one the main process answers: what the page asks of it goes to the main
    /// process as a [`FromRenderer::ProtocolRequest`] (P16 registers a scheme per
    /// `protocol.handle`).
    pub fn register_scheme(&mut self, scheme: &str) {
        self.loader.register_scheme(scheme);
    }

    /// Lets work finished on another thread — a socket, a worker — wake the isolate.
    pub fn set_waker(&mut self, waker: verve::Waker) {
        self.runtime.set_waker(Some(waker));
    }

    /// What the main process asks of the page. A page's messages, evaluation and the zoom factor
    /// are P12/P13's.
    pub fn apply(&mut self, msg: ToRenderer) {
        match msg {
            ToRenderer::Load { url, .. } => self.load(&url),
            ToRenderer::Ipc { .. } | ToRenderer::Post { .. } | ToRenderer::InvokeReply { .. } => {
                api::ipc_renderer::receive(self.runtime.context(), msg);
                self.runtime.run_microtasks();
            }
            ToRenderer::ProtocolResponse { call, response } => {
                self.loader.answer(call, response);
                self.pump();
            }
            _ => {}
        }
    }

    /// Evaluates `code` in the page's realm, answering the result as JSON.
    pub fn eval(&mut self, code: &str) -> Result<String, String> {
        probe::evaluate(&mut self.runtime, code)
    }

    /// A person's click on the first element `selector` matches; `false` if there is none.
    /// P24/P32 bring the pointer itself, and this stays for shells that have no pointer.
    pub fn click(&mut self, selector: &str) -> bool {
        let found =
            verve_dom::host::with_dom(self.runtime.context(), |dom| dom.query(dom.body, selector));
        found.is_some_and(|node| {
            self.document.press(&mut self.runtime, node);
            true
        })
    }

    /// A person's typing into whatever has the focus.
    pub fn type_text(&mut self, text: &str) {
        self.document.type_text(&mut self.runtime, text);
    }

    /// What a person did to the window, to the page under it. Pointer, keys, text, scroll and
    /// focus are P24/P32's: the shell sends none of them while P02's `DomView` draws text and
    /// nothing else. The boxes a shell measured are the exception, until P30 lays pages out on
    /// the page's own thread: a headless shell still sends them.
    pub fn input(&mut self, msg: ToRendererInput) {
        match msg {
            ToRendererInput::Layout(boxes) => self.document.set_layout(&mut self.runtime, boxes),
            ToRendererInput::Viewport { width, height, .. } => {
                self.document.set_viewport(&mut self.runtime, width, height)
            }
            ToRendererInput::ColorScheme { dark } => {
                self.document.set_color_scheme(&mut self.runtime, dark)
            }
            ToRendererInput::Frame { now } => {
                self.document.frame(&mut self.runtime, now);
            }
            _ => {}
        }
    }

    /// Loads `url` into the document: the page is fetched and read as it arrives, and each stage
    /// is told to the main process as the loader reports it.
    fn load(&mut self, url: &str) {
        self.require_from(url);
        self.document.navigate_initial(&mut self.runtime, url);
        self.collect();
    }

    /// A page's `require` resolves relative names against the folder of the page, as Electron's
    /// does for a `file:` page: the `require` the page was given at first resolves them against
    /// the working directory, and is replaced once the page's address is known.
    fn require_from(&mut self, url: &str) {
        let prefs = &self.prefs;
        if !(prefs.node_integration && !prefs.context_isolation && !prefs.sandbox) {
            return;
        }
        let folder = url::Url::parse(url)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok())
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf));
        if let Some(folder) = folder {
            let require = self.runtime.global_require_in(&folder);
            if let Err(error) =
                verve::native::define_global(self.runtime.context(), "require", require)
            {
                verve::report(self.runtime.context(), &error);
            }
        }
    }

    /// Takes what has arrived for the page and moves its load on.
    fn pump(&mut self) {
        self.document.pump_loads(&mut self.runtime);
        self.collect();
    }

    /// The stages the document went through, as events of the page's lifecycle, in the page's
    /// ordered outbox.
    fn collect(&mut self) {
        for event in self.document.take_load_events() {
            let event = match event {
                LoadEvent::Started => PageEvent::DidStartLoading,
                LoadEvent::Committed(url) => PageEvent::DidNavigate { url },
                LoadEvent::DomContentLoaded => PageEvent::DomReady,
                LoadEvent::Loaded => PageEvent::DidFinishLoad,
                LoadEvent::Failed {
                    code,
                    description,
                    url,
                } => PageEvent::DidFailLoad {
                    code,
                    description,
                    url,
                },
                LoadEvent::Title(title) => PageEvent::TitleUpdated(title),
                LoadEvent::Icons(icons) => PageEvent::FaviconUpdated(icons),
                LoadEvent::FirstPaintable => PageEvent::FirstPaintable,
            };
            // With the console lines and requests, in the order they happened.
            self.outbox
                .borrow_mut()
                .push(FromRenderer::Lifecycle(event));
        }
    }
}

/// Makes the realm's `process` a renderer's, and sends its console lines to `outbox`: the main
/// process hears of them as [`FromRenderer::Console`] events.
fn set_up_process(runtime: &mut Runtime, outbox: &Outbox) {
    let info = ProcessInfo {
        title: "electron".into(),
        process_type: Some("renderer".into()),
        ..ProcessInfo::default()
    };
    if let Err(error) = runtime.set_process_info(info) {
        eprintln!("electron: process could not be set up: {error}");
    }
    let sink = Rc::clone(outbox);
    runtime.set_console_sink(Some(Box::new(move |level: Level, text, at| {
        sink.borrow_mut().push(FromRenderer::Console {
            level,
            message: text.to_owned(),
            line: at.map_or(0, |at| at.line),
            source: at.map(|at| at.file.clone()).unwrap_or_default(),
        });
    })));
}

impl Guest for RendererGuest {
    type View = RendererView;
    type Event = FromRenderer;

    fn tick(&mut self) {
        beat::run(&mut self.runtime);
        self.pump();
    }

    fn next_due(&self) -> Option<Duration> {
        let due = beat::next_due(&self.runtime);
        // What the loader answers at once is taken before this is asked. What is left comes
        // from the main process (a custom scheme: its answer is a job, which wakes the page)
        // or from the network (looked at soon, since nothing says when it arrives).
        let unasked = self
            .document
            .pending_loads()
            .saturating_sub(self.loader.waiting());
        let soon = (self.document.is_loading() && unasked > 0).then_some(Duration::from_millis(5));
        due.into_iter().chain(soon).min()
    }

    fn drain(&mut self, _out: &mut IsolateOutbox<FromRenderer>) {
        self.collect();
        let mut outbox = self.outbox.borrow_mut();
        outbox.extend(self.loader.take_outbox());
        outbox.extend(
            self.document
                .take_requests()
                .into_iter()
                .map(FromRenderer::Dom),
        );
        drop(outbox);
        api::ipc_renderer::flush(self.runtime.context());
    }

    fn publish(&mut self, applied: u64, ran: bool) -> Option<RendererView> {
        let update = self.document.update();
        let changed = !update.changes.is_empty();
        (ran || changed).then(|| RendererView {
            applied,
            update: Some(update),
            wants_frame: self.document.wants_frame(),
            cursor: None,
            ime_rect: None,
        })
    }

    fn interrupt_handle(&mut self) -> Option<InterruptHandle> {
        Some(self.runtime.interrupt_handle())
    }
}
