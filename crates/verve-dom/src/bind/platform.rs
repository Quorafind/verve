//! The rest of the web platform libraries take for granted, beside the DOM proper:
//!
//! - `Range` and `Selection`;
//! - `attributes` (`NamedNodeMap`, `Attr`);
//! - shadow roots (held, and drawn as their host's content is not: see the crate's README);
//! - a `<template>`'s `content`;
//! - the CSSOM: a `<style>`'s `sheet`, `document.styleSheets`, `new CSSStyleSheet()` and
//!   `adoptedStyleSheets`, `CSS.escape`/`supports`;
//! - a `<canvas>`'s 2D context — one that keeps its state and measures text, but paints
//!   nothing — and no WebGL;
//! - `DOMMatrix`, `DOMPoint`, `DOMRect`, `Path2D`; `DataTransfer`; `Image`, `Audio`, `Option`;
//!   an element's `animate`.
//!
//! Lifted from su-editor's Obsidian host.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::FunctionObjectBuilder,
    property::{Attribute, PropertyDescriptor},
};

use super::node::{ancestry, read_dom, with_dom, wrap, wrap_opt};
use super::{
    Accessor, ClassSpec, Kind, Method, constructor_of, define_class, handle_instance, handle_of,
    illegal_constructor, native_instance, node_id, node_of, opt_string, plain_object, set, string,
};
use crate::dom::{Dom, NodeId, NodeKind, ShadowMode, SlotAssignment};
use crate::state::shared;
use verve::native::{self, arg, arg_text, function, get, hidden, js_text, list_of, type_error};

// ─── Attributes ─────────────────────────────────────────────────────────────────────────────

/// An `Attr`: its name and value as they were when it was asked for, and its element.
pub fn attr_object(
    context: &mut Context,
    owner: &JsValue,
    name: &str,
    value: &str,
    _document: &JsValue,
) -> JsResult<JsValue> {
    let attr = native_instance(context, "Attr", Kind::Plain, 0);
    let (prefix, local) = match name.split_once(':') {
        Some((prefix, local)) => (js_text(prefix), local),
        None => (JsValue::null(), name),
    };
    for (key, value) in [
        ("name", js_text(name)),
        ("nodeName", js_text(name)),
        ("localName", js_text(local)),
        ("prefix", prefix),
        ("namespaceURI", JsValue::null()),
        ("value", js_text(value)),
        ("nodeValue", js_text(value)),
        ("textContent", js_text(value)),
        ("specified", true.into()),
        ("nodeType", 2.into()),
        ("ownerElement", owner.clone()),
    ] {
        attr.define_property_or_throw(
            JsString::from(key),
            PropertyDescriptor::builder()
                .value(value)
                .writable(true)
                .enumerable(true)
                .configurable(true),
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
        items.push(attr_object(
            context,
            this,
            name,
            value,
            &JsValue::undefined(),
        )?);
    }
    let list = native::array(context, items.clone());
    if let Some(object) = list.as_object() {
        let prototype = crate::state::class_prototype(context, "NamedNodeMap");
        object.set_prototype(Some(prototype));
        hidden(&object, js_string!("__owner"), this.clone(), context)?;
        for ((name, _), attr) in pairs.iter().zip(items) {
            if name.parse::<u32>().is_ok()
                || object.has_own_property(JsString::from(name.as_str()), context)?
            {
                continue;
            }
            hidden(&object, JsString::from(name.as_str()), attr, context)?;
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
        Ok(if found.is_undefined() {
            JsValue::null()
        } else {
            found
        })
    }),
    ("getNamedItem", 1, |this, args, cx| {
        let name = arg_text(args, 0, cx)?;
        for attr in list_of(this, cx)? {
            if string(&get(&attr, "name", cx)?, cx)?.eq_ignore_ascii_case(&name) {
                return Ok(attr);
            }
        }
        Ok(JsValue::null())
    }),
    ("getNamedItemNS", 2, |this, args, cx| {
        super::call_method(this, "getNamedItem", &[arg(args, 1)], cx)
    }),
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
        let names: Vec<JsValue> = read_dom(cx, |dom| dom.attributes(id))
            .into_iter()
            .map(|(n, _)| js_text(&n))
            .collect();
        Ok(native::array(cx, names))
    }),
    ("hasAttributes", 0, |this, _, cx| {
        let id = node_of(this)?;
        Ok((!read_dom(cx, |dom| dom.attributes(id)).is_empty()).into())
    }),
    ("getAttributeNode", 1, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_text(args, 0, cx)?;
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
        let name = arg_text(args, 1, cx)?;
        let found = read_dom(cx, |dom| {
            dom.get_attr(id, &name).or_else(|| {
                dom.attributes(id)
                    .into_iter()
                    .find(|(k, _)| k.rsplit(':').next() == Some(name.as_str()))
                    .map(|(_, v)| v)
            })
        });
        Ok(found.map_or(JsValue::null(), |v| js_text(&v)))
    }),
    ("setAttributeNS", 3, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_text(args, 1, cx)?;
        let value = arg_text(args, 2, cx)?;
        with_dom(cx, |dom| dom.set_attr(id, &name, &value));
        Ok(JsValue::undefined())
    }),
    ("removeAttributeNS", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_text(args, 1, cx)?;
        let qualified = read_dom(cx, |dom| {
            dom.attributes(id)
                .into_iter()
                .find(|(k, _)| *k == name || k.rsplit(':').next() == Some(name.as_str()))
                .map(|(k, _)| k)
        });
        let name = qualified.unwrap_or(name);
        with_dom(cx, |dom| dom.remove_attr(id, &name));
        Ok(JsValue::undefined())
    }),
    ("hasAttributeNS", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_text(args, 1, cx)?;
        Ok(read_dom(cx, |dom| {
            dom.get_attr(id, &name).is_some()
                || dom
                    .attributes(id)
                    .iter()
                    .any(|(k, _)| k.rsplit(':').next() == Some(name.as_str()))
        })
        .into())
    }),
];

