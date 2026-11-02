//! One rendered page: the document, the runtime that changes it, and the frame the shell draws.
//!
//! The page's own thread does everything the page needs: it parses what it is told to load, runs
//! the script that changes the document, and publishes a [`RendererView`] for the shell to draw.
//! Nothing here calls the shell or the main process; what a page wants of them leaves as events
//! through the Hub, which is what keeps the page from ever holding anyone else up.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use verve::isolate::{Guest, InterruptHandle, Outbox};
use verve::{Level, ProcessInfo, Runtime};
use verve_dom::{Document, FrameClock, Options};

use crate::api;
use crate::beat;
use crate::hub::Hub;
use crate::ids::ContentsId;
use crate::msg::{
    FromRenderer, PageEvent, RendererView, ToRenderer, ToRendererInput, WebPreferences,
};

/// One page: the runtime its scripts run in, its document, and what it has yet to tell the main
/// process.
pub struct RendererGuest {
    runtime: Runtime,
    document: Document,
    /// What the page's lifecycle still has to tell the main process.
    lifecycle: Vec<PageEvent>,
    /// The console lines the page wrote since the last turn.
    console: Rc<RefCell<Vec<FromRenderer>>>,
    contents: ContentsId,
    /// What the page was made with; P20 makes the isolate anew with the same ones.
    prefs: WebPreferences,
}

impl RendererGuest {
    /// Page `contents` of the app `hub` routes, with an empty document, made with `prefs`, in
    /// the system theme the app started in. `about:blank` until something is loaded: the
    /// address and the history it brings are P20's.
    pub fn new(hub: &Hub, contents: ContentsId, prefs: WebPreferences) -> Self {
        let dark = hub.options().dark;
        let mut runtime = Runtime::new();
        let console = set_up_process(&mut runtime);
        api::install_page(&mut runtime);
        let document = verve_dom::install(
            &mut runtime,
            Options {
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
            lifecycle: Vec::new(),
            console,
            contents,
            prefs,
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
        self.runtime.set_waker(Some(waker));
    }

    /// What the main process asks of the page. A page's messages, evaluation and the zoom factor
    /// are P12/P13's.
    pub fn apply(&mut self, msg: ToRenderer) {
        if let ToRenderer::Load { url, .. } = msg {
            self.load(&url);
        }
    }

    /// What a person did to the window, to the page under it. Pointer, keys, text, scroll and
    /// focus are P24/P32's: the shell sends none of them while P02's `DomView` draws text and
    /// nothing else.
    pub fn input(&mut self, msg: ToRendererInput) {
        match msg {
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

    /// Loads `url` into the document, replacing what was there.
    fn load(&mut self, url: &str) {
        self.lifecycle.push(PageEvent::DidStartLoading);
        let Some(markup) = markup_of(url) else {
            self.lifecycle.push(PageEvent::DidFailLoad {
                code: -3,
                description: "ERR_ABORTED".into(),
                url: url.to_owned(),
            });
            return;
        };
        let document = self.document.document_node();
        let body = self.document.body();
        verve_dom::host::with_dom(self.runtime.context(), |dom| {
            dom.empty(body);
            verve_dom::html::parse_document(dom, document, &markup);
        });
        self.lifecycle.push(PageEvent::DomReady);
        self.lifecycle.push(PageEvent::DidFinishLoad);
    }
}

/// Makes the realm's `process` a renderer's, and answers where its console lines collect: the
/// main process hears of them as [`FromRenderer::Console`] events.
fn set_up_process(runtime: &mut Runtime) -> Rc<RefCell<Vec<FromRenderer>>> {
    let info = ProcessInfo {
        title: "electron".into(),
        process_type: Some("renderer".into()),
        ..ProcessInfo::default()
    };
    if let Err(error) = runtime.set_process_info(info) {
        eprintln!("electron: process could not be set up: {error}");
    }
    let lines = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&lines);
    runtime.set_console_sink(Some(Box::new(move |level: Level, text, at| {
        sink.borrow_mut().push(FromRenderer::Console {
            level,
            message: text.to_owned(),
            line: at.map_or(0, |at| at.line),
            source: at.map(|at| at.file.clone()).unwrap_or_default(),
        });
    })));
    lines
}

impl Guest for RendererGuest {
    type View = RendererView;
    type Event = FromRenderer;

    fn tick(&mut self) {
        beat::run(&mut self.runtime);
    }

    fn next_due(&self) -> Option<Duration> {
        beat::next_due(&self.runtime)
    }

    fn drain(&mut self, out: &mut Outbox<FromRenderer>) {
        for event in std::mem::take(&mut self.lifecycle) {
            out.push(FromRenderer::Lifecycle(event));
        }
        for line in self.console.take() {
            out.push(line);
        }
        for request in self.document.take_requests() {
            out.push(FromRenderer::Dom(request));
        }
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

/// The markup a `data:` URL carries, text of whatever type the URL says it is.
///
/// P20 replaces this with the page loader proper — `file:`, the network, `<script>`, `<link>` and
/// a document's address following all of it. P02 needs the one URL its own acceptance loads.
fn markup_of(url: &str) -> Option<String> {
    let url = url::Url::parse(url)
        .ok()
        .filter(|url| url.scheme() == "data")?;
    let (_, bytes) = verve::encoding::data_url(&url).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_data_url_is_the_markup_it_carries() {
        let markup = Some("<h1>hi</h1>".to_owned());
        assert_eq!(markup_of("data:text/html,<h1>hi</h1>"), markup);
        assert_eq!(markup_of("data:text/html,%3Ch1%3Ehi%3C%2Fh1%3E"), markup);
        assert_eq!(markup_of("data:text/html;base64,PGgxPmhpPC9oMT4="), markup);
        assert_eq!(markup_of("https://example.com/"), None);
    }
}
