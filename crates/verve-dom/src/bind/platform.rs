//! The rest of the web platform libraries take for granted, beside the DOM proper:
//!
//! - `Range` and `Selection`;
//! - `attributes` (`NamedNodeMap`, `Attr`);
//! - shadow roots (held, and drawn as their host's content is not: see the crate's README);
//! - custom elements: `customElements.define`, an element of the class made by
//!   `createElement`, `connectedCallback` and `disconnectedCallback`;
//! - a `<template>`'s `content`;
//! - the CSSOM: a `<style>`'s `sheet`, `document.styleSheets`, `new CSSStyleSheet()` and
//!   `adoptedStyleSheets`, `getComputedStyle` through the cascade, `CSS.escape`/`supports`;
//! - a `<canvas>`'s 2D context — one that keeps its state and measures text, but paints
//!   nothing — and no WebGL;
//! - `DOMMatrix`, `DOMPoint`, `DOMRect`, `Path2D`; `DataTransfer`; `Image`, `Audio`, `Option`;
//!   `document.fonts`; an element's `animate`.
//!
//! Lifted from su-editor's Obsidian host.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{FunctionObjectBuilder, builtins::JsUint8ClampedArray},
    property::{Attribute, PropertyDescriptor},
};

use super::node::{ancestry, read_dom, with_dom, wrap, wrap_opt};
use super::{
    Accessor, ClassSpec, Kind, Method, arg, arg_string, array, class, constructor_of, function,
    get, handle_of, illegal_constructor, instance, native_instance, node_id, node_of,
    opt_string, plain_object, set, string, text, type_error,
};
use crate::dom::{Dom, NodeId, NodeKind};
use crate::state::shared;

// ─── Attributes ─────────────────────────────────────────────────────────────────────────────

/// An `Attr`: its name and value as they were when it was asked for, and its element.
pub fn attr_object(context: &mut Context, owner: &JsValue, name: &str, value: &str, _document: &JsValue) -> JsResult<JsValue> {
    let attr = native_instance(context, "Attr", Kind::Plain, 0);
    let (prefix, local) = match name.split_once(':') {
        Some((prefix, local)) => (text(prefix), local),
        None => (JsValue::null(), name),
    };
    for (key, value) in [
        ("name", text(name)),
        ("nodeName", text(name)),
        ("localName", text(local)),
        ("prefix", prefix),
        ("namespaceURI", JsValue::null()),
        ("value", text(value)),
        ("nodeValue", text(value)),
        ("textContent", text(value)),
        ("specified", true.into()),
        ("nodeType", 2.into()),
        ("ownerElement", owner.clone()),
    ] {
        attr.define_property_or_throw(
            JsString::from(key),
            PropertyDescriptor::builder().value(value).writable(true).enumerable(true).configurable(true),
            context,
        )?;
    }
    Ok(attr.into())
}

/// `element.attributes`: an array of `Attr`s with a named node map's methods, each also under
/// its name, as `attributes.href`.
pub fn attributes(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let pairs = read_dom(context, |dom| dom.attributes(id));
    let mut items = Vec::new();
    for (name, value) in &pairs {
        items.push(attr_object(context, this, name, value, &JsValue::undefined())?);
    }
    let list = array(items.clone(), context);
    if let Some(object) = list.as_object() {
        let prototype = shared(context).borrow().prototype("NamedNodeMap");
        object.set_prototype(Some(prototype));
        super::hidden(&object, js_string!("__owner"), this.clone(), context)?;
        for ((name, _), attr) in pairs.iter().zip(items) {
            if name.parse::<u32>().is_ok() || object.has_own_property(JsString::from(name.as_str()), context)? {
                continue;
            }
            super::hidden(&object, JsString::from(name.as_str()), attr, context)?;
        }
    }
    Ok(list)
}

const NAMED_NODE_MAP_METHODS: &[Method] = &[
    ("item", 1, |this, args, cx| {
        let at = arg(args, 0).to_number(cx)? as u32;
        let found = match this.as_object() {
            Some(list) => list.get(at, cx)?,
            None => JsValue::undefined(),
        };
        Ok(if found.is_undefined() { JsValue::null() } else { found })
    }),
    ("getNamedItem", 1, |this, args, cx| {
        let name = arg_string(args, 0, cx)?;
        for attr in super::list(this, cx)? {
            if string(&get(&attr, "name", cx)?, cx)?.eq_ignore_ascii_case(&name) {
                return Ok(attr);
            }
        }
        Ok(JsValue::null())
    }),
    ("getNamedItemNS", 2, |this, args, cx| super::call_method(this, "getNamedItem", &[arg(args, 1)], cx)),
    ("removeNamedItem", 1, |this, args, cx| {
        let owner = get(this, "__owner", cx)?;
        super::call_method(&owner, "removeAttribute", &[arg(args, 0)], cx)
    }),
    ("setNamedItem", 1, |this, args, cx| {
        let owner = get(this, "__owner", cx)?;
        let attr = arg(args, 0);
        let name = get(&attr, "name", cx)?;
        let value = get(&attr, "value", cx)?;
        super::call_method(&owner, "setAttribute", &[name, value], cx)?;
        Ok(JsValue::null())
    }),
];

const ATTRIBUTE_METHODS: &[Method] = &[
    ("getAttributeNames", 0, |this, _, cx| {
        let id = node_of(this)?;
        let names: Vec<JsValue> = read_dom(cx, |dom| dom.attributes(id)).into_iter().map(|(n, _)| text(&n)).collect();
        Ok(array(names, cx))
    }),
    ("hasAttributes", 0, |this, _, cx| {
        let id = node_of(this)?;
        Ok((!read_dom(cx, |dom| dom.attributes(id)).is_empty()).into())
    }),
    ("getAttributeNode", 1, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 0, cx)?;
        match read_dom(cx, |dom| dom.get_attr(id, &name)) {
            Some(value) => attr_object(cx, this, &name, &value, &JsValue::undefined()),
            None => Ok(JsValue::null()),
        }
    }),
    ("setAttributeNode", 1, |this, args, cx| {
        let attr = arg(args, 0);
        let name = get(&attr, "name", cx)?;
        let value = get(&attr, "value", cx)?;
        super::call_method(this, "setAttribute", &[name, value], cx)?;
        Ok(JsValue::null())
    }),
    // The namespaced forms, for SVG's `xlink:href` and the like: the local name is the
    // attribute, as an HTML document keeps it.
    ("getAttributeNS", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 1, cx)?;
        let found = read_dom(cx, |dom| {
            dom.get_attr(id, &name).or_else(|| {
                dom.attributes(id).into_iter().find(|(k, _)| k.rsplit(':').next() == Some(name.as_str())).map(|(_, v)| v)
            })
        });
        Ok(found.map_or(JsValue::null(), |v| text(&v)))
    }),
    ("setAttributeNS", 3, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 1, cx)?;
        let value = arg_string(args, 2, cx)?;
        with_dom(cx, |dom| dom.set_attr(id, &name, &value));
        Ok(JsValue::undefined())
    }),
    ("removeAttributeNS", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 1, cx)?;
        let qualified = read_dom(cx, |dom| {
            dom.attributes(id).into_iter().find(|(k, _)| *k == name || k.rsplit(':').next() == Some(name.as_str())).map(|(k, _)| k)
        });
        let name = qualified.unwrap_or(name);
        with_dom(cx, |dom| dom.remove_attr(id, &name));
        Ok(JsValue::undefined())
    }),
    ("hasAttributeNS", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 1, cx)?;
        Ok(read_dom(cx, |dom| {
            dom.get_attr(id, &name).is_some()
                || dom.attributes(id).iter().any(|(k, _)| k.rsplit(':').next() == Some(name.as_str()))
        })
        .into())
    }),
];

// ─── Shadow roots ───────────────────────────────────────────────────────────────────────────

/// `element.attachShadow({ mode })`: a fragment of its own the element hosts, outside its
/// children.
pub fn attach_shadow(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = node_of(this)?;
    if shared(context).borrow().shadow_roots.contains_key(&host) {
        return Err(super::dom_error(
            context,
            "Failed to execute 'attachShadow' on 'Element': Shadow root cannot be created on a host which already hosts a shadow tree.",
            "NotSupportedError",
        ));
    }
    let mode = get(&arg(args, 0), "mode", context)?;
    let mode = if mode.is_undefined() { "open".to_owned() } else { string(&mode, context)? };
    let root = with_dom(context, Dom::create_fragment);
    {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.shadow_roots.insert(host, (root, mode == "open"));
        state.shadow_hosts.insert(root, host);
    }
    let object = wrap(context, root);
    let root_object = object.as_object().expect("a shadow root");
    // The host keeps its root alive, and the root its host.
    if let Some(host_object) = this.as_object() {
        verve::dom_events::hold(&host_object, |held| held.push(root_object.clone()));
        verve::dom_events::hold(&root_object, |held| held.push(host_object.clone()));
    }
    set(&root_object, "mode", text(&mode), context)?;
    let sheets = array(Vec::new(), context);
    set(&root_object, "adoptedStyleSheets", sheets, context)?;
    set(&root_object, "delegatesFocus", get(&arg(args, 0), "delegatesFocus", context)?.to_boolean(), context)?;
    Ok(object)
}

const SHADOW_ROOT_ACCESSORS: &[Accessor] = &[
    (
        "host",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            let host = shared(cx).borrow().shadow_hosts.get(&root).copied();
            Ok(wrap_opt(cx, host))
        }),
        None,
    ),
    (
        "innerHTML",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| crate::html::inner(dom, root))))
        }),
        Some(|this, args, cx| {
            let root = node_of(this)?;
            let markup = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| {
                dom.empty(root);
                crate::html::parse_into(dom, root, &markup);
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "activeElement",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            let focused = read_dom(cx, |dom| dom.focused.filter(|f| dom.contains(root, *f)));
            Ok(wrap_opt(cx, focused))
        }),
        None,
    ),
];

// ─── Custom elements ────────────────────────────────────────────────────────────────────────

/// The class defined for a tag, if one was.
pub fn custom_class(context: &Context, tag: &str) -> Option<JsObject> {
    shared(context)
        .borrow()
        .custom_elements
        .iter()
        .find(|(name, _)| name == tag)
        .map(|(_, c)| c.clone())
}

/// `HTMLElement` constructed: only as `super()` of a class `customElements.define` was given,
/// making an element of that class's tag.
pub fn construct_html_element(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(target) = new_target.as_object() else {
        return Err(type_error("Illegal constructor"));
    };
    let tag = shared(context)
        .borrow()
        .custom_elements
        .iter()
        .find(|(_, class)| JsObject::equals(class, &target))
        .map(|(name, _)| name.clone());
    let Some(tag) = tag else {
        return Err(type_error("Illegal constructor"));
    };
    let prototype = super::prototype_of_target(new_target, context)?;
    let id = with_dom(context, |dom| dom.create_element(&tag));
    let object = verve::dom_events::host_target(prototype, super::node::host_id_of(id));
    super::node::adopt(context, id, &object);
    shared(context).borrow_mut().custom_nodes.insert(id);
    Ok(object.into())
}

