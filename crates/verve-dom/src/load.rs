//! Loading a page: fetching its markup, reading it as it arrives, and running, in HTML's order,
//! what it brings — scripts (classic, `defer`, `async`, modules), stylesheets, images, fonts —
//! until `load`.
//!
//! The embedder supplies a [`Loader`] (what `file:` and its own schemes are) and calls
//! [`Document::pump_loads`] on its beat: that is the one place a result is taken and the page
//! moves on, so nothing here blocks the thread, and a parser that waits for a script or a
//! sheet simply stops being stepped until the thing it waits for has arrived.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use url::Url;
use verve::ImportMap;

use crate::Document;
use crate::html::{Step, StreamParser};

mod handlers;
mod modules;
mod net;
mod script;
mod sheets;

pub(crate) use handlers::attribute;
pub(crate) use script::{connected, install, write};

/// What a page fetches as it loads: its scripts, sheets, images, fonts, icons — and what its
/// relative `fetch`/XHR go to when the scheme is not the network's.
pub trait Loader: 'static {
    /// Starts fetching `url`. `done` is called on the realm's thread (the embedder queues it
    /// back, e.g. as a runtime task) exactly once.
    fn fetch(&self, request: LoadRequest, done: Box<dyn FnOnce(LoadResult)>);
    /// Whether this loader answers `scheme` (file, app, custom ones registered by P16).
    /// http/https/data/blob go through Verve's own fetch unless the loader claims them.
    fn handles(&self, scheme: &str) -> bool;
}

/// What is asked of a [`Loader`].
#[derive(Clone, Debug)]
pub struct LoadRequest {
    pub url: Url,
    pub destination: Destination,
    /// The document that asked for it: `None` when the browser itself did (`loadURL`), so a
    /// loader can refuse what a page may not reach (`file:` from a page that is not one).
    pub initiator: Option<Url>,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

/// What a fetched thing is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    Document,
    Script,
    Module,
    Style,
    Image,
    Font,
    Icon,
    Fetch,
}

/// What a [`Loader`] answers.
#[derive(Debug)]
pub struct LoadResult {
    /// `200` for what was read from a file.
    pub status: u16,
    /// The address it came from, after any redirect.
    pub url: Url,
    pub mime: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Result<Vec<u8>, LoadError>,
}

/// Why nothing came: a Chromium net error, like `-6` (`ERR_FILE_NOT_FOUND`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    pub code: i32,
    pub description: String,
}

impl LoadResult {
    /// The answer for a request that failed with `code`.
    pub fn failed(url: Url, code: i32, description: &str) -> Self {
        Self {
            status: 0,
            url,
            mime: None,
            headers: Vec::new(),
            body: Err(LoadError {
                code,
                description: description.to_owned(),
            }),
        }
    }
}

/// What happened to the page as it loaded, in order: [`Document::take_load_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadEvent {
    Started,
    /// The page's document has come and replaces the last: `did-navigate`.
    Committed(String),
    DomContentLoaded,
    Loaded,
    Failed {
        code: i32,
        description: String,
        url: String,
    },
    Title(String),
    /// The parser reached `<body>` with every blocking sheet in: something can be drawn.
    FirstPaintable,
}

/// Where the page is in loading: `document.readyState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ready {
    Loading,
    Interactive,
    #[default]
    Complete,
}

impl Ready {
    pub fn name(self) -> &'static str {
        match self {
            Ready::Loading => "loading",
            Ready::Interactive => "interactive",
            Ready::Complete => "complete",
        }
    }
}

type Inbox = Rc<RefCell<Vec<(u64, LoadResult)>>>;

/// What a request in flight is for.
pub(crate) enum Wait {
    Document,
    Script(crate::dom::NodeId),
    Module {
        script: crate::dom::NodeId,
        url: String,
    },
    /// A `<link>`'s sheet, and whether scripts and the first paint wait for it.
    Sheet(crate::dom::NodeId, bool),
    /// A sheet a `<link>`'s sheet `@import`s.
    Import(crate::dom::NodeId, Vec<usize>, bool),
}

