//! One window's page: the text of the document's text nodes, in the order the document has them.
//!
//! This is P02's whole drawing. It reads nothing of the styles but `display: none` and lays
//! nothing out: P30 replaces it with the display list `verve-layout` makes on the page's own
//! thread, and P31 with the shaped lines `TextSystem` gives.

use std::rc::Rc;

use gpui::{Context, IntoElement, ParentElement, Render, Rgba, Styled, Window, div, rgb};
use verve_dom::Snapshot;

use crate::app_region::{self, AppRegion};

/// What is called as each frame of a window is built.
type Drawn = Rc<dyn Fn(&Window)>;

/// What a window draws: the page as it was last published.
pub struct DomView {
    snapshot: Option<Snapshot>,
    /// What shows where the page has no background of its own: `None` is a transparent window.
    background: Option<Rgba>,
    regions: Vec<AppRegion>,
    /// Called as each frame is built: how the window knows its first one.
    drawn: Option<Drawn>,
}

impl DomView {
    /// A view with nothing to draw yet, over `background` (`None` for a window that is transparent).
    pub fn new(background: Option<Rgba>) -> Self {
        Self {
            snapshot: None,
            background,
            regions: Vec::new(),
            drawn: None,
        }
    }

    /// The colour under the page, `None` for none.
    pub fn set_background(&mut self, background: Option<Rgba>) {
        self.background = background;
    }

    /// The parts of the window that drag it: P30 hands them over as it lays the page out.
    pub fn set_app_regions(&mut self, regions: Vec<AppRegion>) {
        self.regions = regions;
    }

    /// Calls `drawn` each time a frame is built.
    pub fn on_draw(&mut self, drawn: Drawn) {
        self.drawn = Some(drawn);
    }

    /// Draws `snapshot` from the next frame on.
    pub fn show(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
    }
}

/// Electron's white: what a window shows until its page or its `backgroundColor` says otherwise.
pub fn white() -> Rgba {
    rgb(0x00ff_ffff)
}

impl Render for DomView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if let Some(drawn) = &self.drawn {
            drawn(window);
        }
        let mut page = div().flex().flex_col().size_full();
        if let Some(background) = self.background {
            page = page.bg(background);
        }
        for text in self.snapshot.iter().flat_map(texts) {
            page = page.child(div().flex_shrink_0().child(text));
        }
        page.child(app_region::layer(&self.regions))
    }
}

/// The text of every text node the document draws, in document order. Whitespace-only nodes are
/// not drawn: a browser's layout collapses them, which is P30's and P31's to do properly.
fn texts(snapshot: &Snapshot) -> Vec<String> {
    let dom = &snapshot.dom;
    dom.subtree(dom.document)
        .into_iter()
        .filter(|id| snapshot.is_rendered(*id))
        .filter(|id| dom.get(*id).is_some_and(|node| node.is_text()))
        .filter_map(|id| dom.get(id).and_then(|node| node.data()).map(str::to_owned))
        .filter(|text| !text.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page whose `<h1>` says hi and whose hidden `<p>` says nothing a person sees.
    fn page(markup: &str) -> Snapshot {
        let mut runtime = verve::Runtime::new();
        let document =
            verve_dom::install(&mut runtime, verve_dom::Options::default()).expect("a document");
        let root = document.document_node();
        let body = document.body();
        verve_dom::host::with_dom(runtime.context(), |dom| {
            dom.empty(body);
            verve_dom::html::parse_document(dom, root, markup);
        });
        document.snapshot()
    }

    #[test]
    fn the_texts_are_the_documents_in_order() {
        assert_eq!(
            texts(&page("<h1>hi</h1><p>there</p>")),
            vec!["hi".to_owned(), "there".to_owned()]
        );
    }

    #[test]
    fn a_hidden_subtree_is_not_drawn() {
        assert_eq!(
            texts(&page("<h1>hi</h1><p style=\"display: none\">gone</p>")),
            vec!["hi".to_owned()]
        );
    }
}
