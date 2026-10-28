//! `Document` — the window's, and the ones `DOMParser` and `document.implementation` make —
//! with `DOMParser`, `XMLSerializer` and `DOMImplementation`.

use boa_engine::{Context, JsResult, JsValue, js_string};

use super::node::{
    PARENT_NODE_ACCESSORS, PARENT_NODE_METHODS, collection, read_dom, with_dom, wrap, wrap_opt,
};
use super::{
    Accessor, ClassSpec, Kind, Method, arg, arg_string, class, constructor_of, dom_error,
    extend_prototype, illegal_constructor, instance, native_instance, node_id, node_of, opt_string,
    string, text, type_error,
};
use crate::dom::{Dom, Namespace, NodeId, NodeKind};
use crate::state::shared;

/// The document a document's member is asked of: a parsed one, or the window's.
fn document_of(this: &JsValue, context: &Context) -> NodeId {
    read_dom(context, |dom| {
        node_id(this)
            .filter(|id| matches!(dom.get(*id).map(|n| &n.kind), Some(NodeKind::Document)))
            .unwrap_or(dom.document)
    })
}

fn root_element(dom: &Dom, doc: NodeId) -> Option<NodeId> {
    dom.children(doc)
        .iter()
        .copied()
        .find(|c| dom.get(*c).is_some_and(|n| n.is_element()))
}

fn child_tagged(dom: &Dom, doc: NodeId, tag: &str) -> Option<NodeId> {
    let html = root_element(dom, doc)?;
    dom.children(html)
        .iter()
        .copied()
        .find(|c| dom.get(*c).and_then(|n| n.tag()) == Some(tag))
}

/// The element `createElement` makes: one of a custom element's class when its tag was
/// defined.
pub fn create_element(context: &mut Context, tag: &str, namespace: Namespace) -> JsResult<JsValue> {
    if tag.is_empty()
        || !tag
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':')
        || tag.contains(|c: char| c.is_whitespace() || c == '<' || c == '>')
    {
        return Err(dom_error(
            context,
            &format!(
                "Failed to execute 'createElement' on 'Document': The tag name provided ('{tag}') is not a valid name."
            ),
            "InvalidCharacterError",
        ));
    }
    if namespace == Namespace::Html
        && let Some(class) = super::platform::custom_class(context, &tag.to_ascii_lowercase())
    {
        return Ok(class.construct(&[], None, context)?.into());
    }
    let id = with_dom(context, |dom| dom.create_element_ns(tag, namespace));
    Ok(wrap(context, id))
}

