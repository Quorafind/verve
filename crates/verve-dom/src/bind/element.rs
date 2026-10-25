//! `Element`, `HTMLElement` and the classes under them — the fields, links, tables, templates,
//! media, dialogs — and `SVGElement`'s; with what an element hands out: its `classList`, its
//! `style`, its `dataset`, its `attributes`.
//!
//! Where an element is and how big, it answers from what the drawer last painted (see
//! [`crate::Document::set_layout`]): `getBoundingClientRect`, `offsetWidth`, `clientHeight`…

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    object::builtins::JsProxyBuilder,
    property::{PropertyDescriptor, PropertyKey},
};

use super::node::{
    CHILD_NODE_ACCESSORS, CHILD_NODE_METHODS, PARENT_NODE_ACCESSORS, PARENT_NODE_METHODS,
    collection, read_dom, with_dom, wrap, wrap_opt,
};
use super::{
    Accessor, ClassSpec, Kind, Method, arg, arg_string, array, call_method, class, closure,
    dom_error, extend_prototype, get, handle_of, illegal_constructor, native_instance, node_id,
    node_of, opt_string, plain_object, set, string, text, type_error,
};
use crate::Rect;
use crate::dom::{Dom, Namespace, NodeId, NodeKind, camel_name, css_name};
use crate::state::shared;

/// The class of an HTML element, by its tag.
pub fn html_class(tag: &str) -> &'static str {
    match tag {
        "input" => "HTMLInputElement",
        "textarea" => "HTMLTextAreaElement",
        "select" => "HTMLSelectElement",
        "button" => "HTMLButtonElement",
        "option" => "HTMLOptionElement",
        "optgroup" => "HTMLOptGroupElement",
        "a" => "HTMLAnchorElement",
        "area" => "HTMLAreaElement",
        "template" => "HTMLTemplateElement",
        "div" => "HTMLDivElement",
        "span" => "HTMLSpanElement",
        "img" => "HTMLImageElement",
        "video" => "HTMLVideoElement",
        "audio" => "HTMLAudioElement",
        "source" => "HTMLSourceElement",
        "canvas" => "HTMLCanvasElement",
        "iframe" => "HTMLIFrameElement",
        "form" => "HTMLFormElement",
        "label" => "HTMLLabelElement",
        "fieldset" => "HTMLFieldSetElement",
        "legend" => "HTMLLegendElement",
        "p" => "HTMLParagraphElement",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
        "ul" => "HTMLUListElement",
        "ol" => "HTMLOListElement",
        "li" => "HTMLLIElement",
        "dl" => "HTMLDListElement",
        "table" => "HTMLTableElement",
        "tr" => "HTMLTableRowElement",
        "thead" | "tbody" | "tfoot" => "HTMLTableSectionElement",
        "caption" => "HTMLTableCaptionElement",
        "td" | "th" => "HTMLTableCellElement",
        "col" | "colgroup" => "HTMLTableColElement",
        "pre" | "listing" | "xmp" => "HTMLPreElement",
        "br" => "HTMLBRElement",
        "hr" => "HTMLHRElement",
        "style" => "HTMLStyleElement",
        "script" => "HTMLScriptElement",
        "link" => "HTMLLinkElement",
        "meta" => "HTMLMetaElement",
        "title" => "HTMLTitleElement",
        "base" => "HTMLBaseElement",
        "details" => "HTMLDetailsElement",
        "dialog" => "HTMLDialogElement",
        "progress" => "HTMLProgressElement",
        "meter" => "HTMLMeterElement",
        "output" => "HTMLOutputElement",
        "body" => "HTMLBodyElement",
        "head" => "HTMLHeadElement",
        "html" => "HTMLHtmlElement",
        "slot" => "HTMLSlotElement",
        "picture" => "HTMLPictureElement",
        "time" => "HTMLTimeElement",
        "data" => "HTMLDataElement",
        "blockquote" | "q" => "HTMLQuoteElement",
        "ins" | "del" => "HTMLModElement",
        "embed" => "HTMLEmbedElement",
        "object" => "HTMLObjectElement",
        "map" => "HTMLMapElement",
        "track" => "HTMLTrackElement",
        "menu" => "HTMLMenuElement",
        "datalist" => "HTMLDataListElement",
        "applet" | "bgsound" | "blink" | "isindex" | "keygen" | "multicol" | "nextid"
        | "spacer" => "HTMLUnknownElement",
        _ => "HTMLElement",
    }
}

/// The classes under `HTMLElement` with nothing of their own beyond what is reflected.
const PLAIN_HTML_CLASSES: &[&str] = &[
    "HTMLDivElement", "HTMLSpanElement", "HTMLParagraphElement", "HTMLHeadingElement",
    "HTMLUListElement", "HTMLOListElement", "HTMLLIElement", "HTMLDListElement",
    "HTMLTableCaptionElement", "HTMLTableColElement", "HTMLPreElement", "HTMLBRElement",
    "HTMLHRElement", "HTMLScriptElement", "HTMLLinkElement", "HTMLMetaElement", "HTMLBaseElement",
    "HTMLBodyElement", "HTMLHeadElement", "HTMLHtmlElement", "HTMLPictureElement",
    "HTMLTimeElement", "HTMLDataElement", "HTMLQuoteElement", "HTMLModElement",
    "HTMLEmbedElement", "HTMLObjectElement", "HTMLMapElement", "HTMLTrackElement",
    "HTMLMenuElement", "HTMLDataListElement", "HTMLUnknownElement", "HTMLSourceElement",
    "HTMLOptGroupElement", "HTMLFieldSetElement", "HTMLLegendElement", "HTMLOutputElement",
    "HTMLMeterElement", "HTMLAreaElement", "HTMLIFrameElement", "HTMLSlotElement",
    "HTMLTitleElement",
];

/// SVG's element classes, and the tag each stands for.
pub const SVG_CLASSES: &[(&str, &str)] = &[
    ("svg", "SVGSVGElement"),
    ("g", "SVGGElement"),
    ("path", "SVGPathElement"),
    ("circle", "SVGCircleElement"),
    ("ellipse", "SVGEllipseElement"),
    ("rect", "SVGRectElement"),
    ("line", "SVGLineElement"),
    ("polyline", "SVGPolylineElement"),
    ("polygon", "SVGPolygonElement"),
    ("text", "SVGTextElement"),
    ("tspan", "SVGTSpanElement"),
    ("use", "SVGUseElement"),
    ("defs", "SVGDefsElement"),
    ("symbol", "SVGSymbolElement"),
    ("image", "SVGImageElement"),
    ("foreignObject", "SVGForeignObjectElement"),
    ("mask", "SVGMaskElement"),
    ("clipPath", "SVGClipPathElement"),
    ("linearGradient", "SVGLinearGradientElement"),
    ("radialGradient", "SVGRadialGradientElement"),
    ("stop", "SVGStopElement"),
    ("pattern", "SVGPatternElement"),
    ("marker", "SVGMarkerElement"),
    ("filter", "SVGFilterElement"),
    ("title", "SVGTitleElement"),
    ("desc", "SVGDescElement"),
    ("a", "SVGAElement"),
    ("textPath", "SVGTextPathElement"),
    ("style", "SVGStyleElement"),
    ("script", "SVGScriptElement"),
    ("metadata", "SVGMetadataElement"),
    ("animate", "SVGAnimateElement"),
    ("animateMotion", "SVGAnimateMotionElement"),
    ("animateTransform", "SVGAnimateTransformElement"),
    ("set", "SVGSetElement"),
    ("feBlend", "SVGFEBlendElement"),
    ("feColorMatrix", "SVGFEColorMatrixElement"),
    ("feComposite", "SVGFECompositeElement"),
    ("feFlood", "SVGFEFloodElement"),
    ("feGaussianBlur", "SVGFEGaussianBlurElement"),
    ("feMerge", "SVGFEMergeElement"),
    ("feMergeNode", "SVGFEMergeNodeElement"),
    ("feOffset", "SVGFEOffsetElement"),
    ("feDropShadow", "SVGFEDropShadowElement"),
    ("feTurbulence", "SVGFETurbulenceElement"),
];

/// The class of an SVG element by its tag.
pub fn svg_class(tag: &str) -> Option<&'static str> {
    SVG_CLASSES
        .iter()
        .find(|(t, _)| t.eq_ignore_ascii_case(tag))
        .map(|(_, c)| *c)
}

// ─── Where it was painted ───────────────────────────────────────────────────────────────────

/// Where `id` was last painted: its own box when the drawer measured it, else the box round
/// what of it was measured, else (for the root and the body) the viewport; nothing otherwise.
pub fn painted(context: &Context, id: NodeId) -> Rect {
    let state = shared(context);
    let state = state.borrow();
    if let Some(rect) = state.rects.get(&id) {
        return *rect;
    }
    if !state.dom.is_connected(id) {
        return Rect::default();
    }
    let mut out: Option<Rect> = None;
    for (node, rect) in &state.rects {
        if state.dom.contains(id, *node) {
            out = Some(match out {
                None => *rect,
                Some(o) => o.union(rect),
            });
        }
    }
    if let Some(out) = out {
        return out;
    }
    if id == state.dom.body || id == state.dom.html {
        return Rect {
            x: 0.,
            y: 0.,
            width: state.env.width,
            height: state.env.height,
        };
    }
    Rect::default()
}

/// `getBoundingClientRect`'s answer: a `DOMRect`, relative to the viewport.
pub fn rect_object(context: &mut Context, rect: Rect) -> JsResult<JsValue> {
    super::platform::make_rect(
        context,
        [
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.width),
            f64::from(rect.height),
        ],
    )
}

fn painted_part(this: &JsValue, context: &mut Context, part: usize) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let rect = painted(context, id);
    let value = [rect.x, rect.y, rect.width, rect.height][part];
    Ok(f64::from(value.round()).into())
}

fn scrolled(this: &JsValue, context: &mut Context, axis: usize) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let offset = shared(context)
        .borrow()
        .scroll_offsets
        .get(&id)
        .map_or(0., |o| o[axis]);
    Ok(f64::from(offset).into())
}

/// A scroll asked for from code: kept for the drawer, which does it and says where things are
/// scrolled to (see [`crate::Document::set_scroll_offsets`]). Setting a box to where it is
/// already scrolled asks for nothing.
pub fn request_scroll(context: &mut Context, request: crate::ScrollRequest) {
    let state = shared(context);
    let mut state = state.borrow_mut();
    if let crate::ScrollRequest::To { node, left, top } = &request {
        let at = state.scroll_offsets.get(node).copied().unwrap_or([0., 0.]);
        if left.is_none_or(|l| l == at[0]) && top.is_none_or(|t| t == at[1]) {
            return;
        }
        state.scroll_offsets.insert(*node, [left.unwrap_or(at[0]), top.unwrap_or(at[1])]);
    }
    state.requests.push(crate::Request::Scroll(request));
    state.touch();
}

/// `scrollTo(x, y)`, `scrollTo({ left, top })`, and `scrollBy` the same way, for an element or
/// (`node` the root) the window.
pub fn scroll_to(node: NodeId, args: &[JsValue], context: &mut Context, by: bool) -> JsResult<JsValue> {
    let first = arg(args, 0);
    let number = |value: JsValue, context: &mut Context| -> JsResult<Option<f32>> {
        if value.is_undefined() {
            return Ok(None);
        }
        let n = value.to_number(context)?;
        Ok(n.is_finite().then_some(n as f32))
    };
    let (left, top) = if first.is_object() {
        (
            number(get(&first, "left", context)?, context)?,
            number(get(&first, "top", context)?, context)?,
        )
    } else {
        (number(first, context)?, number(arg(args, 1), context)?)
    };
    let request = if by {
        crate::ScrollRequest::By {
            node,
            left: left.unwrap_or(0.),
            top: top.unwrap_or(0.),
        }
    } else {
        crate::ScrollRequest::To { node, left, top }
    };
    request_scroll(context, request);
    Ok(JsValue::undefined())
}

// ─── Reflected attributes ───────────────────────────────────────────────────────────────────