const CUSTOM_ELEMENTS_METHODS: &[Method] = &[
    ("define", 3, |_, args, cx| {
        let name = arg_string(args, 0, cx)?;
        let Some(class) = arg(args, 1).as_object().filter(|c| c.is_constructor()) else {
            return Err(type_error("Failed to execute 'define' on 'CustomElementRegistry': parameter 2 is not a constructor."));
        };
        if !name.contains('-') || name.chars().next().is_none_or(|c| !c.is_ascii_lowercase()) {
            return Err(super::dom_error(cx, &format!("Failed to execute 'define' on 'CustomElementRegistry': \"{name}\" is not a valid custom element name"), "SyntaxError"));
        }
        if custom_class(cx, &name).is_some() {
            return Err(super::dom_error(cx, &format!("Failed to execute 'define' on 'CustomElementRegistry': the name \"{name}\" has already been used with this registry"), "NotSupportedError"));
        }
        shared(cx).borrow_mut().custom_elements.push((name.clone(), class.clone()));
        // Whoever waited for it hears now.
        let waiting: Vec<JsValue> = {
            let state = shared(cx);
            let mut state = state.borrow_mut();
            let (ready, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut state.custom_waiting).into_iter().partition(|(n, _)| *n == name);
            state.custom_waiting = rest;
            ready.into_iter().map(|(_, resolve)| resolve).collect()
        };
        for resolve in waiting {
            verve::call(cx, &resolve, &JsValue::undefined(), &[class.clone().into()])?;
        }
        Ok(JsValue::undefined())
    }),
    ("get", 1, |_, args, cx| {
        let name = arg_string(args, 0, cx)?;
        Ok(custom_class(cx, &name).map_or(JsValue::undefined(), JsValue::from))
    }),
    ("getName", 1, |_, args, cx| {
        let Some(class) = arg(args, 0).as_object() else { return Ok(JsValue::null()) };
        let name = shared(cx).borrow().custom_elements.iter().find(|(_, c)| JsObject::equals(c, &class)).map(|(n, _)| n.clone());
        Ok(name.map_or(JsValue::null(), |n| text(&n)))
    }),
    ("whenDefined", 1, |_, args, cx| {
        let name = arg_string(args, 0, cx)?;
        if let Some(class) = custom_class(cx, &name) {
            return super::resolved(class.into(), cx);
        }
        let (promise, resolvers) = boa_engine::object::builtins::JsPromise::new_pending(cx);
        shared(cx).borrow_mut().custom_waiting.push((name, resolvers.resolve.into()));
        Ok(promise.into())
    }),
    ("upgrade", 1, |_, _, _| Ok(JsValue::undefined())),
];

/// Tells custom elements they joined or left the document: `connectedCallback` and
/// `disconnectedCallback`, for the subtrees that moved.
pub fn custom_moved(context: &mut Context, moved: Vec<(NodeId, bool)>) {
    for (root, connected) in moved {
        let ids: Vec<NodeId> = {
            let state = shared(context);
            let state = state.borrow();
            state.dom.subtree(root).into_iter().filter(|id| state.custom_nodes.contains(id)).collect()
        };
        for id in ids {
            let Some(object) = super::node::existing_wrapper(context, id) else { continue };
            let name = if connected { "connectedCallback" } else { "disconnectedCallback" };
            let callback = object.get(JsString::from(name), context).unwrap_or_default();
            if callback.is_callable()
                && let Err(error) = verve::call(context, &callback, &object.clone().into(), &[])
            {
                verve::report(context, &error);
            }
        }
    }
}

// ─── A template's content ───────────────────────────────────────────────────────────────────

/// A `<template>`'s content fragment, made the first time it is asked for, holding what was
/// put in the template since; `None` for anything else.
pub fn template_target(context: &mut Context, id: NodeId) -> Option<NodeId> {
    if read_dom(context, |dom| dom.get(id).and_then(|n| n.tag()) != Some("template")) {
        return None;
    }
    let (fragment, fresh) = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let existing = state.template_contents.get(&id).copied().filter(|f| state.dom.exists(*f));
        match existing {
            Some(fragment) => (fragment, false),
            None => {
                let fragment = state.dom.create_fragment();
                state.template_contents.insert(id, fragment);
                (fragment, true)
            }
        }
    };
    if fresh {
        // The template keeps its content alive.
        let template = wrap(context, id).as_object().expect("an element");
        let content = wrap(context, fragment).as_object().expect("a fragment");
        verve::dom_events::hold(&template, |held| held.push(content.clone()));
    }
    with_dom(context, |dom| {
        let children = dom.children(id).to_vec();
        for child in children {
            dom.append(fragment, child);
        }
    });
    Some(fragment)
}

pub const TEMPLATE_ACCESSORS: &[Accessor] = &[(
    "content",
    Some(|this, _, cx| {
        let template = node_of(this)?;
        let fragment = template_target(cx, template);
        Ok(wrap_opt(cx, fragment))
    }),
    None,
)];

// ─── The CSSOM ──────────────────────────────────────────────────────────────────────────────

fn rule_object(context: &mut Context, rule: &str, sheet: &JsValue) -> JsResult<JsValue> {
    let object = plain_object(context);
    set(&object, "cssText", text(rule), context)?;
    if let Some(brace) = rule.find('{') {
        let selector = rule[..brace].trim();
        set(&object, "selectorText", text(selector), context)?;
        let body = rule[brace + 1..].trim_end().trim_end_matches('}').trim();
        let style = plain_object(context);
        set(&style, "cssText", text(body), context)?;
        for declaration in crate::css::declarations(body) {
            set(&style, &declaration.name, text(&declaration.value), context)?;
            let camel = crate::dom::camel_name(&declaration.name);
            set(&style, &camel, text(&declaration.value), context)?;
        }
        let get_value = function(context, "getPropertyValue", 1, |this, args, cx| {
            let name = arg_string(args, 0, cx)?;
            let value = get(this, &name, cx)?;
            Ok(if value.is_undefined() { text("") } else { value })
        });
        set(&style, "getPropertyValue", get_value, context)?;
        set(&object, "style", style, context)?;
        let kind = if selector.starts_with("@media") { 4 } else if selector.starts_with('@') { 0 } else { 1 };
        set(&object, "type", kind, context)?;
        if kind == 4 {
            set(&object, "conditionText", text(selector.trim_start_matches("@media").trim()), context)?;
        }
    } else {
        set(&object, "type", if rule.starts_with("@import") { 3 } else { 0 }, context)?;
    }
    set(&object, "parentStyleSheet", sheet.clone(), context)?;
    Ok(object.into())
}

/// The element's rules, changed by `f`, written back as its text.
fn change_element_rules(context: &mut Context, id: NodeId, f: impl FnOnce(&mut Vec<String>)) {
    with_dom(context, |dom| {
        let mut rules = crate::css::split_rules(&dom.text_content(id));
        f(&mut rules);
        dom.set_text(id, &rules.join("\n"));
    });
}

fn sheet_rules(this: &JsValue, context: &mut Context) -> JsResult<Vec<String>> {
    if let Some(id) = handle_of(this, Kind::Sheet) {
        return Ok(crate::css::split_rules(&read_dom(context, |dom| dom.text_content(id))));
    }
    if let Some(id) = handle_of(this, Kind::Constructed) {
        let rules = shared(context).borrow().constructed_sheets.get(&id).cloned().unwrap_or_default();
        return Ok(rules.iter().flat_map(|r| crate::css::split_rules(r)).collect());
    }
    Err(type_error("Illegal invocation: not a CSSStyleSheet"))
}

/// Changes a sheet's rules: a `<style>`'s text, or a made sheet's list.
fn change_rules(this: &JsValue, context: &mut Context, f: impl FnOnce(&mut Vec<String>)) -> JsResult<()> {
    if let Some(id) = handle_of(this, Kind::Sheet) {
        change_element_rules(context, id, f);
        return Ok(());
    }
    if let Some(id) = handle_of(this, Kind::Constructed) {
        let state = shared(context);
        let mut state = state.borrow_mut();
        if let Some(rules) = state.constructed_sheets.get_mut(&id) {
            let mut split: Vec<String> = rules.iter().flat_map(|r| crate::css::split_rules(r)).collect();
            f(&mut split);
            *rules = split;
        }
        state.sheets_changed();
        return Ok(());
    }
    Err(type_error("Illegal invocation: not a CSSStyleSheet"))
}

