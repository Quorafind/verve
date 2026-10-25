//! The `node_modules` tree: npm v7+'s hoisted layout, built breadth first.
//!
//! Node finds a package by walking up from the requiring file through every `node_modules`
//! folder above it, so one copy at the top serves every dependent whose range it satisfies.
//! npm's arborist builds the tree that way: dependents are visited shallowest first (by depth,
//! then location), each one's missing or unsatisfied dependencies in name order, and each new
//! package goes as high as it can — up from its dependent until the next folder up already has
//! a different package of that name, or until placing it there would hide, from a dependent
//! below, the package that dependent resolves to now and that the new one does not satisfy.
//! A package already reachable that satisfies the range is reused (dedupe). This is that
//! algorithm without arborist's peer handling, which `--legacy-peer-deps` turns off, and
//! without its rare `REPLACE` of an older placed package by a newer one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use super::registry::Manifest;
use super::spec::{self, Spec};

pub type NodeId = usize;

pub const ROOT: NodeId = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Prod,
    Optional,
    Dev,
}

#[derive(Clone, Debug)]
pub struct Edge {
    pub name: String,
    pub raw: String,
    pub kind: EdgeKind,
    /// The spec, or why it cannot be installed.
    pub spec: Result<Spec, String>,
    pub to: Option<NodeId>,
}

#[derive(Clone, Debug)]
pub struct Node {
    /// `node_modules/a/node_modules/b`; empty for the project.
    pub location: String,
    /// The folder's name: the dependency's name, an alias's own.
    pub name: String,
    pub parent: Option<NodeId>,
    pub children: BTreeMap<String, NodeId>,
    pub manifest: Arc<Manifest>,
    pub edges: Vec<Edge>,
    pub edges_in: Vec<(NodeId, usize)>,
    /// Shipped inside its bundler's tarball; never fetched on its own. A bundled package of a
    /// fresh resolution has no manifest until its bundler is extracted.
    pub in_bundle: bool,
    pub removed: bool,
}

impl Node {
    pub fn depth(&self) -> usize {
        self.location.matches("node_modules/").count()
    }
}

/// Which dependency kinds a tree follows.
#[derive(Clone, Copy, Debug)]
pub struct Include {
    pub dev: bool,
    pub optional: bool,
}

/// The edges a package's manifest asks for, by name. Optional dependencies override plain ones
/// of the same name, and the project's dev dependencies override both, as in arborist.
pub fn edges_of(
    manifest: &Manifest,
    root: bool,
    include: Include,
    dev: &BTreeMap<String, String>,
) -> Vec<Edge> {
    let mut by_name: BTreeMap<&str, (&str, EdgeKind)> = BTreeMap::new();
    for (name, raw) in &manifest.dependencies {
        by_name.insert(name, (raw, EdgeKind::Prod));
    }
    for (name, raw) in &manifest.optional_dependencies {
        if include.optional {
            by_name.insert(name, (raw, EdgeKind::Optional));
        } else {
            by_name.remove(name.as_str());
        }
    }
    if root && include.dev {
        for (name, raw) in dev {
            by_name.insert(name, (raw, EdgeKind::Dev));
        }
    }
    by_name
        .into_iter()
        .map(|(name, (raw, kind))| Edge {
            name: name.to_owned(),
            raw: raw.to_owned(),
            kind,
            spec: spec::parse(name, raw),
            to: None,
        })
        .collect()
}

