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
use verve::{Level, Runtime};
use verve_dom::{Document, FrameClock, LoadEvent, Options};

use crate::api::{self, ipc_renderer::Outbox};
use crate::beat;
use crate::hub::Hub;
use crate::ids::ContentsId;
use crate::inspector;
use crate::loader::ElectronLoader;
use crate::log::Log;
use crate::media_host::{MediaBackend, Unsupported};
use crate::msg::{
    DragInput, EditCommand, FromRenderer, PageEvent, RendererView, ToRenderer, ToRendererInput,
    WebPreferences,
};
use crate::page_images::PageImages;
use crate::page_layout::PageLayout;
use crate::renderer_net::{self, RendererNet};
use crate::renderer_permissions::PagePermissions;
use crate::{page_script, preload, probe};

/// One page: the runtime its scripts run in, its document, and what it has yet to tell the main
/// process.
pub struct RendererGuest {
    runtime: Runtime,
    document: Document,
    /// Where the page's boxes are, and what the shell draws of them.
    layout: PageLayout,
    /// The pictures of the page, decoded.
    images: PageImages,
    /// What answers the page's `<video>` and `<audio>`.
    media: Box<dyn MediaBackend>,
    /// Everything the page has to tell the main process, in the order it happened: console lines,
    /// lifecycle, requests and IPC. It leaves as one job after each job the page runs.
    outbox: Outbox,
    contents: ContentsId,
    /// Which isolate this is among the page's: a reply meant for an earlier document's question
    /// carries another one and is dropped.
    generation: u64,
    /// What the page was made with; the Hub makes the isolate of the next document anew with
    /// the same ones.
    prefs: WebPreferences,
    /// What the page's document is loaded through; the other half is the document's.
    loader: ElectronLoader,
    /// The page's `fetch` of the schemes its session's main process answers.
    net: Rc<RendererNet>,
    /// The address the page last told the main process of, to see when the page moved within
    /// itself (`location.hash`, `pushState`).
    url: String,
    /// Where the page writes what it dumps for DevTools.
    log: Arc<Log>,
    /// What a drop from the system would do now, for the shell's cursor.
    drop_effect: Option<verve_dom::DropEffect>,
}