/// How a property reflects its attribute.
#[derive(Clone, Copy)]
pub enum Reflect {
    /// The attribute's text, `""` when absent.
    Text,
    /// Whether the attribute is there.
    Flag,
    /// A whole number, with a default when absent or not a number.
    Number(i32),
    /// An address, resolved against the document's.
    Url,
    /// One of a set of keywords, lower-cased, with a default.
    Keyword(&'static str),
}

#[derive(Clone)]
struct Reflected(JsString, u8, i32, JsString);

impl boa_gc::Finalize for Reflected {}
// SAFETY: strings and numbers hold no garbage-collected value that needs tracing.
unsafe impl boa_gc::Trace for Reflected {
    boa_gc::empty_trace!();
}

/// Puts a property on `prototype` for each `(property, attribute, how)`.
pub fn reflect(
    context: &mut Context,
    prototype: &JsObject,
    list: &[(&str, &str, Reflect)],
) -> JsResult<()> {
    for (property, attribute, how) in list {
        let (code, number, keyword) = match how {
            Reflect::Text => (0, 0, ""),
            Reflect::Flag => (1, 0, ""),
            Reflect::Number(default) => (2, *default, ""),
            Reflect::Url => (3, 0, ""),
            Reflect::Keyword(default) => (4, 0, *default),
        };
        let captures = Reflected(
            JsString::from(*attribute),
            code,
            number,
            JsString::from(keyword),
        );
        let getter = closure(
            context,
            &format!("get {property}"),
            |this, _, r: &Reflected, cx| {
                let id = node_of(this)?;
                let name = r.0.to_std_string_escaped();
                let value = read_dom(cx, |dom| dom.get_attr(id, &name));
                Ok(match r.1 {
                    0 => text(&value.unwrap_or_default()),
                    1 => value.is_some().into(),
                    2 => value
                        .and_then(|v| v.trim().parse::<i32>().ok())
                        .unwrap_or(r.2)
                        .into(),
                    3 => match value {
                        Some(v) => {
                            let base = shared(cx).borrow().url.clone();
                            text(&base.join(&v).map_or(v, |u| u.to_string()))
                        }
                        None => text(""),
                    },
                    _ => text(
                        &value
                            .map(|v| v.to_ascii_lowercase())
                            .filter(|v| !v.is_empty())
                            .unwrap_or_else(|| r.3.to_std_string_escaped()),
                    ),
                })
            },
            captures.clone(),
        );
        let setter = closure(
            context,
            &format!("set {property}"),
            |this, args, r: &Reflected, cx| {
                let id = node_of(this)?;
                let name = r.0.to_std_string_escaped();
                let value = arg(args, 0);
                if r.1 == 1 {
                    let on = value.to_boolean();
                    with_dom(cx, |dom| {
                        if on {
                            dom.set_attr(id, &name, "")
                        } else {
                            dom.remove_attr(id, &name)
                        }
                    });
                } else {
                    let value = if r.1 == 2 {
                        (value.to_number(cx)? as i64).to_string()
                    } else {
                        string(&value, cx)?
                    };
                    with_dom(cx, |dom| dom.set_attr(id, &name, &value));
                }
                Ok(JsValue::undefined())
            },
            captures,
        );
        prototype.define_property_or_throw(
            JsString::from(*property),
            PropertyDescriptor::builder()
                .get(getter)
                .set(setter)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    }
    Ok(())
}

// ─── Element ────────────────────────────────────────────────────────────────────────────────

fn tag_of(context: &Context, id: NodeId) -> Option<String> {
    read_dom(context, |dom| dom.get(id).and_then(|n| n.tag()).map(str::to_owned))
}

fn class_args(args: &[JsValue], context: &mut Context) -> JsResult<Vec<String>> {
    let mut out = Vec::new();
    for value in args {
        let class = string(value, context)?;
        if class.is_empty() {
            return Err(dom_error(
                context,
                "Failed to execute 'add' on 'DOMTokenList': The token provided must not be empty.",
                "SyntaxError",
            ));
        }
        if class.contains(char::is_whitespace) {
            return Err(dom_error(
                context,
                &format!("Failed to execute 'add' on 'DOMTokenList': The token provided ('{class}') contains HTML space characters, which are not valid in tokens."),
                "InvalidCharacterError",
            ));
        }
        out.push(class);
    }
    Ok(out)
}

fn list_node(this: &JsValue) -> JsResult<NodeId> {
    handle_of(this, Kind::ClassList)
        .ok_or_else(|| type_error("Illegal invocation: not a DOMTokenList"))
}

const CLASS_LIST_METHODS: &[Method] = &[
    ("add", 1, |this, args, cx| {
        let id = list_node(this)?;
        let classes = class_args(args, cx)?;
        with_dom(cx, |dom| {
            dom.change_classes(id, |list| {
                for class in classes {
                    if !list.contains(&class) {
                        list.push(class);
                    }
                }
            })
        });
        Ok(JsValue::undefined())
    }),
    ("remove", 1, |this, args, cx| {
        let id = list_node(this)?;
        let classes = class_args(args, cx)?;
        with_dom(cx, |dom| {
            if dom.get(id).is_some_and(|n| classes.iter().any(|c| n.has_class(c))) {
                dom.change_classes(id, |list| list.retain(|c| !classes.contains(c)));
            }
        });
        Ok(JsValue::undefined())
    }),
    ("contains", 1, |this, args, cx| {
        let id = list_node(this)?;
        let class = arg_string(args, 0, cx)?;
        Ok(read_dom(cx, |dom| dom.get(id).is_some_and(|n| n.has_class(&class))).into())
    }),
    ("toggle", 2, |this, args, cx| {
        let id = list_node(this)?;
        let class = class_args(&args[..args.len().min(1)], cx)?.pop().unwrap_or_default();
        let force = arg(args, 1);
        let on = with_dom(cx, |dom| {
            let has = dom.get(id).is_some_and(|n| n.has_class(&class));
            let on = if force.is_undefined() {
                !has
            } else {
                force.to_boolean()
            };
            if on {
                dom.add_class(id, &class)
            } else {
                dom.remove_class(id, &class)
            }
            on
        });
        Ok(on.into())
    }),
    ("replace", 2, |this, args, cx| {
        let id = list_node(this)?;
        let old = arg_string(args, 0, cx)?;
        let fresh = arg_string(args, 1, cx)?;
        let replaced = with_dom(cx, |dom| {
            if !dom.get(id).is_some_and(|n| n.has_class(&old)) {
                return false;
            }
            dom.change_classes(id, |list| {
                if let Some(at) = list.iter().position(|c| *c == old) {
                    list[at] = fresh.clone();
                }
                let mut seen = std::collections::HashSet::new();
                list.retain(|c| seen.insert(c.clone()));
            });
            true
        });
        Ok(replaced.into())
    }),
    ("item", 1, |this, args, cx| {
        let id = list_node(this)?;
        let index = arg(args, 0).to_number(cx)? as usize;
        Ok(read_dom(cx, |dom| dom.get(id).and_then(|n| n.classes.get(index).cloned()))
            .map_or(JsValue::null(), |c| text(&c)))
    }),
    ("supports", 1, |_, _, _| Ok(true.into())),
    ("forEach", 1, |this, args, cx| {
        let id = list_node(this)?;
        let classes = read_dom(cx, |dom| dom.get(id).map(|n| n.classes.clone()).unwrap_or_default());
        for (index, class) in classes.iter().enumerate() {
            verve::call(cx, &arg(args, 0), &arg(args, 1), &[text(class), index.into(), this.clone()])?;
        }
        Ok(JsValue::undefined())
    }),
    ("toString", 0, |this, _, cx| {
        let id = list_node(this)?;
        Ok(text(&read_dom(cx, |dom| dom.get(id).map(|n| n.classes.join(" ")).unwrap_or_default())))
    }),
];

const CLASS_LIST_ACCESSORS: &[Accessor] = &[
    (
        "length",
        Some(|this, _, cx| {
            let id = list_node(this)?;
            Ok(read_dom(cx, |dom| dom.get(id).map_or(0, |n| n.classes.len())).into())
        }),
        None,
    ),
    (
        "value",
        Some(|this, _, cx| {
            let id = list_node(this)?;
            Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "class").unwrap_or_default())))
        }),
        Some(|this, args, cx| {
            let id = list_node(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "class", &value));
            Ok(JsValue::undefined())
        }),
    ),
];

/// An element's `classList`: indexable (`el.classList[0]`) and iterable, as a browser's is.
fn class_list(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let target = native_instance(context, "DOMTokenList", Kind::ClassList, id);
    let proxy = JsProxyBuilder::new(target)
        .get(|_, args, cx| {
            let target = arg(args, 0);
            let key = arg(args, 1);
            if let Some(name) = key.as_string() {
                let name = name.to_std_string_escaped();
                if let Ok(index) = name.parse::<usize>() {
                    let id = handle_of(&target, Kind::ClassList).unwrap_or_default();
                    return Ok(read_dom(cx, |dom| dom.get(id).and_then(|n| n.classes.get(index).cloned()))
                        .map_or(JsValue::undefined(), |c| text(&c)));
                }
            }
            let value = match target.as_object() {
                Some(object) => object.get(key.to_property_key(cx)?, cx)?,
                None => JsValue::undefined(),
            };
            // A method called through the list: on the list itself.
            if value.is_callable() && key.as_string().is_some() {
                return call_method(&value, "bind", &[target], cx);
            }
            Ok(value)
        })
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}

const ELEMENT_ACCESSORS: &[Accessor] = &[
    (
        "tagName",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| crate::html::qualified_name(dom, id, true))))
        }),
        None,
    ),
    (
        "localName",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| crate::html::qualified_name(dom, id, false))))
        }),
        None,
    ),
    (
        "namespaceURI",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.get(id).and_then(|n| n.namespace.uri()))
                .map_or(JsValue::null(), text))
        }),
        None,
    ),
    ("prefix", Some(|_, _, _| Ok(JsValue::null())), None),
    (
        "id",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "id").unwrap_or_default())))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "id", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "className",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "class").unwrap_or_default())))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "class", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "classList",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            class_list(cx, id)
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "class", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "slot",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "slot").unwrap_or_default())))
        }),
        None,
    ),
    (
        "innerHTML",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| crate::html::inner(dom, id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let markup = opt_string(args, 0, cx)?.unwrap_or_default();
            set_inner_html(cx, id, &markup);
            Ok(JsValue::undefined())
        }),
    ),
    (
        "outerHTML",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| crate::html::outer(dom, id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let markup = arg_string(args, 0, cx)?;
            let replaced = with_dom(cx, |dom| {
                let parent = dom.parent(id)?;
                let fragment = dom.create_fragment();
                crate::html::parse_into(dom, fragment, &markup);
                let next = dom.sibling(id, 1, false);
                dom.detach(id);
                dom.insert_before(parent, fragment, next);
                Some(())
            });
            if replaced.is_none() {
                return Err(dom_error(
                    cx,
                    "Failed to set the 'outerHTML' property on 'Element': This element has no parent node.",
                    "NoModificationAllowedError",
                ));
            }
            Ok(JsValue::undefined())
        }),
    ),
    (
        "scrollTop",
        Some(|this, _, cx| scrolled(this, cx, 1)),
        Some(|this, args, cx| {
            let node = node_of(this)?;
            let top = arg(args, 0).to_number(cx)? as f32;
            request_scroll(cx, crate::ScrollRequest::To { node, left: None, top: Some(top) });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "scrollLeft",
        Some(|this, _, cx| scrolled(this, cx, 0)),
        Some(|this, args, cx| {
            let node = node_of(this)?;
            let left = arg(args, 0).to_number(cx)? as f32;
            request_scroll(cx, crate::ScrollRequest::To { node, left: Some(left), top: None });
            Ok(JsValue::undefined())
        }),
    ),
    ("scrollHeight", Some(|this, _, cx| content_size(this, cx, 1)), None),
    ("scrollWidth", Some(|this, _, cx| content_size(this, cx, 0)), None),
    ("clientWidth", Some(|this, _, cx| painted_part(this, cx, 2)), None),
    ("clientHeight", Some(|this, _, cx| painted_part(this, cx, 3)), None),
    ("clientTop", Some(|_, _, _| Ok(0.into())), None),
    ("clientLeft", Some(|_, _, _| Ok(0.into())), None),
    (
        "attributes",
        Some(|this, _, cx| super::platform::attributes(this, cx)),
        None,
    ),
    (
        "shadowRoot",
        Some(|this, _, cx| {
            let host = node_of(this)?;
            let root = shared(cx).borrow().shadow_roots.get(&host).copied();
            Ok(match root {
                Some((root, true)) => wrap(cx, root),
                _ => JsValue::null(),
            })
        }),
        None,
    ),
    ("assignedSlot", Some(|_, _, _| Ok(JsValue::null())), None),
];

/// How big what is inside `this` is on `axis`: what the drawer painted of its content, at least
/// its own box.
fn content_size(this: &JsValue, context: &mut Context, axis: usize) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let own = painted(context, id);
    let state = shared(context);
    let state = state.borrow();
    let mut far = if axis == 0 { own.x + own.width } else { own.y + own.height };
    for (node, rect) in &state.rects {
        if *node != id && state.dom.contains(id, *node) {
            far = far.max(if axis == 0 { rect.x + rect.width } else { rect.y + rect.height });
        }
    }
    let size = if axis == 0 { far - own.x } else { far - own.y };
    Ok(f64::from(size.max(0.).round()).into())
}

/// `innerHTML = markup`: what was there taken out, `markup` read in. A `<template>`'s goes to
/// its content.
pub fn set_inner_html(context: &mut Context, id: NodeId, markup: &str) {
    let target = super::platform::template_target(context, id).unwrap_or(id);
    with_dom(context, |dom| {
        dom.empty(target);
        let fragment = dom.create_fragment();
        crate::html::parse_into(dom, fragment, markup);
        dom.append(target, fragment);
    });
}

/// `insertAdjacent…`'s four places: before `id`, as its first child, its last, or after it.
fn insert_adjacent(context: &mut Context, id: NodeId, place: &str, child: NodeId) -> JsResult<bool> {
    let place = place.to_ascii_lowercase();
    if !matches!(place.as_str(), "beforebegin" | "afterbegin" | "beforeend" | "afterend") {
        return Err(dom_error(
            context,
            &format!("Failed to execute 'insertAdjacent' on 'Element': The value provided ('{place}') is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."),
            "SyntaxError",
        ));
    }
    Ok(with_dom(context, |dom| match place.as_str() {
        "beforebegin" => match dom.parent(id) {
            Some(parent) => dom.insert_before(parent, child, Some(id)),
            None => false,
        },
        "afterbegin" => {
            let first = dom.children(id).first().copied();
            dom.insert_before(id, child, first)
        }
        "beforeend" => dom.append(id, child),
        _ => match dom.parent(id) {
            Some(parent) => {
                let next = dom.sibling(id, 1, false);
                dom.insert_before(parent, child, next)
            }
            None => false,
        },
    }))
}

