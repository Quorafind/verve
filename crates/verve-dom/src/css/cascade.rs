//! The cascade: which declaration wins where, and the computed styles it leaves.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::boxes::Boxes;
use super::calc::{length, length_calc, math_text};
use super::color::color_text;
use super::layer::{History, Place};
use super::media::Environment;
use super::pseudo::{PseudoBox, PseudoElement};
use super::scope::matched_rules;
use super::selector::MatchCache;
use super::sheet::Declaration;
use super::shorthand::{expansion, longhands, pending};
use super::styler::Styler;
use super::values::{
    FONT_LONGHANDS, font_longhands, format_px, is_inherited, resolve_custom, set_declared,
    substitute,
};
use super::{ROOT_FONT_SIZE, font_size_px};
use crate::dom::{Dom, Namespace, NodeId};

/// Where a sheet comes from, which decides first between two declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// The browser's defaults: `div { display: block }`.
    UserAgent,
    /// The user's own sheet: `webFrame.insertCSS(css, { cssOrigin: 'user' })`. Between the
    /// browser's and the page's, and its `!important` declarations over the page's.
    User,
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
    /// The user's normal declarations.
    User,
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
    /// The user's `!important` declarations.
    ImportantUser,
    /// The user agent's `!important` declarations.
    ImportantUserAgent,
}

