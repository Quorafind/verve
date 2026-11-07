//! What the document knows apart from the engine: the arena, the stylesheets, where the drawer
//! last painted each node, the storages, the observers and the rest of the window's state.
//!
//! Kept in the realm's host data as an `Rc<RefCell<State>>`. A native function borrows it for
//! as long as it reads or writes and never while JavaScript runs: a callback a native calls may
//! itself call a native, and that one must find the state free.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use boa_engine::object::WeakJsObject;
use boa_engine::object::builtins::JsFunction;
use boa_engine::{Context, JsObject, JsValue};

use crate::bind::custom::Registry;
use crate::bind::fonts::Fonts;
use crate::bind::slot::Slots;
use crate::css::{AnimationStore, Environment, MediaQuery, Origin, Sheet, Styler};
use crate::dom::{Dom, NodeId};
use crate::{Rect, Request, StorageBackend};
use verve::WorldId;

pub type Shared = Rc<RefCell<State>>;

/// A stylesheet in the cascade, and where it came from.
pub struct SheetRecord {
    pub owner: SheetOwner,
    pub sheet: Sheet,
    /// The tree it applies in: the document, or a shadow root's.
    pub tree: NodeId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetOwner {
    /// The user agent's defaults.
    UserAgent,
    /// One the embedder added, by the name it gave.
    Embedder(String),
    /// One the embedder added as the user's own (`webFrame.insertCSS` with `cssOrigin: 'user'`):
    /// in the cascade between the user agent's and the page's.
    User(String),
    /// A `<style>` element in the document.
    Element(NodeId),
    /// A `new CSSStyleSheet()` adopted by the document, by its number.
    Adopted(u32),
    /// A `<link rel=stylesheet>` in the document.
    Link(NodeId),
    /// A sheet the `<link>`'s `@import`s, by its path of positions (an import of an import
    /// extends its importer's): an import comes before the sheet that imports it.
    Import(NodeId, Vec<usize>),
}

/// An observer, and what it watches.
pub struct ObserverRecord {
    pub id: u32,
    pub watch: Watch,
    pub object: WeakJsObject,
    /// The world the observer was made in: its records are made, and it is told, there.
    pub world: verve::WorldId,
    /// 只有待交付记录临时保活观察者和记录引用的节点。
    pub kept: Vec<JsObject>,
    pub transient: Vec<(NodeId, MutationOptions)>,
    pub targets: Vec<ObservedTarget>,
    /// Mutation records not yet delivered (`takeRecords` takes them).
    pub records: Vec<crate::dom::Mutation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watch {
    Resize,
    Mutation,
    Intersection,
}

#[derive(Clone, Debug)]
pub struct ObservedTarget {
    pub node: NodeId,
    /// The box it was last heard at (resize), or whether it was in sight (intersection).
    pub last: Option<[f32; 4]>,
    pub intersecting: Option<bool>,
    pub options: MutationOptions,
}

/// What `MutationObserver.observe` was asked to watch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MutationOptions {
    pub child_list: bool,
    pub attributes: bool,
    pub character_data: bool,
    pub subtree: bool,
    pub attribute_old_value: bool,
    pub character_data_old_value: bool,
    pub attribute_filter: Option<Vec<String>>,
}

/// How `requestAnimationFrame` is paced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameClock {
    /// On the realm's own timers, every 16 ms while callbacks wait — for a realm nothing draws.
    #[default]
    Timer,
    /// When the drawer says it is drawing a frame: [`crate::Document::frame`].
    Host,
}

/// A `finished` promise that has not been settled, and how to settle it.
pub struct Pending {
    pub promise: JsObject,
    pub resolve: JsFunction,
    pub reject: JsFunction,
}

/// What the realm keeps of an animation's object. While the animation is in the store with its
/// `finished` unsettled the object is held, since a script may have let go of it and the promise
/// it made must still settle; once settled only a weak reference is kept, and the record goes
/// when the object does.
pub struct AnimationRecord {
    pub object: WeakJsObject,
    /// The object, while `finished` waits.
    pub held: Option<JsObject>,
    pub finished: Option<Pending>,
    /// How `finished` was settled.
    pub outcome: Option<crate::css::AnimationOutcome>,
    /// Milliseconds into its effect where it last ended or was cancelled.
    pub ended_at: f64,
}

