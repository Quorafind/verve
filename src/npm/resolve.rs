//! Building the ideal tree, and deciding what of it is not installed and why.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::registry::{Manifest, Registry};
use super::semver::Version;
use super::spec::{Spec, Wanted};
use super::tree::{self, EdgeKind, Include, NodeId, Queue, ROOT, Tree, queue_key};
use super::{InstallError, Platform, SkippedPackage, extract, integrity, parallel_map};

/// A tree with what will not be installed of it: the platform-mismatched optional packages and
/// what only they need. Those stay in the lockfile, as npm keeps them, so it serves every
/// platform.
pub struct Plan {
    pub tree: Tree,
    pub skip: HashSet<NodeId>,
    pub skipped: Vec<SkippedPackage>,
    /// Tarballs fetched while resolving (dependencies given as URLs), by URL.
    pub fetched: HashMap<String, Arc<Vec<u8>>>,
}

pub struct Resolver<'a> {
    pub registry: &'a Registry,
    pub concurrency: usize,
    pub include: Include,
    /// Versions a previous lockfile pinned, by real name: preferred when they still satisfy,
    /// so adding a dependency does not move the others.
    pub hints: HashMap<String, Vec<Arc<Manifest>>>,
    fetched: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    failures: Vec<(NodeId, usize, InstallError)>,
}

impl<'a> Resolver<'a> {
    pub fn new(
        registry: &'a Registry,
        concurrency: usize,
        include: Include,
        hints: HashMap<String, Vec<Arc<Manifest>>>,
    ) -> Self {
        Resolver {
            registry,
            concurrency,
            include,
            hints,
            fetched: Mutex::new(HashMap::new()),
            failures: Vec::new(),
        }
    }

    fn pick(&self, edge_name: &str, raw: &str, spec: &Spec) -> Result<Arc<Manifest>, InstallError> {
        match spec {
            Spec::Registry { name, wanted } => {
                if let Wanted::Range(range) = wanted {
                    let hinted = self.hints.get(name).and_then(|hints| {
                        hints
                            .iter()
                            .filter_map(|m| Version::parse(&m.version).map(|v| (v, m)))
                            .filter(|(v, _)| range.satisfies(v))
                            .max_by(|a, b| a.0.cmp(&b.0))
                            .map(|(_, m)| Arc::clone(m))
                    });
                    if let Some(hinted) = hinted {
                        return Ok(hinted);
                    }
                }
                let packument = self.registry.packument(name)?;
                packument
                    .pick(wanted)
                    .map_err(|reason| InstallError::NoMatchingVersion {
                        package: name.clone(),
                        spec: raw.to_owned(),
                        reason,
                    })
            }
            Spec::Tarball { url } => {
                let bytes = self.registry.tarball(edge_name, url, None)?;
                let integrity = integrity::sha512(&bytes);
                let doc = extract::read_package_json(&bytes).map_err(|reason| {
                    InstallError::BadTarball {
                        package: edge_name.to_owned(),
                        version: url.clone(),
                        reason,
                    }
                })?;
                let mut manifest = Manifest::from_json(&doc);
                if manifest.name.is_empty() || manifest.version.is_empty() {
                    return Err(InstallError::BadTarball {
                        package: edge_name.to_owned(),
                        version: url.clone(),
                        reason: "its package.json has no name or version".to_owned(),
                    });
                }
                manifest.tarball = Some(url.clone());
                manifest.integrity = Some(integrity);
                manifest.shasum = None;
                self.fetched
                    .lock()
                    .expect("fetched tarballs")
                    .insert(url.clone(), Arc::new(bytes));
                Ok(Arc::new(manifest))
            }
        }
    }

    /// Visits dependents shallowest first, placing what each one lacks.
    pub fn build(&mut self, tree: &mut Tree) {
        let mut queue = Queue::new();
        queue.insert(queue_key(tree, ROOT));
        while let Some((_, _, id)) = queue.pop_first() {
            if tree.nodes[id].removed || tree.nodes[id].in_bundle {
                continue;
            }
            let mut problems = Vec::new();
            for edge in 0..tree.nodes[id].edges.len() {
                let (name, spec) = {
                    let e = &tree.nodes[id].edges[edge];
                    (e.name.clone(), e.spec.clone())
                };
                if tree.nodes[id].edges[edge].to.is_some() {
                    continue;
                }
                match spec {
                    Err(reason) => {
                        let raw = tree.nodes[id].edges[edge].raw.clone();
                        self.failures.push((
                            id,
                            edge,
                            InstallError::Spec {
                                package: name,
                                spec: raw,
                                reason,
                            },
                        ));
                    }
                    Ok(spec) => match tree.resolve(id, &name) {
                        Some(found) if tree.satisfies(found, &spec) => tree.link(id, edge, found),
                        _ => problems.push((edge, name, spec)),
                    },
                }
            }
            let picks = parallel_map(&problems, self.concurrency, |(edge, name, spec)| {
                let raw = &tree.nodes[id].edges[*edge].raw;
                self.pick(name, raw, spec)
            });
            for ((edge, name, _), pick) in problems.into_iter().zip(picks) {
                let manifest = match pick {
                    Ok(manifest) => manifest,
                    Err(error) => {
                        self.failures.push((id, edge, error));
                        continue;
                    }
                };
                match tree.place_target(id, &name, &manifest) {
                    Some(target) => {
                        let edges =
                            tree::edges_of(&manifest, false, self.include, &Default::default());
                        let new = tree.add(target, &name, manifest, false);
                        tree.nodes[new].edges = edges;
                        tree.link(id, edge, new);
                        queue.insert(queue_key(tree, new));
                    }
                    None => self.failures.push((
                        id,
                        edge,
                        InstallError::Conflict {
                            package: name.clone(),
                            location: tree.nodes[id].location.clone(),
                        },
                    )),
                }
            }
        }
    }

