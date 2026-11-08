//! What the layout tests share: a page with a document, laid out at 800 by 600 with the
//! headless measurer, and the comparisons the tests make.

#![allow(dead_code)]

use std::sync::Arc;

use verve::Runtime;
use verve_dom::{Document, NodeId, Options, ScrollRequest, Snapshot};
use verve_layout::{DisplayList, HeadlessMeasurer, LayoutEngine, PaintItem};

/// How close a rectangle has to be: Chromium lays out in 1/64 of a pixel, taffy in `f32`.
const TOLERANCE: f32 = 0.02;

/// A page laid out.
pub struct Laid {
    pub runtime: Runtime,
    pub document: Document,
    pub engine: LayoutEngine,
    pub snapshot: Snapshot,
}

/// Runs `f` on a thread with a stack Boa can recurse in, as Verve's own isolates do.
pub fn on_page<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    const STACK: usize = 64 << 20;
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            verve::set_thread_stack(STACK);
            f()
        })
        .expect("a thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// A page whose body is `html` (set the way a script sets it), then `script` run, laid out in an
/// 800 by 600 window.
pub fn lay_out(html: &str, script: &str) -> Laid {
    let mut runtime = Runtime::new();
    let document = verve_dom::install(
        &mut runtime,
        Options {
            viewport: (800., 600.),
            ..Options::default()
        },
    )
    .expect("the document installs");
    let html = serde_json::to_string(html).expect("a string");
    eval(&mut runtime, &format!("document.body.innerHTML = {html};"));
    if !script.is_empty() {
        eval(&mut runtime, script);
    }
    let update = document.update();
    let mut engine = LayoutEngine::new(Arc::new(HeadlessMeasurer));
    engine.update(&update.snapshot, &update.changes);
    Laid {
        runtime,
        document,
        engine,
        snapshot: update.snapshot,
    }
}

fn eval(runtime: &mut Runtime, source: &str) {
    if let Err(error) = runtime.eval(source) {
        panic!("script failed: {error}");
    }
}

impl Laid {
    /// The element with `id`, shadow trees included.
    pub fn node(&self, id: &str) -> NodeId {
        let dom = &self.snapshot.dom;
        if id == "html" {
            return dom.html;
        }
        let mut roots = vec![dom.document];
        let mut seen = 0;
        while seen < roots.len() {
            let root = roots[seen];
            seen += 1;
            for node in dom.descendants(root) {
                if dom.get(node).and_then(|n| n.attr("id")) == Some(id) {
                    return node;
                }
                if let Some(shadow) = dom.shadow_root(node) {
                    roots.push(shadow);
                }
            }
        }
        panic!("no element with id {id}");
    }

    /// The id of a node, or its tag when it has none.
    pub fn name(&self, node: NodeId) -> String {
        let found = self.snapshot.dom.get(node).expect("a node");
        found
            .attr("id")
            .map(str::to_owned)
            .or_else(|| found.tag().map(str::to_owned))
            .unwrap_or_default()
    }

    /// `[x, y, width, height]` of an element's border box.
    pub fn rect(&self, id: &str) -> [f32; 4] {
        let rect = self
            .engine
            .rect(self.node(id))
            .unwrap_or_else(|| panic!("{id} has no box"));
        [rect.x, rect.y, rect.width, rect.height]
    }

    /// Every rectangle is where Chromium put it.
    pub fn expect_rects(&self, expected: &[(&str, [f32; 4])]) {
        let mut wrong = Vec::new();
        for (id, want) in expected {
            let got = self.rect(id);
            if got.iter().zip(want).any(|(a, b)| (a - b).abs() > TOLERANCE) {
                wrong.push(format!("{id}: got {got:?}, want {want:?}"));
            }
        }
        assert!(wrong.is_empty(), "rectangles differ:\n{}", wrong.join("\n"));
    }

    /// The element under each point is the one Chromium's `elementFromPoint` answered.
    pub fn expect_hits(&self, expected: &[(f32, f32, &str)]) {
        let mut wrong = Vec::new();
        for (x, y, want) in expected {
            let got = self.engine.hit_test(*x, *y).map(|node| self.name(node));
            if got.as_deref() != Some(*want) {
                wrong.push(format!("({x}, {y}): got {got:?}, want {want}"));
            }
        }
        assert!(wrong.is_empty(), "hit tests differ:\n{}", wrong.join("\n"));
    }

    /// `[scrollWidth, scrollHeight]`.
    pub fn expect_extent(&self, id: &str, expected: [f32; 2]) {
        let got = self.engine.scroll_extent(self.node(id)).expect("a box");
        assert_eq!(got, expected, "scroll size of {id}");
    }

    /// Scrolls `id` to `(left, top)` as `scrollTo` does.
    pub fn scroll_to(&mut self, id: &str, left: f32, top: f32) -> Vec<(NodeId, [f32; 2])> {
        let node = self.node(id);
        self.engine.apply_scroll(&ScrollRequest::To {
            node,
            left: Some(left),
            top: Some(top),
        })
    }

    pub fn list(&self) -> Arc<DisplayList> {
        self.engine.display_list()
    }

    /// The items of the list that are about `id`.
    pub fn items_of(&self, id: &str) -> Vec<PaintItem> {
        let node = self.node(id);
        self.list()
            .items
            .iter()
            .filter(|item| match item {
                PaintItem::Box { node: n, .. }
                | PaintItem::Image { node: n, .. }
                | PaintItem::Hitbox { node: n, .. }
                | PaintItem::Scroller { node: n, .. } => *n == node,
                _ => false,
            })
            .cloned()
            .collect()
    }
}