const DOCUMENT_METHODS: &[Method] = &[
    ("createElement", 2, |_, args, cx| {
        let tag = arg_string(args, 0, cx)?;
        create_element(cx, &tag, Namespace::Html)
    }),
    ("createElementNS", 3, |_, args, cx| {
        let namespace = opt_string(args, 0, cx)?;
        let name = arg_string(args, 1, cx)?;
        let local = name.rsplit(':').next().unwrap_or(&name).to_owned();
        create_element(cx, &local, Namespace::from_uri(namespace.as_deref()))
    }),
    ("createTextNode", 1, |_, args, cx| {
        let content = arg_string(args, 0, cx)?;
        let id = with_dom(cx, |dom| dom.create_text(&content));
        Ok(wrap(cx, id))
    }),
    ("createCDATASection", 1, |_, args, cx| {
        let content = arg_string(args, 0, cx)?;
        let id = with_dom(cx, |dom| dom.create_text(&content));
        Ok(wrap(cx, id))
    }),
    ("createDocumentFragment", 0, |_, _, cx| {
        let id = with_dom(cx, Dom::create_fragment);
        Ok(wrap(cx, id))
    }),
    ("createComment", 1, |_, args, cx| {
        let data = opt_string(args, 0, cx)?.unwrap_or_default();
        let id = with_dom(cx, |dom| dom.create_comment(&data));
        Ok(wrap(cx, id))
    }),
    ("createProcessingInstruction", 2, |_, args, cx| {
        let data = opt_string(args, 1, cx)?.unwrap_or_default();
        let id = with_dom(cx, |dom| dom.create_comment(&data));
        Ok(wrap(cx, id))
    }),
    ("createAttribute", 1, |this, args, cx| {
        let name = arg_string(args, 0, cx)?;
        super::platform::attr_object(cx, &JsValue::null(), &name.to_ascii_lowercase(), "", this)
    }),
    // The old way to make an event: of the class named, made ready by its `init…Event`.
    ("createEvent", 1, |_, args, cx| {
        let named = arg_string(args, 0, cx).unwrap_or_default();
        let class = match named.trim_end_matches('s').to_ascii_lowercase().as_str() {
            "customevent" => "CustomEvent",
            "mouseevent" => "MouseEvent",
            "uievent" => "UIEvent",
            "keyboardevent" => "KeyboardEvent",
            "focusevent" => "FocusEvent",
            "inputevent" => "InputEvent",
            "wheelevent" => "WheelEvent",
            "pointerevent" => "PointerEvent",
            "touchevent" => "TouchEvent",
            "dragevent" => "DragEvent",
            "compositionevent" => "CompositionEvent",
            "messageevent" => "MessageEvent",
            "htmlevent" | "event" => "Event",
            _ => {
                return Err(dom_error(
                    cx,
                    &format!(
                        "Failed to execute 'createEvent' on 'Document': The provided event type ('{named}') is invalid."
                    ),
                    "NotSupportedError",
                ));
            }
        };
        let constructor = cx
            .global_object()
            .get(boa_engine::JsString::from(class), cx)?
            .as_object()
            .ok_or_else(|| type_error("no such event class"))?;
        Ok(constructor.construct(&[text("")], None, cx)?.into())
    }),
    ("createRange", 0, |_, _, cx| super::platform::new_range(cx)),
    ("createNodeIterator", 3, |_, args, cx| {
        super::traversal::create(args, cx, false)
    }),
    ("createTreeWalker", 3, |_, args, cx| {
        super::traversal::create(args, cx, true)
    }),
    ("getElementById", 1, |this, args, cx| {
        let wanted = arg_string(args, 0, cx)?;
        let doc = document_of(this, cx);
        let found = read_dom(cx, |dom| {
            dom.descendants(doc)
                .into_iter()
                .find(|id| dom.get(*id).and_then(|n| n.attr("id")) == Some(wanted.as_str()))
        });
        Ok(wrap_opt(cx, found))
    }),
    ("getElementsByName", 1, |this, args, cx| {
        let wanted = arg_string(args, 0, cx)?;
        let doc = document_of(this, cx);
        let found = read_dom(cx, |dom| {
            dom.descendants(doc)
                .into_iter()
                .filter(|id| dom.get(*id).and_then(|n| n.attr("name")) == Some(wanted.as_str()))
                .collect()
        });
        Ok(super::node::node_list(found, cx))
    }),
    ("hasFocus", 0, |_, _, _| Ok(true.into())),
    ("importNode", 2, |_, args, cx| {
        let id = super::node_arg(args, 0, "importNode", "Document")?;
        if read_dom(cx, |dom| {
            matches!(dom.get(id).map(|n| &n.kind), Some(NodeKind::Document))
        }) {
            return Err(super::dom_error(
                cx,
                "不能导入文档节点",
                "NotSupportedError",
            ));
        }
        let deep = arg(args, 1).to_boolean();
        let copy = with_dom(cx, |dom| dom.clone_node(id, deep));
        Ok(wrap_opt(cx, copy))
    }),
    ("adoptNode", 1, |_, args, cx| {
        let node = arg(args, 0);
        let id = super::node_arg(args, 0, "adoptNode", "Document")?;
        if read_dom(cx, |dom| {
            matches!(dom.get(id).map(|n| &n.kind), Some(NodeKind::Document))
        }) {
            return Err(super::dom_error(
                cx,
                "不能转移文档节点",
                "NotSupportedError",
            ));
        }
        with_dom(cx, |dom| dom.detach(id));
        Ok(node)
    }),
    ("elementFromPoint", 2, |_, args, cx| {
        let x = arg(args, 0).to_number(cx)? as f32;
        let y = arg(args, 1).to_number(cx)? as f32;
        let found = crate::hit_test(&shared(cx).borrow(), x, y).first().copied();
        Ok(wrap_opt(cx, found))
    }),
    ("elementsFromPoint", 2, |_, args, cx| {
        let x = arg(args, 0).to_number(cx)? as f32;
        let y = arg(args, 1).to_number(cx)? as f32;
        let found = crate::hit_test(&shared(cx).borrow(), x, y);
        Ok(super::node::node_list(found, cx))
    }),
    ("getSelection", 0, |_, _, cx| super::platform::selection(cx)),
    ("exitFullscreen", 0, |_, _, cx| {
        let was = {
            let state = shared(cx);
            let mut state = state.borrow_mut();
            let was = state.fullscreen.take();
            if was.is_some() {
                state.requests.push(crate::Request::Fullscreen(None));
            }
            was
        };
        if let Some(id) = was {
            super::events::fire_at(
                cx,
                id,
                "Event",
                "fullscreenchange",
                &[("bubbles", true.into())],
                false,
            )?;
        }
        super::resolved(JsValue::undefined(), cx)
    }),
    // `document.write` while the page is open: what it is given read into the body.
    ("write", 1, |this, args, cx| {
        let doc = document_of(this, cx);
        let mut markup = String::new();
        for value in args {
            markup.push_str(&string(value, cx)?);
        }
        with_dom(cx, |dom| {
            let body = child_tagged(dom, doc, "body").unwrap_or(doc);
            crate::html::parse_into(dom, body, &markup);
        });
        Ok(JsValue::undefined())
    }),
    ("writeln", 1, |this, args, cx| {
        let mut with_newline = args.to_vec();
        with_newline.push(text("\n"));
        super::call_method(this, "write", &with_newline, cx)
    }),
    ("open", 0, |this, _, cx| {
        let doc = document_of(this, cx);
        with_dom(cx, |dom| {
            if let Some(body) = child_tagged(dom, doc, "body") {
                dom.empty(body);
            }
        });
        Ok(this.clone())
    }),
    ("close", 0, |_, _, _| Ok(JsValue::undefined())),
    // The old editing commands: answered as a page without a focused editable region answers.
    ("execCommand", 3, |_, args, cx| {
        let command = arg_string(args, 0, cx)?.to_ascii_lowercase();
        if command == "copy" {
            let selected =
                super::call_method(&super::platform::selection(cx)?, "toString", &[], cx)?;
            let selected = string(&selected, cx)?;
            if !selected.is_empty() {
                shared(cx)
                    .borrow_mut()
                    .requests
                    .push(crate::Request::CopyText(selected));
                return Ok(true.into());
            }
        }
        Ok(false.into())
    }),
    ("queryCommandSupported", 1, |_, args, cx| {
        let command = arg_string(args, 0, cx)?.to_ascii_lowercase();
        Ok(matches!(
            command.as_str(),
            "copy" | "cut" | "paste" | "inserttext" | "delete" | "selectall" | "undo" | "redo"
        )
        .into())
    }),
    ("queryCommandEnabled", 1, |_, _, _| Ok(true.into())),
    ("queryCommandState", 1, |_, _, _| Ok(false.into())),
    ("queryCommandValue", 1, |_, _, _| Ok(text(""))),
    ("startViewTransition", 1, |_, args, cx| {
        let update = arg(args, 0);
        if update.is_callable() {
            verve::call(cx, &update, &JsValue::undefined(), &[])?;
        }
        let transition = super::plain_object(cx);
        let done = super::resolved(JsValue::undefined(), cx)?;
        for key in ["finished", "ready", "updateCallbackDone"] {
            super::set(&transition, key, done.clone(), cx)?;
        }
        let skip = super::function(cx, "skipTransition", 0, |_, _, _| Ok(JsValue::undefined()));
        super::set(&transition, "skipTransition", skip, cx)?;
        Ok(transition.into())
    }),
];