const SHEET_ACCESSORS: &[Accessor] = &[
    (
        "cssRules",
        Some(|this, _, cx| {
            let rules = sheet_rules(this, cx)?;
            let mut out = Vec::new();
            for rule in rules {
                out.push(rule_object(cx, &rule, this)?);
            }
            Ok(array(out, cx))
        }),
        None,
    ),
    (
        "rules",
        Some(|this, _, cx| {
            let rules = sheet_rules(this, cx)?;
            let mut out = Vec::new();
            for rule in rules {
                out.push(rule_object(cx, &rule, this)?);
            }
            Ok(array(out, cx))
        }),
        None,
    ),
    (
        "ownerNode",
        Some(|this, _, cx| {
            let id = handle_of(this, Kind::Sheet);
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    ("href", Some(|_, _, _| Ok(JsValue::null())), None),
    ("type", Some(|_, _, _| Ok(text("text/css"))), None),
    ("title", Some(|_, _, _| Ok(JsValue::null())), None),
    ("parentStyleSheet", Some(|_, _, _| Ok(JsValue::null())), None),
    ("ownerRule", Some(|_, _, _| Ok(JsValue::null())), None),
    ("disabled", Some(|_, _, _| Ok(false.into())), Some(|_, _, _| Ok(JsValue::undefined()))),
    (
        "media",
        Some(|_, _, cx| {
            let media = array(Vec::new(), cx);
            if let Some(object) = media.as_object() {
                set(&object, "mediaText", text(""), cx)?;
            }
            Ok(media)
        }),
        None,
    ),
];

const SHEET_METHODS: &[Method] = &[
    ("insertRule", 2, |this, args, cx| {
        let rule = arg_string(args, 0, cx)?;
        let at = match arg(args, 1) {
            i if i.is_undefined() => 0,
            i => i.to_number(cx)? as usize,
        };
        let mut placed = 0;
        change_rules(this, cx, |rules| {
            placed = at.min(rules.len());
            rules.insert(placed, rule);
        })?;
        Ok(placed.into())
    }),
    ("deleteRule", 1, |this, args, cx| {
        let at = arg(args, 0).to_number(cx)? as usize;
        change_rules(this, cx, |rules| {
            if at < rules.len() {
                rules.remove(at);
            }
        })?;
        Ok(JsValue::undefined())
    }),
    ("addRule", 2, |this, args, cx| {
        let selector = arg_string(args, 0, cx)?;
        let body = arg_string(args, 1, cx)?;
        change_rules(this, cx, |rules| rules.push(format!("{selector} {{ {body} }}")))?;
        Ok((-1).into())
    }),
    ("removeRule", 1, |this, args, cx| super::call_method(this, "deleteRule", args, cx)),
    ("replaceSync", 1, |this, args, cx| {
        let css = arg_string(args, 0, cx)?;
        change_rules(this, cx, |rules| *rules = crate::css::split_rules(&css))?;
        Ok(JsValue::undefined())
    }),
    ("replace", 1, |this, args, cx| {
        let css = arg_string(args, 0, cx)?;
        change_rules(this, cx, |rules| *rules = crate::css::split_rules(&css))?;
        super::resolved(this.clone(), cx)
    }),
];

/// The sheet of a `<style>` element: the same object each time it is asked for.
fn element_sheet(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let element = wrap(context, id);
    let Some(element_object) = element.as_object() else { return Ok(JsValue::null()) };
    let key = js_string!("__sheet");
    let known = element_object.get(key.clone(), context)?;
    if known.is_object() {
        return Ok(known);
    }
    let sheet = native_instance(context, "CSSStyleSheet", Kind::Sheet, id);
    super::hidden(&element_object, key, sheet.clone(), context)?;
    Ok(sheet.into())
}

pub const STYLE_ELEMENT_ACCESSORS: &[Accessor] = &[(
    "sheet",
    Some(|this, _, cx| {
        let id = node_of(this)?;
        // Only a `<style>` in a document has a sheet.
        if !read_dom(cx, |dom| dom.document_of(id).is_some()) {
            return Ok(JsValue::null());
        }
        element_sheet(cx, id)
    }),
    None,
)];

/// `document.styleSheets`: every `<style>` in the document's, in order.
pub fn style_sheets(context: &mut Context) -> JsResult<JsValue> {
    let ids: Vec<NodeId> = read_dom(context, |dom| dom.style_sheets().into_iter().map(|(id, _)| id).collect());
    let mut out = Vec::new();
    for id in ids {
        out.push(element_sheet(context, id)?);
    }
    let list = array(out, context);
    if let Some(object) = list.as_object() {
        let item = function(context, "item", 1, |this, args, cx| {
            let at = arg(args, 0).to_number(cx)? as u32;
            match this.as_object() {
                Some(list) => list.get(at, cx),
                None => Ok(JsValue::null()),
            }
        });
        super::hidden(&object, js_string!("item"), item, context)?;
    }
    Ok(list)
}

pub fn adopted_sheets(context: &mut Context) -> JsResult<JsValue> {
    let sheets: Vec<JsValue> = shared(context).borrow().adopted_sheets.iter().map(|(_, s)| s.clone().into()).collect();
    Ok(array(sheets, context))
}

pub fn set_adopted_sheets(given: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let mut adopted = Vec::new();
    for sheet in super::list(given, context)? {
        match (handle_of(&sheet, Kind::Constructed), sheet.as_object()) {
            (Some(id), Some(object)) => adopted.push((id, object)),
            _ => {
                return Err(type_error("Failed to set the 'adoptedStyleSheets' property on 'Document': Failed to convert value to 'CSSStyleSheet'."));
            }
        }
    }
    let state = shared(context);
    let mut state = state.borrow_mut();
    state.adopted_sheets = adopted;
    state.sheets_changed();
    Ok(JsValue::undefined())
}

fn construct_sheet(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let id = state.id();
        state.constructed_sheets.insert(id, Vec::new());
        id
    };
    Ok(instance(new_target, Kind::Constructed, id, context)?.into())
}

// ─── Computed style ─────────────────────────────────────────────────────────────────────────

/// A node's computed value of `property`: the cascade over every sheet, `var()` resolved, and
/// the property's initial value when nothing set it.
pub fn computed_value(context: &Context, id: NodeId, property: &str) -> String {
    let state = shared(context);
    let mut state = state.borrow_mut();
    let styler = state.styler();
    let style = crate::css::style_of(&state.dom, &styler, id);
    if let Some(value) = style.get(property) {
        return value.clone();
    }
    let tag = state.dom.get(id).and_then(|n| n.tag()).map(str::to_owned);
    crate::css::initial_value(property, tag.as_deref()).unwrap_or("").to_owned()
}

const COMPUTED_METHODS: &[Method] = &[
    ("getPropertyValue", 1, |this, args, cx| {
        let id = handle_of(this, Kind::Computed).ok_or_else(|| type_error("Illegal invocation"))?;
        let property = arg_string(args, 0, cx)?;
        Ok(text(computed_value(cx, id, property.trim()).trim()))
    }),
    ("getPropertyPriority", 1, |_, _, _| Ok(text(""))),
    ("item", 1, |this, args, cx| {
        let id = handle_of(this, Kind::Computed).ok_or_else(|| type_error("Illegal invocation"))?;
        let at = arg(args, 0).to_number(cx)? as usize;
        let state = shared(cx);
        let mut state = state.borrow_mut();
        let styler = state.styler();
        let style = crate::css::style_of(&state.dom, &styler, id);
        let mut names: Vec<&String> = style.keys().collect();
        names.sort();
        Ok(names.get(at).map_or(text(""), |n| text(n)))
    }),
    ("setProperty", 2, |_, _, cx| {
        Err(super::dom_error(cx, "Failed to execute 'setProperty' on 'CSSStyleDeclaration': These styles are computed, and therefore read-only.", "NoModificationAllowedError"))
    }),
    ("removeProperty", 1, |_, _, cx| {
        Err(super::dom_error(cx, "Failed to execute 'removeProperty' on 'CSSStyleDeclaration': These styles are computed, and therefore read-only.", "NoModificationAllowedError"))
    }),
];

/// `getComputedStyle(element)`: the element's style after the cascade, read as properties —
/// `style.color`, `style.getPropertyValue('--text-muted')`.
pub fn computed_style(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let target = native_instance(context, "CSSStyleDeclaration", Kind::Computed, id);
    let proxy = boa_engine::object::builtins::JsProxyBuilder::new(target)
        .get(|_, args, cx| {
            let target = arg(args, 0);
            let key = arg(args, 1);
            let Some(name) = key.as_string().map(|n| n.to_std_string_escaped()) else {
                return Ok(JsValue::undefined());
            };
            if let Some((_, length, f)) = COMPUTED_METHODS.iter().find(|(m, ..)| *m == name) {
                let method = function(cx, &name, *length, *f);
                return super::call_method(&method.into(), "bind", &[target], cx);
            }
            let id = handle_of(&target, Kind::Computed).unwrap_or_default();
            match name.as_str() {
                "constructor" => return get(&target, "constructor", cx),
                "length" => {
                    let state = shared(cx);
                    let mut state = state.borrow_mut();
                    let styler = state.styler();
                    return Ok(crate::css::style_of(&state.dom, &styler, id).len().into());
                }
                "cssText" => return Ok(text("")),
                "parentRule" => return Ok(JsValue::null()),
                _ => {}
            }
            let property = if name.contains('-') { name } else { crate::dom::css_name(&name) };
            Ok(text(computed_value(cx, id, &property).trim()))
        })
        .set(|_, _, _| Ok(true.into()))
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}

const CSS_FUNCTIONS: &[Method] = &[
    // CSS.escape, as the CSSOM writes it.
    ("escape", 1, |_, args, cx| {
        let value = arg_string(args, 0, cx)?;
        let mut out = String::new();
        let chars: Vec<char> = value.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            let code = c as u32;
            if code == 0 {
                out.push('\u{FFFD}');
            } else if (0x1..=0x1f).contains(&code) || code == 0x7f
                || (i == 0 && c.is_ascii_digit())
                || (i == 1 && c.is_ascii_digit() && chars[0] == '-')
            {
                out.push_str(&format!("\\{code:x} "));
            } else if i == 0 && c == '-' && chars.len() == 1 {
                out.push_str("\\-");
            } else if code >= 0x80 || c == '-' || c == '_' || c.is_ascii_alphanumeric() {
                out.push(c);
            } else {
                out.push('\\');
                out.push(c);
            }
        }
        Ok(text(&out))
    }),
    ("supports", 2, |_, args, cx| {
        // A selector is supported when it parses; a declaration, always.
        let first = arg_string(args, 0, cx)?;
        if let Some(selector) = first.trim().strip_prefix("selector(").and_then(|s| s.strip_suffix(')')) {
            return Ok(crate::css::SelectorList::parse(selector).is_ok().into());
        }
        Ok(true.into())
    }),
    ("registerProperty", 1, |_, _, _| Ok(JsValue::undefined())),
];

/// `document.fonts`: every font is loaded, as on a machine that has them all.
pub fn font_faces(context: &mut Context) -> JsResult<JsValue> {
    let global = context.global_object();
    let known = global.get(js_string!("__fonts"), context)?;
    if known.is_object() {
        return Ok(known);
    }
    let target = shared(context).borrow().class("EventTarget").expect("made").0;
    let fonts = target.construct(&[], None, context)?;
    let ready = super::resolved(fonts.clone().into(), context)?;
    set(&fonts, "ready", ready, context)?;
    set(&fonts, "status", text("loaded"), context)?;
    set(&fonts, "size", 0, context)?;
    let methods: &[Method] = &[
        ("check", 1, |_, _, _| Ok(true.into())),
        ("load", 1, |_, _, cx| {
            let empty = array(Vec::new(), cx);
            super::resolved(empty, cx)
        }),
        ("add", 1, |this, _, _| Ok(this.clone())),
        ("delete", 1, |_, _, _| Ok(false.into())),
        ("clear", 0, |_, _, _| Ok(JsValue::undefined())),
        ("forEach", 1, |_, _, _| Ok(JsValue::undefined())),
        ("has", 1, |_, _, _| Ok(false.into())),
    ];
    for (name, length, f) in methods {
        let made = function(context, name, *length, *f);
        set(&fonts, name, made, context)?;
    }
    super::hidden(&global, js_string!("__fonts"), fonts.clone(), context)?;
    Ok(fonts.into())
}

// ─── Range and Selection ────────────────────────────────────────────────────────────────────

fn construct_range(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let range = instance(new_target, Kind::Plain, 0, context)?;
    let document = context.global_object().get(js_string!("document"), context)?;
    for key in ["startContainer", "endContainer"] {
        set(&range, key, document.clone(), context)?;
    }
    set(&range, "startOffset", 0, context)?;
    set(&range, "endOffset", 0, context)?;
    Ok(range.into())
}

pub fn new_range(context: &mut Context) -> JsResult<JsValue> {
    let constructor = constructor_of(context, "Range").ok_or_else(|| type_error("Range is not installed"))?;
    Ok(constructor.construct(&[], None, context)?.into())
}

/// How many places a boundary may be at inside `id`: characters of text, children otherwise.
fn node_length(dom: &Dom, id: NodeId) -> usize {
    match dom.get(id).map(|n| &n.kind) {
        Some(NodeKind::Text(t)) | Some(NodeKind::Comment(t)) => t.encode_utf16().count(),
        Some(_) => dom.children(id).len(),
        None => 0,
    }
}

fn set_boundary(this: &JsValue, context: &mut Context, which: &str, node: JsValue, offset: u32) -> JsResult<()> {
    let Some(range) = this.as_object() else { return Ok(()) };
    set(&range, &format!("{which}Container"), node, context)?;
    set(&range, &format!("{which}Offset"), offset, context)?;
    Ok(())
}

fn boundary(this: &JsValue, context: &mut Context, which: &str) -> JsResult<(JsValue, u32)> {
    let node = get(this, &format!("{which}Container"), context)?;
    let offset = get(this, &format!("{which}Offset"), context)?.to_u32(context)?;
    Ok((node, offset))
}

/// The node's place in its parent: the parent, and the index there.
fn place_of(context: &mut Context, node: &JsValue) -> JsResult<Option<(JsValue, u32)>> {
    let id = node_of(node)?;
    let found = read_dom(context, |dom| {
        let parent = dom.parent(id)?;
        let index = dom.children(parent).iter().position(|c| *c == id)?;
        Some((parent, index as u32))
    });
    Ok(match found {
        Some((parent, index)) => Some((wrap(context, parent), index)),
        None => None,
    })
}

const RANGE_ACCESSORS: &[Accessor] = &[
    (
        "collapsed",
        Some(|this, _, cx| {
            let (start, start_offset) = boundary(this, cx, "start")?;
            let (end, end_offset) = boundary(this, cx, "end")?;
            Ok((node_id(&start) == node_id(&end) && start_offset == end_offset).into())
        }),
        None,
    ),
    (
        "commonAncestorContainer",
        Some(|this, _, cx| {
            let (start, _) = boundary(this, cx, "start")?;
            let (end, _) = boundary(this, cx, "end")?;
            let (Some(a), Some(b)) = (node_id(&start), node_id(&end)) else { return Ok(start) };
            let common = read_dom(cx, |dom| {
                let chain = ancestry(dom, a);
                ancestry(dom, b).into_iter().rev().find(|n| chain.contains(n))
            });
            Ok(common.map_or(start, |c| wrap(cx, c)))
        }),
        None,
    ),
];

fn split_text_units(content: &str, start: u32, end: u32) -> (String, String, String) {
    let units: Vec<u16> = content.encode_utf16().collect();
    let (a, b) = ((start as usize).min(units.len()), (end as usize).min(units.len()).max(start as usize).min(units.len()));
    (
        String::from_utf16_lossy(&units[..a]),
        String::from_utf16_lossy(&units[a..b]),
        String::from_utf16_lossy(&units[b..]),
    )
}

const RANGE_METHODS: &[Method] = &[
    ("setStart", 2, |this, args, cx| {
        let offset = arg(args, 1).to_u32(cx)?;
        set_boundary(this, cx, "start", arg(args, 0), offset)?;
        Ok(JsValue::undefined())
    }),
    ("setEnd", 2, |this, args, cx| {
        let offset = arg(args, 1).to_u32(cx)?;
        set_boundary(this, cx, "end", arg(args, 0), offset)?;
        Ok(JsValue::undefined())
    }),
    ("setStartBefore", 1, |this, args, cx| {
        if let Some((parent, index)) = place_of(cx, &arg(args, 0))? {
            set_boundary(this, cx, "start", parent, index)?;
        }
        Ok(JsValue::undefined())
    }),
    ("setStartAfter", 1, |this, args, cx| {
        if let Some((parent, index)) = place_of(cx, &arg(args, 0))? {
            set_boundary(this, cx, "start", parent, index + 1)?;
        }
        Ok(JsValue::undefined())
    }),
    ("setEndBefore", 1, |this, args, cx| {
        if let Some((parent, index)) = place_of(cx, &arg(args, 0))? {
            set_boundary(this, cx, "end", parent, index)?;
        }
        Ok(JsValue::undefined())
    }),
    ("setEndAfter", 1, |this, args, cx| {
        if let Some((parent, index)) = place_of(cx, &arg(args, 0))? {
            set_boundary(this, cx, "end", parent, index + 1)?;
        }
        Ok(JsValue::undefined())
    }),
    ("selectNode", 1, |this, args, cx| {
        if let Some((parent, index)) = place_of(cx, &arg(args, 0))? {
            set_boundary(this, cx, "start", parent.clone(), index)?;
            set_boundary(this, cx, "end", parent, index + 1)?;
        }
        Ok(JsValue::undefined())
    }),
    ("selectNodeContents", 1, |this, args, cx| {
        let node = arg(args, 0);
        let id = node_of(&node)?;
        let length = read_dom(cx, |dom| node_length(dom, id)) as u32;
        set_boundary(this, cx, "start", node.clone(), 0)?;
        set_boundary(this, cx, "end", node, length)?;
        Ok(JsValue::undefined())
    }),
    ("collapse", 1, |this, args, cx| {
        let to_start = arg(args, 0).to_boolean();
        let (node, offset) = boundary(this, cx, if to_start { "start" } else { "end" })?;
        set_boundary(this, cx, "start", node.clone(), offset)?;
        set_boundary(this, cx, "end", node, offset)?;
        Ok(JsValue::undefined())
    }),
    ("cloneRange", 0, |this, _, cx| {
        let made = new_range(cx)?;
        for which in ["start", "end"] {
            let (node, offset) = boundary(this, cx, which)?;
            set_boundary(&made, cx, which, node, offset)?;
        }
        Ok(made)
    }),
    // A fragment of what `html` says, parsed as the inside of the range's start would parse it.
    ("createContextualFragment", 1, |_, args, cx| {
        let markup = arg_string(args, 0, cx)?;
        let fragment = with_dom(cx, |dom| {
            let fragment = dom.create_fragment();
            crate::html::parse_into(dom, fragment, &markup);
            fragment
        });
        Ok(wrap(cx, fragment))
    }),
    ("insertNode", 1, |this, args, cx| {
        let (container, offset) = boundary(this, cx, "start")?;
        let node = node_of(&arg(args, 0))?;
        let target = node_of(&container)?;
        with_dom(cx, |dom| {
            if let Some(NodeKind::Text(content)) = dom.get(target).map(|n| n.kind.clone()) {
                // Into a text node: it is split there, and the node goes between the halves.
                let (head, _, tail) = split_text_units(&content, offset, offset);
                dom.set_text(target, &head);
                if let Some(parent) = dom.parent(target) {
                    let next = dom.sibling(target, 1, false);
                    dom.insert_before(parent, node, next);
                    if !tail.is_empty() {
                        let rest = dom.create_text(&tail);
                        let after = dom.sibling(node, 1, false);
                        dom.insert_before(parent, rest, after);
                    }
                }
            } else {
                let before = dom.children(target).get(offset as usize).copied();
                dom.insert_before(target, node, before);
            }
        });
        Ok(JsValue::undefined())
    }),
    ("surroundContents", 1, |this, args, cx| {
        let wrapper = node_of(&arg(args, 0))?;
        let (container, start) = boundary(this, cx, "start")?;
        let (_, end) = boundary(this, cx, "end")?;
        let parent = node_of(&container)?;
        with_dom(cx, |dom| {
            let children: Vec<NodeId> = dom.children(parent).iter().skip(start as usize).take(end.saturating_sub(start) as usize).copied().collect();
            let before = dom.children(parent).get(start as usize).copied();
            dom.insert_before(parent, wrapper, before);
            for child in children {
                dom.append(wrapper, child);
            }
        });
        Ok(JsValue::undefined())
    }),
    ("deleteContents", 0, |this, _, cx| {
        let (container, start) = boundary(this, cx, "start")?;
        let (end_container, end) = boundary(this, cx, "end")?;
        let (Some(parent), Some(end_parent)) = (node_id(&container), node_id(&end_container)) else { return Ok(JsValue::undefined()) };
        if parent != end_parent {
            return Ok(JsValue::undefined());
        }
        with_dom(cx, |dom| {
            if let Some(NodeKind::Text(content)) = dom.get(parent).map(|n| n.kind.clone()) {
                let (head, _, tail) = split_text_units(&content, start, end);
                dom.set_text(parent, &format!("{head}{tail}"));
            } else {
                let doomed: Vec<NodeId> = dom.children(parent).iter().skip(start as usize).take(end.saturating_sub(start) as usize).copied().collect();
                for child in doomed {
                    dom.detach(child);
                }
            }
        });
        set_boundary(this, cx, "end", container, start)?;
        Ok(JsValue::undefined())
    }),
    ("cloneContents", 0, |this, _, cx| {
        let (container, start) = boundary(this, cx, "start")?;
        let (_, end) = boundary(this, cx, "end")?;
        let parent = node_of(&container)?;
        let fragment = with_dom(cx, |dom| {
            let fragment = dom.create_fragment();
            if let Some(NodeKind::Text(content)) = dom.get(parent).map(|n| n.kind.clone()) {
                let (_, piece, _) = split_text_units(&content, start, end);
                let piece = dom.create_text(&piece);
                dom.append(fragment, piece);
            } else {
                let picked: Vec<NodeId> = dom.children(parent).iter().skip(start as usize).take(end.saturating_sub(start) as usize).copied().collect();
                for child in picked {
                    if let Some(copy) = dom.clone_node(child, true) {
                        dom.append(fragment, copy);
                    }
                }
            }
            fragment
        });
        Ok(wrap(cx, fragment))
    }),
    ("extractContents", 0, |this, _, cx| {
        let fragment = super::call_method(this, "cloneContents", &[], cx)?;
        super::call_method(this, "deleteContents", &[], cx)?;
        Ok(fragment)
    }),
    ("toString", 0, |this, _, cx| {
        let (container, start) = boundary(this, cx, "start")?;
        let (end_container, end) = boundary(this, cx, "end")?;
        let (Some(a), Some(b)) = (node_id(&container), node_id(&end_container)) else { return Ok(text("")) };
        Ok(text(&read_dom(cx, |dom| {
            if a == b {
                if let Some(NodeKind::Text(content)) = dom.get(a).map(|n| &n.kind) {
                    return split_text_units(content, start, end).1;
                }
                return dom.children(a).iter().skip(start as usize).take(end.saturating_sub(start) as usize).map(|c| dom.text_content(*c)).collect();
            }
            dom.text_content(a)
        })))
    }),
    ("getBoundingClientRect", 0, |this, _, cx| {
        let (container, _) = boundary(this, cx, "start")?;
        let element = read_dom(cx, |dom| node_id(&container).map(|id| if dom.get(id).is_some_and(|n| n.is_element()) { id } else { dom.parent(id).unwrap_or(id) }));
        match element {
            Some(id) => {
                let rect = super::element::painted(cx, id);
                super::element::rect_object(cx, rect)
            }
            None => make_rect(cx, [0.; 4]),
        }
    }),
    ("getClientRects", 0, |this, _, cx| {
        let rect = super::call_method(this, "getBoundingClientRect", &[], cx)?;
        Ok(array(vec![rect], cx))
    }),
    ("compareBoundaryPoints", 2, |_, _, _| Ok(0.into())),
    ("comparePoint", 2, |_, _, _| Ok(0.into())),
    ("isPointInRange", 2, |_, _, _| Ok(false.into())),
    ("intersectsNode", 1, |_, _, _| Ok(true.into())),
    ("detach", 0, |_, _, _| Ok(JsValue::undefined())),
];

/// The window's one selection.
pub fn selection(context: &mut Context) -> JsResult<JsValue> {
    let global = context.global_object();
    let key = js_string!("__selection");
    let known = global.get(key.clone(), context)?;
    if known.is_object() {
        return Ok(known);
    }
    let made = native_instance(context, "Selection", Kind::Plain, 0);
    let ranges = array(Vec::new(), context);
    set(&made, "__ranges", ranges, context)?;
    super::hidden(&global, key, made.clone(), context)?;
    Ok(made.into())
}

fn selection_ranges(this: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let ranges = get(this, "__ranges", context)?;
    super::list(&ranges, context)
}

fn set_selection_ranges(this: &JsValue, context: &mut Context, ranges: Vec<JsValue>) -> JsResult<()> {
    let list = array(ranges, context);
    if let Some(object) = this.as_object() {
        set(&object, "__ranges", list, context)?;
    }
    let document = context.global_object().get(js_string!("document"), context)?;
    super::events::fire(context, &document, "Event", "selectionchange", &[("bubbles", false.into())])?;
    Ok(())
}

const SELECTION_ACCESSORS: &[Accessor] = &[
    ("rangeCount", Some(|this, _, cx| Ok((selection_ranges(this, cx)?.len() as u32).into())), None),
    (
        "isCollapsed",
        Some(|this, _, cx| match selection_ranges(this, cx)?.first() {
            Some(range) => get(range, "collapsed", cx),
            None => Ok(true.into()),
        }),
        None,
    ),
    (
        "type",
        Some(|this, _, cx| {
            let ranges = selection_ranges(this, cx)?;
            Ok(text(match ranges.first() {
                None => "None",
                Some(range) if get(range, "collapsed", cx)?.to_boolean() => "Caret",
                Some(_) => "Range",
            }))
        }),
        None,
    ),
    (
        "anchorNode",
        Some(|this, _, cx| match selection_ranges(this, cx)?.first() {
            Some(range) => get(range, "startContainer", cx),
            None => Ok(JsValue::null()),
        }),
        None,
    ),
    (
        "anchorOffset",
        Some(|this, _, cx| match selection_ranges(this, cx)?.first() {
            Some(range) => get(range, "startOffset", cx),
            None => Ok(0.into()),
        }),
        None,
    ),
    (
        "focusNode",
        Some(|this, _, cx| match selection_ranges(this, cx)?.first() {
            Some(range) => get(range, "endContainer", cx),
            None => Ok(JsValue::null()),
        }),
        None,
    ),
    (
        "focusOffset",
        Some(|this, _, cx| match selection_ranges(this, cx)?.first() {
            Some(range) => get(range, "endOffset", cx),
            None => Ok(0.into()),
        }),
        None,
    ),
];

const SELECTION_METHODS: &[Method] = &[
    ("getRangeAt", 1, |this, args, cx| {
        let at = arg(args, 0).to_u32(cx)? as usize;
        match selection_ranges(this, cx)?.get(at).cloned() {
            Some(range) => Ok(range),
            None => Err(super::dom_error(cx, "Failed to execute 'getRangeAt' on 'Selection': The index provided is out of range.", "IndexSizeError")),
        }
    }),
    ("addRange", 1, |this, args, cx| {
        set_selection_ranges(this, cx, vec![arg(args, 0)])?;
        Ok(JsValue::undefined())
    }),
    ("removeRange", 1, |this, _, cx| {
        set_selection_ranges(this, cx, Vec::new())?;
        Ok(JsValue::undefined())
    }),
    ("removeAllRanges", 0, |this, _, cx| {
        set_selection_ranges(this, cx, Vec::new())?;
        Ok(JsValue::undefined())
    }),
    ("empty", 0, |this, _, cx| {
        set_selection_ranges(this, cx, Vec::new())?;
        Ok(JsValue::undefined())
    }),
    ("collapse", 2, |this, args, cx| {
        let node = arg(args, 0);
        if node.is_null_or_undefined() {
            set_selection_ranges(this, cx, Vec::new())?;
            return Ok(JsValue::undefined());
        }
        let offset = arg(args, 1).to_u32(cx).unwrap_or(0);
        let range = new_range(cx)?;
        set_boundary(&range, cx, "start", node.clone(), offset)?;
        set_boundary(&range, cx, "end", node, offset)?;
        set_selection_ranges(this, cx, vec![range])?;
        Ok(JsValue::undefined())
    }),
    ("setPosition", 2, |this, args, cx| super::call_method(this, "collapse", args, cx)),
    ("setBaseAndExtent", 4, |this, args, cx| {
        let range = new_range(cx)?;
        let (a, b) = (arg(args, 1).to_u32(cx)?, arg(args, 3).to_u32(cx)?);
        set_boundary(&range, cx, "start", arg(args, 0), a)?;
        set_boundary(&range, cx, "end", arg(args, 2), b)?;
        set_selection_ranges(this, cx, vec![range])?;
        Ok(JsValue::undefined())
    }),
    ("selectAllChildren", 1, |this, args, cx| {
        let range = new_range(cx)?;
        super::call_method(&range, "selectNodeContents", &[arg(args, 0)], cx)?;
        set_selection_ranges(this, cx, vec![range])?;
        Ok(JsValue::undefined())
    }),
    ("collapseToStart", 0, |_, _, _| Ok(JsValue::undefined())),
    ("collapseToEnd", 0, |_, _, _| Ok(JsValue::undefined())),
    ("extend", 2, |_, _, _| Ok(JsValue::undefined())),
    ("modify", 3, |_, _, _| Ok(JsValue::undefined())),
    ("deleteFromDocument", 0, |_, _, _| Ok(JsValue::undefined())),
    ("containsNode", 2, |_, _, _| Ok(false.into())),
    ("toString", 0, |this, _, cx| match selection_ranges(this, cx)?.first() {
        Some(range) => super::call_method(range, "toString", &[], cx),
        None => Ok(text("")),
    }),
];

// ─── Canvas ─────────────────────────────────────────────────────────────────────────────────

/// The font size a canvas font names, in pixels: `bold 16px sans-serif` → 16.
fn font_size(font: &str) -> f64 {
    font.split_whitespace()
        .find_map(|word| word.strip_suffix("px").and_then(|n| n.parse::<f64>().ok()))
        .unwrap_or(10.)
}

/// About how wide `text` is in `font`: wide characters a font size each, the rest a little over
/// half of one.
fn text_width(text: &str, font: &str) -> f64 {
    let size = font_size(font);
    text.chars()
        .map(|c| if (c as u32) >= 0x2E80 { size } else if c == ' ' { size * 0.28 } else { size * 0.55 })
        .sum()
}

const CONTEXT_STATE: &[(&str, &str)] = &[
    ("fillStyle", "#000000"),
    ("strokeStyle", "#000000"),
    ("font", "10px sans-serif"),
    ("textAlign", "start"),
    ("textBaseline", "alphabetic"),
    ("direction", "inherit"),
    ("globalCompositeOperation", "source-over"),
    ("lineCap", "butt"),
    ("lineJoin", "miter"),
    ("shadowColor", "rgba(0, 0, 0, 0)"),
    ("filter", "none"),
    ("imageSmoothingQuality", "low"),
    ("fontKerning", "auto"),
    ("letterSpacing", "0px"),
    ("wordSpacing", "0px"),
];

const CONTEXT_NUMBERS: &[(&str, f64)] = &[
    ("lineWidth", 1.),
    ("globalAlpha", 1.),
    ("miterLimit", 10.),
    ("shadowBlur", 0.),
    ("shadowOffsetX", 0.),
    ("shadowOffsetY", 0.),
    ("lineDashOffset", 0.),
];

const CONTEXT_NOOPS: &[&str] = &[
    "save", "restore", "scale", "rotate", "translate", "transform", "setTransform",
    "resetTransform", "clearRect", "fillRect", "strokeRect", "beginPath", "closePath", "moveTo",
    "lineTo", "bezierCurveTo", "quadraticCurveTo", "arc", "arcTo", "ellipse", "rect", "roundRect",
    "fill", "stroke", "clip", "fillText", "strokeText", "drawImage", "putImageData",
    "setLineDash", "drawFocusIfNeeded", "reset",
];

fn image_data(context: &mut Context, width: u32, height: u32) -> JsResult<JsValue> {
    let object = native_instance(context, "ImageData", Kind::Plain, 0);
    let bytes = JsUint8ClampedArray::from_iter(std::iter::repeat_n(0u8, (width * height * 4) as usize), context)?;
    set(&object, "width", width, context)?;
    set(&object, "height", height, context)?;
    set(&object, "data", bytes, context)?;
    set(&object, "colorSpace", text("srgb"), context)?;
    Ok(object.into())
}

fn construct_image_data(new_target: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let first = arg(args, 0);
    let (data, width, height) = if first.is_object() {
        let width = arg(args, 1).to_u32(context)?;
        let height = match arg(args, 2) {
            h if h.is_undefined() => {
                let length = get(&first, "length", context)?.to_u32(context)?;
                length / 4 / width.max(1)
            }
            h => h.to_u32(context)?,
        };
        (Some(first), width, height)
    } else {
        (None, first.to_u32(context)?, arg(args, 1).to_u32(context)?)
    };
    let object = instance(new_target, Kind::Plain, 0, context)?;
    let data = match data {
        Some(data) => data,
        None => JsUint8ClampedArray::from_iter(std::iter::repeat_n(0u8, (width * height * 4) as usize), context)?.into(),
    };
    set(&object, "width", width, context)?;
    set(&object, "height", height, context)?;
    set(&object, "data", data, context)?;
    set(&object, "colorSpace", text("srgb"), context)?;
    Ok(object.into())
}

fn gradient(context: &mut Context) -> JsValue {
    let object = native_instance(context, "CanvasGradient", Kind::Plain, 0);
    let add = function(context, "addColorStop", 2, |_, _, _| Ok(JsValue::undefined()));
    let _ = set(&object, "addColorStop", add, context);
    object.into()
}

fn context_2d(context: &mut Context, canvas: &JsValue) -> JsResult<JsValue> {
    let object = native_instance(context, "CanvasRenderingContext2D", Kind::Plain, 0);
    set(&object, "canvas", canvas.clone(), context)?;
    for (name, value) in CONTEXT_STATE {
        set(&object, name, text(value), context)?;
    }
    for (name, value) in CONTEXT_NUMBERS {
        set(&object, name, *value, context)?;
    }
    set(&object, "imageSmoothingEnabled", true, context)?;
    Ok(object.into())
}

const CONTEXT_METHODS: &[Method] = &[
    ("measureText", 1, |this, args, cx| {
        let content = arg_string(args, 0, cx)?;
        let font = string(&get(this, "font", cx)?, cx)?;
        let size = font_size(&font);
        let width = text_width(&content, &font);
        let metrics = native_instance(cx, "TextMetrics", Kind::Plain, 0);
        for (key, value) in [
            ("width", width),
            ("actualBoundingBoxLeft", 0.),
            ("actualBoundingBoxRight", width),
            ("actualBoundingBoxAscent", size * 0.8),
            ("actualBoundingBoxDescent", size * 0.2),
            ("fontBoundingBoxAscent", size * 0.9),
            ("fontBoundingBoxDescent", size * 0.25),
            ("emHeightAscent", size * 0.8),
            ("emHeightDescent", size * 0.2),
            ("alphabeticBaseline", 0.),
            ("hangingBaseline", size * 0.8),
            ("ideographicBaseline", -size * 0.2),
        ] {
            set(&metrics, key, value, cx)?;
        }
        Ok(metrics.into())
    }),
    ("getImageData", 4, |_, args, cx| {
        let width = arg(args, 2).to_u32(cx)?.min(8192);
        let height = arg(args, 3).to_u32(cx)?.min(8192);
        image_data(cx, width, height)
    }),
    ("createImageData", 2, |_, args, cx| {
        let first = arg(args, 0);
        let (width, height) = if first.is_object() {
            (get(&first, "width", cx)?.to_u32(cx)?, get(&first, "height", cx)?.to_u32(cx)?)
        } else {
            (first.to_u32(cx)?, arg(args, 1).to_u32(cx)?)
        };
        image_data(cx, width.min(8192), height.min(8192))
    }),
    ("createLinearGradient", 4, |_, _, cx| Ok(gradient(cx))),
    ("createRadialGradient", 6, |_, _, cx| Ok(gradient(cx))),
    ("createConicGradient", 3, |_, _, cx| Ok(gradient(cx))),
    ("createPattern", 2, |_, _, cx| Ok(native_instance(cx, "CanvasPattern", Kind::Plain, 0).into())),
    ("getLineDash", 0, |_, _, cx| Ok(array(Vec::new(), cx))),
    ("isPointInPath", 2, |_, _, _| Ok(false.into())),
    ("isPointInStroke", 2, |_, _, _| Ok(false.into())),
    ("getTransform", 0, |_, _, cx| identity_matrix(cx)),
    ("getContextAttributes", 0, |_, _, cx| {
        let attributes = plain_object(cx);
        set(&attributes, "alpha", true, cx)?;
        set(&attributes, "desynchronized", false, cx)?;
        set(&attributes, "colorSpace", text("srgb"), cx)?;
        set(&attributes, "willReadFrequently", false, cx)?;
        Ok(attributes.into())
    }),
];

/// A transparent pixel, as a PNG: what a canvas that paints nothing turns into.
const BLANK_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

fn canvas_size(this: &JsValue, context: &mut Context, name: &str, default: u32) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let value = read_dom(context, |dom| dom.get_attr(id, name)).and_then(|v| v.parse::<u32>().ok()).unwrap_or(default);
    Ok(value.into())
}

