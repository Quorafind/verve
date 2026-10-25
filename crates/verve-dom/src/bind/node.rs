//! Nodes' objects, and `Node` with what derives from it beside the elements: `CharacterData`,
//! `Text`, `Comment`, `DocumentFragment`, `DocumentType`; and the `ParentNode` and `ChildNode`
//! mixins every element, document and fragment shares.
//!
//! One node has one object: [`wrap`] keeps it (weakly), so `el.parentNode.firstChild === el`
//! holds and what a script stores on a node — React's fiber, a listener — stays with it. An
//! object keeps its parent's and children's objects alive with it (`verve::dom_events::hold`),
//! so a tree a script holds any part of stays whole, expandos and listeners and all, and a tree
//! it let go of entirely is collected — and then freed from the arena by
//! [`crate::Document::sweep`]. The document's own object is held by `window.document`, so
//! everything connected lives as long as the page.

use boa_engine::{Context, JsObject, JsResult, JsValue};

use super::{
    Accessor, ClassSpec, Method, arg, arg_string, array, class, dom_error, illegal_constructor,
    node_arg, node_id, node_of, opt_string, prototype_of_target, text, type_error,
};
use crate::dom::{Dom, Namespace, NodeId, NodeKind};
use crate::state::shared;

/// What distinguishes a node's number among host targets.
const NODE_TAG: u64 = 1 << 40;

/// The host id a node's object carries.
pub fn host_id_of(id: NodeId) -> u64 {
    NODE_TAG | u64::from(id)
}

/// The node a host id names, when it names one.
pub fn node_of_host(id: u64) -> Option<NodeId> {
    (id & NODE_TAG != 0).then_some((id & !NODE_TAG) as NodeId)
}

pub fn with_dom<R>(context: &mut Context, f: impl FnOnce(&mut Dom) -> R) -> R {
    let state = shared(context);
    let (result, relink_due, mutated, moved) = {
        let mut state = state.borrow_mut();
        let result = f(&mut state.dom);
        let mutated = state.dom.has_mutations() && !state.mutation_queued;
        let moved = state.dom.take_moved();
        // Only custom elements hear their subtrees come and go.
        let moved = if state.custom_nodes.is_empty() {
            Vec::new()
        } else {
            moved
        };
        (result, state.dom.has_relinked(), mutated, moved)
    };
    if relink_due {
        relink(context);
    }
    if mutated {
        super::observers::queue_delivery(context);
    }
    if !moved.is_empty() {
        super::platform::custom_moved(context, moved);
    }
    result
}

pub fn read_dom<R>(context: &Context, f: impl FnOnce(&Dom) -> R) -> R {
    let state = shared(context);
    let state = state.borrow();
    f(&state.dom)
}

/// The node's object, if it still has one.
pub fn existing_wrapper(context: &Context, id: NodeId) -> Option<JsObject> {
    shared(context).borrow().wrappers.get(&id)?.upgrade()
}

/// The class a node's object is made of.
fn class_name(dom: &Dom, id: NodeId) -> Option<&'static str> {
    let node = dom.get(id)?;
    Some(match &node.kind {
        NodeKind::Document
            if dom
                .children(id)
                .iter()
                .any(|c| dom.get(*c).and_then(|n| n.tag()) == Some("html")) =>
        {
            "HTMLDocument"
        }
        NodeKind::Document => "Document",
        NodeKind::Text(_) => "Text",
        NodeKind::Comment(_) => "Comment",
        NodeKind::Fragment => "DocumentFragment",
        NodeKind::Doctype(_) => "DocumentType",
        NodeKind::Element(tag) => match node.namespace {
            Namespace::Svg => super::element::svg_class(tag).unwrap_or("SVGElement"),
            Namespace::MathMl => "MathMLElement",
            Namespace::Other => "Element",
            Namespace::Html => super::element::html_class(tag),
        },
    })
}

/// The object that stands for `id`: the same one for as long as anything can see it. `null`
/// for an id that names nothing.
pub fn wrap(context: &mut Context, id: NodeId) -> JsValue {
    if let Some(object) = existing_wrapper(context, id) {
        return object.into();
    }
    let prototype = {
        let state = shared(context);
        let state = state.borrow();
        let is_shadow_root = state.shadow_hosts.contains_key(&id);
        match class_name(&state.dom, id) {
            None => return JsValue::null(),
            Some(_) if is_shadow_root => state.prototype("ShadowRoot"),
            Some(name) => state.prototype(name),
        }
    };
    let object = verve::dom_events::host_target(prototype, host_id_of(id));
    adopt(context, id, &object);
    object.into()
}

pub fn wrap_opt(context: &mut Context, id: Option<NodeId>) -> JsValue {
    match id {
        Some(id) => wrap(context, id),
        None => JsValue::null(),
    }
}

/// The objects of `ids`, as a `NodeList`: an array, with `item`.
pub fn node_list(ids: Vec<NodeId>, context: &mut Context) -> JsValue {
    list_of(ids, "NodeList", context)
}