/// Whether `manifest` (with the folder name it would have) is what `spec` asks for.
pub fn manifest_satisfies(manifest: &Manifest, spec: &Spec) -> bool {
    match spec {
        Spec::Tarball { url } => manifest.tarball.as_deref() == Some(url.as_str()),
        spec => spec.accepts(&manifest.name, &manifest.version),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub nodes: Vec<Node>,
}

impl Tree {
    pub fn new(project: Arc<Manifest>, edges: Vec<Edge>) -> Tree {
        Tree {
            nodes: vec![Node {
                location: String::new(),
                name: project.name.clone(),
                parent: None,
                children: BTreeMap::new(),
                manifest: project,
                edges,
                edges_in: Vec::new(),
                in_bundle: false,
                removed: false,
            }],
        }
    }

    pub fn live(&self) -> impl Iterator<Item = NodeId> + '_ {
        (1..self.nodes.len()).filter(|&id| !self.nodes[id].removed)
    }

    /// What `name` resolves to from `from`'s folder, by Node's walk up the `node_modules`.
    pub fn resolve(&self, from: NodeId, name: &str) -> Option<NodeId> {
        let mut current = Some(from);
        while let Some(id) = current {
            if let Some(&child) = self.nodes[id].children.get(name)
                && !self.nodes[child].removed
            {
                return Some(child);
            }
            current = self.nodes[id].parent;
        }
        None
    }

    /// Whether `id` sits in `ancestor`'s folder (or is it).
    pub fn within(&self, id: NodeId, ancestor: NodeId) -> bool {
        let mut current = Some(id);
        while let Some(at) = current {
            if at == ancestor {
                return true;
            }
            current = self.nodes[at].parent;
        }
        false
    }

    pub fn satisfies(&self, id: NodeId, spec: &Spec) -> bool {
        let node = &self.nodes[id];
        (node.in_bundle && node.manifest.version.is_empty())
            || manifest_satisfies(&node.manifest, spec)
    }

    pub fn link(&mut self, from: NodeId, edge: usize, to: NodeId) {
        self.nodes[from].edges[edge].to = Some(to);
        self.nodes[to].edges_in.push((from, edge));
    }

    pub fn unlink(&mut self, from: NodeId, edge: usize) {
        if let Some(to) = self.nodes[from].edges[edge].to.take() {
            self.nodes[to].edges_in.retain(|&e| e != (from, edge));
        }
    }

    /// The highest folder from `from` up where a new `manifest` for `name` fits; `None` when
    /// not even `from`'s own `node_modules` is free.
    pub fn place_target(&self, from: NodeId, name: &str, manifest: &Manifest) -> Option<NodeId> {
        let mut found = None;
        let mut current = Some(from);
        while let Some(target) = current {
            if self.nodes[target]
                .children
                .get(name)
                .is_some_and(|&c| !self.nodes[c].removed)
            {
                break;
            }
            // A dependent at or below `target` that reaches a package above it now would reach
            // the new one instead: only allowed when the new one satisfies it too.
            if let Some(shadowed) = self.resolve(target, name) {
                let hides = self.nodes[shadowed]
                    .edges_in
                    .iter()
                    .any(|&(dependent, edge)| {
                        self.within(dependent, target)
                            && match &self.nodes[dependent].edges[edge].spec {
                                Ok(spec) => !manifest_satisfies(manifest, spec),
                                Err(_) => false,
                            }
                    });
                if hides {
                    break;
                }
            }
            found = Some(target);
            current = self.nodes[target].parent;
        }
        found
    }

    /// Adds `manifest` as `name` in `target`'s `node_modules`; the dependents below `target`
    /// that resolved `name` above it now resolve to the new node. Bundled dependencies come with
    /// it as placeholders, so nothing is placed over them.
    pub fn add(
        &mut self,
        target: NodeId,
        name: &str,
        manifest: Arc<Manifest>,
        in_bundle: bool,
    ) -> NodeId {
        let shadowed = self.resolve(target, name);
        let location = child_location(&self.nodes[target].location, name);
        let id = self.nodes.len();
        let bundled = if in_bundle {
            Vec::new()
        } else {
            manifest.bundled.clone()
        };
        self.nodes.push(Node {
            location,
            name: name.to_owned(),
            parent: Some(target),
            children: BTreeMap::new(),
            edges: Vec::new(),
            edges_in: Vec::new(),
            manifest,
            in_bundle,
            removed: false,
        });
        self.nodes[target].children.insert(name.to_owned(), id);
        if let Some(shadowed) = shadowed {
            let moved: Vec<(NodeId, usize)> = self.nodes[shadowed]
                .edges_in
                .iter()
                .copied()
                .filter(|&(dependent, _)| self.within(dependent, target))
                .collect();
            for (dependent, edge) in moved {
                self.unlink(dependent, edge);
                self.link(dependent, edge, id);
            }
        }
        for bundled_name in bundled {
            if spec::valid_name(&bundled_name)
                && !self.nodes[id].children.contains_key(&bundled_name)
            {
                let placeholder = Manifest {
                    name: bundled_name.clone(),
                    ..Manifest::default()
                };
                self.add(id, &bundled_name, Arc::new(placeholder), true);
            }
        }
        id
    }

    /// The nodes reached from the project by the edges `follow` accepts, not entering `except`.
    /// A bundled package nothing in the tree depends on is reached with its bundler, which
    /// shipped it for a reason the tree cannot see.
    pub fn reachable(&self, follow: impl Fn(&Edge) -> bool, except: &HashSet<NodeId>) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![ROOT];
        seen[ROOT] = true;
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            let bundled = node
                .children
                .values()
                .copied()
                .filter(|&c| self.nodes[c].in_bundle && self.nodes[c].edges_in.is_empty());
            let targets: Vec<NodeId> = node
                .edges
                .iter()
                .filter(|edge| follow(edge))
                .filter_map(|edge| edge.to)
                .chain(bundled)
                .collect();
            for to in targets {
                if !seen[to] && !self.nodes[to].removed && !except.contains(&to) {
                    seen[to] = true;
                    stack.push(to);
                }
            }
        }
        seen
    }

    /// Removes the nodes nothing reaches any more.
    pub fn prune(&mut self) -> Vec<NodeId> {
        let reached = self.reachable(|_| true, &HashSet::new());
        let pruned: Vec<NodeId> = self.live().filter(|&id| !reached[id]).collect();
        for &id in &pruned {
            self.nodes[id].removed = true;
            for edge in 0..self.nodes[id].edges.len() {
                self.unlink(id, edge);
            }
            if let Some(parent) = self.nodes[id].parent {
                let name = self.nodes[id].name.clone();
                if self.nodes[parent].children.get(&name) == Some(&id) {
                    self.nodes[parent].children.remove(&name);
                }
            }
        }
        pruned
    }

    /// The nodes reached through `id` only — those a failed optional `id` takes with it.
    pub fn ancestors_by_edge(&self, id: NodeId) -> HashSet<NodeId> {
        let mut seen = HashSet::from([id]);
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            for &(dependent, _) in &self.nodes[at].edges_in {
                if seen.insert(dependent) {
                    stack.push(dependent);
                }
            }
        }
        seen
    }

    /// Reads a `package-lock.json`'s `packages` into a tree, every edge resolved the way Node
    /// would on disk. Answers whether the lock is complete: every entry has what installing it
    /// needs, and every required edge resolves to a package that satisfies it.
    pub fn from_lock(
        project: Arc<Manifest>,
        root_edges: Vec<Edge>,
        packages: &serde_json::Map<String, Value>,
        include: Include,
    ) -> (Tree, bool) {
        let mut tree = Tree::new(project, root_edges);
        let mut complete = true;
        let mut by_location: HashMap<String, NodeId> = HashMap::from([(String::new(), ROOT)]);
        let mut locations: Vec<&String> = packages.keys().filter(|k| !k.is_empty()).collect();
        locations
            .sort_by_key(|location| (location.matches("node_modules/").count(), location.as_str()));
        for location in locations {
            let entry = &packages[location];
            let Some((parent_location, name)) = split_location(location) else {
                complete = false;
                continue;
            };
            let Some(&parent) = by_location.get(parent_location) else {
                complete = false;
                continue;
            };
            if !spec::valid_name(name) || entry.get("link").and_then(Value::as_bool) == Some(true) {
                complete = false;
                continue;
            }
            let mut manifest = Manifest::from_json(entry);
            if manifest.name.is_empty() {
                manifest.name = name.to_owned();
            }
            let in_bundle = entry.get("inBundle").and_then(Value::as_bool) == Some(true);
            if manifest.version.is_empty() || (!in_bundle && manifest.tarball.is_none()) {
                complete = false;
            }
            let id = tree.nodes.len();
            tree.nodes.push(Node {
                location: location.clone(),
                name: name.to_owned(),
                parent: Some(parent),
                children: BTreeMap::new(),
                edges: edges_of(&manifest, false, include, &BTreeMap::new()),
                edges_in: Vec::new(),
                manifest: Arc::new(manifest),
                in_bundle,
                removed: false,
            });
            tree.nodes[parent].children.insert(name.to_owned(), id);
            by_location.insert(location.clone(), id);
        }
        for id in 0..tree.nodes.len() {
            // A bundle is what its bundler shipped: its edges are followed where they resolve
            // (for the lockfile's flags) but never make the lock incomplete.
            let in_bundle = tree.nodes[id].in_bundle;
            for edge in 0..tree.nodes[id].edges.len() {
                let (name, spec, kind) = {
                    let e = &tree.nodes[id].edges[edge];
                    (e.name.clone(), e.spec.clone(), e.kind)
                };
                let target = tree.resolve(id, &name);
                match (spec, target) {
                    (Ok(spec), Some(target)) if tree.satisfies(target, &spec) => {
                        tree.link(id, edge, target)
                    }
                    _ if in_bundle => {}
                    // An optional dependency npm could not install is simply absent.
                    (_, None) if kind == EdgeKind::Optional => {}
                    _ => complete = false,
                }
            }
        }
        (tree, complete)
    }
}

pub fn child_location(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        format!("node_modules/{name}")
    } else {
        format!("{parent}/node_modules/{name}")
    }
}

/// `node_modules/a/node_modules/@s/b` → (`node_modules/a`, `@s/b`).
pub fn split_location(location: &str) -> Option<(&str, &str)> {
    let at = location.rfind("node_modules/")?;
    let name = &location[at + "node_modules/".len()..];
    let parent = location[..at].strip_suffix('/').unwrap_or(&location[..at]);
    if !parent.is_empty() && !location[..at].ends_with('/') {
        return None;
    }
    Some((parent, name))
}

/// The breadth-first order arborist visits dependents in.
pub fn queue_key(tree: &Tree, id: NodeId) -> (usize, String, NodeId) {
    (tree.nodes[id].depth(), tree.nodes[id].location.clone(), id)
}

pub type Queue = BTreeSet<(usize, String, NodeId)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_split_into_parent_and_name() {
        assert_eq!(split_location("node_modules/a"), Some(("", "a")));
        assert_eq!(
            split_location("node_modules/a/node_modules/@s/b"),
            Some(("node_modules/a", "@s/b"))
        );
        assert_eq!(split_location("lib/x"), None);
    }
}
