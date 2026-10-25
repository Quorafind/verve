//! `NodeFilter`, `document.createNodeIterator` and `document.createTreeWalker`: walking a
//! subtree in document order, the nodes a script asked to see and its filter accepts.
//!
//! Lifted from su-editor's Obsidian host.
//!
//! Both read the tree as it is at each step, so a node the walk changed is walked as it is now.
//! Where they stand is kept on the object itself (`referenceNode`, `currentNode`).

use boa_engine::{Context, JsResult, JsValue, js_string};

use super::node::{read_dom, wrap};
use super::{arg, call_method, function, get, node_of, plain_object, set};
use crate::dom::{Dom, NodeId, NodeKind};

const SHOW_ALL: u32 = u32::MAX;
const FILTER_ACCEPT: u32 = 1;
const FILTER_REJECT: u32 = 2;

/// The global `NodeFilter`, with its constants.
pub fn install(context: &mut Context) -> JsResult<()> {
    let filter = plain_object(context);
    for (key, value) in [
        ("FILTER_ACCEPT", 1.),
        ("FILTER_REJECT", 2.),
        ("FILTER_SKIP", 3.),
        ("SHOW_ALL", f64::from(SHOW_ALL)),
        ("SHOW_ELEMENT", 0x1 as f64),
        ("SHOW_ATTRIBUTE", 0x2 as f64),
        ("SHOW_TEXT", 0x4 as f64),
        ("SHOW_CDATA_SECTION", 0x8 as f64),
        ("SHOW_PROCESSING_INSTRUCTION", 0x40 as f64),
        ("SHOW_COMMENT", 0x80 as f64),
        ("SHOW_DOCUMENT", 0x100 as f64),
        ("SHOW_DOCUMENT_TYPE", 0x200 as f64),
        ("SHOW_DOCUMENT_FRAGMENT", 0x400 as f64),
    ] {
        set(&filter, key, value, context)?;
    }
    context.register_global_property(
        js_string!("NodeFilter"),
        filter,
        boa_engine::property::Attribute::WRITABLE | boa_engine::property::Attribute::CONFIGURABLE,
    )?;
    Ok(())
}

/// Whether `whatToShow` includes nodes of `id`'s kind.
fn shown(dom: &Dom, id: NodeId, what: u32) -> bool {
    let bit = match dom.get(id).map(|n| &n.kind) {
        Some(NodeKind::Element(_)) => 0x1,
        Some(NodeKind::Text(_)) => 0x4,
        Some(NodeKind::Comment(_)) => 0x80,
        Some(NodeKind::Document) => 0x100,
        Some(NodeKind::Fragment) => 0x400,
        Some(NodeKind::Doctype(_)) => 0x200,
        None => return false,
    };
    what & bit != 0
}

/// What the walk's filter says of `id`: accept, reject or skip. Nodes `whatToShow` leaves out
/// are skipped without asking.
fn judge(context: &mut Context, walk: &JsValue, id: NodeId) -> JsResult<u32> {
    let what = what_to_show(walk, context)?;
    if !read_dom(context, |dom| shown(dom, id, what)) {
        return Ok(3);
    }
    let filter = get(walk, "filter", context)?;
    if filter.is_null_or_undefined() {
        return Ok(FILTER_ACCEPT);
    }
    let node = wrap(context, id);
    let answer = if filter.is_callable() {
        verve::call(context, &filter, &JsValue::undefined(), &[node])?
    } else {
        call_method(&filter, "acceptNode", &[node], context)?
    };
    Ok(answer.to_u32(context)?)
}

fn what_to_show(walk: &JsValue, context: &mut Context) -> JsResult<u32> {
    let what = get(walk, "whatToShow", context)?;
    Ok(if what.is_undefined() {
        SHOW_ALL
    } else {
        what.to_u32(context)?
    })
}

fn node_at(walk: &JsValue, key: &str, context: &mut Context) -> JsResult<NodeId> {
    node_of(&get(walk, key, context)?)
}

/// The subtree under the walk's root, in document order, root first.
fn order(context: &mut Context, walk: &JsValue) -> JsResult<Vec<NodeId>> {
    let root = node_at(walk, "root", context)?;
    Ok(read_dom(context, |dom| dom.subtree(root)))
}

/// `document.createNodeIterator(root, whatToShow, filter)` or `createTreeWalker(…)`.
pub fn create(args: &[JsValue], context: &mut Context, walker: bool) -> JsResult<JsValue> {
    let root = arg(args, 0);
    node_of(&root)?;
    let walk = plain_object(context);
    set(&walk, "root", root.clone(), context)?;
    let what = arg(args, 1);
    let what = if what.is_undefined() {
        f64::from(SHOW_ALL)
    } else {
        f64::from(what.to_u32(context)?)
    };
    set(&walk, "whatToShow", what, context)?;
    let filter = arg(args, 2);
    set(
        &walk,
        "filter",
        if filter.is_undefined() {
            JsValue::null()
        } else {
            filter
        },
        context,
    )?;
    if walker {
        set(&walk, "currentNode", root, context)?;
        for (name, f) in [
            (
                "nextNode",
                walker_next as boa_engine::native_function::NativeFunctionPointer,
            ),
            ("previousNode", walker_previous),
            ("parentNode", walker_parent),
            ("firstChild", walker_first_child),
            ("lastChild", walker_last_child),
            ("nextSibling", walker_next_sibling),
            ("previousSibling", walker_previous_sibling),
        ] {
            let method = function(context, name, 0, f);
            set(&walk, name, method, context)?;
        }
    } else {
        set(&walk, "referenceNode", root, context)?;
        set(&walk, "pointerBeforeReferenceNode", true, context)?;
        for (name, f) in [
            (
                "nextNode",
                iterator_next as boa_engine::native_function::NativeFunctionPointer,
            ),
            ("previousNode", iterator_previous),
            ("detach", |_: &JsValue, _: &[JsValue], _: &mut Context| {
                Ok(JsValue::undefined())
            }),
        ] {
            let method = function(context, name, 0, f);
            set(&walk, name, method, context)?;
        }
    }
    Ok(walk.into())
}