fn selector_list(args: &[JsValue], context: &mut Context, method: &str) -> JsResult<crate::css::SelectorList> {
    let selector = arg_string(args, 0, context)?;
    crate::css::SelectorList::parse(&selector).map_err(|error| {
        dom_error(
            context,
            &format!("Failed to execute '{method}' on 'Element': {}", error.0),
            "SyntaxError",
        )
    })
}

const ELEMENT_METHODS: &[Method] = &[
    ("getAttribute", 1, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 0, cx)?;
        Ok(read_dom(cx, |dom| dom.get_attr(id, &name)).map_or(JsValue::null(), |v| text(&v)))
    }),
    ("setAttribute", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 0, cx)?;
        let value = arg_string(args, 1, cx)?;
        if name.is_empty() || name.contains(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"' || c == '\'' || c == '/' || c == '=') {
            return Err(dom_error(
                cx,
                &format!("Failed to execute 'setAttribute' on 'Element': '{name}' is not a valid attribute name."),
                "InvalidCharacterError",
            ));
        }
        with_dom(cx, |dom| dom.set_attr(id, &name, &value));
        Ok(JsValue::undefined())
    }),
    ("removeAttribute", 1, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 0, cx)?;
        with_dom(cx, |dom| dom.remove_attr(id, &name));
        Ok(JsValue::undefined())
    }),
    ("hasAttribute", 1, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 0, cx)?;
        Ok(read_dom(cx, |dom| dom.get_attr(id, &name).is_some()).into())
    }),
    ("toggleAttribute", 2, |this, args, cx| {
        let id = node_of(this)?;
        let name = arg_string(args, 0, cx)?;
        let force = arg(args, 1);
        let on = with_dom(cx, |dom| {
            let has = dom.get_attr(id, &name).is_some();
            let on = if force.is_undefined() { !has } else { force.to_boolean() };
            if on && !has {
                dom.set_attr(id, &name, "")
            } else if !on && has {
                dom.remove_attr(id, &name)
            }
            on
        });
        Ok(on.into())
    }),
    ("matches", 1, |this, args, cx| {
        let id = node_of(this)?;
        let selector = selector_list(args, cx, "matches")?;
        Ok(read_dom(cx, |dom| selector.matches(dom, id)).into())
    }),
    ("webkitMatchesSelector", 1, |this, args, cx| {
        let id = node_of(this)?;
        let selector = selector_list(args, cx, "webkitMatchesSelector")?;
        Ok(read_dom(cx, |dom| selector.matches(dom, id)).into())
    }),
    ("closest", 1, |this, args, cx| {
        let id = node_of(this)?;
        let selector = selector_list(args, cx, "closest")?;
        let found = read_dom(cx, |dom| {
            let mut at = Some(id);
            while let Some(node) = at {
                if dom.get(node).is_some_and(|n| n.is_element()) && selector.matches(dom, node) {
                    return Some(node);
                }
                at = dom.parent_element(node);
            }
            None
        });
        Ok(wrap_opt(cx, found))
    }),
    ("insertAdjacentElement", 2, |this, args, cx| {
        let id = node_of(this)?;
        let place = arg_string(args, 0, cx)?;
        let node = arg(args, 1);
        let child = super::node_arg(args, 1, "insertAdjacentElement", "Element")?;
        let placed = insert_adjacent(cx, id, &place, child)?;
        Ok(if placed { node } else { JsValue::null() })
    }),
    ("insertAdjacentText", 2, |this, args, cx| {
        let id = node_of(this)?;
        let place = arg_string(args, 0, cx)?;
        let content = arg_string(args, 1, cx)?;
        let child = with_dom(cx, |dom| dom.create_text(&content));
        insert_adjacent(cx, id, &place, child)?;
        Ok(JsValue::undefined())
    }),
    ("insertAdjacentHTML", 2, |this, args, cx| {
        let id = node_of(this)?;
        let place = arg_string(args, 0, cx)?;
        let markup = arg_string(args, 1, cx)?;
        let fragment = with_dom(cx, |dom| {
            let fragment = dom.create_fragment();
            crate::html::parse_into(dom, fragment, &markup);
            fragment
        });
        insert_adjacent(cx, id, &place, fragment)?;
        Ok(JsValue::undefined())
    }),
    ("getBoundingClientRect", 0, |this, _, cx| {
        let id = node_of(this)?;
        let rect = painted(cx, id);
        rect_object(cx, rect)
    }),
    ("getClientRects", 0, |this, _, cx| {
        let id = node_of(this)?;
        let rect = painted(cx, id);
        let items = if rect == Rect::default() { Vec::new() } else { vec![rect_object(cx, rect)?] };
        Ok(array(items, cx))
    }),
    ("scrollIntoView", 1, |this, _, cx| {
        let node = node_of(this)?;
        request_scroll(cx, crate::ScrollRequest::Reveal(node));
        Ok(JsValue::undefined())
    }),
    ("scrollTo", 2, |this, args, cx| scroll_to(node_of(this)?, args, cx, false)),
    ("scroll", 2, |this, args, cx| scroll_to(node_of(this)?, args, cx, false)),
    ("scrollBy", 2, |this, args, cx| scroll_to(node_of(this)?, args, cx, true)),
    // The element is what `document.fullscreenElement` answers, and hears `fullscreenchange`;
    // the window itself is the drawer's to make fullscreen, if it wants.
    ("requestFullscreen", 1, |this, _, cx| {
        let id = node_of(this)?;
        {
            let state = shared(cx);
            let mut state = state.borrow_mut();
            state.fullscreen = Some(id);
            state.requests.push(crate::Request::Fullscreen(Some(id)));
        }
        super::events::fire(cx, &this.clone(), "Event", "fullscreenchange", &[("bubbles", true.into())])?;
        super::resolved(JsValue::undefined(), cx)
    }),
    ("attachShadow", 1, |this, args, cx| super::platform::attach_shadow(this, args, cx)),
    ("setPointerCapture", 1, |_, _, _| Ok(JsValue::undefined())),
    ("releasePointerCapture", 1, |_, _, _| Ok(JsValue::undefined())),
    ("hasPointerCapture", 1, |_, _, _| Ok(false.into())),
    ("getAnimations", 0, |_, _, cx| Ok(array(Vec::new(), cx))),
    // An animation that has already finished: what a page with animations off sees.
    ("animate", 2, |_, _, cx| super::platform::finished_animation(cx)),
    ("checkVisibility", 0, |this, _, cx| {
        let id = node_of(this)?;
        Ok(read_dom(cx, |dom| dom.is_shown(id)).into())
    }),
    ("computedStyleMap", 0, |_, _, cx| Ok(plain_object(cx).into())),
];

// ─── HTMLElement ────────────────────────────────────────────────────────────────────────────

fn style_node(this: &JsValue) -> JsResult<NodeId> {
    handle_of(this, Kind::Style)
        .ok_or_else(|| type_error("Illegal invocation: not a CSSStyleDeclaration"))
}

const STYLE_METHODS: &[Method] = &[
    ("setProperty", 3, |this, args, cx| {
        let id = style_node(this)?;
        let name = arg_string(args, 0, cx)?;
        let name = if name.starts_with("--") { name } else { name.to_ascii_lowercase() };
        let value = opt_string(args, 1, cx)?.unwrap_or_default();
        let important = opt_string(args, 2, cx)?.is_some_and(|p| p.eq_ignore_ascii_case("important"));
        with_dom(cx, |dom| dom.set_style_important(id, &name, value.trim(), important));
        Ok(JsValue::undefined())
    }),
    ("getPropertyValue", 1, |this, args, cx| {
        let id = style_node(this)?;
        let name = arg_string(args, 0, cx)?;
        Ok(text(&read_dom(cx, |dom| {
            dom.get(id).and_then(|n| n.style_value(&name)).unwrap_or("").to_owned()
        })))
    }),
    ("getPropertyPriority", 1, |this, args, cx| {
        let id = style_node(this)?;
        let name = arg_string(args, 0, cx)?;
        let important = read_dom(cx, |dom| dom.get(id).is_some_and(|n| n.important.contains(&name)));
        Ok(text(if important { "important" } else { "" }))
    }),
    ("removeProperty", 1, |this, args, cx| {
        let id = style_node(this)?;
        let name = arg_string(args, 0, cx)?;
        let old = read_dom(cx, |dom| dom.get(id).and_then(|n| n.style_value(&name)).unwrap_or("").to_owned());
        with_dom(cx, |dom| dom.set_style(id, &name, ""));
        Ok(text(&old))
    }),
    ("item", 1, |this, args, cx| {
        let id = style_node(this)?;
        let at = arg(args, 0).to_number(cx)? as usize;
        Ok(text(&read_dom(cx, |dom| {
            dom.get(id).and_then(|n| n.style.get(at).map(|(k, _)| k.clone())).unwrap_or_default()
        })))
    }),
];

/// An element's `style`: every property readable and writable by its camel-cased name
/// (`style.backgroundColor`), by its CSS name through the methods, and as `cssText`.
pub fn style_proxy(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let target = native_instance(context, "CSSStyleDeclaration", Kind::Style, id);
    let proxy = JsProxyBuilder::new(target)
        .get(|_, args, cx| {
            let target = arg(args, 0);
            let key = arg(args, 1);
            let Some(name) = key.as_string() else {
                return match target.as_object() {
                    Some(object) => object.get(key.to_property_key(cx)?, cx),
                    None => Ok(JsValue::undefined()),
                };
            };
            let name = name.to_std_string_escaped();
            let id = handle_of(&target, Kind::Style).unwrap_or_default();
            if STYLE_METHODS.iter().any(|(m, _, _)| *m == name) || name == "constructor" || name == "toString" {
                let method = get(&target, &name, cx)?;
                if name == "constructor" {
                    return Ok(method);
                }
                return call_method(&method, "bind", &[target], cx);
            }
            match name.as_str() {
                "cssText" => {
                    return Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "style").unwrap_or_default())));
                }
                "length" => {
                    return Ok(read_dom(cx, |dom| dom.get(id).map_or(0, |n| n.style.len())).into());
                }
                "parentRule" => return Ok(JsValue::null()),
                _ => {}
            }
            if let Ok(index) = name.parse::<usize>() {
                return Ok(read_dom(cx, |dom| dom.get(id).and_then(|n| n.style.get(index).map(|(k, _)| k.clone())))
                    .map_or(JsValue::undefined(), |k| text(&k)));
            }
            let property = if name.contains('-') { name } else { css_name(&name) };
            Ok(text(&read_dom(cx, |dom| {
                dom.get(id).and_then(|n| n.style_value(&property)).unwrap_or("").to_owned()
            })))
        })
        .set(|_, args, cx| {
            let target = arg(args, 0);
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(true.into());
            };
            let name = name.to_std_string_escaped();
            let id = handle_of(&target, Kind::Style).unwrap_or_default();
            let value = arg(args, 2);
            let value = if value.is_null_or_undefined() {
                String::new()
            } else {
                string(&value, cx)?
            };
            if name == "cssText" {
                with_dom(cx, |dom| dom.set_attr(id, "style", &value));
            } else {
                let property = if name.contains('-') { name } else { css_name(&name) };
                // A value with its own `!important` is not one `style.x` takes.
                if !value.to_ascii_lowercase().contains("!important") {
                    with_dom(cx, |dom| dom.set_style(id, &property, value.trim()));
                }
            }
            Ok(true.into())
        })
        .has(|_, args, _| {
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(false.into());
            };
            let name = name.to_std_string_escaped();
            Ok((!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).into())
        })
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}

/// An element's `dataset`: `data-*` attributes by their camel-cased names.
fn dataset_proxy(context: &mut Context, id: NodeId) -> JsResult<JsValue> {
    let target = native_instance(context, "DOMStringMap", Kind::Dataset, id);
    let proxy = JsProxyBuilder::new(target)
        .get(|_, args, cx| {
            let target = arg(args, 0);
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(JsValue::undefined());
            };
            let id = handle_of(&target, Kind::Dataset).unwrap_or_default();
            let attr = format!("data-{}", css_name(&name.to_std_string_escaped()));
            Ok(read_dom(cx, |dom| dom.get_attr(id, &attr)).map_or(JsValue::undefined(), |v| text(&v)))
        })
        .set(|_, args, cx| {
            let target = arg(args, 0);
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(true.into());
            };
            let id = handle_of(&target, Kind::Dataset).unwrap_or_default();
            let attr = format!("data-{}", css_name(&name.to_std_string_escaped()));
            let value = arg_string(args, 2, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, &attr, &value));
            Ok(true.into())
        })
        .has(|_, args, cx| {
            let target = arg(args, 0);
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(false.into());
            };
            let id = handle_of(&target, Kind::Dataset).unwrap_or_default();
            let attr = format!("data-{}", css_name(&name.to_std_string_escaped()));
            Ok(read_dom(cx, |dom| dom.get_attr(id, &attr).is_some()).into())
        })
        .delete_property(|_, args, cx| {
            let target = arg(args, 0);
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(true.into());
            };
            let id = handle_of(&target, Kind::Dataset).unwrap_or_default();
            let attr = format!("data-{}", css_name(&name.to_std_string_escaped()));
            with_dom(cx, |dom| dom.remove_attr(id, &attr));
            Ok(true.into())
        })
        .own_keys(|_, args, cx| {
            let target = arg(args, 0);
            let id = handle_of(&target, Kind::Dataset).unwrap_or_default();
            let names: Vec<JsValue> = read_dom(cx, |dom| {
                dom.attributes(id)
                    .into_iter()
                    .filter_map(|(k, _)| k.strip_prefix("data-").map(camel_name))
                    .map(|k| text(&k))
                    .collect()
            });
            Ok(array(names, cx))
        })
        .get_own_property_descriptor(|_, args, cx| {
            let target = arg(args, 0);
            let Some(name) = arg(args, 1).as_string() else {
                return Ok(JsValue::undefined());
            };
            let id = handle_of(&target, Kind::Dataset).unwrap_or_default();
            let attr = format!("data-{}", css_name(&name.to_std_string_escaped()));
            match read_dom(cx, |dom| dom.get_attr(id, &attr)) {
                Some(value) => {
                    let descriptor = plain_object(cx);
                    set(&descriptor, "value", text(&value), cx)?;
                    set(&descriptor, "writable", true, cx)?;
                    set(&descriptor, "enumerable", true, cx)?;
                    set(&descriptor, "configurable", true, cx)?;
                    Ok(descriptor.into())
                }
                None => Ok(JsValue::undefined()),
            }
        })
        .build(context)?;
    Ok(JsObject::from(proxy).into())
}