fn set_canvas_size(this: &JsValue, args: &[JsValue], context: &mut Context, name: &str) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let value = arg(args, 0).to_u32(context)?;
    with_dom(context, |dom| dom.set_attr(id, name, &value.to_string()));
    Ok(JsValue::undefined())
}

pub const CANVAS_ACCESSORS: &[Accessor] = &[
    (
        "width",
        Some(|this, _, cx| canvas_size(this, cx, "width", 300)),
        Some(|this, args, cx| set_canvas_size(this, args, cx, "width")),
    ),
    (
        "height",
        Some(|this, _, cx| canvas_size(this, cx, "height", 150)),
        Some(|this, args, cx| set_canvas_size(this, args, cx, "height")),
    ),
];

pub const CANVAS_METHODS: &[Method] = &[
    ("getContext", 1, |this, args, cx| {
        let kind = arg_string(args, 0, cx)?;
        if kind != "2d" {
            // No WebGL, as on a machine without the GPU for it: libraries fall back.
            return Ok(JsValue::null());
        }
        let Some(canvas) = this.as_object() else { return Ok(JsValue::null()) };
        let key = js_string!("__context2d");
        let known = canvas.get(key.clone(), cx)?;
        if known.is_object() {
            return Ok(known);
        }
        let made = context_2d(cx, this)?;
        super::hidden(&canvas, key, made.clone(), cx)?;
        Ok(made)
    }),
    ("toDataURL", 0, |_, _, _| Ok(text(BLANK_PNG))),
    ("toBlob", 1, |_, args, cx| {
        let callback = arg(args, 0);
        let blob = cx.global_object().get(js_string!("Blob"), cx)?;
        let made = match blob.as_constructor() {
            Some(blob) => {
                let options = plain_object(cx);
                set(&options, "type", text("image/png"), cx)?;
                let parts = array(Vec::new(), cx);
                blob.construct(&[parts, options.into()], None, cx)?.into()
            }
            None => JsValue::null(),
        };
        if callback.is_callable() {
            verve::call(cx, &callback, &JsValue::undefined(), &[made])?;
        }
        Ok(JsValue::undefined())
    }),
    ("captureStream", 0, |_, _, cx| Ok(plain_object(cx).into())),
    ("transferControlToOffscreen", 0, |_, _, _| Err(type_error("OffscreenCanvas is not supported"))),
];

