//! A headless web document for [Verve](verve): the DOM, CSS's cascade and the window's platform
//! pieces, every one a native Rust function on Verve's Boa, so React and DOM-using libraries run
//! unchanged with no browser and no webview.
//!
//! [`install`] gives a [`verve::Runtime`] a `window` and a `document`; nothing else in Verve has
//! one. Scripts build and change the document as in a browser; whoever draws it — a GPUI view,
//! a terminal, a test — reads it from the [`Document`] handle:
//!
//! - **what to draw**: [`Document::update`] answers a [`Snapshot`] (the arena and every node's
//!   computed style, cheap to copy and `Send`) and the [`Changes`] since the last one — the
//!   nodes changed, the styles that moved, and the *partitions* (subtrees drawn as views of
//!   their own, [`Document::set_partition`]) that need drawing again;
//! - **what a person does**: pointer, wheel, key, text and focus input, dispatched as the DOM's
//!   events with a browser's default actions ([`Document::pointer_down`],
//!   [`Document::key_down`], [`Document::insert_text`]…);
//! - **where things landed**: the boxes the drawer painted, reported back
//!   ([`Document::set_layout`]), are what `getBoundingClientRect`, `offsetWidth`,
//!   `ResizeObserver` and `IntersectionObserver` answer;
//! - **what only the embedder can do**: navigate, open a window, write the clipboard, scroll —
//!   [`Document::take_requests`].
//!
//! The arena, the CSS engine and most of the bindings are lifted from su-editor's Obsidian
//! host, where the same document holds every Obsidian plugin's DOM; what was Obsidian's own
//! (`createEl`, `setText`, its user agent and `app://obsidian.md`) is left to the embedder,
//! which can add methods to any class with [`host::extend`].

mod bind;
pub mod css;
pub mod dom;
pub mod field;
pub mod html;
mod state;

use std::cell::{Ref, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsResult, JsValue, js_string};

pub use css::{Computed, Environment, Style};
pub use dom::{Dom, Namespace, Node, NodeId, NodeKind};
pub use field::Draft;
pub use state::FrameClock;

use state::{SheetOwner, SheetRecord, Shared, State, shared};

/// A box, in CSS pixels from the viewport's top left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }

    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right >= x && bottom >= y && (right > x || bottom > y || self.width == 0. || self.height == 0.))
            .then(|| Rect { x, y, width: right - x, height: bottom - y })
    }
}

/// A scroll a script asked for, for the drawer to do (and then report with
/// [`Document::set_scroll_offsets`]).
#[derive(Clone, Debug, PartialEq)]
pub enum ScrollRequest {
    /// `scrollTo`, or `scrollTop`/`scrollLeft` set: to this offset, on the axes given. The
    /// document's root element stands for the window.
    To { node: NodeId, left: Option<f32>, top: Option<f32> },
    /// `scrollBy`: this much further.
    By { node: NodeId, left: f32, top: f32 },
    /// `scrollIntoView`: whatever scrolls `node`'s box, until `node` is in sight.
    Reveal(NodeId),
}

/// What a script asked of the window that only the embedder can do.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// Go to another page (`location.href = …`, a link pressed); in-page fragments are the
    /// document's own and never asked for.
    Navigate { url: String, replace: bool },
    /// `location.reload()`.
    Reload,
    /// `window.open(url)`.
    Open(String),
    /// `navigator.clipboard.writeText`, `document.execCommand('copy')`.
    CopyText(String),
    /// `alert`, `confirm` and `prompt`'s message (they answer as a browser that blocked them).
    Alert(String),
    /// `document.title` set.
    Title(String),
    Scroll(ScrollRequest),
    /// `requestFullscreen` (`Some`) or `exitFullscreen` (`None`).
    Fullscreen(Option<NodeId>),
    /// A form submitted (its `submit` event not prevented, or `form.submit()`).
    Submit(NodeId),
    /// An animation frame is wanted, on [`FrameClock::Host`]: call [`Document::frame`].
    Frame,
}

/// Where `localStorage` lives between runs.
pub trait StorageBackend {
    fn load(&self) -> std::collections::BTreeMap<String, String>;
    fn save(&self, items: &std::collections::BTreeMap<String, String>);
}

/// `localStorage` kept as one JSON file.
pub struct FileStorage(pub PathBuf);

impl StorageBackend for FileStorage {
    fn load(&self) -> std::collections::BTreeMap<String, String> {
        std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, items: &std::collections::BTreeMap<String, String>) {
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(items) {
            let _ = std::fs::write(&self.0, text);
        }
    }
}

/// What the window is.
pub struct Options {
    /// The page's address: `location`, `document.URL`, what relative links resolve against.
    pub url: String,
    /// `navigator.userAgent`; `None` keeps Verve's own.
    pub user_agent: Option<String>,
    /// `navigator.language`; `None` keeps Verve's own (`en-US`).
    pub language: Option<String>,
    /// The viewport, in CSS pixels: `innerWidth`/`innerHeight`, what `@media` and
    /// `matchMedia` ask of.
    pub viewport: (f32, f32),
    pub device_pixel_ratio: f32,
    /// `prefers-color-scheme: dark`.
    pub dark: bool,
    /// How `requestAnimationFrame` is paced.
    pub frames: FrameClock,
    /// Whether the user agent's own sheet (`div { display: block }`, the headings' sizes…) is in
    /// the cascade.
    pub user_agent_sheet: bool,
    /// Stylesheets of the embedder's, by name, after the user agent's and before the page's.
    pub stylesheets: Vec<(String, String)>,
    /// Where `localStorage` lives between runs; `None` keeps it for this run only.
    pub storage: Option<Box<dyn StorageBackend>>,
    /// `document.title` when the page has no `<title>`.
    pub title: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            url: "about:blank".into(),
            user_agent: None,
            language: None,
            viewport: (1280., 800.),
            device_pixel_ratio: 1.,
            dark: false,
            frames: FrameClock::Timer,
            user_agent_sheet: true,
            stylesheets: Vec::new(),
            storage: None,
            title: String::new(),
        }
    }
}

