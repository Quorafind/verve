//! Style scoping: a shadow tree's rules are its own, and four selectors reach across the
//! boundary — `:host` and `:host-context()` out to the element hosting the tree, `::slotted()`
//! out to what a slot shows, `::part()` in from the outside to an element a `part` attribute
//! names (CSS Scoping Module Level 1). Which tree a declaration comes from also decides the
//! cascade between it and one from another tree: [`Precedence`].

use std::collections::HashMap;

use super::cascade::{Precedence, Rank};
use super::selector::{
    Compound, MatchCache, MatchContext, Parser, Pseudo, Selector, SelectorError, SelectorList,
};
use super::sheet::Declaration;
use super::styler::Styler;
use crate::dom::{Dom, NodeId};

/// How far a compound's subject reaches beyond the tree its sheet is in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Reach {
    /// The tree the sheet is in.
    #[default]
    Own,
    /// The element hosting that tree (`:host`).
    Host,
    /// What a slot of that tree shows (`::slotted`).
    Slotted,
    /// An element of a shadow tree the host reaches into (`::part`).
    Part,
}

impl Reach {
    pub(super) fn of(pseudo: &[Pseudo]) -> Self {
        let has = |f: fn(&Pseudo) -> bool| pseudo.iter().any(f);
        if has(|p| matches!(p, Pseudo::Host(_) | Pseudo::HostContext(_))) {
            Self::Host
        } else if has(|p| matches!(p, Pseudo::Slotted(_))) {
            Self::Slotted
        } else if has(|p| matches!(p, Pseudo::Part(_))) {
            Self::Part
        } else {
            Self::Own
        }
    }
}

/// Which of `::slotted` and `::part`, if either, is set aside while a compound is asked of the
/// element on the other side of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Aside {
    None,
    Slotted,
    Part,
}

/// Whether `name` is among the names an element's `part` attribute gives.
fn is_part(dom: &Dom, id: NodeId, name: &str) -> bool {
    dom.get(id)
        .and_then(|node| node.attr("part"))
        .is_some_and(|parts| parts.split_whitespace().any(|part| part == name))
}

/// `:host-context(list)`: the host, or something it is in, matches `list`.
fn host_context_matches(
    dom: &Dom,
    host: NodeId,
    list: &SelectorList,
    matching: &MatchContext,
    cache: &mut MatchCache,
) -> bool {
    std::iter::successors(Some(host), |node| dom.parent(*node)).any(|node| {
        dom.get(node).is_some_and(|n| n.is_element())
            && list.matches_cached(dom, node, matching, cache)
    })
}

impl Pseudo {
    /// Whether the scoped pseudo-class or -element `self` holds for `id`: `:host(sel)` and
    /// `:host-context(sel)` of the host, the `::slotted` or `::part` set aside (`aside`) while
    /// the rest of its compound is asked.
    pub(super) fn matches_scoped(
        &self,
        dom: &Dom,
        id: NodeId,
        aside: Aside,
        matching: &MatchContext,
        cache: &mut MatchCache,
    ) -> bool {
        match self {
            Self::Slotted(_) => aside == Aside::Slotted,
            Self::Part(_) => aside == Aside::Part,
            Self::Host(arg) => arg
                .as_ref()
                .is_none_or(|arg| arg.matches_cached(dom, id, matching, cache)),
            Self::HostContext(list) => host_context_matches(dom, id, list, matching, cache),
            _ => false,
        }
    }
}

impl Selector {
    /// Where the part to the left of `part` is looked for: the shadow tree a `::part` reached
    /// into leaves through its host, and a slot's `::slotted` node through the slot.
    pub(super) fn step_out(&self, dom: &Dom, at: NodeId, part: usize) -> Option<NodeId> {
        match self.parts[part].0.reach {
            Reach::Slotted => dom.parent_element(dom.assigned_slot(at)?),
            Reach::Part => dom.shadow_host(dom.root_of(at)),
            Reach::Own | Reach::Host => dom.parent_element(at),
        }
    }
}

impl Compound {
    /// The compound asked of `id` when it is a `::slotted` or a `::part` one; `None` for any
    /// other. Those are about the element on the other side of the boundary: a slotted node's
    /// argument is asked of the node and the rest of the compound of its slot, and a part's
    /// name of the element and the rest of the compound of its host.
    pub(super) fn matches_scoped(
        &self,
        dom: &Dom,
        id: NodeId,
        matching: &MatchContext,
        cache: &mut MatchCache,
    ) -> Option<bool> {
        let pseudo = |wanted: fn(&Pseudo) -> bool| self.pseudo.iter().find(|p| wanted(p));
        match self.reach {
            Reach::Slotted => {
                let Some(Pseudo::Slotted(list)) = pseudo(|p| matches!(p, Pseudo::Slotted(_)))
                else {
                    return None;
                };
                let Some(slot) = dom.assigned_slot(id) else {
                    return Some(false);
                };
                Some(
                    list.matches_cached(dom, id, matching, cache)
                        && self.matches_own(dom, slot, matching, cache, Aside::Slotted),
                )
            }
            Reach::Part => {
                let Some(Pseudo::Part(name)) = pseudo(|p| matches!(p, Pseudo::Part(_))) else {
                    return None;
                };
                let Some(host) = dom.shadow_host(dom.root_of(id)) else {
                    return Some(false);
                };
                Some(
                    is_part(dom, id, name)
                        && self.matches_own(dom, host, matching, cache, Aside::Part),
                )
            }
            Reach::Own | Reach::Host => None,
        }
    }
}