// ─── Shadow roots ───────────────────────────────────────────────────────────────────────────

/// The member `key` of `attachShadow`'s options as the enum `parse` reads, `default` when it
/// is absent (`None`: required).
fn shadow_option<T>(
    options: &JsValue,
    key: &str,
    parse: fn(&str) -> Option<T>,
    default: Option<T>,
    context: &mut Context,
) -> JsResult<T> {
    let given = get(options, key, context)?;
    if given.is_undefined() {
        return default.ok_or_else(|| {
            type_error(format!(
                "Failed to execute 'attachShadow' on 'Element': Failed to read the '{key}' property from 'ShadowRootInit': Required member is undefined."
            ))
        });
    }
    let name = string(&given, context)?;
    parse(&name).ok_or_else(|| {
        type_error(format!(
            "Failed to execute 'attachShadow' on 'Element': Failed to read the '{key}' property from 'ShadowRootInit': The provided value '{name}' is not a valid enum value."
        ))
    })
}

/// `element.attachShadow({ mode })`: a fragment of its own the element hosts, outside its
/// children.
pub fn attach_shadow(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let host = node_of(this)?;
    let options = arg(args, 0);
    let mode = shadow_option(&options, "mode", ShadowMode::parse, None, context)?;
    let assignment = shadow_option(
        &options,
        "slotAssignment",
        SlotAssignment::parse,
        Some(SlotAssignment::Named),
        context,
    )?;
    if !read_dom(context, |dom| dom.can_host_shadow(host)) {
        return Err(super::dom_error(
            context,
            "Failed to execute 'attachShadow' on 'Element': This element does not support attachShadow",
            "NotSupportedError",
        ));
    }
    let delegates_focus = get(&options, "delegatesFocus", context)?.to_boolean();
    let root = with_dom(context, |dom| {
        dom.attach_shadow(host, mode, delegates_focus, assignment)
    });
    let Some(root) = root else {
        return Err(super::dom_error(
            context,
            "Failed to execute 'attachShadow' on 'Element': Shadow root cannot be created on a host which already hosts a shadow tree.",
            "NotSupportedError",
        ));
    };
    let object = wrap(context, root);
    let root_object = object.as_object().expect("a shadow root");
    // The host keeps its root alive, and the root its host.
    if let Some(host_object) = this.as_object() {
        verve::dom_events::hold(&host_object, |held| held.push(root_object.clone()));
        verve::dom_events::hold(&root_object, |held| held.push(host_object.clone()));
    }
    Ok(object)
}

const SHADOW_ROOT_ACCESSORS: &[Accessor] = &[
    (
        "host",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            let host = read_dom(cx, |dom| dom.shadow_host(root));
            Ok(wrap_opt(cx, host))
        }),
        None,
    ),
    (
        "mode",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            let mode = read_dom(cx, |dom| dom.shadow(root).map(|s| s.mode.as_str()));
            Ok(mode.map_or(JsValue::null(), js_text))
        }),
        None,
    ),
    (
        "delegatesFocus",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            Ok(read_dom(cx, |dom| {
                dom.shadow(root).is_some_and(|s| s.delegates_focus)
            })
            .into())
        }),
        None,
    ),
    (
        "slotAssignment",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            let slot = read_dom(cx, |dom| dom.shadow(root).map(|s| s.assignment));
            Ok(js_text(slot.unwrap_or(SlotAssignment::Named).as_str()))
        }),
        None,
    ),
    (
        "innerHTML",
        Some(|this, _, cx| {
            let root = node_of(this)?;
            Ok(js_text(&read_dom(cx, |dom| crate::html::inner(dom, root))))
        }),
        Some(|this, args, cx| {
            let root = node_of(this)?;
            let markup = arg_text(args, 0, cx)?;
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
            let focused = read_dom(cx, |dom| {
                dom.focused
                    .filter(|f| dom.shadow_including_contains(root, *f))
                    .map(|f| dom.retarget(f, Some(root)))
            });
            Ok(wrap_opt(cx, focused))
        }),
        None,
    ),
    (
        "adoptedStyleSheets",
        Some(|this, _, cx| adopted_sheets(node_of(this)?, cx)),
        Some(|this, args, cx| set_adopted_sheets(node_of(this)?, "ShadowRoot", &arg(args, 0), cx)),
    ),
];

// ─── The CSSOM ──────────────────────────────────────────────────────────────────────────────