/// Gives the runtime a document: `window` (the global object, made a `Window`), `document`,
/// and the classes and functions of the web platform (see the README for the list). Answers
/// the handle whoever draws the document keeps. A runtime has one document; installing again
/// answers the one it has.
pub fn install(runtime: &mut verve::Runtime, options: Options) -> Result<Document, String> {
    if let Some(document) = Document::of(runtime) {
        return Ok(document);
    }
    let url = url::Url::parse(&options.url)
        .or_else(|_| url::Url::parse("about:blank"))
        .map_err(|e| e.to_string())?;
    let context = runtime.context();
    let state: Shared = Rc::new(RefCell::new(State::new(url)));
    {
        let mut s = state.borrow_mut();
        s.env = Environment {
            width: options.viewport.0,
            height: options.viewport.1,
            device_pixel_ratio: options.device_pixel_ratio,
            dark: options.dark,
            ..Environment::default()
        };
        s.frame_clock = options.frames;
        s.title_fallback = options.title.clone();
        if options.user_agent_sheet {
            s.sheets.push(SheetRecord {
                owner: SheetOwner::UserAgent,
                sheet: css::Sheet::parse(css::USER_AGENT_SHEET),
            });
        }
        for (name, text) in &options.stylesheets {
            s.sheets.push(SheetRecord {
                owner: SheetOwner::Embedder(name.clone()),
                sheet: css::Sheet::parse(text),
            });
        }
    }
    context.insert_data(state.clone());
    install_classes(context, &options).map_err(|error| verve::describe(context, &error))?;
    let storage = options.storage;
    if let Some(storage) = storage {
        let mut s = state.borrow_mut();
        s.local = storage.load();
        s.storage = Some(storage);
    }
    runtime.run_microtasks();
    Document::of(runtime).ok_or_else(|| "the document did not install".to_owned())
}

fn install_classes(context: &mut Context, options: &Options) -> JsResult<()> {
    bind::events::install(context)?;
    bind::node::install(context)?;
    bind::element::install(context)?;
    bind::document::install(context)?;
    bind::window::install(context, options)?;
    bind::platform::install(context)?;
    bind::observers::install(context)?;
    bind::xhr::install(context)?;
    bind::traversal::install(context)?;
    bind::events::install_handlers(context)?;
    let document_id = shared(context).borrow().dom.document;
    let document = bind::node::wrap(context, document_id);
    context.register_global_property(
        js_string!("document"),
        document,
        boa_engine::property::Attribute::ENUMERABLE,
    )?;
    Ok(())
}

/// The nodes under the point `(x, y)`, innermost first: the deepest painted box that holds the
/// point, then each element it is in — what `elementFromPoint` answers and what the pointer
/// events of [`Document::pointer_move`] target.
pub(crate) fn hit_test(state: &State, x: f32, y: f32) -> Vec<NodeId> {
    let dom = &state.dom;
    // The deepest painted box holding the point; of two as deep, the later in document order.
    let mut best: Option<(usize, NodeId)> = None;
    for (node, rect) in &state.rects {
        if !rect.contains(x, y) || !dom.is_connected(*node) || !dom.get(*node).is_some_and(|n| n.is_element()) {
            continue;
        }
        if !dom.is_shown(*node) {
            continue;
        }
        let depth = bind::node::ancestry(dom, *node).len();
        let better = match best {
            None => true,
            Some((d, other)) => depth > d || (depth == d && dom.order(*node, other).is_gt()),
        };
        if better {
            best = Some((depth, *node));
        }
    }
    let Some((_, target)) = best else {
        return Vec::new();
    };
    let mut path = vec![target];
    let mut at = dom.parent_element(target);
    while let Some(parent) = at {
        path.push(parent);
        at = dom.parent_element(parent);
    }
    path
}

/// What the drawer reads: the document and every node's style, as they were when it was taken.
/// Cheap to take and to clone — the nodes and styles are shared with the document's own until
/// they change — and `Send`, for a drawer on another thread.
#[derive(Clone)]
pub struct Snapshot(Arc<SnapshotData>);

pub struct SnapshotData {
    pub dom: Dom,
    pub styles: Computed,
    /// Moves whenever anything here may have changed.
    pub revision: u64,
    pub viewport: (f32, f32),
    pub url: String,
    pub scroll_offsets: std::collections::HashMap<NodeId, [f32; 2]>,
}

impl std::ops::Deref for Snapshot {
    type Target = SnapshotData;

    fn deref(&self) -> &SnapshotData {
        &self.0
    }
}

impl Snapshot {
    /// A node's computed value of `property`.
    pub fn style(&self, node: NodeId, property: &str) -> Option<&str> {
        self.styles.get(node, property)
    }

    /// Whether the node is drawn at all: connected, and neither it nor anything it is in
    /// `display: none`.
    pub fn is_rendered(&self, node: NodeId) -> bool {
        let mut at = Some(node);
        while let Some(id) = at {
            if self.styles.get(id, "display") == Some("none") {
                return false;
            }
            at = self.dom.parent(id);
        }
        self.dom.is_connected(node)
    }
}

/// What changed since the drawer last asked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Every node changed — its own state (attributes, classes, text, value) or its children.
    pub nodes: Vec<NodeId>,
    /// Every node whose computed style is not what it was.
    pub restyled: Vec<NodeId>,
    /// The partitions with something new to draw: for each changed or restyled node, the
    /// partition it is in (and its parent's, for a node that came or went).
    pub partitions: Vec<NodeId>,
    /// Nodes freed by [`Document::sweep`]: forget what was kept against them.
    pub removed: Vec<NodeId>,
    /// The stylesheets changed, so every style was worked out again.
    pub restyled_all: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.restyled.is_empty() && self.removed.is_empty() && !self.restyled_all
    }
}

/// A snapshot and what changed since the last: [`Document::update`]'s answer.
pub struct Update {
    pub snapshot: Snapshot,
    pub changes: Changes,
}

/// The modifier keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// The pointer, where it is and what it pressed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pointer {
    pub x: f32,
    pub y: f32,
    /// The button pressed or let go: 0 primary, 1 middle (auxiliary), 2 secondary.
    pub button: i16,
    /// How many presses in a row this is: 2 for the second of a double click.
    pub count: u32,
    pub modifiers: Modifiers,
}

/// A key, as `KeyboardEvent` has it: `key` (`"a"`, `"Enter"`, `"ArrowLeft"`) and `code`
/// (`"KeyA"`, `"Enter"`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Key {
    pub key: String,
    pub code: String,
    pub modifiers: Modifiers,
    pub repeat: bool,
}

impl Key {
    /// The key that types `c`, with its `code`.
    pub fn char(c: char) -> Self {
        let code = if c.is_ascii_alphabetic() {
            format!("Key{}", c.to_ascii_uppercase())
        } else if c.is_ascii_digit() {
            format!("Digit{c}")
        } else if c == ' ' {
            "Space".to_owned()
        } else {
            String::new()
        };
        Key {
            key: c.to_string(),
            code,
            modifiers: Modifiers {
                shift: c.is_ascii_uppercase(),
                ..Modifiers::default()
            },
            repeat: false,
        }
    }

    /// A named key: `Key::named("Enter")`, `Key::named("Backspace")`.
    pub fn named(name: &str) -> Self {
        Key {
            key: name.to_owned(),
            code: name.to_owned(),
            ..Key::default()
        }
    }
}

/// The handle whoever draws the document keeps. Every method that dispatches events takes the
/// runtime, runs the microtasks the listeners queued, and leaves the document settled.
#[derive(Clone)]
pub struct Document {
    state: Shared,
    view: Rc<RefCell<View>>,
}

