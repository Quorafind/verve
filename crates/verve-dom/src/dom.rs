//! The document scripts write into: nodes, their attributes and classes, their inline style,
//! and the state a person's hands leave in them — what is typed in a field, which box is
//! ticked, which element has the focus.
//!
//! Plain Rust, with no engine in it. The JavaScript objects a script holds (`HTMLElement`,
//! `document`) are handles onto nodes here — see `bind` — and whoever draws the document reads
//! it from here, in a [`crate::Snapshot`]. What a node *looks* like is [`crate::css`]'s
//! business; what a node *is* is this module's.
//!
//! Lifted from su-editor's Obsidian host, where the same arena holds every plugin's DOM.

use std::collections::HashSet;
use std::sync::Arc;

use crate::field::Draft;

/// 节点编号保持 32 位：低 24 位是槽位，高 8 位是代数；代数耗尽的槽位不再复用。
pub type NodeId = u32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    /// An element, by its tag in lower case (SVG's mixed-case names are given back their case
    /// when written out; see `html`).
    Element(String),
    Text(String),
    /// `<!-- … -->`: in the tree, never drawn — what frameworks anchor their blocks to.
    Comment(String),
    Fragment,
    /// `<!DOCTYPE html>`, by its name.
    Doctype(String),
}

/// The namespace an element is in, as `namespaceURI` answers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Namespace {
    #[default]
    Html,
    Svg,
    MathMl,
    /// Some other namespace `createElementNS` was given; written out as an HTML element.
    Other,
}

impl Namespace {
    pub const HTML: &'static str = "http://www.w3.org/1999/xhtml";
    pub const SVG: &'static str = "http://www.w3.org/2000/svg";
    pub const MATHML: &'static str = "http://www.w3.org/1998/Math/MathML";

    pub fn from_uri(uri: Option<&str>) -> Self {
        match uri {
            None | Some("") | Some(Self::HTML) => Self::Html,
            Some(Self::SVG) => Self::Svg,
            Some(Self::MATHML) => Self::MathMl,
            Some(_) => Self::Other,
        }
    }

    pub fn uri(self) -> Option<&'static str> {
        match self {
            Self::Html => Some(Self::HTML),
            Self::Svg => Some(Self::SVG),
            Self::MathMl => Some(Self::MATHML),
            Self::Other => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub namespace: Namespace,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Every attribute but `class` and `style`, in the order they were first set.
    pub attrs: Vec<(String, String)>,
    pub classes: Vec<String>,
    /// The inline style, as `style="…"` would hold it: property names in their CSS spelling,
    /// with whether each was declared `!important`.
    pub style: Vec<(String, String)>,
    /// The inline declarations marked `!important`.
    pub important: Vec<String>,
    /// What is typed into an `<input>` or a `<textarea>`, with its caret and selection.
    pub draft: Option<Draft>,
    /// The field's value was set or typed: its `value` attribute is only its default now.
    pub dirty_value: bool,
    pub checked: bool,
    /// `checked` was set or clicked: the `checked` attribute is only its default now.
    pub dirty_checked: bool,
    /// A `<select>`'s chosen option, by its place among the options; `None` until one is
    /// chosen, when the first (or the one marked `selected`) is.
    pub selected: Option<usize>,
}

impl Node {
    fn new(kind: NodeKind) -> Self {
        let draft = match &kind {
            NodeKind::Element(tag) if tag == "input" || tag == "textarea" => Some(Draft::default()),
            _ => None,
        };
        Self {
            kind,
            namespace: Namespace::Html,
            parent: None,
            children: Vec::new(),
            attrs: Vec::new(),
            classes: Vec::new(),
            style: Vec::new(),
            important: Vec::new(),
            draft,
            dirty_value: false,
            checked: false,
            dirty_checked: false,
            selected: None,
        }
    }

    pub fn tag(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Element(tag) => Some(tag),
            _ => None,
        }
    }

    pub fn is_element(&self) -> bool {
        matches!(self.kind, NodeKind::Element(_))
    }

    pub fn is_text(&self) -> bool {
        matches!(self.kind, NodeKind::Text(_))
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn has_class(&self, class: &str) -> bool {
        self.classes.iter().any(|c| c == class)
    }

    pub fn style_value(&self, property: &str) -> Option<&str> {
        self.style
            .iter()
            .find(|(key, _)| key == property)
            .map(|(_, value)| value.as_str())
    }

    /// The text of a text node or a comment.
    pub fn data(&self) -> Option<&str> {
        match &self.kind {
            NodeKind::Text(text) | NodeKind::Comment(text) => Some(text),
            _ => None,
        }
    }
}

/// One place in the arena: the node living there, and how many nodes have lived there before
/// it — so an id kept past its node's life names nothing rather than its successor.
///
/// The node is held by an `Arc`: a copy of the arena — the snapshot a drawer reads, on another
/// thread — shares every node with it, and a node changed afterwards is copied then.
///
/// A place whose generation has run out is retired, never reused: no id is ever given twice.
#[derive(Clone, Debug, Default)]
struct Slot {
    generation: u32,
    /// The node is in the window's document — kept here so asking is not a walk to the root.
    connected: bool,
    node: Option<Arc<Node>>,
}

const INDEX_BITS: u32 = 24;
const INDEX_MASK: u32 = (1 << INDEX_BITS) - 1;
/// 代数耗尽时退休槽位，避免旧句柄指向后来创建的节点。
const MAX_GENERATION: u32 = u8::MAX as u32;

fn split(id: NodeId) -> (usize, u32) {
    ((id & INDEX_MASK) as usize, (id >> INDEX_BITS) as u32)
}

fn join(index: usize, generation: u32) -> NodeId {
    (generation << INDEX_BITS) | index as u32
}

/// How many nodes an arena holds at most, unless told otherwise ([`Dom::max_nodes`]).
pub const DEFAULT_MAX_NODES: usize = 1 << 24;

/// What a script's change was, as a `MutationObserver` hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    ChildList {
        target: NodeId,
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous: Option<NodeId>,
        next: Option<NodeId>,
    },
    Attributes {
        target: NodeId,
        name: String,
        old: Option<String>,
    },
    CharacterData {
        target: NodeId,
        old: String,
    },
}

impl Mutation {
    pub fn target(&self) -> NodeId {
        match self {
            Self::ChildList { target, .. }
            | Self::Attributes { target, .. }
            | Self::CharacterData { target, .. } => *target,
        }
    }
}

