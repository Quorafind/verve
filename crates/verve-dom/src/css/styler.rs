//! The rules of every sheet, indexed so that an element is matched only against the rules that
//! could apply to it.

use std::collections::HashMap;
use std::sync::Arc;

use super::animate::Keyframes;
use super::cascade::{Origin, says_revert_layer};
use super::layer::LayerOrder;
use super::media::Environment;
use super::pseudo::PseudoElement;
use super::scope::{Reach, ScopeIndex};
use super::sheet::{Rule, Sheet};
use crate::dom::{Dom, NodeId};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum RuleKey {
    Id(String),
    Class(String),
    Tag(String),
    Any,
}

/// A rule of a sheet: where it comes from, its place in the order of all rules, and the tree
/// its sheet is in (the user agent's apply in every tree).
#[derive(Clone)]
pub(super) struct RuleEntry {
    pub(super) origin: Origin,
    pub(super) order: usize,
    pub(super) tree: NodeId,
    /// The place of the `@layer` the rule is in among those of all sheets; `LAYERLESS` when it
    /// is in none.
    pub(super) layer: u32,
    /// Whether a selector of the rule picks the node itself: only those are candidates by tree.
    own: bool,
    pub(super) rule: Rule,
}

/// The rules of every sheet, filed by what their subjects must be, so a node is matched only
/// against the rules that could apply to it, and by the tree the sheet is in — a shadow tree's
/// rules are its own, `:host` and `::slotted` the rules that reach out of one.
#[derive(Clone, Default)]
pub struct Styler {
    pub(super) rules: Vec<RuleEntry>,
    buckets: HashMap<RuleKey, Vec<(usize, usize)>>,
    scope: ScopeIndex,
    env: Environment,
    /// How many layers the sheets declare: the order between two layers is their numbering.
    pub(super) layers: u32,
    /// Whether any declaration says `revert-layer`: the history it needs is kept only then.
    pub(super) revert_layer: bool,
    /// The parts of elements the rules are about — `::before`, `::-webkit-scrollbar-thumb` —
    /// which are worked out beside each element. A page that says nothing about them pays
    /// nothing for them.
    pub(super) parts: Vec<PseudoElement>,
    /// Whether any rule sets a `transition…` or an `animation…`, or any sheet has `@keyframes`.
    animates: bool,
    /// Every `@keyframes` the sheets declare, by name; a later one of the same name wins.
    keyframes: HashMap<String, Arc<Keyframes>>,
}

impl Styler {
    /// The cascade of `sheets`, in order, each with where it comes from and the tree it is in,
    /// for `env`.
    pub fn new(sheets: &[(Origin, &Sheet, NodeId)], env: &Environment) -> Self {
        let mut out = Styler {
            env: env.clone(),
            ..Styler::default()
        };
        let order = LayerOrder::new(
            &sheets
                .iter()
                .map(|(_, sheet, tree)| (*tree, *sheet))
                .collect::<Vec<_>>(),
        );
        out.layers = order.count();
        for (_, sheet, _) in sheets {
            out.animates |= !sheet.keyframes.is_empty();
            out.keyframes
                .extend(sheet.keyframes.iter().map(|(n, k)| (n.clone(), k.clone())));
        }
        for (ordinal, (origin, sheet, tree)) in sheets.iter().enumerate() {
            for rule in sheet
                .rules
                .iter()
                .filter(|r| r.media.iter().all(|m| m.matches(env)))
            {
                out.revert_layer |= rule.declarations.iter().any(says_revert_layer);
                out.animates |= rule
                    .declarations
                    .iter()
                    .any(|d| d.name.starts_with("transition") || d.name.starts_with("animation"));
                out.add_rule(*origin, *tree, rule, order.place(ordinal, rule.layer));
            }
        }
        out
    }

    fn add_rule(&mut self, origin: Origin, tree: NodeId, rule: &Rule, layer: u32) {
        let index = self.rules.len();
        let mut own = false;
        for (k, selector) in rule.selectors.0.iter().enumerate() {
            self.buckets
                .entry(selector.key())
                .or_default()
                .push((index, k));
            if let Some(part) = selector.pseudo_element()
                && !self.parts.contains(&part)
            {
                self.parts.push(part);
            }
            let reach = selector.subject().reach;
            own |= reach == Reach::Own;
            self.scope.add(tree, (index, k), reach);
        }
        let order = index + 1;
        self.rules.push(RuleEntry {
            origin,
            order,
            tree,
            layer,
            own,
            rule: rule.clone(),
        });
    }

    /// Whether the sheets ask for transitions or animations at all.
    pub(crate) fn animates(&self) -> bool {
        self.animates
    }

    /// The `@keyframes` a name stands for.
    pub(super) fn keyframes(&self, name: &str) -> Option<Arc<Keyframes>> {
        self.keyframes.get(name).cloned()
    }

    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// The rules that could apply to `id` from the tree it is in, each with the selector of it
    /// to try.
    pub(super) fn candidates(&self, dom: &Dom, id: NodeId, tree: NodeId) -> Vec<(usize, usize)> {
        let Some(node) = dom.get(id) else {
            return Vec::new();
        };
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut add = |key: RuleKey| {
            if let Some(list) = self.buckets.get(&key) {
                out.extend(
                    list.iter()
                        .copied()
                        .filter(|(index, _)| self.applies(*index, tree)),
                );
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
        out.extend(self.scope.extra_candidates(dom, id));
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Whether the rule `index` is the tree `tree`'s business: its own rules, or the user
    /// agent's. A `:host`, `::slotted` or `::part` rule is asked of the element it reaches,
    /// not found among a node's own candidates.
    fn applies(&self, index: usize, tree: NodeId) -> bool {
        let entry = &self.rules[index];
        entry.own && (entry.origin == Origin::UserAgent || entry.tree == tree)
    }
}