impl RendererGuest {
    /// Page `contents` of the app `hub` routes, with an empty document, made with `prefs`, in
    /// the system theme the app started in. `about:blank` until something is loaded.
    pub fn new(
        hub: &Arc<Hub>,
        contents: ContentsId,
        generation: u64,
        prefs: WebPreferences,
    ) -> Self {
        let dark = hub.dark();
        let mut runtime = Runtime::new();
        runtime.set_permissions(preload::permissions_for(&prefs));
        runtime.set_vfs(hub.overlay());
        // A page without Node resolves its modules as a browser does.
        runtime.set_web_modules(!preload::page_has_node(&prefs));
        api::install_page(
            &mut runtime,
            Arc::clone(hub),
            (contents, generation),
            &prefs,
        )
        .expect("a page's electron module installs");
        let outbox = api::ipc_renderer::outbox(runtime.context());
        set_up_process(&mut runtime, &outbox);
        let network = renderer_net::install(&mut runtime, hub, &prefs, &outbox);
        let permissions = PagePermissions::new(Arc::clone(&network.session), &outbox, generation);
        let loader = ElectronLoader::new(
            hub.overlay(),
            hub.protocol_calls(),
            network.session,
            network.privileged.clone(),
        );
        let document = verve_dom::install(
            &mut runtime,
            Options {
                loader: Some(Box::new(loader.clone())),
                // The shell says when a frame is drawn: there are no timers to drive
                // `requestAnimationFrame` on a page nobody draws.
                frames: FrameClock::Host,
                dark,
                standard_schemes: renderer_net::scheme_names(&network.privileged, |p| p.standard),
                secure_schemes: renderer_net::scheme_names(&network.privileged, |p| p.secure),
                cookies: Some(network.cookies),
                permissions: Some(Box::new(permissions)),
                ..Options::default()
            },
        )
        .expect("a page's document installs");
        preload::prepare(&mut runtime, &document, &prefs, &outbox, hub.overlay());
        let layout = PageLayout::install(&document, hub.text_measurer().for_page());
        Self {
            runtime,
            document,
            layout,
            images: PageImages::new(),
            media: Box::new(Unsupported),
            outbox,
            contents,
            generation,
            prefs,
            loader,
            net: network.net,
            url: String::new(),
            log: Arc::clone(hub.log()),
            drop_effect: None,
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

    /// Lets work finished on another thread — a socket, a worker — wake the isolate.
    pub fn set_waker(&mut self, waker: verve::Waker) {
        self.images.set_waker(waker.clone());
        self.runtime.set_waker(Some(waker));
    }

    /// What the main process asks of the page: a load, a message, a script to run, a sheet, an
    /// edit, or to close.
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
            ToRenderer::ProtocolHead {
                call,
                status,
                status_text,
                headers,
            } => self.net.head(call, status, status_text, headers),
            ToRenderer::ProtocolChunk { call, bytes } => self.net.chunk(call, bytes),
            ToRenderer::ProtocolEnd { call, error } => self.net.end(call, error),
            ToRenderer::WebRequestReply { call, reply } => {
                self.net.web_reply(call, reply, self.runtime.context());
            }
            ToRenderer::PermissionReply { call, granted } => {
                if let Some(ask) = PagePermissions::ask_of(call, self.generation) {
                    self.document
                        .settle_permission(&mut self.runtime, ask, granted);
                }
            }
            ToRenderer::ExecuteJavaScript {
                call, code, world, ..
            } => {
                let ran = page_script::run(&mut self.runtime, &code, world);
                // A `window.open` is heard before the script's result, as it is in Electron, where it
                // has been done by then; a navigation after it, as a page that has gone away says nothing.
                let (opens, rest): (Vec<_>, Vec<_>) = self
                    .document
                    .take_requests()
                    .into_iter()
                    .partition(|request| matches!(request, verve_dom::Request::Open { .. }));
                self.queue(opens);
                page_script::settle(&mut self.runtime, call, ran);
                self.queue(rest);
            }
            ToRenderer::InsertCss { key, css, user } => {
                page_script::insert_css(&mut self.runtime, &key, &css, user);
            }
            ToRenderer::RemoveCss { key } => page_script::remove_css(&mut self.runtime, &key),
            // The clipboard is the shell's: the document asks for it and pastes when it is sent.
            ToRenderer::EditCommand(EditCommand::Paste) => {
                self.document.request_paste(&mut self.runtime, false);
            }
            ToRenderer::EditCommand(EditCommand::PasteAndMatchStyle) => {
                self.document.request_paste(&mut self.runtime, true);
            }
            ToRenderer::EditCommand(command) => {
                page_script::edit(&mut self.runtime, &self.document, command);
            }
            ToRenderer::ConsoleError { message } => {
                verve::console_line(self.runtime.context(), Level::Error, message);
            }
            ToRenderer::InspectTree { at } => {
                inspector::dump(&self.document, &self.log, self.contents, at);
            }
            ToRenderer::InspectorEval { call, code } => {
                inspector::eval(&mut self.runtime, call, &code);
            }
            ToRenderer::BeforeUnload { call } => self.before_unload(call),
            ToRenderer::Unload { call } => self.unload(call),
            _ => {}
        }
    }

    /// The window is closing: `beforeunload` is dispatched, and the answer is told to the main
    /// process.
    fn before_unload(&mut self, call: u64) {
        let proceed = self.document.before_unload(&mut self.runtime);
        let reply = FromRenderer::BeforeUnloadReply { call, proceed };
        self.outbox.borrow_mut().push(reply);
    }

    /// The close goes ahead: `pagehide` and `unload` are dispatched, and the main process is told
    /// they were.
    fn unload(&mut self, call: u64) {
        self.document.unload(&mut self.runtime);
        self.outbox
            .borrow_mut()
            .push(FromRenderer::UnloadReply { call });
    }

    /// Evaluates `code` in the page's realm, answering the result as JSON.
    pub fn eval(&mut self, code: &str) -> Result<String, String> {
        probe::evaluate(&mut self.runtime, code)
    }

