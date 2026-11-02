//! The cascade: which declaration wins where, and the computed styles it leaves.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::scope::matched_rules;
use super::selector::MatchCache;
use super::sheet::Declaration;
use super::styler::Styler;
use super::values::{
    FONT_LONGHANDS, font_longhands, font_size_px, format_px, is_inherited, length, resolve_custom,
    set_declared, substitute,
};
use crate::dom::{Dom, Namespace, NodeId};

/// Where a sheet comes from, which decides first between two declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// The browser's defaults: `div { display: block }`.
    UserAgent,
    /// The embedder's and the document's.
    Author,
}

/// Where a declaration ranks in the cascade, lowest first: normal declarations from the user
/// agent up to the inline style, then the `!important` ones the other way round. Between
/// trees, the outer one wins a normal declaration and the inner one an important one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Precedence {
    /// The user agent's normal declarations.
    UserAgent,
    /// Normal author declarations of an inner tree reaching out (`:host`, `::slotted`).
    InnerAuthor,
    /// Normal author declarations.
    Author,
    /// The `style` attribute.
    Inline,
    /// `!important` author declarations.
    ImportantAuthor,
    /// `!important` in the `style` attribute.
    ImportantInline,
    /// `!important` declarations of an inner tree reaching out.
    ImportantInnerAuthor,
    /// The user agent's `!important` declarations.
    ImportantUserAgent,
}

impl Precedence {
    /// The precedence of a rule's declaration; `inner` when it comes from a shadow tree and reaches
    /// out of it.
    pub(super) fn of(important: bool, origin: Origin, inner: bool) -> Self {
        match (important, origin, inner) {
            (false, Origin::UserAgent, _) => Self::UserAgent,
            (false, Origin::Author, true) => Self::InnerAuthor,
            (false, Origin::Author, false) => Self::Author,
            (true, Origin::Author, true) => Self::ImportantInnerAuthor,
            (true, Origin::Author, false) => Self::ImportantAuthor,
            (true, Origin::UserAgent, _) => Self::ImportantUserAgent,
        }
    }
}

/// A node's properties, after the cascade.
pub type Style = HashMap<String, String>;

/// Every node's properties, kept from one change to the next.
///
/// Worked out whole when the stylesheets change, and otherwise only for the subtrees whose
/// nodes changed — a class added, a child put in — each from its parent, since a selector can
/// read a node's place among its siblings and everything it is in.
///
/// Each node's properties are held by an `Arc`, so a copy — the snapshot a drawer reads —
/// shares them, and a node worked out again gets a map of its own.
#[derive(Clone, Default)]
pub struct Computed {
    styles: HashMap<NodeId, Arc<Style>>,
    key: Option<u64>,
    /// The nodes a `:hover` or `:active` rule would change: the ones a drawer follows the
    /// pointer on.
    pointer: HashSet<NodeId>,
}

impl Computed {
    /// The value of `property` on `node`, after the cascade and `var()`.
    pub fn get(&self, node: NodeId, property: &str) -> Option<&str> {
        self.styles
            .get(&node)
            .and_then(|style| style.get(property))
            .map(String::as_str)
    }

    pub fn style(&self, node: NodeId) -> Option<&Style> {
        self.styles.get(&node).map(|style| &**style)
    }

    /// The node's style, shared.
    pub fn shared(&self, node: NodeId) -> Option<Arc<Style>> {
        self.styles.get(&node).cloned()
    }

    /// Whether a `:hover` or `:active` rule would change how `node` looks.
    pub fn hover_sensitive(&self, node: NodeId) -> bool {
        self.pointer.contains(&node)
    }

    /// Cascades `styler` onto every node in the document.
    pub fn cascade(dom: &Dom, styler: &Styler) -> Self {
        let mut out = Computed::default();
        out.subtree(dom, styler, dom.document, &Style::new());
        out
    }

