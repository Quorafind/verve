//! Shadow trees in the arena: which element hosts which root, how the host's children are
//! assigned to the root's `<slot>`s, and the flat tree that results — what is drawn and what the
//! cascade inherits along. A shadow root is a node the arena owns, kept apart from its host's
//! children: it is reached through its host, and lives and dies with it.

use std::collections::HashMap;

use super::{Dom, Node, NodeId, NodeKind};

/// Whether a shadow root's tree may be reached from a script through `element.shadowRoot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowMode {
    /// Reachable from a script through `element.shadowRoot`.
    Open,
    /// Reachable only by whoever attached it.
    Closed,
}

impl ShadowMode {
    /// The mode `attachShadow({ mode })` names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "open" => Some(Self::Open),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }

    /// The mode as `shadowRoot.mode` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }
}

/// How the slottables of a shadow root's host are assigned to its slots: by their `slot`
/// attribute and the slots' `name` (the default), or only by `slot.assign()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotAssignment {
    /// By the `slot` attribute and the slot's `name`.
    Named,
    /// Only by `slot.assign()`.
    Manual,
}

impl SlotAssignment {
    /// The assignment `attachShadow({ slotAssignment })` names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "named" => Some(Self::Named),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    /// The assignment as `shadowRoot.slotAssignment` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Named => "named",
            Self::Manual => "manual",
        }
    }
}

/// What a shadow root is: the element hosting it, and how it takes part in the flat tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shadow {
    /// The element the root is attached to.
    pub host: NodeId,
    /// Whether a script can reach the tree through `element.shadowRoot`.
    pub mode: ShadowMode,
    /// Whether focusing the host focuses the first focusable element inside instead.
    pub delegates_focus: bool,
    /// How the host's children are given to slots.
    pub assignment: SlotAssignment,
}

/// Every shadow root, by itself and by its host, and what `slot.assign()` gave each slot. Kept
/// in the arena rather than on each node: almost no node has any of it.
#[derive(Clone, Debug, Default)]
pub(super) struct Shadows {
    roots: HashMap<NodeId, Shadow>,
    hosts: HashMap<NodeId, NodeId>,
    assigned: HashMap<NodeId, Vec<NodeId>>,
}

impl Shadows {
    /// The shadow root `host` hosts.
    pub(super) fn root_of_host(&self, host: NodeId) -> Option<NodeId> {
        self.hosts.get(&host).copied()
    }

    /// Forgets `id`, and answers the shadow root it hosted.
    pub(super) fn forget(&mut self, id: NodeId) -> Option<NodeId> {
        self.roots.remove(&id);
        self.assigned.remove(&id);
        self.hosts.remove(&id)
    }
}

/// The tags other than custom elements that `attachShadow` is allowed on.
const HOSTABLE: &[&str] = &[
    "article",
    "aside",
    "blockquote",
    "body",
    "div",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "main",
    "nav",
    "p",
    "section",
    "span",
];

/// Names that look like custom element names but are reserved by SVG and MathML.
const RESERVED: &[&str] = &[
    "annotation-xml",
    "color-profile",
    "font-face",
    "font-face-src",
    "font-face-uri",
    "font-face-format",
    "font-face-name",
    "missing-glyph",
];

/// Whether `name` may be the name of a custom element: lower case, with a hyphen, and not
/// one of the reserved ones.
pub(crate) fn is_custom_element_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.contains('-')
        && !name.contains(|c: char| c.is_ascii_uppercase() || c.is_whitespace())
        && !RESERVED.contains(&name)
}

impl Dom {
    /// Attaches a shadow root to `host`, which takes what it holds out of the host's children:
    /// answers `None` when the element already hosts one.
    pub(crate) fn attach_shadow(
        &mut self,
        host: NodeId,
        mode: ShadowMode,
        delegates_focus: bool,
        assignment: SlotAssignment,
    ) -> Option<NodeId> {
        if !self.exists(host) || self.shadows.hosts.contains_key(&host) {
            return None;
        }
        let root = self.insert(Node::new(NodeKind::Fragment));
        let settings = Shadow {
            host,
            mode,
            delegates_focus,
            assignment,
        };
        self.shadows.roots.insert(root, settings);
        self.shadows.hosts.insert(host, root);
        // A shadow root has no place of its own in any tree: it lives and dies with its host.
        self.roots.remove(&root);
        self.note_slot_input(None);
        self.mark(host);
        self.mark(root);
        self.styles_revision = self.styles_revision.wrapping_add(1);
        if self.is_connected(host) {
            self.set_connected(root, true);
        }
        Some(root)
    }

    /// Whether `attachShadow` may be called on `host`: a custom element, or one of the tags
    /// the specification lists.
    pub(crate) fn can_host_shadow(&self, host: NodeId) -> bool {
        self.get(host).is_some_and(|node| {
            node.namespace == super::Namespace::Html
                && node
                    .tag()
                    .is_some_and(|tag| HOSTABLE.contains(&tag) || is_custom_element_name(tag))
        })
    }