/// The document: one tree under `<html>`, every node made but not (yet) put in it, and the
/// documents `DOMParser` makes, kept in one arena for the life of the realm.
///
/// A node's id is its place in the arena and the generation of that place, so ids stay small,
/// lookups are an index, and a freed place is reused without a stale id ever reaching the new
/// node. The arena remembers what changed since it was last asked — [`Dom::take_changes`] — so
/// styles are worked out again and views redrawn for those subtrees alone, and it keeps the
/// roots of detached trees so they can be reclaimed once nothing can reach them.
///
/// Cloning it is cheap — the nodes are shared, see `Slot` — which is what a snapshot of it is.
#[derive(Clone, Debug)]
pub struct Dom {
    slots: Vec<Slot>,
    free: Vec<u32>,
    live: usize,
    pub document: NodeId,
    pub html: NodeId,
    pub head: NodeId,
    pub body: NodeId,
    /// The element with the keyboard, if any.
    pub focused: Option<NodeId>,
    /// The elements the pointer is over, innermost first; see [`Dom::set_hovered`].
    hovered: Vec<NodeId>,
    /// The element a button is held down on: `:active`.
    pub active: Option<NodeId>,
    /// Moves on every change, so whoever draws the tree knows when to draw it again.
    pub revision: u64,
    /// Nodes changed since the last [`Dom::take_changes`], in the order they changed.
    changed: Vec<NodeId>,
    changed_set: HashSet<NodeId>,
    /// 媒体绑定独立消费的变更，不能依赖观察者是否启用，也不能抢走绘制变化。
    media_changed: HashSet<NodeId>,
    /// Subtrees whose place in the document changed: `true` connected, `false` disconnected.
    moved: Vec<(NodeId, bool)>,
    /// Nodes whose parent changed since the bindings last looked: their objects' links follow.
    relinked: Vec<NodeId>,
    /// What changed, as observers hear it — kept only while [`Dom::record_mutations`] says.
    mutations: Vec<(Mutation, Vec<NodeId>)>,
    /// Whether to keep [`Mutation`]s: while a `MutationObserver` watches.
    pub record_mutations: bool,
    /// Trees in no tree: candidates for reclaiming.
    roots: HashSet<NodeId>,
    /// Nodes the host itself holds — the document, the places it puts things — never reclaimed.
    pinned: HashSet<NodeId>,
    /// Nodes whose subtrees are drawn as views of their own; see [`Dom::partition_of`].
    partitions: HashSet<NodeId>,
    /// Every `<style>` element made: the ones in the document are stylesheets.
    styles: HashSet<NodeId>,
    /// Moves when what a `<style>` holds, or which are in the document, may have changed.
    styles_revision: u64,
    /// Where the last child looked up was among its parent's children: walking siblings one
    /// by one finds each next to the last, not by a search. Only a hint, always checked.
    sibling_hint: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// 宿主建议的节点上限；创建前可通过 [`Dom::is_full`] 检查。
    pub max_nodes: usize,
}