const DOCUMENT_ACCESSORS: &[Accessor] = &[
    (
        "documentElement",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let id = read_dom(cx, |dom| root_element(dom, doc));
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    (
        "body",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let id = read_dom(cx, |dom| child_tagged(dom, doc, "body"));
            Ok(wrap_opt(cx, id))
        }),
        Some(|this, args, cx| {
            let doc = document_of(this, cx);
            let body = super::node_arg(args, 0, "body", "Document")?;
            with_dom(cx, |dom| {
                if let Some(html) = root_element(dom, doc) {
                    if let Some(old) = child_tagged(dom, doc, "body") {
                        dom.insert_before(html, body, Some(old));
                        dom.detach(old);
                    } else {
                        dom.append(html, body);
                    }
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "head",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let id = read_dom(cx, |dom| child_tagged(dom, doc, "head"));
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    (
        "activeElement",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let id = read_dom(cx, |dom| {
                if doc != dom.document {
                    return None;
                }
                dom.focused
                    .filter(|f| dom.is_connected(*f))
                    .or(Some(dom.body))
            });
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    (
        "defaultView",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let window = read_dom(cx, |dom| doc == dom.document);
            Ok(if window {
                cx.global_object().into()
            } else {
                JsValue::null()
            })
        }),
        None,
    ),
    (
        "title",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let title = read_dom(cx, |dom| {
                dom.descendants(doc)
                    .into_iter()
                    .find(|n| {
                        dom.get(*n).is_some_and(|x| {
                            x.tag() == Some("title") && x.namespace == Namespace::Html
                        })
                    })
                    .map(|t| {
                        dom.text_content(t)
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
            });
            Ok(text(&title.unwrap_or_else(|| {
                shared(cx).borrow().title_fallback.clone()
            })))
        }),
        Some(|this, args, cx| {
            let doc = document_of(this, cx);
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| {
                let found = dom
                    .descendants(doc)
                    .into_iter()
                    .find(|n| dom.get(*n).and_then(|x| x.tag()) == Some("title"));
                let title = match found {
                    Some(t) => t,
                    None => {
                        let t = dom.create_element("title");
                        if let Some(head) = child_tagged(dom, doc, "head") {
                            dom.append(head, t);
                        }
                        t
                    }
                };
                dom.set_text(title, &value);
            });
            let state = shared(cx);
            let mut state = state.borrow_mut();
            state.requests.push(crate::Request::Title(value));
            state.touch();
            Ok(JsValue::undefined())
        }),
    ),
    (
        "location",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            if read_dom(cx, |dom| doc != dom.document) {
                return Ok(JsValue::null());
            }
            cx.global_object().get(js_string!("location"), cx)
        }),
        Some(|_, args, cx| {
            let href = arg_string(args, 0, cx)?;
            super::window::navigate(cx, &href, false)?;
            Ok(JsValue::undefined())
        }),
    ),
    (
        "URL",
        Some(|_, _, cx| Ok(text(shared(cx).borrow().url.as_str()))),
        None,
    ),
    (
        "documentURI",
        Some(|_, _, cx| Ok(text(shared(cx).borrow().url.as_str()))),
        None,
    ),
    (
        "domain",
        Some(|_, _, cx| Ok(text(shared(cx).borrow().url.host_str().unwrap_or("")))),
        Some(|_, _, _| Ok(JsValue::undefined())),
    ),
    ("referrer", Some(|_, _, _| Ok(text(""))), None),
    (
        "cookie",
        Some(|_, _, cx| {
            let global = cx.global_object();
            let jar = global.get(js_string!("__cookies"), cx)?;
            Ok(if jar.is_undefined() { text("") } else { jar })
        }),
        Some(|_, args, cx| {
            let given = arg_string(args, 0, cx)?;
            let pair = given.split(';').next().unwrap_or("").trim().to_owned();
            let Some((name, _)) = pair.split_once('=') else {
                return Ok(JsValue::undefined());
            };
            let global = cx.global_object();
            let jar = global.get(js_string!("__cookies"), cx)?;
            let jar = if jar.is_undefined() {
                String::new()
            } else {
                string(&jar, cx)?
            };
            let expired = given.to_ascii_lowercase().contains("max-age=0")
                || given
                    .to_ascii_lowercase()
                    .contains("expires=thu, 01 jan 1970");
            let mut kept: Vec<String> = jar
                .split("; ")
                .filter(|c| !c.is_empty() && !c.starts_with(&format!("{}=", name.trim())))
                .map(str::to_owned)
                .collect();
            if !expired {
                kept.push(pair);
            }
            super::hidden(&global, js_string!("__cookies"), text(&kept.join("; ")), cx)?;
            Ok(JsValue::undefined())
        }),
    ),
    ("readyState", Some(|_, _, _| Ok(text("complete"))), None),
    ("visibilityState", Some(|_, _, _| Ok(text("visible"))), None),
    ("hidden", Some(|_, _, _| Ok(false.into())), None),
    ("compatMode", Some(|_, _, _| Ok(text("CSS1Compat"))), None),
    ("characterSet", Some(|_, _, _| Ok(text("UTF-8"))), None),
    ("charset", Some(|_, _, _| Ok(text("UTF-8"))), None),
    ("inputEncoding", Some(|_, _, _| Ok(text("UTF-8"))), None),
    ("contentType", Some(|_, _, _| Ok(text("text/html"))), None),
    (
        "designMode",
        Some(|_, _, _| Ok(text("off"))),
        Some(|_, _, _| Ok(JsValue::undefined())),
    ),
    (
        "dir",
        Some(|_, _, _| Ok(text(""))),
        Some(|_, _, _| Ok(JsValue::undefined())),
    ),
    ("currentScript", Some(|_, _, _| Ok(JsValue::null())), None),
    ("fullscreenEnabled", Some(|_, _, _| Ok(true.into())), None),
    (
        "pictureInPictureEnabled",
        Some(|_, _, _| Ok(false.into())),
        None,
    ),
    (
        "fullscreenElement",
        Some(|_, _, cx| {
            let id = shared(cx).borrow().fullscreen;
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    (
        "pointerLockElement",
        Some(|_, _, _| Ok(JsValue::null())),
        None,
    ),
    (
        "scrollingElement",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let id = read_dom(cx, |dom| root_element(dom, doc));
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    (
        "doctype",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let id = read_dom(cx, |dom| {
                dom.children(doc)
                    .iter()
                    .copied()
                    .find(|c| matches!(dom.get(*c).map(|n| &n.kind), Some(NodeKind::Doctype(_))))
            });
            Ok(wrap_opt(cx, id))
        }),
        None,
    ),
    (
        "implementation",
        Some(|_, _, cx| Ok(native_instance(cx, "DOMImplementation", Kind::Plain, 0).into())),
        None,
    ),
    (
        "forms",
        Some(|this, _, cx| tagged_collection(this, cx, &["form"])),
        None,
    ),
    (
        "images",
        Some(|this, _, cx| tagged_collection(this, cx, &["img"])),
        None,
    ),
    (
        "scripts",
        Some(|this, _, cx| tagged_collection(this, cx, &["script"])),
        None,
    ),
    (
        "links",
        Some(|this, _, cx| {
            let doc = document_of(this, cx);
            let found = read_dom(cx, |dom| {
                dom.descendants(doc)
                    .into_iter()
                    .filter(|n| {
                        matches!(dom.get(*n).and_then(|x| x.tag()), Some("a" | "area"))
                            && dom.get_attr(*n, "href").is_some()
                    })
                    .collect()
            });
            Ok(collection(found, cx))
        }),
        None,
    ),
    (
        "styleSheets",
        Some(|_, _, cx| super::platform::style_sheets(cx)),
        None,
    ),
    (
        "adoptedStyleSheets",
        Some(|_, _, cx| super::platform::adopted_sheets(cx)),
        Some(|_, args, cx| super::platform::set_adopted_sheets(&arg(args, 0), cx)),
    ),
    (
        "fonts",
        Some(|_, _, cx| super::platform::font_faces(cx)),
        None,
    ),
    (
        "timeline",
        Some(|_, _, cx| {
            let timeline = super::plain_object(cx);
            let now = verve::now(cx);
            super::set(&timeline, "currentTime", now, cx)?;
            Ok(timeline.into())
        }),
        None,
    ),
];

fn tagged_collection(this: &JsValue, context: &mut Context, tags: &[&str]) -> JsResult<JsValue> {
    let doc = document_of(this, context);
    let found = read_dom(context, |dom| {
        dom.descendants(doc)
            .into_iter()
            .filter(|n| {
                dom.get(*n)
                    .and_then(|x| x.tag())
                    .is_some_and(|t| tags.contains(&t))
            })
            .collect()
    });
    Ok(collection(found, context))
}

// ─── DOMParser ──────────────────────────────────────────────────────────────────────────────

const DOM_PARSER_METHODS: &[Method] = &[
    // A document of its own, holding what `text` says: an HTML page, or an XML (an SVG's) root.
    ("parseFromString", 2, |_, args, cx| {
        let markup = arg_string(args, 0, cx)?;
        let kind = arg_string(args, 1, cx).unwrap_or_default();
        let doc = with_dom(cx, |dom| {
            if kind == "text/html" {
                let (doc, ..) = dom.create_html_document();
                crate::html::parse_document(dom, doc, &markup);
                doc
            } else {
                let doc = dom.create_document();
                let stripped = markup.trim_start();
                let stripped = if stripped.starts_with("<?xml") {
                    stripped
                        .find("?>")
                        .map_or(stripped, |end| &stripped[end + 2..])
                } else {
                    stripped
                };
                crate::html::parse_into(dom, doc, stripped);
                doc
            }
        });
        Ok(wrap(cx, doc))
    }),
];

// ─── XMLSerializer, DOMImplementation ───────────────────────────────────────────────────────

const SERIALIZER_METHODS: &[Method] = &[("serializeToString", 1, |_, args, cx| {
    let id = node_of(&arg(args, 0))?;
    Ok(text(&read_dom(cx, |dom| {
        match dom.get(id).map(|n| &n.kind) {
            Some(NodeKind::Document | NodeKind::Fragment) => crate::html::inner(dom, id),
            Some(_) => crate::html::outer(dom, id),
            None => String::new(),
        }
    })))
})];

/// A new HTML document: `<html>` with a `<head>` (and its `<title>`) and a `<body>`.
pub fn html_document(context: &mut Context, title: Option<&str>) -> JsValue {
    let doc = with_dom(context, |dom| {
        let (doc, _, head, _) = dom.create_html_document();
        let doctype = dom.create_doctype("html");
        let first = dom.children(doc).first().copied();
        dom.insert_before(doc, doctype, first);
        if let Some(title) = title {
            let element = dom.create_element("title");
            dom.set_text(element, title);
            dom.append(head, element);
        }
        doc
    });
    wrap(context, doc)
}

const IMPLEMENTATION_METHODS: &[Method] = &[
    ("createHTMLDocument", 1, |_, args, cx| {
        let title = opt_string(args, 0, cx)?;
        Ok(html_document(cx, title.as_deref()))
    }),
    ("createDocument", 3, |_, args, cx| {
        let namespace = opt_string(args, 0, cx)?;
        let name = opt_string(args, 1, cx)?.unwrap_or_default();
        let doc = with_dom(cx, |dom| {
            let doc = dom.create_document();
            if !name.is_empty() {
                let root = dom.create_element_ns(&name, Namespace::from_uri(namespace.as_deref()));
                dom.append(doc, root);
            }
            doc
        });
        Ok(wrap(cx, doc))
    }),
    ("createDocumentType", 3, |_, args, cx| {
        let name = arg_string(args, 0, cx)?;
        let id = with_dom(cx, |dom| dom.create_doctype(&name));
        Ok(wrap(cx, id))
    }),
    ("hasFeature", 0, |_, _, _| Ok(true.into())),
];

pub fn install(context: &mut Context) -> JsResult<()> {
    let (_, document) = class(
        context,
        ClassSpec {
            name: "Document",
            length: 0,
            // `new Document()`: an empty document of its own.
            construct: |new_target, _, cx| {
                let prototype = super::prototype_of_target(new_target, cx)?;
                let id = with_dom(cx, Dom::create_document);
                let object = verve::dom_events::host_target(prototype, super::node::host_id_of(id));
                super::node::adopt(cx, id, &object);
                Ok(object.into())
            },
            parent: Some("Node"),
            methods: DOCUMENT_METHODS,
            accessors: DOCUMENT_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    extend_prototype(
        context,
        &document,
        PARENT_NODE_METHODS,
        PARENT_NODE_ACCESSORS,
    )?;
    for name in ["HTMLDocument", "XMLDocument"] {
        class(
            context,
            ClassSpec {
                name,
                length: 0,
                construct: illegal_constructor,
                parent: Some("Document"),
                methods: &[],
                accessors: &[],
                statics: &[],
                global: true,
            },
        )?;
    }
    class(
        context,
        ClassSpec {
            name: "DOMParser",
            length: 0,
            construct: |new_target, _, cx| Ok(instance(new_target, Kind::Plain, 0, cx)?.into()),
            parent: None,
            methods: DOM_PARSER_METHODS,
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    class(
        context,
        ClassSpec {
            name: "XMLSerializer",
            length: 0,
            construct: |new_target, _, cx| Ok(instance(new_target, Kind::Plain, 0, cx)?.into()),
            parent: None,
            methods: SERIALIZER_METHODS,
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    class(
        context,
        ClassSpec {
            name: "DOMImplementation",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: IMPLEMENTATION_METHODS,
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    let _ = constructor_of;
    Ok(())
}