fn rule_object(context: &mut Context, rule: &str, sheet: &JsValue) -> JsResult<JsValue> {
    let object = plain_object(context);
    set(&object, "cssText", js_text(rule), context)?;
    if let Some(brace) = rule.find('{') {
        let selector = rule[..brace].trim();
        set(&object, "selectorText", js_text(selector), context)?;
        let body = rule[brace + 1..].trim_end().trim_end_matches('}').trim();
        let style = plain_object(context);
        set(&style, "cssText", js_text(body), context)?;
        for declaration in crate::css::declarations(body) {
            let value = js_text(&declaration.value);
            set(&style, &declaration.name, value.clone(), context)?;
            let camel = crate::dom::camel_name(&declaration.name);
            set(&style, &camel, value, context)?;
        }
        let get_value = function(context, "getPropertyValue", 1, |this, args, cx| {
            let name = arg_text(args, 0, cx)?;
            let value = get(this, &name, cx)?;
            Ok(if value.is_undefined() {
                js_text("")
            } else {
                value
            })
        });
        set(&style, "getPropertyValue", get_value, context)?;
        set(&object, "style", style, context)?;
        let kind = if selector.starts_with("@media") {
            4
        } else if selector.starts_with('@') {
            0
        } else {
            1
        };
        set(&object, "type", kind, context)?;
        if kind == 4 {
            set(
                &object,
                "conditionText",
                js_text(selector.trim_start_matches("@media").trim()),
                context,
            )?;
        }
    } else {
        set(
            &object,
            "type",
            if rule.starts_with("@import") { 3 } else { 0 },
            context,
        )?;
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
        return Ok(crate::css::split_rules(&read_dom(context, |dom| {
            dom.text_content(id)
        })));
    }
    if let Some(id) = handle_of(this, Kind::Constructed) {
        let rules = shared(context)
            .borrow()
            .constructed_sheets
            .get(&id)
            .cloned()
            .unwrap_or_default();
        return Ok(rules
            .iter()
            .flat_map(|r| crate::css::split_rules(r))
            .collect());
    }
    Err(type_error("Illegal invocation: not a CSSStyleSheet"))
}