/// The same as an `HTMLCollection`, with `namedItem`.
pub fn collection(ids: Vec<NodeId>, context: &mut Context) -> JsValue {
    list_of(ids, "HTMLCollection", context)
}

fn list_of(ids: Vec<NodeId>, class: &str, context: &mut Context) -> JsValue {
    let values = ids.into_iter().map(|id| wrap(context, id)).collect();
    let list = array(values, context);
    if let Some(object) = list.as_object() {
        let prototype = shared(context).borrow().prototype(class);
        object.set_prototype(Some(prototype));
    }
    list
}

/// Remembers `object` as the node's object, linked to its parent's.
pub fn adopt(context: &mut Context, id: NodeId, object: &JsObject) {
    shared(context)
        .borrow_mut()
        .wrappers
        .insert(id, object.downgrade());
    link(context, id, object);
}

fn link(context: &mut Context, id: NodeId, object: &JsObject) {
    let parent = read_dom(context, |dom| dom.parent(id));
    match parent {
        Some(parent) => {
            let Some(parent_object) = wrap(context, parent).as_object() else {
                return;
            };
            verve::dom_events::hold(object, |held| held.push(parent_object.clone()));
            verve::dom_events::hold(&parent_object, |held| held.push(object.clone()));
            shared(context).borrow_mut().links.insert(id, parent);
        }
        None => {
            shared(context).borrow_mut().links.remove(&id);
        }
    }
}

/// The objects of the nodes whose parent changed follow them: let go of the old parent's,
/// linked to the new one's.
pub fn relink(context: &mut Context) {
    let moved = shared(context).borrow_mut().dom.take_relinked();
    let mut seen = std::collections::HashSet::new();
    for id in moved {
        if !seen.insert(id) {
            continue;
        }
        let Some(object) = existing_wrapper(context, id) else {
            continue;
        };
        let (old, new) = {
            let state = shared(context);
            let state = state.borrow();
            (state.links.get(&id).copied(), state.dom.parent(id))
        };
        if old == new {
            continue;
        }
        if let Some(old) = old
            && let Some(old_object) = existing_wrapper(context, old)
        {
            verve::dom_events::hold(&old_object, |held| {
                if let Some(at) = held.iter().position(|o| JsObject::equals(o, &object)) {
                    held.swap_remove(at);
                }
            });
            verve::dom_events::hold(&object, |held| {
                if let Some(at) = held.iter().position(|o| JsObject::equals(o, &old_object)) {
                    held.swap_remove(at);
                }
            });
        }
        link(context, id, &object);
    }
}

/// A node from an argument that may be a node or a string (`append("text")`).
pub fn node_or_text(value: &JsValue, context: &mut Context) -> JsResult<NodeId> {
    if let Some(id) = node_id(value) {
        return Ok(id);
    }
    let content = super::string(value, context)?;
    Ok(with_dom(context, |dom| dom.create_text(&content)))
}

/// `nodes` — nodes or strings — as one node: the node itself, or a fragment of them.
fn convert_nodes(args: &[JsValue], context: &mut Context) -> JsResult<NodeId> {
    if let [only] = args
        && let Some(id) = node_id(only)
    {
        return Ok(id);
    }
    let mut ids = Vec::new();
    for value in args {
        ids.push(node_or_text(value, context)?);
    }
    Ok(with_dom(context, |dom| {
        let fragment = dom.create_fragment();
        for id in ids {
            dom.append(fragment, id);
        }
        fragment
    }))
}

/// Inserts `child` into `parent` before `before`, as `insertBefore` does: the DOM's errors for
/// what may not go there.
pub fn insert(
    context: &mut Context,
    parent: NodeId,
    child: NodeId,
    before: Option<NodeId>,
    method: &str,
) -> JsResult<()> {
    let (allowed, reference_ok) = read_dom(context, |dom| {
        (
            dom.may_insert(parent, child),
            before.is_none_or(|b| dom.parent(b) == Some(parent)),
        )
    });
    if !reference_ok {
        return Err(dom_error(
            context,
            &format!(
                "Failed to execute '{method}' on 'Node': The node before which the new node is to be inserted is not a child of this node."
            ),
            "NotFoundError",
        ));
    }
    if !allowed {
        return Err(dom_error(
            context,
            &format!(
                "Failed to execute '{method}' on 'Node': The new child element contains the parent."
            ),
            "HierarchyRequestError",
        ));
    }
    with_dom(context, |dom| dom.insert_before(parent, child, before));
    Ok(())
}