    /// The shadow root `host` hosts.
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        self.shadows.root_of_host(host)
    }

    /// The element that hosts a shadow root.
    pub fn shadow_host(&self, root: NodeId) -> Option<NodeId> {
        self.shadow(root).map(|shadow| shadow.host)
    }

    /// Whether `id` is a shadow root.
    pub fn is_shadow_root(&self, id: NodeId) -> bool {
        self.shadows.roots.contains_key(&id)
    }

    /// What the shadow root `root` is: its host, mode and slot assignment.
    pub fn shadow(&self, root: NodeId) -> Option<&Shadow> {
        self.shadows.roots.get(&root)
    }

    /// The hosts of the shadow trees `id` is in, innermost first: where an event or the focus
    /// goes next when it leaves a shadow tree.
    pub(crate) fn host_chain(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let up = |at: &NodeId| self.shadow_host(self.root_of(*at));
        std::iter::successors(up(&id), up)
    }

    /// The root of the tree `id` is in, going through shadow roots (`getRootNode({composed})`).
    pub(crate) fn composed_root_of(&self, id: NodeId) -> NodeId {
        self.root_of(self.host_chain(id).last().unwrap_or(id))
    }

    /// Whether `viewer` is in the shadow tree `host` hosts, nested ones included.
    fn is_inside_shadow(&self, host: NodeId, viewer: Option<NodeId>) -> bool {
        let root = self.shadow_root(host);
        root.zip(viewer)
            .is_some_and(|(root, viewer)| self.shadow_including_contains(root, viewer))
    }

    /// What `target` appears as to `viewer`: the host of each shadow tree it is in that the
    /// viewer is not inside (`None` is outside every tree), as events and `activeElement` say.
    pub(crate) fn retarget(&self, target: NodeId, viewer: Option<NodeId>) -> NodeId {
        let mut at = target;
        for host in self.host_chain(target) {
            if self.is_inside_shadow(host, viewer) {
                break;
            }
            at = host;
        }
        at
    }

    /// Whether `viewer` may see `item` in an event's path: not what is inside a closed shadow
    /// tree `viewer` is outside of.
    pub(crate) fn is_visible_from(&self, item: NodeId, viewer: Option<NodeId>) -> bool {
        self.host_chain(item).all(|host| {
            let closed = (self.shadow_root(host).and_then(|root| self.shadow(root)))
                .is_some_and(|shadow| shadow.mode == ShadowMode::Closed);
            !closed || self.is_inside_shadow(host, viewer)
        })
    }

    /// Whether `id` has the focus, or a shadow tree it hosts does: `:focus` on a host.
    pub(crate) fn has_focus(&self, id: NodeId) -> bool {
        self.focused
            .is_some_and(|f| f == id || self.host_chain(f).any(|host| host == id))
    }

    /// Where `focus()` on `id` lands: a host that delegates focus hands it to the first element
    /// of its shadow tree `can_focus` accepts.
    pub(crate) fn focus_target(
        &self,
        id: NodeId,
        can_focus: impl Fn(&Dom, NodeId) -> bool,
    ) -> NodeId {
        let delegating = self
            .shadow_root(id)
            .filter(|root| self.shadow(*root).is_some_and(|s| s.delegates_focus));
        delegating
            .and_then(|root| {
                self.shadow_including_descendants(root)
                    .into_iter()
                    .find(|node| can_focus(self, *node))
            })
            .unwrap_or(id)
    }

    /// The children a box is drawn with: a host's shadow root's children (slots in place), a
    /// slot's assigned nodes (or its own children as fallback), anything else's own children.
    pub fn flat_children(&self, id: NodeId) -> Vec<NodeId> {
        if let Some(root) = self.shadow_root(id) {
            return self.children(root).to_vec();
        }
        if self.is_slot(id) {
            let assigned = self.slottables(id);
            if !assigned.is_empty() {
                return assigned;
            }
        }
        self.children(id).to_vec()
    }

    /// The parent in the flat tree: what an assigned node is drawn inside, the host for a
    /// shadow root's top level. `None` for a node the flat tree leaves out.
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        match self.parent(id) {
            Some(parent) if self.is_shadow_root(parent) => self.shadow_host(parent),
            Some(parent) if self.shadow_root(parent).is_some() => self.assigned_slot(id),
            Some(parent) => Some(parent),
            None => self.shadow_host(id),
        }
    }

    /// Whether `id` is a `<slot>`.
    fn is_slot(&self, id: NodeId) -> bool {
        self.get(id).and_then(|n| n.tag()) == Some("slot")
    }

    /// `root` and every node under it, the shadow roots it hosts and their nodes included, in
    /// document order.
    pub(crate) fn shadow_including_tree(&self, root: NodeId) -> Vec<NodeId> {
        let below = self.shadow_including_descendants(root);
        std::iter::once(root).chain(below).collect()
    }

    /// Every node under `root`, the shadow roots it hosts and their nodes included.
    pub(crate) fn shadow_including_descendants(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.children(root).iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.shadow_root(id));
            stack.extend(self.children(id).iter().rev());
        }
        out
    }

    /// Whether `inner` is `outer` or inside it, shadow trees included: `Node.contains`.
    pub(crate) fn shadow_including_contains(&self, outer: NodeId, inner: NodeId) -> bool {
        let mut at = Some(inner);
        while let Some(id) = at {
            if id == outer {
                return true;
            }
            at = self.parent(id).or_else(|| self.shadow_host(id));
        }
        false
    }

    /// The slot `node` is assigned to, when its parent is a host with a shadow root.
    pub(crate) fn assigned_slot(&self, node: NodeId) -> Option<NodeId> {
        let root = self.shadow_root(self.parent(node)?)?;
        let shadow = self.shadow(root)?;
        self.slots_of(root)
            .into_iter()
            .find(|slot| match shadow.assignment {
                SlotAssignment::Named => {
                    let name = self.get(node).and_then(|n| n.attr("slot")).unwrap_or("");
                    self.slot_name(*slot) == name
                }
                SlotAssignment::Manual => self
                    .shadows
                    .assigned
                    .get(slot)
                    .is_some_and(|list| list.contains(&node)),
            })
    }

    /// What is assigned to a slot: its slottables.
    pub(crate) fn slottables(&self, slot: NodeId) -> Vec<NodeId> {
        let root = self.root_of(slot);
        let Some(shadow) = self.shadow(root) else {
            return Vec::new();
        };
        match shadow.assignment {
            SlotAssignment::Named => {
                let name = self.slot_name(slot);
                // Of several slots of one name, the first takes them.
                let first = self
                    .slots_of(root)
                    .into_iter()
                    .find(|other| self.slot_name(*other) == name);
                if first != Some(slot) {
                    return Vec::new();
                }
                let slotted_as = |child: &NodeId| {
                    self.get(*child).and_then(|n| n.attr("slot")).unwrap_or("") == name
                };
                self.children(shadow.host)
                    .iter()
                    .copied()
                    .filter(slotted_as)
                    .collect()
            }
            SlotAssignment::Manual => self
                .shadows
                .assigned
                .get(&slot)
                .into_iter()
                .flatten()
                .copied()
                .filter(|id| self.exists(*id))
                .collect(),
        }
    }

    /// `assignedNodes`: what a slot shows. With `flatten`, a slot it is given stands for what
    /// that slot shows, and an empty slot for its fallback content.
    pub(crate) fn assigned_nodes(&self, slot: NodeId, flatten: bool) -> Vec<NodeId> {
        let assigned = self.slottables(slot);
        if !flatten {
            return assigned;
        }
        let shown = if assigned.is_empty() {
            self.children(slot).to_vec()
        } else {
            assigned
        };
        let in_shadow_slot =
            |id: &NodeId| self.is_slot(*id) && self.is_shadow_root(self.root_of(*id));
        shown
            .into_iter()
            .flat_map(|node| {
                if in_shadow_slot(&node) {
                    self.assigned_nodes(node, true)
                } else {
                    vec![node]
                }
            })
            .collect()
    }

    /// The slots of a shadow tree, in tree order.
    pub(crate) fn slots_of(&self, root: NodeId) -> Vec<NodeId> {
        self.subtree(root)
            .into_iter()
            .filter(|id| self.is_slot(*id))
            .collect()
    }

    /// What every slot of every shadow tree shows now.
    pub(crate) fn slot_assignments(&self) -> Vec<(NodeId, Vec<NodeId>)> {
        let roots = self.shadows.roots.keys();
        let slots = roots.flat_map(|root| self.slots_of(*root));
        slots.map(|slot| (slot, self.slottables(slot))).collect()
    }

    /// `slot.assign(…)`'s slottables.
    pub(crate) fn assign_slot(&mut self, slot: NodeId, nodes: Vec<NodeId>) {
        self.shadows.assigned.insert(slot, nodes);
        self.note_slot_input(None);
        self.mark(slot);
    }

    /// Notes that something that decides a slot's assignment changed: the tree's shape (`None`)
    /// or an attribute, which matters when it is `slot` or `name`.
    pub(super) fn note_slot_input(&mut self, attr: Option<&str>) {
        if attr.is_none_or(|name| matches!(name, "slot" | "name")) {
            self.slot_dirty = true;
        }
    }

    /// Whether something that decides a slot's assignment changed since this was last asked.
    pub(crate) fn take_slot_dirty(&mut self) -> bool {
        std::mem::take(&mut self.slot_dirty)
    }

    fn slot_name(&self, slot: NodeId) -> &str {
        self.get(slot).and_then(|n| n.attr("name")).unwrap_or("")
    }
}
