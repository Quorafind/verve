//! `@layer` (CSS Cascade Level 5): the order the layers of a cascade are in, where a
//! declaration of a layer stands against the others for its importance, and how
//! `revert-layer` rolls a property back.
//!
//! Layers are per tree: a shadow tree's `@layer a` is not the document's. Between trees the
//! order is the order they come in, so a rank can always be compared.

use std::collections::HashMap;

use super::cascade::Precedence;
use super::sheet::{LAYERLESS, Sheet};
use crate::dom::NodeId;

/// Where every layer of every sheet of a cascade stands, as one number each, the first of
/// them lowest.
pub(super) struct LayerOrder {
    /// For each sheet, the place of each of its layers.
    places: Vec<Vec<u32>>,
    count: u32,
}

impl LayerOrder {
    /// The order of the layers `sheets` declare, each sheet with the tree it is in. A layer is
    /// one layer across the sheets of a tree, ordered by where the tree's sheets first name it;
    /// a sublayer comes before the rules of the layer it is in, which are that layer's last
    /// sublayer; and a layer written without a name is a layer of its own.
    pub(super) fn new(sheets: &[(NodeId, &Sheet)]) -> Self {
        let mut trees: Vec<(NodeId, Names)> = Vec::new();
        for (ordinal, (tree, sheet)) in sheets.iter().enumerate() {
            let at = trees
                .iter()
                .position(|(t, _)| t == tree)
                .unwrap_or_else(|| {
                    trees.push((*tree, Names::default()));
                    trees.len() - 1
                });
            for path in &sheet.layers {
                trees[at].1.insert(&qualified(ordinal, path));
            }
        }
        let mut found: HashMap<(NodeId, String), u32> = HashMap::new();
        for (tree, names) in &trees {
            let mut order = Vec::new();
            names.post_order("", &mut order);
            for path in order {
                let place = found.len() as u32;
                found.insert((*tree, path), place);
            }
        }
        let places = sheets
            .iter()
            .enumerate()
            .map(|(ordinal, (tree, sheet))| {
                sheet
                    .layers
                    .iter()
                    .map(|path| found[&(*tree, qualified(ordinal, path).join("."))])
                    .collect()
            })
            .collect();
        Self {
            places,
            count: found.len() as u32,
        }
    }

    /// The place of layer `layer` of sheet number `sheet`; [`LAYERLESS`] stays as it is.
    pub(super) fn place(&self, sheet: usize, layer: u32) -> u32 {
        if layer == LAYERLESS {
            LAYERLESS
        } else {
            self.places[sheet][layer as usize]
        }
    }

    pub(super) fn count(&self) -> u32 {
        self.count
    }
}

/// The names in a layer path, with the number of a layer written without one made the
/// sheet's own, so that two sheets' `@layer { … }` are two layers.
fn qualified(sheet: usize, path: &str) -> Vec<String> {
    path.split('.')
        .map(|name| match name.strip_prefix('<') {
            Some(number) => format!("<{sheet}:{number}"),
            None => name.to_owned(),
        })
        .collect()
}

/// The layers of one tree as a tree of names, each in the order it was first named.
#[derive(Default)]
struct Names {
    children: Vec<(String, Names)>,
}

impl Names {
    fn insert(&mut self, path: &[String]) {
        let Some((name, rest)) = path.split_first() else {
            return;
        };
        let at = self
            .children
            .iter()
            .position(|(child, _)| child == name)
            .unwrap_or_else(|| {
                self.children.push((name.clone(), Names::default()));
                self.children.len() - 1
            });
        self.children[at].1.insert(rest);
    }

    /// Every layer below, as full names, each after its sublayers.
    fn post_order(&self, prefix: &str, out: &mut Vec<String>) {
        for (name, child) in &self.children {
            let full = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}.{name}")
            };
            child.post_order(&full, out);
            out.push(full);
        }
    }
}

/// Where a declaration of `layer` stands among the others of its importance, of the `count`
/// layers there are: a normal declaration of a later layer wins, an important one of an
/// earlier layer wins, and an unlayered one is the last layer — so it wins when normal and
/// loses when important.
pub(super) fn key(layer: u32, important: bool, count: u32) -> u32 {
    match (layer == LAYERLESS, important) {
        (true, false) => count,
        (true, true) => 0,
        (false, false) => layer,
        (false, true) => count - layer,
    }
}

/// What each property was set to by the declarations applied so far, for `revert-layer`.
#[derive(Default)]
pub(super) struct History {
    by_property: HashMap<String, Vec<(Place, String)>>,
}

/// The origin, importance and layer a declaration comes from.
pub(super) type Place = (Precedence, u32);

impl History {
    /// Notes that `property` was set to `value` from `place`.
    pub(super) fn record(&mut self, property: &str, place: Place, value: &str) {
        self.by_property
            .entry(property.to_owned())
            .or_default()
            .push((place, value.to_owned()));
    }

    /// What `property` was before the layer at `place`: what the declarations of that layer
    /// and of any above it are taken back from, or `None` when nothing set it below — from the
    /// user agent's, say, since a rule of another origin is under every layer.
    pub(super) fn revert(&mut self, property: &str, place: Place) -> Option<&str> {
        let history = self.by_property.entry(property.to_owned()).or_default();
        while history.last().is_some_and(|(at, _)| *at >= place) {
            history.pop();
        }
        history.last().map(|(_, value)| value.as_str())
    }
}