/// How far the page's markup has gone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Phase {
    /// The parser is still reading.
    #[default]
    Reading,
    /// Read to its end; `DOMContentLoaded` waits for the deferred scripts.
    Read,
    /// `DOMContentLoaded` has been heard.
    Interactive,
}

/// How a script is run among the page's others.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// The parser waits for it.
    Blocking,
    /// After the parse, in order.
    Defer,
    /// As soon as it is here.
    Async,
    /// As soon as it is here, but after those inserted before it (`script.async = false`).
    Ordered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Fetching,
    Ready,
    Failed,
}

/// A `<script>` the page has to run.
pub(crate) struct Script {
    pub node: crate::dom::NodeId,
    pub flow: Flow,
    pub module: bool,
    pub url: Option<Url>,
    pub text: String,
    pub status: Status,
}

impl Load {
    /// Lets go of what is kept for nodes that were freed.
    pub(crate) fn forget(&mut self, gone: &HashSet<crate::dom::NodeId>) {
        self.handled.retain(|(node, _)| !gone.contains(node));
        self.forced.retain(|node| !gone.contains(node));
        self.ordered.retain(|node| !gone.contains(node));
        self.linked.retain(|node| !gone.contains(node));
        self.import_ranks
            .retain(|(node, _), _| !gone.contains(node));
        self.imports_coming.retain(|node, _| !gone.contains(node));
        self.link_events.retain(|node, _| !gone.contains(node));
        self.pending.retain(|node, _| !gone.contains(node));
        self.failed_graphs.retain(|node| !gone.contains(node));
    }
}

/// The state of the page's loading, kept on [`crate::state::State`].
#[derive(Default)]
pub(crate) struct Load {
    pub loader: Option<Rc<dyn Loader>>,
    /// Whether this document loads: a loader was given, or a page was handed to it.
    pub active: bool,
    pub ready: Ready,
    pub events: Vec<LoadEvent>,
    pub current_script: Option<crate::dom::NodeId>,
    /// A parser-inserted script is running, or one it ran: the one time `document.write` is
    /// taken while the page is read.
    pub writing: bool,
    /// A script that does not hold the parser (`async`, `defer`, a module) is running: what it
    /// `document.write`s is ignored, with a console warning, as Chromium does.
    pub apart: bool,
    /// The event handler attributes read already, by element.
    pub handled: HashSet<(crate::dom::NodeId, String)>,
    /// Scripts a page made and ran: `script.async` stays true for them.
    pub forced: HashSet<crate::dom::NodeId>,
    pub parser: Option<StreamParser>,
    inbox: Inbox,
    next: u64,
    waiting: HashMap<u64, Wait>,
    pub scripts: Vec<Script>,
    /// How many `@import`s each sheet of a `<link>` (by its path) has asked for: the order their
    /// sheets go in.
    import_ranks: HashMap<(crate::dom::NodeId, Vec<usize>), usize>,
    /// How many `@import`s of each `<link>` are still coming.
    imports_coming: HashMap<crate::dom::NodeId, usize>,
    /// The `<link>`s whose own sheet is in and whose event waits for the imports: which event.
    link_events: HashMap<crate::dom::NodeId, &'static str>,
    /// The `<link>`s already asked for.
    linked: HashSet<crate::dom::NodeId>,
    /// Dynamic scripts that asked to run in order (`script.async = false`).
    pub ordered: HashSet<crate::dom::NodeId>,
    /// Stylesheets still coming that scripts wait for.
    pub blocking_sheets: usize,
    /// Events for elements, fired once what has arrived has been dealt with: a task of their
    /// own, after the script that was waiting for the sheet.
    pub notices: Vec<(crate::dom::NodeId, &'static str)>,
    pub modules: Rc<RefCell<HashMap<String, Rc<str>>>>,
    /// What was asked for that is not a module.
    requested: HashSet<String>,
    /// Sheets to ask for once the state is free of the borrow that found them.
    wanted: Vec<(Url, Wait)>,
    /// The modules asked for, so one that several graphs import is fetched once.
    mods: HashMap<String, modules::Mod>,
    /// The module scripts waiting for each module still coming.
    /// Each with whether the module is one its graph cannot do without: a literal `import()` is
    /// only fetched ahead, and fails when it runs, where the page can catch it.
    waiters: HashMap<String, HashMap<crate::dom::NodeId, bool>>,
    /// The modules each module script's graph still waits for.
    pending: HashMap<crate::dom::NodeId, HashSet<String>>,
    failed_graphs: HashSet<crate::dom::NodeId>,
    pub import_map: Option<ImportMap>,
    /// The runtime's `fetch` as it was before the page could replace it.
    pub original_fetch: Option<boa_engine::JsValue>,
    /// `Response.prototype.url`'s getter as it was before the page could replace it: where a
    /// response of the runtime's own `fetch` came from, whatever the page says `url` is.
    pub original_response_url: Option<boa_engine::JsValue>,
    pub module_scripts_started: usize,
    pub phase: Phase,
    painted: bool,
}

impl Document {
    /// Loads `url` into this (fresh) document: fetches it through the loader, parses it as it
    /// arrives, runs its scripts in HTML's order. Progress comes back through
    /// [`Document::take_load_events`].
    pub fn navigate_initial(&self, runtime: &mut verve::Runtime, url: &str) {
        let Ok(parsed) = Url::parse(url) else {
            return self.fail(url, -300, "ERR_INVALID_URL");
        };
        self.start_load(runtime, &parsed);
        net::start(
            runtime.context(),
            parsed,
            Destination::Document,
            Wait::Document,
        );
        self.pump_loads(runtime);
    }

