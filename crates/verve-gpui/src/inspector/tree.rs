//! The element tree the inspector shows: the rows of the document's visible nodes, in order, which
//! a person folds and unfolds and which follow the page's changes without being rebuilt.
//!
//! The model is plain Rust: it reads a [`Snapshot`] and the [`Changes`] since the last one, and
//! knows nothing of GPUI or of a window.

use std::collections::{HashMap, HashSet};

use verve_dom::{Changes, Dom, NodeId, NodeKind, Snapshot};

/// The longest an attribute value or a text is written on a row.
const SHORT: usize = 60;

/// What a row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Element,
    Text,
    Comment,
    Doctype,
}

/// One line of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub node: NodeId,
    /// How many ancestors the row has in the tree.
    pub depth: usize,
    pub kind: RowKind,
    /// `<div id="main" class="wide">`, a quoted text, `<!-- … -->`.
    pub text: String,
    /// The node has children the tree shows, whether it is folded or not.
    pub has_children: bool,
    pub folded: bool,
}

/// The tags whose children are folded until a person unfolds them: what a page's head and its
/// scripts hold is rarely what is looked for.
const FOLDED_AT_FIRST: &[&str] = &["head", "script", "style", "svg", "template"];

/// The element tree of one page.
#[derive(Default)]
pub struct Tree {
    snapshot: Option<Snapshot>,
    rows: Vec<Row>,
    /// Where each node's row is.
    index: HashMap<NodeId, usize>,
    /// What a person chose, for the nodes they folded or unfolded.
    chosen: HashMap<NodeId, bool>,
}

impl Tree {
    /// The visible rows, top to bottom.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The snapshot the rows were made from.
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    /// The row of `node`, if the tree shows it.
    pub fn index_of(&self, node: NodeId) -> Option<usize> {
        self.index.get(&node).copied()
    }

    /// Forgets the document: a new one is coming, and what a person chose was about the nodes of
    /// the old.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Takes the page's newest snapshot and what changed since the last. Only the rows of the
    /// changed nodes are made again — a whole rebuild is what a new document needs (another
    /// address) or what is cheaper (most of the tree changed).
    pub fn apply(&mut self, snapshot: Snapshot, changes: &Changes) {
        let moved = self
            .snapshot
            .as_ref()
            .is_none_or(|old| old.url != snapshot.url);
        let most = changes.nodes.len() * 2 > self.rows.len().max(8);
        self.snapshot = Some(snapshot);
        for node in &changes.removed {
            self.chosen.remove(node);
        }
        if moved || most {
            self.rebuild();
            return;
        }
        let Some(mut dirty) = self.dirty_rows(changes) else {
            self.rebuild();
            return;
        };
        dirty.sort_unstable();
        dirty.dedup();
        // Last row first: a row made again moves only the rows after it, which are done.
        for at in dirty.into_iter().rev() {
            self.refresh(at);
        }
        self.reindex();
    }

    /// The rows the changed nodes make again, or `None` when only a rebuild will do. A node with
    /// no row (white space between tags, a node under the document) is not in the rows, but its
    /// parent's children are not what they were; a text node's data can add or take away its own
    /// row the same way.
    fn dirty_rows(&self, changes: &Changes) -> Option<Vec<usize>> {
        let dom = &self.snapshot.as_ref()?.dom;
        let mut dirty = Vec::new();
        for &node in &changes.nodes {
            if !dom.exists(node) {
                continue;
            }
            let row = self.index.get(&node).copied();
            dirty.extend(row);
            let is_text = matches!(dom.get(node).map(|n| &n.kind), Some(NodeKind::Text(_)));
            if row.is_some() && !is_text {
                continue;
            }
            match dom.parent(node) {
                // Under the document, a change in the list of children is the tree's top level.
                _ if node == dom.document => return None,
                Some(parent) if parent == dom.document => return None,
                Some(parent) => dirty.extend(self.index.get(&parent).copied()),
                None => {}
            }
        }
        Some(dirty)
    }

    /// Whether the children of `node` are folded now.
    pub fn is_folded(&self, node: NodeId) -> bool {
        match self.chosen.get(&node) {
            Some(folded) => *folded,
            None => self
                .snapshot
                .as_ref()
                .is_some_and(|s| folded_at_first(&s.dom, node)),
        }
    }

    /// Folds or unfolds `node`.
    pub fn set_folded(&mut self, node: NodeId, folded: bool) {
        self.chosen.insert(node, folded);
        if let Some(at) = self.index_of(node) {
            self.refresh(at);
            self.reindex();
        }
    }

    /// Folds `node` if it is unfolded, and the other way round.
    pub fn toggle(&mut self, node: NodeId) {
        let folded = self.is_folded(node);
        self.set_folded(node, !folded);
    }

