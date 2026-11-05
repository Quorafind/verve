//! The boxes a page has when no drawer measured them: `getBoundingClientRect`, `ResizeObserver`
//! and `IntersectionObserver` need rectangles, and a headless shell has no GPUI to make them.
//!
//! [`FakeLayout::BlockStack`] is the rule everything is written against, simple enough to
//! compute in the head: a block element takes the whole width it is in and the height of what it
//! holds, stacked one under the other, and text is 16 px a line, 8 px a character that is not
//! whitespace (half an em of 16 px, which P30's headless text measurer will also say). Margins,
//! padding, borders and explicit sizes are ignored, and an element with no text is as wide as
//! nothing.

use verve_dom::{NodeId, Rect, Snapshot};

/// The height of a line of text.
const LINE: f32 = 16.;
/// The width of a character.
const CHAR: f32 = 8.;

/// How a headless shell works out where a page's elements are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FakeLayout {
    /// Block elements stacked, text of fixed size; the rule above. P30 adds `Engine`, the real
    /// layout over a measurer of the same character width, and it replaces this.
    #[default]
    BlockStack,
}

impl FakeLayout {
    /// Every rendered element's border box in viewport coordinates, for a page as wide as the
    /// snapshot's viewport.
    pub(crate) fn boxes(self, snapshot: &Snapshot) -> Vec<(NodeId, Rect)> {
        let Self::BlockStack = self;
        let mut flow = Flow {
            snapshot,
            boxes: Vec::new(),
        };
        flow.block(snapshot.dom.html, 0., 0., snapshot.viewport.0);
        flow.boxes
    }
}

/// What a child is to the block it is in.
enum Role {
    /// Not drawn: `display: none`, comments, doctypes.
    Skipped,
    /// Text, or an element laid along a line.
    Inline,
    Block,
}

struct Flow<'a> {
    snapshot: &'a Snapshot,
    boxes: Vec<(NodeId, Rect)>,
}

impl Flow<'_> {
    /// Lays out the block `node` at `(x, y)`, `width` wide, and answers its height.
    fn block(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let mut cursor = y;
        let mut run = Vec::new();
        for &child in self.snapshot.dom.children(node) {
            match self.role(child) {
                Role::Skipped => {}
                Role::Inline => run.push(child),
                Role::Block => {
                    cursor += self.line_run(&mut run, x, cursor, width);
                    cursor += self.block(child, x, cursor, width);
                }
            }
        }
        cursor += self.line_run(&mut run, x, cursor, width);
        self.boxes.push((node, Rect::new(x, y, width, cursor - y)));
        cursor - y
    }

    /// Lays out the inline `run` of a block as text that wraps, answering its height. Each
    /// element in it is as wide as its text and one line high; one that wraps is not split.
    fn line_run(&mut self, run: &mut Vec<NodeId>, x: f32, y: f32, width: f32) -> f32 {
        let mut offset = 0.;
        for node in run.drain(..) {
            self.inline(node, (x, y), width, &mut offset);
        }
        if offset == 0. {
            return 0.;
        }
        (offset / width.max(CHAR)).ceil().max(1.) * LINE
    }

    /// Advances `offset`, the width of text laid so far, over `node`, boxing the elements.
    fn inline(&mut self, node: NodeId, at: (f32, f32), width: f32, offset: &mut f32) {
        let dom = &self.snapshot.dom;
        let Some(data) = dom.get(node) else {
            return;
        };
        if data.is_text() {
            let words = data.data().unwrap_or_default().split_whitespace();
            *offset += words.map(|word| word.chars().count() as f32).sum::<f32>() * CHAR;
            return;
        }
        let start = *offset;
        for &child in dom.children(node) {
            if !matches!(self.role(child), Role::Skipped) {
                self.inline(child, at, width, offset);
            }
        }
        let left = start.min(width);
        let own = (*offset - start).min(width - left);
        self.boxes
            .push((node, Rect::new(at.0 + left, at.1, own, LINE)));
    }

    fn role(&self, node: NodeId) -> Role {
        let Some(data) = self.snapshot.dom.get(node) else {
            return Role::Skipped;
        };
        if data.is_text() {
            return Role::Inline;
        }
        if !data.is_element() {
            return Role::Skipped;
        }
        // A style the sheets never set is the initial one, `inline`.
        match self.snapshot.style(node, "display") {
            Some("none") => Role::Skipped,
            None => Role::Inline,
            Some(display) if display.starts_with("inline") => Role::Inline,
            Some(_) => Role::Block,
        }
    }
}