pub struct State {
    pub dom: Dom,
    pub url: url::Url,
    pub title_fallback: String,
    /// What the window's `navigator` was given (user agent, language), for a world made later.
    pub navigator: (Option<String>, Option<String>),
    /// Each node's object in each world, held without keeping it alive.
    pub wrappers: HashMap<(WorldId, NodeId), WeakJsObject>,
    /// The parent each object was linked to (see `bind::node::relink`).
    pub links: HashMap<(WorldId, NodeId), NodeId>,
    /// The classes each world made, by name: constructor and prototype.
    pub classes: HashMap<(WorldId, &'static str), (JsObject, JsObject)>,
    /// How many worlds there are: the page's, and the isolated ones made for it.
    pub worlds: WorldId,
    pub sheets: Vec<SheetRecord>,
    /// Moves when a sheet comes, goes or changes.
    pub sheets_key: u64,
    /// What the `<style>` elements held when they were last read.
    pub style_texts: Vec<(NodeId, String)>,
    pub style_revision: Option<u64>,
    pub styler: Option<(u64, Rc<Styler>)>,
    pub env: Environment,
    /// Each `new CSSStyleSheet()`'s rules, by its number; and which the document adopted.
    pub constructed_sheets: HashMap<u32, Vec<String>>,
    /// What each tree (a document, or a shadow root) adopted, by the tree's root.
    pub adopted: BTreeMap<NodeId, Vec<(u32, JsObject)>>,
    /// Where the drawer last painted each node it measured.
    pub rects: HashMap<NodeId, Rect>,
    /// Works `rects` out again when a script reads them after the document changed, if a layout
    /// provider is installed ([`crate::Document::set_layout_provider`]).
    pub layout_hook: Option<Rc<dyn Fn()>>,
    /// Answers the boxes of a range of text, if the page's layout can
    /// ([`crate::Document::set_range_provider`]).
    pub range_hook: Option<crate::layout_provider::RangeHook>,
    /// Answers the text position under a point, if the page's layout can
    /// ([`crate::Document::set_position_provider`]).
    pub position_hook: Option<crate::layout_provider::PositionHook>,
    /// `[scrollWidth, scrollHeight]` of each box, as the provider last worked them out.
    pub scroll_extents: HashMap<NodeId, [f32; 2]>,
    /// The revision and viewport the provider last laid the document out for.
    pub laid: Option<(u64, (f32, f32))>,
    pub scroll_offsets: HashMap<NodeId, [f32; 2]>,
    pub requests: Vec<Request>,
    pub media: crate::media::MediaStore,
    /// The pixels and objects of the page's canvases.
    #[cfg(feature = "canvas")]
    pub canvas: crate::canvas::CanvasStore,
    /// The images the page has bytes for, and what each `<img>` names.
    pub images: crate::canvas::ImageStore,
    pub local: BTreeMap<String, String>,
    pub session: BTreeMap<String, String>,
    pub storage: Option<Box<dyn StorageBackend>>,
    pub observers: Vec<ObserverRecord>,
    /// A microtask is queued to deliver mutation records.
    pub mutation_queued: bool,
    pub frame_clock: FrameClock,
    pub frames: Vec<(u32, JsValue)>,
    /// The timer that runs the next frame, on [`FrameClock::Timer`].
    pub frame_timer: Option<u32>,
    /// Every transition and animation the document runs.
    pub animations: AnimationStore,
    /// The objects of the animations a script has been handed, by animation.
    pub animation_records: HashMap<u32, AnimationRecord>,
    /// The time of the last frame, in milliseconds.
    pub now: f64,
    /// Whether any sheet says something about transitions or animations: a realm with a
    /// [`FrameClock::Timer`] only keeps a frame coming for the page that does.
    pub animates: bool,
    /// The revision of the arena a frame was last asked for at.
    pub woken: u64,
    /// Every `matchMedia` list a script still holds, and whether it matched when last asked.
    pub media_lists: Vec<(WeakJsObject, MediaQuery, bool)>,
    pub slots: Slots,
    pub custom: Registry,
    pub template_contents: HashMap<NodeId, NodeId>,
    pub fullscreen: Option<NodeId>,
    pub fonts: Fonts,
    /// Whether the window is out of sight (minimized, hidden): `document.hidden`.
    pub hidden: bool,
    /// Whether the window has the keyboard: `document.hasFocus()`.
    pub window_focused: bool,
    /// The rectangles of the lines an inline box is broken over, as the drawer reported them;
    /// `getClientRects` answers these over the one border box.
    pub fragments: HashMap<NodeId, Vec<crate::Rect>>,
    /// The history's entries: `pushState`'s states and their addresses, and where it stands.
    pub history: Vec<(JsValue, String)>,
    pub history_at: usize,
    /// `base_url()` for the document as it was at a revision: (revision, address, base).
    base_cache: std::cell::RefCell<Option<(u64, url::Url, url::Url)>>,
    pub next_id: u32,
    /// Moves when something outside the arena changed: a sheet, the viewport, the title.
    pub revision: u64,
    /// The page's loading.
    pub load: crate::load::Load,
}

impl State {
    pub fn new(url: url::Url) -> Self {
        Self {
            dom: Dom::new(),
            history: vec![(JsValue::null(), url.to_string())],
            url,
            title_fallback: String::new(),
            navigator: (None, None),
            wrappers: HashMap::new(),
            links: HashMap::new(),
            classes: HashMap::new(),
            worlds: 1,
            sheets: Vec::new(),
            sheets_key: 1,
            style_texts: Vec::new(),
            style_revision: None,
            styler: None,
            env: Environment::default(),
            constructed_sheets: HashMap::new(),
            adopted: BTreeMap::new(),
            rects: HashMap::new(),
            layout_hook: None,
            range_hook: None,
            position_hook: None,
            scroll_extents: HashMap::new(),
            laid: None,
            scroll_offsets: HashMap::new(),
            requests: Vec::new(),
            media: crate::media::MediaStore::default(),
            #[cfg(feature = "canvas")]
            canvas: crate::canvas::CanvasStore::default(),
            images: crate::canvas::ImageStore::default(),
            local: BTreeMap::new(),
            session: BTreeMap::new(),
            storage: None,
            observers: Vec::new(),
            mutation_queued: false,
            frame_clock: FrameClock::Timer,
            frames: Vec::new(),
            frame_timer: None,
            animations: AnimationStore::default(),
            animation_records: HashMap::new(),
            now: 0.,
            animates: false,
            woken: 0,
            media_lists: Vec::new(),
            slots: Default::default(),
            custom: Default::default(),
            template_contents: HashMap::new(),
            fullscreen: None,
            fonts: Default::default(),
            hidden: false,
            window_focused: true,
            fragments: HashMap::new(),
            history_at: 0,
            base_cache: Default::default(),
            next_id: 1,
            revision: 0,
            load: Default::default(),
        }
    }

