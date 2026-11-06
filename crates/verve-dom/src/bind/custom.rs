//! Custom elements: `customElements.define`, the elements of its classes, and the callbacks
//! they hear — `connectedCallback`, `disconnectedCallback`, `attributeChangedCallback`.
//!
//! An element is a custom element once it is *upgraded*: its class's constructor run on it (its
//! `super()` hands back the element rather than making one), and the attributes it has and its
//! place in the document told to it. Elements the parser makes, `innerHTML` among them, are
//! upgraded when they reach the document, and `define` upgrades what is there already, both in
//! document order. The callbacks run when the DOM method that caused them returns: every change
//! goes through [`with_dom`], which hands what happened to [`react`].

use std::collections::HashSet;

use boa_engine::{Context, JsObject, JsResult, JsString, JsValue, object::builtins::JsPromise};

use super::node::{existing_wrapper, read_dom, with_dom, wrap};
use super::{Method, dom_error, string};
use crate::dom::{AttrChange, Namespace, NodeId, is_custom_element_name};
use crate::state::shared;
use verve::native::{arg, js_text, list_of, text_of, type_error};

/// A tag `customElements.define` was given a class for.
pub struct Definition {
    name: String,
    class: JsObject,
    /// The attributes of the class's `observedAttributes`.
    observed: Vec<String>,
}

/// Every definition, the elements that are instances of one, and what waits on them.
#[derive(Default)]
pub struct Registry {
    definitions: Vec<Definition>,
    /// The elements that are instances of a defined class.
    nodes: HashSet<NodeId>,
    /// `whenDefined`'s promises, by the name waited for.
    waiting: Vec<(String, JsValue)>,
    /// The elements being upgraded, with their tags: what `HTMLElement`'s constructor hands
    /// back in place of making one.
    upgrading: Vec<(String, NodeId)>,
}

impl Registry {
    fn named(&self, name: &str) -> Option<&Definition> {
        self.definitions.iter().find(|d| d.name == name)
    }

    fn of_class(&self, class: &JsObject) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|d| JsObject::equals(&d.class, class))
    }

    /// Whether nothing was defined: no element hears anything.
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Forgets a node that was freed.
    pub fn forget(&mut self, id: NodeId) {
        self.nodes.remove(&id);
    }
}

/// The class defined for a tag, if one was.
pub fn class_of(context: &Context, tag: &str) -> Option<JsObject> {
    let state = shared(context);
    let state = state.borrow();
    state.custom.named(tag).map(|d| d.class.clone())
}

/// The tag a class was defined for.
fn tag_of(context: &Context, class: &JsObject) -> Option<String> {
    let state = shared(context);
    let state = state.borrow();
    state.custom.of_class(class).map(|d| d.name.clone())
}

/// `HTMLElement` constructed: only as `super()` of a class `customElements.define` was given,
/// answering the element being upgraded or making one of that class's tag.
pub fn construct_html_element(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let tag = new_target
        .as_object()
        .and_then(|target| tag_of(context, &target))
        .ok_or_else(|| type_error("Illegal constructor"))?;
    let prototype = super::prototype_of_target(new_target, context)?;
    let upgrading = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let at = state.custom.upgrading.iter().rposition(|(t, _)| *t == tag);
        at.map(|at| state.custom.upgrading.remove(at).1)
    };
    if let Some(id) = upgrading {
        let object = wrap(context, id).as_object().expect("a node's object");
        object.set_prototype(Some(prototype));
        return Ok(object.into());
    }
    let id = with_dom(context, |dom| dom.create_element(&tag));
    let object = verve::dom_events::host_target(prototype, super::node::host_id_of(id));
    super::node::adopt(context, id, &object);
    shared(context).borrow_mut().custom.nodes.insert(id);
    Ok(object.into())
}

/// `createElement` for a defined tag: an element of its class, which, as the specification
/// asks, comes out of the constructor with no attributes and no children.
pub fn create(context: &mut Context, class: &JsObject) -> JsResult<JsValue> {
    let element = class.construct(&[], None, context)?;
    let id = super::node_id(&element.clone().into());
    let dirty = id.is_some_and(|id| {
        read_dom(context, |dom| {
            !dom.children(id).is_empty() || !dom.attributes(id).is_empty()
        })
    });
    if dirty {
        return Err(dom_error(
            context,
            "The result must not have attributes or children",
            "NotSupportedError",
        ));
    }
    Ok(element.into())
}