/// Where the rules that reach across a tree boundary are, filed by the tree they were written
/// in, so an element is asked only the ones that can reach it.
#[derive(Clone, Default)]
pub(super) struct ScopeIndex {
    /// The `:host` rules of each shadow root's sheets, by the root.
    host_rules: HashMap<NodeId, Vec<(usize, usize)>>,
    /// The `::slotted` rules of each shadow root's sheets, by the root.
    slotted_rules: HashMap<NodeId, Vec<(usize, usize)>>,
    /// The `::part` rules, which reach into a shadow tree from outside.
    part_rules: Vec<(usize, usize)>,
}

impl ScopeIndex {
    /// Files selector `rule.1` of the rule `rule.0`, from a sheet in `tree`, by how far it reaches.
    pub(super) fn add(&mut self, tree: NodeId, rule: (usize, usize), reach: Reach) {
        match reach {
            Reach::Host => self.host_rules.entry(tree).or_default().push(rule),
            Reach::Slotted => self.slotted_rules.entry(tree).or_default().push(rule),
            Reach::Part => self.part_rules.push(rule),
            Reach::Own => {}
        }
    }

    /// The rules written in other trees that could apply to `id`: `::part` ones if it is in a
    /// shadow tree, the `:host` ones of the tree it hosts, the `::slotted` ones of the slot
    /// it is assigned to.
    pub(super) fn extra_candidates(&self, dom: &Dom, id: NodeId) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        if dom.shadow_host(dom.root_of(id)).is_some() {
            out.extend(self.part_rules.iter().copied());
        }
        let hosted = dom.shadow_root(id);
        let slotted = dom.assigned_slot(id).map(|slot| dom.root_of(slot));
        let by_root = |rules: &HashMap<NodeId, Vec<(usize, usize)>>, root: Option<NodeId>| {
            root.and_then(|root| rules.get(&root)).cloned()
        };
        out.extend(by_root(&self.host_rules, hosted).unwrap_or_default());
        out.extend(by_root(&self.slotted_rules, slotted).unwrap_or_default());
        out
    }
}

impl Parser<'_> {
    /// The argument of `:host(…)` or `:host-context(…)`, a compound selector kept as one
    /// selector.
    pub(super) fn host_pseudo(&mut self, name: &str) -> Result<Pseudo, SelectorError> {
        let list = SelectorList(vec![self.complex(false)?]);
        Ok(match name {
            "host" => Pseudo::Host(Some(list)),
            _ => Pseudo::HostContext(list),
        })
    }

    /// What follows `::` in a compound: `::part(name)` and `::slotted(selectors)` are kept, any
    /// other pseudo-element makes a compound that matches no element.
    pub(super) fn pseudo_element(&mut self, out: &mut Compound) -> Result<(), SelectorError> {
        let name = self
            .ident()
            .ok_or_else(|| self.error())?
            .to_ascii_lowercase();
        if self.peek() != Some('(') {
            out.element = true;
            return Ok(());
        }
        match name.as_str() {
            "part" => {
                self.at += 1;
                self.skip_space();
                let part = self.ident().ok_or_else(|| self.error())?;
                self.skip_space();
                self.close()?;
                out.pseudo.push(Pseudo::Part(part));
            }
            "slotted" => {
                self.at += 1;
                self.skip_space();
                let list = self.list(false)?;
                self.close()?;
                out.pseudo.push(Pseudo::Slotted(list));
            }
            _ => {
                self.skip_arguments()?;
                out.element = true;
            }
        }
        Ok(())
    }
}

/// The declarations of the rules that match `id`, each with its rank, and whether the pointer
/// being on it would make another rule match.
pub(super) fn matched_rules<'a>(
    dom: &Dom,
    styler: &'a Styler,
    id: NodeId,
    cache: &mut MatchCache,
) -> (Vec<(Rank, &'a Declaration)>, bool) {
    let mut matched = Vec::new();
    let mut pointer = false;
    let matching = MatchContext::default();
    let tree = dom.root_of(id);
    for (index, k) in styler.candidates(dom, id, tree) {
        let entry = &styler.rules[index];
        let selector = &entry.rule.selectors.0[k];
        if selector.matches_cached(dom, id, &matching, cache) {
            let specificity = selector.specificity();
            // The tree a declaration comes from decides between it and one from another tree.
            let inner = matches!(selector.subject().reach, Reach::Host | Reach::Slotted)
                && entry.tree != tree;
            for declaration in &entry.rule.declarations {
                let precedence = Precedence::of(declaration.important, entry.origin, inner);
                matched.push(((precedence, specificity, entry.order), declaration));
            }
        } else if entry.rule.pointer && !pointer {
            // Would the pointer on it make this rule apply?
            let probe = MatchContext {
                scope: None,
                hover_probe: Some(id),
            };
            pointer = selector.matches_with(dom, id, &probe);
        }
    }
    (matched, pointer)
}