    /// Same, with the markup already in hand (`loadURL('data:…')`, tests).
    pub fn load_html(&self, runtime: &mut verve::Runtime, url: &str, html: &str) {
        let parsed = Url::parse(url).unwrap_or_else(|_| Url::parse("about:blank").expect("a URL"));
        self.start_load(runtime, &parsed);
        self.state
            .borrow_mut()
            .load
            .events
            .push(LoadEvent::Committed(parsed.to_string()));
        self.begin(html);
        self.pump_loads(runtime);
    }

    /// What happened since last asked, in order.
    pub fn take_load_events(&self) -> Vec<LoadEvent> {
        std::mem::take(&mut self.state.borrow_mut().load.events)
    }

    /// The address of the page: where it was loaded from, after redirects.
    pub fn url(&self) -> String {
        self.state.borrow().url.to_string()
    }

    /// How many requests are out, waiting for an answer.
    pub fn pending_loads(&self) -> usize {
        self.state.borrow().load.waiting.len()
    }

    /// Whether the page still has something coming that [`Document::pump_loads`] has to take.
    pub fn is_loading(&self) -> bool {
        let state = self.state.borrow();
        state.load.active && (state.load.ready != Ready::Complete || !state.load.waiting.is_empty())
    }

    fn start_load(&self, runtime: &mut verve::Runtime, url: &Url) {
        {
            let mut state = self.state.borrow_mut();
            state.url = url.clone();
            state.history = vec![(boa_engine::JsValue::null(), url.to_string())];
            state.history_at = 0;
            state.load.active = true;
            // Attribute changes are how a handler or a script's `src` set later is heard.
            state.dom.watch_attrs = true;
            state.load.ready = Ready::Loading;
            state.load.events.push(LoadEvent::Started);
            state.touch();
        }
        // Electron's rule: only a page that is a local file may read local files.
        runtime.set_local_modules(url.scheme() == "file");
        let cache = Rc::clone(&self.state.borrow().load.modules);
        runtime.set_module_source(Some(Rc::new(move |url: &Url| {
            cache.borrow().get(url.as_str()).cloned()
        })));
    }

    fn fail(&self, url: &str, code: i32, description: &str) {
        let mut state = self.state.borrow_mut();
        state.load.ready = Ready::Complete;
        state.load.events.push(LoadEvent::Failed {
            code,
            description: description.to_owned(),
            url: url.to_owned(),
        });
    }