/// What the drawer has been told so far.
#[derive(Default)]
struct View {
    computed: Computed,
    changed: HashSet<NodeId>,
    restyled: HashSet<NodeId>,
    partitions: HashSet<NodeId>,
    removed: Vec<NodeId>,
    restyled_all: bool,
    /// The element a button went down on, for the click its release makes.
    pressed: Option<NodeId>,
    /// The element the pointer was last over.
    over: Option<NodeId>,
}

impl Document {
    fn new(state: Shared) -> Self {
        Self {
            state,
            view: Rc::new(RefCell::new(View::default())),
        }
    }

    /// The document installed in `runtime`, if one is.
    pub fn of(runtime: &mut verve::Runtime) -> Option<Document> {
        let state = state::try_shared(runtime.context())?;
        let view = runtime
            .context()
            .get_data::<Rc<RefCell<View>>>()
            .cloned();
        let document = match view {
            Some(view) => Document { state, view },
            None => {
                let document = Document::new(state);
                runtime.context().insert_data(document.view.clone());
                document
            }
        };
        Some(document)
    }

    /// The arena itself, as it is now.
    pub fn dom(&self) -> Ref<'_, Dom> {
        Ref::map(self.state.borrow(), |state| &state.dom)
    }

    /// The window's document node.
    pub fn document_node(&self) -> NodeId {
        self.state.borrow().dom.document
    }

    pub fn body(&self) -> NodeId {
        self.state.borrow().dom.body
    }

    /// Brings the styles up to date and answers a snapshot, with what changed since the last
    /// update.
    pub fn update(&self) -> Update {
        self.absorb();
        let snapshot = self.snapshot();
        let mut view = self.view.borrow_mut();
        let mut nodes: Vec<NodeId> = view.changed.drain().collect();
        nodes.sort_unstable();
        let mut restyled: Vec<NodeId> = view.restyled.drain().collect();
        restyled.sort_unstable();
        let mut partitions: Vec<NodeId> = view.partitions.drain().collect();
        partitions.sort_unstable();
        let changes = Changes {
            nodes,
            restyled,
            partitions,
            removed: std::mem::take(&mut view.removed),
            restyled_all: std::mem::take(&mut view.restyled_all),
        };
        Update { snapshot, changes }
    }

    /// A snapshot of the document and its styles, without taking what changed.
    pub fn snapshot(&self) -> Snapshot {
        self.absorb();
        let state = self.state.borrow();
        let view = self.view.borrow();
        Snapshot(Arc::new(SnapshotData {
            dom: state.dom.clone(),
            styles: view.computed.clone(),
            revision: state.dom.revision.wrapping_add(state.revision),
            viewport: (state.env.width, state.env.height),
            url: state.url.to_string(),
            scroll_offsets: state.scroll_offsets.clone(),
        }))
    }

    /// Every node's style, up to date.
    pub fn computed(&self) -> Ref<'_, Computed> {
        self.absorb();
        Ref::map(self.view.borrow(), |view| &view.computed)
    }

    /// Styles and partitions brought up to date with what the document says changed.
    fn absorb(&self) {
        let mut state = self.state.borrow_mut();
        let styler = state.styler();
        let key = state.sheets_key;
        let changes = state.dom.take_changes();
        let mut view = self.view.borrow_mut();
        let full = !view.computed.is_fresh(key);
        if changes.nodes.is_empty() && !full {
            return;
        }
        let before = view.computed.clone();
        view.computed.refresh(&state.dom, &styler, key, &changes.nodes);
        let dom = &state.dom;
        let mut restyled = Vec::new();
        if full {
            view.restyled_all = true;
            for id in dom.subtree(dom.document) {
                if before.shared(id) != view.computed.shared(id) {
                    restyled.push(id);
                }
            }
        } else {
            for root in &changes.nodes {
                let root = dom.parent(*root).unwrap_or(*root);
                for id in dom.subtree(root) {
                    let old = before.shared(id);
                    let new = view.computed.shared(id);
                    let same = match (&old, &new) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        restyled.push(id);
                    }
                }
            }
        }
        for id in &changes.nodes {
            if dom.exists(*id) {
                view.changed.insert(*id);
                let partition = dom.partition_of(*id);
                view.partitions.insert(partition);
                if let Some(parent) = dom.parent(*id) {
                    let partition = dom.partition_of(parent);
                    view.partitions.insert(partition);
                }
            }
        }
        for id in restyled {
            if dom.exists(id) {
                view.restyled.insert(id);
                let partition = dom.partition_of(id);
                view.partitions.insert(partition);
            }
        }
    }

    /// Draws `node`'s subtree as a view of its own: a change inside it marks only it dirty (see
    /// [`Changes::partitions`]).
    pub fn set_partition(&self, node: NodeId, on: bool) {
        self.state.borrow_mut().dom.set_partition(node, on);
    }

    /// What scripts asked of the window since this was last asked.
    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut self.state.borrow_mut().requests)
    }

    /// Whether `node` has a listener for any of `kinds` (`click`, `pointerdown`…): what makes a
    /// box something to press.
    pub fn listens(&self, runtime: &mut verve::Runtime, node: NodeId, kinds: &[&str]) -> bool {
        let context = runtime.context();
        let Some(object) = bind::node::existing_wrapper(context, node) else {
            return false;
        };
        verve::dom_events::listens(&object.into(), kinds, context)
    }

    /// Whether the drawer should follow the pointer on `node`: a `:hover` or `:active` rule
    /// would change how it looks.
    pub fn hover_sensitive(&self, node: NodeId) -> bool {
        self.computed().hover_sensitive(node)
    }

    /// Adds (or replaces) a stylesheet of the embedder's, by name: after the user agent's and
    /// before the page's.
    pub fn set_stylesheet(&self, name: &str, css: &str) {
        let mut state = self.state.borrow_mut();
        let sheet = css::Sheet::parse(css);
        let owner = SheetOwner::Embedder(name.to_owned());
        match state.sheets.iter_mut().find(|r| r.owner == owner) {
            Some(record) => record.sheet = sheet,
            None => {
                let at = state
                    .sheets
                    .iter()
                    .position(|r| matches!(r.owner, SheetOwner::Element(_) | SheetOwner::Adopted(_)))
                    .unwrap_or(state.sheets.len());
                state.sheets.insert(at, SheetRecord { owner, sheet });
            }
        }
        state.sheets_changed();
    }

    pub fn remove_stylesheet(&self, name: &str) {
        let mut state = self.state.borrow_mut();
        let owner = SheetOwner::Embedder(name.to_owned());
        state.sheets.retain(|r| r.owner != owner);
        state.sheets_changed();
    }

    // ─── Where things landed ────────────────────────────────────────────────────────────────

    /// The viewport's size: `innerWidth`, `innerHeight`, what `@media` asks of. The window
    /// hears `resize`, and `matchMedia` lists whose answer moved hear `change`.
    pub fn set_viewport(&self, runtime: &mut verve::Runtime, width: f32, height: f32) {
        {
            let mut state = self.state.borrow_mut();
            if state.env.width == width && state.env.height == height {
                return;
            }
            state.env.width = width;
            state.env.height = height;
            state.sheets_changed();
        }
        let context = runtime.context();
        let global: JsValue = context.global_object().into();
        let _ = bind::events::fire(context, &global, "UIEvent", "resize", &[("bubbles", false.into()), ("cancelable", false.into())]);
        if let Err(error) = bind::window::media_changed(context) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }

    /// The colour scheme the window prefers: `prefers-color-scheme`.
    pub fn set_color_scheme(&self, runtime: &mut verve::Runtime, dark: bool) {
        {
            let mut state = self.state.borrow_mut();
            if state.env.dark == dark {
                return;
            }
            state.env.dark = dark;
            state.sheets_changed();
        }
        let context = runtime.context();
        if let Err(error) = bind::window::media_changed(context) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }

    /// Where the drawer painted the nodes it measured, each its border box in viewport
    /// coordinates: what `getBoundingClientRect` and the sizes answer from now on, replacing
    /// what was reported before. The resize and intersection observers whose targets moved
    /// hear it.
    pub fn set_layout(&self, runtime: &mut verve::Runtime, rects: impl IntoIterator<Item = (NodeId, Rect)>) {
        {
            let mut state = self.state.borrow_mut();
            state.rects.clear();
            state.rects.extend(rects);
        }
        self.deliver_layout(runtime);
    }

    /// [`Document::set_layout`] for some nodes only, the others kept.
    pub fn update_layout(&self, runtime: &mut verve::Runtime, rects: impl IntoIterator<Item = (NodeId, Rect)>) {
        self.state.borrow_mut().rects.extend(rects);
        self.deliver_layout(runtime);
    }

    fn deliver_layout(&self, runtime: &mut verve::Runtime) {
        let context = runtime.context();
        if bind::observers::watching_layout(context)
            && let Err(error) = bind::observers::deliver_layout(context)
        {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }

    /// The scrolling the drawer did (by the person, or for a [`Request::Scroll`]): each box's
    /// offset `[left, top]`, the document's root element for the window's. Every box whose
    /// offset moved hears `scroll`.
    pub fn set_scroll_offsets(&self, runtime: &mut verve::Runtime, offsets: impl IntoIterator<Item = (NodeId, [f32; 2])>) {
        let moved: Vec<NodeId> = {
            let mut state = self.state.borrow_mut();
            let mut moved = Vec::new();
            for (node, offset) in offsets {
                if state.scroll_offsets.get(&node) != Some(&offset) {
                    state.scroll_offsets.insert(node, offset);
                    moved.push(node);
                }
            }
            moved
        };
        let context = runtime.context();
        let root = shared(context).borrow().dom.html;
        for node in moved {
            let result = if node == root {
                let document = bind::node::wrap(context, shared(context).borrow().dom.document);
                bind::events::fire(context, &document, "Event", "scroll", &[("bubbles", true.into())])
            } else {
                bind::events::fire_at(context, node, "Event", "scroll", &[("bubbles", false.into())], true)
            };
            if let Err(error) = result {
                verve::report(context, &error);
            }
        }
        runtime.run_microtasks();
    }

    // ─── Time ───────────────────────────────────────────────────────────────────────────────

    /// A frame is being drawn ([`FrameClock::Host`]): the `requestAnimationFrame` callbacks run
    /// with `now`, and the resize and intersection observers hear what moved. Answers how many
    /// callbacks ran.
    pub fn frame(&self, runtime: &mut verve::Runtime, now: f64) -> usize {
        let context = runtime.context();
        let ran = bind::window::run_frame(context, now);
        runtime.run_microtasks();
        self.deliver_layout(runtime);
        ran
    }

    /// Whether `requestAnimationFrame` callbacks are waiting.
    pub fn wants_frame(&self) -> bool {
        !self.state.borrow().frames.is_empty()
    }

    /// Reclaims every detached tree nothing can reach any more: no object of a node in it is
    /// alive, and the document holds none of it. Answers how many nodes were freed.
    pub fn sweep(&self, runtime: &mut verve::Runtime) -> usize {
        runtime.run_microtasks();
        runtime.collect_garbage();
        let roots = self.state.borrow().dom.detached_roots();
        let mut freed_all = Vec::new();
        for root in roots {
            let alive = {
                let state = self.state.borrow();
                state
                    .dom
                    .subtree(root)
                    .iter()
                    .any(|id| state.wrappers.get(id).is_some_and(|w| w.is_alive()))
                    || state.template_contents.values().any(|f| *f == root)
                    || state.shadow_hosts.contains_key(&root)
            };
            if alive {
                continue;
            }
            let freed = self.state.borrow_mut().dom.free(root);
            freed_all.extend(freed);
        }
        if freed_all.is_empty() {
            return 0;
        }
        let gone: HashSet<NodeId> = freed_all.iter().copied().collect();
        {
            let mut state = self.state.borrow_mut();
            for id in &freed_all {
                state.wrappers.remove(id);
                state.links.remove(id);
                state.rects.remove(id);
                state.scroll_offsets.remove(id);
                state.custom_nodes.remove(id);
            }
            state.template_contents.retain(|t, f| !gone.contains(t) && !gone.contains(f));
            for observer in &mut state.observers {
                observer.targets.retain(|t| !gone.contains(&t.node));
            }
        }
        let mut view = self.view.borrow_mut();
        view.computed.forget(&freed_all);
        view.changed.retain(|id| !gone.contains(id));
        view.restyled.retain(|id| !gone.contains(id));
        view.partitions.retain(|id| !gone.contains(id));
        view.removed.extend(freed_all.iter().copied());
        freed_all.len()
    }

    // ─── What a person does ─────────────────────────────────────────────────────────────────

    /// The element under `(x, y)`, from what was last painted: what the pointer's events
    /// target. `None` outside every painted box.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        hit_test(&self.state.borrow(), x, y).first().copied()
    }

    fn pointer_target(&self, pointer: &Pointer) -> NodeId {
        let state = self.state.borrow();
        hit_test(&state, pointer.x, pointer.y)
            .first()
            .copied()
            .unwrap_or(state.dom.body)
    }

    fn pointer_init(pointer: &Pointer, buttons: u16) -> Vec<(&'static str, JsValue)> {
        vec![
            ("clientX", f64::from(pointer.x).into()),
            ("clientY", f64::from(pointer.y).into()),
            ("screenX", f64::from(pointer.x).into()),
            ("screenY", f64::from(pointer.y).into()),
            ("pageX", f64::from(pointer.x).into()),
            ("pageY", f64::from(pointer.y).into()),
            ("button", pointer.button.into()),
            ("buttons", buttons.into()),
            ("ctrlKey", pointer.modifiers.ctrl.into()),
            ("shiftKey", pointer.modifiers.shift.into()),
            ("altKey", pointer.modifiers.alt.into()),
            ("metaKey", pointer.modifiers.meta.into()),
            ("detail", pointer.count.max(1).into()),
        ]
    }

    fn fire(&self, runtime: &mut verve::Runtime, node: NodeId, class: &str, kind: &str, init: &[(&str, JsValue)]) -> bool {
        let context = runtime.context();
        match bind::events::fire_at(context, node, class, kind, init, true) {
            Ok(allowed) => allowed,
            Err(error) => {
                verve::report(context, &error);
                true
            }
        }
    }

    /// The pointer moved to `pointer`'s place: the elements it left hear `pointerout`/
    /// `mouseout` and `pointerleave`/`mouseleave`, the ones it came onto `pointerover`/
    /// `mouseover` and `pointerenter`/`mouseenter`, a `:hover` rule follows, and the element
    /// under it hears `pointermove` and `mousemove`.
    pub fn pointer_move(&self, runtime: &mut verve::Runtime, pointer: &Pointer) {
        let target = self.pointer_target(pointer);
        self.hover_node(runtime, Some(target), pointer);
        let init = Self::pointer_init(pointer, 0);
        self.fire(runtime, target, "PointerEvent", "pointermove", &init);
        self.fire(runtime, target, "MouseEvent", "mousemove", &init);
        runtime.run_microtasks();
    }

    /// The pointer left the document.
    pub fn pointer_leave(&self, runtime: &mut verve::Runtime) {
        self.hover_node(runtime, None, &Pointer::default());
        runtime.run_microtasks();
    }

    /// The pointer is over `node` now (or over nothing): the hover events and `:hover`, without
    /// hit testing — for a drawer that knows which node is under the pointer itself.
    pub fn hover(&self, runtime: &mut verve::Runtime, node: Option<NodeId>, pointer: &Pointer) {
        self.hover_node(runtime, node, pointer);
        runtime.run_microtasks();
    }

    fn hover_node(&self, runtime: &mut verve::Runtime, node: Option<NodeId>, pointer: &Pointer) {
        let previous = self.view.borrow().over;
        if previous == node {
            return;
        }
        self.view.borrow_mut().over = node;
        let path: Vec<NodeId> = {
            let state = self.state.borrow();
            match node {
                Some(node) => {
                    let mut path = vec![node];
                    let mut at = state.dom.parent_element(node);
                    while let Some(parent) = at {
                        path.push(parent);
                        at = state.dom.parent_element(parent);
                    }
                    path
                }
                None => Vec::new(),
            }
        };
        let (left, entered) = self.state.borrow_mut().dom.set_hovered(path);
        let context = runtime.context();
        let related = |n: Option<NodeId>, cx: &mut Context| n.map_or(JsValue::null(), |n| bind::node::wrap(cx, n));
        let mut init = Self::pointer_init(pointer, 0);
        if let Some(previous) = previous.filter(|p| self.state.borrow().dom.exists(*p)) {
            let to = related(node, context);
            init.push(("relatedTarget", to));
            self.fire(runtime, previous, "PointerEvent", "pointerout", &init);
            self.fire(runtime, previous, "MouseEvent", "mouseout", &init);
            for id in &left {
                self.fire(runtime, *id, "PointerEvent", "pointerleave", &init);
                self.fire(runtime, *id, "MouseEvent", "mouseleave", &init);
            }
            init.pop();
        }
        if let Some(node) = node {
            let from = related(previous, runtime.context());
            init.push(("relatedTarget", from));
            self.fire(runtime, node, "PointerEvent", "pointerover", &init);
            self.fire(runtime, node, "MouseEvent", "mouseover", &init);
            for id in entered.iter().rev() {
                self.fire(runtime, *id, "PointerEvent", "pointerenter", &init);
                self.fire(runtime, *id, "MouseEvent", "mouseenter", &init);
            }
        }
    }

    /// A button pressed at `pointer`: `pointerdown` and `mousedown` at the element under it,
    /// `:active`, and — unless `mousedown` was prevented — the focus moving to it (or to what
    /// focusable thing it is in; away, for none). Answers whether nothing prevented it.
    pub fn pointer_down(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> bool {
        let target = self.pointer_target(pointer);
        self.press_down(runtime, target, pointer)
    }

    /// [`Document::pointer_down`] on `node`, without hit testing.
    pub fn pointer_down_on(&self, runtime: &mut verve::Runtime, node: NodeId, pointer: &Pointer) -> bool {
        self.press_down(runtime, node, pointer)
    }

    fn press_down(&self, runtime: &mut verve::Runtime, target: NodeId, pointer: &Pointer) -> bool {
        self.hover_node(runtime, Some(target), pointer);
        let buttons = match pointer.button {
            1 => 4,
            2 => 2,
            _ => 1,
        };
        let init = Self::pointer_init(pointer, buttons);
        let pointer_allowed = self.fire(runtime, target, "PointerEvent", "pointerdown", &init);
        let allowed = pointer_allowed && self.fire(runtime, target, "MouseEvent", "mousedown", &init);
        {
            let mut state = self.state.borrow_mut();
            state.dom.active = Some(target);
            state.dom.mark(target);
        }
        self.view.borrow_mut().pressed = Some(target);
        if allowed && pointer.button == 0 {
            let context = runtime.context();
            let focus = {
                let mut at = Some(target);
                let mut found = None;
                while let Some(node) = at {
                    if bind::events::focusable(context, node) {
                        found = Some(node);
                        break;
                    }
                    at = self.state.borrow().dom.parent(node);
                }
                found
            };
            if let Err(error) = bind::events::set_focus_from(context, focus, true) {
                verve::report(context, &error);
            }
            if let Some(focus) = focus {
                let field = self.state.borrow().dom.get(focus).is_some_and(|n| n.draft.is_some());
                if field {
                    bind::node::with_dom(runtime.context(), |dom| bind::element::ensure_draft(dom, focus));
                }
            }
        }
        runtime.run_microtasks();
        allowed
    }

    /// The button let go at `pointer`: `pointerup` and `mouseup`, then what the button makes of
    /// it where it went down and came up — `click` (with its activation: a box ticked, a form
    /// submitted, a label's control pressed) and `dblclick` on the second of two for the
    /// primary button, `auxclick` for the middle, `contextmenu` for the secondary. Answers
    /// whether nothing prevented the click.
    pub fn pointer_up(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> bool {
        let target = self.pointer_target(pointer);
        self.release(runtime, target, pointer)
    }

    /// [`Document::pointer_up`] on `node`, without hit testing.
    pub fn pointer_up_on(&self, runtime: &mut verve::Runtime, node: NodeId, pointer: &Pointer) -> bool {
        self.release(runtime, node, pointer)
    }

    fn release(&self, runtime: &mut verve::Runtime, target: NodeId, pointer: &Pointer) -> bool {
        let init = Self::pointer_init(pointer, 0);
        self.fire(runtime, target, "PointerEvent", "pointerup", &init);
        self.fire(runtime, target, "MouseEvent", "mouseup", &init);
        {
            let mut state = self.state.borrow_mut();
            if let Some(active) = state.dom.active.take() {
                state.dom.mark(active);
            }
        }
        let pressed = self.view.borrow_mut().pressed.take();
        // The click goes to what both the press and the release were in.
        let clicked = match pressed {
            Some(down) => {
                let dom = &self.state.borrow().dom;
                let mut at = Some(target);
                let mut found = None;
                while let Some(node) = at {
                    if dom.contains(node, down) {
                        found = Some(node);
                        break;
                    }
                    at = dom.parent(node);
                }
                found
            }
            None => Some(target),
        };
        let mut allowed = true;
        if let Some(clicked) = clicked {
            match pointer.button {
                0 => {
                    let context = runtime.context();
                    allowed = bind::events::click(context, clicked, &init, true).unwrap_or_else(|error| {
                        verve::report(context, &error);
                        true
                    });
                    if pointer.count == 2 {
                        self.fire(runtime, clicked, "MouseEvent", "dblclick", &init);
                    }
                }
                1 => {
                    allowed = self.fire(runtime, clicked, "PointerEvent", "auxclick", &init);
                }
                _ => {
                    allowed = self.fire(runtime, clicked, "PointerEvent", "contextmenu", &init);
                }
            }
        }
        runtime.run_microtasks();
        allowed
    }

    /// A press on `node` from start to end — down, up, click — as a pointer at its painted
    /// box's middle: for a drawer that hit tests itself, and for tests.
    pub fn press(&self, runtime: &mut verve::Runtime, node: NodeId) -> bool {
        let rect = self.state.borrow().rects.get(&node).copied().unwrap_or_default();
        let pointer = Pointer {
            x: rect.x + rect.width / 2.,
            y: rect.y + rect.height / 2.,
            count: 1,
            ..Pointer::default()
        };
        self.press_down(runtime, node, &pointer);
        self.release(runtime, node, &pointer)
    }

    /// The wheel turned at `pointer`: `wheel` at the element under it. Answers whether nothing
    /// prevented it — whether the drawer should scroll.
    pub fn wheel(&self, runtime: &mut verve::Runtime, pointer: &Pointer, delta_x: f32, delta_y: f32) -> bool {
        let target = self.pointer_target(pointer);
        let mut init = Self::pointer_init(pointer, 0);
        init.push(("deltaX", f64::from(delta_x).into()));
        init.push(("deltaY", f64::from(delta_y).into()));
        let allowed = self.fire(runtime, target, "WheelEvent", "wheel", &init);
        runtime.run_microtasks();
        allowed
    }

    /// The element with the keyboard.
    pub fn focused(&self) -> Option<NodeId> {
        self.state.borrow().dom.focused
    }

    /// Gives `node` the focus (or takes it away), with the focus events.
    pub fn focus(&self, runtime: &mut verve::Runtime, node: Option<NodeId>) {
        let context = runtime.context();
        if let Err(error) = bind::events::set_focus_from(context, node, true) {
            verve::report(context, &error);
        }
        runtime.run_microtasks();
    }

    fn key_init(key: &Key, char_code: u32) -> Vec<(&'static str, JsValue)> {
        let code = key_code(&key.key);
        vec![
            ("key", bind::text(&key.key)),
            ("code", bind::text(&key.code)),
            ("keyCode", code.into()),
            ("which", (if char_code != 0 { char_code } else { code }).into()),
            ("charCode", char_code.into()),
            ("repeat", key.repeat.into()),
            ("ctrlKey", key.modifiers.ctrl.into()),
            ("shiftKey", key.modifiers.shift.into()),
            ("altKey", key.modifiers.alt.into()),
            ("metaKey", key.modifiers.meta.into()),
        ]
    }

    fn key_target(&self) -> NodeId {
        let state = self.state.borrow();
        state
            .dom
            .focused
            .filter(|f| state.dom.is_connected(*f))
            .unwrap_or(state.dom.body)
    }

    /// A key pressed: `keydown` at the focused element (or the body), and — unless prevented —
    /// what the key does there: a character typed into a field (`keypress`, `beforeinput`,
    /// `input`), Backspace and Delete, the caret's keys, Enter submitting a form or pressing a
    /// button, Tab moving the focus. Answers whether nothing prevented it.
    pub fn key_down(&self, runtime: &mut verve::Runtime, key: &Key) -> bool {
        let target = self.key_target();
        let allowed = self.fire(runtime, target, "KeyboardEvent", "keydown", &Self::key_init(key, 0));
        if allowed {
            self.key_default(runtime, target, key);
        }
        runtime.run_microtasks();
        allowed
    }

    /// A key let go: `keyup` — and Space on a button or a box presses it, as it does when let
    /// go in a browser.
    pub fn key_up(&self, runtime: &mut verve::Runtime, key: &Key) -> bool {
        let target = self.key_target();
        let allowed = self.fire(runtime, target, "KeyboardEvent", "keyup", &Self::key_init(key, 0));
        if allowed && key.key == " " && self.is_button_like(target) {
            let context = runtime.context();
            let _ = bind::events::click(context, target, &[], true);
        }
        runtime.run_microtasks();
        allowed
    }

    fn is_button_like(&self, id: NodeId) -> bool {
        let state = self.state.borrow();
        let dom = &state.dom;
        match dom.get(id).and_then(|n| n.tag()) {
            Some("button" | "summary") => true,
            Some("input") => matches!(
                bind::element::input_type(dom, id).as_str(),
                "checkbox" | "radio" | "button" | "submit" | "reset" | "image"
            ),
            _ => false,
        }
    }

    fn is_text_field(&self, id: NodeId) -> bool {
        let state = self.state.borrow();
        let dom = &state.dom;
        let Some(node) = dom.get(id) else { return false };
        if node.attr("readonly").is_some() || node.attr("disabled").is_some() {
            return false;
        }
        match node.tag() {
            Some("textarea") => true,
            Some("input") => !matches!(
                bind::element::input_type(dom, id).as_str(),
                "checkbox" | "radio" | "button" | "submit" | "reset" | "image" | "file" | "hidden" | "range" | "color"
            ),
            _ => false,
        }
    }

    fn key_default(&self, runtime: &mut verve::Runtime, target: NodeId, key: &Key) {
        let field = self.is_text_field(target);
        let chord = key.modifiers.ctrl || key.modifiers.meta;
        let printable = key.key.chars().count() == 1 && !chord;
        if printable {
            let char_code = key.key.chars().next().map_or(0, |c| c as u32);
            let allowed = self.fire(runtime, target, "KeyboardEvent", "keypress", &Self::key_init(key, char_code));
            if allowed && field {
                self.insert_into(runtime, target, &key.key, "insertText");
            }
            return;
        }
        let textarea = self.state.borrow().dom.get(target).and_then(|n| n.tag()) == Some("textarea");
        match key.key.as_str() {
            "Enter" if field && textarea => self.insert_into(runtime, target, "\n", "insertLineBreak"),
            "Enter" if field => self.implicit_submit(runtime, target),
            "Enter" if self.is_button_like(target) || self.state.borrow().dom.get(target).is_some_and(|n| n.tag() == Some("a") && n.attr("href").is_some()) => {
                let checkbox = self.state.borrow().dom.get(target).and_then(|n| n.tag()) == Some("input");
                if !checkbox {
                    let context = runtime.context();
                    let _ = bind::events::click(context, target, &[], true);
                }
            }
            "Backspace" | "Delete" if field => {
                let word = chord || key.modifiers.alt;
                let backward = key.key == "Backspace";
                self.delete_in(runtime, target, backward, word);
            }
            "ArrowLeft" | "ArrowRight" | "Home" | "End" if field => {
                let shift = key.modifiers.shift;
                let word = chord || key.modifiers.alt;
                bind::node::with_dom(runtime.context(), |dom| {
                    bind::element::ensure_draft(dom, target);
                    if let Some(draft) = dom.get_mut(target).and_then(|n| n.draft.as_mut()) {
                        match (key.key.as_str(), shift, word) {
                            ("ArrowLeft", false, false) => draft.left(),
                            ("ArrowLeft", false, true) => draft.word_left(),
                            ("ArrowLeft", true, false) => draft.extend_left(),
                            ("ArrowLeft", true, true) => draft.extend_word_left(),
                            ("ArrowRight", false, false) => draft.right(),
                            ("ArrowRight", false, true) => draft.word_right(),
                            ("ArrowRight", true, false) => draft.extend_right(),
                            ("ArrowRight", true, true) => draft.extend_word_right(),
                            ("Home", false, _) => draft.home(),
                            ("Home", true, _) => draft.extend_home(),
                            ("End", false, _) => draft.end(),
                            _ => draft.extend_end(),
                        };
                    }
                });
            }
            "a" | "A" if field && chord => {
                bind::node::with_dom(runtime.context(), |dom| {
                    bind::element::ensure_draft(dom, target);
                    if let Some(draft) = dom.get_mut(target).and_then(|n| n.draft.as_mut()) {
                        draft.select_all();
                    }
                });
            }
            "Tab" => self.tab(runtime, target, key.modifiers.shift),
            _ => {}
        }
    }

    /// Enter in a form's line of text: the form's first submit button pressed, or the form
    /// submitted when it has none.
    fn implicit_submit(&self, runtime: &mut verve::Runtime, field: NodeId) {
        let (form, button) = {
            let state = self.state.borrow();
            let dom = &state.dom;
            let mut at = dom.parent(field);
            let mut form = None;
            while let Some(node) = at {
                if dom.get(node).and_then(|n| n.tag()) == Some("form") {
                    form = Some(node);
                    break;
                }
                at = dom.parent(node);
            }
            let button = form.and_then(|f| {
                dom.descendants(f).into_iter().find(|n| match dom.get(*n).and_then(|x| x.tag()) {
                    Some("button") => !matches!(dom.get_attr(*n, "type").map(|t| t.to_ascii_lowercase()).as_deref(), Some("button" | "reset")),
                    Some("input") => matches!(bind::element::input_type(dom, *n).as_str(), "submit" | "image"),
                    _ => false,
                })
            });
            (form, button)
        };
        let context = runtime.context();
        let result = match (form, button) {
            (_, Some(button)) => bind::events::click(context, button, &[], true).map(|_| ()),
            (Some(form), None) => bind::events::submit(context, form, None),
            _ => Ok(()),
        };
        if let Err(error) = result {
            verve::report(context, &error);
        }
    }

    /// Tab: the focus to the next focusable element in document order (`tabindex` order
    /// first), or the previous with Shift.
    fn tab(&self, runtime: &mut verve::Runtime, from: NodeId, backward: bool) {
        let context = runtime.context();
        let candidates: Vec<NodeId> = {
            let state = self.state.borrow();
            let dom = &state.dom;
            dom.descendants(dom.document)
                .into_iter()
                .filter(|n| dom.get_attr(*n, "tabindex").and_then(|t| t.parse::<i32>().ok()).is_none_or(|t| t >= 0))
                .collect()
        };
        let candidates: Vec<NodeId> = candidates.into_iter().filter(|n| bind::events::focusable(context, *n)).collect();
        if candidates.is_empty() {
            return;
        }
        let mut ordered: Vec<(i32, usize, NodeId)> = {
            let state = self.state.borrow();
            candidates
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let index = state.dom.get_attr(*n, "tabindex").and_then(|t| t.parse::<i32>().ok()).unwrap_or(0);
                    (if index > 0 { index } else { i32::MAX }, i, *n)
                })
                .collect()
        };
        ordered.sort();
        let at = ordered.iter().position(|(_, _, n)| *n == from);
        let next = match (at, backward) {
            (Some(at), false) => ordered.get(at + 1).or(ordered.first()),
            (Some(at), true) => if at == 0 { ordered.last() } else { ordered.get(at - 1) },
            (None, false) => ordered.first(),
            (None, true) => ordered.last(),
        };
        if let Some((_, _, next)) = next.copied() {
            if let Err(error) = bind::events::set_focus_from(context, Some(next), true) {
                verve::report(context, &error);
            }
        }
    }

    fn insert_into(&self, runtime: &mut verve::Runtime, field: NodeId, text: &str, input_type: &str) {
        let init = [
            ("data", bind::text(text)),
            ("inputType", bind::text(input_type)),
        ];
        let allowed = self.fire(runtime, field, "InputEvent", "beforeinput", &init);
        if !allowed {
            return;
        }
        let changed = bind::node::with_dom(runtime.context(), |dom| {
            bind::element::ensure_draft(dom, field);
            let max = dom.get_attr(field, "maxlength").and_then(|m| m.parse::<usize>().ok());
            let single_line = dom.get(field).and_then(|n| n.tag()) == Some("input");
            let Some(node) = dom.get_mut(field) else { return false };
            let Some(draft) = node.draft.as_mut() else { return false };
            let mut text = if single_line { text.replace(['\r', '\n'], "") } else { text.to_owned() };
            if let Some(max) = max {
                let room = max.saturating_sub(draft.text().encode_utf16().count() - draft.selected_text().encode_utf16().count());
                let fits: String = text.chars().scan(0, |used, c| {
                    *used += c.len_utf16();
                    (*used <= room).then_some(c)
                }).collect();
                text = fits;
            }
            if text.is_empty() && !draft.has_selection() {
                return false;
            }
            draft.insert(&text);
            true
        });
        if changed {
            self.fire(runtime, field, "InputEvent", "input", &init);
        }
    }

    fn delete_in(&self, runtime: &mut verve::Runtime, field: NodeId, backward: bool, word: bool) {
        let input_type = match (backward, word) {
            (true, false) => "deleteContentBackward",
            (true, true) => "deleteWordBackward",
            (false, false) => "deleteContentForward",
            (false, true) => "deleteWordForward",
        };
        let init = [("inputType", bind::text(input_type)), ("data", JsValue::null())];
        if !self.fire(runtime, field, "InputEvent", "beforeinput", &init) {
            return;
        }
        let changed = bind::node::with_dom(runtime.context(), |dom| {
            bind::element::ensure_draft(dom, field);
            let Some(draft) = dom.get_mut(field).and_then(|n| n.draft.as_mut()) else { return false };
            match (backward, word) {
                (true, false) => draft.backspace(),
                (true, true) => draft.backspace_word(),
                (false, _) => draft.delete(),
            }
        });
        if changed {
            self.fire(runtime, field, "InputEvent", "input", &init);
        }
    }

    /// Text typed key by key into whatever has the focus: each character's `keydown`,
    /// `keypress`, `beforeinput`, `input` and `keyup`, as a person's typing makes them.
    pub fn type_text(&self, runtime: &mut verve::Runtime, text: &str) {
        for c in text.chars() {
            let key = if c == '\n' { Key::named("Enter") } else { Key::char(c) };
            self.key_down(runtime, &key);
            self.key_up(runtime, &key);
        }
    }

    /// Text put into the focused field at once — what an input method commits, or a paste:
    /// `beforeinput` and `input`, no keys.
    pub fn insert_text(&self, runtime: &mut verve::Runtime, text: &str) {
        let target = self.key_target();
        if self.is_text_field(target) {
            self.insert_into(runtime, target, text, "insertText");
        }
        runtime.run_microtasks();
    }

    /// Does `edit` to a field's text — a drawer that edits fields itself (its own caret, its
    /// own input method) — and, when the text changed, dispatches the `input` event scripts
    /// hear (`onChange`, in React).
    pub fn edit_field(&self, runtime: &mut verve::Runtime, node: NodeId, edit: impl FnOnce(&mut Draft)) {
        let changed = bind::node::with_dom(runtime.context(), |dom| {
            bind::element::ensure_draft(dom, node);
            let Some(draft) = dom.get_mut(node).and_then(|n| n.draft.as_mut()) else {
                return false;
            };
            let before = draft.text().to_owned();
            edit(draft);
            draft.text() != before
        });
        if changed {
            self.fire(runtime, node, "InputEvent", "input", &[("inputType", bind::text("insertText"))]);
        }
        runtime.run_microtasks();
    }

    /// A field's text, caret and selection.
    pub fn draft(&self, node: NodeId) -> Option<Draft> {
        let node_id = node;
        let state = self.state.borrow();
        let node = state.dom.get(node)?;
        let draft = node.draft.clone()?;
        if !node.dirty_value {
            return Some(Draft::new(state.dom.value(node_id)));
        }
        Some(draft)
    }

    /// The person is done with a field: its `change`.
    pub fn commit_field(&self, runtime: &mut verve::Runtime, node: NodeId) {
        self.fire(runtime, node, "Event", "change", &[]);
        runtime.run_microtasks();
    }

    /// One of a `<select>`'s options chosen, by its value: `input` and `change`.
    pub fn choose(&self, runtime: &mut verve::Runtime, select: NodeId, value: &str) {
        let changed = bind::node::with_dom(runtime.context(), |dom| dom.set_value(select, value));
        if changed {
            self.fire(runtime, select, "Event", "input", &[]);
            self.fire(runtime, select, "Event", "change", &[]);
        }
        runtime.run_microtasks();
    }
}

