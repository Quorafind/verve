//! A page's boxes: laid out on the page's own thread, where its scripts run, so a script that
//! changes the document and reads its geometry on the next line is answered from the new layout.
//!
//! [`PageLayout`] is the layout provider of the page's document ([`Document::set_layout_provider`])
//! and the owner of the [`LayoutEngine`], which also keeps the display list the shell draws and
//! does the scrolling scripts ask for.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use verve::Runtime;
use verve_dom::{Document, FontData, Layout, NodeId, Request, ScrollRequest};
use verve_layout::{DisplayList, LayoutEngine, TextMeasurer};

/// The layout of one page.
pub(crate) struct PageLayout {
    engine: Rc<RefCell<LayoutEngine>>,
    /// The revision of the display list the shell was last given.
    published: u64,
    /// The field that had the focus when the page was last published.
    focus: Option<NodeId>,
    /// How far a one-line field was scrolled when it lost the focus, by field.
    parked: HashMap<NodeId, f32>,
}

impl PageLayout {
    /// Lays `document` out from now on, measuring text with `measurer`: the shell's over GPUI's
    /// text system, or the headless one.
    pub(crate) fn install(document: &Document, measurer: Arc<dyn TextMeasurer>) -> Self {
        let engine = Rc::new(RefCell::new(LayoutEngine::new(Arc::clone(&measurer))));
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
                svg_boxes: engine.svg_boxes().collect(),
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
        let fields = Rc::clone(&engine);
        document.set_field_position_provider(Some(Box::new(move |field, x, y| {
            fields.borrow().field_offset_at(field, x, y)
        })));
        // The bytes of a font the page declared go to the measurer; the layout then takes the face
        // by its family, and the document is laid out again.
        let fonts = Rc::clone(&engine);
        document.set_font_provider(Some(Box::new(move |font: FontData| {
            let FontData {
                family,
                weight,
                italic,
                bytes,
            } = font;
            if !measurer.add_face(&family, weight, italic, bytes) {
                return false;
            }
            fonts.borrow_mut().register_font(&family, weight, italic);
            true
        })));
        Self {
            engine,
            published: 0,
            focus: None,
            parked: HashMap::new(),
        }
    }

    /// Shows the document's selection as it is now: the display list has it from here on.
    pub(crate) fn show_selection(&self, runtime: &mut Runtime, document: &Document) {
        let selection = document.selection_boundaries(runtime);
        self.engine.borrow_mut().set_selection(selection);
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
        let moved = self.engine.borrow_mut().apply_scroll(request);
        self.settle_scroll(runtime, document, moved);
    }

    /// Tells the document the boxes that a scroll moved.
    fn settle_scroll(
        &self,
        runtime: &mut Runtime,
        document: &Document,
        moved: Vec<(NodeId, [f32; 2])>,
    ) {
        let rects = self.engine.borrow().rects().collect::<Vec<_>>();
        document.set_layout(runtime, rects);
        document.set_scroll_offsets(runtime, moved);
    }

    /// An `<input>` that loses the focus is scrolled back to its start and gets its scroll back
    /// when it has the focus again; a `<textarea>` keeps its scroll. Observed in Electron
    /// 39.8.10 (`p32c-review/fld-out.json`, `fld3-out.json`): `scrollLeft` went 140 to 0 on blur
    /// and back to 140 on a script's `focus()`, and a text area's `scrollTop` stayed 52.
    pub(crate) fn follow_focus(&mut self, runtime: &mut Runtime, document: &Document) {
        let now = document.focused();
        let before = std::mem::replace(&mut self.focus, now);
        if before == now {
            return;
        }
        let one_line =
            |node: NodeId| document.dom().get(node).and_then(|n| n.tag()) == Some("input");
        let mut moved = Vec::new();
        if let Some(old) = before.filter(|old| one_line(*old)) {
            let from = self.engine.borrow().scroll_offset(old)[0];
            if from != 0. {
                self.parked.insert(old, from);
                let to_start = ScrollRequest::To {
                    node: old,
                    left: Some(0.),
                    top: None,
                };
                moved.extend(self.engine.borrow_mut().apply_scroll(&to_start));
            }
        }
        if let Some(back) = now.and_then(|new| Some((new, self.parked.remove(&new)?))) {
            let request = ScrollRequest::To {
                node: back.0,
                left: Some(back.1),
                top: None,
            };
            moved.extend(self.engine.borrow_mut().apply_scroll(&request));
        }
        if !moved.is_empty() {
            self.settle_scroll(runtime, document, moved);
        }
    }

    /// Scrolls the focused text field, if there is one, until its caret shows: after a key, text,
    /// composition or press that may have moved it.
    pub(crate) fn follow_caret(&self, runtime: &mut Runtime, document: &Document) {
        let Some(field) = document.focused() else {
            return;
        };
        document.ensure_layout();
        let moved = self.engine.borrow_mut().reveal_caret(field);
        if !moved.is_empty() {
            self.settle_scroll(runtime, document, moved);
        }
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