/// `style` and `dataset`, which SVG's elements share with HTML's.
const STYLED_ACCESSORS: &[Accessor] = &[
    (
        "style",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            style_proxy(cx, id)
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "style", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "dataset",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            dataset_proxy(cx, id)
        }),
        None,
    ),
];

/// What `focus()` and `blur()` do, on every HTML and SVG element.
const FOCUS_METHODS: &[Method] = &[
    ("focus", 1, |this, _, cx| {
        let id = node_of(this)?;
        if super::events::focusable(cx, id) {
            super::events::set_focus(cx, Some(id))?;
        }
        Ok(JsValue::undefined())
    }),
    ("blur", 0, |this, _, cx| {
        let id = node_of(this)?;
        if read_dom(cx, |dom| dom.focused == Some(id)) {
            super::events::set_focus(cx, None)?;
        }
        Ok(JsValue::undefined())
    }),
];

const HTML_ELEMENT_ACCESSORS: &[Accessor] = &[
    (
        "innerText",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| inner_text(dom, id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = opt_string(args, 0, cx)?.unwrap_or_default();
            set_inner_text(cx, id, &value);
            Ok(JsValue::undefined())
        }),
    ),
    (
        "outerText",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| inner_text(dom, id))))
        }),
        None,
    ),
    (
        "tabIndex",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| {
                let node = dom.get(id);
                node.and_then(|n| n.attr("tabindex"))
                    .and_then(|v| v.trim().parse::<i32>().ok())
                    .unwrap_or_else(|| {
                        let natural = node.and_then(|n| n.tag()).is_some_and(|t| {
                            matches!(t, "input" | "select" | "textarea" | "button" | "iframe" | "summary")
                                || (t == "a" && node.is_some_and(|n| n.attr("href").is_some()))
                        });
                        if natural { 0 } else { -1 }
                    })
            })
            .into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg(args, 0).to_number(cx)? as i64;
            with_dom(cx, |dom| dom.set_attr(id, "tabindex", &value.to_string()));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "isContentEditable",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| {
                let mut at = Some(id);
                while let Some(node) = at {
                    if let Some(value) = dom.get(node).and_then(|n| n.attr("contenteditable")) {
                        return value != "false";
                    }
                    at = dom.parent(node);
                }
                false
            })
            .into())
        }),
        None,
    ),
    (
        "contentEditable",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                match dom.get_attr(id, "contenteditable").as_deref() {
                    Some("" | "true") => "true".to_owned(),
                    Some("false") => "false".to_owned(),
                    Some("plaintext-only") => "plaintext-only".to_owned(),
                    _ => "inherit".to_owned(),
                }
            })))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?.to_ascii_lowercase();
            with_dom(cx, |dom| {
                if value == "inherit" {
                    dom.remove_attr(id, "contenteditable")
                } else {
                    dom.set_attr(id, "contenteditable", &value)
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    ("offsetWidth", Some(|this, _, cx| painted_part(this, cx, 2)), None),
    ("offsetHeight", Some(|this, _, cx| painted_part(this, cx, 3)), None),
    ("offsetTop", Some(|this, _, cx| painted_part(this, cx, 1)), None),
    ("offsetLeft", Some(|this, _, cx| painted_part(this, cx, 0)), None),
    (
        "offsetParent",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let parent = read_dom(cx, |dom| dom.is_connected(id).then_some(dom.body));
            Ok(wrap_opt(cx, parent))
        }),
        None,
    ),
    (
        "inert",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.get_attr(id, "inert").is_some()).into())
        }),
        None,
    ),
];

const HTML_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("title", "title", Reflect::Text),
    ("lang", "lang", Reflect::Text),
    ("dir", "dir", Reflect::Keyword("")),
    ("hidden", "hidden", Reflect::Flag),
    ("accessKey", "accesskey", Reflect::Text),
    ("autofocus", "autofocus", Reflect::Flag),
    ("nonce", "nonce", Reflect::Text),
    ("inputMode", "inputmode", Reflect::Text),
    ("enterKeyHint", "enterkeyhint", Reflect::Text),
    ("autocapitalize", "autocapitalize", Reflect::Text),
    ("translate", "translate", Reflect::Flag),
    ("popover", "popover", Reflect::Text),
];

const HTML_ELEMENT_METHODS: &[Method] = &[
    ("click", 0, |this, _, cx| {
        let id = node_of(this)?;
        super::events::click(cx, id, &[], false)?;
        Ok(JsValue::undefined())
    }),
    ("attachInternals", 0, |_, _, cx| Ok(plain_object(cx).into())),
    ("showPopover", 0, |_, _, _| Ok(JsValue::undefined())),
    ("hidePopover", 0, |_, _, _| Ok(JsValue::undefined())),
    ("togglePopover", 0, |_, _, _| Ok(false.into())),
];

/// `innerText`: the text, with what is not drawn left out.
fn inner_text(dom: &Dom, id: NodeId) -> String {
    let mut out = String::new();
    collect_inner_text(dom, id, &mut out);
    out
}

fn collect_inner_text(dom: &Dom, id: NodeId, out: &mut String) {
    let Some(node) = dom.get(id) else { return };
    match &node.kind {
        NodeKind::Text(text) => out.push_str(text),
        NodeKind::Element(tag) => {
            if matches!(tag.as_str(), "script" | "style" | "template" | "head" | "title")
                || node.attr("hidden").is_some()
                || node.style_value("display") == Some("none")
            {
                return;
            }
            if tag == "br" {
                out.push('\n');
                return;
            }
            let block = matches!(tag.as_str(), "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "section" | "article" | "header" | "footer" | "ul" | "ol" | "pre" | "blockquote" | "table");
            if block && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for child in &node.children {
                collect_inner_text(dom, *child, out);
            }
            if block && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        _ => {
            for child in &node.children {
                collect_inner_text(dom, *child, out);
            }
        }
    }
}

fn set_inner_text(context: &mut Context, id: NodeId, value: &str) {
    with_dom(context, |dom| {
        dom.empty(id);
        for (i, line) in value.split('\n').enumerate() {
            if i > 0 {
                let br = dom.create_element("br");
                dom.append(id, br);
            }
            if !line.is_empty() {
                let run = dom.create_text(line);
                dom.append(id, run);
            }
        }
    });
}

// ─── Fields ─────────────────────────────────────────────────────────────────────────────────

fn selection_part(this: &JsValue, context: &mut Context, end: bool) -> JsResult<JsValue> {
    let id = node_of(this)?;
    Ok(read_dom(context, |dom| {
        dom.get(id).and_then(|n| n.draft.as_ref()).map(|d| {
            let range = d.selection();
            d.offset_to_utf16(if end { range.end } else { range.start })
        })
    })
    .map_or(JsValue::null(), JsValue::from))
}

/// Makes the field's draft the text it shows, before an edit to it.
pub fn ensure_draft(dom: &mut Dom, id: NodeId) {
    let value = dom.value(id);
    if let Some(node) = dom.get_mut(id)
        && node.draft.is_some()
        && !node.dirty_value
    {
        let at = value.len();
        let mut draft = crate::field::Draft::new(value);
        draft.place(at);
        node.draft = Some(draft);
        node.dirty_value = true;
    }
}

fn set_selection(context: &mut Context, id: NodeId, start: Option<f64>, end: Option<f64>) {
    with_dom(context, |dom| {
        ensure_draft(dom, id);
        if let Some(draft) = dom.get_mut(id).and_then(|n| n.draft.as_mut()) {
            let range = draft.selection();
            let start = start.map_or(range.start, |s| draft.offset_from_utf16(s.max(0.) as usize));
            let end = end.map_or(range.end, |e| draft.offset_from_utf16(e.max(0.) as usize)).max(start);
            draft.place(start);
            draft.extend_to(end);
        }
    });
}

const FIELD_ACCESSORS: &[Accessor] = &[
    (
        "value",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.value(id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = opt_string(args, 0, cx)?.unwrap_or_default();
            // A field's text never holds a line break unless it is a textarea's.
            let value = if tag_of(cx, id).as_deref() == Some("input") {
                value.replace(['\r', '\n'], "")
            } else {
                value
            };
            with_dom(cx, |dom| {
                if let Some(node) = dom.get_mut(id) {
                    node.dirty_value = true;
                }
                dom.set_value(id, &value)
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "defaultValue",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                if dom.get(id).and_then(|n| n.tag()) == Some("textarea") {
                    dom.text_content(id)
                } else {
                    dom.get_attr(id, "value").unwrap_or_default()
                }
            })))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| {
                if dom.get(id).and_then(|n| n.tag()) == Some("textarea") {
                    dom.set_text(id, &value)
                } else {
                    dom.set_attr(id, "value", &value)
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    ("selectionStart", Some(|this, _, cx| selection_part(this, cx, false)), Some(|this, args, cx| {
        let id = node_of(this)?;
        let start = arg(args, 0).to_number(cx)?;
        set_selection(cx, id, Some(start), None);
        Ok(JsValue::undefined())
    })),
    ("selectionEnd", Some(|this, _, cx| selection_part(this, cx, true)), Some(|this, args, cx| {
        let id = node_of(this)?;
        let end = arg(args, 0).to_number(cx)?;
        set_selection(cx, id, None, Some(end));
        Ok(JsValue::undefined())
    })),
    (
        "selectionDirection",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(read_dom(cx, |dom| {
                if dom.get(id).and_then(|n| n.draft.as_ref()).is_some_and(|d| d.is_reversed()) { "backward" } else { "forward" }
            })))
        }),
        Some(|_, _, _| Ok(JsValue::undefined())),
    ),
    (
        "textLength",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.value(id).encode_utf16().count()).into())
        }),
        None,
    ),
    ("form", Some(|this, _, cx| form_of(this, cx)), None),
    ("labels", Some(|this, _, cx| labels_of(this, cx)), None),
    ("validity", Some(|_, _, cx| validity(cx)), None),
    ("validationMessage", Some(|_, _, _| Ok(text(""))), None),
    ("willValidate", Some(|_, _, _| Ok(true.into())), None),
];

const FIELD_METHODS: &[Method] = &[
    ("select", 0, |this, _, cx| {
        let id = node_of(this)?;
        with_dom(cx, |dom| {
            ensure_draft(dom, id);
            if let Some(draft) = dom.get_mut(id).and_then(|n| n.draft.as_mut()) {
                draft.select_all();
            }
        });
        super::events::fire(cx, this, "Event", "select", &[("bubbles", true.into())])?;
        Ok(JsValue::undefined())
    }),
    ("setSelectionRange", 3, |this, args, cx| {
        let id = node_of(this)?;
        let start = arg(args, 0).to_number(cx)?;
        let end = arg(args, 1).to_number(cx)?;
        set_selection(cx, id, Some(start), Some(end));
        Ok(JsValue::undefined())
    }),
    ("setRangeText", 4, |this, args, cx| {
        let id = node_of(this)?;
        let replacement = arg_string(args, 0, cx)?;
        let (start, end) = (arg(args, 1), arg(args, 2));
        with_dom(cx, |dom| {
            ensure_draft(dom, id);
            if let Some(draft) = dom.get_mut(id).and_then(|n| n.draft.as_mut()) {
                if !start.is_undefined() {
                    let s = start.as_number().unwrap_or(0.) as usize;
                    let e = end.as_number().unwrap_or(s as f64) as usize;
                    let (s, e) = (draft.offset_from_utf16(s), draft.offset_from_utf16(e));
                    draft.place(s);
                    draft.extend_to(e.max(s));
                }
                draft.insert(&replacement);
            }
            dom.mark(id);
        });
        Ok(JsValue::undefined())
    }),
    ("checkValidity", 0, |_, _, _| Ok(true.into())),
    ("reportValidity", 0, |_, _, _| Ok(true.into())),
    ("setCustomValidity", 1, |_, _, _| Ok(JsValue::undefined())),
];

fn validity(context: &mut Context) -> JsResult<JsValue> {
    let object = plain_object(context);
    for key in [
        "badInput", "customError", "patternMismatch", "rangeOverflow", "rangeUnderflow",
        "stepMismatch", "tooLong", "tooShort", "typeMismatch", "valueMissing",
    ] {
        set(&object, key, false, context)?;
    }
    set(&object, "valid", true, context)?;
    Ok(object.into())
}