// ─── NodeIterator ───────────────────────────────────────────────────────────────────────────

fn iterator_step(this: &JsValue, context: &mut Context, forward: bool) -> JsResult<JsValue> {
    let order = order(context, this)?;
    let reference = node_at(this, "referenceNode", context)?;
    let mut before = get(this, "pointerBeforeReferenceNode", context)?.to_boolean();
    let Some(mut at) = order.iter().position(|id| *id == reference) else {
        return Ok(JsValue::null());
    };
    loop {
        // Where the pointer goes next, and which node it passes.
        let candidate = match (forward, before) {
            (true, true) => {
                before = false;
                at
            }
            (true, false) => {
                at += 1;
                if at >= order.len() {
                    return Ok(JsValue::null());
                }
                at
            }
            (false, false) => {
                before = true;
                at
            }
            (false, true) => {
                if at == 0 {
                    return Ok(JsValue::null());
                }
                at -= 1;
                at
            }
        };
        let id = order[candidate];
        let node = wrap(context, id);
        set(
            &this.as_object().expect("an iterator is an object"),
            "referenceNode",
            node.clone(),
            context,
        )?;
        set(
            &this.as_object().expect("an iterator is an object"),
            "pointerBeforeReferenceNode",
            before,
            context,
        )?;
        if judge(context, this, id)? == FILTER_ACCEPT {
            return Ok(node);
        }
    }
}

fn iterator_next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    iterator_step(this, context, true)
}

fn iterator_previous(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    iterator_step(this, context, false)
}

// ─── TreeWalker ─────────────────────────────────────────────────────────────────────────────

/// Makes `id` the walker's current node and answers it.
fn land(this: &JsValue, context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let node = wrap(context, id);
    set(
        &this.as_object().expect("a walker is an object"),
        "currentNode",
        node.clone(),
        context,
    )?;
    Ok(node)
}

/// Whether `id` is inside a node under the root that the filter rejected — a rejected node's
/// subtree is not walked.
fn under_rejected(
    context: &mut Context,
    this: &JsValue,
    id: NodeId,
    root: NodeId,
) -> JsResult<bool> {
    let mut at = read_dom(context, |dom| dom.parent(id));
    while let Some(parent) = at {
        if parent == root {
            return Ok(false);
        }
        if judge(context, this, parent)? == FILTER_REJECT {
            return Ok(true);
        }
        at = read_dom(context, |dom| dom.parent(parent));
    }
    Ok(false)
}

fn walker_step(this: &JsValue, context: &mut Context, forward: bool) -> JsResult<JsValue> {
    let order = order(context, this)?;
    let root = node_at(this, "root", context)?;
    let current = node_at(this, "currentNode", context)?;
    let Some(at) = order.iter().position(|id| *id == current) else {
        return Ok(JsValue::null());
    };
    let candidates: Vec<NodeId> = match forward {
        true => order[at + 1..].to_vec(),
        false => order[..at].iter().rev().copied().collect(),
    };
    for id in candidates {
        if judge(context, this, id)? == FILTER_ACCEPT && !under_rejected(context, this, id, root)? {
            return land(this, context, id);
        }
    }
    Ok(JsValue::null())
}

fn walker_next(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    walker_step(this, context, true)
}

fn walker_previous(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    walker_step(this, context, false)
}

fn walker_parent(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let root = node_at(this, "root", context)?;
    let mut at = node_at(this, "currentNode", context)?;
    while at != root {
        let Some(parent) = read_dom(context, |dom| dom.parent(at)) else {
            break;
        };
        if judge(context, this, parent)? == FILTER_ACCEPT {
            return land(this, context, parent);
        }
        at = parent;
    }
    Ok(JsValue::null())
}

/// The first (or last) of `ids` the filter accepts.
fn first_accepted(this: &JsValue, context: &mut Context, ids: Vec<NodeId>) -> JsResult<JsValue> {
    for id in ids {
        if judge(context, this, id)? == FILTER_ACCEPT {
            return land(this, context, id);
        }
    }
    Ok(JsValue::null())
}

fn walker_first_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let current = node_at(this, "currentNode", context)?;
    let children = read_dom(context, |dom| dom.children(current).to_vec());
    first_accepted(this, context, children)
}

fn walker_last_child(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let current = node_at(this, "currentNode", context)?;
    let children = read_dom(context, |dom| {
        dom.children(current).iter().rev().copied().collect()
    });
    first_accepted(this, context, children)
}

fn siblings(context: &mut Context, this: &JsValue, forward: bool) -> JsResult<Vec<NodeId>> {
    let root = node_at(this, "root", context)?;
    let current = node_at(this, "currentNode", context)?;
    if current == root {
        return Ok(Vec::new());
    }
    Ok(read_dom(context, |dom| {
        let Some(parent) = dom.parent(current) else {
            return Vec::new();
        };
        let all = dom.children(parent);
        let at = all.iter().position(|id| *id == current).unwrap_or(0);
        match forward {
            true => all[at + 1..].to_vec(),
            false => all[..at].iter().rev().copied().collect(),
        }
    }))
}

fn walker_next_sibling(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let ids = siblings(context, this, true)?;
    first_accepted(this, context, ids)
}

fn walker_previous_sibling(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let ids = siblings(context, this, false)?;
    first_accepted(this, context, ids)
}