const NODE_ACCESSORS: &[Accessor] = &[
    (
        "nodeType",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| match dom.get(id).map(|n| &n.kind) {
                Some(NodeKind::Element(_)) => 1,
                Some(NodeKind::Text(_)) => 3,
                Some(NodeKind::Comment(_)) => 8,
                Some(NodeKind::Document) => 9,
                Some(NodeKind::Doctype(_)) => 10,
                _ => 11,
            })
            .into())
        }),
        None,
    ),
    (
        "nodeName",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                match dom.get(id).map(|n| &n.kind) {
                    Some(NodeKind::Element(_)) => crate::html::qualified_name(dom, id, true),
                    Some(NodeKind::Text(_)) => "#text".into(),
                    Some(NodeKind::Comment(_)) => "#comment".into(),
                    Some(NodeKind::Document) => "#document".into(),
                    Some(NodeKind::Doctype(name)) => name.clone(),
                    _ => "#document-fragment".into(),
                }
            })))
        }),
        None,
    ),
    (
        "parentNode",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let parent = read_dom(cx, |dom| dom.parent(id));
            Ok(wrap_opt(cx, parent))
        }),
        None,
    ),
    (
        "parentElement",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let parent = read_dom(cx, |dom| dom.parent_element(id));
            Ok(wrap_opt(cx, parent))
        }),
        None,
    ),
    (
        "childNodes",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let children = read_dom(cx, |dom| dom.children(id).to_vec());
            Ok(node_list(children, cx))
        }),
        None,
    ),
    (
        "firstChild",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let child = read_dom(cx, |dom| dom.children(id).first().copied());
            Ok(wrap_opt(cx, child))
        }),
        None,
    ),
    (
        "lastChild",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let child = read_dom(cx, |dom| dom.children(id).last().copied());
            Ok(wrap_opt(cx, child))
        }),
        None,
    ),
    (
        "nextSibling",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let sibling = read_dom(cx, |dom| dom.sibling(id, 1, false));
            Ok(wrap_opt(cx, sibling))
        }),
        None,
    ),
    (
        "previousSibling",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let sibling = read_dom(cx, |dom| dom.sibling(id, -1, false));
            Ok(wrap_opt(cx, sibling))
        }),
        None,
    ),
    (
        "textContent",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| match dom.get(id).map(|n| &n.kind) {
                Some(NodeKind::Document | NodeKind::Doctype(_)) => JsValue::null(),
                _ => text(&dom.text_content(id)),
            }))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = opt_string(args, 0, cx)?.unwrap_or_default();
            with_dom(cx, |dom| {
                if !matches!(
                    dom.get(id).map(|n| &n.kind),
                    Some(NodeKind::Document | NodeKind::Doctype(_))
                ) {
                    dom.set_text(id, &value)
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "nodeValue",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| match dom.get(id).map(|n| &n.kind) {
                Some(NodeKind::Text(t) | NodeKind::Comment(t)) => text(t),
                _ => JsValue::null(),
            }))
        }),
        // A text's or a comment's own; on anything else, nothing.
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = opt_string(args, 0, cx)?.unwrap_or_default();
            with_dom(cx, |dom| {
                if dom.get(id).is_some_and(|n| n.data().is_some()) {
                    dom.set_text(id, &value);
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "ownerDocument",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let doc = read_dom(cx, |dom| {
                if matches!(dom.get(id).map(|n| &n.kind), Some(NodeKind::Document)) {
                    return None;
                }
                Some(dom.document_of(id).unwrap_or(dom.document))
            });
            Ok(wrap_opt(cx, doc))
        }),
        None,
    ),
    (
        "isConnected",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.document_of(id).is_some()).into())
        }),
        None,
    ),
    (
        "baseURI",
        Some(|_, _, cx| Ok(text(shared(cx).borrow().url.as_str()))),
        None,
    ),
];