/// The form an element is in, or the one its `form` attribute names.
fn form_of(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let form = read_dom(context, |dom| {
        if let Some(named) = dom.get_attr(id, "form") {
            return dom
                .descendants(dom.document)
                .into_iter()
                .find(|n| dom.get_attr(*n, "id").as_deref() == Some(named.as_str()));
        }
        let mut at = dom.parent(id);
        while let Some(node) = at {
            if dom.get(node).and_then(|n| n.tag()) == Some("form") {
                return Some(node);
            }
            at = dom.parent(node);
        }
        None
    });
    Ok(wrap_opt(context, form))
}

fn labels_of(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let labels = read_dom(context, |dom| {
        let own = dom.get_attr(id, "id");
        let root = dom.root_of(id);
        dom.descendants(root)
            .into_iter()
            .filter(|l| {
                dom.get(*l).and_then(|n| n.tag()) == Some("label")
                    && (dom.get_attr(*l, "for").is_some_and(|f| Some(f) == own) || dom.contains(*l, id))
            })
            .collect()
    });
    Ok(super::node::node_list(labels, context))
}

const INPUT_ACCESSORS: &[Accessor] = &[
    (
        "checked",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.get(id).is_some_and(|n| n.checked)).into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let on = arg(args, 0).to_boolean();
            super::events::set_checked(cx, id, on);
            Ok(JsValue::undefined())
        }),
    ),
    (
        "defaultChecked",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.get_attr(id, "checked").is_some()).into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let on = arg(args, 0).to_boolean();
            with_dom(cx, |dom| if on { dom.set_attr(id, "checked", "") } else { dom.remove_attr(id, "checked") });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "indeterminate",
        Some(|this, _, cx| get(this, "__indeterminate", cx).map(|v| v.to_boolean().into())),
        Some(|this, args, cx| {
            if let Some(object) = this.as_object() {
                super::hidden(&object, js_string!("__indeterminate"), arg(args, 0).to_boolean(), cx)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
    (
        "type",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| input_type(dom, id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "type", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "valueAsNumber",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.value(id).trim().parse::<f64>().unwrap_or(f64::NAN)).into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let n = arg(args, 0).to_number(cx)?;
            with_dom(cx, |dom| dom.set_value(id, &if n.is_nan() { String::new() } else { n.to_string() }));
            Ok(JsValue::undefined())
        }),
    ),
    ("files", Some(|_, _, cx| Ok(array(Vec::new(), cx))), Some(|_, _, _| Ok(JsValue::undefined()))),
    ("list", Some(|this, _, cx| {
        let id = node_of(this)?;
        let list = read_dom(cx, |dom| {
            let named = dom.get_attr(id, "list")?;
            dom.descendants(dom.document).into_iter().find(|n| dom.get_attr(*n, "id").as_deref() == Some(named.as_str()))
        });
        Ok(wrap_opt(cx, list))
    }), None),
];

/// An `<input>`'s type, as the `type` property answers it: one of the known, lower-cased; `text`
/// for any other.
pub fn input_type(dom: &Dom, id: NodeId) -> String {
    let declared = dom.get_attr(id, "type").unwrap_or_default().to_ascii_lowercase();
    match declared.as_str() {
        "hidden" | "text" | "search" | "tel" | "url" | "email" | "password" | "date" | "month"
        | "week" | "time" | "datetime-local" | "number" | "range" | "color" | "checkbox"
        | "radio" | "file" | "submit" | "image" | "reset" | "button" => declared,
        _ => "text".to_owned(),
    }
}

const INPUT_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("name", "name", Reflect::Text),
    ("placeholder", "placeholder", Reflect::Text),
    ("min", "min", Reflect::Text),
    ("max", "max", Reflect::Text),
    ("step", "step", Reflect::Text),
    ("pattern", "pattern", Reflect::Text),
    ("minLength", "minlength", Reflect::Number(-1)),
    ("maxLength", "maxlength", Reflect::Number(-1)),
    ("size", "size", Reflect::Number(20)),
    ("required", "required", Reflect::Flag),
    ("disabled", "disabled", Reflect::Flag),
    ("readOnly", "readonly", Reflect::Flag),
    ("multiple", "multiple", Reflect::Flag),
    ("spellcheck", "spellcheck", Reflect::Flag),
    ("autocomplete", "autocomplete", Reflect::Text),
    ("accept", "accept", Reflect::Text),
    ("alt", "alt", Reflect::Text),
    ("src", "src", Reflect::Url),
    ("formAction", "formaction", Reflect::Url),
    ("formMethod", "formmethod", Reflect::Text),
    ("formNoValidate", "formnovalidate", Reflect::Flag),
    ("formTarget", "formtarget", Reflect::Text),
    ("width", "width", Reflect::Number(0)),
    ("height", "height", Reflect::Number(0)),
];

const TEXTAREA_ACCESSORS: &[Accessor] = &[(
    "type",
    Some(|_, _, _| Ok(text("textarea"))),
    None,
)];

const TEXTAREA_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("name", "name", Reflect::Text),
    ("placeholder", "placeholder", Reflect::Text),
    ("rows", "rows", Reflect::Number(2)),
    ("cols", "cols", Reflect::Number(20)),
    ("wrap", "wrap", Reflect::Text),
    ("minLength", "minlength", Reflect::Number(-1)),
    ("maxLength", "maxlength", Reflect::Number(-1)),
    ("required", "required", Reflect::Flag),
    ("disabled", "disabled", Reflect::Flag),
    ("readOnly", "readonly", Reflect::Flag),
    ("autocomplete", "autocomplete", Reflect::Text),
    ("spellcheck", "spellcheck", Reflect::Flag),
];

// ─── Select and option ──────────────────────────────────────────────────────────────────────

fn select_of_option(dom: &Dom, option: NodeId) -> Option<NodeId> {
    let parent = dom.parent(option)?;
    match dom.get(parent).and_then(|n| n.tag()) {
        Some("select") => Some(parent),
        Some("optgroup") | Some("datalist") => dom
            .parent(parent)
            .filter(|p| dom.get(*p).and_then(|n| n.tag()) == Some("select")),
        _ => None,
    }
}

fn is_multiple(dom: &Dom, select: NodeId) -> bool {
    dom.get_attr(select, "multiple").is_some()
}

/// Whether `option` is selected.
pub fn option_selected(dom: &Dom, option: NodeId) -> bool {
    match select_of_option(dom, option) {
        Some(select) if is_multiple(dom, select) => dom.get(option).is_some_and(|n| {
            n.checked || (!n.dirty_checked && n.attr("selected").is_some())
        }),
        Some(select) => dom.selected_option(select) == Some(option),
        None => dom.get(option).is_some_and(|n| n.attr("selected").is_some()),
    }
}

/// Selects `option` (or takes it out of the selection, for a multiple select).
pub fn set_option_selected(dom: &mut Dom, option: NodeId, on: bool) {
    match select_of_option(dom, option) {
        Some(select) if is_multiple(dom, select) => {
            if let Some(node) = dom.get_mut(option) {
                node.checked = on;
                node.dirty_checked = true;
            }
            dom.mark(select);
        }
        Some(select) => {
            let index = dom.options(select).iter().position(|o| *o == option);
            if on {
                dom.select_index(select, index);
            } else if dom.selected_option(select) == Some(option) {
                dom.select_index(select, None);
            }
        }
        None => {}
    }
}

const SELECT_ACCESSORS: &[Accessor] = &[
    (
        "value",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                if is_multiple(dom, id) {
                    dom.options(id).into_iter().find(|o| option_selected(dom, *o)).map(|o| dom.option_value(o)).unwrap_or_default()
                } else {
                    dom.value(id)
                }
            })))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| {
                if is_multiple(dom, id) {
                    for option in dom.options(id) {
                        let on = dom.option_value(option) == value;
                        set_option_selected(dom, option, on);
                    }
                } else {
                    dom.set_value(id, &value);
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "options",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let options = read_dom(cx, |dom| dom.options(id));
            Ok(collection(options, cx))
        }),
        None,
    ),
    (
        "selectedOptions",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let options = read_dom(cx, |dom| dom.options(id).into_iter().filter(|o| option_selected(dom, *o)).collect());
            Ok(collection(options, cx))
        }),
        None,
    ),
    (
        "length",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.options(id).len()).into())
        }),
        None,
    ),
    (
        "selectedIndex",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| {
                let options = dom.options(id);
                options.iter().position(|o| option_selected(dom, *o)).map_or(-1, |i| i as i32)
            })
            .into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let index = arg(args, 0).to_number(cx)?;
            with_dom(cx, |dom| {
                let options = dom.options(id);
                if is_multiple(dom, id) {
                    for (i, option) in options.iter().enumerate() {
                        set_option_selected(dom, *option, index >= 0. && i == index as usize);
                    }
                } else {
                    let index = (index >= 0. && (index as usize) < options.len()).then_some(index as usize);
                    dom.select_index(id, index);
                }
            });
            Ok(JsValue::undefined())
        }),
    ),
    (
        "type",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(if read_dom(cx, |dom| is_multiple(dom, id)) { "select-multiple" } else { "select-one" }))
        }),
        None,
    ),
    ("form", Some(|this, _, cx| form_of(this, cx)), None),
    ("labels", Some(|this, _, cx| labels_of(this, cx)), None),
    ("validity", Some(|_, _, cx| validity(cx)), None),
];

const SELECT_METHODS: &[Method] = &[
    ("item", 1, |this, args, cx| {
        let id = node_of(this)?;
        let at = arg(args, 0).to_number(cx)? as usize;
        let option = read_dom(cx, |dom| dom.options(id).get(at).copied());
        Ok(wrap_opt(cx, option))
    }),
    ("add", 2, |this, args, cx| {
        let id = node_of(this)?;
        let option = super::node_arg(args, 0, "add", "HTMLSelectElement")?;
        let before = node_id(&arg(args, 1)).or_else(|| {
            arg(args, 1).as_number().and_then(|i| read_dom(cx, |dom| dom.options(id).get(i as usize).copied()))
        });
        let parent = match before {
            Some(b) => read_dom(cx, |dom| dom.parent(b)).unwrap_or(id),
            None => id,
        };
        super::node::insert(cx, parent, option, before, "add")?;
        Ok(JsValue::undefined())
    }),
    ("remove", 1, |this, args, cx| {
        let id = node_of(this)?;
        let index = arg(args, 0);
        if index.is_undefined() {
            with_dom(cx, |dom| dom.detach(id));
        } else {
            let at = index.to_number(cx)? as usize;
            with_dom(cx, |dom| {
                if let Some(option) = dom.options(id).get(at).copied() {
                    dom.detach(option);
                }
            });
        }
        Ok(JsValue::undefined())
    }),
    ("checkValidity", 0, |_, _, _| Ok(true.into())),
    ("reportValidity", 0, |_, _, _| Ok(true.into())),
    ("setCustomValidity", 1, |_, _, _| Ok(JsValue::undefined())),
];

const SELECT_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("name", "name", Reflect::Text),
    ("multiple", "multiple", Reflect::Flag),
    ("disabled", "disabled", Reflect::Flag),
    ("required", "required", Reflect::Flag),
    ("size", "size", Reflect::Number(0)),
    ("autocomplete", "autocomplete", Reflect::Text),
];

const OPTION_ACCESSORS: &[Accessor] = &[
    (
        "value",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.option_value(id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "value", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "text",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.text_content(id).split_whitespace().collect::<Vec<_>>().join(" "))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_text(id, &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "label",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "label").unwrap_or_else(|| dom.text_content(id).trim().to_owned()))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "label", &value));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "selected",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| option_selected(dom, id)).into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let on = arg(args, 0).to_boolean();
            with_dom(cx, |dom| set_option_selected(dom, id, on));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "index",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| {
                select_of_option(dom, id).and_then(|s| dom.options(s).iter().position(|o| *o == id)).unwrap_or(0)
            })
            .into())
        }),
        None,
    ),
    ("form", Some(|this, _, cx| form_of(this, cx)), None),
];

const OPTION_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("defaultSelected", "selected", Reflect::Flag),
    ("disabled", "disabled", Reflect::Flag),
];

// ─── Buttons, forms, labels ─────────────────────────────────────────────────────────────────

const BUTTON_ACCESSORS: &[Accessor] = &[
    (
        "type",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| {
                match dom.get_attr(id, "type").map(|t| t.to_ascii_lowercase()).as_deref() {
                    Some("button") => "button",
                    Some("reset") => "reset",
                    _ => "submit",
                }
                .to_owned()
            })))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "type", &value));
            Ok(JsValue::undefined())
        }),
    ),
    ("form", Some(|this, _, cx| form_of(this, cx)), None),
    ("labels", Some(|this, _, cx| labels_of(this, cx)), None),
    ("validity", Some(|_, _, cx| validity(cx)), None),
];

const BUTTON_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("name", "name", Reflect::Text),
    ("value", "value", Reflect::Text),
    ("disabled", "disabled", Reflect::Flag),
    ("autofocus", "autofocus", Reflect::Flag),
    ("formAction", "formaction", Reflect::Url),
    ("formNoValidate", "formnovalidate", Reflect::Flag),
];

/// The controls a form holds, in document order.
fn form_controls(dom: &Dom, form: NodeId) -> Vec<NodeId> {
    dom.descendants(form)
        .into_iter()
        .filter(|n| matches!(dom.get(*n).and_then(|x| x.tag()), Some("input" | "select" | "textarea" | "button" | "fieldset" | "output" | "object")))
        .collect()
}

const FORM_ACCESSORS: &[Accessor] = &[
    (
        "elements",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let controls = read_dom(cx, |dom| form_controls(dom, id));
            Ok(collection(controls, cx))
        }),
        None,
    ),
    (
        "length",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| form_controls(dom, id).len()).into())
        }),
        None,
    ),
];

