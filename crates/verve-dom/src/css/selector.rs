//! Selectors: what a rule's prelude says, and which elements match it.

use std::collections::{HashMap, HashSet};

use super::pseudo::PseudoElement;
use super::scope::{Aside, Reach};
use super::styler::RuleKey;
use crate::dom::{Dom, Namespace, NodeId, NodeKind};

/// A comma-separated list of selectors: what `querySelector` and a rule's prelude are.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorList(pub Vec<Selector>);

/// One complex selector: compounds joined by combinators, kept right to left — the subject
/// first, then what it must be reached from.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    pub(super) parts: Vec<(Compound, Combinator)>,
    /// A relative selector (`:has(> img)`): where its leftmost part stands from the anchor.
    relative: Option<Combinator>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Combinator {
    /// The leftmost part, which has nothing to its left.
    None,
    Descendant,
    Child,
    /// `a + b`
    Next,
    /// `a ~ b`
    Subsequent,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrSelector>,
    pub(super) pseudo: Vec<Pseudo>,
    /// `::before` and the rest: the part of the element the compound is about, which it
    /// otherwise matches as it would the element.
    pub(super) element: Option<PseudoElement>,
    /// A pseudo-element this engine has no part for (`::file-selector-button`): matches no
    /// element.
    pub(super) foreign: bool,
    /// How far the subject reaches beyond its own tree, worked out once when it is parsed.
    pub(super) reach: Reach,
}

