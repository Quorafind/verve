//! Reading the children of a block into boxes and runs of inline content.
//!
//! A block's children are text, inline elements, atomic inlines (`inline-block`, `<img>`), block
//! boxes, and boxes taken out of the flow (floats and absolute boxes). When there are no block
//! boxes the block's own taffy node lays its lines out. When there are, each run of inline content
//! between them goes into an *anonymous* box of its own, as CSS says; in a flex or grid container
//! each such run is an item. A run that is nothing but collapsible white space makes no box.

use verve_dom::NodeId;
use verve_dom::css::{ContentItem, PseudoElement};

use crate::fonts::Platform;
use crate::grid::Areas;
use crate::inline::style::{self, Env, TextDecoration};
use crate::inline::{Collector, Plan, has_content};
use crate::props::{BoxProps, Display, ReadContext, read};
use crate::tree::{BoxNode, Builder};

/// One thing in the content of a block, before its boxes are made.
pub(crate) enum Step {
    /// Text, the start or end of an inline element, a break, generated text.
    Inline(Plan),
    /// An atomic inline: a box that sits in a line.
    Atomic(NodeId, Box<BoxProps>),
    /// A box of its own in the flow.
    Block(NodeId, Box<BoxProps>),
    /// A float or an out-of-flow box.
    Aside(NodeId, Box<BoxProps>),
}