    /// The markup is here: the parser is made, and it has it all.
    fn begin(&self, html: &str) {
        let mut state = self.state.borrow_mut();
        let mut parser = StreamParser::new(&state.dom, state.dom.document);
        parser.feed(html);
        parser.finish();
        state.load.parser = Some(parser);
    }

    /// Takes what has arrived and moves the page on as far as it can go. Answers whether
    /// anything happened. The embedder calls it on its beat, and while [`Document::is_loading`].
    pub fn pump_loads(&self, runtime: &mut verve::Runtime) -> bool {
        if !self.state.borrow().load.active {
            return false;
        }
        let mut did = false;
        loop {
            runtime.run_microtasks();
            let mut moved = self.route_requests(runtime);
            moved |= self.deliver(runtime);
            moved |= self.advance(runtime);
            if !moved {
                return did;
            }
            did = true;
        }
    }

    /// The sheets a sheet `@import`s are asked for through the loader.
    fn route_requests(&self, runtime: &mut verve::Runtime) -> bool {
        let wanted = std::mem::take(&mut self.state.borrow_mut().load.wanted);
        let did = !wanted.is_empty();
        for (url, wait) in wanted {
            net::start(runtime.context(), url, Destination::Style, wait);
        }
        did
    }

    /// Takes the results that came back.
    fn deliver(&self, runtime: &mut verve::Runtime) -> bool {
        let items = {
            let state = self.state.borrow();
            let mut inbox = state.load.inbox.borrow_mut();
            std::mem::take(&mut *inbox)
        };
        let did = !items.is_empty();
        for (token, result) in items {
            let wait = self.state.borrow_mut().load.waiting.remove(&token);
            if let Some(wait) = wait {
                self.arrived(runtime, wait, result);
            }
        }
        did
    }

    fn arrived(&self, runtime: &mut verve::Runtime, wait: Wait, result: LoadResult) {
        match wait {
            Wait::Document => self.document_arrived(result),
            Wait::Script(node) => script::fetched(self, node, result),
            Wait::Module { script, url } => modules::fetched(self, runtime, script, &url, result),
            Wait::Sheet(node, blocking) => sheets::fetched(self, node, blocking, result),
            Wait::Import(node, rank, blocking) => {
                sheets::imported(self, (node, rank, blocking), result)
            }
        }
    }

    fn document_arrived(&self, result: LoadResult) {
        match result.body {
            Err(error) => self.fail(result.url.as_str(), error.code, &error.description),
            Ok(bytes) => {
                let url = result.url;
                {
                    let mut state = self.state.borrow_mut();
                    state
                        .load
                        .events
                        .push(LoadEvent::Committed(url.to_string()));
                    state.url = url;
                }
                self.begin(&net::text(&bytes));
            }
        }
    }

    /// Runs what can run, reads on, and moves the page through its phases.
    fn advance(&self, runtime: &mut verve::Runtime) -> bool {
        let mut did = false;
        while let Some(script) = self.next_script() {
            script::execute(self, runtime, script);
            did = true;
        }
        did |= self.parse(runtime);
        did |= self.phases(runtime);
        let notices = std::mem::take(&mut self.state.borrow_mut().load.notices);
        for (node, kind) in notices {
            script::fire_on(runtime.context(), node, kind);
            did = true;
        }
        did
    }

    /// The next script that may run now, taken out of the list.
    fn next_script(&self) -> Option<Script> {
        let mut state = self.state.borrow_mut();
        let load = &mut state.load;
        let (parsed, sheets) = (load.phase >= Phase::Read, load.blocking_sheets);
        let (mut deferred_waits, mut ordered_waits) = (false, false);
        let mut found = None;
        for (at, script) in load.scripts.iter().enumerate() {
            let here = script.status != Status::Fetching;
            let may = match script.flow {
                Flow::Async => here,
                Flow::Blocking => here && sheets == 0,
                Flow::Defer => parsed && sheets == 0 && here && !deferred_waits,
                Flow::Ordered => here && !ordered_waits,
            };
            deferred_waits |= script.flow == Flow::Defer && !here;
            ordered_waits |= script.flow == Flow::Ordered && !here;
            if may {
                found = Some(at);
                break;
            }
        }
        found.map(|at| load.scripts.remove(at))
    }

