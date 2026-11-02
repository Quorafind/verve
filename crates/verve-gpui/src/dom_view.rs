//! One window's page: the text of the document's text nodes, in the order the document has them.
//!
//! This is P02's whole drawing. It reads nothing of the styles but `display: none` and lays
//! nothing out: P30 replaces it with the display list `verve-layout` makes on the page's own
//! thread, and P31 with the shaped lines `TextSystem` gives.

use gpui::{Context, IntoElement, ParentElement, Render, Styled, Window, div};
use verve_dom::Snapshot;

/// What a window draws: the page as it was last published.
pub struct DomView {
    snapshot: Option<Snapshot>,
}

impl DomView {
    /// A view with nothing to draw yet.
    pub fn new() -> Self {
        Self { snapshot: None }
    }

    /// Draws `snapshot` from the next frame on.
    pub fn show(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(snapshot);
    }
}

impl Default for DomView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for DomView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let Some(snapshot) = &self.snapshot else {
            return div();
        };
        let mut page = div().flex().flex_col().size_full();
        for text in texts(snapshot) {
            page = page.child(div().flex_shrink_0().child(text));
        }
        page
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