// ─── Geometry ───────────────────────────────────────────────────────────────────────────────

/// A matrix's sixteen numbers, column by column as CSS writes them (`m11`, `m12`, … `m44`).
type Matrix = [f64; 16];

const IDENTITY: Matrix = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

const MATRIX_NAMES: [&str; 16] = [
    "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41",
    "m42", "m43", "m44",
];

/// `a`·`b`, as CSS composes transforms: `b` applied first.
fn multiply(a: &Matrix, b: &Matrix) -> Matrix {
    let mut out = [0.; 16];
    for column in 0..4 {
        for row in 0..4 {
            out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
        }
    }
    out
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let mut inv = [0.; 16];
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15] + m[9] * m[7] * m[14] + m[13] * m[6] * m[11] - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15] - m[8] * m[7] * m[14] - m[12] * m[6] * m[11] + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15] + m[8] * m[7] * m[13] + m[12] * m[5] * m[11] - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14] - m[8] * m[6] * m[13] - m[12] * m[5] * m[10] + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15] - m[9] * m[3] * m[14] - m[13] * m[2] * m[11] + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15] + m[8] * m[3] * m[14] + m[12] * m[2] * m[11] - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15] - m[8] * m[3] * m[13] - m[12] * m[1] * m[11] + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14] + m[8] * m[2] * m[13] + m[12] * m[1] * m[10] - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15] + m[5] * m[3] * m[14] + m[13] * m[2] * m[7] - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15] - m[4] * m[3] * m[14] - m[12] * m[2] * m[7] + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15] + m[4] * m[3] * m[13] + m[12] * m[1] * m[7] - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14] - m[4] * m[2] * m[13] - m[12] * m[1] * m[6] + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11] - m[5] * m[3] * m[10] - m[9] * m[2] * m[7] + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11] + m[4] * m[3] * m[10] + m[8] * m[2] * m[7] - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11] - m[4] * m[3] * m[9] - m[8] * m[1] * m[7] + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10] + m[4] * m[2] * m[9] + m[8] * m[1] * m[6] - m[8] * m[2] * m[5];
    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if det.abs() < f64::EPSILON {
        return None;
    }
    Some(inv.map(|v| v / det))
}