/// What changed since the arena was last asked.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    /// Every node changed — its own state, or its list of children.
    pub nodes: Vec<NodeId>,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    pub fn new() -> Self {
        let mut dom = Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            document: 0,
            html: 0,
            head: 0,
            body: 0,
            focused: None,
            hovered: Vec::new(),
            active: None,
            revision: 0,
            changed: Vec::new(),
            changed_set: HashSet::new(),
            media_changed: HashSet::new(),
            moved: Vec::new(),
            relinked: Vec::new(),
            mutations: Vec::new(),
            record_mutations: false,
            roots: HashSet::new(),
            pinned: HashSet::new(),
            partitions: HashSet::new(),
            styles: HashSet::new(),
            styles_revision: 0,
            sibling_hint: Default::default(),
            max_nodes: DEFAULT_MAX_NODES,
        };
        let document = dom.create_document();
        dom.slots[split(document).0].connected = true;
        let (html, head, body) = dom.fill_html_document(document);
        dom.document = document;
        dom.html = html;
        dom.head = head;
        dom.body = body;
        for id in [document, html, head, body] {
            dom.pinned.insert(id);
        }
        dom.take_changes();
        dom.take_moved();
        dom.relinked.clear();
        dom
    }

    /// A document with `<html>`, `<head>` and `<body>` in it: answers the four.
    pub fn create_html_document(&mut self) -> (NodeId, NodeId, NodeId, NodeId) {
        let document = self.create_document();
        let (html, head, body) = self.fill_html_document(document);
        (document, html, head, body)
    }

    fn fill_html_document(&mut self, document: NodeId) -> (NodeId, NodeId, NodeId) {
        let html = self.create_element("html");
        let head = self.create_element("head");
        let body = self.create_element("body");
        self.append(document, html);
        self.append(html, head);
        self.append(html, body);
        (html, head, body)
    }

    fn insert(&mut self, node: Node) -> NodeId {
        self.live += 1;
        self.revision = self.revision.wrapping_add(1);
        let id = match self.free.pop() {
            Some(index) => {
                let slot = &mut self.slots[index as usize];
                slot.node = Some(Arc::new(node));
                slot.connected = false;
                join(index as usize, slot.generation)
            }
            None => {
                // 槽位编号不能侵入代数位；耗尽的槽位永不复用。
                let index = self.slots.len();
                assert!(index <= INDEX_MASK as usize, "DOM 槽位已耗尽");
                self.slots.push(Slot {
                    generation: 0,
                    connected: false,
                    node: Some(Arc::new(node)),
                });
                join(index, 0)
            }
        };
        self.roots.insert(id);
        self.mark_media(id);
        id
    }

    /// 是否达到建议节点上限，或已无可编码且未退役的槽位。
    pub fn is_full(&self) -> bool {
        self.live >= self.max_nodes
            || (self.free.is_empty() && self.slots.len() > INDEX_MASK as usize)
    }

    /// Moves when a `<style>`'s text, or which `<style>`s are in the document, may have
    /// changed: when it has not, the sheets read last are still the sheets.
    pub fn styles_revision(&self) -> u64 {
        self.styles_revision
    }

    /// Marks `id` changed.
    pub fn mark(&mut self, id: NodeId) {
        self.mark_media(id);
        self.revision = self.revision.wrapping_add(1);
        if !self.styles.is_empty()
            && (self.styles.contains(&id)
                || self.parent(id).is_some_and(|p| self.styles.contains(&p)))
        {
            self.styles_revision = self.styles_revision.wrapping_add(1);
        }
        if self.changed_set.insert(id) {
            self.changed.push(id);
        }
    }

    fn mark_media(&mut self, id: NodeId) {
        if self.get(id).is_some_and(|n| {
            n.namespace == Namespace::Html && matches!(n.tag(), Some("video" | "audio"))
        }) {
            self.media_changed.insert(id);
        }
    }

    pub(crate) fn take_media_changes(&mut self) -> Vec<NodeId> {
        let mut nodes: Vec<_> = self.media_changed.drain().collect();
        nodes.sort_unstable();
        nodes
    }

    /// What changed since the last time this was asked, and forgets it.
    pub fn take_changes(&mut self) -> Changes {
        self.changed_set.clear();
        Changes {
            nodes: std::mem::take(&mut self.changed),
        }
    }

    /// Whether anything changed since [`Dom::take_changes`] was last asked.
    pub fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }

    /// Subtrees that joined (`true`) or left (`false`) the document since this was last asked.
    pub fn take_moved(&mut self) -> Vec<(NodeId, bool)> {
        std::mem::take(&mut self.moved)
    }

    pub(crate) fn take_relinked(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.relinked)
    }

    pub(crate) fn has_relinked(&self) -> bool {
        !self.relinked.is_empty()
    }

    /// The mutations recorded since this was last asked.
    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        self.take_mutation_paths()
            .into_iter()
            .map(|(mutation, _)| mutation)
            .collect()
    }

    /// 祖先链在变更发生时冻结，绑定层据此确定接收者，而不是等微任务时重查树。
    pub(crate) fn take_mutation_paths(&mut self) -> Vec<(Mutation, Vec<NodeId>)> {
        std::mem::take(&mut self.mutations)
    }

    pub fn has_mutations(&self) -> bool {
        !self.mutations.is_empty()
    }

    fn record(&mut self, mutation: Mutation) {
        if self.record_mutations {
            let mut path = Vec::new();
            let mut at = Some(mutation.target());
            while let Some(node) = at {
                path.push(node);
                at = self.parent(node);
            }
            self.mutations.push((mutation, path));
        }
    }

    pub fn create_element(&mut self, tag: &str) -> NodeId {
        self.create_element_ns(tag, Namespace::Html)
    }

    pub fn create_element_ns(&mut self, tag: &str, namespace: Namespace) -> NodeId {
        let tag = tag.to_ascii_lowercase();
        let style = tag == "style";
        let mut node = Node::new(NodeKind::Element(tag));
        node.namespace = namespace;
        if namespace != Namespace::Html {
            node.draft = None;
        }
        let id = self.insert(node);
        if style {
            self.styles.insert(id);
        }
        id
    }

    /// The `<style>` elements in the document `root` is, with the stylesheet each holds, in
    /// document order.
    pub fn style_sheets(&self) -> Vec<(NodeId, String)> {
        let mut out: Vec<(NodeId, String)> = self
            .styles
            .iter()
            .copied()
            .filter(|id| self.exists(*id) && self.is_connected(*id))
            .map(|id| (id, self.text_content(id)))
            .collect();
        let order: std::collections::HashMap<NodeId, usize> = self
            .subtree(self.document)
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect();
        out.sort_by_key(|(id, _)| order.get(id).copied().unwrap_or(usize::MAX));
        out
    }

    /// Where the node is in its tree: its place among its parent's children, and its
    /// parent's, and so on up, read from the top.
    fn tree_path(&self, id: NodeId) -> Vec<usize> {
        let mut out = Vec::new();
        let mut at = id;
        while let Some(parent) = self.parent(at) {
            out.push(self.index_of(parent, at).unwrap_or(0));
            at = parent;
        }
        out.reverse();
        out
    }

    /// Document order of two nodes in one tree.
    pub fn order(&self, a: NodeId, b: NodeId) -> std::cmp::Ordering {
        self.tree_path(a).cmp(&self.tree_path(b))
    }

    /// Where `child` is among `parent`'s children: next to the last one looked up when it is
    /// there (walking siblings), searched for otherwise.
    pub fn index_of(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        use std::sync::atomic::Ordering;
        let children = self.children(parent);
        let hint = self.sibling_hint.load(Ordering::Relaxed);
        let found = [
            hint,
            hint.wrapping_add(1),
            hint.wrapping_sub(1),
            hint.wrapping_add(2),
        ]
        .into_iter()
        .find(|at| children.get(*at) == Some(&child))
        .or_else(|| children.iter().position(|c| *c == child))?;
        self.sibling_hint.store(found, Ordering::Relaxed);
        Some(found)
    }

    pub fn create_comment(&mut self, data: &str) -> NodeId {
        self.insert(Node::new(NodeKind::Comment(data.to_owned())))
    }

    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.insert(Node::new(NodeKind::Text(text.to_owned())))
    }

    pub fn create_fragment(&mut self) -> NodeId {
        self.insert(Node::new(NodeKind::Fragment))
    }

    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        self.insert(Node::new(NodeKind::Doctype(name.to_owned())))
    }

    /// A document of its own, outside the window's: what `DOMParser` makes.
    pub fn create_document(&mut self) -> NodeId {
        self.insert(Node::new(NodeKind::Document))
    }

    /// The first element `tag` in `root`, `root` itself included.
    pub fn first_tag(&self, root: NodeId, tag: &str) -> Option<NodeId> {
        std::iter::once(root)
            .chain(self.descendants(root))
            .find(|id| self.get(*id).and_then(|n| n.tag()) == Some(tag))
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        let (index, generation) = split(id);
        let slot = self.slots.get(index)?;
        (slot.generation == generation)
            .then_some(slot.node.as_deref())
            .flatten()
    }

    /// The node, to change; it is marked changed.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if !self.exists(id) {
            return None;
        }
        self.mark(id);
        self.node_mut(id)
    }

    fn node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let (index, generation) = split(id);
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        slot.node.as_mut().map(Arc::make_mut)
    }

    pub fn exists(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.get(id).map_or(&[], |node| &node.children)
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.get(id).and_then(|node| node.parent)
    }

    /// The element `id` is in, if what it is in is an element.
    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id)
            .filter(|p| self.get(*p).is_some_and(Node::is_element))
    }

    /// Whether `inner` is `outer` or inside it.
    pub fn contains(&self, outer: NodeId, inner: NodeId) -> bool {
        let mut at = Some(inner);
        while let Some(id) = at {
            if id == outer {
                return true;
            }
            at = self.parent(id);
        }
        false
    }

    /// The node before or after `id` among its parent's children (`step` -1 or 1), elements
    /// only when `elements_only`.
    pub fn sibling(&self, id: NodeId, step: isize, elements_only: bool) -> Option<NodeId> {
        let parent = self.parent(id)?;
        let children = self.children(parent);
        let mut at = self.index_of(parent, id)? as isize;
        loop {
            at += step;
            if at < 0 || at as usize >= children.len() {
                return None;
            }
            let candidate = children[at as usize];
            if !elements_only || self.get(candidate).is_some_and(Node::is_element) {
                self.sibling_hint
                    .store(at as usize, std::sync::atomic::Ordering::Relaxed);
                return Some(candidate);
            }
        }
    }

    /// The pointer is over `path` now: the element it is on first, then each element that one
    /// is in. Answers the elements it left and the ones it came onto, each innermost first, and
    /// marks them changed so a `:hover` rule is applied or taken off.
    pub fn set_hovered(&mut self, path: Vec<NodeId>) -> (Vec<NodeId>, Vec<NodeId>) {
        let left: Vec<NodeId> = self
            .hovered
            .iter()
            .copied()
            .filter(|id| !path.contains(id))
            .collect();
        let entered: Vec<NodeId> = path
            .iter()
            .copied()
            .filter(|id| !self.hovered.contains(id))
            .collect();
        self.hovered = path;
        for id in left.iter().chain(entered.iter()) {
            if self.exists(*id) {
                self.mark(*id);
            }
        }
        (left, entered)
    }

    /// Whether the pointer is over `id`: on it, or on something inside it.
    pub fn is_hovered(&self, id: NodeId) -> bool {
        self.hovered.contains(&id)
    }

    /// The innermost element the pointer is over.
    pub fn hovered(&self) -> Option<NodeId> {
        self.hovered.first().copied()
    }

    /// Whether the element with the keyboard is `id` or inside it: `:focus-within`.
    pub fn focus_within(&self, id: NodeId) -> bool {
        self.focused.is_some_and(|f| self.contains(id, f))
    }

    /// The root of the tree `id` is in.
    pub fn root_of(&self, id: NodeId) -> NodeId {
        let mut at = id;
        while let Some(parent) = self.parent(at) {
            at = parent;
        }
        at
    }

    /// The document `id` is in, if it is in one.
    pub fn document_of(&self, id: NodeId) -> Option<NodeId> {
        let root = self.root_of(id);
        matches!(self.get(root).map(|n| &n.kind), Some(NodeKind::Document)).then_some(root)
    }

    /// Whether the node is in the window's document.
    pub fn is_connected(&self, id: NodeId) -> bool {
        let (index, generation) = split(id);
        self.slots
            .get(index)
            .is_some_and(|s| s.generation == generation && s.node.is_some() && s.connected)
    }

    /// Marks `id`'s subtree in or out of the window's document, when that changed.
    fn set_connected(&mut self, id: NodeId, connected: bool) {
        if self.is_connected(id) == connected {
            return;
        }
        let mut stack = vec![id];
        let mut style = false;
        while let Some(at) = stack.pop() {
            let (index, generation) = split(at);
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            if slot.generation != generation {
                continue;
            }
            slot.connected = connected;
            if let Some(node) = &slot.node {
                style |= node.tag() == Some("style");
                stack.extend(node.children.iter().copied());
                if node.namespace == Namespace::Html
                    && matches!(node.tag(), Some("video" | "audio"))
                {
                    self.media_changed.insert(at);
                }
            }
        }
        if style {
            self.styles_revision = self.styles_revision.wrapping_add(1);
        }
    }

    /// The host holds `id` itself: it is never reclaimed.
    pub fn pin(&mut self, id: NodeId) {
        self.pinned.insert(id);
    }

    pub fn unpin(&mut self, id: NodeId) {
        self.pinned.remove(&id);
    }

    pub fn is_pinned(&self, id: NodeId) -> bool {
        self.pinned.contains(&id)
    }

    /// Draws `id`'s subtree as a view of its own, redrawn only when something in it changes.
    pub fn set_partition(&mut self, id: NodeId, on: bool) {
        if on {
            self.partitions.insert(id);
        } else {
            self.partitions.remove(&id);
        }
    }

    pub fn is_partition(&self, id: NodeId) -> bool {
        self.partitions.contains(&id)
    }

    /// The nearest partition `id` is in, itself included; the root of its tree when there is
    /// none.
    pub fn partition_of(&self, id: NodeId) -> NodeId {
        let mut at = id;
        loop {
            if self.partitions.contains(&at) {
                return at;
            }
            match self.parent(at) {
                Some(parent) => at = parent,
                None => return at,
            }
        }
    }

    /// Takes the node out of its parent, if it has one.
    pub fn detach(&mut self, id: NodeId) {
        let Some(parent) = self.parent(id) else {
            return;
        };
        let Some(at) = self.index_of(parent, id) else {
            return;
        };
        let (previous, next) = if self.record_mutations {
            let children = self.children(parent);
            (
                at.checked_sub(1).map(|i| children[i]),
                children.get(at + 1).copied(),
            )
        } else {
            (None, None)
        };
        if let Some(node) = self.node_mut(parent) {
            node.children.remove(at);
        }
        self.unlink(id);
        self.mark(parent);
        self.record(Mutation::ChildList {
            target: parent,
            added: Vec::new(),
            removed: vec![id],
            previous,
            next,
        });
    }

    /// 多节点转换一次移走同一父节点中的成员，避免每次搬移都扫描和移动整条兄弟列表。
    pub(crate) fn detach_many(&mut self, ids: &[NodeId]) {
        let selected: HashSet<NodeId> = ids.iter().copied().collect();
        let parents: HashSet<NodeId> = ids.iter().filter_map(|id| self.parent(*id)).collect();
        let mut links = std::collections::HashMap::new();
        for parent in &parents {
            let children = self.children(*parent);
            for (at, id) in children.iter().enumerate() {
                links.insert(
                    *id,
                    (
                        *parent,
                        at.checked_sub(1).map(|i| children[i]),
                        children.get(at + 1).copied(),
                    ),
                );
            }
        }
        for parent in parents {
            if let Some(node) = self.node_mut(parent) {
                node.children.retain(|id| !selected.contains(id));
            }
        }
        for id in ids {
            let Some((parent, previous, next)) = links.remove(id) else {
                continue;
            };
            if let Some(previous) = previous.and_then(|id| links.get_mut(&id)) {
                previous.2 = next;
            }
            if let Some(next) = next.and_then(|id| links.get_mut(&id)) {
                next.1 = previous;
            }
            self.unlink(*id);
            self.mark(parent);
            self.record(Mutation::ChildList {
                target: parent,
                added: Vec::new(),
                removed: vec![*id],
                previous,
                next,
            });
        }
    }

    /// What taking `id` out of its parent does to `id`, once its parent no longer lists it.
    fn unlink(&mut self, id: NodeId) {
        let was_connected = self.is_connected(id);
        if let Some(node) = self.node_mut(id) {
            node.parent = None;
        }
        if self
            .focused
            .is_some_and(|focused| self.contains(id, focused))
        {
            self.focused = None;
        }
        if self.active.is_some_and(|active| self.contains(id, active)) {
            self.active = None;
        }
        self.roots.insert(id);
        self.relinked.push(id);
        if was_connected {
            self.set_connected(id, false);
            self.moved.push((id, false));
        }
    }

    /// Puts `child` last in `parent`. A fragment gives up its children instead of itself, as the
    /// DOM's does.
    pub fn append(&mut self, parent: NodeId, child: NodeId) -> bool {
        self.insert_before(parent, child, None)
    }

    /// Whether `child` may go in `parent`: what `appendChild` refuses with a
    /// `HierarchyRequestError`.
    pub fn may_insert(&self, parent: NodeId, child: NodeId) -> bool {
        if !self.exists(parent) || !self.exists(child) || child == parent {
            return false;
        }
        // Only what has children can hold the parent; a fresh leaf, the usual child, cannot.
        if !self.children(child).is_empty() && self.contains(child, parent) {
            return false;
        }
        let parent_kind = &self.get(parent).expect("checked").kind;
        let child_kind = &self.get(child).expect("checked").kind;
        if matches!(
            parent_kind,
            NodeKind::Text(_) | NodeKind::Comment(_) | NodeKind::Doctype(_)
        ) || matches!(child_kind, NodeKind::Document)
        {
            return false;
        }
        if matches!(child_kind, NodeKind::Doctype(_)) && !matches!(parent_kind, NodeKind::Document)
        {
            return false;
        }
        if matches!(parent_kind, NodeKind::Document) {
            return self.may_go_in_document(parent, child);
        }
        true
    }

    /// A document holds one element and one doctype at most, and no text.
    fn may_go_in_document(&self, document: NodeId, child: NodeId) -> bool {
        let others = |what: fn(&Node) -> bool| {
            self.children(document)
                .iter()
                .any(|c| *c != child && self.get(*c).is_some_and(what))
        };
        let is_doctype = |n: &Node| matches!(n.kind, NodeKind::Doctype(_));
        match &self.get(child).expect("checked").kind {
            NodeKind::Text(_) => false,
            NodeKind::Element(_) => !others(Node::is_element),
            NodeKind::Doctype(_) => !others(is_doctype),
            NodeKind::Fragment => {
                let kids = self.children(child);
                let elements = kids
                    .iter()
                    .filter(|c| self.get(**c).is_some_and(Node::is_element))
                    .count();
                !kids.iter().any(|c| self.get(*c).is_some_and(Node::is_text))
                    && elements <= 1
                    && (elements == 0 || !others(Node::is_element))
            }
            _ => true,
        }
    }

    /// Puts `child` in `parent` before `before`, or last. Refuses what would make a loop.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, before: Option<NodeId>) -> bool {
        if !self.may_insert(parent, child)
            || before.is_some_and(|id| self.parent(id) != Some(parent))
        {
            return false;
        }
        let nodes = if matches!(self.get(child).map(|n| &n.kind), Some(NodeKind::Fragment)) {
            // A fragment gives up all its children at once, heard as one change of each list.
            let moved = match self.node_mut(child) {
                Some(fragment) => std::mem::take(&mut fragment.children),
                None => Vec::new(),
            };
            if moved.is_empty() {
                return true;
            }
            for node in &moved {
                if let Some(node) = self.node_mut(*node) {
                    node.parent = None;
                }
            }
            self.mark(child);
            self.record(Mutation::ChildList {
                target: child,
                added: Vec::new(),
                removed: moved.clone(),
                previous: None,
                next: None,
            });
            moved
        } else {
            // Before itself is before what follows it.
            let before = if before == Some(child) {
                self.sibling(child, 1, false)
            } else {
                before
            };
            self.detach(child);
            return self.place(parent, vec![child], before);
        };
        let before = before.filter(|b| !nodes.contains(b));
        self.place(parent, nodes, before)
    }

    /// Puts `nodes`, in no tree now, in `parent` before `before` (or last), as one change.
    fn place(&mut self, parent: NodeId, nodes: Vec<NodeId>, before: Option<NodeId>) -> bool {
        let connected = self.is_connected(parent);
        let at = before
            .and_then(|before| self.index_of(parent, before))
            .unwrap_or(self.children(parent).len());
        let Some(node) = self.node_mut(parent) else {
            return false;
        };
        node.children.splice(at..at, nodes.iter().copied());
        let (previous, next) = if self.record_mutations {
            let children = self.children(parent);
            (
                at.checked_sub(1).map(|i| children[i]),
                children.get(at + nodes.len()).copied(),
            )
        } else {
            (None, None)
        };
        for child in &nodes {
            let child = *child;
            if let Some(node) = self.node_mut(child) {
                node.parent = Some(parent);
            }
            self.roots.remove(&child);
            self.relinked.push(child);
            self.mark(child);
            if connected {
                self.set_connected(child, true);
                self.moved.push((child, true));
            }
        }
        self.mark(parent);
        self.record(Mutation::ChildList {
            target: parent,
            added: nodes,
            removed: Vec::new(),
            previous,
            next,
        });
        true
    }

    /// Takes every child out of `id`, recording each removal in document order.
    pub fn empty(&mut self, id: NodeId) {
        let children = match self.node_mut(id) {
            Some(node) if !node.children.is_empty() => std::mem::take(&mut node.children),
            _ => return,
        };
        for (at, child) in children.iter().enumerate() {
            self.unlink(*child);
            if self.record_mutations {
                // Like removing the first child repeatedly, without shifting the wide list.
                self.record(Mutation::ChildList {
                    target: id,
                    added: Vec::new(),
                    removed: vec![*child],
                    previous: None,
                    next: children.get(at + 1).copied(),
                });
            }
        }
        self.mark(id);
    }

    /// The text of the node and everything in it, as `textContent` reads it.
    pub fn text_content(&self, id: NodeId) -> String {
        // A comment's is its own data; inside anything else, comments are no text.
        if let Some(NodeKind::Comment(data)) = self.get(id).map(|n| &n.kind) {
            return data.clone();
        }
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: NodeId, out: &mut String) {
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            let Some(node) = self.get(at) else { continue };
            match &node.kind {
                NodeKind::Text(text) => out.push_str(text),
                NodeKind::Comment(_) | NodeKind::Doctype(_) => {}
                _ => stack.extend(node.children.iter().rev()),
            }
        }
    }

    /// Replaces everything in the node with one run of text, as setting `textContent` does; a
    /// text node's or a comment's own data, for those.
    pub fn set_text(&mut self, id: NodeId, text: &str) {
        let old = match self.get(id).map(|n| &n.kind) {
            Some(NodeKind::Text(own) | NodeKind::Comment(own)) => Some(own.clone()),
            _ => None,
        };
        if let Some(old) = old {
            if old == text {
                return;
            }
            if let Some(Node {
                kind: NodeKind::Text(own) | NodeKind::Comment(own),
                ..
            }) = self.node_mut(id)
            {
                *own = text.to_owned();
            }
            self.mark(id);
            self.record(Mutation::CharacterData { target: id, old });
            return;
        }
        self.empty(id);
        if !text.is_empty() {
            let run = self.create_text(text);
            self.append(id, run);
        }
    }

    /// 解析器追加相邻文本时只扩展尾部，不反复复制已有内容。
    pub(crate) fn append_text(&mut self, id: NodeId, text: &str) {
        let old = self
            .record_mutations
            .then(|| self.get(id).and_then(Node::data).unwrap_or("").to_owned());
        if let Some(Node {
            kind: NodeKind::Text(own),
            ..
        }) = self.node_mut(id)
        {
            own.push_str(text);
            self.mark(id);
            if let Some(old) = old {
                self.record(Mutation::CharacterData { target: id, old });
            }
        }
    }

    pub fn set_attr(&mut self, id: NodeId, name: &str, value: &str) {
        let name = if self.get(id).is_some_and(|n| n.namespace == Namespace::Html) {
            name.to_ascii_lowercase()
        } else {
            name.to_owned()
        };
        let old = if self.record_mutations {
            self.get_attr(id, &name)
        } else {
            None
        };
        let Some(node) = self.node_mut(id) else {
            return;
        };
        match name.as_str() {
            "class" => {
                node.classes = value.split_whitespace().map(str::to_owned).collect();
                // An empty `class=""` is an attribute still.
                upsert(&mut node.attrs, "class", "");
            }
            "style" => {
                let (style, important) = parse_style(value);
                node.style = style;
                node.important = important;
                upsert(&mut node.attrs, "style", "");
            }
            "value" if node.draft.is_some() => {
                // The default value: what the field shows until something is typed or set.
                upsert(&mut node.attrs, &name, value);
                if !node.dirty_value {
                    node.draft = Some(Draft::new(value));
                }
            }
            "checked" if node.tag() == Some("input") => {
                upsert(&mut node.attrs, &name, value);
                if !node.dirty_checked {
                    node.checked = true;
                }
            }
            _ => upsert(&mut node.attrs, &name, value),
        }
        self.mark(id);
        self.record(Mutation::Attributes {
            target: id,
            name,
            old,
        });
    }

    pub fn remove_attr(&mut self, id: NodeId, name: &str) {
        let name = if self.get(id).is_some_and(|n| n.namespace == Namespace::Html) {
            name.to_ascii_lowercase()
        } else {
            name.to_owned()
        };
        let old = self.get_attr(id, &name);
        if old.is_none() {
            return;
        }
        if let Some(node) = self.node_mut(id) {
            match name.as_str() {
                "class" => node.classes.clear(),
                "style" => {
                    node.style.clear();
                    node.important.clear();
                }
                "checked" if node.tag() == Some("input") && !node.dirty_checked => {
                    node.checked = false
                }
                _ => {}
            }
            node.attrs.retain(|(key, _)| *key != name);
            self.mark(id);
        }
        self.record(Mutation::Attributes {
            target: id,
            name,
            old,
        });
    }

    /// The attribute as `getAttribute` answers it, `class` and `style` included.
    pub fn get_attr(&self, id: NodeId, name: &str) -> Option<String> {
        let node = self.get(id)?;
        let lower;
        let name = if node.namespace == Namespace::Html {
            lower = name.to_ascii_lowercase();
            lower.as_str()
        } else {
            name
        };
        match name {
            "class" => node.attr("class").map(|_| node.classes.join(" ")),
            "style" => node.attr("style").map(|_| style_text(node)),
            other => node.attr(other).map(str::to_owned),
        }
    }

    /// Every attribute, `class` and `style` as their text, in the order they were set.
    pub fn attributes(&self, id: NodeId) -> Vec<(String, String)> {
        let Some(node) = self.get(id) else {
            return Vec::new();
        };
        node.attrs
            .iter()
            .map(|(key, value)| match key.as_str() {
                "class" => (key.clone(), node.classes.join(" ")),
                "style" => (key.clone(), style_text(node)),
                _ => (key.clone(), value.clone()),
            })
            .collect()
    }

    /// Changes the classes with `f`, as one change to the `class` attribute.
    pub fn change_classes(&mut self, id: NodeId, f: impl FnOnce(&mut Vec<String>)) {
        let old = self.get_attr(id, "class");
        let Some(node) = self.node_mut(id) else {
            return;
        };
        let before = node.classes.clone();
        f(&mut node.classes);
        if node.classes == before && node.attr("class").is_some() {
            return;
        }
        upsert(&mut node.attrs, "class", "");
        self.mark(id);
        self.record(Mutation::Attributes {
            target: id,
            name: "class".into(),
            old,
        });
    }

    pub fn add_class(&mut self, id: NodeId, class: &str) {
        if class.is_empty() {
            return;
        }
        self.change_classes(id, |classes| {
            if !classes.iter().any(|c| c == class) {
                classes.push(class.to_owned());
            }
        });
    }

    pub fn remove_class(&mut self, id: NodeId, class: &str) {
        if !self.get(id).is_some_and(|n| n.has_class(class)) {
            return;
        }
        self.change_classes(id, |classes| classes.retain(|c| c != class));
    }

    /// Sets one inline property — an empty value takes it away — as `style.setProperty` does.
    pub fn set_style(&mut self, id: NodeId, property: &str, value: &str) {
        self.set_style_important(id, property, value, false);
    }

    pub fn set_style_important(
        &mut self,
        id: NodeId,
        property: &str,
        value: &str,
        important: bool,
    ) {
        let old = self.get_attr(id, "style");
        let Some(node) = self.node_mut(id) else {
            return;
        };
        if value.is_empty() {
            if node.style_value(property).is_none() {
                return;
            }
            node.style.retain(|(k, _)| k != property);
            node.important.retain(|k| k != property);
        } else {
            if node.style_value(property) == Some(value)
                && node.important.iter().any(|k| k == property) == important
            {
                return;
            }
            upsert(&mut node.style, property, value);
            node.important.retain(|k| k != property);
            if important {
                node.important.push(property.to_owned());
            }
        }
        upsert(&mut node.attrs, "style", "");
        self.mark(id);
        self.record(Mutation::Attributes {
            target: id,
            name: "style".into(),
            old,
        });
    }

    /// What is in a field: the typed text of an `<input>` or `<textarea>`, the chosen option's
    /// value of a `<select>`.
    pub fn value(&self, id: NodeId) -> String {
        let Some(node) = self.get(id) else {
            return String::new();
        };
        if node.tag() == Some("textarea") && !node.dirty_value {
            return self.text_content(id);
        }
        if let Some(draft) = &node.draft {
            return draft.text().to_owned();
        }
        if node.tag() == Some("select") {
            return self
                .selected_option(id)
                .map(|option| self.option_value(option))
                .unwrap_or_default();
        }
        node.attr("value").unwrap_or_default().to_owned()
    }

    /// Sets what is in a field, as setting `value` does: the caret goes to the end. Answers
    /// whether it changed.
    pub fn set_value(&mut self, id: NodeId, value: &str) -> bool {
        if self.get(id).and_then(|n| n.tag()) == Some("select") {
            let options = self.options(id);
            let index = options.iter().position(|o| self.option_value(*o) == value);
            let changed = self.get(id).and_then(|n| n.selected) != index || index.is_none();
            if let Some(node) = self.node_mut(id) {
                node.selected = index.or(Some(usize::MAX));
            }
            self.mark(id);
            return changed;
        }
        let Some(node) = self.node_mut(id) else {
            return false;
        };
        let changed = match &mut node.draft {
            Some(draft) => {
                let changed = draft.text() != value;
                if changed {
                    let mut fresh = Draft::new(value);
                    fresh.place(value.len());
                    *draft = fresh;
                }
                node.dirty_value = true;
                changed
            }
            None => {
                let changed = node.attr("value") != Some(value);
                upsert(&mut node.attrs, "value", value);
                changed
            }
        };
        if changed {
            self.mark(id);
        }
        changed
    }

    /// A `<select>`'s options, in order: its own and those in its `<optgroup>`s.
    pub fn options(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for child in self.children(id) {
            match self.get(*child).and_then(Node::tag) {
                Some("option") => out.push(*child),
                Some("optgroup") => out.extend(
                    self.children(*child)
                        .iter()
                        .copied()
                        .filter(|c| self.get(*c).and_then(Node::tag) == Some("option")),
                ),
                _ => {}
            }
        }
        out
    }

    /// An option's value: its `value` attribute, or its text.
    pub fn option_value(&self, option: NodeId) -> String {
        self.get(option).and_then(|n| n.attr("value")).map_or_else(
            || self.text_content(option).trim().to_owned(),
            str::to_owned,
        )
    }

    /// The option a `<select>` has chosen.
    pub fn selected_option(&self, select: NodeId) -> Option<NodeId> {
        let options = self.options(select);
        match self.get(select)?.selected {
            Some(index) => options.get(index).copied(),
            None => options
                .iter()
                .copied()
                .find(|o| self.get(*o).is_some_and(|n| n.attr("selected").is_some()))
                .or_else(|| options.first().copied()),
        }
    }

    /// Chooses the `index`th option of a `<select>`.
    pub fn select_index(&mut self, select: NodeId, index: Option<usize>) {
        if let Some(node) = self.node_mut(select) {
            node.selected = Some(index.unwrap_or(usize::MAX));
        }
        self.mark(select);
    }

    /// Every element under `root`, in document order, `root` itself left out.
    pub fn descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.children(root).iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            if self.get(id).is_some_and(Node::is_element) {
                out.push(id);
            }
            stack.extend(self.children(id).iter().rev());
        }
        out
    }

    /// `root` and every node under it, text included, in document order.
    pub fn subtree(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if !self.exists(id) {
                continue;
            }
            out.push(id);
            stack.extend(self.children(id).iter().rev());
        }
        out
    }

    /// What `:scope` is for a query asked of `root`: the element itself, or the document's
    /// root element for a document.
    pub fn scope_of(&self, root: NodeId) -> NodeId {
        match self.get(root).map(|n| &n.kind) {
            Some(NodeKind::Document) => self
                .children(root)
                .iter()
                .copied()
                .find(|c| self.get(*c).is_some_and(Node::is_element))
                .unwrap_or(root),
            _ => root,
        }
    }

    /// The first element under `root` the selector matches; `None` for one that does not parse
    /// as well as for one that matches nothing (see [`crate::css::SelectorList::parse`]).
    pub fn query(&self, root: NodeId, selector: &str) -> Option<NodeId> {
        let selector = crate::css::SelectorList::parse(selector).ok()?;
        let scope = self.scope_of(root);
        selector
            .select(self, self.descendants(root), Some(scope), true)
            .into_iter()
            .next()
    }

    pub fn query_all(&self, root: NodeId, selector: &str) -> Vec<NodeId> {
        let Ok(selector) = crate::css::SelectorList::parse(selector) else {
            return Vec::new();
        };
        let scope = self.scope_of(root);
        selector.select(self, self.descendants(root), Some(scope), false)
    }

    /// A copy of the node — and of everything in it, when `deep`. The copy is in no tree.
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> Option<NodeId> {
        let copy = self.clone_one(id)?;
        if deep {
            // A loop, not a recursion: a tree of any depth is copied.
            let mut stack = vec![(id, copy)];
            while let Some((from, to)) = stack.pop() {
                let children = self.children(from).to_vec();
                let mut copies = Vec::with_capacity(children.len());
                for child in children {
                    if let Some(made) = self.clone_one(child) {
                        if let Some(node) = self.node_mut(made) {
                            node.parent = Some(to);
                        }
                        self.roots.remove(&made);
                        copies.push(made);
                        stack.push((child, made));
                    }
                }
                if let Some(node) = self.node_mut(to) {
                    node.children = copies;
                }
            }
        }
        Some(copy)
    }

    /// A copy of the node alone, in no tree.
    fn clone_one(&mut self, id: NodeId) -> Option<NodeId> {
        let mut node = self.get(id)?.clone();
        node.children = Vec::new();
        node.parent = None;
        let style = node.tag() == Some("style");
        let copy = self.insert(node);
        if style {
            self.styles.insert(copy);
        }
        Some(copy)
    }

    /// Whether the node is drawn: in the document, and neither it nor anything it is in hidden
    /// by its own inline style or the `hidden` attribute.
    pub fn is_shown(&self, id: NodeId) -> bool {
        let mut at = Some(id);
        while let Some(node) = at {
            if self.get(node).is_some_and(|n| {
                n.style_value("display") == Some("none") || n.attr("hidden").is_some()
            }) {
                return false;
            }
            at = self.parent(node);
        }
        self.is_connected(id)
    }

    /// The roots of detached trees, the candidates for [`Dom::free`].
    pub fn detached_roots(&self) -> Vec<NodeId> {
        self.roots
            .iter()
            .copied()
            .filter(|id| *id != self.document && !self.pinned.contains(id))
            .collect()
    }

    /// Forgets the node and everything in it. Only for what no handle can reach any more:
    /// answers the ids freed, so whoever kept something against them can let it go.
    pub fn free(&mut self, id: NodeId) -> Vec<NodeId> {
        self.detach(id);
        let mut freed = Vec::new();
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            let (index, generation) = split(at);
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            if slot.generation != generation {
                continue;
            }
            if let Some(node) = slot.node.take() {
                match Arc::try_unwrap(node) {
                    Ok(node) => stack.extend(node.children),
                    Err(shared) => stack.extend(shared.children.iter().copied()),
                }
                slot.connected = false;
                // A place whose generations ran out is retired: no id is ever given twice.
                if slot.generation < MAX_GENERATION {
                    slot.generation += 1;
                    self.free.push(index as u32);
                }
                self.live -= 1;
                self.roots.remove(&at);
                self.partitions.remove(&at);
                self.styles.remove(&at);
                self.pinned.remove(&at);
                freed.push(at);
            }
        }
        let gone: HashSet<NodeId> = freed.iter().copied().collect();
        self.hovered.retain(|h| !gone.contains(h));
        if self.focused.is_some_and(|f| gone.contains(&f)) {
            self.focused = None;
        }
        if self.active.is_some_and(|a| gone.contains(&a)) {
            self.active = None;
        }
        let mut was_changed = false;
        for id in &freed {
            was_changed |= self.changed_set.remove(id);
        }
        if was_changed {
            self.changed.retain(|c| !gone.contains(c));
        }
        self.relinked.retain(|r| !gone.contains(r));
        self.moved.retain(|(id, _)| !gone.contains(id));
        self.mutations.retain(|(m, _)| !gone.contains(&m.target()));
        self.revision = self.revision.wrapping_add(1);
        freed
    }

    /// How many nodes are alive.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }
}