/// The legacy `keyCode` of a key, as browsers answer it.
fn key_code(key: &str) -> u32 {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' => c.to_ascii_uppercase() as u32,
            'A'..='Z' | '0'..='9' => c as u32,
            ' ' => 32,
            ';' | ':' => 186,
            '=' | '+' => 187,
            ',' | '<' => 188,
            '-' | '_' => 189,
            '.' | '>' => 190,
            '/' | '?' => 191,
            '`' | '~' => 192,
            '[' | '{' => 219,
            '\\' | '|' => 220,
            ']' | '}' => 221,
            '\'' | '"' => 222,
            _ => 0,
        };
    }
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Shift" => 16,
        "Control" => 17,
        "Alt" => 18,
        "Pause" => 19,
        "CapsLock" => 20,
        "Escape" => 27,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Insert" => 45,
        "Delete" => 46,
        "Meta" => 91,
        "F1" => 112,
        "F2" => 113,
        "F3" => 114,
        "F4" => 115,
        "F5" => 116,
        "F6" => 117,
        "F7" => 118,
        "F8" => 119,
        "F9" => 120,
        "F10" => 121,
        "F11" => 122,
        "F12" => 123,
        _ => 0,
    }
}

/// What an embedder that extends the document builds with: its own methods on the DOM's
/// classes — su-editor's `createEl` and `setText`, say — reading and changing the same arena.
pub mod host {
    use boa_engine::{Context, JsResult, JsValue};