fn matrix_of(value: &JsValue, context: &mut Context) -> JsResult<Matrix> {
    let mut out = [0.; 16];
    for (i, name) in MATRIX_NAMES.iter().enumerate() {
        out[i] = get(value, name, context)?.to_number(context)?;
    }
    Ok(out)
}

/// Reads a matrix from what `new DOMMatrix(init)` is given: six numbers, sixteen, or a CSS
/// transform list of `matrix()`, `translate()`, `scale()` and `rotate()`.
fn matrix_from_init(init: &JsValue, context: &mut Context) -> JsResult<Matrix> {
    if init.is_undefined() {
        return Ok(IDENTITY);
    }
    if let Some(text_value) = init.as_string() {
        return Ok(parse_transform(&text_value.to_std_string_escaped()));
    }
    let length = get(init, "length", context)?;
    if length.is_undefined() {
        return matrix_of(init, context).or(Ok(IDENTITY));
    }
    let numbers: Vec<f64> = super::list(init, context)?
        .into_iter()
        .map(|v| v.to_number(context))
        .collect::<JsResult<_>>()?;
    Ok(match numbers.len() {
        6 => from_2d(numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5]),
        16 => {
            let mut out = [0.; 16];
            out.copy_from_slice(&numbers);
            out
        }
        _ => return Err(type_error("Failed to construct 'DOMMatrix': The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix.")),
    })
}