    /// Settles what failed: an optional dependency that could not be had is skipped (with
    /// whatever only it needed), a required one fails the install, naming the package.
    pub fn finish(mut self, mut tree: Tree, platform: &Platform) -> Result<Plan, InstallError> {
        let mut skipped = Vec::new();
        let none = HashSet::new();
        let required = tree.reachable(|edge| edge.kind != EdgeKind::Optional, &none);
        let mut cut = Vec::new();
        for (from, edge, error) in std::mem::take(&mut self.failures) {
            let e = &tree.nodes[from].edges[edge];
            let required_by = shown_location(&tree.nodes[from].location);
            if e.kind == EdgeKind::Optional {
                skipped.push(SkippedPackage {
                    name: e.name.clone(),
                    wanted: e.raw.clone(),
                    location: None,
                    required_by,
                    reason: error.to_string(),
                });
            } else if required[from] {
                return Err(InstallError::Dependency {
                    name: e.name.clone(),
                    spec: e.raw.clone(),
                    required_by,
                    error: Box::new(error),
                });
            } else {
                cut.push((from, format!("{}@{}: {error}", e.name, e.raw)));
            }
        }
        // An optional package whose own dependency failed goes, with all that only it needed:
        // every path to it has an optional edge into a package only optional edges reach, and
        // those edges are cut.
        for (failed, reason) in cut {
            let leads_there = tree.ancestors_by_edge(failed);
            for id in 0..tree.nodes.len() {
                for edge in 0..tree.nodes[id].edges.len() {
                    let e = &tree.nodes[id].edges[edge];
                    let cuts = e.kind == EdgeKind::Optional
                        && e.to
                            .is_some_and(|to| leads_there.contains(&to) && !required[to]);
                    if cuts {
                        skipped.push(SkippedPackage {
                            name: e.name.clone(),
                            wanted: e.raw.clone(),
                            location: None,
                            required_by: shown_location(&tree.nodes[id].location),
                            reason: format!("its dependency {reason}"),
                        });
                        tree.unlink(id, edge);
                    }
                }
            }
        }
        tree.prune();

        let required = tree.reachable(|edge| edge.kind != EdgeKind::Optional, &none);
        let mut mismatched = HashSet::new();
        for id in tree.live().collect::<Vec<_>>() {
            let node = &tree.nodes[id];
            if let Err(reason) = platform.check(&node.manifest) {
                if required[id] {
                    return Err(InstallError::BadPlatform {
                        package: node.manifest.name.clone(),
                        version: node.manifest.version.clone(),
                        reason,
                    });
                }
                skipped.push(SkippedPackage {
                    name: node.manifest.name.clone(),
                    wanted: node.manifest.version.clone(),
                    location: Some(node.location.clone()),
                    required_by: dependents(&tree, id),
                    reason,
                });
                mismatched.insert(id);
            }
        }
        let installed = tree.reachable(|_| true, &mismatched);
        let skip: HashSet<NodeId> = tree.live().filter(|&id| !installed[id]).collect();
        let fetched = std::mem::take(&mut *self.fetched.lock().expect("fetched tarballs"));
        Ok(Plan {
            tree,
            skip,
            skipped,
            fetched,
        })
    }
}

/// How a location reads in a message: the project is "the project".
pub fn shown_location(location: &str) -> String {
    if location.is_empty() {
        "the project".to_owned()
    } else {
        location.to_owned()
    }
}

fn dependents(tree: &Tree, id: NodeId) -> String {
    let mut names: Vec<String> = tree.nodes[id]
        .edges_in
        .iter()
        .map(|&(from, _)| shown_location(&tree.nodes[from].location))
        .collect();
    names.sort();
    names.dedup();
    names.join(", ")
}
