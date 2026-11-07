//! Collecting the content of an inline formatting context: the text of a block and what is in it
//! (inline elements, `<br>`, atomic inlines, generated text), white space processed and the text
//! shaped, in the order a line reads it.

use std::collections::HashMap;
use std::sync::Arc;

use verve_dom::css::Style;
use verve_dom::{NodeId, Snapshot};

use super::ifc::{Atom, Item, Parts, Span, TextItem, glyph_order, stops_of};
use super::space::{Flow, Processed, Segment, WhiteSpace, process};
use super::style::{self, Env, TextDecoration, TextStyle};

/// One thing the tree builder found in the content of a block, in document order.
#[derive(Clone, Debug)]
pub(crate) enum Plan {
    /// A text node, whose text takes the style of its `parent` element.
    Text { node: NodeId, parent: NodeId },
    /// What an element's `::before` or `::after` generates, in the style of the box.
    Generated {
        owner: NodeId,
        text: String,
        style: Arc<Style>,
    },
    /// The start of an inline element.
    Open(NodeId),
    /// The end of the inline element opened last.
    Close,
    /// An inline element goes on after a block box that split it (the part after the block): it
    /// has no start edge, as a box that is sliced has its start only on the first part.
    Resume(NodeId),
    /// An inline element is split by a block box: this part has no end edge.
    Pause,
    /// `<br>`.
    Break,
    /// `<wbr>`.
    WordBreak,
    /// An atomic inline, whose box is `box_index` in the tree.
    Atomic { node: NodeId, box_index: usize },
}

/// Whether text takes room in a block: it has something besides collapsible white space.
fn text_is_content(text: &str, white_space: WhiteSpace) -> bool {
    if text.chars().any(|ch| !ch.is_whitespace()) {
        return true;
    }
    match white_space {
        WhiteSpace::Normal | WhiteSpace::Nowrap => false,
        WhiteSpace::PreLine => text.contains('\n'),
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => !text.is_empty(),
    }
}

/// Whether a run of content makes lines: something is on them (text that is not collapsed away,
/// a `<br>`, an atomic inline, an inline element with margin, border or padding). A block holding a run that does not has no inline content.
pub(crate) fn has_content(snapshot: &Snapshot, plans: &[Plan]) -> bool {
    plans.iter().any(|plan| match plan {
        Plan::Atomic { .. } | Plan::Break => true,
        Plan::Generated { text, .. } => !text.is_empty(),
        Plan::Text { node, parent } => {
            let text = snapshot
                .dom
                .get(*node)
                .and_then(|found| found.data())
                .unwrap_or("");
            let mode = snapshot
                .style(*parent, "white-space")
                .map_or(WhiteSpace::Normal, WhiteSpace::parse);
            text_is_content(text, mode)
        }
        // An inline element with a margin, a border or padding takes room even when it is empty
        // (CSS 2.1 §9.4.2).
        Plan::Open(node) | Plan::Resume(node) => style::has_edges(snapshot, *node),
        Plan::Close | Plan::Pause | Plan::WordBreak => false,
    })
}

/// Collects the plans of one block into [`Parts`].
pub(crate) struct Collector<'a> {
    env: &'a Env<'a>,
    owner: NodeId,
    root: Arc<TextStyle>,
    /// What the ancestors of the block draw through its text.
    inherited: Vec<TextDecoration>,
    styles: HashMap<NodeId, Arc<TextStyle>>,
    flow: Flow,
    items: Vec<Item>,
    spans: Vec<Span>,
    atoms: Vec<Atom>,
    stack: Vec<usize>,
}

impl<'a> Collector<'a> {
    /// A collector for the block `owner`. `inherited` is the decorations its ancestors pass down.
    pub(crate) fn new(env: &'a Env<'a>, owner: NodeId, inherited: Vec<TextDecoration>) -> Self {
        let root = Arc::new(style::read(env, owner));
        let mut styles = HashMap::new();
        styles.insert(owner, root.clone());
        Self {
            env,
            owner,
            root,
            inherited,
            styles,
            flow: Flow::new(),
            items: Vec::new(),
            spans: Vec::new(),
            atoms: Vec::new(),
            stack: Vec::new(),
        }
    }

    pub(crate) fn plan(&mut self, plan: &Plan) {
        match plan {
            Plan::Text { node, parent } => self.text(*node, *parent),
            Plan::Generated { owner, text, style } => self.generated(*owner, text, style),
            Plan::Open(node) => self.open(*node, false),
            Plan::Resume(node) => self.open(*node, true),
            Plan::Close => self.close(false),
            Plan::Pause => self.close(true),
            Plan::Break => {
                self.flow.newline();
                self.items.push(Item::Break);
            }
            Plan::WordBreak => self.items.push(Item::WordBreak),
            Plan::Atomic { node, box_index } => self.atomic(*node, *box_index),
        }
    }