fn from_2d(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Matrix {
    [a, b, 0., 0., c, d, 0., 0., 0., 0., 1., 0., e, f, 0., 1.]
}

fn translation(x: f64, y: f64, z: f64) -> Matrix {
    [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.]
}

fn scaling(x: f64, y: f64, z: f64) -> Matrix {
    [x, 0., 0., 0., 0., y, 0., 0., 0., 0., z, 0., 0., 0., 0., 1.]
}

fn rotation_z(degrees: f64) -> Matrix {
    let (sin, cos) = degrees.to_radians().sin_cos();
    from_2d(cos, sin, -sin, cos, 0., 0.)
}

fn parse_transform(text: &str) -> Matrix {
    let mut out = IDENTITY;
    for part in text.split(')') {
        let Some((name, arguments)) = part.split_once('(') else { continue };
        let numbers: Vec<f64> = arguments
            .split([',', ' '])
            .filter(|n| !n.is_empty())
            .filter_map(|n| n.trim_end_matches("px").trim_end_matches("deg").parse().ok())
            .collect();
        let n = |i: usize, default: f64| numbers.get(i).copied().unwrap_or(default);
        let step = match name.trim() {
            "matrix" if numbers.len() == 6 => from_2d(n(0, 1.), n(1, 0.), n(2, 0.), n(3, 1.), n(4, 0.), n(5, 0.)),
            "translate" => translation(n(0, 0.), n(1, 0.), 0.),
            "translateX" => translation(n(0, 0.), 0., 0.),
            "translateY" => translation(0., n(0, 0.), 0.),
            "scale" => scaling(n(0, 1.), n(1, n(0, 1.)), 1.),
            "scaleX" => scaling(n(0, 1.), 1., 1.),
            "scaleY" => scaling(1., n(0, 1.), 1.),
            "rotate" | "rotateZ" => rotation_z(n(0, 0.)),
            _ => continue,
        };
        out = multiply(&out, &step);
    }
    out
}

fn set_matrix(object: &JsObject, m: &Matrix, context: &mut Context) -> JsResult<()> {
    for (i, name) in MATRIX_NAMES.iter().enumerate() {
        set(object, name, m[i], context)?;
    }
    for (name, i) in [("a", 0), ("b", 1), ("c", 4), ("d", 5), ("e", 12), ("f", 13)] {
        set(object, name, m[i], context)?;
    }
    let is_2d = m[2] == 0. && m[3] == 0. && m[6] == 0. && m[7] == 0. && m[8] == 0. && m[9] == 0. && m[10] == 1. && m[11] == 0. && m[14] == 0. && m[15] == 1.;
    set(object, "is2D", is_2d, context)?;
    set(object, "isIdentity", *m == IDENTITY, context)?;
    Ok(())
}

fn make_matrix(context: &mut Context, m: &Matrix) -> JsResult<JsValue> {
    let object = native_instance(context, "DOMMatrix", Kind::Plain, 0);
    set_matrix(&object, m, context)?;
    Ok(object.into())
}

pub fn identity_matrix(context: &mut Context) -> JsResult<JsValue> {
    make_matrix(context, &IDENTITY)
}

fn construct_matrix(new_target: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let m = matrix_from_init(&arg(args, 0), context)?;
    let object = instance(new_target, Kind::Plain, 0, context)?;
    set_matrix(&object, &m, context)?;
    Ok(object.into())
}

fn number_arg(args: &[JsValue], index: usize, default: f64, context: &mut Context) -> JsResult<f64> {
    let value = arg(args, index);
    if value.is_undefined() { Ok(default) } else { value.to_number(context) }
}

fn matrix_step(this: &JsValue, args: &[JsValue], context: &mut Context, name: &str) -> JsResult<Matrix> {
    let m = matrix_of(this, context)?;
    Ok(match name {
        "multiply" => multiply(&m, &matrix_from_init(&arg(args, 0), context)?),
        "preMultiply" => multiply(&matrix_from_init(&arg(args, 0), context)?, &m),
        "translate" => multiply(&m, &translation(number_arg(args, 0, 0., context)?, number_arg(args, 1, 0., context)?, number_arg(args, 2, 0., context)?)),
        "scale" => {
            let x = number_arg(args, 0, 1., context)?;
            let y = number_arg(args, 1, x, context)?;
            let z = number_arg(args, 2, 1., context)?;
            let (ox, oy) = (number_arg(args, 3, 0., context)?, number_arg(args, 4, 0., context)?);
            let moved = multiply(&m, &translation(ox, oy, 0.));
            let scaled = multiply(&moved, &scaling(x, y, z));
            multiply(&scaled, &translation(-ox, -oy, 0.))
        }
        "scaleNonUniform" => multiply(&m, &scaling(number_arg(args, 0, 1., context)?, number_arg(args, 1, 1., context)?, 1.)),
        "rotate" => {
            let angle = if arg(args, 1).is_undefined() { number_arg(args, 0, 0., context)? } else { number_arg(args, 2, 0., context)? };
            multiply(&m, &rotation_z(angle))
        }
        "skewX" => {
            let t = number_arg(args, 0, 0., context)?.to_radians().tan();
            multiply(&m, &from_2d(1., 0., t, 1., 0., 0.))
        }
        "skewY" => {
            let t = number_arg(args, 0, 0., context)?.to_radians().tan();
            multiply(&m, &from_2d(1., t, 0., 1., 0., 0.))
        }
        "flipX" => multiply(&m, &scaling(-1., 1., 1.)),
        "flipY" => multiply(&m, &scaling(1., -1., 1.)),
        "inverse" => invert(&m).unwrap_or([f64::NAN; 16]),
        _ => m,
    })
}

const MATRIX_STEPS: &[&str] = &["multiply", "preMultiply", "translate", "scale", "scaleNonUniform", "rotate", "skewX", "skewY", "flipX", "flipY", "inverse"];

fn simple_class(context: &mut Context, name: &'static str, construct: boa_engine::native_function::NativeFunctionPointer, parent: Option<&str>, methods: &[Method], accessors: &[Accessor], global: bool) -> JsResult<JsObject> {
    Ok(class(context, ClassSpec { name, length: 0, construct, parent, methods, accessors, statics: &[], global })?.1)
}

fn install_geometry(context: &mut Context) -> JsResult<()> {
    let prototype = simple_class(context, "DOMMatrixReadOnly", construct_matrix, None, &[], &[], true)?;
    let own = simple_class(context, "DOMMatrix", construct_matrix, Some("DOMMatrixReadOnly"), &[], &[], true)?;
    for name in MATRIX_STEPS {
        let name: &'static str = name;
        let answer = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, args, cx| {
                let m = matrix_step(this, args, cx, name)?;
                make_matrix(cx, &m)
            }),
        )
        .name(JsString::from(name))
        .build();
        set(&prototype, name, answer, context)?;
        let self_name = format!("{}Self", if name == "inverse" { "invert" } else { name });
        let in_place = FunctionObjectBuilder::new(
            context.realm(),
            NativeFunction::from_copy_closure(move |this, args, cx| {
                let m = matrix_step(this, args, cx, name)?;
                if let Some(object) = this.as_object() {
                    set_matrix(&object, &m, cx)?;
                }
                Ok(this.clone())
            }),
        )
        .name(JsString::from(self_name.as_str()))
        .build();
        set(&own, &self_name, in_place, context)?;
    }
    let methods: &[Method] = &[
        ("transformPoint", 1, |this, args, cx| {
            let m = matrix_of(this, cx)?;
            let point = arg(args, 0);
            let read = |key: &str, default: f64, cx: &mut Context| -> JsResult<f64> {
                let v = get(&point, key, cx)?;
                if v.is_undefined() { Ok(default) } else { v.to_number(cx) }
            };
            let (x, y, z, w) = (read("x", 0., cx)?, read("y", 0., cx)?, read("z", 0., cx)?, read("w", 1., cx)?);
            let out = [
                m[0] * x + m[4] * y + m[8] * z + m[12] * w,
                m[1] * x + m[5] * y + m[9] * z + m[13] * w,
                m[2] * x + m[6] * y + m[10] * z + m[14] * w,
                m[3] * x + m[7] * y + m[11] * z + m[15] * w,
            ];
            make_point(cx, out)
        }),
        ("toFloat32Array", 0, |this, _, cx| {
            let m = matrix_of(this, cx)?;
            Ok(boa_engine::object::builtins::JsFloat32Array::from_iter(m.map(|v| v as f32), cx)?.into())
        }),
        ("toFloat64Array", 0, |this, _, cx| {
            let m = matrix_of(this, cx)?;
            Ok(boa_engine::object::builtins::JsFloat64Array::from_iter(m, cx)?.into())
        }),
        ("toString", 0, |this, _, cx| {
            let m = matrix_of(this, cx)?;
            let is_2d = get(this, "is2D", cx)?.to_boolean();
            Ok(text(&if is_2d {
                format!("matrix({}, {}, {}, {}, {}, {})", m[0], m[1], m[4], m[5], m[12], m[13])
            } else {
                format!("matrix3d({})", m.iter().map(f64::to_string).collect::<Vec<_>>().join(", "))
            }))
        }),
        ("toJSON", 0, |this, _, cx| {
            let m = matrix_of(this, cx)?;
            let out = plain_object(cx);
            set_matrix(&out, &m, cx)?;
            Ok(out.into())
        }),
    ];
    for (name, length, f) in methods {
        let made = function(context, name, *length, *f);
        set(&prototype, name, made, context)?;
    }
    for owner in ["DOMMatrixReadOnly", "DOMMatrix"] {
        if let Some(constructor) = constructor_of(context, owner) {
            let from = function(context, "fromMatrix", 1, |_, args, cx| {
                let m = matrix_from_init(&arg(args, 0), cx)?;
                make_matrix(cx, &m)
            });
            set(&constructor, "fromMatrix", from.clone(), context)?;
            set(&constructor, "fromFloat32Array", from.clone(), context)?;
            set(&constructor, "fromFloat64Array", from, context)?;
        }
    }
    let matrix = constructor_of(context, "DOMMatrix").map_or(JsValue::undefined(), JsValue::from);
    context.register_global_property(js_string!("WebKitCSSMatrix"), matrix, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    simple_class(context, "DOMPointReadOnly", construct_point, None, &[
        ("matrixTransform", 1, |this, args, cx| super::call_method(&arg(args, 0), "transformPoint", std::slice::from_ref(this), cx)),
        ("toJSON", 0, |this, _, cx| {
            let out = plain_object(cx);
            for key in ["x", "y", "z", "w"] {
                let value = get(this, key, cx)?;
                set(&out, key, value, cx)?;
            }
            Ok(out.into())
        }),
    ], &[], true)?;
    simple_class(context, "DOMPoint", construct_point, Some("DOMPointReadOnly"), &[], &[], true)?;
    simple_class(context, "DOMRectReadOnly", construct_rect, None, &[("toJSON", 0, |this, _, cx| {
        let out = plain_object(cx);
        for key in ["x", "y", "width", "height", "top", "right", "bottom", "left"] {
            let value = get(this, key, cx)?;
            set(&out, key, value, cx)?;
        }
        Ok(out.into())
    })], &[], true)?;
    simple_class(context, "DOMRect", construct_rect, Some("DOMRectReadOnly"), &[], &[], true)?;
    if let Some(constructor) = constructor_of(context, "DOMRect") {
        let from = function(context, "fromRect", 1, |_, args, cx| {
            let given = arg(args, 0);
            let mut numbers = [0.; 4];
            for (i, key) in ["x", "y", "width", "height"].iter().enumerate() {
                let v = get(&given, key, cx)?;
                numbers[i] = if v.is_undefined() { 0. } else { v.to_number(cx)? };
            }
            make_rect(cx, numbers)
        });
        set(&constructor, "fromRect", from, context)?;
    }
    let path = simple_class(context, "Path2D", |t, _, c| Ok(instance(t, Kind::Plain, 0, c)?.into()), None, &[], &[], true)?;
    for name in ["addPath", "closePath", "moveTo", "lineTo", "bezierCurveTo", "quadraticCurveTo", "arc", "arcTo", "ellipse", "rect", "roundRect"] {
        let noop = function(context, name, 0, |_, _, _| Ok(JsValue::undefined()));
        set(&path, name, noop, context)?;
    }
    Ok(())
}