    /// Brings every style up to date with `changed`: everything again when the sheets are not
    /// the ones (`key`, a number that moves when they change) the styles were worked out from,
    /// otherwise each changed node's subtree from its parent. Answers how many nodes were
    /// worked out.
    pub fn refresh(&mut self, dom: &Dom, styler: &Styler, key: u64, changed: &[NodeId]) -> usize {
        if self.key != Some(key) {
            self.styles.clear();
            self.pointer.clear();
            self.key = Some(key);
            return self.subtree(dom, styler, dom.document, &Style::new());
        }
        let mut roots: Vec<NodeId> = Vec::new();
        for id in changed {
            if !dom.exists(*id) || !dom.is_connected(*id) {
                continue;
            }
            let root = dom.flat_parent(*id).unwrap_or(*id);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        // A root inside another root is worked out with it.
        let outer: Vec<NodeId> = roots
            .iter()
            .copied()
            .filter(|root| {
                !roots
                    .iter()
                    .any(|other| other != root && dom.shadow_including_contains(*other, *root))
            })
            .collect();
        let mut count = 0;
        for root in outer {
            let inherited = dom
                .flat_parent(root)
                .and_then(|parent| self.styles.get(&parent).map(|style| (**style).clone()))
                .unwrap_or_default();
            count += self.subtree(dom, styler, root, &inherited);
        }
        count
    }

    /// Whether these styles were worked out from the sheets `key` stands for.
    pub fn is_fresh(&self, key: u64) -> bool {
        self.key == Some(key)
    }

    /// Forgets the styles of nodes that are gone.
    pub fn forget(&mut self, ids: &[NodeId]) {
        for id in ids {
            self.styles.remove(id);
            self.pointer.remove(id);
        }
    }

    fn subtree(&mut self, dom: &Dom, styler: &Styler, root: NodeId, inherited: &Style) -> usize {
        let mut count = 0;
        let mut stack = vec![(root, inherited.clone())];
        let mut cache = MatchCache::default();
        while let Some((id, inherited)) = stack.pop() {
            let (style, pointer) = own_style(dom, styler, id, &inherited, &mut cache);
            for child in dom.flat_children(id) {
                stack.push((child, style.clone()));
            }
            self.styles.insert(id, Arc::new(style));
            if pointer {
                self.pointer.insert(id);
            } else {
                self.pointer.remove(&id);
            }
            count += 1;
        }
        count
    }
}

/// The style of one node, worked out along its ancestors from the root: what
/// `getComputedStyle` answers in the middle of a turn, before the document is cascaded again.
pub fn style_of(dom: &Dom, styler: &Styler, id: NodeId) -> Style {
    let mut chain = vec![id];
    let mut at = dom.flat_parent(id);
    while let Some(node) = at {
        chain.push(node);
        at = dom.flat_parent(node);
    }
    let mut style = Style::new();
    let mut cache = MatchCache::default();
    for node in chain.into_iter().rev() {
        style = own_style(dom, styler, node, &style, &mut cache).0;
    }
    style
}

/// Where a declaration ranks in the cascade: its precedence, specificity, order.
pub(super) type Rank = (Precedence, (u32, u32, u32), usize);

fn own_style(
    dom: &Dom,
    styler: &Styler,
    id: NodeId,
    inherited: &Style,
    cache: &mut MatchCache,
) -> (Style, bool) {
    let mut style = Style::new();
    for (key, value) in inherited {
        if is_inherited(key) {
            style.insert(key.clone(), value.clone());
        }
    }
    let Some(node) = dom.get(id) else {
        return (style, false);
    };
    if !node.is_element() {
        return (style, false);
    }
    let (mut matched, pointer) = matched_rules(dom, styler, id, cache);
    // The inline style: over every rule of its importance.
    let inline: Vec<Declaration> = node
        .style
        .iter()
        .map(|(name, value)| Declaration {
            name: name.clone(),
            value: value.clone(),
            important: node.important.contains(name),
        })
        .collect();
    for declaration in &inline {
        let precedence = if declaration.important {
            Precedence::ImportantInline
        } else {
            Precedence::Inline
        };
        matched.push(((precedence, (u32::MAX, 0, 0), usize::MAX), declaration));
    }
    matched.sort_by_key(|a| a.0);
    // `hidden` is the user agent's `display: none`, under any author rule.
    if node.attr("hidden").is_some() && node.namespace == Namespace::Html {
        style.insert("display".into(), "none".into());
    }
    // The `font` shorthand is opened up once its `var()`s are resolved (below): the longhands
    // this node declares after its own `font` win over it, as later declarations do.
    let mut own_font = false;
    let mut font_overridden: Vec<&str> = Vec::new();
    for (_, declaration) in matched {
        if declaration.name == "font" {
            own_font = true;
            font_overridden.clear();
        } else if own_font && FONT_LONGHANDS.contains(&declaration.name.as_str()) {
            font_overridden.push(declaration.name.as_str());
        }
        set_declared(&mut style, &declaration.name, &declaration.value, inherited);
    }
    // `var()` resolved against what this node holds, custom properties included.
    let custom: HashMap<String, String> = style
        .iter()
        .filter(|(k, _)| k.starts_with("--"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let custom = resolve_custom(&custom);
    let keys: Vec<String> = style.keys().cloned().collect();
    for key in keys {
        let resolved = if key.starts_with("--") {
            custom.get(&key).cloned()
        } else if style[&key].contains("var(") {
            substitute(&style[&key], &custom, 0)
        } else {
            Some(style[&key].clone())
        };
        match resolved {
            Some(value) => {
                style.insert(key, value);
            }
            None => {
                style.remove(&key);
                if !key.starts_with("--")
                    && is_inherited(&key)
                    && let Some(value) = inherited.get(&key)
                {
                    style.insert(key, value.clone());
                }
            }
        }
    }
    if own_font && let Some(longhands) = style.get("font").and_then(|font| font_longhands(font)) {
        for (name, value) in longhands {
            if !font_overridden.contains(&name) {
                style.insert(name.to_owned(), value);
            }
        }
    }
    // A font size is computed to pixels, so what inherits it inherits the size and not `1.5em`
    // to apply again.
    let parent_size = inherited
        .get("font-size")
        .and_then(|v| length(v, 16.))
        .unwrap_or(16.);
    if let Some(size) = style.get("font-size").cloned()
        && let Some(px) = font_size_px(&size, parent_size)
    {
        style.insert("font-size".into(), format_px(px));
    }
    (style, pointer)
}