/// Changes a sheet's rules: a `<style>`'s text, or a made sheet's list.
fn change_rules(
    this: &JsValue,
    context: &mut Context,
    f: impl FnOnce(&mut Vec<String>),
) -> JsResult<()> {
    if let Some(id) = handle_of(this, Kind::Sheet) {
        change_element_rules(context, id, f);
        return Ok(());
    }
    if let Some(id) = handle_of(this, Kind::Constructed) {
        let state = shared(context);
        let mut state = state.borrow_mut();
        if let Some(rules) = state.constructed_sheets.get_mut(&id) {
            let mut split: Vec<String> = rules
                .iter()
                .flat_map(|r| crate::css::split_rules(r))
                .collect();
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
            Ok(native::array(cx, out))
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
            Ok(native::array(cx, out))
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
    ("type", Some(|_, _, _| Ok(js_text("text/css"))), None),
    ("title", Some(|_, _, _| Ok(JsValue::null())), None),
    (
        "parentStyleSheet",
        Some(|_, _, _| Ok(JsValue::null())),
        None,
    ),
    ("ownerRule", Some(|_, _, _| Ok(JsValue::null())), None),
    (
        "disabled",
        Some(|_, _, _| Ok(false.into())),
        Some(|_, _, _| Ok(JsValue::undefined())),
    ),
    (
        "media",
        Some(|_, _, cx| {
            let media = native::array(cx, Vec::new());
            if let Some(object) = media.as_object() {
                set(&object, "mediaText", js_text(""), cx)?;
            }
            Ok(media)
        }),
        None,
    ),
];

const SHEET_METHODS: &[Method] = &[
    ("insertRule", 2, |this, args, cx| {
        let rule = arg_text(args, 0, cx)?;
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
        let selector = arg_text(args, 0, cx)?;
        let body = arg_text(args, 1, cx)?;
        change_rules(this, cx, |rules| {
            rules.push(format!("{selector} {{ {body} }}"))
        })?;
        Ok((-1).into())
    }),
    ("removeRule", 1, |this, args, cx| {
        super::call_method(this, "deleteRule", args, cx)
    }),
    ("replaceSync", 1, |this, args, cx| {
        let css = arg_text(args, 0, cx)?;
        change_rules(this, cx, |rules| *rules = crate::css::split_rules(&css))?;
        Ok(JsValue::undefined())
    }),
    ("replace", 1, |this, args, cx| {
        let css = arg_text(args, 0, cx)?;
        change_rules(this, cx, |rules| *rules = crate::css::split_rules(&css))?;
        super::resolved(this.clone(), cx)
    }),
];

/// The sheet of a `<style>` element: the same object each time it is asked for.
fn element_sheet(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let element = wrap(context, id);
    let Some(element_object) = element.as_object() else {
        return Ok(JsValue::null());
    };
    let key = js_string!("__sheet");
    let known = element_object.get(key.clone(), context)?;
    if known.is_object() {
        return Ok(known);
    }
    let sheet = native_instance(context, "CSSStyleSheet", Kind::Sheet, id);
    hidden(&element_object, key, sheet.clone(), context)?;
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
    let ids: Vec<NodeId> = read_dom(context, |dom| {
        dom.style_sheets().into_iter().map(|(id, _)| id).collect()
    });
    let mut out = Vec::new();
    for id in ids {
        out.push(element_sheet(context, id)?);
    }
    let list = native::array(context, out);
    if let Some(object) = list.as_object() {
        let item = function(context, "item", 1, |this, args, cx| {
            let at = arg(args, 0).to_number(cx)? as u32;
            match this.as_object() {
                Some(list) => list.get(at, cx),
                None => Ok(JsValue::null()),
            }
        });
        hidden(&object, js_string!("item"), item, context)?;
    }
    Ok(list)
}

/// `adoptedStyleSheets` of the tree rooted at `tree` (a document or a shadow root).
pub fn adopted_sheets(tree: NodeId, context: &mut Context) -> JsResult<JsValue> {
    let sheets: Vec<JsValue> = shared(context)
        .borrow()
        .adopted
        .get(&tree)
        .map(|list| list.iter().map(|(_, s)| s.clone().into()).collect())
        .unwrap_or_default();
    Ok(native::array(context, sheets))
}

/// Sets the `adoptedStyleSheets` of the tree rooted at `tree`; `owner` names its class in the
/// error.
pub fn set_adopted_sheets(
    tree: NodeId,
    owner: &str,
    given: &JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut adopted = Vec::new();
    for sheet in list_of(given, context)? {
        match (handle_of(&sheet, Kind::Constructed), sheet.as_object()) {
            (Some(id), Some(object)) => adopted.push((id, object)),
            _ => {
                return Err(type_error(format!(
                    "Failed to set the 'adoptedStyleSheets' property on '{owner}': Failed to convert value to 'CSSStyleSheet'."
                )));
            }
        }
    }
    let state = shared(context);
    let mut state = state.borrow_mut();
    state.adopted.insert(tree, adopted);
    state.sheets_changed();
    Ok(JsValue::undefined())
}

fn construct_sheet(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let id = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let id = state.id();
        state.constructed_sheets.insert(id, Vec::new());
        id
    };
    Ok(handle_instance(new_target, Kind::Constructed, id, context)?.into())
}

const CSS_FUNCTIONS: &[Method] = &[
    // CSS.escape, as the CSSOM writes it.
    ("escape", 1, |_, args, cx| {
        let value = arg_text(args, 0, cx)?;
        let mut out = String::new();
        let chars: Vec<char> = value.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            let code = c as u32;
            if code == 0 {
                out.push('\u{FFFD}');
            } else if (0x1..=0x1f).contains(&code)
                || code == 0x7f
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
        Ok(js_text(&out))
    }),
    ("supports", 2, |_, args, cx| {
        // A selector is supported when it parses; a declaration, always.
        let first = arg_text(args, 0, cx)?;
        if let Some(selector) = first
            .trim()
            .strip_prefix("selector(")
            .and_then(|s| s.strip_suffix(')'))
        {
            return Ok(crate::css::SelectorList::parse(selector).is_ok().into());
        }
        Ok(true.into())
    }),
    ("registerProperty", 1, |_, _, _| Ok(JsValue::undefined())),
];

// ─── Range and Selection ────────────────────────────────────────────────────────────────────

fn construct_range(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let range = handle_instance(new_target, Kind::Plain, 0, context)?;
    let document = context
        .global_object()
        .get(js_string!("document"), context)?;
    for key in ["startContainer", "endContainer"] {
        set(&range, key, document.clone(), context)?;
    }
    set(&range, "startOffset", 0, context)?;
    set(&range, "endOffset", 0, context)?;
    Ok(range.into())
}

pub fn new_range(context: &mut Context) -> JsResult<JsValue> {
    let constructor =
        constructor_of(context, "Range").ok_or_else(|| type_error("Range is not installed"))?;
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

fn set_boundary(
    this: &JsValue,
    context: &mut Context,
    which: &str,
    node: JsValue,
    offset: u32,
) -> JsResult<()> {
    let Some(range) = this.as_object() else {
        return Ok(());
    };
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
    Ok(found.map(|(parent, index)| (wrap(context, parent), index)))
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
            let (Some(a), Some(b)) = (node_id(&start), node_id(&end)) else {
                return Ok(start);
            };
            let common = read_dom(cx, |dom| {
                let chain = ancestry(dom, a);
                ancestry(dom, b)
                    .into_iter()
                    .rev()
                    .find(|n| chain.contains(n))
            });
            Ok(common.map_or(start, |c| wrap(cx, c)))
        }),
        None,
    ),
];