const NODE_METHODS: &[Method] = &[
    ("appendChild", 1, |this, args, cx| {
        let parent = node_of(this)?;
        let child = node_arg(args, 0, "appendChild", "Node")?;
        insert(cx, parent, child, None, "appendChild")?;
        Ok(arg(args, 0))
    }),
    ("insertBefore", 2, |this, args, cx| {
        let parent = node_of(this)?;
        let child = node_arg(args, 0, "insertBefore", "Node")?;
        let before = node_id(&arg(args, 1));
        insert(cx, parent, child, before, "insertBefore")?;
        Ok(arg(args, 0))
    }),
    ("removeChild", 1, |this, args, cx| {
        let parent = node_of(this)?;
        let child = node_arg(args, 0, "removeChild", "Node")?;
        if read_dom(cx, |dom| dom.parent(child)) != Some(parent) {
            return Err(dom_error(
                cx,
                "Failed to execute 'removeChild' on 'Node': The node to be removed is not a child of this node.",
                "NotFoundError",
            ));
        }
        with_dom(cx, |dom| dom.detach(child));
        Ok(arg(args, 0))
    }),
    ("replaceChild", 2, |this, args, cx| {
        let parent = node_of(this)?;
        let fresh = node_arg(args, 0, "replaceChild", "Node")?;
        let old = node_arg(args, 1, "replaceChild", "Node")?;
        if read_dom(cx, |dom| dom.parent(old)) != Some(parent) {
            return Err(dom_error(
                cx,
                "Failed to execute 'replaceChild' on 'Node': The node to be replaced is not a child of this node.",
                "NotFoundError",
            ));
        }
        if fresh != old {
            let next = read_dom(cx, |dom| dom.sibling(old, 1, false));
            with_dom(cx, |dom| dom.detach(old));
            let next = if next == Some(fresh) {
                read_dom(cx, |dom| dom.sibling(fresh, 1, false))
            } else {
                next
            };
            insert(cx, parent, fresh, next, "replaceChild")?;
        }
        Ok(arg(args, 1))
    }),
    ("contains", 1, |this, args, cx| {
        let outer = node_of(this)?;
        let Some(inner) = node_id(&arg(args, 0)) else {
            return Ok(false.into());
        };
        Ok(read_dom(cx, |dom| dom.contains(outer, inner)).into())
    }),
    ("hasChildNodes", 0, |this, _, cx| {
        let id = node_of(this)?;
        Ok(read_dom(cx, |dom| !dom.children(id).is_empty()).into())
    }),
    ("getRootNode", 1, |this, args, cx| {
        let id = node_of(this)?;
        let composed = super::get(&arg(args, 0), "composed", cx)?.to_boolean();
        let root = {
            let state = shared(cx);
            let state = state.borrow();
            let mut root = state.dom.root_of(id);
            while composed && let Some(host) = state.shadow_hosts.get(&root) {
                root = state.dom.root_of(*host);
            }
            root
        };
        Ok(wrap(cx, root))
    }),
    ("cloneNode", 1, |this, args, cx| {
        let id = node_of(this)?;
        let deep = arg(args, 0).to_boolean();
        let copy = with_dom(cx, |dom| dom.clone_node(id, deep));
        Ok(wrap_opt(cx, copy))
    }),
    ("compareDocumentPosition", 1, |this, args, cx| {
        let node = node_of(this)?;
        let other = node_arg(args, 0, "compareDocumentPosition", "Node")?;
        Ok(read_dom(cx, |dom| compare_position(dom, node, other)).into())
    }),
    ("isSameNode", 1, |this, args, _| {
        Ok((node_id(this).is_some() && node_id(this) == node_id(&arg(args, 0))).into())
    }),
    ("isEqualNode", 1, |this, args, cx| {
        let (Some(a), Some(b)) = (node_id(this), node_id(&arg(args, 0))) else {
            return Ok(false.into());
        };
        Ok(read_dom(cx, |dom| equal_nodes(dom, a, b)).into())
    }),
    // Adjacent text nodes joined into one, and empty ones gone.
    ("normalize", 0, |this, _, cx| {
        let id = node_of(this)?;
        with_dom(cx, |dom| {
            for parent in dom.subtree(id) {
                let children = dom.children(parent).to_vec();
                let mut previous: Option<NodeId> = None;
                for child in children {
                    let Some(NodeKind::Text(content)) = dom.get(child).map(|n| n.kind.clone())
                    else {
                        previous = None;
                        continue;
                    };
                    if content.is_empty() {
                        dom.detach(child);
                        continue;
                    }
                    match previous {
                        Some(before) => {
                            let joined = format!("{}{content}", dom.text_content(before));
                            dom.set_text(before, &joined);
                            dom.detach(child);
                        }
                        None => previous = Some(child),
                    }
                }
            }
        });
        Ok(JsValue::undefined())
    }),
    ("lookupNamespaceURI", 1, |this, args, cx| {
        let id = node_of(this)?;
        let prefix = opt_string(args, 0, cx)?;
        if prefix.is_some() {
            return Ok(JsValue::null());
        }
        Ok(
            read_dom(cx, |dom| dom.get(id).and_then(|n| n.namespace.uri()))
                .map_or(JsValue::null(), text),
        )
    }),
    ("lookupPrefix", 1, |_, _, _| Ok(JsValue::null())),
    ("isDefaultNamespace", 1, |this, args, cx| {
        let id = node_of(this)?;
        let uri = opt_string(args, 0, cx)?;
        Ok(
            (read_dom(cx, |dom| dom.get(id).and_then(|n| n.namespace.uri())) == uri.as_deref())
                .into(),
        )
    }),
];

const DISCONNECTED: u32 = 1;
const PRECEDING: u32 = 2;
const FOLLOWING: u32 = 4;
const CONTAINS: u32 = 8;
const CONTAINED_BY: u32 = 16;
const IMPLEMENTATION_SPECIFIC: u32 = 32;

pub fn ancestry(dom: &Dom, id: NodeId) -> Vec<NodeId> {
    let mut chain = vec![id];
    let mut at = dom.parent(id);
    while let Some(node) = at {
        chain.push(node);
        at = dom.parent(node);
    }
    chain.reverse();
    chain
}

/// Where `other` is from `node`, as `node.compareDocumentPosition(other)` answers.
pub fn compare_position(dom: &Dom, node: NodeId, other: NodeId) -> u32 {
    if node == other {
        return 0;
    }
    let a = ancestry(dom, node);
    let b = ancestry(dom, other);
    if a.first() != b.first() {
        let order = if other < node { PRECEDING } else { FOLLOWING };
        return DISCONNECTED | IMPLEMENTATION_SPECIFIC | order;
    }
    if a.contains(&other) {
        return CONTAINS | PRECEDING;
    }
    if b.contains(&node) {
        return CONTAINED_BY | FOLLOWING;
    }
    let shared_depth = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let parent = a[shared_depth - 1];
    let children = dom.children(parent);
    let at = |id: NodeId| children.iter().position(|c| *c == id).unwrap_or(0);
    if at(b[shared_depth]) < at(a[shared_depth]) {
        PRECEDING
    } else {
        FOLLOWING
    }
}

