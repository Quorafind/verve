//! How a node looks: the stylesheets — the user agent's defaults, the embedder's, every
//! `<style>` element and adopted sheet in the document — cascaded onto the tree in
//! [`crate::dom`].
//!
//! Selectors are the web's (Selectors level 4, less what needs layout): types, `*`, `#id`,
//! `.class`, attributes with every operator and the `i` flag, the descendant, child, next- and
//! subsequent-sibling combinators, `:not()`, `:is()`, `:where()`, `:has()`, the structural
//! pseudo-classes (`:nth-child(2n+1 of S)` and its family), and the state ones — `:hover` and
//! `:active` from the pointer, `:focus`, `:focus-within`, `:checked`, `:disabled`… A
//! pseudo-element (`::before`) parses and matches no element. One that does not parse is
//! `querySelector`'s `SyntaxError`, and a stylesheet's rule that has one is dropped, as a
//! browser drops it.
//!
//! The cascade is origin, importance, specificity and order; the inline style wins over every
//! rule of its importance; the inherited properties come down from the parent; `inherit`,
//! `initial` and `unset` are honoured; and `var()` reads custom properties, which inherit like
//! any other — so `var(--text-muted)` follows whatever the document set `--text-muted` to.
//! `@media` is evaluated against the [`Environment`] (the viewport, the colour scheme);
//! `@supports` and `@layer` blocks apply; the rest (`@keyframes`, `@font-face`) are not rules
//! about a node.
//!
//! What comes out is each property's value as text; turning `12px` into a length and `#fff`
//! into a colour is the drawer's, with [`length`] and [`color`].
//!
//! Lifted from su-editor's Obsidian host (its cascade, `var()` and incremental restyle) with a
//! selector engine of its own.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::dom::{Dom, Namespace, NodeId, NodeKind};

// ─── Selectors ──────────────────────────────────────────────────────────────────────────────

/// A comma-separated list of selectors: what `querySelector` and a rule's prelude are.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorList(pub Vec<Selector>);

/// One complex selector: compounds joined by combinators, kept right to left — the subject
/// first, then what it must be reached from.
#[derive(Clone, Debug, PartialEq)]
pub struct Selector {
    parts: Vec<(Compound, Combinator)>,
    /// A relative selector (`:has(> img)`): where its leftmost part stands from the anchor.
    relative: Option<Combinator>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combinator {
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
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrSelector>,
    pseudo: Vec<Pseudo>,
    /// `::before` and the rest: matches no element.
    element: bool,
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
enum Pseudo {
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

    pub fn matches_with(&self, dom: &Dom, id: NodeId, cx: &MatchContext) -> bool {
        self.0.iter().any(|s| s.matches_with(dom, id, cx))
    }

    fn says(&self, f: &dyn Fn(&Pseudo) -> bool) -> bool {
        self.0.iter().any(|s| s.says(f))
    }
}

impl Selector {
    pub fn matches_with(&self, dom: &Dom, id: NodeId, cx: &MatchContext) -> bool {
        self.matches_from(dom, id, 0, cx)
    }

    fn matches_from(&self, dom: &Dom, id: NodeId, part: usize, cx: &MatchContext) -> bool {
        let Some((compound, hop)) = self.parts.get(part) else {
            return true;
        };
        if !compound.matches(dom, id, cx) {
            return false;
        }
        if part + 1 == self.parts.len() {
            return true;
        }
        match hop {
            Combinator::Child => dom
                .parent_element(id)
                .is_some_and(|parent| self.matches_from(dom, parent, part + 1, cx)),
            Combinator::Descendant | Combinator::None => {
                let mut at = dom.parent_element(id);
                while let Some(ancestor) = at {
                    if self.matches_from(dom, ancestor, part + 1, cx) {
                        return true;
                    }
                    at = dom.parent_element(ancestor);
                }
                false
            }
            Combinator::Next => dom
                .sibling(id, -1, true)
                .is_some_and(|before| self.matches_from(dom, before, part + 1, cx)),
            Combinator::Subsequent => {
                let mut at = dom.sibling(id, -1, true);
                while let Some(before) = at {
                    if self.matches_from(dom, before, part + 1, cx) {
                        return true;
                    }
                    at = dom.sibling(before, -1, true);
                }
                false
            }
        }
    }

    /// For a relative selector (`:has(…)`), whether it matches something from `anchor`.
    fn matches_relative(&self, dom: &Dom, anchor: NodeId, cx: &MatchContext) -> bool {
        let candidates: Vec<NodeId> = match self.relative.unwrap_or(Combinator::Descendant) {
            Combinator::Child => dom
                .children(anchor)
                .iter()
                .copied()
                .filter(|c| dom.get(*c).is_some_and(|n| n.is_element()))
                .collect(),
            Combinator::Next => dom.sibling(anchor, 1, true).into_iter().collect(),
            Combinator::Subsequent => {
                let mut out = Vec::new();
                let mut at = dom.sibling(anchor, 1, true);
                while let Some(next) = at {
                    out.push(next);
                    at = dom.sibling(next, 1, true);
                }
                out
            }
            _ => dom.descendants(anchor),
        };
        // The leftmost part must stand where the combinator says from the anchor; matching
        // right to left from each candidate, the chain must end at one of those.
        candidates.into_iter().any(|candidate| {
            self.matches_with(dom, candidate, cx) && self.reaches(dom, candidate, anchor, cx)
        })
    }

    /// Whether the chain matched from `id` can end at a part standing right of `anchor`.
    fn reaches(&self, dom: &Dom, id: NodeId, anchor: NodeId, _cx: &MatchContext) -> bool {
        // A single compound: the candidate itself stands where it must.
        if self.parts.len() == 1 {
            return true;
        }
        dom.contains(anchor, id) || dom.parent(anchor) == dom.parent(id)
    }

    fn specificity(&self) -> (u32, u32, u32) {
        self.parts.iter().fold((0, 0, 0), |sum, (compound, _)| {
            let s = compound.specificity();
            (sum.0 + s.0, sum.1 + s.1, sum.2 + s.2)
        })
    }

    fn says(&self, f: &dyn Fn(&Pseudo) -> bool) -> bool {
        self.parts.iter().any(|(compound, _)| {
            compound.pseudo.iter().any(|p| {
                f(p) || match p {
                    Pseudo::Not(list) | Pseudo::Is(list) | Pseudo::Where(list) | Pseudo::Has(list) => {
                        list.says(f)
                    }
                    Pseudo::Nth { of: Some(list), .. } => list.says(f),
                    _ => false,
                }
            })
        })
    }

    /// What the subject must be — an id, a class, a tag — for an index of rules to file it under.
    fn key(&self) -> RuleKey {
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
    matches!(tag, "input" | "textarea" | "select" | "button" | "option" | "optgroup" | "fieldset")
}

impl Compound {
    fn matches(&self, dom: &Dom, id: NodeId, cx: &MatchContext) -> bool {
        if self.element {
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
            Pseudo::Hover => {
                dom.is_hovered(id) || cx.hover_probe.is_some_and(|p| dom.contains(id, p))
            }
            Pseudo::Active => {
                dom.active.is_some_and(|a| dom.contains(id, a))
                    || cx.hover_probe.is_some_and(|p| dom.contains(id, p))
            }
            Pseudo::Focus | Pseudo::FocusVisible => dom.focused == Some(id),
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
            Pseudo::Scope => match cx.scope {
                Some(scope) => scope == id,
                None => dom
                    .parent(id)
                    .is_some_and(|p| matches!(dom.get(p).map(|n| &n.kind), Some(NodeKind::Document))),
            },
            Pseudo::Link => {
                matches!(tag.as_str(), "a" | "area") && node.attr("href").is_some()
            }
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
                let siblings: Vec<NodeId> = dom
                    .children(parent)
                    .iter()
                    .copied()
                    .filter(|c| {
                        dom.get(*c).is_some_and(|n| {
                            n.is_element()
                                && (!*of_type || n.tag() == Some(tag.as_str()))
                        }) && of.as_ref().is_none_or(|of| of.matches_with(dom, *c, cx))
                    })
                    .collect();
                let Some(position) = siblings.iter().position(|c| *c == id) else {
                    return false;
                };
                let index = if *last {
                    (siblings.len() - position) as i32
                } else {
                    position as i32 + 1
                };
                nth(*a, *b, index)
            }
            Pseudo::Not(list) => !list.matches_with(dom, id, cx),
            Pseudo::Is(list) | Pseudo::Where(list) => list.matches_with(dom, id, cx),
            Pseudo::Has(list) => list.0.iter().any(|s| s.matches_relative(dom, id, cx)),
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

    fn specificity(&self) -> (u32, u32, u32) {
        let mut out = (
            self.id.is_some() as u32,
            (self.classes.len() + self.attrs.len()) as u32,
            (self.tag.is_some() || self.element) as u32,
        );
        for pseudo in &self.pseudo {
            match pseudo {
                Pseudo::Where(_) => {}
                Pseudo::Not(list) | Pseudo::Is(list) | Pseudo::Has(list) => {
                    let best = list.0.iter().map(Selector::specificity).max().unwrap_or_default();
                    out.0 += best.0;
                    out.1 += best.1;
                    out.2 += best.2;
                }
                Pseudo::Nth { of: Some(list), .. } => {
                    let best = list.0.iter().map(Selector::specificity).max().unwrap_or_default();
                    out.0 += best.0;
                    out.1 += best.1 + 1;
                    out.2 += best.2;
                }
                _ => out.1 += 1,
            }
        }
        out
    }
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
        Some("input" | "textarea") => {
            node.attr("readonly").is_none() && !disabled(dom, id)
        }
        _ => {
            let mut at = Some(id);
            while let Some(n) = at {
                if let Some(value) = dom.get(n).and_then(|n| n.attr("contenteditable")) {
                    return value != "false";
                }
                at = dom.parent(n);
            }
            false
        }
    }
}

/// A recursive-descent reader of selectors, character by character.
struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    text: &'a str,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().collect(),
            at: 0,
            text,
        }
    }