/// Calls `name` on the element if its class has it; a throw is reported, not rethrown.
fn call_back(context: &mut Context, id: NodeId, name: &str, args: &[JsValue]) {
    let Some(object) = existing_wrapper(context, id) else {
        return;
    };
    let result = object
        .get(JsString::from(name), context)
        .and_then(|callback| match callback.is_callable() {
            true => verve::call(context, &callback, &object.into(), args).map(drop),
            false => Ok(()),
        });
    if let Err(error) = result {
        verve::report(context, &error);
    }
}

/// What upgrading `id` needs.
struct Plan {
    tag: String,
    class: JsObject,
    /// The observed attributes the element has, with their values.
    attrs: Vec<(String, String)>,
    /// Whether it is in the document.
    connected: bool,
}

/// What upgrading `id` takes, or `None` when it is no element to upgrade.
fn upgrade_plan(context: &Context, id: NodeId) -> Option<Plan> {
    let state = shared(context);
    let state = state.borrow();
    let node = state.dom.get(id)?;
    if node.namespace != Namespace::Html || state.custom.nodes.contains(&id) {
        return None;
    }
    let tag = node.tag()?.to_owned();
    let definition = state.custom.named(&tag)?;
    let attrs = (state.dom.attributes(id).into_iter())
        .filter(|(name, _)| definition.observed.contains(name))
        .collect();
    let class = definition.class.clone();
    Some(Plan {
        tag,
        class,
        attrs,
        connected: state.dom.is_connected(id),
    })
}

/// Runs the class defined for `id`'s tag on it, if there is one and it is not an instance yet,
/// and tells it what it has. Answers whether it did, `connectedCallback` included.
fn upgrade(context: &mut Context, id: NodeId) -> bool {
    let Some(Plan {
        tag,
        class,
        attrs,
        connected,
    }) = upgrade_plan(context, id)
    else {
        return false;
    };
    shared(context)
        .borrow_mut()
        .custom
        .upgrading
        .push((tag, id));
    let object = wrap(context, id);
    let built = class.construct(&[], None, context);
    shared(context)
        .borrow_mut()
        .custom
        .upgrading
        .retain(|(_, n)| *n != id);
    let error = match built {
        Ok(built) if JsValue::from(built.clone()) == object => None,
        Ok(_) => Some(type_error("The constructor did not return the element")),
        Err(error) => Some(error),
    };
    if let Some(error) = error {
        verve::report(context, &error);
        return false;
    }
    shared(context).borrow_mut().custom.nodes.insert(id);
    for (name, value) in attrs {
        let args = [js_text(&name), JsValue::null(), js_text(&value)];
        call_back(context, id, "attributeChangedCallback", &args);
    }
    if connected {
        call_back(context, id, "connectedCallback", &[]);
    }
    true
}

/// Whether `id` is a custom element listening for `name`.
fn observes(context: &Context, id: NodeId, name: &str) -> bool {
    let state = shared(context);
    let state = state.borrow();
    let tag = state.dom.get(id).and_then(|n| n.tag());
    let observed = tag
        .and_then(|tag| state.custom.named(tag))
        .map(|d| &d.observed);
    state.custom.nodes.contains(&id) && observed.is_some_and(|o| o.iter().any(|n| n == name))
}

/// What a change in the DOM tells custom elements: attributes that changed, then subtrees that
/// joined the document (upgraded, then `connectedCallback`) or left it (`disconnectedCallback`).
pub fn react(context: &mut Context, attrs: Vec<AttrChange>, moved: Vec<(NodeId, bool)>) {
    for change in attrs {
        if observes(context, change.node, &change.name) {
            let value = |v: Option<String>| v.map_or(JsValue::null(), |v| js_text(&v));
            let args = [js_text(&change.name), value(change.old), value(change.new)];
            call_back(context, change.node, "attributeChangedCallback", &args);
        }
    }
    // The parser reports each element it appends: a subtree already told is not told again.
    let mut told = HashSet::new();
    for (root, connected) in moved {
        if told.insert((root, connected)) {
            let ids = read_dom(context, |dom| dom.shadow_including_tree(root));
            for id in ids
                .into_iter()
                .filter(|id| *id == root || told.insert((*id, connected)))
            {
                moved_element(context, id, connected);
            }
        }
    }
}