#[derive(Clone, Debug, PartialEq)]
struct AttrSelector {
    name: String,
    test: Option<(AttrOp, String, bool)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttrOp {
    Equals,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Pseudo {
    Hover,
    Active,
    Focus,
    FocusVisible,
    FocusWithin,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Empty,
    Root,
    Scope,
    Link,
    Defined,
    /// What is never so here: `:visited`, `:target`, `:fullscreen`…
    Never,
    Nth {
        a: i32,
        b: i32,
        last: bool,
        of_type: bool,
        of: Option<SelectorList>,
    },
    Not(SelectorList),
    Is(SelectorList),
    Where(SelectorList),
    Has(SelectorList),
    /// `:host`, and `:host(sel)`'s argument: the element hosting the sheet's shadow tree.
    Host(Option<SelectorList>),
    /// `:host-context(sel)`: a host with such an ancestor outside the shadow tree.
    HostContext(SelectorList),
    /// `::slotted(sel)`: what a slot shows, when it matches `sel`.
    Slotted(SelectorList),
    /// `::part(name)`: an element of a shadow tree its `part` attribute names.
    Part(String),
    Lang(String),
    Dir(String),
}

/// Why a selector did not parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectorError(pub String);

/// What matching needs beyond the tree: the element `:scope` stands for, and a node to treat as
/// though the pointer were on it (to learn whether a `:hover` rule would change something).
#[derive(Clone, Copy, Debug, Default)]
pub struct MatchContext {
    pub scope: Option<NodeId>,
    pub hover_probe: Option<NodeId>,
}

#[derive(Default)]
pub(super) struct MatchCache {
    // Lives for one query: it is not reused after the nodes, the styles or the scope change.
    states: HashMap<(usize, NodeId, usize, u8), bool>,
    relative: HashMap<(usize, NodeId), bool>,
    nth: HashMap<(usize, NodeId, String), HashMap<NodeId, i32>>,
}

impl SelectorList {
    /// `a, b > c` as its two selectors.
    pub fn parse(text: &str) -> Result<Self, SelectorError> {
        let mut parser = Parser::new(text);
        let list = parser.list(false)?;
        parser.skip_space();
        if !parser.done() {
            return Err(parser.error());
        }
        Ok(list)
    }

    pub fn matches(&self, dom: &Dom, id: NodeId) -> bool {
        self.matches_with(dom, id, &MatchContext::default())
    }

    /// Whether it matches `id` for a query asked of `scope`: what `:scope` stands for.
    pub fn matches_in(&self, dom: &Dom, id: NodeId, scope: Option<NodeId>) -> bool {
        self.matches_with(
            dom,
            id,
            &MatchContext {
                scope,
                hover_probe: None,
            },
        )
    }

    pub fn matches_with(&self, dom: &Dom, id: NodeId, matching: &MatchContext) -> bool {
        self.matches_cached(dom, id, matching, &mut MatchCache::default())
    }

    pub(super) fn matches_cached(
        &self,
        dom: &Dom,
        id: NodeId,
        matching: &MatchContext,
        cache: &mut MatchCache,
    ) -> bool {
        // A query asks about elements: `querySelector('p::before')` finds nothing, as in a
        // browser, even though the selector is one to write in a stylesheet.
        self.0
            .iter()
            .any(|s| s.pseudo_element().is_none() && s.matches_cached(dom, id, matching, cache))
    }

    pub(crate) fn select(
        &self,
        dom: &Dom,
        nodes: impl IntoIterator<Item = NodeId>,
        scope: Option<NodeId>,
        first: bool,
    ) -> Vec<NodeId> {
        let matching = MatchContext {
            scope,
            hover_probe: None,
        };
        let mut cache = MatchCache::default();
        let mut found = Vec::new();
        for id in nodes {
            if self.matches_cached(dom, id, &matching, &mut cache) {
                found.push(id);
                if first {
                    break;
                }
            }
        }
        found
    }

    pub(super) fn says(&self, f: &dyn Fn(&Pseudo) -> bool) -> bool {
        self.0.iter().any(|s| s.says(f))
    }
}

impl Selector {
    pub fn matches_with(&self, dom: &Dom, id: NodeId, matching: &MatchContext) -> bool {
        self.matches_cached(dom, id, matching, &mut MatchCache::default())
    }

    pub(super) fn matches_cached(
        &self,
        dom: &Dom,
        id: NodeId,
        matching: &MatchContext,
        cache: &mut MatchCache,
    ) -> bool {
        let key = self as *const Self as usize;
        // Axis 1 and 2 look for candidates among the ancestors and the previous siblings; each state
        // is evaluated at most once.
        let mut stack = vec![(id, 0, 0, false)];
        while let Some((at, part, axis, finish)) = stack.pop() {
            let state = (key, at, part, axis);
            if cache.states.contains_key(&state) {
                continue;
            }
            if axis == 0 && !finish {
                if !self.parts[part].0.matches(dom, at, matching, cache) {
                    cache.states.insert(state, false);
                    continue;
                }
                if part + 1 == self.parts.len() {
                    cache.states.insert(state, true);
                    continue;
                }
            }
            let steps = if axis != 0 {
                let next = if axis == 1 {
                    self.step_out(dom, at, part)
                } else {
                    dom.sibling(at, -1, true)
                };
                [(Some(at), part, 0), (next, part, axis)]
            } else {
                let (next, axis) = match self.parts[part].1 {
                    Combinator::Child => (self.step_out(dom, at, part), 0),
                    Combinator::Descendant | Combinator::None => (self.step_out(dom, at, part), 1),
                    Combinator::Next => (dom.sibling(at, -1, true), 0),
                    Combinator::Subsequent => (dom.sibling(at, -1, true), 2),
                };
                [(next, part + 1, axis), (None, part, 0)]
            };
            if finish {
                let matched = steps.iter().any(|(node, part, axis)| {
                    node.is_some_and(|node| {
                        cache.states.get(&(key, node, *part, *axis)) == Some(&true)
                    })
                });
                cache.states.insert(state, matched);
            } else {
                stack.push((at, part, axis, true));
                for (node, part, axis) in steps {
                    if let Some(node) = node {
                        stack.push((node, part, axis, false));
                    }
                }
            }
        }
        cache.states.get(&(key, id, 0, 0)) == Some(&true)
    }

    /// Matches a relative selector rightwards from its anchor: its left end may not borrow an
    /// ancestor from beyond the anchor.
    fn matches_relative(
        &self,
        dom: &Dom,
        anchor: NodeId,
        matching: &MatchContext,
        cache: &mut MatchCache,
    ) -> bool {
        let key = (self as *const Self as usize, anchor);
        if let Some(found) = cache.relative.get(&key) {
            return *found;
        }
        let mut frontier = vec![anchor];
        for part in (0..self.parts.len()).rev() {
            let hop = if part + 1 == self.parts.len() {
                self.relative.unwrap_or(Combinator::Descendant)
            } else {
                self.parts[part].1
            };
            let mut pending = Vec::new();
            for at in frontier {
                match hop {
                    Combinator::Next | Combinator::Subsequent => {
                        pending.extend(dom.sibling(at, 1, true))
                    }
                    _ => pending.extend(dom.children(at).iter().copied()),
                }
            }
            let mut seen = HashSet::new();
            frontier = Vec::new();
            while let Some(at) = pending.pop() {
                if !seen.insert(at) {
                    continue;
                }
                match hop {
                    Combinator::Descendant | Combinator::None => {
                        pending.extend(dom.children(at).iter().copied())
                    }
                    Combinator::Subsequent => pending.extend(dom.sibling(at, 1, true)),
                    _ => {}
                }
                if self.parts[part].0.matches(dom, at, matching, cache) {
                    frontier.push(at);
                }
            }
            if frontier.is_empty() {
                break;
            }
        }
        let found = !frontier.is_empty();
        cache.relative.insert(key, found);
        found
    }

    /// The compound whose element the selector picks: its first, kept leftmost.
    pub(super) fn subject(&self) -> &Compound {
        &self.parts[0].0
    }

    /// The part of the element it is about: `None` for the element itself.
    pub(super) fn pseudo_element(&self) -> Option<PseudoElement> {
        self.subject().element
    }

    pub(super) fn specificity(&self) -> (u32, u32, u32) {
        self.parts.iter().fold((0, 0, 0), |sum, (compound, _)| {
            let s = compound.specificity();
            (sum.0 + s.0, sum.1 + s.1, sum.2 + s.2)
        })
    }

    fn says(&self, f: &dyn Fn(&Pseudo) -> bool) -> bool {
        self.parts.iter().any(|(compound, _)| {
            compound.pseudo.iter().any(|p| {
                f(p) || match p {
                    Pseudo::Not(list)
                    | Pseudo::Is(list)
                    | Pseudo::Where(list)
                    | Pseudo::Has(list) => list.says(f),
                    Pseudo::Nth { of: Some(list), .. } => list.says(f),
                    _ => false,
                }
            })
        })
    }

    /// What the subject must be — an id, a class, a tag — for an index of rules to file it under.
    pub(super) fn key(&self) -> RuleKey {
        let Some((subject, _)) = self.parts.first() else {
            return RuleKey::Any;
        };
        if let Some(id) = &subject.id {
            return RuleKey::Id(id.clone());
        }
        if let Some(class) = subject.classes.first() {
            return RuleKey::Class(class.clone());
        }
        if let Some(tag) = &subject.tag {
            return RuleKey::Tag(tag.clone());
        }
        RuleKey::Any
    }
}

fn is_field(tag: &str) -> bool {
    matches!(
        tag,
        "input" | "textarea" | "select" | "button" | "option" | "optgroup" | "fieldset"
    )
}

impl Compound {
    fn matches(
        &self,
        dom: &Dom,
        id: NodeId,
        matching: &MatchContext,
        cache: &mut MatchCache,
    ) -> bool {
        match self.matches_scoped(dom, id, matching, cache) {
            Some(answer) => answer,
            None => self.matches_own(dom, id, matching, cache, Aside::None),
        }
    }

    /// The compound's own conditions on `id`, the pseudo that sent it here set aside.
    pub(super) fn matches_own(
        &self,
        dom: &Dom,
        id: NodeId,
        matching: &MatchContext,
        cache: &mut MatchCache,
        aside: Aside,
    ) -> bool {
        if self.foreign {
            return false;
        }
        let Some(node) = dom.get(id) else {
            return false;
        };
        let NodeKind::Element(tag) = &node.kind else {
            return false;
        };
        if let Some(want) = &self.tag {
            let same = if node.namespace == Namespace::Html {
                want.eq_ignore_ascii_case(tag)
            } else {
                want == tag || want.eq_ignore_ascii_case(tag)
            };
            if !same {
                return false;
            }
        }
        if let Some(want) = &self.id
            && node.attr("id") != Some(want.as_str())
        {
            return false;
        }
        if !self.classes.iter().all(|class| node.has_class(class)) {
            return false;
        }
        for attr in &self.attrs {
            let Some(have) = dom.get_attr(id, &attr.name) else {
                return false;
            };
            let Some((op, want, insensitive)) = &attr.test else {
                continue;
            };
            let (have, want) = if *insensitive {
                (have.to_lowercase(), want.to_lowercase())
            } else {
                (have, want.clone())
            };
            let hit = match op {
                AttrOp::Equals => have == want,
                AttrOp::Includes => !want.is_empty() && have.split_whitespace().any(|w| w == want),
                AttrOp::DashMatch => have == want || have.starts_with(&format!("{want}-")),
                AttrOp::Prefix => !want.is_empty() && have.starts_with(&want),
                AttrOp::Suffix => !want.is_empty() && have.ends_with(&want),
                AttrOp::Substring => !want.is_empty() && have.contains(&want),
            };
            if !hit {
                return false;
            }
        }
        self.pseudo.iter().all(|pseudo| match pseudo {
            Pseudo::Slotted(_) | Pseudo::Part(_) | Pseudo::Host(_) | Pseudo::HostContext(_) => {
                pseudo.matches_scoped(dom, id, aside, matching, cache)
            }
            Pseudo::Hover => {
                dom.is_hovered(id) || matching.hover_probe.is_some_and(|p| dom.contains(id, p))
            }
            Pseudo::Active => {
                dom.active.is_some_and(|a| dom.contains(id, a))
                    || matching.hover_probe.is_some_and(|p| dom.contains(id, p))
            }
            Pseudo::Focus | Pseudo::FocusVisible => dom.has_focus(id),
            Pseudo::FocusWithin => dom.focus_within(id),
            Pseudo::Checked => match tag.as_str() {
                "input" => node.checked,
                "option" => dom
                    .parent(id)
                    .and_then(|p| {
                        let select = if dom.get(p).and_then(|n| n.tag()) == Some("optgroup") {
                            dom.parent(p)?
                        } else {
                            p
                        };
                        dom.selected_option(select)
                    })
                    .is_some_and(|o| o == id),
                _ => false,
            },
            Pseudo::Disabled => is_field(tag) && disabled(dom, id),
            Pseudo::Enabled => is_field(tag) && !disabled(dom, id),
            Pseudo::Required => node.attr("required").is_some(),
            Pseudo::Optional => is_field(tag) && node.attr("required").is_none(),
            Pseudo::ReadOnly => !editable(dom, id),
            Pseudo::ReadWrite => editable(dom, id),
            Pseudo::PlaceholderShown => {
                node.attr("placeholder").is_some()
                    && node.draft.as_ref().is_some_and(|d| d.is_empty())
            }
            Pseudo::Empty => dom.children(id).iter().all(|c| {
                dom.get(*c).is_some_and(|n| match &n.kind {
                    NodeKind::Comment(_) => true,
                    NodeKind::Text(text) => text.is_empty(),
                    _ => false,
                })
            }),
            Pseudo::Root => dom
                .parent(id)
                .is_some_and(|p| matches!(dom.get(p).map(|n| &n.kind), Some(NodeKind::Document))),
            Pseudo::Scope => match matching.scope {
                Some(scope) => scope == id,
                None => dom.parent(id).is_some_and(|p| {
                    matches!(dom.get(p).map(|n| &n.kind), Some(NodeKind::Document))
                }),
            },
            Pseudo::Link => matches!(tag.as_str(), "a" | "area") && node.attr("href").is_some(),
            Pseudo::Defined => true,
            Pseudo::Never => false,
            Pseudo::Nth {
                a,
                b,
                last,
                of_type,
                of,
            } => {
                let Some(parent) = dom.parent(id) else {
                    return false;
                };
                let key = (
                    pseudo as *const Pseudo as usize,
                    parent,
                    if *of_type { tag.clone() } else { String::new() },
                );
                if !cache.nth.contains_key(&key) {
                    let siblings: Vec<NodeId> = dom
                        .children(parent)
                        .iter()
                        .copied()
                        .filter(|c| {
                            dom.get(*c).is_some_and(|n| {
                                n.is_element() && (!*of_type || n.tag() == Some(tag.as_str()))
                            }) && of
                                .as_ref()
                                .is_none_or(|of| of.matches_cached(dom, *c, matching, cache))
                        })
                        .collect();
                    let indices = siblings
                        .iter()
                        .enumerate()
                        .map(|(i, node)| {
                            let index = if *last {
                                (siblings.len() - i) as i32
                            } else {
                                i as i32 + 1
                            };
                            (*node, index)
                        })
                        .collect();
                    cache.nth.insert(key.clone(), indices);
                }
                cache.nth[&key]
                    .get(&id)
                    .is_some_and(|index| nth(*a, *b, *index))
            }
            Pseudo::Not(list) => !list.matches_cached(dom, id, matching, cache),
            Pseudo::Is(list) | Pseudo::Where(list) => list.matches_cached(dom, id, matching, cache),
            Pseudo::Has(list) => list
                .0
                .iter()
                .any(|s| s.matches_relative(dom, id, matching, cache)),
            Pseudo::Lang(want) => {
                let mut at = Some(id);
                while let Some(node) = at {
                    if let Some(lang) = dom.get(node).and_then(|n| n.attr("lang")) {
                        let lang = lang.to_ascii_lowercase();
                        let want = want.to_ascii_lowercase();
                        return lang == want || lang.starts_with(&format!("{want}-"));
                    }
                    at = dom.parent(node);
                }
                false
            }
            Pseudo::Dir(want) => {
                let mut at = Some(id);
                while let Some(node) = at {
                    if let Some(dir) = dom.get(node).and_then(|n| n.attr("dir")) {
                        return dir.eq_ignore_ascii_case(want);
                    }
                    at = dom.parent(node);
                }
                want == "ltr"
            }
        })
    }

    pub(super) fn specificity(&self) -> (u32, u32, u32) {
        let mut out = (
            self.id.is_some() as u32,
            (self.classes.len() + self.attrs.len()) as u32,
            (self.tag.is_some() || self.element.is_some() || self.foreign) as u32,
        );
        for pseudo in &self.pseudo {
            // What the pseudo adds itself, and the selector list it takes, if it counts.
            let (own, list) = match pseudo {
                Pseudo::Where(_) => continue,
                Pseudo::Not(list) | Pseudo::Is(list) | Pseudo::Has(list) => ((0, 0, 0), Some(list)),
                Pseudo::Nth { of, .. } => ((0, 1, 0), of.as_ref()),
                Pseudo::Host(list) => ((0, 1, 0), list.as_ref()),
                Pseudo::Slotted(list) => ((0, 0, 1), Some(list)),
                Pseudo::Part(_) => ((0, 0, 1), None),
                _ => ((0, 1, 0), None),
            };
            let best = list.map(best_specificity).unwrap_or_default();
            out = (
                out.0 + own.0 + best.0,
                out.1 + own.1 + best.1,
                out.2 + own.2 + best.2,
            );
        }
        out
    }
}

/// The specificity of the most specific selector of `list`.
fn best_specificity(list: &SelectorList) -> (u32, u32, u32) {
    list.0
        .iter()
        .map(Selector::specificity)
        .max()
        .unwrap_or_default()
}

fn nth(a: i32, b: i32, index: i32) -> bool {
    if a == 0 {
        return index == b;
    }
    let n = index - b;
    n % a == 0 && n / a >= 0
}

fn disabled(dom: &Dom, id: NodeId) -> bool {
    let mut at = Some(id);
    while let Some(node) = at {
        let Some(n) = dom.get(node) else { break };
        if n.attr("disabled").is_some() && n.tag().is_some_and(is_field) {
            return true;
        }
        at = dom.parent(node);
    }
    false
}

fn editable(dom: &Dom, id: NodeId) -> bool {
    let Some(node) = dom.get(id) else {
        return false;
    };
    match node.tag() {
        Some("input" | "textarea") => node.attr("readonly").is_none() && !disabled(dom, id),
        _ => dom.is_editable(id),
    }
}

/// A recursive-descent reader of selectors, character by character.
pub(super) struct Parser<'a> {
    chars: Vec<char>,
    pub(super) at: usize,
    text: &'a str,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().collect(),
            at: 0,
            text,
            depth: 0,
        }
    }

    pub(super) fn error(&self) -> SelectorError {
        SelectorError(format!("'{}' is not a valid selector", self.text))
    }

    fn done(&self) -> bool {
        self.at >= self.chars.len()
    }

    pub(super) fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    pub(super) fn skip_space(&mut self) -> bool {
        let start = self.at;
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.at += 1;
            }
            // Comments count as space.
            if self.peek() != Some('/') || self.peek_at(1) != Some('*') {
                break;
            }
            self.at += 2;
            while !self.done() && (self.peek() != Some('*') || self.peek_at(1) != Some('/')) {
                self.at += 1;
            }
            self.at = (self.at + 2).min(self.chars.len());
        }
        self.at > start
    }

    pub(super) fn list(&mut self, relative: bool) -> Result<SelectorList, SelectorError> {
        if self.depth >= 64 {
            return Err(self.error());
        }
        self.depth += 1;
        let mut out = Vec::new();
        loop {
            self.skip_space();
            out.push(self.complex(relative)?);
            self.skip_space();
            if self.peek() == Some(',') {
                self.at += 1;
                continue;
            }
            break;
        }
        self.depth -= 1;
        Ok(SelectorList(out))
    }

    fn combinator(&mut self) -> Option<Combinator> {
        match self.peek() {
            Some('>') => {
                self.at += 1;
                Some(Combinator::Child)
            }
            Some('+') => {
                self.at += 1;
                Some(Combinator::Next)
            }
            Some('~') if self.peek_at(1) != Some('=') => {
                self.at += 1;
                Some(Combinator::Subsequent)
            }
            _ => None,
        }
    }

    pub(super) fn complex(&mut self, relative: bool) -> Result<Selector, SelectorError> {
        let mut left_to_right: Vec<(Compound, Combinator)> = Vec::new();
        let mut leading = None;
        if relative {
            self.skip_space();
            leading = self.combinator();
        }
        let mut pending = Combinator::None;
        loop {
            self.skip_space();
            let compound = self.compound()?;
            left_to_right.push((compound, pending));
            let spaced = self.skip_space();
            match self.peek() {
                None | Some(',') | Some(')') => break,
                _ => {}
            }
            pending = match self.combinator() {
                Some(c) => c,
                None if spaced => Combinator::Descendant,
                None => return Err(self.error()),
            };
        }
        // A pseudo-element ends a selector: `p::before span` is no selector.
        if left_to_right[..left_to_right.len() - 1]
            .iter()
            .any(|(compound, _)| compound.element.is_some() || compound.foreign)
        {
            return Err(self.error());
        }
        // Right to left: each part carries how the part to its left is reached from it.
        let len = left_to_right.len();
        let parts = (0..len)
            .map(|k| {
                let (compound, _) = &left_to_right[len - 1 - k];
                let hop = if k + 1 < len {
                    left_to_right[len - 1 - k].1
                } else {
                    Combinator::None
                };
                (compound.clone(), hop)
            })
            .collect();
        Ok(Selector {
            parts,
            relative: leading.or(relative.then_some(Combinator::Descendant)),
        })
    }

    /// The pseudo-element of CSS 2 that follows a single colon — `:before`, `:after`,
    /// `:first-line`, `:first-letter` — and is read as the two-colon one.
    fn legacy_pseudo_element(&mut self) -> Option<PseudoElement> {
        let start = self.at;
        let name = self.ident()?.to_ascii_lowercase();
        let part = match name.as_str() {
            "before" | "after" | "first-line" | "first-letter" => PseudoElement::from_name(&name),
            _ => None,
        };
        if part.is_none() {
            self.at = start;
        }
        part
    }

    /// Whether what is left of the text starts with `prefix`.
    fn rest_starts(&self, prefix: &str) -> bool {
        prefix
            .chars()
            .enumerate()
            .all(|(k, c)| self.chars.get(self.at + k) == Some(&c))
    }

    pub(super) fn ident(&mut self) -> Option<String> {
        let mut out = String::new();
        // A leading `-` (or `--`) is part of an identifier.
        while let Some(c) = self.peek() {
            if c == '\\' {
                self.at += 1;
                let mut hex = String::new();
                while hex.len() < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    hex.push(self.peek().expect("peeked"));
                    self.at += 1;
                }
                if hex.is_empty() {
                    if let Some(c) = self.peek() {
                        out.push(c);
                        self.at += 1;
                    }
                } else {
                    if self.peek().is_some_and(char::is_whitespace) {
                        self.at += 1;
                    }
                    out.push(
                        u32::from_str_radix(&hex, 16)
                            .ok()
                            .and_then(char::from_u32)
                            .unwrap_or('\u{fffd}'),
                    );
                }
            } else if c.is_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() {
                out.push(c);
                self.at += 1;
            } else {
                break;
            }
        }
        (!out.is_empty()).then_some(out)
    }

    fn string_or_ident(&mut self) -> Result<String, SelectorError> {
        match self.peek() {
            Some(q @ ('"' | '\'')) => {
                self.at += 1;
                let mut out = String::new();
                while let Some(c) = self.peek() {
                    self.at += 1;
                    if c == q {
                        return Ok(out);
                    }
                    if c == '\\' {
                        if let Some(next) = self.peek() {
                            out.push(next);
                            self.at += 1;
                        }
                        continue;
                    }
                    out.push(c);
                }
                Err(self.error())
            }
            _ => self.ident().ok_or_else(|| self.error()),
        }
    }

    fn compound(&mut self) -> Result<Compound, SelectorError> {
        let mut out = Compound::default();
        let start = self.at;
        if self.peek() == Some('*') {
            self.at += 1;
            if self.peek() == Some('|') {
                self.at += 1;
                if self.peek() == Some('*') {
                    self.at += 1;
                } else {
                    out.tag = Some(
                        self.ident()
                            .ok_or_else(|| self.error())?
                            .to_ascii_lowercase(),
                    );
                }
            }
        } else if self
            .peek()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '\\' || !c.is_ascii())
        {
            out.tag = Some(
                self.ident()
                    .ok_or_else(|| self.error())?
                    .to_ascii_lowercase(),
            );
        }
        loop {
            // Nothing but pseudo-classes may follow a pseudo-element: `p::before.foo` and
            // `p::before::after` are no selectors.
            let ended = out.element.is_some() || out.foreign;
            if ended && (matches!(self.peek(), Some('#' | '.' | '[')) || self.rest_starts("::")) {
                return Err(self.error());
            }
            match self.peek() {
                Some('#') => {
                    self.at += 1;
                    out.id = Some(self.ident().ok_or_else(|| self.error())?);
                }
                Some('.') => {
                    self.at += 1;
                    out.classes.push(self.ident().ok_or_else(|| self.error())?);
                }
                Some('[') => {
                    self.at += 1;
                    out.attrs.push(self.attribute()?);
                }
                Some(':') => {
                    self.at += 1;
                    if self.peek() == Some(':') {
                        self.at += 1;
                        self.pseudo_element(&mut out)?;
                    } else if let Some(part) = self.legacy_pseudo_element() {
                        out.element = Some(part);
                    } else {
                        let pseudo = self.pseudo()?;
                        out.pseudo.push(pseudo);
                    }
                }
                _ => break,
            }
        }
        if self.at == start {
            return Err(self.error());
        }
        out.reach = Reach::of(&out.pseudo);
        Ok(out)
    }

    fn attribute(&mut self) -> Result<AttrSelector, SelectorError> {
        self.skip_space();
        let mut name = self.ident().ok_or_else(|| self.error())?;
        // `[ns|attr]`: the namespace is not kept.
        if self.peek() == Some('|') && self.peek_at(1) != Some('=') {
            self.at += 1;
            name = self.ident().ok_or_else(|| self.error())?;
        }
        self.skip_space();
        let op = match (self.peek(), self.peek_at(1)) {
            (Some(']'), _) => {
                self.at += 1;
                return Ok(AttrSelector {
                    name: name.to_ascii_lowercase(),
                    test: None,
                });
            }
            (Some('='), _) => {
                self.at += 1;
                AttrOp::Equals
            }
            (Some(c), Some('=')) => {
                self.at += 2;
                match c {
                    '~' => AttrOp::Includes,
                    '|' => AttrOp::DashMatch,
                    '^' => AttrOp::Prefix,
                    '$' => AttrOp::Suffix,
                    '*' => AttrOp::Substring,
                    _ => return Err(self.error()),
                }
            }
            _ => return Err(self.error()),
        };
        self.skip_space();
        let value = self.string_or_ident()?;
        self.skip_space();
        let mut insensitive = false;
        if matches!(self.peek(), Some('i' | 'I' | 's' | 'S')) {
            insensitive = matches!(self.peek(), Some('i' | 'I'));
            self.at += 1;
            self.skip_space();
        }
        if self.peek() != Some(']') {
            return Err(self.error());
        }
        self.at += 1;
        Ok(AttrSelector {
            name: name.to_ascii_lowercase(),
            test: Some((op, value, insensitive)),
        })
    }

    pub(super) fn skip_arguments(&mut self) -> Result<(), SelectorError> {
        let mut depth = 0;
        while let Some(c) = self.peek() {
            self.at += 1;
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
        Err(self.error())
    }

    pub(super) fn close(&mut self) -> Result<(), SelectorError> {
        self.skip_space();
        if self.peek() != Some(')') {
            return Err(self.error());
        }
        self.at += 1;
        Ok(())
    }

    fn pseudo(&mut self) -> Result<Pseudo, SelectorError> {
        let name = self
            .ident()
            .ok_or_else(|| self.error())?
            .to_ascii_lowercase();
        if self.peek() == Some('(') {
            self.at += 1;
            self.skip_space();
            let pseudo = match name.as_str() {
                "not" => Pseudo::Not(self.list(false)?),
                "is" | "matches" | "-webkit-any" | "any" => Pseudo::Is(self.list(false)?),
                "where" => Pseudo::Where(self.list(false)?),
                "has" => {
                    let list = self.list(true)?;
                    if list.says(&|p| matches!(p, Pseudo::Has(_))) {
                        return Err(self.error());
                    }
                    Pseudo::Has(list)
                }
                "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
                    let (a, b) = self.an_plus_b()?;
                    self.skip_space();
                    let of = if !name.ends_with("of-type")
                        && self.peek().is_some_and(|c| c == 'o' || c == 'O')
                    {
                        let word = self.ident().ok_or_else(|| self.error())?;
                        if !word.eq_ignore_ascii_case("of") {
                            return Err(self.error());
                        }
                        Some(self.list(false)?)
                    } else {
                        None
                    };
                    Pseudo::Nth {
                        a,
                        b,
                        last: name.contains("last"),
                        of_type: name.ends_with("of-type"),
                        of,
                    }
                }
                "lang" => Pseudo::Lang(self.string_or_ident()?),
                "dir" => Pseudo::Dir(
                    self.ident()
                        .ok_or_else(|| self.error())?
                        .to_ascii_lowercase(),
                ),
                "host" | "host-context" => self.host_pseudo(&name)?,
                "state" => {
                    self.at -= 1;
                    self.skip_arguments()?;
                    return Ok(Pseudo::Never);
                }
                _ => return Err(self.error()),
            };
            self.close()?;
            return Ok(pseudo);
        }
        let structural = |a, b, last, of_type| Pseudo::Nth {
            a,
            b,
            last,
            of_type,
            of: None,
        };
        Ok(match name.as_str() {
            "hover" => Pseudo::Hover,
            "active" => Pseudo::Active,
            "focus" => Pseudo::Focus,
            "focus-visible" => Pseudo::FocusVisible,
            "focus-within" => Pseudo::FocusWithin,
            "checked" => Pseudo::Checked,
            "disabled" => Pseudo::Disabled,
            "enabled" => Pseudo::Enabled,
            "required" => Pseudo::Required,
            "optional" => Pseudo::Optional,
            "read-only" => Pseudo::ReadOnly,
            "read-write" => Pseudo::ReadWrite,
            "placeholder-shown" => Pseudo::PlaceholderShown,
            "empty" => Pseudo::Empty,
            "root" => Pseudo::Root,
            "host" => Pseudo::Host(None),
            "scope" => Pseudo::Scope,
            "link" | "any-link" => Pseudo::Link,
            "defined" => Pseudo::Defined,
            "first-child" => structural(0, 1, false, false),
            "last-child" => structural(0, 1, true, false),
            "first-of-type" => structural(0, 1, false, true),
            "last-of-type" => structural(0, 1, true, true),
            "only-child" => Pseudo::Is(SelectorList(vec![Selector {
                parts: vec![(
                    Compound {
                        pseudo: vec![
                            structural(0, 1, false, false),
                            structural(0, 1, true, false),
                        ],
                        ..Compound::default()
                    },
                    Combinator::None,
                )],
                relative: None,
            }])),
            "only-of-type" => Pseudo::Is(SelectorList(vec![Selector {
                parts: vec![(
                    Compound {
                        pseudo: vec![structural(0, 1, false, true), structural(0, 1, true, true)],
                        ..Compound::default()
                    },
                    Combinator::None,
                )],
                relative: None,
            }])),
            "visited" | "target" | "target-within" | "fullscreen" | "modal" | "popover-open"
            | "autofill" | "-webkit-autofill" | "indeterminate" | "invalid" | "user-invalid"
            | "playing" | "paused" | "picture-in-picture" | "open" => Pseudo::Never,
            "valid" | "user-valid" | "default" | "in-range" | "local-link" => Pseudo::Defined,
            other
                if other.starts_with("-webkit-")
                    || other.starts_with("-moz-")
                    || other.starts_with("-ms-") =>
            {
                Pseudo::Never
            }
            _ => return Err(self.error()),
        })
    }

    /// `2n+1`, `odd`, `-n + 3`, `5`.
    fn an_plus_b(&mut self) -> Result<(i32, i32), SelectorError> {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '+' || c == '-' || c.is_whitespace() {
                // `of` begins the selector list.
                if (c == 'o' || c == 'O') && !text.trim().is_empty() {
                    break;
                }
                text.push(c);
                self.at += 1;
            } else {
                break;
            }
        }
        // Give back the space before `of`.
        let text: String = text.split_whitespace().collect();
        let text = text.to_ascii_lowercase();
        match text.as_str() {
            "odd" => return Ok((2, 1)),
            "even" => return Ok((2, 0)),
            _ => {}
        }
        let parse = |t: &str| -> Option<i32> {
            match t {
                "" | "+" => Some(1),
                "-" => Some(-1),
                t => t.parse().ok(),
            }
        };
        let parsed = match text.split_once('n') {
            Some((a, b)) => parse(a).zip(if b.is_empty() {
                Some(0)
            } else {
                b.parse::<i32>().ok()
            }),
            None => text.parse::<i32>().ok().map(|b| (0, b)),
        };
        parsed.ok_or_else(|| self.error())
    }
}
