//! Live collections: the `NodeList` of `childNodes` and the `HTMLCollection`s of `children`,
//! `getElementsByTagName`, `form.elements`, `select.options`… A script holds one while the tree
//! changes under it, and `length` and the items answer for the tree as it is when asked.
//!
//! Each is a `Proxy` over a hidden [`Live`] carrying the query that finds its nodes. The result
//! is kept until the arena's `revision` moves, so reading `length` in a loop costs one
//! query per change, not one per read.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsString, JsValue, Trace, js_string,
    object::builtins::JsProxyBuilder, property::PropertyKey,
};

use super::node::{read_dom, wrap};
use super::{arg, hidden};
use crate::dom::{Dom, NodeId};
use crate::state::shared;

type Query = Rc<dyn Fn(&Dom) -> Vec<NodeId>>;

#[derive(Trace, Finalize, JsData)]
struct Live {
    #[unsafe_ignore_trace]
    query: Query,
    /// The nodes, and the arena's `revision` they were found at.
    #[unsafe_ignore_trace]
    found: RefCell<Option<(u64, Rc<Vec<NodeId>>)>>,
    /// Whether an `HTMLCollection`: its items are also properties, by `id` and `name`.
    #[unsafe_ignore_trace]
    named: bool,
}

impl Live {
    /// The nodes now, found again only if the tree changed since they were last asked for.
    fn nodes(&self, context: &Context) -> Rc<Vec<NodeId>> {
        let revision = read_dom(context, |dom| dom.revision);
        let mut found = self.found.borrow_mut();
        match &*found {
            Some((seen, nodes)) if *seen == revision => nodes.clone(),
            _ => {
                let nodes = Rc::new(read_dom(context, |dom| (self.query)(dom)));
                *found = Some((revision, nodes.clone()));
                nodes
            }
        }
    }
}

/// A `NodeList` that follows the tree: `query` is asked whenever the tree has changed.
pub fn node_list(
    context: &mut Context,
    query: impl Fn(&Dom) -> Vec<NodeId> + 'static,
) -> JsResult<JsValue> {
    build(context, "NodeList", Rc::new(query), false)
}

/// An `HTMLCollection` that follows the tree.
pub fn collection(
    context: &mut Context,
    query: impl Fn(&Dom) -> Vec<NodeId> + 'static,
) -> JsResult<JsValue> {
    build(context, "HTMLCollection", Rc::new(query), true)
}

fn build(context: &mut Context, class: &str, query: Query, named: bool) -> JsResult<JsValue> {
    let prototype = shared(context).borrow().prototype(class);
    let live = Live {
        query,
        found: RefCell::default(),
        named,
    };
    let target = JsObject::from_proto_and_data(prototype, live);
    let proxy = JsProxyBuilder::new(target)
        .get(trap_get)
        .has(trap_has)
        .build(context)?;
    Ok(proxy.into())
}

/// The index a property key names, if it is a canonical array index.
fn index_of(key: &PropertyKey) -> Option<usize> {
    match key {
        PropertyKey::Index(i) => Some(i.get() as usize),
        _ => None,
    }
}

/// The first of `nodes` whose `id` or `name` is `name`: `namedItem`.
pub fn named_item(context: &Context, nodes: &[NodeId], name: &str) -> Option<NodeId> {
    read_dom(context, |dom| {
        nodes.iter().copied().find(|id| {
            dom.get_attr(*id, "id").as_deref() == Some(name)
                || dom.get_attr(*id, "name").as_deref() == Some(name)
        })
    })
}

fn trap_get(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0).as_object().expect("a proxy's target");
    let key = arg(args, 1).to_property_key(context)?;
    let live = target.downcast_ref::<Live>().expect("a live collection");
    let nodes = live.nodes(context);
    if let Some(at) = index_of(&key) {
        return Ok(nodes
            .get(at)
            .map_or(JsValue::undefined(), |id| wrap(context, *id)));
    }
    if key == PropertyKey::from(js_string!("length")) {
        return Ok(nodes.len().into());
    }
    let found = target.get(key.clone(), context)?;
    if let (true, true, PropertyKey::String(name)) = (found.is_undefined(), live.named, &key)
        && let Some(id) = named_item(context, &nodes, &name.to_std_string_escaped())
    {
        return Ok(wrap(context, id));
    }
    Ok(found)
}

fn trap_has(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let target = arg(args, 0).as_object().expect("a proxy's target");
    let key = arg(args, 1).to_property_key(context)?;
    let live = target.downcast_ref::<Live>().expect("a live collection");
    if let Some(at) = index_of(&key) {
        return Ok((at < live.nodes(context).len()).into());
    }
    Ok(
        (key == PropertyKey::from(js_string!("length")) || target.has_property(key, context)?)
            .into(),
    )
}

/// [`collection`], the same one each time `this` is asked for `key`.
pub fn same_collection(
    this: &JsValue,
    key: &str,
    context: &mut Context,
    query: impl Fn(&Dom) -> Vec<NodeId> + 'static,
) -> JsResult<JsValue> {
    same_object(this, key, context, |context| collection(context, query))
}

/// [`node_list`], the same one each time `this` is asked for `key`.
pub fn same_node_list(
    this: &JsValue,
    key: &str,
    context: &mut Context,
    query: impl Fn(&Dom) -> Vec<NodeId> + 'static,
) -> JsResult<JsValue> {
    same_object(this, key, context, |context| node_list(context, query))
}

/// The object `[SameObject]` says: made the first time `this` is asked for `key`, then kept on
/// it, so `el.childNodes === el.childNodes`.
pub fn same_object(
    this: &JsValue,
    key: &str,
    context: &mut Context,
    make: impl FnOnce(&mut Context) -> JsResult<JsValue>,
) -> JsResult<JsValue> {
    let Some(owner) = this.as_object() else {
        return make(context);
    };
    let key = JsString::from(format!("__{key}"));
    let kept = owner.get(key.clone(), context)?;
    if !kept.is_undefined() {
        return Ok(kept);
    }
    let made = make(context)?;
    hidden(&owner, key, made.clone(), context)?;
    Ok(made)
}