    /// Unfolds what keeps `node` out of sight; answers its row.
    pub fn reveal(&mut self, node: NodeId) -> Option<usize> {
        let snapshot = self.snapshot.clone()?;
        if !snapshot.dom.exists(node) {
            return None;
        }
        let mut hidden = false;
        let mut at = snapshot.dom.parent(node);
        while let Some(ancestor) = at {
            if self.is_folded(ancestor)
                && snapshot.dom.get(ancestor).is_some_and(|n| n.is_element())
            {
                self.chosen.insert(ancestor, false);
                hidden = true;
            }
            at = snapshot.dom.parent(ancestor);
        }
        if hidden {
            self.rebuild();
        }
        self.index_of(node)
    }

    /// The row after `at`, or before it, that is shown: for the arrow keys.
    pub fn neighbour(&self, node: NodeId, step: isize) -> Option<NodeId> {
        let at = self.index_of(node)?.checked_add_signed(step)?;
        self.rows.get(at).map(|row| row.node)
    }

    fn rebuild(&mut self) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let known: HashSet<NodeId> = (snapshot.dom.descendants(snapshot.dom.document))
            .into_iter()
            .collect();
        self.chosen.retain(|node, _| known.contains(node));
        let mut rows = Vec::new();
        for child in shown_children(&snapshot.dom, snapshot.dom.document) {
            self.emit(&snapshot, child, 0, &mut rows);
        }
        self.rows = rows;
        self.reindex();
    }

    fn reindex(&mut self) {
        self.index = (self.rows.iter().enumerate())
            .map(|(at, row)| (row.node, at))
            .collect();
    }

    /// Makes the row at `at` and what is under it again from the snapshot. A row whose children
    /// are the same ones as before keeps their rows: they are made again by their own change.
    fn refresh(&mut self, at: usize) {
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        let Some(row) = self.rows.get(at) else {
            return;
        };
        let (node, depth) = (row.node, row.depth);
        let end = self.subtree_end(at);
        if !snapshot.dom.exists(node) {
            return;
        }
        let kids = shown_children(&snapshot.dom, node);
        let folded = self.is_folded(node);
        let shown_now: Vec<NodeId> = (self.rows[at + 1..end].iter())
            .filter(|row| row.depth == depth + 1)
            .map(|row| row.node)
            .collect();
        let same_children = if folded {
            shown_now.is_empty()
        } else {
            shown_now == kids
        };
        if same_children {
            self.rows[at] = self.row(&snapshot, node, depth);
            return;
        }
        let mut made = Vec::new();
        self.emit(&snapshot, node, depth, &mut made);
        self.rows.splice(at..end, made);
    }

    /// The row after the last one under row `at`.
    fn subtree_end(&self, at: usize) -> usize {
        let depth = self.rows[at].depth;
        let under = self.rows[at + 1..]
            .iter()
            .take_while(|row| row.depth > depth);
        at + 1 + under.count()
    }

    /// The rows of `node` and, unless it is folded, of everything under it.
    fn emit(&self, snapshot: &Snapshot, node: NodeId, depth: usize, out: &mut Vec<Row>) {
        let mut pending = vec![(node, depth)];
        while let Some((node, depth)) = pending.pop() {
            let row = self.row(snapshot, node, depth);
            let open = row.has_children && !row.folded;
            out.push(row);
            if open {
                let kids = shown_children(&snapshot.dom, node);
                pending.extend(kids.into_iter().rev().map(|kid| (kid, depth + 1)));
            }
        }
    }

    fn row(&self, snapshot: &Snapshot, node: NodeId, depth: usize) -> Row {
        let dom = &snapshot.dom;
        let has_children = !shown_children(dom, node).is_empty();
        let folded = has_children && self.is_folded(node);
        let (kind, mut text) = describe(dom, node);
        if folded && kind == RowKind::Element {
            let tag = dom.get(node).and_then(|n| n.tag()).unwrap_or("?");
            text.push_str(&format!("…</{tag}>"));
        }
        Row {
            node,
            depth,
            kind,
            text,
            has_children,
            folded,
        }
    }
}

/// Whether `node` starts folded, when nobody has chosen.
fn folded_at_first(dom: &Dom, node: NodeId) -> bool {
    let tag = dom.get(node).and_then(|n| n.tag());
    tag.is_some_and(|tag| FOLDED_AT_FIRST.contains(&tag))
}

/// The children of `node` the tree shows: elements, comments, a doctype and text that is not only
/// white space.
pub(crate) fn shown_children(dom: &Dom, node: NodeId) -> Vec<NodeId> {
    let shows = |child: &&NodeId| match dom.get(**child).map(|n| &n.kind) {
        Some(NodeKind::Element(_) | NodeKind::Comment(_) | NodeKind::Doctype(_)) => true,
        Some(NodeKind::Text(text)) => !text.trim().is_empty(),
        _ => false,
    };
    dom.children(node).iter().filter(shows).copied().collect()
}