fn split_text_units(content: &str, start: u32, end: u32) -> (String, String, String) {
    let units: Vec<u16> = content.encode_utf16().collect();
    let (a, b) = (
        (start as usize).min(units.len()),
        (end as usize)
            .min(units.len())
            .max(start as usize)
            .min(units.len()),
    );
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
        let markup = arg_text(args, 0, cx)?;
        let fragment = with_dom(cx, |dom| {
            let fragment = dom.create_fragment();
            crate::html::parse_runnable(dom, fragment, &markup);
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
            let children: Vec<NodeId> = dom
                .children(parent)
                .iter()
                .skip(start as usize)
                .take(end.saturating_sub(start) as usize)
                .copied()
                .collect();
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
        let (Some(parent), Some(end_parent)) = (node_id(&container), node_id(&end_container))
        else {
            return Ok(JsValue::undefined());
        };
        if parent != end_parent {
            return Ok(JsValue::undefined());
        }
        with_dom(cx, |dom| {
            if let Some(NodeKind::Text(content)) = dom.get(parent).map(|n| n.kind.clone()) {
                let (head, _, tail) = split_text_units(&content, start, end);
                dom.set_text(parent, &format!("{head}{tail}"));
            } else {
                let doomed: Vec<NodeId> = dom
                    .children(parent)
                    .iter()
                    .skip(start as usize)
                    .take(end.saturating_sub(start) as usize)
                    .copied()
                    .collect();
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
                let picked: Vec<NodeId> = dom
                    .children(parent)
                    .iter()
                    .skip(start as usize)
                    .take(end.saturating_sub(start) as usize)
                    .copied()
                    .collect();
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
        let (Some(a), Some(b)) = (node_id(&container), node_id(&end_container)) else {
            return Ok(js_text(""));
        };
        Ok(js_text(&read_dom(cx, |dom| {
            if a == b {
                if let Some(NodeKind::Text(content)) = dom.get(a).map(|n| &n.kind) {
                    return split_text_units(content, start, end).1;
                }
                return dom
                    .children(a)
                    .iter()
                    .skip(start as usize)
                    .take(end.saturating_sub(start) as usize)
                    .map(|c| dom.text_content(*c))
                    .collect();
            }
            dom.text_content(a)
        })))
    }),
    ("getBoundingClientRect", 0, |this, _, cx| {
        let (container, _) = boundary(this, cx, "start")?;
        let element = read_dom(cx, |dom| {
            node_id(&container).map(|id| {
                if dom.get(id).is_some_and(|n| n.is_element()) {
                    id
                } else {
                    dom.parent(id).unwrap_or(id)
                }
            })
        });
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
        Ok(native::array(cx, vec![rect]))
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
    let ranges = native::array(context, Vec::new());
    set(&made, "__ranges", ranges, context)?;
    hidden(&global, key, made.clone(), context)?;
    Ok(made.into())
}

fn selection_ranges(this: &JsValue, context: &mut Context) -> JsResult<Vec<JsValue>> {
    let ranges = get(this, "__ranges", context)?;
    list_of(&ranges, context)
}

fn set_selection_ranges(
    this: &JsValue,
    context: &mut Context,
    ranges: Vec<JsValue>,
) -> JsResult<()> {
    let list = native::array(context, ranges);
    if let Some(object) = this.as_object() {
        set(&object, "__ranges", list, context)?;
    }
    let document = context
        .global_object()
        .get(js_string!("document"), context)?;
    super::events::fire(
        context,
        &document,
        "Event",
        "selectionchange",
        &[("bubbles", false.into())],
    )?;
    Ok(())
}

const SELECTION_ACCESSORS: &[Accessor] = &[
    (
        "rangeCount",
        Some(|this, _, cx| Ok((selection_ranges(this, cx)?.len() as u32).into())),
        None,
    ),
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
            Ok(js_text(match ranges.first() {
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
            None => Err(super::dom_error(
                cx,
                "Failed to execute 'getRangeAt' on 'Selection': The index provided is out of range.",
                "IndexSizeError",
            )),
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
    ("setPosition", 2, |this, args, cx| {
        super::call_method(this, "collapse", args, cx)
    }),
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
    ("toString", 0, |this, _, cx| {
        match selection_ranges(this, cx)?.first() {
            Some(range) => super::call_method(range, "toString", &[], cx),
            None => Ok(js_text("")),
        }
    }),
];

// ─── Geometry ───────────────────────────────────────────────────────────────────────────────

/// A matrix's sixteen numbers, column by column as CSS writes them (`m11`, `m12`, … `m44`).
pub(crate) type Matrix = [f64; 16];

const IDENTITY: Matrix = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];

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
    inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
        + m[9] * m[7] * m[14]
        + m[13] * m[6] * m[11]
        - m[13] * m[7] * m[10];
    inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
        - m[8] * m[7] * m[14]
        - m[12] * m[6] * m[11]
        + m[12] * m[7] * m[10];
    inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
        + m[8] * m[7] * m[13]
        + m[12] * m[5] * m[11]
        - m[12] * m[7] * m[9];
    inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
        - m[8] * m[6] * m[13]
        - m[12] * m[5] * m[10]
        + m[12] * m[6] * m[9];
    inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
        - m[9] * m[3] * m[14]
        - m[13] * m[2] * m[11]
        + m[13] * m[3] * m[10];
    inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
        + m[8] * m[3] * m[14]
        + m[12] * m[2] * m[11]
        - m[12] * m[3] * m[10];
    inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
        - m[8] * m[3] * m[13]
        - m[12] * m[1] * m[11]
        + m[12] * m[3] * m[9];
    inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
        + m[8] * m[2] * m[13]
        + m[12] * m[1] * m[10]
        - m[12] * m[2] * m[9];
    inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
        + m[5] * m[3] * m[14]
        + m[13] * m[2] * m[7]
        - m[13] * m[3] * m[6];
    inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
        - m[4] * m[3] * m[14]
        - m[12] * m[2] * m[7]
        + m[12] * m[3] * m[6];
    inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
        + m[4] * m[3] * m[13]
        + m[12] * m[1] * m[7]
        - m[12] * m[3] * m[5];
    inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
        - m[4] * m[2] * m[13]
        - m[12] * m[1] * m[6]
        + m[12] * m[2] * m[5];
    inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
        - m[5] * m[3] * m[10]
        - m[9] * m[2] * m[7]
        + m[9] * m[3] * m[6];
    inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
        + m[4] * m[3] * m[10]
        + m[8] * m[2] * m[7]
        - m[8] * m[3] * m[6];
    inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
        - m[4] * m[3] * m[9]
        - m[8] * m[1] * m[7]
        + m[8] * m[3] * m[5];
    inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
        + m[4] * m[2] * m[9]
        + m[8] * m[1] * m[6]
        - m[8] * m[2] * m[5];
    let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
    if det.abs() < f64::EPSILON {
        return None;
    }
    Some(inv.map(|v| v / det))
}

pub(crate) fn matrix_of(value: &JsValue, context: &mut Context) -> JsResult<Matrix> {
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
    let numbers: Vec<f64> = list_of(init, context)?
        .into_iter()
        .map(|v| v.to_number(context))
        .collect::<JsResult<_>>()?;
    Ok(match numbers.len() {
        6 => from_2d(
            numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
        ),
        16 => {
            let mut out = [0.; 16];
            out.copy_from_slice(&numbers);
            out
        }
        _ => {
            return Err(type_error(
                "Failed to construct 'DOMMatrix': The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix.",
            ));
        }
    })
}

pub(crate) fn from_2d(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Matrix {
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
        let Some((name, arguments)) = part.split_once('(') else {
            continue;
        };
        let numbers: Vec<f64> = arguments
            .split([',', ' '])
            .filter(|n| !n.is_empty())
            .filter_map(|n| {
                n.trim_end_matches("px")
                    .trim_end_matches("deg")
                    .parse()
                    .ok()
            })
            .collect();
        let n = |i: usize, default: f64| numbers.get(i).copied().unwrap_or(default);
        let step = match name.trim() {
            "matrix" if numbers.len() == 6 => {
                from_2d(n(0, 1.), n(1, 0.), n(2, 0.), n(3, 1.), n(4, 0.), n(5, 0.))
            }
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
    let is_2d = m[2] == 0.
        && m[3] == 0.
        && m[6] == 0.
        && m[7] == 0.
        && m[8] == 0.
        && m[9] == 0.
        && m[10] == 1.
        && m[11] == 0.
        && m[14] == 0.
        && m[15] == 1.;
    set(object, "is2D", is_2d, context)?;
    set(object, "isIdentity", *m == IDENTITY, context)?;
    Ok(())
}

pub(crate) fn make_matrix(context: &mut Context, m: &Matrix) -> JsResult<JsValue> {
    let object = native_instance(context, "DOMMatrix", Kind::Plain, 0);
    set_matrix(&object, m, context)?;
    Ok(object.into())
}

pub fn identity_matrix(context: &mut Context) -> JsResult<JsValue> {
    make_matrix(context, &IDENTITY)
}

fn construct_matrix(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let m = matrix_from_init(&arg(args, 0), context)?;
    let object = handle_instance(new_target, Kind::Plain, 0, context)?;
    set_matrix(&object, &m, context)?;
    Ok(object.into())
}

fn number_arg(
    args: &[JsValue],
    index: usize,
    default: f64,
    context: &mut Context,
) -> JsResult<f64> {
    let value = arg(args, index);
    if value.is_undefined() {
        Ok(default)
    } else {
        value.to_number(context)
    }
}

fn matrix_step(
    this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
    name: &str,
) -> JsResult<Matrix> {
    let m = matrix_of(this, context)?;
    Ok(match name {
        "multiply" => multiply(&m, &matrix_from_init(&arg(args, 0), context)?),
        "preMultiply" => multiply(&matrix_from_init(&arg(args, 0), context)?, &m),
        "translate" => multiply(
            &m,
            &translation(
                number_arg(args, 0, 0., context)?,
                number_arg(args, 1, 0., context)?,
                number_arg(args, 2, 0., context)?,
            ),
        ),
        "scale" => {
            let x = number_arg(args, 0, 1., context)?;
            let y = number_arg(args, 1, x, context)?;
            let z = number_arg(args, 2, 1., context)?;
            let (ox, oy) = (
                number_arg(args, 3, 0., context)?,
                number_arg(args, 4, 0., context)?,
            );
            let moved = multiply(&m, &translation(ox, oy, 0.));
            let scaled = multiply(&moved, &scaling(x, y, z));
            multiply(&scaled, &translation(-ox, -oy, 0.))
        }
        "scaleNonUniform" => multiply(
            &m,
            &scaling(
                number_arg(args, 0, 1., context)?,
                number_arg(args, 1, 1., context)?,
                1.,
            ),
        ),
        "rotate" => {
            let angle = if arg(args, 1).is_undefined() {
                number_arg(args, 0, 0., context)?
            } else {
                number_arg(args, 2, 0., context)?
            };
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

const MATRIX_STEPS: &[&str] = &[
    "multiply",
    "preMultiply",
    "translate",
    "scale",
    "scaleNonUniform",
    "rotate",
    "skewX",
    "skewY",
    "flipX",
    "flipY",
    "inverse",
];

fn simple_class(
    context: &mut Context,
    name: &'static str,
    construct: boa_engine::native_function::NativeFunctionPointer,
    parent: Option<&str>,
    methods: &[Method],
    accessors: &[Accessor],
    global: bool,
) -> JsResult<JsObject> {
    Ok(define_class(
        context,
        ClassSpec {
            name,
            length: 0,
            construct,
            parent,
            methods,
            accessors,
            statics: &[],
            global,
        },
    )?
    .1)
}

fn install_geometry(context: &mut Context) -> JsResult<()> {
    let prototype = simple_class(
        context,
        "DOMMatrixReadOnly",
        construct_matrix,
        None,
        &[],
        &[],
        true,
    )?;
    let own = simple_class(
        context,
        "DOMMatrix",
        construct_matrix,
        Some("DOMMatrixReadOnly"),
        &[],
        &[],
        true,
    )?;
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
                if v.is_undefined() {
                    Ok(default)
                } else {
                    v.to_number(cx)
                }
            };
            let (x, y, z, w) = (
                read("x", 0., cx)?,
                read("y", 0., cx)?,
                read("z", 0., cx)?,
                read("w", 1., cx)?,
            );
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
            Ok(
                boa_engine::object::builtins::JsFloat32Array::from_iter(m.map(|v| v as f32), cx)?
                    .into(),
            )
        }),
        ("toFloat64Array", 0, |this, _, cx| {
            let m = matrix_of(this, cx)?;
            Ok(boa_engine::object::builtins::JsFloat64Array::from_iter(m, cx)?.into())
        }),
        ("toString", 0, |this, _, cx| {
            let m = matrix_of(this, cx)?;
            let is_2d = get(this, "is2D", cx)?.to_boolean();
            Ok(js_text(&if is_2d {
                format!(
                    "matrix({}, {}, {}, {}, {}, {})",
                    m[0], m[1], m[4], m[5], m[12], m[13]
                )
            } else {
                format!(
                    "matrix3d({})",
                    m.iter().map(f64::to_string).collect::<Vec<_>>().join(", ")
                )
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
    context.register_global_property(
        js_string!("WebKitCSSMatrix"),
        matrix,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
    simple_class(
        context,
        "DOMPointReadOnly",
        construct_point,
        None,
        &[
            ("matrixTransform", 1, |this, args, cx| {
                super::call_method(
                    &arg(args, 0),
                    "transformPoint",
                    std::slice::from_ref(this),
                    cx,
                )
            }),
            ("toJSON", 0, |this, _, cx| {
                let out = plain_object(cx);
                for key in ["x", "y", "z", "w"] {
                    let value = get(this, key, cx)?;
                    set(&out, key, value, cx)?;
                }
                Ok(out.into())
            }),
        ],
        &[],
        true,
    )?;
    simple_class(
        context,
        "DOMPoint",
        construct_point,
        Some("DOMPointReadOnly"),
        &[],
        &[],
        true,
    )?;
    simple_class(
        context,
        "DOMRectReadOnly",
        construct_rect,
        None,
        &[("toJSON", 0, |this, _, cx| {
            let out = plain_object(cx);
            for key in [
                "x", "y", "width", "height", "top", "right", "bottom", "left",
            ] {
                let value = get(this, key, cx)?;
                set(&out, key, value, cx)?;
            }
            Ok(out.into())
        })],
        &[],
        true,
    )?;
    simple_class(
        context,
        "DOMRect",
        construct_rect,
        Some("DOMRectReadOnly"),
        &[],
        &[],
        true,
    )?;
    if let Some(constructor) = constructor_of(context, "DOMRect") {
        let from = function(context, "fromRect", 1, |_, args, cx| {
            let given = arg(args, 0);
            let mut numbers = [0.; 4];
            for (i, key) in ["x", "y", "width", "height"].iter().enumerate() {
                let v = get(&given, key, cx)?;
                numbers[i] = if v.is_undefined() {
                    0.
                } else {
                    v.to_number(cx)?
                };
            }
            make_rect(cx, numbers)
        });
        set(&constructor, "fromRect", from, context)?;
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

fn construct_point(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let object = handle_instance(new_target, Kind::Plain, 0, context)?;
    for (i, (key, default)) in [("x", 0.), ("y", 0.), ("z", 0.), ("w", 1.)]
        .into_iter()
        .enumerate()
    {
        let value = number_arg(args, i, default, context)?;
        set(&object, key, value, context)?;
    }
    Ok(object.into())
}

fn set_rect(
    object: &JsObject,
    [x, y, width, height]: [f64; 4],
    context: &mut Context,
) -> JsResult<()> {
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

fn construct_rect(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let object = handle_instance(new_target, Kind::Plain, 0, context)?;
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
        let f = function(context, name, 1, |_, _, cx| {
            Err(super::dom_error(
                cx,
                "The index provided is out of range.",
                "IndexSizeError",
            ))
        });
        set(&ranges, name, f, context)?;
    }
    Ok(ranges.into())
}

fn construct_data_transfer(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let transfer = handle_instance(new_target, Kind::Plain, 0, context)?;
    let data = plain_object(context);
    hidden(&transfer, js_string!("__data"), data, context)?;
    set(&transfer, "dropEffect", js_text("none"), context)?;
    set(&transfer, "effectAllowed", js_text("all"), context)?;
    let files = native::array(context, Vec::new());
    set(&transfer, "files", files, context)?;
    let items = native::array(context, Vec::new());
    set(&transfer, "items", items, context)?;
    Ok(transfer.into())
}

const DATA_TRANSFER_METHODS: &[Method] = &[
    ("setData", 2, |this, args, cx| {
        let data = get(this, "__data", cx)?;
        let format = arg_text(args, 0, cx)?.to_ascii_lowercase();
        let format = if format == "text" {
            "text/plain".to_owned()
        } else {
            format
        };
        let value = arg_text(args, 1, cx)?;
        if let Some(data) = data.as_object() {
            set(&data, &format, js_text(&value), cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("getData", 1, |this, args, cx| {
        let data = get(this, "__data", cx)?;
        let format = arg_text(args, 0, cx)?.to_ascii_lowercase();
        let format = if format == "text" {
            "text/plain".to_owned()
        } else {
            format
        };
        let value = get(&data, &format, cx)?;
        Ok(if value.is_undefined() {
            js_text("")
        } else {
            value
        })
    }),
    ("clearData", 1, |this, args, cx| {
        let format = arg(args, 0);
        if format.is_undefined() {
            let data = plain_object(cx);
            if let Some(this) = this.as_object() {
                hidden(&this, js_string!("__data"), data, cx)?;
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
            Some(data) => data
                .own_property_keys(cx)?
                .into_iter()
                .map(|k| js_text(&k.to_string()))
                .collect(),
            None => Vec::new(),
        };
        Ok(native::array(cx, keys))
    }),
    None,
)];

// ─── Element constructors ───────────────────────────────────────────────────────────────────

/// A constructor that makes an element of `tag`, whose prototype is its class's: `Image` is
/// `HTMLImageElement`'s, as in a browser.
fn element_constructor(
    context: &mut Context,
    name: &str,
    class: &str,
    make: boa_engine::native_function::NativeFunctionPointer,
) -> JsResult<()> {
    let prototype = crate::state::class_prototype(context, class);
    let constructor: JsObject =
        FunctionObjectBuilder::new(context.realm(), NativeFunction::from_fn_ptr(make))
            .name(JsString::from(name))
            .length(0)
            .constructor(true)
            .build()
            .into();
    constructor.define_property_or_throw(
        js_string!("prototype"),
        PropertyDescriptor::builder()
            .value(prototype)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    context.register_global_property(
        JsString::from(name),
        constructor,
        Attribute::WRITABLE | Attribute::CONFIGURABLE,
    )?;
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
    let element = crate::state::class_prototype(context, "Element");
    super::extend_prototype(context, &element, ATTRIBUTE_METHODS, &[])?;
    simple_class(
        context,
        "Attr",
        illegal_constructor,
        Some("Node"),
        &[],
        &[],
        true,
    )?;
    let map = simple_class(
        context,
        "NamedNodeMap",
        illegal_constructor,
        None,
        NAMED_NODE_MAP_METHODS,
        &[],
        true,
    )?;
    let array_prototype = context.intrinsics().constructors().array().prototype();
    map.set_prototype(Some(array_prototype));
    simple_class(
        context,
        "ShadowRoot",
        illegal_constructor,
        Some("DocumentFragment"),
        &[],
        SHADOW_ROOT_ACCESSORS,
        true,
    )?;
    simple_class(
        context,
        "CustomElementRegistry",
        illegal_constructor,
        None,
        super::custom::METHODS,
        &[],
        true,
    )?;
    super::fonts::install(context)?;
    let registry = native_instance(context, "CustomElementRegistry", Kind::Plain, 0);
    context.register_global_property(js_string!("customElements"), registry, Attribute::all())?;
    simple_class(
        context,
        "StyleSheet",
        illegal_constructor,
        None,
        &[],
        &[],
        true,
    )?;
    simple_class(
        context,
        "CSSStyleSheet",
        construct_sheet,
        Some("StyleSheet"),
        SHEET_METHODS,
        SHEET_ACCESSORS,
        true,
    )?;
    for name in [
        "CSSRule",
        "CSSStyleRule",
        "CSSMediaRule",
        "StyleSheetList",
        "CSSRuleList",
        "MediaList",
    ] {
        simple_class(context, name, illegal_constructor, None, &[], &[], true)?;
    }
    let css = plain_object(context);
    for (name, length, f) in CSS_FUNCTIONS {
        let made = function(context, name, *length, *f);
        set(&css, name, made, context)?;
    }
    context.register_global_property(js_string!("CSS"), css, Attribute::all())?;
    simple_class(
        context,
        "Range",
        construct_range,
        None,
        RANGE_METHODS,
        RANGE_ACCESSORS,
        true,
    )?;
    simple_class(
        context,
        "StaticRange",
        construct_range,
        None,
        &[],
        &[],
        true,
    )?;
    simple_class(
        context,
        "Selection",
        illegal_constructor,
        None,
        SELECTION_METHODS,
        SELECTION_ACCESSORS,
        true,
    )?;
    crate::canvas::install(context)?;
    simple_class(
        context,
        "DataTransfer",
        construct_data_transfer,
        None,
        DATA_TRANSFER_METHODS,
        DATA_TRANSFER_ACCESSORS,
        true,
    )?;
    install_geometry(context)?;
    element_constructor(context, "Image", "HTMLImageElement", construct_image)?;
    element_constructor(context, "Audio", "HTMLAudioElement", construct_audio)?;
    element_constructor(context, "Option", "HTMLOptionElement", construct_option)?;
    Ok(())
}