    /// The content, with the facts about the block that decide `text-overflow` and the clamp.
    pub(crate) fn finish(self, clips: bool, webkit_box: bool) -> Parts {
        Parts {
            node: self.owner,
            root: self.root,
            items: self.items,
            spans: self.spans,
            atoms: self.atoms,
            clips,
            webkit_box,
        }
    }

    fn style_of(&mut self, node: NodeId) -> Arc<TextStyle> {
        let env = self.env;
        self.styles
            .entry(node)
            .or_insert_with(|| Arc::new(style::read(env, node)))
            .clone()
    }

    /// The decorations drawn through text inside the open elements, not counting the innermost
    /// one (its own are the text's style's).
    fn passed_down(&self) -> Vec<TextDecoration> {
        let mut passed = self.inherited.clone();
        if self.stack.is_empty() {
            return passed;
        }
        passed.extend(self.root.decorations.iter().cloned());
        for span in &self.stack[..self.stack.len() - 1] {
            passed.extend(self.spans[*span].style.decorations.iter().cloned());
        }
        passed
    }

    /// Starts an inline element; when it `resumes` after a block, it has no start edge.
    fn open(&mut self, node: NodeId, resumes: bool) {
        let style = self.style_of(node);
        let mut edges = style::edges_of(self.env, node);
        if resumes {
            edges.cut(3);
        }
        self.spans.push(Span {
            node,
            parent: self.stack.last().copied(),
            style,
            edges,
            paint: style::span_paint(self.env, node),
        });
        let index = self.spans.len() - 1;
        self.stack.push(index);
        self.items.push(Item::Open(index));
    }

    /// Ends the inline element opened last; when a block `splits` it, this part has no end edge.
    fn close(&mut self, splits: bool) {
        if let Some(span) = self.stack.pop() {
            if splits {
                self.spans[span].edges.cut(1);
            }
            self.items.push(Item::Close(span));
        }
    }

    fn atomic(&mut self, node: NodeId, box_index: usize) {
        let style = self.style_of(node);
        self.atoms.push(Atom {
            node,
            box_index,
            style,
            margin: style::margins_of(self.env, node),
            size: [0.; 2],
            baseline: None,
            max_width: 0.,
            min_width: 0.,
            avail: None,
        });
        self.flow.atomic();
        self.items.push(Item::Atomic(self.atoms.len() - 1));
    }

    fn text(&mut self, node: NodeId, parent: NodeId) {
        let Some(source) = self
            .env
            .snapshot
            .dom
            .get(node)
            .and_then(|found| found.data())
        else {
            return;
        };
        let style = self.style_of(parent);
        let segments = process(
            source,
            style.white_space,
            style.transform,
            style.tab_size,
            &mut self.flow,
        );
        let mut segments = segments.into_iter().peekable();
        while let Some(segment) = segments.next() {
            match segment {
                Segment::Newline(_) => self.items.push(Item::Break),
                Segment::Text(processed) => {
                    // A text ends where the newline after it is, or at the end of the node.
                    let end = match segments.peek() {
                        Some(Segment::Newline(byte)) => *byte,
                        _ => source.len() as u32,
                    };
                    self.push_text(node, &style, processed, end, false);
                }
            }
        }
    }

    fn generated(&mut self, owner: NodeId, text: &str, style: &Arc<Style>) {
        let style = Arc::new(style::read_generated(self.env, owner, style));
        let segments = process(
            text,
            style.white_space,
            style.transform,
            style.tab_size,
            &mut self.flow,
        );
        for segment in segments {
            match segment {
                Segment::Newline(_) => self.items.push(Item::Break),
                Segment::Text(processed) => {
                    self.push_text(owner, &style, processed, 0, true);
                }
            }
        }
    }

    fn push_text(
        &mut self,
        node: NodeId,
        style: &Arc<TextStyle>,
        processed: Processed,
        source_len: u32,
        generated: bool,
    ) {
        let measurer = self.env.measurer;
        let run = measurer.shape(&processed.text, style.font, style.size);
        let chars: Vec<char> = processed.text.chars().collect();
        let (stops, shifts) = stops_of(
            &run,
            &processed,
            &chars,
            style.letter_spacing,
            style.word_spacing,
        );
        let paint = Arc::new(style.paint(&self.passed_down()));
        let order = glyph_order(&run);
        self.items.push(Item::Text(TextItem {
            node,
            generated,
            span: self.stack.last().copied(),
            style: style.clone(),
            paint,
            processed,
            chars,
            run,
            order,
            stops,
            shifts,
            source_len,
        }));
    }
}