const FORM_METHODS: &[Method] = &[
    // `submit()` sends no `submit` event, as a browser's does not; the drawer hears it.
    ("submit", 0, |this, _, cx| {
        let id = node_of(this)?;
        shared(cx).borrow_mut().requests.push(crate::Request::Submit(id));
        Ok(JsValue::undefined())
    }),
    ("requestSubmit", 1, |this, args, cx| {
        let id = node_of(this)?;
        super::events::submit(cx, id, node_id(&arg(args, 0)))?;
        Ok(JsValue::undefined())
    }),
    ("reset", 0, |this, _, cx| {
        let id = node_of(this)?;
        let allowed = super::events::fire(cx, this, "Event", "reset", &[("bubbles", true.into()), ("cancelable", true.into())])?;
        if allowed {
            with_dom(cx, |dom| {
                for control in form_controls(dom, id) {
                    if let Some(node) = dom.get_mut(control) {
                        node.dirty_value = false;
                        node.dirty_checked = false;
                        node.checked = node.attr("checked").is_some();
                        node.selected = None;
                        let default = node.attr("value").unwrap_or("").to_owned();
                        if let Some(draft) = node.draft.as_mut() {
                            *draft = crate::field::Draft::new(default);
                        }
                    }
                }
            });
        }
        Ok(JsValue::undefined())
    }),
    ("checkValidity", 0, |_, _, _| Ok(true.into())),
    ("reportValidity", 0, |_, _, _| Ok(true.into())),
];

const FORM_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("action", "action", Reflect::Url),
    ("method", "method", Reflect::Keyword("get")),
    ("name", "name", Reflect::Text),
    ("target", "target", Reflect::Text),
    ("enctype", "enctype", Reflect::Keyword("application/x-www-form-urlencoded")),
    ("noValidate", "novalidate", Reflect::Flag),
    ("autocomplete", "autocomplete", Reflect::Keyword("on")),
    ("acceptCharset", "accept-charset", Reflect::Text),
];

/// What a `<label>` is for: the control its `for` names, or the first one inside it.
pub fn label_control(dom: &Dom, label: NodeId) -> Option<NodeId> {
    if let Some(named) = dom.get_attr(label, "for") {
        let root = dom.root_of(label);
        return dom
            .descendants(root)
            .into_iter()
            .find(|n| dom.get_attr(*n, "id").as_deref() == Some(named.as_str()));
    }
    dom.descendants(label).into_iter().find(|n| {
        matches!(dom.get(*n).and_then(|x| x.tag()), Some("input" | "select" | "textarea" | "button" | "meter" | "output" | "progress"))
    })
}

const LABEL_ACCESSORS: &[Accessor] = &[
    (
        "control",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let control = read_dom(cx, |dom| label_control(dom, id));
            Ok(wrap_opt(cx, control))
        }),
        None,
    ),
    ("form", Some(|this, _, cx| form_of(this, cx)), None),
];

// ─── Links ──────────────────────────────────────────────────────────────────────────────────

/// The anchor's `href`, resolved against the document's address; `None` when it has none or it
/// does not parse.
fn anchor_url(this: &JsValue, context: &Context) -> Option<url::Url> {
    let id = node_of(this).ok()?;
    let href = read_dom(context, |dom| dom.get_attr(id, "href"))?;
    shared(context).borrow().url.join(href.trim()).ok()
}

/// Changes one part of the anchor's URL, as setting `a.pathname` does.
fn set_anchor_part(this: &JsValue, args: &[JsValue], context: &mut Context, part: fn(&mut url::Url, &str)) -> JsResult<JsValue> {
    let id = node_of(this)?;
    let value = arg_string(args, 0, context)?;
    if let Some(mut url) = anchor_url(this, context) {
        part(&mut url, &value);
        with_dom(context, |dom| dom.set_attr(id, "href", url.as_str()));
    }
    Ok(JsValue::undefined())
}

macro_rules! url_part {
    ($name:literal => $get:expr, $set:expr) => {
        (
            $name,
            Some(|this, _, cx| {
                let part: fn(&url::Url) -> String = $get;
                Ok(text(&anchor_url(this, cx).map(|u| part(&u)).unwrap_or_default()))
            }),
            Some(|this, args, cx| set_anchor_part(this, args, cx, $set)),
        )
    };
}

/// The parts of a URL as `location` and an `<a>` answer them.
pub fn url_parts() -> [(&'static str, fn(&url::Url) -> String); 9] {
    [
        ("protocol", |u| format!("{}:", u.scheme())),
        ("host", |u| match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => String::new(),
        }),
        ("hostname", |u| u.host_str().unwrap_or("").to_owned()),
        ("port", |u| u.port().map(|p| p.to_string()).unwrap_or_default()),
        ("pathname", |u| u.path().to_owned()),
        ("search", |u| u.query().filter(|q| !q.is_empty()).map(|q| format!("?{q}")).unwrap_or_default()),
        ("hash", |u| u.fragment().filter(|f| !f.is_empty()).map(|f| format!("#{f}")).unwrap_or_default()),
        ("origin", |u| u.origin().ascii_serialization()),
        ("href", |u| u.to_string()),
    ]
}

const ANCHOR_ACCESSORS: &[Accessor] = &[
    (
        "href",
        Some(|this, _, cx| {
            match anchor_url(this, cx) {
                Some(url) => Ok(text(url.as_str())),
                None => {
                    let id = node_of(this)?;
                    Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "href")).unwrap_or_default()))
                }
            }
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "href", &value));
            Ok(JsValue::undefined())
        }),
    ),
    url_part!("protocol" => |u| format!("{}:", u.scheme()), |u, v| { let _ = u.set_scheme(v.trim_end_matches(':')); }),
    url_part!("host" => |u| match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => String::new(),
    }, |u, v| { let (host, port) = v.split_once(':').map_or((v, None), |(h, p)| (h, p.parse().ok())); let _ = u.set_host(Some(host)); let _ = u.set_port(port); }),
    url_part!("hostname" => |u| u.host_str().unwrap_or("").to_owned(), |u, v| { let _ = u.set_host(Some(v)); }),
    url_part!("port" => |u| u.port().map(|p| p.to_string()).unwrap_or_default(), |u, v| { let _ = u.set_port(v.parse().ok()); }),
    url_part!("pathname" => |u| u.path().to_owned(), |u, v| u.set_path(v)),
    url_part!("search" => |u| u.query().filter(|q| !q.is_empty()).map(|q| format!("?{q}")).unwrap_or_default(), |u, v| u.set_query(Some(v.trim_start_matches('?')).filter(|q| !q.is_empty()))),
    url_part!("hash" => |u| u.fragment().filter(|f| !f.is_empty()).map(|f| format!("#{f}")).unwrap_or_default(), |u, v| u.set_fragment(Some(v.trim_start_matches('#')).filter(|f| !f.is_empty()))),
    url_part!("username" => |u| u.username().to_owned(), |u, v| { let _ = u.set_username(v); }),
    url_part!("password" => |u| u.password().unwrap_or("").to_owned(), |u, v| { let _ = u.set_password(Some(v)); }),
    (
        "origin",
        Some(|this, _, cx| Ok(text(&anchor_url(this, cx).map(|u| u.origin().ascii_serialization()).unwrap_or_default()))),
        None,
    ),
    (
        "text",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.text_content(id))))
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg_string(args, 0, cx)?;
            with_dom(cx, |dom| dom.set_text(id, &value));
            Ok(JsValue::undefined())
        }),
    ),
];

const ANCHOR_METHODS: &[Method] = &[("toString", 0, |this, _, cx| {
    Ok(text(&anchor_url(this, cx).map(|u| u.to_string()).unwrap_or_default()))
})];

const ANCHOR_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("target", "target", Reflect::Text),
    ("rel", "rel", Reflect::Text),
    ("download", "download", Reflect::Text),
    ("hreflang", "hreflang", Reflect::Text),
    ("type", "type", Reflect::Text),
    ("referrerPolicy", "referrerpolicy", Reflect::Text),
    ("ping", "ping", Reflect::Text),
];

// ─── Media, images, dialogs, details ────────────────────────────────────────────────────────

const IMAGE_ACCESSORS: &[Accessor] = &[
    ("naturalWidth", Some(|_, _, _| Ok(0.into())), None),
    ("naturalHeight", Some(|_, _, _| Ok(0.into())), None),
    ("complete", Some(|_, _, _| Ok(true.into())), None),
    (
        "currentSrc",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(text(&read_dom(cx, |dom| dom.get_attr(id, "src")).unwrap_or_default()))
        }),
        None,
    ),
];

const IMAGE_METHODS: &[Method] = &[("decode", 0, |_, _, cx| super::resolved(JsValue::undefined(), cx))];

const IMAGE_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("src", "src", Reflect::Url),
    ("srcset", "srcset", Reflect::Text),
    ("sizes", "sizes", Reflect::Text),
    ("alt", "alt", Reflect::Text),
    ("width", "width", Reflect::Number(0)),
    ("height", "height", Reflect::Number(0)),
    ("loading", "loading", Reflect::Keyword("eager")),
    ("decoding", "decoding", Reflect::Keyword("auto")),
    ("crossOrigin", "crossorigin", Reflect::Text),
    ("useMap", "usemap", Reflect::Text),
    ("isMap", "ismap", Reflect::Flag),
    ("referrerPolicy", "referrerpolicy", Reflect::Text),
];

const MEDIA_METHODS: &[Method] = &[
    ("play", 0, |this, _, cx| {
        if let Some(object) = this.as_object() {
            super::hidden(&object, js_string!("__paused"), false, cx)?;
        }
        super::events::fire(cx, this, "Event", "play", &[])?;
        super::resolved(JsValue::undefined(), cx)
    }),
    ("pause", 0, |this, _, cx| {
        if let Some(object) = this.as_object() {
            super::hidden(&object, js_string!("__paused"), true, cx)?;
        }
        super::events::fire(cx, this, "Event", "pause", &[])?;
        Ok(JsValue::undefined())
    }),
    ("load", 0, |_, _, _| Ok(JsValue::undefined())),
    ("canPlayType", 1, |_, _, _| Ok(text(""))),
    ("fastSeek", 1, |_, _, _| Ok(JsValue::undefined())),
    ("addTextTrack", 3, |_, _, cx| Ok(plain_object(cx).into())),
];

const MEDIA_ACCESSORS: &[Accessor] = &[
    (
        "paused",
        Some(|this, _, cx| {
            let paused = get(this, "__paused", cx)?;
            Ok((paused.is_undefined() || paused.to_boolean()).into())
        }),
        None,
    ),
    ("duration", Some(|_, _, _| Ok(f64::NAN.into())), None),
    ("readyState", Some(|_, _, _| Ok(0.into())), None),
    ("networkState", Some(|_, _, _| Ok(0.into())), None),
    ("ended", Some(|_, _, _| Ok(false.into())), None),
    ("seeking", Some(|_, _, _| Ok(false.into())), None),
    ("buffered", Some(|_, _, cx| super::platform::empty_ranges(cx)), None),
    ("played", Some(|_, _, cx| super::platform::empty_ranges(cx)), None),
    ("seekable", Some(|_, _, cx| super::platform::empty_ranges(cx)), None),
    ("textTracks", Some(|_, _, cx| Ok(array(Vec::new(), cx))), None),
    ("videoWidth", Some(|_, _, _| Ok(0.into())), None),
    ("videoHeight", Some(|_, _, _| Ok(0.into())), None),
];

const MEDIA_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("src", "src", Reflect::Url),
    ("autoplay", "autoplay", Reflect::Flag),
    ("loop", "loop", Reflect::Flag),
    ("controls", "controls", Reflect::Flag),
    ("muted", "muted", Reflect::Flag),
    ("defaultMuted", "muted", Reflect::Flag),
    ("preload", "preload", Reflect::Keyword("auto")),
    ("poster", "poster", Reflect::Url),
    ("crossOrigin", "crossorigin", Reflect::Text),
    ("playsInline", "playsinline", Reflect::Flag),
];

const DIALOG_METHODS: &[Method] = &[
    ("show", 0, |this, _, cx| {
        let id = node_of(this)?;
        with_dom(cx, |dom| dom.set_attr(id, "open", ""));
        Ok(JsValue::undefined())
    }),
    ("showModal", 0, |this, _, cx| {
        let id = node_of(this)?;
        with_dom(cx, |dom| dom.set_attr(id, "open", ""));
        Ok(JsValue::undefined())
    }),
    ("close", 1, |this, args, cx| {
        let id = node_of(this)?;
        let was_open = read_dom(cx, |dom| dom.get_attr(id, "open").is_some());
        if let Some(value) = opt_string(args, 0, cx)?
            && let Some(object) = this.as_object()
        {
            super::hidden(&object, js_string!("__returnValue"), text(&value), cx)?;
        }
        with_dom(cx, |dom| dom.remove_attr(id, "open"));
        if was_open {
            super::events::fire(cx, this, "Event", "close", &[])?;
        }
        Ok(JsValue::undefined())
    }),
    ("requestClose", 1, |this, args, cx| call_method(this, "close", args, cx)),
];

const DIALOG_ACCESSORS: &[Accessor] = &[(
    "returnValue",
    Some(|this, _, cx| {
        let value = get(this, "__returnValue", cx)?;
        Ok(if value.is_undefined() { text("") } else { value })
    }),
    Some(|this, args, cx| {
        if let Some(object) = this.as_object() {
            let value = arg_string(args, 0, cx)?;
            super::hidden(&object, js_string!("__returnValue"), text(&value), cx)?;
        }
        Ok(JsValue::undefined())
    }),
)];