fn make_point(context: &mut Context, [x, y, z, w]: [f64; 4]) -> JsResult<JsValue> {
    let object = native_instance(context, "DOMPoint", Kind::Plain, 0);
    for (key, value) in [("x", x), ("y", y), ("z", z), ("w", w)] {
        set(&object, key, value, context)?;
    }
    Ok(object.into())
}

pub fn new_point(context: &mut Context) -> JsResult<JsValue> {
    make_point(context, [0., 0., 0., 1.])
}

fn construct_point(new_target: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = instance(new_target, Kind::Plain, 0, context)?;
    for (i, (key, default)) in [("x", 0.), ("y", 0.), ("z", 0.), ("w", 1.)].into_iter().enumerate() {
        let value = number_arg(args, i, default, context)?;
        set(&object, key, value, context)?;
    }
    Ok(object.into())
}

fn set_rect(object: &JsObject, [x, y, width, height]: [f64; 4], context: &mut Context) -> JsResult<()> {
    for (key, value) in [
        ("x", x),
        ("y", y),
        ("width", width),
        ("height", height),
        ("left", x.min(x + width)),
        ("top", y.min(y + height)),
        ("right", x.max(x + width)),
        ("bottom", y.max(y + height)),
    ] {
        set(object, key, value, context)?;
    }
    Ok(())
}

pub fn make_rect(context: &mut Context, numbers: [f64; 4]) -> JsResult<JsValue> {
    let object = native_instance(context, "DOMRect", Kind::Plain, 0);
    set_rect(&object, numbers, context)?;
    Ok(object.into())
}

fn construct_rect(new_target: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let object = instance(new_target, Kind::Plain, 0, context)?;
    let mut numbers = [0.; 4];
    for (i, slot) in numbers.iter_mut().enumerate() {
        *slot = number_arg(args, i, 0., context)?;
    }
    set_rect(&object, numbers, context)?;
    Ok(object.into())
}

// ─── Media, animations, drag and drop ───────────────────────────────────────────────────────

/// An empty `TimeRanges`.
pub fn empty_ranges(context: &mut Context) -> JsResult<JsValue> {
    let ranges = plain_object(context);
    set(&ranges, "length", 0, context)?;
    for name in ["start", "end"] {
        let f = function(context, name, 1, |_, _, cx| Err(super::dom_error(cx, "The index provided is out of range.", "IndexSizeError")));
        set(&ranges, name, f, context)?;
    }
    Ok(ranges.into())
}

/// `element.animate(…)`: an animation already finished, as a page with animations off has.
pub fn finished_animation(context: &mut Context) -> JsResult<JsValue> {
    let target = shared(context).borrow().class("EventTarget").expect("made").0;
    let animation = target.construct(&[], None, context)?;
    let done = super::resolved(animation.clone().into(), context)?;
    set(&animation, "finished", done.clone(), context)?;
    set(&animation, "ready", done, context)?;
    set(&animation, "playState", text("finished"), context)?;
    set(&animation, "currentTime", 0, context)?;
    set(&animation, "playbackRate", 1, context)?;
    set(&animation, "onfinish", JsValue::null(), context)?;
    set(&animation, "oncancel", JsValue::null(), context)?;
    for name in ["play", "pause", "cancel", "finish", "reverse", "persist", "commitStyles", "updatePlaybackRate"] {
        let f = function(context, name, 0, |_, _, _| Ok(JsValue::undefined()));
        set(&animation, name, f, context)?;
    }
    Ok(animation.into())
}

fn construct_data_transfer(new_target: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let transfer = instance(new_target, Kind::Plain, 0, context)?;
    let data = plain_object(context);
    super::hidden(&transfer, js_string!("__data"), data, context)?;
    set(&transfer, "dropEffect", text("none"), context)?;
    set(&transfer, "effectAllowed", text("all"), context)?;
    let files = array(Vec::new(), context);
    set(&transfer, "files", files, context)?;
    let items = array(Vec::new(), context);
    set(&transfer, "items", items, context)?;
    Ok(transfer.into())
}

const DATA_TRANSFER_METHODS: &[Method] = &[
    ("setData", 2, |this, args, cx| {
        let data = get(this, "__data", cx)?;
        let format = arg_string(args, 0, cx)?.to_ascii_lowercase();
        let format = if format == "text" { "text/plain".to_owned() } else { format };
        let value = arg_string(args, 1, cx)?;
        if let Some(data) = data.as_object() {
            set(&data, &format, text(&value), cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("getData", 1, |this, args, cx| {
        let data = get(this, "__data", cx)?;
        let format = arg_string(args, 0, cx)?.to_ascii_lowercase();
        let format = if format == "text" { "text/plain".to_owned() } else { format };
        let value = get(&data, &format, cx)?;
        Ok(if value.is_undefined() { text("") } else { value })
    }),
    ("clearData", 1, |this, args, cx| {
        let format = arg(args, 0);
        if format.is_undefined() {
            let data = plain_object(cx);
            if let Some(this) = this.as_object() {
                super::hidden(&this, js_string!("__data"), data, cx)?;
            }
        } else {
            let data = get(this, "__data", cx)?;
            let format = string(&format, cx)?.to_ascii_lowercase();
            if let Some(data) = data.as_object() {
                data.delete_property_or_throw(JsString::from(format.as_str()), cx)?;
            }
        }
        Ok(JsValue::undefined())
    }),
    ("setDragImage", 3, |_, _, _| Ok(JsValue::undefined())),
];

const DATA_TRANSFER_ACCESSORS: &[Accessor] = &[(
    "types",
    Some(|this, _, cx| {
        let data = get(this, "__data", cx)?;
        let keys = match data.as_object() {
            Some(data) => data.own_property_keys(cx)?.into_iter().map(|k| text(&k.to_string())).collect(),
            None => Vec::new(),
        };
        Ok(array(keys, cx))
    }),
    None,
)];

// ─── Element constructors ───────────────────────────────────────────────────────────────────

/// A constructor that makes an element of `tag`, whose prototype is its class's: `Image` is
/// `HTMLImageElement`'s, as in a browser.
fn element_constructor(context: &mut Context, name: &str, class: &str, make: boa_engine::native_function::NativeFunctionPointer) -> JsResult<()> {
    let prototype = shared(context).borrow().prototype(class);
    let constructor: JsObject = FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(make))
        .name(JsString::from(name))
        .length(0)
        .constructor(true)
        .build()
        .into();
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder().value(prototype).writable(false).enumerable(false).configurable(false),
        context,
    )?;
    context.register_global_property(JsString::from(name), constructor, Attribute::WRITABLE | Attribute::CONFIGURABLE)?;
    Ok(())
}

fn made_element(context: &mut Context, tag: &str) -> (NodeId, JsValue) {
    let id = with_dom(context, |dom| dom.create_element(tag));
    (id, wrap(context, id))
}

fn construct_image(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (id, object) = made_element(context, "img");
    for (index, name) in [(0, "width"), (1, "height")] {
        let size = arg(args, index);
        if !size.is_undefined() {
            let size = size.to_number(context)?;
            with_dom(context, |dom| dom.set_attr(id, name, &size.to_string()));
        }
    }
    Ok(object)
}

fn construct_audio(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (id, object) = made_element(context, "audio");
    if let Some(src) = opt_string(args, 0, context)? {
        with_dom(context, |dom| dom.set_attr(id, "src", &src));
    }
    Ok(object)
}

fn construct_option(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (id, object) = made_element(context, "option");
    let label = opt_string(args, 0, context)?.unwrap_or_default();
    let value = opt_string(args, 1, context)?;
    let selected = arg(args, 3).to_boolean() || arg(args, 2).to_boolean();
    with_dom(context, |dom| {
        dom.set_text(id, &label);
        if let Some(value) = &value {
            dom.set_attr(id, "value", value);
        }
        if selected {
            dom.set_attr(id, "selected", "");
        }
    });
    Ok(object)
}

// ─── Installing ─────────────────────────────────────────────────────────────────────────────

pub fn install(context: &mut Context) -> JsResult<()> {
    let element = shared(context).borrow().prototype("Element");
    super::extend_prototype(context, &element, ATTRIBUTE_METHODS, &[])?;
    simple_class(context, "Attr", illegal_constructor, Some("Node"), &[], &[], true)?;
    let map = simple_class(context, "NamedNodeMap", illegal_constructor, None, NAMED_NODE_MAP_METHODS, &[], true)?;
    let array_prototype = context.intrinsics().constructors().array().prototype();
    map.set_prototype(Some(array_prototype));
    simple_class(context, "ShadowRoot", illegal_constructor, Some("DocumentFragment"), &[], SHADOW_ROOT_ACCESSORS, true)?;
    simple_class(context, "CustomElementRegistry", illegal_constructor, None, CUSTOM_ELEMENTS_METHODS, &[], true)?;
    let registry = native_instance(context, "CustomElementRegistry", Kind::Plain, 0);
    context.register_global_property(js_string!("customElements"), registry, Attribute::all())?;
    simple_class(context, "StyleSheet", illegal_constructor, None, &[], &[], true)?;
    simple_class(context, "CSSStyleSheet", construct_sheet, Some("StyleSheet"), SHEET_METHODS, SHEET_ACCESSORS, true)?;
    for name in ["CSSRule", "CSSStyleRule", "CSSMediaRule", "StyleSheetList", "CSSRuleList", "MediaList"] {
        simple_class(context, name, illegal_constructor, None, &[], &[], true)?;
    }
    let css = plain_object(context);
    for (name, length, f) in CSS_FUNCTIONS {
        let made = function(context, name, *length, *f);
        set(&css, name, made, context)?;
    }
    context.register_global_property(js_string!("CSS"), css, Attribute::all())?;
    simple_class(context, "Range", construct_range, None, RANGE_METHODS, RANGE_ACCESSORS, true)?;
    simple_class(context, "StaticRange", construct_range, None, &[], &[], true)?;
    simple_class(context, "Selection", illegal_constructor, None, SELECTION_METHODS, SELECTION_ACCESSORS, true)?;
    let canvas_context = simple_class(context, "CanvasRenderingContext2D", illegal_constructor, None, CONTEXT_METHODS, &[], true)?;
    for name in CONTEXT_NOOPS {
        let noop = function(context, name, 0, |_, _, _| Ok(JsValue::undefined()));
        set(&canvas_context, name, noop, context)?;
    }
    for name in ["CanvasGradient", "CanvasPattern", "TextMetrics"] {
        simple_class(context, name, illegal_constructor, None, &[], &[], true)?;
    }
    simple_class(context, "ImageData", construct_image_data, None, &[], &[], true)?;
    simple_class(context, "DataTransfer", construct_data_transfer, None, DATA_TRANSFER_METHODS, DATA_TRANSFER_ACCESSORS, true)?;
    install_geometry(context)?;
    element_constructor(context, "Image", "HTMLImageElement", construct_image)?;
    element_constructor(context, "Audio", "HTMLAudioElement", construct_audio)?;
    element_constructor(context, "Option", "HTMLOptionElement", construct_option)?;
    Ok(())
}