/// The kind of a node's row and its text.
fn describe(dom: &Dom, node: NodeId) -> (RowKind, String) {
    let Some(data) = dom.get(node) else {
        return (RowKind::Text, String::new());
    };
    match &data.kind {
        NodeKind::Element(tag) => {
            let mut text = format!("<{tag}");
            if let Some(id) = data.attr("id").filter(|id| !id.is_empty()) {
                text.push_str(&format!(" id=\"{}\"", short(id)));
            }
            if !data.classes.is_empty() {
                text.push_str(&format!(" class=\"{}\"", short(&data.classes.join(" "))));
            }
            // The classes are kept apart from the attributes, and written once above.
            let others = data
                .attrs
                .iter()
                .filter(|(name, _)| name != "id" && name != "class");
            for (name, value) in others {
                text.push_str(&format!(" {name}=\"{}\"", short(value)));
            }
            text.push('>');
            (RowKind::Element, text)
        }
        NodeKind::Text(text) => (RowKind::Text, format!("\"{}\"", short(text.trim()))),
        NodeKind::Comment(text) => (RowKind::Comment, format!("<!-- {} -->", short(text.trim()))),
        NodeKind::Doctype(name) => (RowKind::Doctype, format!("<!DOCTYPE {name}>")),
        NodeKind::Document | NodeKind::Fragment => (RowKind::Element, String::new()),
    }
}

/// `text` cut to [`SHORT`] characters, a line at a time.
fn short(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= SHORT {
        return flat;
    }
    let cut: String = flat.chars().take(SHORT).collect();
    format!("{cut}…")
}

#[cfg(test)]
pub(crate) mod tests {
    use verve::Runtime;
    use verve_dom::{Document, Options};

    use super::*;

    /// A document with `html` as its body, and the runtime that changes it.
    pub(crate) fn page(html: &str) -> (Runtime, Document) {
        let mut runtime = Runtime::new();
        let document = verve_dom::install(&mut runtime, Options::default()).expect("a document");
        let body = format!("document.body.innerHTML = {html:?};");
        runtime.eval(&body).expect("the page is set");
        (runtime, document)
    }

    /// The rows as indented text, without the doctype a document may begin with.
    fn texts(tree: &Tree) -> Vec<String> {
        let row = |row: &Row| format!("{}{}", "  ".repeat(row.depth), row.text);
        let rows = tree
            .rows()
            .iter()
            .filter(|row| row.kind != RowKind::Doctype);
        rows.map(row).collect()
    }

