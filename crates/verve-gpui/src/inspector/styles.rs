//! The computed style of a node, as the inspector lists it: every property the page's cascade
//! gave it, by name.

use verve_dom::{NodeId, Snapshot};

/// One property and its computed value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleLine {
    pub name: String,
    pub value: String,
}

/// The properties of `node` in the order of their names; none for a node the cascade has not
/// styled (a text node, a node that left the document).
pub fn computed(snapshot: &Snapshot, node: NodeId) -> Vec<StyleLine> {
    let Some(style) = snapshot.styles.style(node) else {
        return Vec::new();
    };
    let mut lines: Vec<StyleLine> = style
        .iter()
        .map(|(name, value)| StyleLine {
            name: name.clone(),
            value: value.clone(),
        })
        .collect();
    lines.sort_by(|a, b| a.name.cmp(&b.name));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::tree::tests::page;

    #[test]
    fn a_nodes_properties_are_listed_by_name_with_their_computed_values() {
        let (_runtime, document) = page("<div id=a style='margin-top: 4px; color: red'>x</div>");
        let snapshot = document.update().snapshot;
        let div = snapshot
            .dom
            .query(snapshot.dom.body, "#a")
            .expect("the div");
        let lines = computed(&snapshot, div);
        let names: Vec<&str> = lines.iter().map(|l| l.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        let value = |name: &str| {
            let line = lines.iter().find(|l| l.name == name);
            line.map(|l| l.value.as_str())
        };
        assert_eq!(value("margin-top"), Some("4px"));
        assert_eq!(value("display"), Some("block"));
        assert!(value("color").is_some(), "{lines:?}");
    }

    #[test]
    fn a_node_that_is_not_in_the_document_has_no_style() {
        let (_runtime, document) = page("<p>x</p>");
        let snapshot = document.update().snapshot;
        assert!(computed(&snapshot, 0x00ff_ffff).is_empty());
    }
}