    /// Steps the parser until it needs something, hands it each script it stops at.
    fn parse(&self, runtime: &mut verve::Runtime) -> bool {
        let mut did = false;
        loop {
            let ready = {
                let state = self.state.borrow();
                state.load.parser.is_some()
                    && !state.load.scripts.iter().any(|s| s.flow == Flow::Blocking)
            };
            if !ready {
                return did;
            }
            let Some(mut parser) = self.state.borrow_mut().load.parser.take() else {
                return did;
            };
            let step = crate::bind::node::with_dom(runtime.context(), |dom| parser.step(dom));
            match step {
                Step::Script(node) => {
                    self.state.borrow_mut().load.parser = Some(parser);
                    script::parsed(self, runtime, node);
                }
                Step::NeedMore => {
                    self.state.borrow_mut().load.parser = Some(parser);
                    return did;
                }
                Step::Done => {
                    self.state.borrow_mut().load.phase = Phase::Read;
                    return true;
                }
            }
            did = true;
        }
    }

    /// `interactive`, `DOMContentLoaded`, `complete` and `load`, each when it is time.
    fn phases(&self, runtime: &mut verve::Runtime) -> bool {
        // What the parse asked for is on its way before `load` is thought of.
        let mut did = self.route_requests(runtime);
        did |= self.first_paintable();
        let (parsed, ready, deferred, dcl) = {
            let state = self.state.borrow();
            let load = &state.load;
            let deferred = load.scripts.iter().any(|s| s.flow == Flow::Defer);
            (
                load.phase >= Phase::Read,
                load.ready,
                deferred,
                load.phase >= Phase::Interactive,
            )
        };
        let context = runtime.context();
        if parsed && ready == Ready::Loading {
            self.state.borrow_mut().load.ready = Ready::Interactive;
            script::fire_on_document(context, "readystatechange", false);
            did = true;
        }
        if parsed && !deferred && !dcl {
            self.state.borrow_mut().load.phase = Phase::Interactive;
            self.announce_page();
            script::fire_on_document(runtime.context(), "DOMContentLoaded", true);
            self.state
                .borrow_mut()
                .load
                .events
                .push(LoadEvent::DomContentLoaded);
            did = true;
        }
        did | self.complete(runtime)
    }

    /// The title the page named, once its head is read.
    fn announce_page(&self) {
        let mut state = self.state.borrow_mut();
        let title = state
            .dom
            .first_tag(state.dom.document, "title")
            .map(|node| state.dom.text_content(node))
            .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        if !title.is_empty() {
            state.load.events.push(LoadEvent::Title(title));
        }
    }

    fn first_paintable(&self) -> bool {
        let mut state = self.state.borrow_mut();
        let started =
            !state.dom.children(state.dom.body).is_empty() || state.load.phase >= Phase::Read;
        if state.load.painted || !started || state.load.blocking_sheets > 0 {
            return false;
        }
        state.load.painted = true;
        state.load.events.push(LoadEvent::FirstPaintable);
        true
    }

    /// `complete` and `load`: when every subresource and every script is in.
    fn complete(&self, runtime: &mut verve::Runtime) -> bool {
        let done = {
            let state = self.state.borrow();
            let load = &state.load;
            load.phase >= Phase::Interactive
                && load.ready == Ready::Interactive
                && load.scripts.is_empty()
                && load.waiting.is_empty()
        };
        if !done {
            return false;
        }
        self.state.borrow_mut().load.ready = Ready::Complete;
        let context = runtime.context();
        script::fire_on_document(context, "readystatechange", false);
        script::fire_on_window(context, "load");
        script::fire_on_window(context, "pageshow");
        self.state.borrow_mut().load.events.push(LoadEvent::Loaded);
        true
    }
}
