//! A page's boxes: laid out on the page's own thread, where its scripts run, so a script that
//! changes the document and reads its geometry on the next line is answered from the new layout.
//!
//! [`PageLayout`] is the layout provider of the page's document ([`Document::set_layout_provider`])
//! and the owner of the [`LayoutEngine`], which also keeps the display list the shell draws and
//! does the scrolling scripts ask for.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use verve::Runtime;
use verve_dom::{Document, Layout, Request, ScrollRequest};
use verve_layout::{DisplayList, HeadlessMeasurer, LayoutEngine};

/// The layout of one page.
pub(crate) struct PageLayout {
    engine: Rc<RefCell<LayoutEngine>>,
    /// The revision of the display list the shell was last given.
    published: u64,
}

impl PageLayout {
    /// Lays `document` out from now on, measuring text with the headless measurer: P31 brings
    /// the shell's real one.
    pub(crate) fn install(document: &Document) -> Self {
        let engine = Rc::new(RefCell::new(LayoutEngine::new(Arc::new(HeadlessMeasurer))));
        let provider = Rc::clone(&engine);
        document.set_layout_provider(Some(Box::new(move |update| {
            let mut engine = provider.borrow_mut();
            engine.update(&update.snapshot, &update.changes);
            Layout {
                rects: engine.rects().collect(),
                scroll_offsets: engine.scroll_offsets().into_iter().collect(),
                scroll_extents: engine
                    .rects()
                    .filter_map(|(node, _)| Some((node, engine.scroll_extent(node)?)))
                    .collect(),
                fragments: engine.fragments().collect(),
            }
        })));
        // A range's boxes come from the same lines. The provider is asked after the layout is up
        // to date, and is not called while the engine is being updated.
        let ranges = Rc::clone(&engine);
        document.set_range_provider(Some(Box::new(move |start, end| {
            ranges.borrow().range_rects(start, end)
        })));
        let positions = Rc::clone(&engine);
        document.set_position_provider(Some(Box::new(move |x, y| {
            positions.borrow().text_position_at(x, y)
        })));
        Self {
            engine,
            published: 0,
        }
    }

    /// The display list of the document as it is laid out now, and whether it is not the one the
    /// shell was last given.
    pub(crate) fn list(&mut self) -> (Arc<DisplayList>, bool) {
        let list = self.engine.borrow().display_list();
        let new = list.revision != self.published;
        self.published = list.revision;
        (list, new)
    }

    /// Does the `scrolls` that `Reveal` makes here, where the boxes are: the boxes that moved
    /// are told to the document, which says `scroll` of them. The scrolls to a place or by an
    /// amount were done by the document itself, when the script asked, and the layout clamped
    /// them at its next read.
    pub(crate) fn reveal(
        &self,
        runtime: &mut Runtime,
        document: &Document,
        scrolls: Vec<ScrollRequest>,
    ) {
        for scroll in scrolls {
            if matches!(scroll, ScrollRequest::Reveal { .. }) {
                self.scroll(runtime, document, &scroll);
            }
        }
    }

    /// The wheel turned by `(dx, dy)` at `(x, y)`: the nearest container under the point that can
    /// move that way scrolls, and the next one out when it cannot.
    pub(crate) fn wheel(
        &self,
        runtime: &mut Runtime,
        document: &Document,
        at: [f32; 2],
        by: [f32; 2],
    ) {
        document.ensure_layout();
        let target = self
            .engine
            .borrow()
            .display_list()
            .wheel(at[0], at[1], by, &|_| None);
        if let Some((node, _)) = target {
            let request = ScrollRequest::By {
                node,
                left: by[0],
                top: by[1],
            };
            self.scroll(runtime, document, &request);
        }
    }

    fn scroll(&self, runtime: &mut Runtime, document: &Document, request: &ScrollRequest) {
        document.ensure_layout();
        let (moved, rects) = {
            let mut engine = self.engine.borrow_mut();
            let moved = engine.apply_scroll(request);
            (moved, engine.rects().collect::<Vec<_>>())
        };
        document.set_layout(runtime, rects);
        document.set_scroll_offsets(runtime, moved);
    }
}

/// The scrolls among `requests`, and the requests that are not scrolls.
pub(crate) fn split_scrolls(requests: Vec<Request>) -> (Vec<ScrollRequest>, Vec<Request>) {
    let mut scrolls = Vec::new();
    let mut rest = Vec::new();
    for request in requests {
        match request {
            Request::Scroll(scroll) => scrolls.push(scroll),
            other => rest.push(other),
        }
    }
    (scrolls, rest)
}