impl Builder<'_> {
    /// Makes the boxes for what is in `node`, which is the box `owner`, and appends them to `out`.
    pub(crate) fn build_content(
        &mut self,
        node: NodeId,
        owner: usize,
        areas: Option<&Areas>,
        out: &mut Vec<usize>,
    ) {
        let flex = self.boxes[owner].props.display.is_flex_or_grid();
        let mut steps = Vec::new();
        self.generated(node, PseudoElement::Before, &mut steps);
        self.flatten(node, flex, areas, &mut steps);
        self.generated(node, PseudoElement::After, &mut steps);
        self.assemble(owner, node, steps, flex, out);
    }

    /// The steps for the children of `node`: transparent elements are opened up, inline elements
    /// become a start and an end around their content.
    fn flatten(&mut self, node: NodeId, flex: bool, areas: Option<&Areas>, steps: &mut Vec<Step>) {
        let snapshot = self.snapshot;
        for child in snapshot.dom.flat_children(node) {
            let Some(found) = snapshot.dom.get(child) else {
                continue;
            };
            if found.is_text() {
                steps.push(Step::Inline(Plan::Text {
                    node: child,
                    parent: node,
                }));
            } else if found.is_element() {
                self.element(child, flex, areas, steps);
            }
        }
    }

    fn element(&mut self, child: NodeId, flex: bool, areas: Option<&Areas>, steps: &mut Vec<Step>) {
        let snapshot = self.snapshot;
        let context = ReadContext {
            snapshot,
            root_font_size: self.root_font_size,
            parent_areas: areas,
            blockify: flex,
        };
        let mut props = read(&context, child);
        let is_slot = snapshot.dom.get(child).and_then(|n| n.tag()) == Some("slot");
        if is_slot && snapshot.style(child, "display").is_none() {
            props.display = Display::Contents;
        }
        self.unsupported += props.unsupported;
        let display = props.display;
        if display == Display::Inline {
            self.inline_element(child, flex, areas, steps);
        } else if display.is_transparent() {
            self.flatten(child, flex, areas, steps);
        } else if display.has_box() {
            let out_of_flow = props.position.is_out_of_flow() || props.float;
            let atomic = matches!(
                display,
                Display::InlineBlock | Display::InlineFlex | Display::InlineGrid
            );
            let boxed = Box::new(props);
            steps.push(match (out_of_flow, atomic) {
                (true, _) => Step::Aside(child, boxed),
                (false, true) => Step::Atomic(child, boxed),
                (false, false) => Step::Block(child, boxed),
            });
        }
    }

    fn inline_element(
        &mut self,
        node: NodeId,
        flex: bool,
        areas: Option<&Areas>,
        steps: &mut Vec<Step>,
    ) {
        let tag = self.snapshot.dom.get(node).and_then(|n| n.tag());
        match tag {
            Some("br") => steps.push(Step::Inline(Plan::Break)),
            Some("wbr") => steps.push(Step::Inline(Plan::WordBreak)),
            _ => {
                steps.push(Step::Inline(Plan::Open(node)));
                self.generated(node, PseudoElement::Before, steps);
                self.flatten(node, flex, areas, steps);
                self.generated(node, PseudoElement::After, steps);
                steps.push(Step::Inline(Plan::Close));
            }
        }
    }

    /// The text `::before` or `::after` of `node` generates, if it generates any.
    fn generated(&self, node: NodeId, part: PseudoElement, steps: &mut Vec<Step>) {
        let Some(pseudo) = self.snapshot.pseudo.get(&(node, part)) else {
            return;
        };
        if pseudo
            .style
            .get("display")
            .is_some_and(|display| display.trim() == "none")
        {
            return;
        }
        let mut text = String::new();
        for item in &pseudo.content {
            match item {
                ContentItem::Text(piece) => text.push_str(piece),
                ContentItem::OpenQuote => text.push('\u{201C}'),
                ContentItem::CloseQuote => text.push('\u{201D}'),
                ContentItem::Image(_) => {}
            }
        }
        if !text.is_empty() {
            steps.push(Step::Inline(Plan::Generated {
                owner: node,
                text,
                style: pseudo.style.clone(),
            }));
        }
    }

    /// Makes the boxes of the steps, cutting the inline content into runs at the block boxes.
    fn assemble(
        &mut self,
        owner: usize,
        owner_node: NodeId,
        steps: Vec<Step>,
        flex: bool,
        out: &mut Vec<usize>,
    ) {
        let inline_only = steps
            .iter()
            .all(|step| matches!(step, Step::Inline(_) | Step::Atomic(..)));
        if inline_only && !flex {
            // The block itself holds the lines.
            if self.has_content(&steps) {
                self.fill(owner, owner_node, steps, out);
            }
            return;
        }
        let mut run: Vec<Step> = Vec::new();
        let mut deferred: Vec<Step> = Vec::new();
        let mut open: Vec<NodeId> = Vec::new();
        for step in steps {
            match step {
                Step::Inline(Plan::Open(node)) => {
                    open.push(node);
                    run.push(Step::Inline(Plan::Open(node)));
                }
                Step::Inline(Plan::Close) => {
                    open.pop();
                    run.push(Step::Inline(Plan::Close));
                }
                Step::Inline(_) | Step::Atomic(..) => run.push(step),
                Step::Aside(..) if self.has_content(&run) => deferred.push(step),
                Step::Aside(node, props) => self.make_box(node, *props, Some(owner), out),
                Step::Block(node, props) => {
                    // The inline elements that are open end before the block and begin again
                    // after it.
                    run.extend(open.iter().map(|_| Step::Inline(Plan::Pause)));
                    let before = std::mem::take(&mut run);
                    self.flush(
                        owner,
                        owner_node,
                        before,
                        std::mem::take(&mut deferred),
                        out,
                    );
                    self.make_box(node, *props, Some(owner), out);
                    run.extend(open.iter().map(|n| Step::Inline(Plan::Resume(*n))));
                }
            }
        }
        self.flush(owner, owner_node, run, deferred, out);
    }

    /// Whether a run of steps makes lines.
    fn has_content(&self, run: &[Step]) -> bool {
        run.iter().any(|step| match step {
            Step::Atomic(..) => true,
            Step::Inline(plan) => has_content(self.snapshot, std::slice::from_ref(plan)),
            _ => false,
        })
    }

    /// Puts a run in an anonymous box of its own, followed by the boxes that waited for it.
    fn flush(
        &mut self,
        owner: usize,
        owner_node: NodeId,
        run: Vec<Step>,
        deferred: Vec<Step>,
        out: &mut Vec<usize>,
    ) {
        if self.has_content(&run) {
            let holder = self.anonymous_box(owner);
            let mut children = Vec::new();
            self.fill(holder, owner_node, run, &mut children);
            self.boxes[holder].children = children;
            out.push(holder);
        }
        for step in deferred {
            if let Step::Aside(node, props) = step {
                self.make_box(node, *props, Some(owner), out);
            }
        }
    }

    fn anonymous_box(&mut self, owner: usize) -> usize {
        let index = self.boxes.len();
        let node = self.boxes[owner].node;
        self.boxes.push(BoxNode {
            node,
            props: BoxProps::anonymous(),
            taffy: taffy::NodeId::new(0),
            parent: Some(owner),
            flat_parent: Some(owner),
            children: Vec::new(),
            ghost: None,
            follows_ghost: [false; 2],
            anonymous: true,
            atomic: false,
        });
        index
    }

    /// Makes the atomic inlines of a run, as children of `holder`, and collects the run into the
    /// inline content `holder` lays out. `owner_node` is the element whose style the run's text
    /// takes when nothing nearer says otherwise.
    fn fill(
        &mut self,
        holder: usize,
        owner_node: NodeId,
        steps: Vec<Step>,
        children: &mut Vec<usize>,
    ) {
        let snapshot = self.snapshot;
        let measurer = self.measurer.clone();
        let env = Env {
            measurer: &*measurer,
            snapshot,
            root_font_size: self.root_font_size,
            platform: Platform::current(),
            registry: self.fonts,
        };
        let mut collector = Collector::new(&env, owner_node, propagated(&env, owner_node));
        for step in steps {
            match step {
                Step::Inline(plan) => collector.plan(&plan),
                Step::Atomic(node, props) => {
                    let mut made = Vec::new();
                    self.make_box(node, *props, Some(holder), &mut made);
                    if let Some(&index) = made.first() {
                        self.boxes[index].atomic = true;
                        children.push(index);
                        collector.plan(&Plan::Atomic {
                            node,
                            box_index: index,
                        });
                    }
                }
                Step::Block(..) | Step::Aside(..) => {}
            }
        }
        let owner = &self.boxes[holder];
        let clips = owner.props.clips() || self.owner_clips(holder);
        let webkit_box = snapshot
            .style(owner_node, "display")
            .is_some_and(|display| display.trim().starts_with("-webkit-"));
        self.ifcs
            .push((holder, collector.finish(clips, webkit_box)));
    }

    /// Whether the block an anonymous box is in clips its overflow.
    fn owner_clips(&self, holder: usize) -> bool {
        self.boxes[holder]
            .parent
            .is_some_and(|parent| self.boxes[holder].anonymous && self.boxes[parent].props.clips())
    }
}