impl Precedence {
    /// The precedence of a rule's declaration; `inner` when it comes from a shadow tree and reaches
    /// out of it.
    pub(super) fn of(important: bool, origin: Origin, inner: bool) -> Self {
        match (important, origin, inner) {
            (false, Origin::UserAgent, _) => Self::UserAgent,
            (false, Origin::User, _) => Self::User,
            (false, Origin::Author, true) => Self::InnerAuthor,
            (false, Origin::Author, false) => Self::Author,
            (true, Origin::Author, true) => Self::ImportantInnerAuthor,
            (true, Origin::Author, false) => Self::ImportantAuthor,
            (true, Origin::User, _) => Self::ImportantUser,
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
    /// The boxes of the pseudo-elements.
    boxes: Boxes,
    /// What an animation makes of the nodes it drives, over what the cascade made of them.
    overlay: HashMap<NodeId, Arc<Style>>,
}

impl Computed {
    /// The value of `property` on `node`, after the cascade and `var()` — and the animation
    /// that drives it, if one does.
    pub fn get(&self, node: NodeId, property: &str) -> Option<&str> {
        self.style(node)
            .and_then(|style| style.get(property))
            .map(String::as_str)
    }

    /// The node's style as it shows now, if it has been worked out: an animation's values over
    /// the cascade's.
    pub fn style(&self, node: NodeId) -> Option<&Style> {
        self.overlay
            .get(&node)
            .or_else(|| self.styles.get(&node))
            .map(|style| &**style)
    }

    /// The node's style as the cascade made it, animations aside, shared: what a restyle is
    /// compared with and an animation starts from.
    pub(crate) fn cascaded(&self, node: NodeId) -> Option<Arc<Style>> {
        self.styles.get(&node).cloned()
    }

    /// Puts the values animations give the nodes they drive over the cascade's. Answers the
    /// nodes whose shown style changed by it.
    pub(crate) fn set_overlay(&mut self, overlay: HashMap<NodeId, Arc<Style>>) -> Vec<NodeId> {
        let moved = |a: &HashMap<NodeId, Arc<Style>>, b: &HashMap<NodeId, Arc<Style>>| {
            a.iter()
                .filter(|(node, style)| b.get(node) != Some(style))
                .map(|(node, _)| *node)
                .collect::<Vec<_>>()
        };
        let mut changed = moved(&overlay, &self.overlay);
        changed.extend(moved(&self.overlay, &overlay));
        changed.sort_unstable();
        changed.dedup();
        self.overlay = overlay;
        changed
    }

    /// Whether a `:hover` or `:active` rule would change how `node` looks.
    pub fn hover_sensitive(&self, node: NodeId) -> bool {
        self.pointer.contains(&node)
    }

    /// The generated boxes (an element's `::before`, `::after`) and the styles of the parts
    /// that are no boxes of their own, by the element and the part. They are not nodes.
    pub(crate) fn pseudo(&self) -> &Arc<HashMap<(NodeId, PseudoElement), PseudoBox>> {
        self.boxes.all()
    }

    /// The nodes whose pseudo-element boxes changed since this was last asked, which look
    /// different though their own style may not.
    pub(crate) fn take_pseudo_changes(&mut self) -> Vec<NodeId> {
        self.boxes.take_changed()
    }

    /// Cascades `styler` onto every node in the document.
    pub fn cascade(dom: &Dom, styler: &Styler) -> Self {
        let mut out = Computed::default();
        out.subtree(dom, styler, dom.document, &Style::new());
        out.boxes.count(dom, &out.styles);
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
            self.boxes.clear();
            self.key = Some(key);
            let count = self.subtree(dom, styler, dom.document, &Style::new());
            self.boxes.count(dom, &self.styles);
            return count;
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
        self.boxes.count(dom, &self.styles);
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
            self.overlay.remove(id);
        }
        self.boxes.forget(ids);
    }

    fn subtree(&mut self, dom: &Dom, styler: &Styler, root: NodeId, inherited: &Style) -> usize {
        let mut count = 0;
        let mut stack = vec![(root, inherited.clone())];
        let root_font_size = self
            .style(dom.html)
            .and_then(|style| style.get("font-size"))
            .and_then(|size| length(size, ROOT_FONT_SIZE))
            .unwrap_or(ROOT_FONT_SIZE);
        let mut walk = Walk::new(dom, styler, root_font_size);
        while let Some((id, inherited)) = stack.pop() {
            let Styled {
                style,
                pointer,
                mut parts,
            } = walk.styled(id, &inherited, Parts::Matching);
            for child in dom.flat_children(id) {
                stack.push((child, style.clone()));
            }
            // What is not rendered has no boxes.
            let rendered = style.get("display").is_none_or(|display| display != "none");
            for part in &styler.parts {
                let found = parts.iter().position(|(found, _)| found == part);
                let made = found.filter(|_| rendered).map(|at| parts.swap_remove(at).1);
                self.boxes.set(dom, id, *part, made);
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
    style_along(dom, styler, id, Parts::No).0
}

/// [`style_of`] for one of the element's parts: `::before`'s style, which inherits from the
/// element and is set by the rules written `::before` — what `getComputedStyle(el, '::before')`
/// answers, whether or not the part has a box.
pub(crate) fn style_of_part(dom: &Dom, styler: &Styler, id: NodeId, part: PseudoElement) -> Style {
    style_along(dom, styler, id, Parts::Only(part)).1
}

/// The style of `id` and of the `wanted` part of it, worked out down from the root.
fn style_along(dom: &Dom, styler: &Styler, id: NodeId, wanted: Parts) -> (Style, Style) {
    let mut chain = vec![id];
    let mut at = dom.flat_parent(id);
    while let Some(node) = at {
        chain.push(node);
        at = dom.flat_parent(node);
    }
    let mut walk = Walk::new(dom, styler, ROOT_FONT_SIZE);
    let mut style = Style::new();
    let mut part = Style::new();
    for node in chain.into_iter().rev() {
        // Only the element itself has a part the caller asked about.
        let wanted = if node == id { wanted } else { Parts::No };
        let styled = walk.styled(node, &style, wanted);
        style = styled.style;
        part = styled
            .parts
            .into_iter()
            .next()
            .map(|(_, part)| part)
            .unwrap_or_default();
    }
    (style, part)
}

/// Where a declaration ranks in the cascade, lowest first: its precedence, then the layer it
/// is in, then the specificity of its selector, then the order of its rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Rank {
    pub(super) precedence: Precedence,
    /// The layer's place as [`layer::key`] gives it.
    pub(super) layer: u32,
    pub(super) specificity: (u32, u32, u32),
    pub(super) order: usize,
}

impl Rank {
    /// The inline style's: over every rule of its precedence.
    fn inline(important: bool) -> Self {
        Self {
            precedence: if important {
                Precedence::ImportantInline
            } else {
                Precedence::Inline
            },
            layer: 0,
            specificity: (u32::MAX, 0, 0),
            order: usize::MAX,
        }
    }

    /// The origin, importance and layer, which `revert-layer` rolls back by.
    fn place(self) -> Place {
        (self.precedence, self.layer)
    }
}

/// One pass of the cascade over a tree: what it reads, and what it keeps from one node to the
/// next.
struct Walk<'a> {
    dom: &'a Dom,
    styler: &'a Styler,
    cache: MatchCache,
    /// The root element's font size, which `rem` is measured against; sixteen until the root
    /// is styled.
    root_font_size: f32,
    /// What values came to, kept for the pass: a page says the same few hundred values over and
    /// over, and working one out is a parse.
    memo: Memo,
}

/// The computed text of colors and math functions already worked out in a pass.
#[derive(Default)]
struct Memo {
    colors: HashMap<String, Option<String>>,
    /// By value, font size and root font size (their bits, as `f32` is no key).
    math: HashMap<(String, u32, u32), Option<String>>,
}

/// What the cascade makes of a node: its style, whether the pointer being on it would make
/// another rule apply, and the styles of those of its parts that are pseudo-elements.
struct Styled {
    style: Style,
    pointer: bool,
    parts: Vec<(PseudoElement, Style)>,
}

/// Which parts of an element are worked out beside it.
#[derive(Clone, Copy)]
enum Parts {
    /// None: the style of an ancestor, or of a text node.
    No,
    /// Those that rules are written for and the element can have: what a drawer is given.
    Matching,
    /// This one, whatever the rules say: what `getComputedStyle(element, '::before')` answers.
    Only(PseudoElement),
}

/// What differs between the styles of the things a walk works out.
#[derive(Clone, Copy, Default)]
struct Cascading {
    /// It is the root element, whose `rem` is the initial font size.
    root: bool,
    /// It has the `hidden` attribute, which is the user agent's `display: none`.
    hidden: bool,
    /// Some declaration of it says `revert-layer`.
    revert: bool,
}

impl<'a> Walk<'a> {
    fn new(dom: &'a Dom, styler: &'a Styler, root_font_size: f32) -> Self {
        Self {
            dom,
            styler,
            cache: MatchCache::default(),
            root_font_size,
            memo: Memo::default(),
        }
    }

    /// The style of one node from what it `inherited`, with those of its `wanted` parts.
    fn styled(&mut self, id: NodeId, inherited: &Style, wanted: Parts) -> Styled {
        let Some(node) = self.dom.get(id).filter(|node| node.is_element()) else {
            return Styled {
                style: inheritable(inherited),
                pointer: false,
                parts: Vec::new(),
            };
        };
        let inline: Vec<Declaration> = node
            .style
            .iter()
            .map(|(name, value)| Declaration {
                name: name.clone(),
                value: value.clone(),
                important: node.important.contains(name),
            })
            .collect();
        let styler = self.styler;
        let mut matched = matched_rules(self.dom, styler, id, &mut self.cache);
        matched.element.extend(
            inline
                .iter()
                .map(|declaration| (Rank::inline(declaration.important), declaration)),
        );
        matched.element.sort_by_key(|a| a.0);
        let root = id == self.dom.html;
        let cascading = Cascading {
            root,
            hidden: node.attr("hidden").is_some() && node.namespace == Namespace::Html,
            revert: styler.revert_layer || inline.iter().any(says_revert_layer),
        };
        let (style, font_size) = self.compute(matched.element, inherited, cascading);
        if root {
            self.root_font_size = font_size;
        }
        let mut parts = Vec::new();
        for (part, mut declarations) in matched.parts {
            let wanted = match wanted {
                Parts::No => false,
                // A reset like `*, ::before, ::after { box-sizing: border-box }` is about every
                // element, but a generated box with no `content` is no box.
                Parts::Matching => {
                    let generated = matches!(part, PseudoElement::Before | PseudoElement::After);
                    part.is_part_of(node, &style)
                        && (!generated || declarations.iter().any(|(_, d)| d.name == "content"))
                }
                Parts::Only(only) => part == only,
            };
            if wanted {
                declarations.sort_by_key(|a| a.0);
                parts.push((part, self.part_style(declarations, &style)));
            }
        }
        if let Parts::Only(only) = wanted
            && parts.is_empty()
        {
            parts.push((only, self.part_style(Vec::new(), &style)));
        }
        Styled {
            style,
            pointer: matched.pointer,
            parts,
        }
    }

    /// The style of a part of an element, which inherits from the element and is set by the
    /// rules written for the part.
    fn part_style(&mut self, declarations: Vec<(Rank, &Declaration)>, element: &Style) -> Style {
        let cascading = Cascading {
            revert: self.styler.revert_layer,
            ..Cascading::default()
        };
        self.compute(declarations, element, cascading).0
    }

    /// The style `matched`, lowest rank first, makes over what is `inherited`, with the size of
    /// its font in pixels.
    fn compute(
        &mut self,
        matched: Vec<(Rank, &Declaration)>,
        inherited: &Style,
        cascading: Cascading,
    ) -> (Style, f32) {
        let mut style = inheritable(inherited);
        // `hidden` is the user agent's `display: none`, under any author rule.
        if cascading.hidden {
            style.insert("display".into(), "none".into());
        }
        let font = apply(&mut style, matched, inherited, cascading.revert);
        resolve_variables(&mut style, inherited);
        normalize_colors(&mut style, &mut self.memo.colors);
        font.open(&mut style);
        let root = if cascading.root {
            ROOT_FONT_SIZE
        } else {
            self.root_font_size
        };
        let env = self.styler.environment();
        let font_size = compute_font_size(&mut style, inherited, root, env);
        compute_math(&mut style, (font_size, root), env, &mut self.memo.math);
        (style, font_size)
    }
}

/// The properties of a parent a child takes unless something says otherwise.
fn inheritable(parent: &Style) -> Style {
    parent
        .iter()
        .filter(|(key, _)| is_inherited(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Whether a declaration's value is the keyword `revert-layer`.
pub(super) fn says_revert_layer(declaration: &Declaration) -> bool {
    declaration
        .value
        .trim()
        .eq_ignore_ascii_case("revert-layer")
}

/// The `font` shorthand is opened up once its `var()`s are resolved: the longhands a node
/// declares after its own `font` win over it, as later declarations do.
#[derive(Default)]
struct Font<'a> {
    own: bool,
    overridden: Vec<&'a str>,
}

impl<'a> Font<'a> {
    fn note(&mut self, property: &'a str) {
        if property == "font" {
            self.own = true;
            self.overridden.clear();
        } else if self.own && FONT_LONGHANDS.contains(&property) {
            self.overridden.push(property);
        }
    }

    fn open(&self, style: &mut Style) {
        if !self.own {
            return;
        }
        if let Some(longhands) = style.get("font").and_then(|font| font_longhands(font)) {
            for (name, value) in longhands {
                if !self.overridden.contains(&name) {
                    style.insert(name.to_owned(), value);
                }
            }
        }
    }
}

/// Sets `style` by `matched`, lowest rank first. With `revert`, what each longhand was is kept
/// so that `revert-layer` can roll it back: by longhand, so that `padding: revert-layer` finds
/// what `padding-top` said and the other way round.
fn apply<'a>(
    style: &mut Style,
    matched: Vec<(Rank, &'a Declaration)>,
    inherited: &Style,
    revert: bool,
) -> Font<'a> {
    let mut font = Font::default();
    let mut history = History::default();
    for (rank, declaration) in matched {
        let (name, value) = (declaration.name.as_str(), declaration.value.as_str());
        font.note(name);
        if !revert {
            set_declared(style, name, value, inherited);
        } else if says_revert_layer(declaration) {
            for longhand in longhands(name) {
                // What the layers under it said, opened up as a declaration is; `unset` when
                // none did, as a property nothing set.
                match history.revert(&longhand, rank.place()).map(str::to_owned) {
                    Some(before) => style.insert(longhand, before),
                    None => {
                        set_declared(style, &longhand, "unset", inherited);
                        None
                    }
                };
            }
        } else {
            let before = style.clone();
            set_declared(style, name, value, inherited);
            for (key, now) in style.iter() {
                if before.get(key) != Some(now) {
                    history.record(key, rank.place(), now);
                }
            }
        }
    }
    font
}

/// `var()` resolved against what `style` holds, custom properties included; a value that
/// cannot be falls back to what it inherits, if it is inherited.
fn resolve_variables(style: &mut Style, inherited: &Style) {
    let custom: HashMap<String, String> = style
        .iter()
        .filter(|(k, _)| k.starts_with("--"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let custom = resolve_custom(&custom);
    let keys: Vec<String> = style.keys().cloned().collect();
    let mut reopen = Vec::new();
    for key in keys {
        let varied = !key.starts_with("--") && style[&key].contains("var(");
        let resolved = if key.starts_with("--") {
            custom.get(&key).cloned()
        } else if varied {
            substitute(&style[&key], &custom, 0)
        } else {
            Some(style[&key].clone())
        };
        match resolved {
            Some(value) => {
                // What a `var()` stood for may be several words of a shorthand's, which can be
                // told apart only now.
                if varied && matches!(key.as_str(), "transition" | "animation") {
                    reopen.push((key.clone(), value.clone()));
                }
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
    for (key, value) in reopen {
        if let Some(expand) = expansion(&key) {
            // Only the longhands nothing declared after the shorthand take what it comes to.
            let mut opened = Style::new();
            expand(&mut opened, &key, &value);
            for (longhand, text) in opened {
                if longhand == key || style.contains_key(&pending(&longhand)) {
                    style.insert(longhand, text);
                }
            }
        }
    }
    style.retain(|k, _| !k.starts_with('\u{0}'));
}

/// Whether `property` is a color's: what `getComputedStyle` answers as `rgb()`.
fn is_color_property(property: &str) -> bool {
    !property.starts_with("--")
        && (property == "color"
            || property.ends_with("-color")
            || matches!(property, "fill" | "stroke"))
}

/// A color is computed to the sRGB it comes to, as `getComputedStyle` answers it, except for
/// those of the newer spaces, which keep the text and the space they were written in.
fn normalize_colors(style: &mut Style, memo: &mut HashMap<String, Option<String>>) {
    for (property, value) in style.iter_mut() {
        if is_color_property(property)
            && let Some(text) = memo
                .entry(value.clone())
                .or_insert_with_key(|value| color_text(value))
        {
            value.clone_from(text);
        }
    }
    // `currentcolor` in any other color property computes to the element's own color, as
    // Chrome answers it.
    let current = style
        .get("color")
        .cloned()
        .unwrap_or_else(|| "rgb(0, 0, 0)".to_owned());
    for (property, value) in style.iter_mut() {
        if property.ends_with("-color") && value.eq_ignore_ascii_case("currentcolor") {
            value.clone_from(&current);
        }
    }
}

/// [`font_size_px`], with what `rem` and the viewport units are measured against, and
/// `calc()` too.
fn computed_font_size(value: &str, parent: f32, root: f32, env: &Environment) -> Option<f32> {
    let value = value.trim();
    if value.contains('(') || value.ends_with("rem") {
        let amount = length_calc(value, parent, root, env)?;
        return Some(amount.px + amount.percent / 100. * parent);
    }
    font_size_px(value, parent)
}

/// A font size is computed to pixels, so what inherits it inherits the size and not `1.5em`
/// to apply again. Answers the size in pixels, the node's own or its parent's.
fn compute_font_size(style: &mut Style, inherited: &Style, root: f32, env: &Environment) -> f32 {
    let parent_size = inherited
        .get("font-size")
        .and_then(|v| length(v, ROOT_FONT_SIZE))
        .unwrap_or(ROOT_FONT_SIZE);
    let own = style
        .get("font-size")
        .and_then(|size| computed_font_size(size, parent_size, root, env));
    if let Some(px) = own {
        style.insert("font-size".into(), format_px(px));
        return px;
    }
    // Written in pixels already, or not a size at all: the parent's.
    style
        .get("font-size")
        .and_then(|size| length(size, parent_size))
        .unwrap_or(parent_size)
}

/// A value that is a mathematical function is computed: what is all absolute folds into
/// pixels, what has a percentage in it keeps a `calc()` for the drawer to resolve against its
/// basis.
fn compute_math(
    style: &mut Style,
    (font_size, root): (f32, f32),
    env: &Environment,
    memo: &mut HashMap<(String, u32, u32), Option<String>>,
) {
    for (property, value) in style.iter_mut() {
        if property.starts_with("--") || !value.contains('(') {
            continue;
        }
        let key = (value.clone(), font_size.to_bits(), root.to_bits());
        if let Some(text) = memo
            .entry(key)
            .or_insert_with_key(|(value, ..)| math_text(value, font_size, root, env))
        {
            value.clone_from(text);
        }
    }
}
