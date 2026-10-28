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
use boa_engine::{Context, JsObject, JsValue};

use crate::css::{Environment, MediaQuery, Origin, Sheet, Styler};
use crate::dom::{Dom, NodeId};
use crate::{Rect, Request, StorageBackend};

pub type Shared = Rc<RefCell<State>>;

/// A stylesheet in the cascade, and where it came from.
pub struct SheetRecord {
    pub owner: SheetOwner,
    pub sheet: Sheet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SheetOwner {
    /// The user agent's defaults.
    UserAgent,
    /// One the embedder added, by the name it gave.
    Embedder(String),
    /// A `<style>` element in the document.
    Element(NodeId),
    /// A `new CSSStyleSheet()` adopted by the document, by its number.
    Adopted(u32),
}

/// An observer, and what it watches.
pub struct ObserverRecord {
    pub id: u32,
    pub watch: Watch,
    pub object: WeakJsObject,
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

pub struct State {
    pub dom: Dom,
    pub url: url::Url,
    pub title_fallback: String,
    /// Each node's object, held without keeping it alive.
    pub wrappers: HashMap<NodeId, WeakJsObject>,
    /// The parent each object was linked to (see `bind::node::relink`).
    pub links: HashMap<NodeId, NodeId>,
    /// The classes made, by name: constructor and prototype.
    pub classes: HashMap<&'static str, (JsObject, JsObject)>,
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
    pub adopted_sheets: Vec<(u32, JsObject)>,
    /// Where the drawer last painted each node it measured.
    pub rects: HashMap<NodeId, Rect>,
    pub scroll_offsets: HashMap<NodeId, [f32; 2]>,
    pub requests: Vec<Request>,
    pub media: crate::media::MediaStore,
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
    /// Every `matchMedia` list a script still holds, and whether it matched when last asked.
    pub media_lists: Vec<(WeakJsObject, MediaQuery, bool)>,
    pub shadow_roots: HashMap<NodeId, (NodeId, bool)>,
    pub shadow_hosts: HashMap<NodeId, NodeId>,
    pub custom_elements: Vec<(String, JsObject)>,
    pub custom_nodes: HashSet<NodeId>,
    pub custom_waiting: Vec<(String, JsValue)>,
    pub template_contents: HashMap<NodeId, NodeId>,
    pub fullscreen: Option<NodeId>,
    /// The history's entries: `pushState`'s states and their addresses, and where it stands.
    pub history: Vec<(JsValue, String)>,
    pub history_at: usize,
    pub next_id: u32,
    /// Moves when something outside the arena changed: a sheet, the viewport, the title.
    pub revision: u64,
}

impl State {
    pub fn new(url: url::Url) -> Self {
        Self {
            dom: Dom::new(),
            history: vec![(JsValue::null(), url.to_string())],
            url,
            title_fallback: String::new(),
            wrappers: HashMap::new(),
            links: HashMap::new(),
            classes: HashMap::new(),
            sheets: Vec::new(),
            sheets_key: 1,
            style_texts: Vec::new(),
            style_revision: None,
            styler: None,
            env: Environment::default(),
            constructed_sheets: HashMap::new(),
            adopted_sheets: Vec::new(),
            rects: HashMap::new(),
            scroll_offsets: HashMap::new(),
            requests: Vec::new(),
            media: crate::media::MediaStore::default(),
            local: BTreeMap::new(),
            session: BTreeMap::new(),
            storage: None,
            observers: Vec::new(),
            mutation_queued: false,
            frame_clock: FrameClock::Timer,
            frames: Vec::new(),
            frame_timer: None,
            media_lists: Vec::new(),
            shadow_roots: HashMap::new(),
            shadow_hosts: HashMap::new(),
            custom_elements: Vec::new(),
            custom_nodes: HashSet::new(),
            custom_waiting: Vec::new(),
            template_contents: HashMap::new(),
            fullscreen: None,
            history_at: 0,
            next_id: 1,
            revision: 0,
        }
    }

    pub fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn class(&self, name: &str) -> Option<(JsObject, JsObject)> {
        self.classes.get(name).cloned()
    }

    pub fn prototype(&self, name: &str) -> JsObject {
        self.classes
            .get(name)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| panic!("the class {name} is installed"))
    }

    /// Reads the `<style>` elements and the adopted sheets again, when what they hold changed:
    /// answers whether the cascade's sheets changed.
    pub fn read_style_elements(&mut self) -> bool {
        let revision = self.dom.styles_revision();
        let found = if self.style_revision == Some(revision) {
            None
        } else {
            Some(self.dom.style_sheets())
        };
        self.style_revision = Some(revision);
        // A `<style>` in a document a script made (`DOMParser`) is no sheet of the window's.
        let adopted: Vec<(u32, String)> = self
            .adopted_sheets
            .iter()
            .filter_map(|(id, _)| self.constructed_sheets.get(id).map(|r| (*id, r.join("\n"))))
            .collect();
        let known_adopted: Vec<(u32, String)> = self
            .sheets
            .iter()
            .filter_map(|r| match r.owner {
                SheetOwner::Adopted(id) => Some((id, r.sheet.source().to_owned())),
                _ => None,
            })
            .collect();
        if found
            .as_ref()
            .is_none_or(|found| *found == self.style_texts)
            && adopted == known_adopted
        {
            return false;
        }
        let found = found.unwrap_or_else(|| self.style_texts.clone());
        self.sheets
            .retain(|r| !matches!(r.owner, SheetOwner::Element(_) | SheetOwner::Adopted(_)));
        for (id, text) in &found {
            self.sheets.push(SheetRecord {
                owner: SheetOwner::Element(*id),
                sheet: Sheet::parse(text),
            });
        }
        for (id, text) in adopted {
            self.sheets.push(SheetRecord {
                owner: SheetOwner::Adopted(id),
                sheet: Sheet::parse(&text),
            });
        }
        self.style_texts = found;
        self.sheets_changed();
        true
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
        let sheets: Vec<(Origin, &Sheet)> = self
            .sheets
            .iter()
            .map(|r| {
                let origin = if r.owner == SheetOwner::UserAgent {
                    Origin::UserAgent
                } else {
                    Origin::Author
                };
                (origin, &r.sheet)
            })
            .collect();
        let styler = Rc::new(Styler::new(&sheets, &self.env));
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