fn equal_nodes(dom: &Dom, a: NodeId, b: NodeId) -> bool {
    let (Some(x), Some(y)) = (dom.get(a), dom.get(b)) else {
        return false;
    };
    if x.kind != y.kind || x.namespace != y.namespace {
        return false;
    }
    let mut ax = dom.attributes(a);
    let mut ay = dom.attributes(b);
    ax.sort();
    ay.sort();
    ax == ay
        && x.children.len() == y.children.len()
        && x.children
            .iter()
            .zip(&y.children)
            .all(|(c, d)| equal_nodes(dom, *c, *d))
}

// ─── ParentNode and ChildNode ───────────────────────────────────────────────────────────────

pub const PARENT_NODE_METHODS: &[Method] = &[
    ("append", 0, |this, args, cx| {
        let parent = node_of(this)?;
        let node = convert_nodes(args, cx)?;
        insert(cx, parent, node, None, "append")?;
        Ok(JsValue::undefined())
    }),
    ("prepend", 0, |this, args, cx| {
        let parent = node_of(this)?;
        let node = convert_nodes(args, cx)?;
        let first = read_dom(cx, |dom| dom.children(parent).first().copied());
        insert(cx, parent, node, first, "prepend")?;
        Ok(JsValue::undefined())
    }),
    ("replaceChildren", 0, |this, args, cx| {
        let parent = node_of(this)?;
        let node = convert_nodes(args, cx)?;
        with_dom(cx, |dom| dom.empty(parent));
        insert(cx, parent, node, None, "replaceChildren")?;
        Ok(JsValue::undefined())
    }),
    ("querySelector", 1, |this, args, cx| {
        let id = node_of(this)?;
        let selector = query_selector(args, cx)?;
        let found = read_dom(cx, |dom| {
            let scope = dom.scope_of(id);
            dom.descendants(id)
                .into_iter()
                .find(|n| selector.matches_in(dom, *n, Some(scope)))
        });
        Ok(wrap_opt(cx, found))
    }),
    ("querySelectorAll", 1, |this, args, cx| {
        let id = node_of(this)?;
        let selector = query_selector(args, cx)?;
        let found = read_dom(cx, |dom| {
            let scope = dom.scope_of(id);
            dom.descendants(id)
                .into_iter()
                .filter(|n| selector.matches_in(dom, *n, Some(scope)))
                .collect()
        });
        Ok(node_list(found, cx))
    }),
    ("getElementsByClassName", 1, |this, args, cx| {
        let id = node_of(this)?;
        let classes = arg_string(args, 0, cx)?;
        let wanted: Vec<String> = classes.split_whitespace().map(str::to_owned).collect();
        let found = read_dom(cx, |dom| {
            if wanted.is_empty() {
                return Vec::new();
            }
            dom.descendants(id)
                .into_iter()
                .filter(|n| {
                    dom.get(*n)
                        .is_some_and(|n| wanted.iter().all(|c| n.has_class(c)))
                })
                .collect()
        });
        Ok(collection(found, cx))
    }),
    ("getElementsByTagName", 1, |this, args, cx| {
        let id = node_of(this)?;
        let tag = arg_string(args, 0, cx)?.to_ascii_lowercase();
        let found = read_dom(cx, |dom| {
            dom.descendants(id)
                .into_iter()
                .filter(|n| tag == "*" || dom.get(*n).and_then(|n| n.tag()) == Some(tag.as_str()))
                .collect()
        });
        Ok(collection(found, cx))
    }),
    ("getElementsByTagNameNS", 2, |this, args, cx| {
        let id = node_of(this)?;
        let tag = arg_string(args, 1, cx)?.to_ascii_lowercase();
        let found = read_dom(cx, |dom| {
            dom.descendants(id)
                .into_iter()
                .filter(|n| tag == "*" || dom.get(*n).and_then(|n| n.tag()) == Some(tag.as_str()))
                .collect()
        });
        Ok(collection(found, cx))
    }),
];

/// `querySelector`'s selector, or its `SyntaxError`.
pub fn query_selector(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<crate::css::SelectorList> {
    let selector = arg_string(args, 0, context)?;
    crate::css::SelectorList::parse(&selector).map_err(|error| {
        dom_error(
            context,
            &format!(
                "Failed to execute 'querySelector' on 'Document': {}",
                error.0
            ),
            "SyntaxError",
        )
    })
}

fn element_children(dom: &Dom, id: NodeId) -> Vec<NodeId> {
    dom.children(id)
        .iter()
        .copied()
        .filter(|c| dom.get(*c).is_some_and(|n| n.is_element()))
        .collect()
}

pub const PARENT_NODE_ACCESSORS: &[Accessor] = &[
    (
        "children",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let children = read_dom(cx, |dom| element_children(dom, id));
            Ok(collection(children, cx))
        }),
        None,
    ),
    (
        "childElementCount",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| element_children(dom, id).len()).into())
        }),
        None,
    ),
    (
        "firstElementChild",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let child = read_dom(cx, |dom| element_children(dom, id).first().copied());
            Ok(wrap_opt(cx, child))
        }),
        None,
    ),
    (
        "lastElementChild",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let child = read_dom(cx, |dom| element_children(dom, id).last().copied());
            Ok(wrap_opt(cx, child))
        }),
        None,
    ),
];