/// The decorations of the ancestors of `node` that are drawn through its text: `text-decoration`
/// goes on down through blocks, and stops at an atomic inline, a float and an absolute box.
fn propagated(env: &Env, node: NodeId) -> Vec<TextDecoration> {
    let snapshot = env.snapshot;
    let mut inner_first: Vec<Vec<TextDecoration>> = Vec::new();
    let mut at = node;
    while let Some(parent) = snapshot.dom.get(at).and_then(|found| found.parent) {
        let is_element = snapshot
            .dom
            .get(parent)
            .is_some_and(|found| found.is_element());
        if !is_element || blocks_decoration(env, at) {
            break;
        }
        inner_first.push(style::decorations_of(env, parent));
        at = parent;
    }
    inner_first.into_iter().rev().flatten().collect()
}

/// Whether the decorations of an element's parent stop at the element.
fn blocks_decoration(env: &Env, node: NodeId) -> bool {
    let snapshot = env.snapshot;
    let display = snapshot.style(node, "display").unwrap_or("").trim();
    let floats = !matches!(
        snapshot.style(node, "float").unwrap_or("none").trim(),
        "none" | ""
    );
    let out_of_flow = matches!(
        snapshot.style(node, "position").unwrap_or("static").trim(),
        "absolute" | "fixed"
    );
    display.starts_with("inline-") || floats || out_of_flow
}