    /// The row whose text starts with `start`.
    fn find<'a>(tree: &'a Tree, start: &str) -> &'a Row {
        let found = tree.rows().iter().find(|row| row.text.starts_with(start));
        found.unwrap_or_else(|| panic!("no row starts with {start}: {:?}", texts(tree)))
    }

    fn load(document: &Document) -> Tree {
        let mut tree = Tree::default();
        tree.apply(document.update().snapshot, &Changes::default());
        tree
    }

    #[test]
    fn the_tree_lists_the_elements_text_and_comments_and_folds_the_head() {
        let (mut runtime, document) =
            page("<div id=main class='wide dark'><p>one</p><!--note--></div><span>two</span>");
        runtime
            .eval("document.head.innerHTML = '<title>t</title>'")
            .expect("script");
        let tree = load(&document);
        assert_eq!(
            texts(&tree),
            [
                "<html>",
                "  <head>…</head>",
                "  <body>",
                "    <div id=\"main\" class=\"wide dark\">",
                "      <p>",
                "        \"one\"",
                "      <!-- note -->",
                "    <span>",
                "      \"two\"",
            ]
        );
        let body = find(&tree, "<body>");
        assert!(body.has_children && !body.folded);
        assert_eq!(
            tree.rows()[tree.index_of(body.node).expect("a row")].node,
            body.node
        );
    }

    #[test]
    fn folding_hides_the_rows_under_a_node_and_unfolding_brings_them_back() {
        let (_runtime, document) = page("<div id=a><p>x</p><p>y</p></div><div id=b></div>");
        let mut tree = load(&document);
        let div = find(&tree, "<div id=\"a\"").node;
        let before = tree.rows().len();
        tree.toggle(div);
        assert!(tree.is_folded(div));
        assert_eq!(tree.rows().len(), before - 4);
        assert!(texts(&tree).contains(&"    <div id=\"a\">…</div>".to_owned()));
        tree.toggle(div);
        assert_eq!(tree.rows().len(), before);
        // The head starts folded, and a person unfolds it.
        let head = find(&tree, "<head>").node;
        assert!(tree.is_folded(head));
        tree.set_folded(head, false);
        assert!(!tree.is_folded(head));
    }

    #[test]
    fn a_change_makes_again_the_rows_of_the_changed_nodes_only() {
        let (mut runtime, document) = page("<ul id=list><li>a</li><li>b</li></ul><p id=p>x</p>");
        let mut tree = load(&document);
        let before = tree.rows().to_vec();
        // An attribute: the row's text changes and the others are the same rows.
        runtime
            .eval("document.getElementById('p').className = 'hot'")
            .expect("script");
        let update = document.update();
        tree.apply(update.snapshot, &update.changes);
        let after = texts(&tree);
        assert!(
            after.contains(&"    <p id=\"p\" class=\"hot\">".to_owned()),
            "{after:?}"
        );
        assert_eq!(tree.rows().len(), before.len());
        // A child added: a row appears under the list, in its place.
        runtime
            .eval("document.getElementById('list').appendChild(document.createElement('li'))")
            .expect("script");
        let update = document.update();
        tree.apply(update.snapshot, &update.changes);
        let after = texts(&tree);
        assert_eq!(tree.rows().len(), before.len() + 1);
        let list = after
            .iter()
            .position(|t| t.contains("id=\"list\""))
            .expect("list");
        assert_eq!(after[list + 5], "      <li>", "{after:?}");
        // A node removed: its rows go.
        runtime
            .eval("document.getElementById('list').remove()")
            .expect("script");
        let update = document.update();
        tree.apply(update.snapshot, &update.changes);
        assert!(!texts(&tree).iter().any(|t| t.contains("<ul")));
    }

    /// Runs `script` on a page, applies the update to a tree made before it, and compares the tree
    /// with one made from nothing.
    fn assert_incremental(html: &str, script: &str) {
        let (mut runtime, document) = page(html);
        let mut tree = load(&document);
        runtime.eval(script).expect("script");
        let update = document.update();
        tree.apply(update.snapshot, &update.changes);
        assert_eq!(tree.rows(), load(&document).rows(), "{script}");
    }

    #[test]
    fn an_incremental_update_comes_to_the_tree_a_rebuild_makes() {
        let many = "<div><p>x</p></div>".repeat(8);
        assert_incremental(
            &many,
            "const d = document.querySelectorAll('div')[3]; d.appendChild(document.createElement('b')); d.firstChild.textContent = 'changed';",
        );
        // White space gets text, and text becomes white space (what Svelte's `set_data` does).
        assert_incremental(
            &format!("{many}<section id=s> </section><section id=t>x</section>"),
            "document.getElementById('s').firstChild.data = 'now'; document.getElementById('t').firstChild.data = ' ';",
        );
        // `<html>` replaced: its parent is the document, which has no row.
        assert_incremental(
            &many,
            "const html = document.createElement('html'); html.innerHTML = '<body><p>new</p></body>'; document.replaceChild(html, document.documentElement);",
        );
        // A node moved between two parents.
        assert_incremental(
            &many,
            "const [a, b] = document.querySelectorAll('div'); b.appendChild(a.firstChild);",
        );
    }

    #[test]
    fn a_node_folded_by_a_person_stays_folded_through_changes() {
        let (mut runtime, document) = page("<div id=a><p>x</p></div><div id=b><p>y</p></div>");
        let mut tree = load(&document);
        let div = find(&tree, "<div id=\"a\"").node;
        tree.set_folded(div, true);
        runtime
            .eval("document.getElementById('b').className = 'c'")
            .expect("script");
        let update = document.update();
        tree.apply(update.snapshot, &update.changes);
        assert!(tree.is_folded(div));
        assert!(texts(&tree).contains(&"    <div id=\"a\">…</div>".to_owned()));
    }

    #[test]
    fn revealing_a_node_unfolds_what_hides_it() {
        let (_runtime, document) = page("<div id=a><p id=deep>x</p></div>");
        let mut tree = load(&document);
        let snapshot = tree.snapshot().expect("a snapshot").clone();
        let deep = snapshot
            .dom
            .query(snapshot.dom.body, "#deep")
            .expect("deep");
        let div = snapshot.dom.parent(deep).expect("parent");
        tree.set_folded(div, true);
        assert!(tree.index_of(deep).is_none());
        let at = tree.reveal(deep).expect("a row");
        assert_eq!(tree.rows()[at].node, deep);
        assert!(!tree.is_folded(div));
    }

    #[test]
    fn a_long_value_is_cut() {
        let (_runtime, document) = page(&format!("<a href='{}'>x</a>", "y".repeat(200)));
        let tree = load(&document);
        let link = find(&tree, "<a ");
        assert!(
            link.text.contains('…') && link.text.chars().count() < 100,
            "{}",
            link.text
        );
    }
}