    /// What relative addresses resolve against: the first `<base href>`, else the document's own.
    pub fn base_url(&self) -> url::Url {
        if let Some((revision, url, base)) = &*self.base_cache.borrow()
            && *revision == self.dom.revision
            && *url == self.url
        {
            return base.clone();
        }
        let base = self.dom.query(self.dom.document, "base[href]");
        let href = base.and_then(|node| self.dom.get_attr(node, "href"));
        let base = href
            .and_then(|href| self.url.join(href.trim()).ok())
            .unwrap_or_else(|| self.url.clone());
        *self.base_cache.borrow_mut() = Some((self.dom.revision, self.url.clone(), base.clone()));
        base
    }

    pub fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// The revision and viewport a layout of the document is for.
    pub fn layout_key(&self) -> (u64, (f32, f32)) {
        (
            self.dom.revision.wrapping_add(self.revision),
            (self.env.width, self.env.height),
        )
    }

    pub fn class(&self, world: WorldId, name: &str) -> Option<(JsObject, JsObject)> {
        self.classes.get(&(world, name)).cloned()
    }

    pub fn prototype(&self, world: WorldId, name: &str) -> JsObject {
        self.classes
            .get(&(world, name))
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| panic!("the class {name} is installed"))
    }

    /// Whether some world has an object for the node alive.
    pub fn has_object(&self, id: NodeId) -> bool {
        (0..self.worlds).any(|w| self.wrappers.get(&(w, id)).is_some_and(|o| o.is_alive()))
    }