const OPEN_REFLECTED: &[(&str, &str, Reflect)] = &[("open", "open", Reflect::Flag)];

const PROGRESS_ACCESSORS: &[Accessor] = &[
    (
        "value",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.get_attr(id, "value").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.)).into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg(args, 0).to_number(cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "value", &value.to_string()));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "max",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| dom.get_attr(id, "max").and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.)).into())
        }),
        Some(|this, args, cx| {
            let id = node_of(this)?;
            let value = arg(args, 0).to_number(cx)?;
            with_dom(cx, |dom| dom.set_attr(id, "max", &value.to_string()));
            Ok(JsValue::undefined())
        }),
    ),
    (
        "position",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            Ok(read_dom(cx, |dom| {
                let value = dom.get_attr(id, "value").and_then(|v| v.parse::<f64>().ok());
                let max = dom.get_attr(id, "max").and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.);
                value.map_or(-1., |v| v / max)
            })
            .into())
        }),
        None,
    ),
];

// ─── Tables ─────────────────────────────────────────────────────────────────────────────────

fn children_tagged(dom: &Dom, id: NodeId, tags: &[&str]) -> Vec<NodeId> {
    dom.children(id)
        .iter()
        .copied()
        .filter(|c| dom.get(*c).and_then(|n| n.tag()).is_some_and(|t| tags.contains(&t)))
        .collect()
}

fn table_rows(dom: &Dom, table: NodeId) -> Vec<NodeId> {
    let mut rows = Vec::new();
    let mut bodies = Vec::new();
    for child in dom.children(table).iter().copied() {
        match dom.get(child).and_then(|n| n.tag()) {
            Some("tr") => bodies.push(vec![child]),
            Some("thead") => rows.extend(children_tagged(dom, child, &["tr"])),
            Some("tbody") => bodies.push(children_tagged(dom, child, &["tr"])),
            _ => {}
        }
    }
    rows.extend(bodies.into_iter().flatten());
    for child in dom.children(table).iter().copied() {
        if dom.get(child).and_then(|n| n.tag()) == Some("tfoot") {
            rows.extend(children_tagged(dom, child, &["tr"]));
        }
    }
    rows
}

fn add_child(context: &mut Context, parent: NodeId, tag: &str, index: Option<i64>, among: &[&str]) -> JsResult<JsValue> {
    let id = with_dom(context, |dom| {
        let siblings = children_tagged(dom, parent, among);
        if index.is_some_and(|i| i < -1 || i > siblings.len() as i64) {
            return None;
        }
        let id = dom.create_element(tag);
        let before = match index {
            Some(i) if i >= 0 && (i as usize) < siblings.len() => Some(siblings[i as usize]),
            _ => None,
        };
        dom.insert_before(parent, id, before);
        Some(id)
    });
    match id {
        Some(id) => Ok(wrap(context, id)),
        None => Err(dom_error(context, "The index provided is outside the range of the collection.", "IndexSizeError")),
    }
}

fn index_arg(args: &[JsValue], context: &mut Context) -> JsResult<Option<i64>> {
    let given = arg(args, 0);
    Ok(if given.is_undefined() { None } else { Some(given.to_number(context)? as i64) })
}

const TABLE_ACCESSORS: &[Accessor] = &[
    (
        "rows",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let rows = read_dom(cx, |dom| table_rows(dom, id));
            Ok(collection(rows, cx))
        }),
        None,
    ),
    (
        "tBodies",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let bodies = read_dom(cx, |dom| children_tagged(dom, id, &["tbody"]));
            Ok(collection(bodies, cx))
        }),
        None,
    ),
    (
        "tHead",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let head = read_dom(cx, |dom| children_tagged(dom, id, &["thead"]).first().copied());
            Ok(wrap_opt(cx, head))
        }),
        None,
    ),
    (
        "tFoot",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let foot = read_dom(cx, |dom| children_tagged(dom, id, &["tfoot"]).first().copied());
            Ok(wrap_opt(cx, foot))
        }),
        None,
    ),
    (
        "caption",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let caption = read_dom(cx, |dom| children_tagged(dom, id, &["caption"]).first().copied());
            Ok(wrap_opt(cx, caption))
        }),
        None,
    ),
];

fn first_or_new(context: &mut Context, table: NodeId, tag: &str, first: bool) -> JsResult<JsValue> {
    let found = read_dom(context, |dom| children_tagged(dom, table, &[tag]).first().copied());
    if let Some(found) = found {
        return Ok(wrap(context, found));
    }
    let id = with_dom(context, |dom| {
        let id = dom.create_element(tag);
        let before = if first { dom.children(table).first().copied() } else { None };
        dom.insert_before(table, id, before);
        id
    });
    Ok(wrap(context, id))
}

const TABLE_METHODS: &[Method] = &[
    ("createTBody", 0, |this, _, cx| {
        let table = node_of(this)?;
        add_child(cx, table, "tbody", None, &["tbody"])
    }),
    ("createTHead", 0, |this, _, cx| first_or_new(cx, node_of(this)?, "thead", true)),
    ("createTFoot", 0, |this, _, cx| first_or_new(cx, node_of(this)?, "tfoot", false)),
    ("createCaption", 0, |this, _, cx| first_or_new(cx, node_of(this)?, "caption", true)),
    ("deleteTHead", 0, |this, _, cx| {
        let table = node_of(this)?;
        with_dom(cx, |dom| children_tagged(dom, table, &["thead"]).into_iter().for_each(|n| dom.detach(n)));
        Ok(JsValue::undefined())
    }),
    ("deleteTFoot", 0, |this, _, cx| {
        let table = node_of(this)?;
        with_dom(cx, |dom| children_tagged(dom, table, &["tfoot"]).into_iter().for_each(|n| dom.detach(n)));
        Ok(JsValue::undefined())
    }),
    ("deleteCaption", 0, |this, _, cx| {
        let table = node_of(this)?;
        with_dom(cx, |dom| children_tagged(dom, table, &["caption"]).into_iter().for_each(|n| dom.detach(n)));
        Ok(JsValue::undefined())
    }),
    ("insertRow", 1, |this, args, cx| {
        let table = node_of(this)?;
        let index = index_arg(args, cx)?;
        let body = read_dom(cx, |dom| children_tagged(dom, table, &["tbody"]).last().copied());
        let body = match body {
            Some(body) => body,
            None => node_of(&add_child(cx, table, "tbody", None, &[])?)?,
        };
        add_child(cx, body, "tr", index, &["tr"])
    }),
    ("deleteRow", 1, |this, args, cx| {
        let table = node_of(this)?;
        let index = arg(args, 0).to_number(cx)? as i64;
        with_dom(cx, |dom| {
            let rows = table_rows(dom, table);
            let at = if index < 0 { rows.len() as i64 - 1 } else { index };
            if let Some(row) = rows.get(at as usize) {
                dom.detach(*row);
            }
        });
        Ok(JsValue::undefined())
    }),
];

const SECTION_ACCESSORS: &[Accessor] = &[(
    "rows",
    Some(|this, _, cx| {
        let id = node_of(this)?;
        let rows = read_dom(cx, |dom| children_tagged(dom, id, &["tr"]));
        Ok(collection(rows, cx))
    }),
    None,
)];

const SECTION_METHODS: &[Method] = &[
    ("insertRow", 1, |this, args, cx| {
        let section = node_of(this)?;
        let index = index_arg(args, cx)?;
        add_child(cx, section, "tr", index, &["tr"])
    }),
    ("deleteRow", 1, |this, args, cx| {
        let section = node_of(this)?;
        let index = arg(args, 0).to_number(cx)? as i64;
        with_dom(cx, |dom| {
            let rows = children_tagged(dom, section, &["tr"]);
            let at = if index < 0 { rows.len() as i64 - 1 } else { index };
            if let Some(row) = rows.get(at as usize) {
                dom.detach(*row);
            }
        });
        Ok(JsValue::undefined())
    }),
];

const ROW_ACCESSORS: &[Accessor] = &[
    (
        "cells",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let cells = read_dom(cx, |dom| children_tagged(dom, id, &["td", "th"]));
            Ok(collection(cells, cx))
        }),
        None,
    ),
    (
        "rowIndex",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let index = read_dom(cx, |dom| {
                let mut at = dom.parent(id);
                while let Some(node) = at {
                    if dom.get(node).and_then(|n| n.tag()) == Some("table") {
                        return table_rows(dom, node).iter().position(|r| *r == id);
                    }
                    at = dom.parent(node);
                }
                None
            });
            Ok(index.map_or(-1., |i| i as f64).into())
        }),
        None,
    ),
    (
        "sectionRowIndex",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let index = read_dom(cx, |dom| dom.parent(id).and_then(|p| children_tagged(dom, p, &["tr"]).iter().position(|r| *r == id)));
            Ok(index.map_or(-1., |i| i as f64).into())
        }),
        None,
    ),
];

const ROW_METHODS: &[Method] = &[
    ("insertCell", 1, |this, args, cx| {
        let row = node_of(this)?;
        let index = index_arg(args, cx)?;
        add_child(cx, row, "td", index, &["td", "th"])
    }),
    ("deleteCell", 1, |this, args, cx| {
        let row = node_of(this)?;
        let index = arg(args, 0).to_number(cx)? as i64;
        with_dom(cx, |dom| {
            let cells = children_tagged(dom, row, &["td", "th"]);
            let at = if index < 0 { cells.len() as i64 - 1 } else { index };
            if let Some(cell) = cells.get(at as usize) {
                dom.detach(*cell);
            }
        });
        Ok(JsValue::undefined())
    }),
];

const CELL_ACCESSORS: &[Accessor] = &[(
    "cellIndex",
    Some(|this, _, cx| {
        let id = node_of(this)?;
        let index = read_dom(cx, |dom| dom.parent(id).and_then(|row| children_tagged(dom, row, &["td", "th"]).iter().position(|c| *c == id)));
        Ok(index.map_or(-1., |i| i as f64).into())
    }),
    None,
)];

const CELL_REFLECTED: &[(&str, &str, Reflect)] = &[
    ("colSpan", "colspan", Reflect::Number(1)),
    ("rowSpan", "rowspan", Reflect::Number(1)),
    ("headers", "headers", Reflect::Text),
    ("scope", "scope", Reflect::Text),
    ("abbr", "abbr", Reflect::Text),
];

// ─── SVG ────────────────────────────────────────────────────────────────────────────────────

const SVG_ACCESSORS: &[Accessor] = &[
    (
        "ownerSVGElement",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let owner = read_dom(cx, |dom| {
                let mut at = dom.parent(id);
                while let Some(node) = at {
                    if dom.get(node).is_some_and(|n| n.tag() == Some("svg") && n.namespace == Namespace::Svg) {
                        return Some(node);
                    }
                    at = dom.parent(node);
                }
                None
            });
            Ok(wrap_opt(cx, owner))
        }),
        None,
    ),
    ("viewportElement", Some(|_, _, _| Ok(JsValue::null())), None),
];

const SVG_GRAPHICS_METHODS: &[Method] = &[
    ("getBBox", 0, |this, _, cx| {
        let id = node_of(this)?;
        let rect = painted(cx, id);
        rect_object(cx, Rect { x: 0., y: 0., ..rect })
    }),
    ("getCTM", 0, |_, _, cx| super::platform::identity_matrix(cx)),
    ("getScreenCTM", 0, |_, _, cx| super::platform::identity_matrix(cx)),
    ("getTotalLength", 0, |_, _, _| Ok(0.into())),
    ("getPointAtLength", 1, |_, _, cx| {
        let point = plain_object(cx);
        set(&point, "x", 0, cx)?;
        set(&point, "y", 0, cx)?;
        Ok(point.into())
    }),
];

const SVG_SVG_METHODS: &[Method] = &[
    ("createSVGPoint", 0, |_, _, cx| super::platform::new_point(cx)),
    ("createSVGMatrix", 0, |_, _, cx| super::platform::identity_matrix(cx)),
    ("suspendRedraw", 1, |_, _, _| Ok(0.into())),
    ("unsuspendRedraw", 1, |_, _, _| Ok(JsValue::undefined())),
    ("forceRedraw", 0, |_, _, _| Ok(JsValue::undefined())),
];

// ─── Installing ─────────────────────────────────────────────────────────────────────────────