    pub use crate::bind::{Accessor, Method};
    use crate::dom::{Dom, NodeId};

    /// Lays `methods` and `accessors` over the class named (`"Element"`, `"Node"`,
    /// `"HTMLElement"`, `"Document"`, `"Window"`…), after [`crate::install`]. A class this
    /// document does not have is an error.
    pub fn extend(context: &mut Context, class: &str, methods: &[Method], accessors: &[Accessor]) -> JsResult<()> {
        let prototype = crate::state::shared(context)
            .borrow()
            .class(class)
            .map(|(_, p)| p)
            .ok_or_else(|| crate::bind::type_error(format!("verve-dom has no class {class}")))?;
        crate::bind::extend_prototype(context, &prototype, methods, accessors)
    }

    /// The node `value` is the object of.
    pub fn node_of(value: &JsValue) -> Option<NodeId> {
        crate::bind::node_id(value)
    }

    /// The object of `node`: the same one for as long as a script can see it.
    pub fn wrap(context: &mut Context, node: NodeId) -> JsValue {
        crate::bind::node::wrap(context, node)
    }

    /// Changes the arena from a native: the objects' links and the mutation observers follow.
    pub fn with_dom<R>(context: &mut Context, f: impl FnOnce(&mut Dom) -> R) -> R {
        crate::bind::node::with_dom(context, f)
    }

    pub fn read_dom<R>(context: &Context, f: impl FnOnce(&Dom) -> R) -> R {
        crate::bind::node::read_dom(context, f)
    }

    /// Dispatches a fresh event of `class` (`"MouseEvent"`, `"Event"`…) at `node`, its
    /// `init`'s fields over the kind's defaults. Answers whether nothing prevented it.
    pub fn fire(context: &mut Context, node: NodeId, class: &str, kind: &str, init: &[(&str, JsValue)]) -> JsResult<bool> {
        crate::bind::events::fire_at(context, node, class, kind, init, true)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_snapshot_crosses_threads() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<super::Snapshot>();
        send_sync::<super::Changes>();
    }
}