/// An element joined the document (`connected`) or left it: upgraded if it is not yet an
/// instance, then told.
fn moved_element(context: &mut Context, id: NodeId, connected: bool) {
    if connected && upgrade(context, id) {
        return;
    }
    if shared(context).borrow().custom.nodes.contains(&id) {
        let name = if connected {
            "connectedCallback"
        } else {
            "disconnectedCallback"
        };
        call_back(context, id, name, &[]);
    }
}

/// The attributes a class lists in its static `observedAttributes`.
fn observed_attributes(class: &JsObject, context: &mut Context) -> JsResult<Vec<String>> {
    let value = class.get(JsString::from("observedAttributes"), context)?;
    if value.is_undefined() {
        return Ok(Vec::new());
    }
    list_of(&value, context)?
        .iter()
        .map(|name| string(name, context))
        .collect()
}

fn define(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = text_of(&arg(args, 0), context)?;
    let Some(class) = arg(args, 1).as_object().filter(|c| c.is_constructor()) else {
        return Err(type_error(
            "Failed to execute 'define' on 'CustomElementRegistry': parameter 2 is not a constructor.",
        ));
    };
    let refuse = |context: &mut Context, why: String, kind: &str| {
        let message = format!("Failed to execute 'define' on 'CustomElementRegistry': {why}");
        dom_error(context, &message, kind)
    };
    if !is_custom_element_name(&name) {
        let why = format!("\"{name}\" is not a valid custom element name");
        return Err(refuse(context, why, "SyntaxError"));
    }
    if class_of(context, &name).is_some() {
        let why = format!("the name \"{name}\" has already been used with this registry");
        return Err(refuse(context, why, "NotSupportedError"));
    }
    if tag_of(context, &class).is_some() {
        let why = "this constructor has already been used with this registry".to_owned();
        return Err(refuse(context, why, "NotSupportedError"));
    }
    let observed = observed_attributes(&class, context)?;
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.dom.watch_attrs |= !observed.is_empty();
        let name = name.clone();
        state.custom.definitions.push(Definition {
            name,
            class: class.clone(),
            observed,
        });
    }
    let candidates: Vec<NodeId> = read_dom(context, |dom| {
        let tagged = |id: &NodeId| dom.get(*id).and_then(|n| n.tag()) == Some(name.as_str());
        dom.shadow_including_tree(dom.document)
            .into_iter()
            .filter(tagged)
            .collect()
    });
    for id in candidates {
        upgrade(context, id);
    }
    // Whoever waited for it hears now.
    let waiting: Vec<JsValue> = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut state.custom.waiting)
            .into_iter()
            .partition(|(n, _)| *n == name);
        state.custom.waiting = rest;
        ready.into_iter().map(|(_, resolve)| resolve).collect()
    };
    for resolve in waiting {
        let class = class.clone().into();
        verve::call(context, &resolve, &JsValue::undefined(), &[class])?;
    }
    Ok(JsValue::undefined())
}

/// `customElements`' methods.
pub const METHODS: &[Method] = &[
    ("define", 3, define),
    ("get", 1, |_, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        Ok(class_of(cx, &name).map_or(JsValue::undefined(), JsValue::from))
    }),
    ("getName", 1, |_, args, cx| {
        let name = arg(args, 0)
            .as_object()
            .and_then(|class| tag_of(cx, &class));
        Ok(name.map_or(JsValue::null(), |n| js_text(&n)))
    }),
    ("whenDefined", 1, |_, args, cx| {
        let name = text_of(&arg(args, 0), cx)?;
        if let Some(class) = class_of(cx, &name) {
            return super::resolved(class.into(), cx);
        }
        let (promise, resolvers) = JsPromise::new_pending(cx);
        shared(cx)
            .borrow_mut()
            .custom
            .waiting
            .push((name, resolvers.resolve.into()));
        Ok(promise.into())
    }),
    ("upgrade", 1, |_, args, cx| {
        let root = super::node_arg(args, 0, "upgrade", "CustomElementRegistry")?;
        for id in read_dom(cx, |dom| dom.shadow_including_tree(root)) {
            upgrade(cx, id);
        }
        Ok(JsValue::undefined())
    }),
];