    /// Answers the media commands the page gave: this host plays nothing, so a source fails with
    /// `MEDIA_ERR_SRC_NOT_SUPPORTED`.
    fn serve_media(&mut self) {
        for command in self.document.take_media_commands() {
            for update in self.media.handle(&command) {
                let sent = self.document.media_update(
                    &mut self.runtime,
                    command.node,
                    command.generation,
                    update,
                );
                if let Err(error) = sent {
                    eprintln!("electron: a media answer was refused: {error}");
                }
            }
        }
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

    /// What a person did to the window, to the page under it: the pointer, the wheel, keys and text
    /// act on the document ([`page_script::person`]). The page lays itself out, so the boxes a
    /// shell measured ([`ToRendererInput::Layout`]) are not taken; the viewport, the colour scheme
    /// and the frames are; composition goes to the document (P24), focus is P32's.
    pub fn input(&mut self, msg: ToRendererInput) {
        match msg {
            ToRendererInput::Layout(_) => {}
            ToRendererInput::Viewport { width, height, .. } => {
                self.document.set_viewport(&mut self.runtime, width, height)
            }
            ToRendererInput::ColorScheme { dark } => {
                self.document.set_color_scheme(&mut self.runtime, dark)
            }
            // The page hears `wheel`; unless a listener prevents it, the wheel scrolls, or with
            // Control held it asks to zoom (`zoom-changed`) and scrolls nothing. Shift turns a
            // vertical wheel into a horizontal scroll, while the page still hears `deltaY`.
            ToRendererInput::Wheel {
                pointer,
                dx,
                dy,
                target,
            } => {
                let node = page_script::live(&self.document, target);
                let allowed = match node {
                    Some(node) => {
                        self.document
                            .wheel_on(&mut self.runtime, node, &pointer, (dx, dy))
                    }
                    None => self.document.wheel(&mut self.runtime, &pointer, dx, dy),
                };
                if !allowed {
                    return;
                }
                if pointer.modifiers.ctrl && dy != 0. {
                    let zoom = PageEvent::ZoomChanged { zoom_in: dy < 0. };
                    (self.outbox.borrow_mut()).push(FromRenderer::Lifecycle(zoom));
                    return;
                }
                let at = [pointer.x, pointer.y];
                let by = if pointer.modifiers.shift && dx == 0. {
                    [dy, 0.]
                } else {
                    [dx, dy]
                };
                self.layout.wheel(&mut self.runtime, &self.document, at, by);
            }
            ToRendererInput::Focus(on) => self.document.set_window_focus(&mut self.runtime, on),
            ToRendererInput::Frame { now } => {
                self.document.frame(&mut self.runtime, now);
            }
            ToRendererInput::Drag(drag) => self.drag(drag),
            ToRendererInput::ClipboardContents { call, data } => {
                self.document
                    .settle_clipboard(&mut self.runtime, call, &data);
            }
            other => {
                let heard = page_script::person(&mut self.runtime, &self.document, other);
                if let Some(message) = heard {
                    self.outbox.borrow_mut().push(message);
                }
            }
        }
    }

    /// A drag from the system: the document is told, and the shell is told what a drop would do.
    fn drag(&mut self, drag: DragInput) {
        let (runtime, document) = (&mut self.runtime, &self.document);
        self.drop_effect = match drag {
            DragInput::Enter { data, pointer } => {
                Some(document.drag_enter(runtime, data, &pointer))
            }
            DragInput::Over { pointer } => Some(document.drag_over(runtime, &pointer)),
            DragInput::Leave => {
                document.drag_leave(runtime);
                None
            }
            DragInput::Drop { pointer } => {
                document.drop(runtime, &pointer);
                None
            }
        };
    }

    /// Loads `url` into the document: the page is fetched and read as it arrives, and each stage
    /// is told to the main process as the loader reports it.
    fn load(&mut self, url: &str) {
        // Whatever the last document was waiting for is of no use to this one.
        let asked = self.loader.abandon();
        self.net.abandon(asked, self.runtime.context());
        self.require_from(url);
        self.document
            .navigate_initial_with(&mut self.runtime, url, preload::run);
        self.collect();
    }

    /// A page's `require` resolves relative names against the folder of the page, as Electron's
    /// does for a `file:` page: the `require` the page was given at first resolves them against
    /// the working directory, and is replaced once the page's address is known.
    fn require_from(&mut self, url: &str) {
        let prefs = &self.prefs;
        if !preload::page_has_node(prefs) || prefs.sandbox {
            return;
        }
        if let Some(folder) = preload::page_folder(url) {
            let require = self.runtime.global_require_in(&folder);
            let context = self.runtime.context();
            let defined = preload::define_page_node(context, require, url, &folder);
            api::define::report(context, defined);
        }
    }

    /// Takes what has arrived for the page and moves its load on.
    fn pump(&mut self) {
        self.document.pump_loads(&mut self.runtime);
        self.collect();
    }

    /// What the page asked of the window (a navigation, `window.open`, a dialog) goes to the
    /// outbox, in the order it happened with the rest.
    fn queue_requests(&mut self) {
        let requests = self.document.take_requests();
        self.queue(requests);
    }

    fn queue(&mut self, requests: Vec<verve_dom::Request>) {
        let (scrolls, requests) = crate::page_layout::split_scrolls(requests);
        {
            let mut outbox = self.outbox.borrow_mut();
            for request in requests {
                outbox.push(match request {
                    verve_dom::Request::Reload => FromRenderer::Reload {
                        url: self.document.url(),
                    },
                    other => FromRenderer::Dom(other),
                });
            }
        }
        // After the rest: what the scroll listeners write comes after what was asked before.
        self.layout
            .reveal(&mut self.runtime, &self.document, scrolls);
    }

    /// A page that is now at another address without a load (`location.hash = …`, `pushState`) has
    /// navigated within itself: the main process is told as `DidNavigateInPage`.
    fn report_move_within_page(&mut self) {
        let now = self.document.url();
        if self.url.is_empty() || now == self.url {
            return;
        }
        self.url.clone_from(&now);
        self.net.set_page(&now);
        let moved = PageEvent::DidNavigateInPage { url: now };
        self.outbox
            .borrow_mut()
            .push(FromRenderer::Lifecycle(moved));
    }

    /// The stages the document went through, as events of the page's lifecycle, in the page's
    /// ordered outbox.
    fn collect(&mut self) {
        for event in self.document.take_load_events() {
            let event = match event {
                LoadEvent::Started => PageEvent::DidStartLoading,
                LoadEvent::Committed(url) => {
                    self.url.clone_from(&url);
                    self.net.set_page(&url);
                    PageEvent::DidNavigate { url }
                }
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
        self.report_move_within_page();
    }
}

/// Makes the realm's `process` a renderer's, and sends its console lines to `outbox`: the main
/// process hears of them as [`FromRenderer::Console`] events.
fn set_up_process(runtime: &mut Runtime, outbox: &Outbox) {
    if let Err(error) = runtime.set_process_info(preload::renderer_info()) {
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
        // The page lays itself out and nobody reports the boxes: the observers hear once per
        // turn, after the timers, the frames and what the loader ran, and what their callbacks
        // ask for is drained next. The layout itself is made when something reads it, not after
        // each message.
        self.document.deliver_layout(&mut self.runtime);
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
        // A decoder that cannot wake the page is asked after.
        let decoding = self.images.pending() > 0 && !self.images.wakes();
        let soon = ((self.document.is_loading() && unasked > 0) || decoding)
            .then_some(Duration::from_millis(5));
        due.into_iter().chain(soon).min()
    }

    fn drain(&mut self, _out: &mut IsolateOutbox<FromRenderer>) {
        self.serve_media();
        self.collect();
        self.outbox.borrow_mut().extend(self.loader.take_outbox());
        self.queue_requests();
        api::ipc_renderer::flush(self.runtime.context());
    }

    fn publish(&mut self, applied: u64, ran: bool) -> Option<RendererView> {
        self.document.ensure_layout();
        self.layout
            .show_selection(&mut self.runtime, &self.document);
        let update = self.document.update();
        let changed = !update.changes.is_empty();
        let (list, new_list) = self.layout.list();
        self.images
            .sync(&update.snapshot.images, new_list.then_some(&*list));
        let (images, new_images) = self.images.table();
        (ran || changed || new_list || new_images).then(|| RendererView {
            applied,
            update: Some(update),
            wants_frame: self.document.wants_frame(),
            cursor: None,
            ime_rect: self.document.caret_rect(),
            drop_effect: self.drop_effect,
            display_list: Some(list),
            images,
        })
    }

    fn interrupt_handle(&mut self) -> Option<InterruptHandle> {
        Some(self.runtime.interrupt_handle())
    }
}
