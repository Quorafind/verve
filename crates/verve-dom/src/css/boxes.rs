//! The boxes of the pseudo-elements, kept beside the styles of the nodes they belong to: a
//! generated box (`::before`, `::after`) with what its `content` says, the style of a part
//! that is no box (`::selection`, a scrollbar's thumb).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::cascade::Style;
use super::counters::{Counters, walk};
use super::pseudo::{ContentItem, Piece, PseudoBox, PseudoElement, content_pieces};
use crate::dom::{Dom, NodeId};

type Key = (NodeId, PseudoElement);

/// Every pseudo-element box of a document, by the node it belongs to and the part it is.
#[derive(Clone, Default)]
pub(super) struct Boxes {
    /// Shared, so that a snapshot takes it by cloning a pointer.
    boxes: Arc<HashMap<Key, PseudoBox>>,
    /// The `content` of the boxes that read a counter, as it was written: counters are counted
    /// over the whole document, so it is read again whenever the styles are brought up to date.
    counted: HashMap<Key, Vec<Piece>>,
    /// The nodes with a box that is not what it was since this was last asked.
    changed: Vec<NodeId>,
}

impl Boxes {
    pub(super) fn all(&self) -> &Arc<HashMap<Key, PseudoBox>> {
        &self.boxes
    }

    /// Forgets every box, as when the stylesheets change and all is worked out again.
    pub(super) fn clear(&mut self) {
        self.boxes = Arc::default();
        self.counted.clear();
    }

    /// Forgets the boxes of nodes that are gone.
    pub(super) fn forget(&mut self, ids: &[NodeId]) {
        let gone: HashSet<NodeId> = ids.iter().copied().collect();
        if self.boxes.keys().any(|(id, _)| gone.contains(id)) {
            Arc::make_mut(&mut self.boxes).retain(|(id, _), _| !gone.contains(id));
        }
        self.counted.retain(|(id, _), _| !gone.contains(id));
    }

    /// The nodes whose boxes changed since this was last asked, for the drawer to be told they
    /// look different.
    pub(super) fn take_changed(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.changed)
    }

    /// Notes what the rules written for `part` of `id` come to, `style`: the box it makes, if
    /// it makes one. A generated box only exists with a `content` that draws something.
    pub(super) fn set(&mut self, dom: &Dom, id: NodeId, part: PseudoElement, style: Option<Style>) {
        let key = (id, part);
        let made = style.and_then(|style| {
            let generated = matches!(part, PseudoElement::Before | PseudoElement::After);
            let pieces = style
                .get("content")
                .map_or_else(Vec::new, |content| content_pieces(content));
            (!generated || !pieces.is_empty()).then_some((style, pieces))
        });
        let Some((style, pieces)) = made else {
            self.counted.remove(&key);
            if self.boxes.contains_key(&key) {
                Arc::make_mut(&mut self.boxes).remove(&key);
                self.changed.push(id);
            }
            return;
        };
        let content = items(dom, id, &pieces, None);
        if pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Counter { .. }))
        {
            self.counted.insert(key, pieces);
        } else {
            self.counted.remove(&key);
        }
        self.put(
            key,
            PseudoBox {
                style: Arc::new(style),
                content,
            },
        );
    }

    /// Reads the counters again into every box that reads one. The counters stand where the
    /// walk of the document stands, so this is a walk of the whole document.
    pub(super) fn count(&mut self, dom: &Dom, styles: &HashMap<NodeId, Arc<Style>>) {
        if self.counted.is_empty() {
            return;
        }
        let mut counted = Vec::new();
        walk(dom, styles, |id, part, level, counters| {
            let key = (id, part);
            if let Some(found) = self.boxes.get(&key) {
                counters.apply(&found.style, level);
                if let Some(pieces) = self.counted.get(&key) {
                    counted.push((key, items(dom, id, pieces, Some(&*counters))));
                }
            }
        });
        for (key, content) in counted {
            if let Some(found) = self.boxes.get(&key)
                && found.content != content
            {
                let style = found.style.clone();
                self.put(key, PseudoBox { style, content });
            }
        }
    }

    fn put(&mut self, key: Key, made: PseudoBox) {
        if self.boxes.get(&key) != Some(&made) {
            Arc::make_mut(&mut self.boxes).insert(key, made);
            self.changed.push(key.0);
        }
    }
}

/// The items `pieces` generate for `id`: `attr()` reads the element, and a counter its value
/// where the walk stands, or nothing when it is not known yet.
fn items(dom: &Dom, id: NodeId, pieces: &[Piece], counters: Option<&Counters>) -> Vec<ContentItem> {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Text(text) => ContentItem::Text(text.clone()),
            Piece::Image(url) => ContentItem::Image(url.clone()),
            Piece::OpenQuote => ContentItem::OpenQuote,
            Piece::CloseQuote => ContentItem::CloseQuote,
            Piece::Attr(name) => ContentItem::Text(dom.get_attr(id, name).unwrap_or_default()),
            Piece::Counter {
                name,
                separator,
                style,
            } => ContentItem::Text(counters.map_or_else(String::new, |counters| {
                counters.text(name, separator.as_deref(), style)
            })),
        })
        .collect()
}