pub const CHILD_NODE_METHODS: &[Method] = &[
    ("remove", 0, |this, _, cx| {
        let id = node_of(this)?;
        with_dom(cx, |dom| dom.detach(id));
        Ok(JsValue::undefined())
    }),
    ("before", 0, |this, args, cx| {
        let id = node_of(this)?;
        let Some(parent) = read_dom(cx, |dom| dom.parent(id)) else {
            return Ok(JsValue::undefined());
        };
        let node = convert_nodes(args, cx)?;
        insert(cx, parent, node, Some(id), "before")?;
        Ok(JsValue::undefined())
    }),
    ("after", 0, |this, args, cx| {
        let id = node_of(this)?;
        let Some(parent) = read_dom(cx, |dom| dom.parent(id)) else {
            return Ok(JsValue::undefined());
        };
        let node = convert_nodes(args, cx)?;
        let next = read_dom(cx, |dom| dom.sibling(id, 1, false));
        let next = if next == Some(node) {
            read_dom(cx, |dom| dom.sibling(node, 1, false))
        } else {
            next
        };
        insert(cx, parent, node, next, "after")?;
        Ok(JsValue::undefined())
    }),
    ("replaceWith", 0, |this, args, cx| {
        let id = node_of(this)?;
        let Some(parent) = read_dom(cx, |dom| dom.parent(id)) else {
            return Ok(JsValue::undefined());
        };
        let node = convert_nodes(args, cx)?;
        if node != id {
            let next = read_dom(cx, |dom| dom.sibling(id, 1, false));
            with_dom(cx, |dom| dom.detach(id));
            insert(cx, parent, node, next.filter(|n| *n != node), "replaceWith")?;
        }
        Ok(JsValue::undefined())
    }),
];

pub const CHILD_NODE_ACCESSORS: &[Accessor] = &[
    (
        "nextElementSibling",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let next = read_dom(cx, |dom| dom.sibling(id, 1, true));
            Ok(wrap_opt(cx, next))
        }),
        None,
    ),
    (
        "previousElementSibling",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let previous = read_dom(cx, |dom| dom.sibling(id, -1, true));
            Ok(wrap_opt(cx, previous))
        }),
        None,
    ),
];

// ─── CharacterData, Text, Comment ───────────────────────────────────────────────────────────

