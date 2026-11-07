//! What one inspector window knows of its page, as plain data: the element tree, which node is
//! selected and which is under the pointer, the console and what is being typed. A window draws
//! it and a person's clicks and keys change it; nothing here needs GPUI.

use std::sync::Arc;

use verve_dom::{NodeId, Update};
use verve_layout::DisplayList;

use super::boxmodel::{self, BoxModel, Edges};
use super::console::{Console, LineEditor};
use super::styles::{self, StyleLine};
use super::tree::Tree;

/// The node a window draws over its page, and the widths around its border box. The box itself
/// is looked up in the page's list each frame, so the highlight follows a scroll.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Highlight {
    pub node: NodeId,
    pub margin: Edges,
    pub border: Edges,
    pub padding: Edges,
}

/// Everything an inspector window shows of its page.
#[derive(Default)]
pub struct Model {
    pub tree: Tree,
    pub console: Console,
    pub input: LineEditor,
    /// Whether moving over the page picks the node under the pointer.
    pub picking: bool,
    selected: Option<NodeId>,
    hovered: Option<NodeId>,
    list: Option<Arc<DisplayList>>,
}

impl Model {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes what the page published: its document with what changed, and its layout.
    pub fn page(&mut self, update: Option<Update>, list: Option<Arc<DisplayList>>) {
        if let Some(update) = update {
            self.tree.apply(update.snapshot, &update.changes);
            let gone = |node: &mut Option<NodeId>| {
                let snapshot = self.tree.snapshot();
                let there = |n: NodeId| snapshot.is_some_and(|s| s.dom.is_connected(n));
                if node.is_some_and(|n| !there(n)) {
                    *node = None;
                }
            };
            let (mut selected, mut hovered) = (self.selected, self.hovered);
            gone(&mut selected);
            gone(&mut hovered);
            (self.selected, self.hovered) = (selected, hovered);
        }
        if list.is_some() {
            self.list = list;
        }
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selected
    }

    pub fn hovered(&self) -> Option<NodeId> {
        self.hovered
    }

    /// Selects `node` (or nothing) and unfolds what hides it. Answers its row, to scroll to.
    pub fn select(&mut self, node: Option<NodeId>) -> Option<usize> {
        self.selected = node;
        node.and_then(|node| self.tree.reveal(node))
    }

    /// The node the pointer is over, in the tree or on the page.
    pub fn hover(&mut self, node: Option<NodeId>) {
        self.hovered = node;
    }

    /// The computed style of the selected node.
    pub fn styles(&self) -> Vec<StyleLine> {
        match (self.tree.snapshot(), self.selected) {
            (Some(snapshot), Some(node)) => styles::computed(snapshot, node),
            _ => Vec::new(),
        }
    }

    /// The box model of the selected node, if the page has drawn a box for it.
    pub fn box_model(&self) -> Option<BoxModel> {
        let (snapshot, list) = (self.tree.snapshot()?, self.list.as_ref()?);
        boxmodel::of(snapshot, list, self.selected?, &|_| None)
    }

    /// What to draw over the page: the node under the pointer, else the selected one.
    pub fn highlight(&self) -> Option<Highlight> {
        let node = self.hovered.or(self.selected)?;
        let snapshot = self.tree.snapshot()?;
        let [margin, border, padding] = boxmodel::widths(snapshot, self.list.as_deref(), node);
        Some(Highlight {
            node,
            margin,
            border,
            padding,
        })
    }

    /// The page is showing another document (it navigated, or reloaded the same address): what
    /// was chosen, selected or asked about was about the old one's nodes. The input line and its
    /// history stay.
    pub fn new_document(&mut self) {
        self.tree.reset();
        self.console.clear();
        self.picking = false;
        self.selected = None;
        self.hovered = None;
        self.list = None;
    }
}

#[cfg(test)]
mod tests {
    use verve_dom::Rect;
    use verve_layout::{Cursor, PaintItem};

    use super::*;
    use crate::inspector::tree::tests::page;

    fn list_of(node: NodeId, bounds: Rect) -> Arc<DisplayList> {
        Arc::new(DisplayList {
            revision: 1,
            items: vec![PaintItem::Hitbox {
                node,
                bounds,
                cursor: Cursor::Auto,
                app_region: None,
            }],
            ..DisplayList::default()
        })
    }

    #[test]
    fn a_selected_node_has_styles_a_box_and_a_highlight() {
        let (_runtime, document) = page("<div id=a style='padding: 3px'>x</div>");
        let update = document.update();
        let div = update
            .snapshot
            .dom
            .query(update.snapshot.dom.body, "#a")
            .expect("div");
        let mut model = Model::new();
        model.page(
            Some(update),
            Some(list_of(div, Rect::new(8., 8., 100., 20.))),
        );
        assert!(model.styles().is_empty());
        assert!(model.highlight().is_none());
        let row = model.select(Some(div)).expect("a row");
        assert_eq!(model.tree.rows()[row].node, div);
        assert!(
            model
                .styles()
                .iter()
                .any(|l| l.name == "padding-top" && l.value == "3px")
        );
        let boxes = model.box_model().expect("a box");
        assert_eq!(boxes.border_box, Rect::new(8., 8., 100., 20.));
        assert_eq!(boxes.content_box(), Some(Rect::new(11., 11., 94., 14.)));
        let highlight = model.highlight().expect("a highlight");
        assert_eq!((highlight.node, highlight.padding.left), (div, Some(3.)));
        // The node under the pointer is highlighted in its place.
        model.hover(Some(div + 1));
        assert_eq!(model.highlight().map(|h| h.node), Some(div + 1));
        model.hover(None);
        assert_eq!(model.highlight().map(|h| h.node), Some(div));
    }

    #[test]
    fn a_new_document_leaves_nothing_of_the_old_one() {
        let (_runtime, document) = page("<p id=a>x</p>");
        let update = document.update();
        let p = update
            .snapshot
            .dom
            .query(update.snapshot.dom.body, "#a")
            .expect("p");
        let mut model = Model::new();
        model.page(
            Some(update),
            Some(list_of(p, Rect::new(10., 10., 50., 20.))),
        );
        model.select(Some(p));
        model.hover(Some(p));
        model.console.ask(1, "1");
        model.new_document();
        assert_eq!((model.selected(), model.hovered()), (None, None));
        assert!(model.tree.rows().is_empty() && model.console.lines().is_empty());
        assert!(model.box_model().is_none() && model.highlight().is_none());
        assert!(!model.console.answer(1, "1".into(), false));
    }

    #[test]
    fn a_selected_node_that_leaves_the_document_is_deselected() {
        let (mut runtime, document) = page("<p id=a>x</p><p id=b>y</p>");
        let update = document.update();
        let a = update
            .snapshot
            .dom
            .query(update.snapshot.dom.body, "#a")
            .expect("a");
        let mut model = Model::new();
        model.page(Some(update), None);
        model.select(Some(a));
        model.hover(Some(a));
        runtime
            .eval("document.getElementById('a').remove()")
            .expect("script");
        model.page(Some(document.update()), None);
        assert_eq!(model.selected(), None);
        assert_eq!(model.hovered(), None);
        assert!(model.highlight().is_none());
    }
}