    fn error(&self) -> SelectorError {
        SelectorError(format!("'{}' is not a valid selector", self.text))
    }

    fn done(&self) -> bool {
        self.at >= self.chars.len()
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn skip_space(&mut self) -> bool {
        let start = self.at;
        while self.peek().is_some_and(char::is_whitespace) {
            self.at += 1;
        }
        // Comments count as space.
        if self.peek() == Some('/') && self.peek_at(1) == Some('*') {
            self.at += 2;
            while !self.done() && !(self.peek() == Some('*') && self.peek_at(1) == Some('/')) {
                self.at += 1;
            }
            self.at = (self.at + 2).min(self.chars.len());
            self.skip_space();
        }
        self.at > start
    }

    fn list(&mut self, relative: bool) -> Result<SelectorList, SelectorError> {
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

    fn complex(&mut self, relative: bool) -> Result<Selector, SelectorError> {
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

    fn ident(&mut self) -> Option<String> {
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
                    out.tag = Some(self.ident().ok_or_else(|| self.error())?.to_ascii_lowercase());
                }
            }
        } else if self.peek().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '\\' || !c.is_ascii())
        {
            out.tag = Some(self.ident().ok_or_else(|| self.error())?.to_ascii_lowercase());
        }
        loop {
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
                        self.ident().ok_or_else(|| self.error())?;
                        if self.peek() == Some('(') {
                            self.skip_arguments()?;
                        }
                        out.element = true;
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

    fn skip_arguments(&mut self) -> Result<(), SelectorError> {
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

    fn close(&mut self) -> Result<(), SelectorError> {
        self.skip_space();
        if self.peek() != Some(')') {
            return Err(self.error());
        }
        self.at += 1;
        Ok(())
    }

    fn pseudo(&mut self) -> Result<Pseudo, SelectorError> {
        let name = self.ident().ok_or_else(|| self.error())?.to_ascii_lowercase();
        if self.peek() == Some('(') {
            self.at += 1;
            self.skip_space();
            let pseudo = match name.as_str() {
                "not" => Pseudo::Not(self.list(false)?),
                "is" | "matches" | "-webkit-any" | "any" => Pseudo::Is(self.list(false)?),
                "where" => Pseudo::Where(self.list(false)?),
                "has" => Pseudo::Has(self.list(true)?),
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
                "dir" => Pseudo::Dir(self.ident().ok_or_else(|| self.error())?.to_ascii_lowercase()),
                "host" | "host-context" | "state" => {
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
                        pseudo: vec![structural(0, 1, false, false), structural(0, 1, true, false)],
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
            | "playing" | "paused" | "host" | "picture-in-picture" | "open" => Pseudo::Never,
            "valid" | "user-valid" | "default" | "in-range" | "local-link" => Pseudo::Defined,
            // The pseudo-elements of CSS 2, written with one colon.
            "before" | "after" | "first-line" | "first-letter" => {
                return Ok(Pseudo::Never);
            }
            other if other.starts_with("-webkit-") || other.starts_with("-moz-") || other.starts_with("-ms-") => {
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
                if (c == 'o' || c == 'O') && text.trim().len() > 0 {
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
            Some((a, b)) => parse(a).zip(if b.is_empty() { Some(0) } else { b.parse::<i32>().ok() }),
            None => text.parse::<i32>().ok().map(|b| (0, b)),
        };
        parsed.ok_or_else(|| self.error())
    }
}

// ─── Stylesheets ────────────────────────────────────────────────────────────────────────────

/// One declaration of a rule or an inline style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// `color: red; padding: 4px !important` as declarations, `;` inside parentheses and quotes
/// (`url(data:…;base64,…)`, `content: ";"`) left alone.
pub fn declarations(text: &str) -> Vec<Declaration> {
    split_outside(text, ';')
        .into_iter()
        .filter_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            let name = name.trim();
            let name = if name.starts_with("--") {
                name.to_owned()
            } else {
                name.to_ascii_lowercase()
            };
            let mut value = value.trim();
            let mut important = false;
            if let Some(at) = value.to_ascii_lowercase().rfind("!important")
                && value[at + "!important".len()..].trim().is_empty()
            {
                important = true;
                value = value[..at].trim();
            }
            (!name.is_empty() && (!value.is_empty() || name.starts_with("--"))).then(|| {
                Declaration {
                    name,
                    value: value.to_owned(),
                    important,
                }
            })
        })
        .collect()
}

fn split_outside(text: &str, separator: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut escaped = false;
    for c in text.chars() {
        if escaped {
            current.push(c);
            escaped = false;
            continue;
        }
        match (quote, c) {
            (_, '\\') => {
                escaped = true;
                current.push(c);
                continue;
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth -= 1,
            (None, c) if c == separator && depth <= 0 => {
                out.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// A media query, as `@media` and `matchMedia` give it.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaQuery(String);

/// What media queries are asked of: the viewport and what the window prefers.
#[derive(Clone, Debug, PartialEq)]
pub struct Environment {
    pub width: f32,
    pub height: f32,
    pub device_pixel_ratio: f32,
    pub dark: bool,
    pub reduced_motion: bool,
    /// `print` rather than `screen`.
    pub print: bool,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            width: 1280.,
            height: 800.,
            device_pixel_ratio: 1.,
            dark: false,
            reduced_motion: false,
            print: false,
        }
    }
}

impl MediaQuery {
    pub fn new(text: &str) -> Self {
        Self(text.trim().to_owned())
    }

    pub fn text(&self) -> &str {
        &self.0
    }

    /// Whether the query holds in `env`. A list holds when any of its queries does; a feature
    /// this engine does not know holds, as a window that draws what it is given would say.
    pub fn matches(&self, env: &Environment) -> bool {
        if self.0.is_empty() {
            return true;
        }
        split_outside(&self.0, ',')
            .iter()
            .any(|query| query_matches(query.trim(), env))
    }
}

fn query_matches(query: &str, env: &Environment) -> bool {
    let lower = query.to_ascii_lowercase();
    let (negated, rest) = match lower.strip_prefix("not ") {
        Some(rest) => (true, rest.trim().to_owned()),
        None => (false, lower.strip_prefix("only ").unwrap_or(&lower).trim().to_owned()),
    };
    let mut holds = true;
    for term in split_terms(&rest) {
        let term = term.trim();
        let ok = if let Some(inner) = term.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            feature_matches(inner.trim(), env)
        } else {
            match term {
                "all" | "" => true,
                "screen" => !env.print,
                "print" => env.print,
                _ => false,
            }
        };
        holds &= ok;
    }
    holds != negated
}

/// `screen and (min-width: 1px) and (hover)` as its terms.
fn split_terms(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 && text[i..].starts_with(" and ") => {
                out.push(text[start..i].to_owned());
                i += 5;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(text[start..].to_owned());
    out
}

fn feature_matches(feature: &str, env: &Environment) -> bool {
    // Ranges: `width >= 600px`, `400px < width < 800px`.
    for op in [">=", "<=", ">", "<", "="] {
        if feature.contains(op) && !feature.contains(':') {
            let parts: Vec<&str> = feature.split(op).map(str::trim).collect();
            if parts.len() == 2 {
                let (name, value, flipped) = if let Some(v) = media_length(parts[1]) {
                    (parts[0], v, false)
                } else if let Some(v) = media_length(parts[0]) {
                    (parts[1], v, true)
                } else {
                    return true;
                };
                let have = match name {
                    "width" => env.width,
                    "height" => env.height,
                    _ => return true,
                };
                let (a, b) = if flipped { (value, have) } else { (have, value) };
                return match op {
                    ">=" => a >= b,
                    "<=" => a <= b,
                    ">" => a > b,
                    "<" => a < b,
                    _ => a == b,
                };
            }
        }
    }
    let (name, value) = match feature.split_once(':') {
        Some((name, value)) => (name.trim(), Some(value.trim())),
        None => (feature.trim(), None),
    };
    let width = |v: Option<&str>| v.and_then(media_length);
    match (name, value) {
        ("min-width", v) => width(v).is_none_or(|w| env.width >= w),
        ("max-width", v) => width(v).is_none_or(|w| env.width <= w),
        ("width", v) => width(v).is_none_or(|w| env.width == w),
        ("min-height", v) => width(v).is_none_or(|h| env.height >= h),
        ("max-height", v) => width(v).is_none_or(|h| env.height <= h),
        ("height", v) => width(v).is_none_or(|h| env.height == h),
        ("prefers-color-scheme", Some(v)) => (v == "dark") == env.dark,
        ("prefers-reduced-motion", Some(v)) => (v == "reduce") == env.reduced_motion,
        ("prefers-reduced-motion", None) => env.reduced_motion,
        ("prefers-contrast", Some(v)) => v == "no-preference",
        ("forced-colors", Some(v)) => v == "none",
        ("forced-colors", None) => false,
        ("hover" | "any-hover", Some(v)) => v == "hover",
        ("hover" | "any-hover", None) => true,
        ("pointer" | "any-pointer", Some(v)) => v == "fine",
        ("pointer" | "any-pointer", None) => true,
        ("orientation", Some(v)) => (v == "landscape") == (env.width >= env.height),
        ("min-resolution" | "-webkit-min-device-pixel-ratio", Some(v)) => {
            resolution(v).is_none_or(|r| env.device_pixel_ratio >= r)
        }
        ("max-resolution" | "-webkit-max-device-pixel-ratio", Some(v)) => {
            resolution(v).is_none_or(|r| env.device_pixel_ratio <= r)
        }
        ("display-mode", Some(v)) => v == "browser",
        ("scripting", Some(v)) => v == "enabled",
        _ => true,
    }
}

fn media_length(text: &str) -> Option<f32> {
    length(text, 16.)
}

fn resolution(text: &str) -> Option<f32> {
    let text = text.trim();
    text.strip_suffix("dppx")
        .or_else(|| text.strip_suffix('x'))
        .and_then(|n| n.trim().parse().ok())
        .or_else(|| text.strip_suffix("dpi").and_then(|n| n.trim().parse::<f32>().ok()).map(|d| d / 96.))
        .or_else(|| text.parse().ok())
}

#[derive(Clone, Debug)]
struct Rule {
    selectors: SelectorList,
    declarations: Vec<Declaration>,
    media: Vec<MediaQuery>,
    /// Whether a selector of it says `:hover` (or `:active`): the nodes it would change are
    /// the ones a drawer follows the pointer on.
    pointer: bool,
}

/// One stylesheet: a `<style>`'s text, an adopted sheet, the embedder's own.
#[derive(Clone, Debug, Default)]
pub struct Sheet {
    rules: Vec<Rule>,
    /// The text it was read from, comments and all: what the CSSOM shows.
    source: String,
}

impl Sheet {
    pub fn parse(text: &str) -> Self {
        let source = text.to_owned();
        let text = strip_comments(text);
        let mut rules = Vec::new();
        parse_block(&text, &[], &mut rules);
        Self { rules, source }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// The top-level rules of a stylesheet as their own texts: what a CSSOM's `cssRules` lists.
pub fn split_rules(css: &str) -> Vec<String> {
    let text = strip_comments(css);
    let mut out = Vec::new();
    let mut rest = text.as_str();
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        // A statement at-rule: `@import …;`, `@charset …;`.
        if trimmed.starts_with('@') {
            let semi = trimmed.find(';');
            let brace = trimmed.find('{');
            if let Some(semi) = semi
                && brace.is_none_or(|b| semi < b)
            {
                out.push(trimmed[..=semi].trim().to_owned());
                rest = &trimmed[semi + 1..];
                continue;
            }
        }
        let Some(open) = trimmed.find('{') else { break };
        let close = matching_brace(trimmed, open).unwrap_or(trimmed.len() - 1);
        out.push(trimmed[..=close.min(trimmed.len() - 1)].trim().to_owned());
        rest = trimmed.get(close + 1..).unwrap_or("");
    }
    out
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = match rest[start + 2..].find("*/") {
            Some(end) => &rest[start + 2 + end + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (offset, c) in text[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{') => depth += 1,
            (None, '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_block(text: &str, media: &[MediaQuery], rules: &mut Vec<Rule>) {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        // Statement at-rules end at their `;`.
        if trimmed.starts_with('@') {
            let semi = trimmed.find(';');
            let brace = trimmed.find('{');
            if let Some(semi) = semi
                && brace.is_none_or(|b| semi < b)
            {
                rest = &trimmed[semi + 1..];
                continue;
            }
        }
        let Some(open) = trimmed.find('{') else { break };
        let prelude = trimmed[..open].trim();
        let close = matching_brace(trimmed, open).unwrap_or(trimmed.len());
        let body = &trimmed[open + 1..close.min(trimmed.len())];
        if let Some(at_rule) = prelude.strip_prefix('@') {
            let (name, condition) = at_rule
                .split_once(|c: char| c.is_whitespace() || c == '(')
                .map_or((at_rule, ""), |(n, _)| (n, &at_rule[n.len()..]));
            match name.to_ascii_lowercase().as_str() {
                "media" => {
                    let mut inner = media.to_vec();
                    inner.push(MediaQuery::new(condition));
                    parse_block(body, &inner, rules);
                }
                // What it supports is what it was given; layers are read in order.
                "supports" | "layer" | "document" | "-moz-document" | "scope" | "starting-style" => {
                    parse_block(body, media, rules)
                }
                _ => {}
            }
        } else if let Ok(selectors) = SelectorList::parse(prelude) {
            let pointer = selectors.says(&|p| matches!(p, Pseudo::Hover | Pseudo::Active));
            rules.push(Rule {
                pointer,
                selectors,
                declarations: declarations(body),
                media: media.to_vec(),
            });
        }
        rest = trimmed.get(close + 1..).unwrap_or("");
    }
}

// ─── The cascade ────────────────────────────────────────────────────────────────────────────

/// Where a sheet comes from, which decides first between two declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// The browser's defaults: `div { display: block }`.
    UserAgent,
    /// The embedder's and the document's.
    Author,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum RuleKey {
    Id(String),
    Class(String),
    Tag(String),
    Any,
}

/// The rules of every sheet, filed by what their subjects must be, so a node is matched only
/// against the rules that could apply to it.
#[derive(Clone, Default)]
pub struct Styler {
    rules: Vec<(Origin, usize, Rule)>,
    buckets: HashMap<RuleKey, Vec<(usize, usize)>>,
    env: Environment,
}

impl Styler {
    /// The cascade of `sheets`, in order, each with where it comes from, for `env`.
    pub fn new(sheets: &[(Origin, &Sheet)], env: &Environment) -> Self {
        let mut out = Styler {
            env: env.clone(),
            ..Styler::default()
        };
        let mut order = 0;
        for (origin, sheet) in sheets {
            for rule in &sheet.rules {
                if !rule.media.iter().all(|m| m.matches(env)) {
                    continue;
                }
                order += 1;
                let index = out.rules.len();
                for (k, selector) in rule.selectors.0.iter().enumerate() {
                    out.buckets
                        .entry(selector.key())
                        .or_default()
                        .push((index, k));
                }
                out.rules.push((*origin, order, rule.clone()));
            }
        }
        out
    }

    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// The rules that could apply to `id`, each with the selector of it to try.
    fn candidates(&self, dom: &Dom, id: NodeId) -> Vec<(usize, usize)> {
        let Some(node) = dom.get(id) else {
            return Vec::new();
        };
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut add = |key: RuleKey| {
            if let Some(list) = self.buckets.get(&key) {
                out.extend(list.iter().copied());
            }
        };
        add(RuleKey::Any);
        if let Some(tag) = node.tag() {
            add(RuleKey::Tag(tag.to_owned()));
        }
        if let Some(id) = node.attr("id") {
            add(RuleKey::Id(id.to_owned()));
        }
        for class in &node.classes {
            add(RuleKey::Class(class.clone()));
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// The properties a child takes from its parent unless something says otherwise.
const INHERITED: &[&str] = &[
    "color",
    "font",
    "font-family",
    "font-size",
    "font-style",
    "font-variant",
    "font-weight",
    "font-stretch",
    "font-feature-settings",
    "font-variation-settings",
    "line-height",
    "letter-spacing",
    "word-spacing",
    "text-align",
    "text-indent",
    "text-transform",
    "text-shadow",
    "text-rendering",
    "white-space",
    "word-break",
    "word-wrap",
    "overflow-wrap",
    "hyphens",
    "tab-size",
    "direction",
    "writing-mode",
    "visibility",
    "cursor",
    "caret-color",
    "list-style",
    "list-style-type",
    "list-style-position",
    "list-style-image",
    "quotes",
    "pointer-events",
    "border-collapse",
    "border-spacing",
    "empty-cells",
    "caption-side",
    "color-scheme",
    "-webkit-font-smoothing",
    "-moz-osx-font-smoothing",
    "user-select",
    "-webkit-user-select",
];

/// Whether `property` is inherited.
pub fn is_inherited(property: &str) -> bool {
    property.starts_with("--") || INHERITED.contains(&property)
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
            let root = dom.parent(*id).unwrap_or(*id);
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
                    .any(|other| other != root && dom.contains(*other, *root))
            })
            .collect();
        let mut count = 0;
        for root in outer {
            let inherited = dom
                .parent(root)
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
        while let Some((id, inherited)) = stack.pop() {
            let (style, pointer) = own_style(dom, styler, id, &inherited);
            for child in dom.children(id) {
                stack.push((*child, style.clone()));
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
    let mut at = dom.parent(id);
    while let Some(node) = at {
        chain.push(node);
        at = dom.parent(node);
    }
    let mut style = Style::new();
    for node in chain.into_iter().rev() {
        style = own_style(dom, styler, node, &style).0;
    }
    style
}

fn own_style(dom: &Dom, styler: &Styler, id: NodeId, inherited: &Style) -> (Style, bool) {
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
    // Sorted by: importance (and, within it, origin), specificity, order.
    let mut matched: Vec<((u8, (u32, u32, u32), usize), &Declaration)> = Vec::new();
    let mut pointer = false;
    let cx = MatchContext::default();
    for (index, k) in styler.candidates(dom, id) {
        let (origin, order, rule) = &styler.rules[index];
        let selector = &rule.selectors.0[k];
        if selector.matches_with(dom, id, &cx) {
            let specificity = selector.specificity();
            for declaration in &rule.declarations {
                let rank = match (declaration.important, origin) {
                    (false, Origin::UserAgent) => 0,
                    (false, Origin::Author) => 1,
                    (true, Origin::Author) => 3,
                    (true, Origin::UserAgent) => 4,
                };
                matched.push(((rank, specificity, *order), declaration));
            }
        } else if rule.pointer && !pointer {
            // Would the pointer on it make this rule apply?
            let probe = MatchContext {
                scope: None,
                hover_probe: Some(id),
            };
            pointer = selector.matches_with(dom, id, &probe);
        }
    }
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
        let rank = if declaration.important { 3 } else { 2 };
        matched.push(((rank, (u32::MAX, 0, 0), usize::MAX), declaration));
    }
    matched.sort_by(|a, b| a.0.cmp(&b.0));
    // `hidden` is the user agent's `display: none`, under any author rule.
    if node.attr("hidden").is_some() && node.namespace == Namespace::Html {
        style.insert("display".into(), "none".into());
    }
    for (_, declaration) in matched {
        set_declared(&mut style, &declaration.name, &declaration.value, inherited);
    }
    // `var()` resolved against what this node holds, custom properties included.
    let custom: HashMap<String, String> = style
        .iter()
        .filter(|(k, _)| k.starts_with("--"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for value in style.values_mut() {
        if value.contains("var(") {
            *value = resolve_vars(value, &custom, 0);
        }
    }
    (style, pointer)
}

/// Writes a declaration, opening up the shorthands this engine reads as their longhands, and
/// the keywords every property takes.
fn set_declared(style: &mut Style, key: &str, value: &str, inherited: &Style) {
    match value.trim().to_ascii_lowercase().as_str() {
        "inherit" => {
            match inherited.get(key) {
                Some(parent) => style.insert(key.to_owned(), parent.clone()),
                None => style.remove(key),
            };
            return;
        }
        "initial" | "revert" | "revert-layer" => {
            style.remove(key);
            return;
        }
        "unset" => {
            if is_inherited(key) {
                match inherited.get(key) {
                    Some(parent) => style.insert(key.to_owned(), parent.clone()),
                    None => style.remove(key),
                };
            } else {
                style.remove(key);
            }
            return;
        }
        _ => {}
    }
    let sides = ["top", "right", "bottom", "left"];
    match key {
        "padding" | "margin" | "inset" | "scroll-margin" | "scroll-padding" => {
            for (side, v) in sides.iter().zip(box_sides(value)) {
                let name = if key == "inset" {
                    (*side).to_owned()
                } else {
                    format!("{key}-{side}")
                };
                style.insert(name, v);
            }
            style.insert(key.to_owned(), value.to_owned());
        }
        "border-width" | "border-style" | "border-color" => {
            let part = key.trim_start_matches("border-");
            for (side, v) in sides.iter().zip(box_sides(value)) {
                style.insert(format!("border-{side}-{part}"), v);
            }
            style.insert(key.to_owned(), value.to_owned());
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            let (width, line, colour) = border_parts(value);
            let targets: Vec<String> = match key {
                "border" => sides.iter().map(|s| format!("border-{s}")).collect(),
                other => vec![other.to_owned()],
            };
            for target in &targets {
                style.insert(format!("{target}-width"), width.clone());
                style.insert(format!("{target}-style"), line.clone());
                style.insert(format!("{target}-color"), colour.clone());
                style.insert(target.clone(), value.to_owned());
            }
            if key == "border" {
                style.insert("border-width".into(), width);
                style.insert("border-style".into(), line);
                style.insert("border-color".into(), colour);
                style.insert("border".into(), value.to_owned());
            }
        }
        "border-radius" => {
            let first = value.split('/').next().unwrap_or(value);
            let corners = ["top-left", "top-right", "bottom-right", "bottom-left"];
            for (corner, v) in corners.iter().zip(box_sides(first)) {
                style.insert(format!("border-{corner}-radius"), v);
            }
            style.insert(key.to_owned(), value.to_owned());
        }
        "background" => {
            // The colour is what a drawer reads; the rest is kept whole.
            let colour = split_outside(value, ' ')
                .into_iter()
                .map(|p| p.trim().to_owned())
                .rfind(|p| color(p).is_some() || p.starts_with("var(") || p == "currentcolor")
                .unwrap_or_else(|| "transparent".to_owned());
            style.insert("background-color".into(), colour);
            if value.contains("url(") || value.contains("gradient(") {
                style.insert("background-image".into(), value.to_owned());
            }
            style.insert("background".into(), value.to_owned());
        }
        "gap" | "grid-gap" => {
            let mut parts = value.split_whitespace();
            let row = parts.next().unwrap_or("0").to_owned();
            let column = parts.next().map_or_else(|| row.clone(), str::to_owned);
            style.insert("row-gap".into(), row);
            style.insert("column-gap".into(), column);
            style.insert("gap".into(), value.to_owned());
        }
        "flex" => {
            let (grow, shrink, basis) = flex_parts(value);
            style.insert("flex-grow".into(), grow);
            style.insert("flex-shrink".into(), shrink);
            style.insert("flex-basis".into(), basis);
            style.insert("flex".into(), value.to_owned());
        }
        "flex-flow" => {
            for part in value.split_whitespace() {
                if part.contains("wrap") {
                    style.insert("flex-wrap".into(), part.to_owned());
                } else {
                    style.insert("flex-direction".into(), part.to_owned());
                }
            }
            style.insert("flex-flow".into(), value.to_owned());
        }
        "overflow" => {
            let mut parts = value.split_whitespace();
            let x = parts.next().unwrap_or("visible").to_owned();
            let y = parts.next().map_or_else(|| x.clone(), str::to_owned);
            style.insert("overflow-x".into(), x);
            style.insert("overflow-y".into(), y);
            style.insert("overflow".into(), value.to_owned());
        }
        "place-items" | "place-content" | "place-self" => {
            let part = key.trim_start_matches("place-");
            let mut parts = value.split_whitespace();
            let align = parts.next().unwrap_or("normal").to_owned();
            let justify = parts.next().map_or_else(|| align.clone(), str::to_owned);
            style.insert(format!("align-{part}"), align);
            style.insert(format!("justify-{part}"), justify);
            style.insert(key.to_owned(), value.to_owned());
        }
        _ => {
            style.insert(key.to_owned(), value.to_owned());
        }
    }
}

fn border_parts(value: &str) -> (String, String, String) {
    let mut width = "medium".to_owned();
    let mut line = "none".to_owned();
    let mut colour = "currentcolor".to_owned();
    for part in split_outside(value, ' ') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if length(part, 16.).is_some() || matches!(part, "thin" | "medium" | "thick") {
            width = part.to_owned();
        } else if matches!(
            part,
            "none" | "hidden" | "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset"
        ) {
            line = part.to_owned();
        } else {
            colour = part.to_owned();
        }
    }
    if line == "none" {
        width = "0".to_owned();
    }
    (width, line, colour)
}

fn flex_parts(value: &str) -> (String, String, String) {
    let parts: Vec<&str> = value.split_whitespace().collect();
    match parts.as_slice() {
        ["none"] => ("0".into(), "0".into(), "auto".into()),
        ["auto"] => ("1".into(), "1".into(), "auto".into()),
        [one] if one.parse::<f32>().is_ok() => ((*one).into(), "1".into(), "0%".into()),
        [one] => ("1".into(), "1".into(), (*one).into()),
        [a, b] if b.parse::<f32>().is_ok() => ((*a).into(), (*b).into(), "0%".into()),
        [a, b] => ((*a).into(), "1".into(), (*b).into()),
        [a, b, c, ..] => ((*a).into(), (*b).into(), (*c).into()),
        [] => ("0".into(), "1".into(), "auto".into()),
    }
}

fn box_sides(value: &str) -> [String; 4] {
    let parts: Vec<String> = split_outside(value, ' ')
        .into_iter()
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    let pick = |i: usize| parts.get(i).cloned().unwrap_or_else(|| "0".to_owned());
    match parts.len() {
        1 => [pick(0), pick(0), pick(0), pick(0)],
        2 => [pick(0), pick(1), pick(0), pick(1)],
        3 => [pick(0), pick(1), pick(2), pick(1)],
        _ => [pick(0), pick(1), pick(2), pick(3)],
    }
}

fn resolve_vars(value: &str, custom: &HashMap<String, String>, depth: u32) -> String {
    if depth > 16 {
        return value.to_owned();
    }
    let Some(start) = value.find("var(") else {
        return value.to_owned();
    };
    let inner_start = start + 4;
    let mut paren = 1;
    let mut end = value.len();
    for (offset, c) in value[inner_start..].char_indices() {
        match c {
            '(' => paren += 1,
            ')' => {
                paren -= 1;
                if paren == 0 {
                    end = inner_start + offset;
                    break;
                }
            }
            _ => {}
        }
    }
    let inner = &value[inner_start..end];
    let (name, fallback) = match inner.split_once(',') {
        Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
        None => (inner.trim(), None),
    };
    let replaced = custom
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.trim().is_empty())
        .or(fallback)
        .unwrap_or("");
    let replaced = resolve_vars(replaced, custom, depth + 1);
    let rest = value.get(end + 1..).unwrap_or("");
    resolve_vars(
        &format!("{}{}{}", &value[..start], replaced, rest),
        custom,
        depth + 1,
    )
}

/// What a property is when nothing set it, as `getComputedStyle` answers it.
pub fn initial_value(property: &str, tag: Option<&str>) -> Option<&'static str> {
    Some(match property {
        "display" => match tag {
            Some(tag) if BLOCK_TAGS.contains(&tag) => "block",
            Some("li") => "list-item",
            Some("table") => "table",
            Some("tr") => "table-row",
            Some("td" | "th") => "table-cell",
            Some("thead") => "table-header-group",
            Some("tbody") => "table-row-group",
            Some("tfoot") => "table-footer-group",
            Some("head" | "script" | "style" | "template" | "title" | "meta" | "link") => "none",
            _ => "inline",
        },
        "position" => "static",
        "visibility" => "visible",
        "opacity" => "1",
        "color" => "rgb(0, 0, 0)",
        "background-color" => "rgba(0, 0, 0, 0)",
        "font-size" => "16px",
        "font-weight" => "400",
        "font-style" => "normal",
        "line-height" => "normal",
        "overflow" | "overflow-x" | "overflow-y" => "visible",
        "float" => "none",
        "z-index" | "width" | "height" | "top" | "left" | "right" | "bottom" => "auto",
        "box-sizing" => "content-box",
        "pointer-events" | "cursor" => "auto",
        "white-space" => "normal",
        "text-align" => "start",
        "direction" => "ltr",
        "transform" | "max-width" | "max-height" | "border-style" => "none",
        "min-width" | "min-height" => "0px",
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" | "padding-top"
        | "padding-right" | "padding-bottom" | "padding-left" | "border-top-width"
        | "border-right-width" | "border-bottom-width" | "border-left-width" => "0px",
        "flex-direction" => "row",
        "flex-wrap" => "nowrap",
        "flex-grow" => "0",
        "flex-shrink" => "1",
        "flex-basis" => "auto",
        "transition-duration" | "animation-duration" | "transition-delay" | "animation-delay" => "0s",
        "animation-name" => "none",
        _ => return None,
    })
}

const BLOCK_TAGS: &[&str] = &[
    "html", "body", "div", "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "dl", "dt", "dd",
    "pre", "blockquote", "section", "article", "aside", "header", "footer", "nav", "main", "form",
    "fieldset", "figure", "figcaption", "address", "details", "summary", "hr", "legend", "menu",
    "dialog", "search", "hgroup", "center", "optgroup",
];

/// The user agent's own sheet: what an element looks like before any author says otherwise —
/// enough of a browser's for a drawer to lay a page out by (`display`, the headings' sizes, the
/// lists' indents, what is never drawn).
pub const USER_AGENT_SHEET: &str = r#"
html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, dl, dt, dd, pre, blockquote, section, article,
aside, header, footer, nav, main, form, fieldset, figure, figcaption, address, details, summary,
hr, legend, menu, dialog, search, hgroup, center, optgroup { display: block }
head, script, style, template, title, meta, link, base, noscript, datalist, param, area { display: none }
[hidden] { display: none }
li { display: list-item }
table { display: table; border-collapse: separate; border-spacing: 2px }
caption { display: table-caption; text-align: center }
thead { display: table-header-group }
tbody { display: table-row-group }
tfoot { display: table-footer-group }
tr { display: table-row }
td, th { display: table-cell; padding: 1px }
th { font-weight: bold; text-align: center }
col { display: table-column }
colgroup { display: table-column-group }
input, select, textarea, button, img, video, canvas, iframe, svg, audio, progress, meter { display: inline-block }
body { margin: 8px }
p, blockquote, figure, dl, menu { margin-top: 1em; margin-bottom: 1em }
ul, ol { margin-top: 1em; margin-bottom: 1em; padding-left: 40px }
ul { list-style-type: disc }
ol { list-style-type: decimal }
blockquote, figure { margin-left: 40px; margin-right: 40px }
dd { margin-left: 40px }
h1 { font-size: 2em; margin-top: 0.67em; margin-bottom: 0.67em; font-weight: bold }
h2 { font-size: 1.5em; margin-top: 0.83em; margin-bottom: 0.83em; font-weight: bold }
h3 { font-size: 1.17em; margin-top: 1em; margin-bottom: 1em; font-weight: bold }
h4 { margin-top: 1.33em; margin-bottom: 1.33em; font-weight: bold }
h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em; font-weight: bold }
h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em; font-weight: bold }
b, strong { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small { font-size: smaller }
big { font-size: larger }
sub { vertical-align: sub; font-size: smaller }
sup { vertical-align: super; font-size: smaller }
code, kbd, samp, tt, pre { font-family: monospace }
pre { white-space: pre; margin-top: 1em; margin-bottom: 1em }
textarea { white-space: pre-wrap }
a:any-link { color: #0000ee; text-decoration: underline; cursor: pointer }
mark { background-color: yellow; color: black }
hr { border: 1px inset; margin-top: 0.5em; margin-bottom: 0.5em }
button { cursor: default }
center { text-align: center }
"#;

/// A length in pixels: `12px`, `0.5em` against `font_size`, `1rem` against 16, `pt`, a bare
/// number or `0`.
pub fn length(text: &str, font_size: f32) -> Option<f32> {
    let text = text.trim();
    if text == "0" {
        return Some(0.);
    }
    let number = |suffix: &str| text.strip_suffix(suffix)?.trim().parse::<f32>().ok();
    number("px")
        .or_else(|| number("rem").map(|n| n * 16.))
        .or_else(|| number("em").map(|n| n * font_size))
        .or_else(|| number("pt").map(|n| n * 4. / 3.))
        .or_else(|| text.parse::<f32>().ok())
}

/// A colour as red, green, blue and alpha in `0..=1`: hex, `rgb()`/`rgba()`, `hsl()`/`hsla()`
/// and the named colours.
pub fn color(text: &str) -> Option<[f32; 4]> {
    let text = text.trim().to_ascii_lowercase();
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let pair = |a: u8, b: u8| f32::from(a * 16 + b) / 255.;
        let single = |a: u8| f32::from(a * 17) / 255.;
        return match digits.len() {
            3 => Some([single(digits[0]), single(digits[1]), single(digits[2]), 1.]),
            4 => Some([
                single(digits[0]),
                single(digits[1]),
                single(digits[2]),
                single(digits[3]),
            ]),
            6 => Some([
                pair(digits[0], digits[1]),
                pair(digits[2], digits[3]),
                pair(digits[4], digits[5]),
                1.,
            ]),
            8 => Some([
                pair(digits[0], digits[1]),
                pair(digits[2], digits[3]),
                pair(digits[4], digits[5]),
                pair(digits[6], digits[7]),
            ]),
            _ => None,
        };
    }
    let function = |prefixes: &[&str]| -> Option<Vec<(f32, bool)>> {
        let args = prefixes
            .iter()
            .find_map(|p| text.strip_prefix(p))
            .and_then(|rest| rest.strip_suffix(')'))?;
        Some(
            args.split([',', ' ', '/'])
                .filter(|p| !p.is_empty())
                .filter_map(|p| match p.strip_suffix('%') {
                    Some(percent) => percent.parse::<f32>().ok().map(|v| (v, true)),
                    None => p.trim_end_matches("deg").parse::<f32>().ok().map(|v| (v, false)),
                })
                .collect(),
        )
    };
    if let Some(parts) = function(&["rgba(", "rgb("]) {
        if parts.len() < 3 {
            return None;
        }
        let channel = |(v, percent): (f32, bool)| if percent { v / 100. } else { v / 255. };
        let alpha = parts
            .get(3)
            .map_or(1., |(v, percent)| if *percent { v / 100. } else { *v });
        return Some([channel(parts[0]), channel(parts[1]), channel(parts[2]), alpha]);
    }
    if let Some(parts) = function(&["hsla(", "hsl("]) {
        if parts.len() < 3 {
            return None;
        }
        let h = parts[0].0.rem_euclid(360.) / 360.;
        let s = parts[1].0 / 100.;
        let l = parts[2].0 / 100.;
        let alpha = parts
            .get(3)
            .map_or(1., |(v, percent)| if *percent { v / 100. } else { *v });
        let q = if l < 0.5 { l * (1. + s) } else { l + s - l * s };
        let p = 2. * l - q;
        let hue = |mut t: f32| {
            if t < 0. {
                t += 1.;
            }
            if t > 1. {
                t -= 1.;
            }
            if t < 1. / 6. {
                p + (q - p) * 6. * t
            } else if t < 0.5 {
                q
            } else if t < 2. / 3. {
                p + (q - p) * (2. / 3. - t) * 6.
            } else {
                p
            }
        };
        return Some([hue(h + 1. / 3.), hue(h), hue(h - 1. / 3.), alpha]);
    }
    if text == "transparent" {
        return Some([0., 0., 0., 0.]);
    }
    let hex = NAMED_COLORS
        .iter()
        .find(|(name, _)| *name == text)
        .map(|(_, hex)| *hex)?;
    color(&format!("#{hex:06x}"))
}

/// CSS's named colours.
const NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff), ("antiquewhite", 0xfaebd7), ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4), ("azure", 0xf0ffff), ("beige", 0xf5f5dc), ("bisque", 0xffe4c4),
    ("black", 0x000000), ("blanchedalmond", 0xffebcd), ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2), ("brown", 0xa52a2a), ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0), ("chartreuse", 0x7fff00), ("chocolate", 0xd2691e),
    ("coral", 0xff7f50), ("cornflowerblue", 0x6495ed), ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c), ("cyan", 0x00ffff), ("darkblue", 0x00008b), ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b), ("darkgray", 0xa9a9a9), ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9), ("darkkhaki", 0xbdb76b), ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f), ("darkorange", 0xff8c00), ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000), ("darksalmon", 0xe9967a), ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b), ("darkslategray", 0x2f4f4f), ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1), ("darkviolet", 0x9400d3), ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff), ("dimgray", 0x696969), ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff), ("firebrick", 0xb22222), ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22), ("fuchsia", 0xff00ff), ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff), ("gold", 0xffd700), ("goldenrod", 0xdaa520), ("gray", 0x808080),
    ("green", 0x008000), ("greenyellow", 0xadff2f), ("grey", 0x808080), ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4), ("indianred", 0xcd5c5c), ("indigo", 0x4b0082), ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c), ("lavender", 0xe6e6fa), ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00), ("lemonchiffon", 0xfffacd), ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080), ("lightcyan", 0xe0ffff), ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3), ("lightgreen", 0x90ee90), ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1), ("lightsalmon", 0xffa07a), ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa), ("lightslategray", 0x778899), ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de), ("lightyellow", 0xffffe0), ("lime", 0x00ff00),
    ("limegreen", 0x32cd32), ("linen", 0xfaf0e6), ("magenta", 0xff00ff), ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa), ("mediumblue", 0x0000cd), ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db), ("mediumseagreen", 0x3cb371), ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a), ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585), ("midnightblue", 0x191970), ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1), ("moccasin", 0xffe4b5), ("navajowhite", 0xffdead),
    ("navy", 0x000080), ("oldlace", 0xfdf5e6), ("olive", 0x808000), ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500), ("orangered", 0xff4500), ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa), ("palegreen", 0x98fb98), ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093), ("papayawhip", 0xffefd5), ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f), ("pink", 0xffc0cb), ("plum", 0xdda0dd), ("powderblue", 0xb0e0e6),
    ("purple", 0x800080), ("rebeccapurple", 0x663399), ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f), ("royalblue", 0x4169e1), ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072), ("sandybrown", 0xf4a460), ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee), ("sienna", 0xa0522d), ("silver", 0xc0c0c0), ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd), ("slategray", 0x708090), ("slategrey", 0x708090),
    ("snow", 0xfffafa), ("springgreen", 0x00ff7f), ("steelblue", 0x4682b4), ("tan", 0xd2b48c),
    ("teal", 0x008080), ("thistle", 0xd8bfd8), ("tomato", 0xff6347), ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee), ("wheat", 0xf5deb3), ("white", 0xffffff), ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00), ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn styler(sheet: &Sheet) -> Styler {
        Styler::new(&[(Origin::Author, sheet)], &Environment::default())
    }

    #[test]
    fn rules_cascade_by_specificity_order_and_inheritance() {
        let mut dom = Dom::new();
        let modal = dom.create_element("div");
        dom.set_attr(modal, "class", "modal");
        let title = dom.create_element("div");
        dom.set_attr(title, "class", "modal-title");
        dom.append(dom.body, modal);
        dom.append(modal, title);
        dom.set_style(dom.body, "--text-normal", "#112233");
        let sheet = Sheet::parse(
            "/* c */ .modal { color: var(--text-normal); padding: 4px 8px }
             div { color: red }
             .modal > .modal-title { font-weight: 600 }
             @media (max-width: 10px) { .modal-title { font-size: 20px } }
             @media (min-width: 10px) { .modal-title { font-style: italic } }
             .modal .missing { color: blue }
             .modal-title { color: green !important }
             #nope, .modal-title:not(.other) { text-align: center }",
        );
        let computed = Computed::cascade(&dom, &styler(&sheet));
        assert_eq!(computed.get(modal, "color"), Some("#112233"));
        assert_eq!(computed.get(modal, "padding-left"), Some("8px"));
        assert_eq!(computed.get(title, "color"), Some("green"));
        assert_eq!(computed.get(title, "font-weight"), Some("600"));
        assert_eq!(computed.get(title, "font-size"), None);
        assert_eq!(computed.get(title, "font-style"), Some("italic"));
        assert_eq!(computed.get(title, "text-align"), Some("center"));
        assert_eq!(color("#ff000080").unwrap()[3], 128. / 255.);
        assert_eq!(color("rebeccapurple").unwrap()[0], 0x66 as f32 / 255.);
        assert_eq!(length("1.5em", 10.), Some(15.));
    }

    #[test]
    fn selectors_query_the_tree() {
        let mut dom = Dom::new();
        let a = dom.create_element("div");
        dom.set_attr(a, "id", "x");
        let b = dom.create_element("input");
        dom.set_attr(b, "type", "text");
        let c = dom.create_element("p");
        dom.set_attr(c, "data-kind", "lead-in");
        let d = dom.create_element("p");
        dom.append(dom.body, a);
        dom.append(a, b);
        dom.append(a, c);
        dom.append(a, d);
        assert_eq!(dom.query(dom.document, "#x input[type=text]"), Some(b));
        assert_eq!(dom.query(dom.document, "body > div"), Some(a));
        assert_eq!(dom.query(dom.document, "html > input"), None);
        assert_eq!(dom.query_all(dom.document, "div, input").len(), 2);
        assert_eq!(dom.query(dom.document, "input + p"), Some(c));
        assert_eq!(dom.query_all(dom.document, "input ~ p").len(), 2);
        assert_eq!(dom.query(dom.document, "p:nth-child(3)"), Some(d));
        assert_eq!(dom.query(dom.document, "p:last-child"), Some(d));
        assert_eq!(dom.query(dom.document, "p:nth-of-type(1)"), Some(c));
        assert_eq!(dom.query(dom.document, "[data-kind|=lead]"), Some(c));
        assert_eq!(dom.query(dom.document, "[data-kind^='lead']"), Some(c));
        assert_eq!(dom.query(dom.document, "p:not([data-kind])"), Some(d));
        assert_eq!(dom.query(dom.document, "div:has(> input)"), Some(a));
        assert_eq!(dom.query(dom.document, ":is(span, p)"), Some(c));
        assert!(SelectorList::parse("div[").is_err());
        assert!(SelectorList::parse("p:nope").is_err());
        assert!(SelectorList::parse("a::before").is_ok());
        assert_eq!(dom.query(dom.document, ".a\\:b"), None);
    }

    #[test]
    fn media_queries_read_the_environment() {
        let env = Environment {
            width: 500.,
            dark: true,
            ..Environment::default()
        };
        assert!(MediaQuery::new("(max-width: 600px)").matches(&env));
        assert!(!MediaQuery::new("screen and (min-width: 600px)").matches(&env));
        assert!(MediaQuery::new("(prefers-color-scheme: dark)").matches(&env));
        assert!(MediaQuery::new("print, (width < 600px)").matches(&env));
        assert!(!MediaQuery::new("not all and (prefers-color-scheme: dark)").matches(&env));
    }
}