fn plain_class(context: &mut Context, name: &'static str, parent: &str) -> JsResult<JsObject> {
    Ok(class(
        context,
        ClassSpec {
            name,
            length: 0,
            construct: illegal_constructor,
            parent: Some(parent),
            methods: &[],
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?
    .1)
}

fn full_class(
    context: &mut Context,
    name: &'static str,
    parent: &str,
    methods: &[Method],
    accessors: &[Accessor],
    reflected: &[(&str, &str, Reflect)],
) -> JsResult<JsObject> {
    let (_, prototype) = class(
        context,
        ClassSpec {
            name,
            length: 0,
            construct: illegal_constructor,
            parent: Some(parent),
            methods,
            accessors,
            statics: &[],
            global: true,
        },
    )?;
    reflect(context, &prototype, reflected)?;
    Ok(prototype)
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let element = full_class(context, "Element", "Node", ELEMENT_METHODS, ELEMENT_ACCESSORS, &[])?;
    extend_prototype(context, &element, PARENT_NODE_METHODS, PARENT_NODE_ACCESSORS)?;
    extend_prototype(context, &element, CHILD_NODE_METHODS, CHILD_NODE_ACCESSORS)?;
    // `HTMLElement` is constructed only as the `super()` of a custom element's class.
    let (_, html) = class(
        context,
        ClassSpec {
            name: "HTMLElement",
            length: 0,
            construct: super::platform::construct_html_element,
            parent: Some("Element"),
            methods: HTML_ELEMENT_METHODS,
            accessors: HTML_ELEMENT_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    reflect(context, &html, HTML_REFLECTED)?;
    extend_prototype(context, &html, FOCUS_METHODS, STYLED_ACCESSORS)?;
    for name in PLAIN_HTML_CLASSES {
        plain_class(context, name, "HTMLElement")?;
    }
    full_class(context, "HTMLInputElement", "HTMLElement", FIELD_METHODS, FIELD_ACCESSORS, INPUT_REFLECTED)
        .and_then(|p| extend_prototype(context, &p, &[
            ("stepUp", 1, |_, _, _| Ok(JsValue::undefined())),
            ("stepDown", 1, |_, _, _| Ok(JsValue::undefined())),
            ("showPicker", 0, |_, _, _| Ok(JsValue::undefined())),
        ], INPUT_ACCESSORS).map(|_| p))?;
    full_class(context, "HTMLTextAreaElement", "HTMLElement", FIELD_METHODS, FIELD_ACCESSORS, TEXTAREA_REFLECTED)
        .and_then(|p| extend_prototype(context, &p, &[], TEXTAREA_ACCESSORS).map(|_| p))?;
    full_class(context, "HTMLSelectElement", "HTMLElement", SELECT_METHODS, SELECT_ACCESSORS, SELECT_REFLECTED)?;
    full_class(context, "HTMLOptionElement", "HTMLElement", &[], OPTION_ACCESSORS, OPTION_REFLECTED)?;
    full_class(context, "HTMLButtonElement", "HTMLElement", &[
        ("checkValidity", 0, |_, _, _| Ok(true.into())),
        ("reportValidity", 0, |_, _, _| Ok(true.into())),
        ("setCustomValidity", 1, |_, _, _| Ok(JsValue::undefined())),
    ], BUTTON_ACCESSORS, BUTTON_REFLECTED)?;
    full_class(context, "HTMLFormElement", "HTMLElement", FORM_METHODS, FORM_ACCESSORS, FORM_REFLECTED)?;
    full_class(context, "HTMLLabelElement", "HTMLElement", &[], LABEL_ACCESSORS, &[("htmlFor", "for", Reflect::Text)])?;
    full_class(context, "HTMLAnchorElement", "HTMLElement", ANCHOR_METHODS, ANCHOR_ACCESSORS, ANCHOR_REFLECTED)?;
    {
        let (_, area) = shared(context).borrow().class("HTMLAreaElement").expect("made");
        extend_prototype(context, &area, ANCHOR_METHODS, ANCHOR_ACCESSORS)?;
        reflect(context, &area, &[("alt", "alt", Reflect::Text), ("coords", "coords", Reflect::Text), ("shape", "shape", Reflect::Text)])?;
    }
    full_class(context, "HTMLImageElement", "HTMLElement", IMAGE_METHODS, IMAGE_ACCESSORS, IMAGE_REFLECTED)?;
    full_class(context, "HTMLMediaElement", "HTMLElement", MEDIA_METHODS, MEDIA_ACCESSORS, MEDIA_REFLECTED)?;
    let media = shared(context).borrow().prototype("HTMLMediaElement");
    for (name, value) in [("currentTime", JsValue::from(0)), ("volume", 1.into()), ("playbackRate", 1.into()), ("defaultPlaybackRate", 1.into())] {
        media.set(JsString::from(name), value, false, context)?;
    }
    full_class(context, "HTMLVideoElement", "HTMLMediaElement", &[], &[], &[("width", "width", Reflect::Number(0)), ("height", "height", Reflect::Number(0))])?;
    plain_class(context, "HTMLAudioElement", "HTMLMediaElement")?;
    full_class(context, "HTMLDialogElement", "HTMLElement", DIALOG_METHODS, DIALOG_ACCESSORS, OPEN_REFLECTED)?;
    full_class(context, "HTMLDetailsElement", "HTMLElement", &[], &[], OPEN_REFLECTED)?;
    full_class(context, "HTMLProgressElement", "HTMLElement", &[], PROGRESS_ACCESSORS, &[])?;
    full_class(context, "HTMLTableElement", "HTMLElement", TABLE_METHODS, TABLE_ACCESSORS, &[])?;
    full_class(context, "HTMLTableSectionElement", "HTMLElement", SECTION_METHODS, SECTION_ACCESSORS, &[])?;
    full_class(context, "HTMLTableRowElement", "HTMLElement", ROW_METHODS, ROW_ACCESSORS, &[])?;
    full_class(context, "HTMLTableCellElement", "HTMLElement", &[], CELL_ACCESSORS, CELL_REFLECTED)?;
    full_class(context, "HTMLTemplateElement", "HTMLElement", &[], super::platform::TEMPLATE_ACCESSORS, &[])?;
    full_class(context, "HTMLStyleElement", "HTMLElement", &[], super::platform::STYLE_ELEMENT_ACCESSORS, &[("media", "media", Reflect::Text), ("disabled", "disabled", Reflect::Flag)])?;
    full_class(context, "HTMLCanvasElement", "HTMLElement", super::platform::CANVAS_METHODS, super::platform::CANVAS_ACCESSORS, &[])?;
    // The rest's own reflected attributes.
    for (name, reflected) in [
        ("HTMLScriptElement", &[("src", "src", Reflect::Url), ("type", "type", Reflect::Text), ("async", "async", Reflect::Flag), ("defer", "defer", Reflect::Flag), ("noModule", "nomodule", Reflect::Flag), ("crossOrigin", "crossorigin", Reflect::Text), ("integrity", "integrity", Reflect::Text)][..]),
        ("HTMLLinkElement", &[("href", "href", Reflect::Url), ("rel", "rel", Reflect::Text), ("media", "media", Reflect::Text), ("type", "type", Reflect::Text), ("as", "as", Reflect::Text), ("crossOrigin", "crossorigin", Reflect::Text), ("hreflang", "hreflang", Reflect::Text)][..]),
        ("HTMLMetaElement", &[("name", "name", Reflect::Text), ("content", "content", Reflect::Text), ("httpEquiv", "http-equiv", Reflect::Text), ("charset", "charset", Reflect::Text)][..]),
        ("HTMLIFrameElement", &[("src", "src", Reflect::Url), ("srcdoc", "srcdoc", Reflect::Text), ("name", "name", Reflect::Text), ("width", "width", Reflect::Text), ("height", "height", Reflect::Text), ("allow", "allow", Reflect::Text), ("loading", "loading", Reflect::Keyword("eager"))][..]),
        ("HTMLSourceElement", &[("src", "src", Reflect::Url), ("type", "type", Reflect::Text), ("srcset", "srcset", Reflect::Text), ("media", "media", Reflect::Text)][..]),
        ("HTMLOptGroupElement", &[("label", "label", Reflect::Text), ("disabled", "disabled", Reflect::Flag)][..]),
        ("HTMLFieldSetElement", &[("disabled", "disabled", Reflect::Flag), ("name", "name", Reflect::Text)][..]),
        ("HTMLOListElement", &[("start", "start", Reflect::Number(1)), ("reversed", "reversed", Reflect::Flag), ("type", "type", Reflect::Text)][..]),
        ("HTMLLIElement", &[("value", "value", Reflect::Number(0))][..]),
        ("HTMLTableColElement", &[("span", "span", Reflect::Number(1))][..]),
        ("HTMLTimeElement", &[("dateTime", "datetime", Reflect::Text)][..]),
        ("HTMLDataElement", &[("value", "value", Reflect::Text)][..]),
        ("HTMLQuoteElement", &[("cite", "cite", Reflect::Url)][..]),
        ("HTMLModElement", &[("cite", "cite", Reflect::Url), ("dateTime", "datetime", Reflect::Text)][..]),
        ("HTMLBaseElement", &[("href", "href", Reflect::Url), ("target", "target", Reflect::Text)][..]),
        ("HTMLSlotElement", &[("name", "name", Reflect::Text)][..]),
        ("HTMLOutputElement", &[("name", "name", Reflect::Text)][..]),
        ("HTMLMeterElement", &[("min", "min", Reflect::Text), ("max", "max", Reflect::Text), ("low", "low", Reflect::Text), ("high", "high", Reflect::Text), ("optimum", "optimum", Reflect::Text)][..]),
        ("HTMLEmbedElement", &[("src", "src", Reflect::Url), ("type", "type", Reflect::Text), ("width", "width", Reflect::Text), ("height", "height", Reflect::Text)][..]),
        ("HTMLObjectElement", &[("data", "data", Reflect::Url), ("type", "type", Reflect::Text), ("name", "name", Reflect::Text), ("width", "width", Reflect::Text), ("height", "height", Reflect::Text)][..]),
    ] {
        let prototype = shared(context).borrow().prototype(name);
        reflect(context, &prototype, reflected)?;
    }
    for (name, accessors) in [
        ("HTMLTitleElement", &[(
            "text",
            Some((|this, _, cx| {
                let id = node_of(this)?;
                Ok(text(&read_dom(cx, |dom| dom.text_content(id))))
            }) as boa_engine::native_function::NativeFunctionPointer),
            Some((|this, args, cx| {
                let id = node_of(this)?;
                let value = arg_string(args, 0, cx)?;
                with_dom(cx, |dom| dom.set_text(id, &value));
                Ok(JsValue::undefined())
            }) as boa_engine::native_function::NativeFunctionPointer),
        )][..]),
        ("HTMLScriptElement", &[(
            "text",
            Some((|this, _, cx| {
                let id = node_of(this)?;
                Ok(text(&read_dom(cx, |dom| dom.text_content(id))))
            }) as boa_engine::native_function::NativeFunctionPointer),
            Some((|this, args, cx| {
                let id = node_of(this)?;
                let value = arg_string(args, 0, cx)?;
                with_dom(cx, |dom| dom.set_text(id, &value));
                Ok(JsValue::undefined())
            }) as boa_engine::native_function::NativeFunctionPointer),
        )][..]),
        ("HTMLIFrameElement", &[
            ("contentWindow", Some((|_, _, _| Ok(JsValue::null())) as boa_engine::native_function::NativeFunctionPointer), None),
            ("contentDocument", Some((|_, _, _| Ok(JsValue::null())) as boa_engine::native_function::NativeFunctionPointer), None),
        ][..]),
        ("HTMLLinkElement", &[("sheet", Some((|_, _, _| Ok(JsValue::null())) as boa_engine::native_function::NativeFunctionPointer), None)][..]),
        ("HTMLSlotElement", &[][..]),
    ] {
        let prototype = shared(context).borrow().prototype(name);
        extend_prototype(context, &prototype, &[], accessors)?;
    }
    {
        let slot = shared(context).borrow().prototype("HTMLSlotElement");
        extend_prototype(context, &slot, &[
            ("assignedNodes", 1, |_, _, cx| Ok(array(Vec::new(), cx))),
            ("assignedElements", 1, |_, _, cx| Ok(array(Vec::new(), cx))),
            ("assign", 0, |_, _, _| Ok(JsValue::undefined())),
        ], &[])?;
    }
    // SVG's classes, under one that shapes share.
    let svg = full_class(context, "SVGElement", "Element", &[], SVG_ACCESSORS, &[])?;
    extend_prototype(context, &svg, FOCUS_METHODS, STYLED_ACCESSORS)?;
    full_class(context, "SVGGraphicsElement", "SVGElement", SVG_GRAPHICS_METHODS, &[], &[])?;
    for (_, name) in SVG_CLASSES {
        let parent = if *name == "SVGSVGElement" { "SVGGraphicsElement" } else { "SVGGraphicsElement" };
        plain_class(context, name, parent)?;
    }
    let svg_svg = shared(context).borrow().prototype("SVGSVGElement");
    extend_prototype(context, &svg_svg, SVG_SVG_METHODS, &[])?;
    plain_class(context, "MathMLElement", "Element")?;
    // What an element hands out.
    class(
        context,
        ClassSpec {
            name: "DOMTokenList",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: CLASS_LIST_METHODS,
            accessors: CLASS_LIST_ACCESSORS,
            statics: &[],
            global: true,
        },
    )?;
    {
        // Iterable, as a browser's is: `[...el.classList]`, `for (const c of el.classList)`.
        let prototype = shared(context).borrow().prototype("DOMTokenList");
        let values = closure(
            context,
            "values",
            |this, _, _: &(), cx| {
                let id = list_node(this)?;
                let classes = read_dom(cx, |dom| dom.get(id).map(|n| n.classes.clone()).unwrap_or_default());
                let items: Vec<JsValue> = classes.iter().map(|c| text(c)).collect();
                let list = array(items, cx);
                call_method(&list, "values", &[], cx)
            },
            (),
        );
        for key in [PropertyKey::from(boa_engine::JsSymbol::iterator()), PropertyKey::from(js_string!("values"))] {
            prototype.define_property_or_throw(
                key,
                PropertyDescriptor::builder().value(values.clone()).writable(true).enumerable(false).configurable(true),
                context,
            )?;
        }
    }
    class(
        context,
        ClassSpec {
            name: "CSSStyleDeclaration",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: STYLE_METHODS,
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    class(
        context,
        ClassSpec {
            name: "DOMStringMap",
            length: 0,
            construct: illegal_constructor,
            parent: None,
            methods: &[],
            accessors: &[],
            statics: &[],
            global: true,
        },
    )?;
    Ok(())
}