    /// Reads the `<style>` elements and the adopted sheets again, when what they hold changed:
    /// answers whether the cascade's sheets changed.
    pub fn read_style_elements(&mut self) -> bool {
        let pruned = self.prune_links();
        let revision = self.dom.styles_revision();
        let found = if self.style_revision == Some(revision) {
            None
        } else {
            Some(self.dom.style_sheets())
        };
        self.style_revision = Some(revision);
        // A `<style>` in a document a script made (`DOMParser`) is no sheet of the window's.
        let adopted: Vec<(u32, String, NodeId)> = self
            .adopted
            .iter()
            .flat_map(|(tree, sheets)| sheets.iter().map(move |(id, _)| (*id, *tree)))
            .filter_map(|(id, tree)| {
                let rules = self.constructed_sheets.get(&id)?;
                Some((id, rules.join("\n"), tree))
            })
            .collect();
        let known_adopted: Vec<(u32, String, NodeId)> = self
            .sheets
            .iter()
            .filter_map(|r| match r.owner {
                SheetOwner::Adopted(id) => Some((id, r.sheet.source().to_owned(), r.tree)),
                _ => None,
            })
            .collect();
        if found
            .as_ref()
            .is_none_or(|found| *found == self.style_texts)
            && adopted == known_adopted
        {
            if pruned {
                self.sheets_changed();
            }
            return pruned;
        }
        let found = found.unwrap_or_else(|| self.style_texts.clone());
        self.sheets
            .retain(|r| !matches!(r.owner, SheetOwner::Element(_) | SheetOwner::Adopted(_)));
        for (id, text) in &found {
            self.sheets.push(SheetRecord {
                owner: SheetOwner::Element(*id),
                sheet: Sheet::parse(text),
                // A `<style>` counts in the tree it is in, a shadow tree's among its own.
                tree: self.dom.root_of(*id),
            });
        }
        for (id, text, tree) in adopted {
            self.sheets.push(SheetRecord {
                owner: SheetOwner::Adopted(id),
                sheet: Sheet::parse(&text),
                tree,
            });
        }
        self.style_texts = found;
        self.order_sheets();
        self.sheets_changed();
        true
    }

    /// Forgets the sheets of `<link>`s that left the document.
    fn prune_links(&mut self) -> bool {
        let before = self.sheets.len();
        let dom = &self.dom;
        self.sheets.retain(|r| match r.owner {
            SheetOwner::Link(node) | SheetOwner::Import(node, _) => dom.is_connected(node),
            _ => true,
        });
        self.sheets.len() != before
    }

    /// The page's sheets in the order of their elements, those it adopted last.
    pub(crate) fn order_sheets(&mut self) {
        let dom = &self.dom;
        let group = |owner: &SheetOwner| match owner {
            SheetOwner::UserAgent | SheetOwner::Embedder(_) | SheetOwner::User(_) => 0,
            SheetOwner::Element(_) | SheetOwner::Link(_) | SheetOwner::Import(..) => 1,
            SheetOwner::Adopted(_) => 2,
        };
        // Where in the page an element's sheets are, and in what order: imports, then its own.
        fn place(owner: &SheetOwner) -> Option<(NodeId, &[usize])> {
            match owner {
                SheetOwner::Element(x) | SheetOwner::Link(x) => Some((*x, &[])),
                SheetOwner::Import(x, path) => Some((*x, path.as_slice())),
                _ => None,
            }
        }
        let before = |a: &SheetRecord, b: &SheetRecord| match (place(&a.owner), place(&b.owner)) {
            (Some((x, px)), Some((y, py))) => dom.order(x, y).then(import_order(px, py)),
            _ => group(&a.owner).cmp(&group(&b.owner)),
        };
        // An insertion sort, stable, in place: the list is nearly in order, and a sort of the
        // standard library would be one more copy of it in the binary.
        for i in 1..self.sheets.len() {
            let mut at = i;
            while at > 0 && before(&self.sheets[at - 1], &self.sheets[at]).is_gt() {
                self.sheets.swap(at - 1, at);
                at -= 1;
            }
        }
    }

    /// Adds the embedder's sheet `owner`, or replaces it: after the user agent's and before the
    /// page's.
    pub fn put_sheet(&mut self, owner: SheetOwner, css: &str) {
        let sheet = Sheet::parse(css);
        match self.sheets.iter_mut().find(|r| r.owner == owner) {
            Some(record) => record.sheet = sheet,
            None => {
                let at = self
                    .sheets
                    .iter()
                    .position(|r| {
                        matches!(r.owner, SheetOwner::Element(_) | SheetOwner::Adopted(_))
                    })
                    .unwrap_or(self.sheets.len());
                let tree = self.dom.document;
                self.sheets.insert(at, SheetRecord { owner, sheet, tree });
            }
        }
        self.sheets_changed();
    }

    pub fn drop_sheet(&mut self, owner: &SheetOwner) {
        self.sheets.retain(|r| r.owner != *owner);
        self.sheets_changed();
    }

    pub fn sheets_changed(&mut self) {
        self.sheets_key = self.sheets_key.wrapping_add(1);
        self.touch();
    }