fn data_of(this: &JsValue, context: &Context) -> JsResult<(NodeId, String)> {
    let id = node_of(this)?;
    let data = read_dom(context, |dom| {
        dom.get(id).and_then(|n| n.data()).map(str::to_owned)
    });
    data.map(|d| (id, d))
        .ok_or_else(|| type_error("Illegal invocation: not CharacterData"))
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `offset` and `count` in UTF-16 units over `units`, clamped as the DOM clamps them.
fn span(units: &[u16], offset: f64, count: f64) -> (usize, usize) {
    let start = (offset.max(0.) as usize).min(units.len());
    let end = (start + count.max(0.) as usize).min(units.len());
    (start, end)
}

const CHARACTER_DATA_ACCESSORS: &[Accessor] = &[
    (
        "data",
        Some(|this, _, cx| Ok(text(&data_of(this, cx)?.1))),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = opt_string(args, 0, cx)?.unwrap_or_default();
            with_dom(cx, |dom| dom.set_text(id, &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "length",
        Some(|this, _, cx| Ok(utf16(&data_of(this, cx)?.1).len().into())),
        None,
    ),
];

const CHARACTER_DATA_METHODS: &[Method] = &[
    ("appendData", 1, |this, args, cx| {
        let (id, data) = data_of(this, cx)?;
        let more = arg_string(args, 0, cx)?;
        with_dom(cx, |dom| dom.set_text(id, &format!("{data}{more}")));
        Ok(JsValue::undefined())
    }),
    ("insertData", 2, |this, args, cx| {
        let (id, data) = data_of(this, cx)?;
        let offset = arg(args, 0).to_number(cx)?;
        let more = arg_string(args, 1, cx)?;
        let units = utf16(&data);
        let (at, _) = span(&units, offset, 0.);
        let joined = format!(
            "{}{more}{}",
            String::from_utf16_lossy(&units[..at]),
            String::from_utf16_lossy(&units[at..])
        );
        with_dom(cx, |dom| dom.set_text(id, &joined));
        Ok(JsValue::undefined())
    }),
    ("deleteData", 2, |this, args, cx| {
        let (id, data) = data_of(this, cx)?;
        let (offset, count) = (arg(args, 0).to_number(cx)?, arg(args, 1).to_number(cx)?);
        let units = utf16(&data);
        let (start, end) = span(&units, offset, count);
        let kept = format!(
            "{}{}",
            String::from_utf16_lossy(&units[..start]),
            String::from_utf16_lossy(&units[end..])
        );
        with_dom(cx, |dom| dom.set_text(id, &kept));
        Ok(JsValue::undefined())
    }),
    ("replaceData", 3, |this, args, cx| {
        let (id, data) = data_of(this, cx)?;
        let (offset, count) = (arg(args, 0).to_number(cx)?, arg(args, 1).to_number(cx)?);
        let more = arg_string(args, 2, cx)?;
        let units = utf16(&data);
        let (start, end) = span(&units, offset, count);
        let joined = format!(
            "{}{more}{}",
            String::from_utf16_lossy(&units[..start]),
            String::from_utf16_lossy(&units[end..])
        );
        with_dom(cx, |dom| dom.set_text(id, &joined));
        Ok(JsValue::undefined())
    }),
    ("substringData", 2, |this, args, cx| {
        let (_, data) = data_of(this, cx)?;
        let (offset, count) = (arg(args, 0).to_number(cx)?, arg(args, 1).to_number(cx)?);
        let units = utf16(&data);
        let (start, end) = span(&units, offset, count);
        Ok(text(&String::from_utf16_lossy(&units[start..end])))
    }),
];

const TEXT_METHODS: &[Method] = &[("splitText", 1, |this, args, cx| {
    let (id, data) = data_of(this, cx)?;
    let offset = arg(args, 0).to_number(cx)?;
    let units = utf16(&data);
    if offset as usize > units.len() {
        return Err(dom_error(
            cx,
            "Failed to execute 'splitText' on 'Text': The offset is larger than the Text node's length.",
            "IndexSizeError",
        ));
    }
    let (at, _) = span(&units, offset, 0.);
    let head = String::from_utf16_lossy(&units[..at]);
    let tail = String::from_utf16_lossy(&units[at..]);
    let rest = with_dom(cx, |dom| {
        dom.set_text(id, &head);
        let rest = dom.create_text(&tail);
        if let Some(parent) = dom.parent(id) {
            let next = dom.sibling(id, 1, false);
            dom.insert_before(parent, rest, next);
        }
        rest
    });
    Ok(wrap(cx, rest))
})];

const TEXT_ACCESSORS: &[Accessor] = &[
    (
        "wholeText",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                let mut start = id;
                while let Some(previous) = dom
                    .sibling(start, -1, false)
                    .filter(|p| dom.get(*p).is_some_and(|n| n.is_text()))
                {
                    start = previous;
                }
                let mut out = String::new();
                let mut at = Some(start);
                while let Some(node) = at.filter(|n| dom.get(*n).is_some_and(|n| n.is_text())) {
                    out.push_str(dom.get(node).and_then(|n| n.data()).unwrap_or(""));
                    at = dom.sibling(node, 1, false);
                }
                out
            })))
        }),
        None,
    ),
    ("assignedSlot", Some(|_, _, _| Ok(JsValue::null())), None),
];

/// `new Text(data)`, `new Comment(data)`, `new DocumentFragment()`: the node made, its object
/// on the prototype of the class `new` named.
fn construct_node(
    new_target: &JsValue,
    context: &mut Context,
    make: impl FnOnce(&mut Dom) -> NodeId,
) -> JsResult<JsValue> {
    let prototype = prototype_of_target(new_target, context)?;
    let id = with_dom(context, make);
    let object = verve::dom_events::host_target(prototype, host_id_of(id));
    adopt(context, id, &object);
    Ok(object.into())
}

const DOCTYPE_ACCESSORS: &[Accessor] = &[
    (
        "name",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                match dom.get(id).map(|n| &n.kind) {
                    Some(NodeKind::Doctype(name)) => name.clone(),
                    _ => String::new(),
                }
            })))
        }),
        None,
    ),
    ("publicId", Some(|_, _, _| Ok(text(""))), None),
    ("systemId", Some(|_, _, _| Ok(text(""))), None),
];

