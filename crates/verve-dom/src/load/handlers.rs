//! Event handlers written as attributes (`<img onload="…">`, `<body onload="…">`): each becomes
//! the element's handler property, a function of `event` with the attribute's text as its body.
//! An attribute is read when it is set, and when its element first joins the document; moving
//! the element later leaves what a script made of the property alone.

use boa_engine::{Context, JsString, JsValue};

use crate::bind::node::wrap;
use crate::dom::NodeId;
use crate::state::{State, shared};

/// The handlers `<body>` and `<frameset>` take for the window (HTML's "Window-reflecting body
/// element event handler set", with those `GlobalEventHandlers` the window also has).
const WINDOW_REFLECTING: &[&str] = &[
    "onafterprint",
    "onbeforeprint",
    "onbeforeunload",
    "onhashchange",
    "onlanguagechange",
    "onmessage",
    "onmessageerror",
    "onoffline",
    "ononline",
    "onpagehide",
    "onpagereveal",
    "onpageshow",
    "onpageswap",
    "onpopstate",
    "onrejectionhandled",
    "onstorage",
    "onunhandledrejection",
    "onunload",
    "onblur",
    "onerror",
    "onfocus",
    "onload",
    "onresize",
    "onscroll",
];

type Handler = (NodeId, String, String, bool);

fn handler_of(state: &State, node: NodeId, name: &str) -> Option<Handler> {
    let element = state.dom.get(node).filter(|n| n.is_element())?;
    // Not a template's content, and not a document of the page's own making (`DOMParser`,
    // `createHTMLDocument`): their markup is inert, and `<body onfocus>` there must not
    // reach the window.
    let foreign = state.dom.document_of(node).is_some() && !state.dom.is_connected(node);
    let content = state
        .template_contents
        .values()
        .any(|c| *c == state.dom.root_of(node));
    if foreign || content || state.dom.in_template(node) {
        return None;
    }
    let value = state.dom.get_attr(node, name).unwrap_or_default();
    let on_body = matches!(element.tag(), Some("body" | "frameset"));
    let to_window = on_body && WINDOW_REFLECTING.contains(&name) && state.dom.is_connected(node);
    Some((node, name.to_owned(), value, to_window))
}

/// The handlers the elements of the subtrees at `roots` say, those not read yet.
pub(super) fn connected(context: &mut Context, roots: &[NodeId]) {
    let found: Vec<Handler> = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        let mut found = Vec::new();
        let nodes: Vec<NodeId> = roots
            .iter()
            .flat_map(|root| state.dom.shadow_including_tree(*root))
            .filter(|id| !state.dom.in_template(*id))
            .collect();
        for id in nodes {
            for (name, _) in state.dom.attributes(id) {
                if !is_handler(&name) || !state.load.handled.insert((id, name.clone())) {
                    continue;
                }
                found.extend(handler_of(&state, id, &name).filter(|h| !h.2.is_empty()));
            }
        }
        found
    };
    for handler in found {
        install(context, handler);
    }
}

/// An attribute was set or removed: a handler's is read again, and gone when the attribute is.
pub(crate) fn attribute(context: &mut Context, node: NodeId, name: &str) {
    if !is_handler(name) {
        return;
    }
    let found = {
        let state = shared(context);
        let mut state = state.borrow_mut();
        state.load.handled.insert((node, name.to_owned()));
        handler_of(&state, node, name)
    };
    if let Some(handler) = found {
        install(context, handler);
    }
}

fn is_handler(name: &str) -> bool {
    name.len() > 2 && name.starts_with("on")
}

fn install(context: &mut Context, (node, name, body, to_window): Handler) {
    let target: JsValue = if to_window {
        context.global_object().into()
    } else {
        wrap(context, node)
    };
    if body.is_empty() {
        let target = target.as_object();
        if let Some(Err(error)) =
            target.map(|t| t.set(JsString::from(name), JsValue::null(), false, context))
        {
            verve::report(context, &error);
        }
        return;
    }
    let source = [
        verve::native::js_text("event"),
        verve::native::js_text(&body),
    ];
    match verve::native::construct_global(context, "Function", &source) {
        Ok(function) => {
            let Some(target) = target.as_object() else {
                return;
            };
            if let Err(error) = target.set(JsString::from(name), function, false, context) {
                verve::report(context, &error);
            }
        }
        Err(error) => verve::report(context, &error),
    }
}