    /// The cascade of every sheet now, for the environment now.
    pub fn styler(&mut self) -> Rc<Styler> {
        self.read_style_elements();
        if let Some((key, styler)) = &self.styler
            && *key == self.sheets_key
        {
            return styler.clone();
        }
        let sheets: Vec<(Origin, &Sheet, NodeId)> = self
            .sheets
            .iter()
            .map(|r| {
                let origin = match r.owner {
                    SheetOwner::UserAgent => Origin::UserAgent,
                    SheetOwner::User(_) => Origin::User,
                    _ => Origin::Author,
                };
                (origin, &r.sheet, r.tree)
            })
            .collect();
        let styler = Rc::new(Styler::new(&sheets, &self.env));
        self.animates = styler.animates();
        self.styler = Some((self.sheets_key, styler.clone()));
        styler
    }

    /// Writes `localStorage` where it is kept, if anywhere.
    pub fn persist_storage(&self) {
        if let Some(storage) = &self.storage {
            storage.save(&self.local);
        }
    }
}

/// The state, from inside a native function.
/// How two sheets of one `<link>` go, by their paths: the imports of a sheet come before it, in
/// the order it names them.
fn import_order(a: &[usize], b: &[usize]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if a == b {
        Ordering::Equal
    } else if a.starts_with(b) {
        Ordering::Less
    } else if b.starts_with(a) {
        Ordering::Greater
    } else {
        a.cmp(b)
    }
}

/// The class `name` of the world that is running: constructor and prototype.
pub fn class_of(context: &Context, name: &str) -> Option<(JsObject, JsObject)> {
    shared(context)
        .borrow()
        .class(verve::current_world(context), name)
}

/// The prototype of the class `name` of the world that is running.
///
/// # Panics
///
/// The class is not installed.
pub fn class_prototype(context: &Context, name: &str) -> JsObject {
    shared(context)
        .borrow()
        .prototype(verve::current_world(context), name)
}

pub fn shared(context: &Context) -> Shared {
    context
        .get_data::<Shared>()
        .cloned()
        .expect("verve-dom installed its state")
}

/// The state, when a document is installed.
pub fn try_shared(context: &Context) -> Option<Shared> {
    context.get_data::<Shared>().cloned()
}

impl State {
    /// The detached trees among `roots` that are still wanted: some object of a node in them is
    /// alive, or the arena pins one, or a template keeps them through its content.
    pub fn reachable(&self, roots: &[NodeId]) -> HashSet<NodeId> {
        let mut alive = HashSet::from([self.dom.document]);
        let in_use = |id: &NodeId| self.dom.is_pinned(*id) || self.has_object(*id);
        for root in roots {
            if self.dom.shadow_including_tree(*root).iter().any(in_use) {
                alive.insert(*root);
            }
        }
        // A template keeps its content alive, and the other way round only through the objects
        // of the nodes. Only real ownership edges keep a tree.
        let mut edges = HashMap::<NodeId, Vec<NodeId>>::new();
        for (template, content) in &self.template_contents {
            edges
                .entry(self.dom.root_of(*template))
                .or_default()
                .push(*content);
        }
        let mut pending: Vec<NodeId> = alive.iter().copied().collect();
        while let Some(root) = pending.pop() {
            for next in edges.get(&root).into_iter().flatten() {
                if alive.insert(*next) {
                    pending.push(*next);
                }
            }
        }
        alive
    }

    /// Forgets what is kept against the nodes `freed`, which a sweep has freed.
    pub fn forget_nodes(&mut self, freed: &[NodeId], gone: &HashSet<NodeId>) {
        for id in freed {
            for world in 0..self.worlds {
                self.wrappers.remove(&(world, *id));
                self.links.remove(&(world, *id));
            }
            self.rects.remove(id);
            self.fragments.remove(id);
            self.scroll_offsets.remove(id);
            self.custom.forget(*id);
        }
        crate::canvas::forget(self, gone);
        self.load.forget(gone);
        self.template_contents
            .retain(|t, f| !gone.contains(t) && !gone.contains(f));
        if self.fullscreen.is_some_and(|id| gone.contains(&id)) {
            self.fullscreen = None;
        }
        for observer in &mut self.observers {
            observer.targets.retain(|t| !gone.contains(&t.node));
            observer.transient.retain(|(node, _)| !gone.contains(node));
        }
        self.observers
            .retain(|o| o.object.is_alive() && (!o.targets.is_empty() || !o.records.is_empty()));
        self.dom.record_mutations = self
            .observers
            .iter()
            .any(|o| o.watch == Watch::Mutation && !o.targets.is_empty());
    }
}