pub fn install(context: &mut Context) -> JsResult<()> {
    class(
        context,
        ClassSpec {
            name: "Node",
            length: 0,
            construct: illegal_constructor,
            parent: Some("EventTarget"),
            methods: NODE_METHODS,
            accessors: NODE_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let (node, node_proto) = shared(context).borrow().class("Node").expect("made");
    for (key, value) in [
        ("ELEMENT_NODE", 1),
        ("ATTRIBUTE_NODE", 2),
        ("TEXT_NODE", 3),
        ("CDATA_SECTION_NODE", 4),
        ("ENTITY_REFERENCE_NODE", 5),
        ("ENTITY_NODE", 6),
        ("PROCESSING_INSTRUCTION_NODE", 7),
        ("COMMENT_NODE", 8),
        ("DOCUMENT_NODE", 9),
        ("DOCUMENT_TYPE_NODE", 10),
        ("DOCUMENT_FRAGMENT_NODE", 11),
        ("NOTATION_NODE", 12),
        ("DOCUMENT_POSITION_DISCONNECTED", 1),
        ("DOCUMENT_POSITION_PRECEDING", 2),
        ("DOCUMENT_POSITION_FOLLOWING", 4),
        ("DOCUMENT_POSITION_CONTAINS", 8),
        ("DOCUMENT_POSITION_CONTAINED_BY", 16),
        ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32),
    ] {
        for target in [&node, &node_proto] {
            target.define_property_or_throw(
                boa_engine::JsString::from(key),
                boa_engine::property::PropertyDescriptor::builder()
                    .value(value)
                    .writable(false)
                    .enumerable(true)
                    .configurable(false),
                context,
            )?;
        }
    }
    class(
        context,
        ClassSpec {
            name: "CharacterData",
            length: 0,
            construct: illegal_constructor,
            parent: Some("Node"),
            methods: CHARACTER_DATA_METHODS,
            accessors: CHARACTER_DATA_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let (_, character_data) = shared(context)
        .borrow()
        .class("CharacterData")
        .expect("made");
    super::extend_prototype(
        context,
        &character_data,
        CHILD_NODE_METHODS,
        CHILD_NODE_ACCESSORS,
    )?;
    class(
        context,
        ClassSpec {
            name: "Text",
            length: 1,
            construct: |new_target, args, cx| {
                let content = opt_string(args, 0, cx)?.unwrap_or_default();
                construct_node(new_target, cx, |dom| dom.create_text(&content))
            },
            parent: Some("CharacterData"),
            methods: TEXT_METHODS,
            accessors: TEXT_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    class(
        context,
        ClassSpec {
            name: "CDATASection",
            length: 0,
            construct: illegal_constructor,
            parent: Some("Text"),
            methods: &[],
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    class(
        context,
        ClassSpec {
            name: "Comment",
            length: 1,
            construct: |new_target, args, cx| {
                let data = opt_string(args, 0, cx)?.unwrap_or_default();
                construct_node(new_target, cx, |dom| dom.create_comment(&data))
            },
            parent: Some("CharacterData"),
            methods: &[],
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    class(
        context,
        ClassSpec {
            name: "DocumentFragment",
            length: 0,
            construct: |new_target, _, cx| construct_node(new_target, cx, Dom::create_fragment),
            parent: Some("Node"),
            methods: PARENT_NODE_METHODS,
            accessors: PARENT_NODE_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    let (_, fragment) = shared(context)
        .borrow()
        .class("DocumentFragment")
        .expect("made");
    super::extend_prototype(
        context,
        &fragment,
        &[("getElementById", 1, |this, args, cx| {
            let root = node_of(this)?;
            let wanted = arg_string(args, 0, cx)?;
            let found = read_dom(cx, |dom| {
                dom.descendants(root)
                    .into_iter()
                    .find(|n| dom.get(*n).and_then(|n| n.attr("id")) == Some(wanted.as_str()))
            });
            Ok(wrap_opt(cx, found))
        })],
        &[],
    )?;
    class(
        context,
        ClassSpec {
            name: "DocumentType",
            length: 0,
            construct: illegal_constructor,
            parent: Some("Node"),
            methods: CHILD_NODE_METHODS,
            accessors: DOCTYPE_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    // Lists of nodes are arrays here, with the list's own methods on top.
    let array_prototype = context.intrinsics().constructors().array().prototype();
    for (name, methods) in [
        ("NodeList", LIST_METHODS),
        ("HTMLCollection", COLLECTION_METHODS),
    ] {
        let (_, prototype) = class(
            context,
            ClassSpec {
                name,
                length: 0,
                construct: illegal_constructor,
                parent: None,
                methods,
                accessors: &[],
                statics: &[],
                global: true,
            },
        )?;
        prototype.set_prototype(Some(array_prototype.clone()));
    }
    Ok(())
}

const LIST_METHODS: &[Method] = &[("item", 1, |this, args, cx| {
    let at = arg(args, 0).to_number(cx)?;
    let found = match this.as_object() {
        Some(list) if at >= 0. => list.get(at as u32, cx)?,
        _ => JsValue::undefined(),
    };
    Ok(if found.is_undefined() {
        JsValue::null()
    } else {
        found
    })
})];

const COLLECTION_METHODS: &[Method] = &[
    ("item", 1, |this, args, cx| {
        let at = arg(args, 0).to_number(cx)?;
        let found = match this.as_object() {
            Some(list) if at >= 0. => list.get(at as u32, cx)?,
            _ => JsValue::undefined(),
        };
        Ok(if found.is_undefined() {
            JsValue::null()
        } else {
            found
        })
    }),
    ("namedItem", 1, |this, args, cx| {
        let name = arg_string(args, 0, cx)?;
        for item in super::list(this, cx)? {
            let Some(id) = node_id(&item) else { continue };
            let hit = read_dom(cx, |dom| {
                dom.get_attr(id, "id").as_deref() == Some(name.as_str())
                    || dom.get_attr(id, "name").as_deref() == Some(name.as_str())
            });
            if hit {
                return Ok(item);
            }
        }
        Ok(JsValue::null())
    }),
];
