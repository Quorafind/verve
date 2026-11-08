//! The link under the pointer: what `update-target-url` reports.

use verve_dom::{NodeId, Snapshot};

/// The `href` of the link `node` is in (or is), as written: no address is made of it.
pub(crate) fn href_of(snapshot: &Snapshot, node: NodeId) -> Option<&str> {
    let dom = &snapshot.dom;
    let mut at = Some(node);
    while let Some(id) = at {
        let link = dom
            .get(id)
            .filter(|n| matches!(n.tag(), Some("a" | "area")));
        if let Some(href) = link.and_then(|n| n.attr("href")) {
            return Some(href);
        }
        at = dom.parent(id);
    }
    None
}

/// The address of the link `node` is in (or is), resolved against the page's base, as the status
/// bubble of a browser shows it. `None` outside every `a`/`area` with an `href`.
pub(crate) fn of(snapshot: &Snapshot, node: NodeId) -> Option<String> {
    href_of(snapshot, node).map(|href| resolve(&snapshot.base_url, href))
}

/// `href` against `base`; the text as it is when the two do not make an address.
fn resolve(base: &str, href: &str) -> String {
    let joined = url::Url::parse(base).and_then(|base| base.join(href));
    joined.map_or_else(|_| href.to_owned(), String::from)
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn a_relative_address_is_resolved_against_the_pages() {
        assert_eq!(
            resolve("https://example.test/a/b.html", "../x?y=1#z"),
            "https://example.test/x?y=1#z"
        );
        assert_eq!(resolve("", "/x"), "/x");
        assert_eq!(
            resolve("https://e.test/", "mailto:a@b.test"),
            "mailto:a@b.test"
        );
    }
}