fn upsert(list: &mut Vec<(String, String)>, key: &str, value: &str) {
    match list.iter_mut().find(|(k, _)| k == key) {
        Some(entry) => entry.1 = value.to_owned(),
        None => list.push((key.to_owned(), value.to_owned())),
    }
}

/// The inline style as `style="…"` writes it.
fn style_text(node: &Node) -> String {
    node.style
        .iter()
        .map(|(k, v)| {
            if node.important.contains(k) {
                format!("{k}: {v} !important;")
            } else {
                format!("{k}: {v};")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `color: red; padding: 4px !important` as pairs, and the names declared `!important`.
pub fn parse_style(text: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut important = Vec::new();
    let pairs = crate::css::declarations(text)
        .into_iter()
        .map(|d| {
            if d.important {
                important.push(d.name.clone());
            }
            (d.name, d.value)
        })
        .collect();
    (pairs, important)
}

/// `fontSize` as `font-size`, `cssFloat` as `float`, `webkitTransform` as `-webkit-transform`; a
/// custom property is left alone.
pub fn css_name(camel: &str) -> String {
    if camel.starts_with("--") {
        return camel.to_owned();
    }
    if camel == "cssFloat" {
        return "float".to_owned();
    }
    let mut out = String::with_capacity(camel.len() + 4);
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    for prefix in ["webkit-", "moz-", "ms-"] {
        if out.starts_with(prefix) {
            return format!("-{out}");
        }
    }
    out
}

/// `font-size` as `fontSize`.
pub fn camel_name(css: &str) -> String {
    if css.starts_with("--") {
        return css.to_owned();
    }
    if css == "float" {
        return "cssFloat".to_owned();
    }
    let css = css.strip_prefix('-').unwrap_or(css);
    let mut out = String::with_capacity(css.len());
    let mut upper = false;
    for c in css.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_is_built_moved_and_read() {
        let mut dom = Dom::new();
        let div = dom.create_element("DIV");
        let span = dom.create_element("span");
        dom.append(dom.body, div);
        dom.append(div, span);
        dom.set_text(span, "hi");
        assert_eq!(dom.text_content(dom.body), "hi");
        assert!(dom.is_connected(span));
        // No loops.
        assert!(!dom.append(span, div));
        dom.detach(div);
        assert!(!dom.is_connected(span));
        let fragment = dom.create_fragment();
        let a = dom.create_text("a");
        let b = dom.create_text("b");
        dom.append(fragment, a);
        dom.append(fragment, b);
        dom.append(dom.body, fragment);
        assert_eq!(dom.text_content(dom.body), "ab");
        assert!(dom.children(fragment).is_empty());
    }

    #[test]
    fn classes_attributes_and_fields() {
        let mut dom = Dom::new();
        let input = dom.create_element("input");
        dom.set_attr(input, "class", "a  b");
        dom.add_class(input, "c");
        dom.remove_class(input, "a");
        assert_eq!(dom.get_attr(input, "class").as_deref(), Some("b c"));
        dom.set_value(input, "typed");
        assert_eq!(dom.value(input), "typed");
        dom.set_attr(input, "style", "display: none; color: red !important");
        assert_eq!(dom.get(input).unwrap().style_value("color"), Some("red"));
        assert_eq!(
            dom.get_attr(input, "style").as_deref(),
            Some("display: none; color: red !important;")
        );
        assert_eq!(css_name("backgroundColor"), "background-color");
        assert_eq!(css_name("webkitTransform"), "-webkit-transform");
        assert_eq!(camel_name("-webkit-transform"), "webkitTransform");
    }

    #[test]
    fn mutations_are_recorded_while_asked_for() {
        let mut dom = Dom::new();
        let div = dom.create_element("div");
        dom.append(dom.body, div);
        assert!(!dom.has_mutations());
        dom.record_mutations = true;
        dom.set_attr(div, "id", "x");
        let text = dom.create_text("t");
        dom.append(div, text);
        dom.set_text(text, "u");
        let found = dom.take_mutations();
        assert_eq!(found.len(), 3);
        assert!(matches!(&found[0], Mutation::Attributes { name, old: None, .. } if name == "id"));
        assert!(matches!(&found[1], Mutation::ChildList { added, .. } if added == &vec![text]));
        assert!(matches!(&found[2], Mutation::CharacterData { old, .. } if old == "t"));
    }
}
