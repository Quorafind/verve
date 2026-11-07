//! Nodes written out as markup: HTML serialization (`innerHTML`, `outerHTML`) and XML
//! (`XMLSerializer`).

use super::{VOID, escape, qualified_name, raw_text};
use crate::dom::{Dom, Namespace, NodeId, NodeKind};

/// The node's children as markup (HTML serialization).
pub fn inner(dom: &Dom, id: NodeId) -> String {
    inner_in(dom, id, false)
}

/// The node's children as XML (`XMLSerializer`).
pub fn inner_xml(dom: &Dom, id: NodeId) -> String {
    inner_in(dom, id, true)
}

fn inner_in(dom: &Dom, id: NodeId, xml: bool) -> String {
    let mut out = String::new();
    let raw = dom
        .get(id)
        .and_then(|n| n.tag())
        .is_some_and(|t| raw_text(t) == Some(false));
    for child in dom.children(id) {
        write(dom, *child, &mut out, raw, xml);
    }
    out
}

/// The node itself as markup (HTML serialization).
pub fn outer(dom: &Dom, id: NodeId) -> String {
    let mut out = String::new();
    write(dom, id, &mut out, false, false);
    out
}

/// The node itself as XML (`XMLSerializer`).
pub fn outer_xml(dom: &Dom, id: NodeId) -> String {
    let mut out = String::new();
    write(dom, id, &mut out, false, true);
    out
}

/// Writes `id` out. HTML serialization (`xml` false) gives every element that is not void a start
/// and an end tag, SVG and MathML ones too; XML serialization writes an empty element outside the
/// HTML namespace as `<x/>` and says a namespace where it changes with an `xmlns` attribute.
/// Electron 39.8.10 (a hidden window, probe `SCRATCH\probe-svg`): `innerHTML` is
/// `<svg><circle></circle></svg><math><mi></mi></math>`, and `new XMLSerializer()` writes
/// `<svg xmlns="http://www.w3.org/2000/svg"><circle/></svg>`.
fn write(dom: &Dom, id: NodeId, out: &mut String, raw: bool, xml: bool) {
    // 结束标签也放入显式栈，树的深度不再占用原生调用栈。
    let mut stack = vec![(id, raw, false)];
    while let Some((id, raw, closing)) = stack.pop() {
        let Some(node) = dom.get(id) else { continue };
        if closing {
            out.push_str("</");
            out.push_str(&qualified_name(dom, id, false));
            out.push('>');
            continue;
        }
        match &node.kind {
            NodeKind::Text(text) if raw => out.push_str(text),
            NodeKind::Text(text) => out.push_str(&escape(text, false)),
            NodeKind::Comment(data) => {
                out.push_str("<!--");
                out.push_str(data);
                out.push_str("-->");
            }
            NodeKind::Doctype(name) => {
                out.push_str("<!DOCTYPE ");
                out.push_str(name);
                out.push('>');
            }
            NodeKind::Element(tag) => {
                let name = qualified_name(dom, id, false);
                out.push('<');
                out.push_str(&name);
                for (key, value) in dom.attributes(id) {
                    out.push(' ');
                    out.push_str(&key);
                    out.push_str("=\"");
                    out.push_str(&escape(&value, true));
                    out.push('"');
                }
                let html = node.namespace == Namespace::Html;
                if xml && !html {
                    let parent = dom.parent(id).and_then(|p| dom.get(p)).map(|p| p.namespace);
                    let uri = node
                        .namespace
                        .uri()
                        .filter(|_| parent != Some(node.namespace));
                    if let Some(uri) = uri.filter(|_| dom.get_attr(id, "xmlns").is_none()) {
                        out.push_str(" xmlns=\"");
                        out.push_str(uri);
                        out.push('"');
                    }
                }
                if xml && !html && node.children.is_empty() {
                    out.push_str("/>");
                    continue;
                }
                out.push('>');
                if html && VOID.contains(&tag.as_str()) {
                    continue;
                }
                stack.push((id, false, true));
                let raw = html && raw_text(tag) == Some(false);
                stack.extend(node.children.iter().rev().map(|child| (*child, raw, false)));
            }
            NodeKind::Document | NodeKind::Fragment => {
                stack.extend(node.children.iter().rev().map(|child| (*child, raw, false)));
            }
        }
    }
}
