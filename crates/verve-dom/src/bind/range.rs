//! `Range`, `StaticRange` and `Selection`, as views of the live ranges the arena keeps
//! ([`crate::dom::Ranges`]).
//!
//! A `Range` object holds a slot of that table and gives it back when it is collected. Its
//! boundary points are the arena's, so they move as the tree changes, whoever changes it. The
//! window's one `Selection` reads the arena's selection; `selectionchange` is queued when it
//! moves, once for any number of moves before the page next gets to run.

use std::sync::{Arc, Mutex};

use boa_engine::{Context, Finalize, JsData, JsObject, JsResult, JsValue, Trace, js_string};
use verve::native::{self, arg, arg_text, hidden, js_text, type_error};

use super::node::{read_dom, with_dom, wrap};
use super::{Accessor, Kind, Method, dom_error, native_instance, node_arg, node_of, set};
use crate::dom::{Contents, Direction, Dom, NodeId, NodeKind, RangeData, RangeError};
use crate::layout_provider::Boundary;
use crate::selection::Granularity;
use crate::state::{class_prototype, shared};

/// What a `Range` object holds: its slot in the arena's table.
#[derive(Trace, JsData)]
pub struct RangeHandle {
    #[unsafe_ignore_trace]
    slot: u32,
    #[unsafe_ignore_trace]
    queue: Arc<Mutex<Vec<u32>>>,
}

impl Finalize for RangeHandle {
    fn finalize(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(self.slot);
        }
    }
}

/// A `Range` object for `slot`, taking over the reference `slot` was allocated with.
fn range_object(context: &mut Context, slot: u32, prototype: JsObject) -> JsObject {
    let queue = shared(context).borrow().dom.ranges.release_queue();
    JsObject::from_proto_and_data(prototype, RangeHandle { slot, queue })
}

/// A new `Range` holding `data`.
pub fn range_of(context: &mut Context, data: RangeData) -> JsValue {
    let slot = with_dom(context, |dom| dom.new_range_of(data));
    let prototype = class_prototype(context, "Range");
    range_object(context, slot, prototype).into()
}

/// A new `Range`, collapsed at the start of the document.
pub fn new_range(context: &mut Context) -> JsResult<JsValue> {
    let slot = with_dom(context, Dom::new_range);
    let prototype = class_prototype(context, "Range");
    Ok(range_object(context, slot, prototype).into())
}

fn construct_range(
    new_target: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let prototype = super::prototype_of_target(new_target, context)?;
    let slot = with_dom(context, Dom::new_range);
    Ok(range_object(context, slot, prototype).into())
}

fn slot_of(value: &JsValue) -> JsResult<u32> {
    value
        .as_object()
        .and_then(|o| o.downcast_ref::<RangeHandle>().map(|h| h.slot))
        .ok_or_else(|| type_error("Illegal invocation: not a Range"))
}

/// The two ends of the range `this`.
pub(super) fn data_of(this: &JsValue, context: &Context) -> JsResult<RangeData> {
    let slot = slot_of(this)?;
    read_dom(context, |dom| dom.range(slot)).ok_or_else(|| type_error("The range is gone"))
}

fn fail(context: &mut Context, error: RangeError) -> boa_engine::JsError {
    dom_error(
        context,
        &format!("The range operation failed: {}", error.name()),
        error.name(),
    )
}

fn point_args(args: &[JsValue], at: usize, method: &str, cx: &mut Context) -> JsResult<Boundary> {
    let node = node_arg(args, at, method, "Range")?;
    let offset = arg(args, at + 1).to_u32(cx)? as usize;
    Ok((node, offset))
}

fn set_end(this: &JsValue, args: &[JsValue], start: bool, cx: &mut Context) -> JsResult<JsValue> {
    let slot = slot_of(this)?;
    let method = if start { "setStart" } else { "setEnd" };
    let point = point_args(args, 0, method, cx)?;
    with_dom(cx, |dom| dom.set_range_end(slot, point, start)).map_err(|e| fail(cx, e))?;
    Ok(JsValue::undefined())
}

fn set_beside(
    this: &JsValue,
    args: &[JsValue],
    start: bool,
    after: bool,
    cx: &mut Context,
) -> JsResult<JsValue> {
    let slot = slot_of(this)?;
    let node = node_arg(args, 0, "setBeside", "Range")?;
    let point = read_dom(cx, |dom| {
        if after {
            dom.after_node(node)
        } else {
            dom.before_node(node)
        }
    });
    let Some(point) = point else {
        return Err(fail(cx, RangeError::InvalidNodeType));
    };
    with_dom(cx, |dom| dom.set_range_end(slot, point, start)).map_err(|e| fail(cx, e))?;
    Ok(JsValue::undefined())
}

fn select(this: &JsValue, args: &[JsValue], contents: bool, cx: &mut Context) -> JsResult<JsValue> {
    let slot = slot_of(this)?;
    let node = node_arg(args, 0, "selectNode", "Range")?;
    with_dom(cx, |dom| dom.select_range_of(slot, node, contents)).map_err(|e| fail(cx, e))?;
    Ok(JsValue::undefined())
}

fn contents(this: &JsValue, mode: Contents, cx: &mut Context) -> JsResult<JsValue> {
    let slot = slot_of(this)?;
    let fragment = with_dom(cx, |dom| {
        let data = dom.range(slot)?;
        // Cloning and extracting refuse a document type; deleting just removes it.
        if mode != Contents::Delete
            && let Err(error) = dom.check_contents(data)
        {
            return Some(Err(error));
        }
        let (fragment, after) = dom.copy_contents(data, mode);
        if mode != Contents::Clone {
            dom.collapse_range(slot, after.start);
        }
        Some(Ok(fragment))
    });
    let fragment = fragment.transpose().map_err(|e| fail(cx, e))?;
    Ok(match (fragment, mode) {
        (Some(fragment), Contents::Delete) => {
            with_dom(cx, |dom| dom.empty(fragment));
            JsValue::undefined()
        }
        (Some(fragment), _) => wrap(cx, fragment),
        (None, _) => JsValue::undefined(),
    })
}

fn compare_to(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let point = point_args(args, 0, "comparePoint", cx)?;
    let range = data_of(this, cx)?;
    match read_dom(cx, |dom| dom.compare_point(range, point)) {
        Ok(n) => Ok(n.into()),
        Err(e) => Err(fail(cx, e)),
    }
}

const RANGE_METHODS: &[Method] = &[
    ("setStart", 2, |this, args, cx| {
        set_end(this, args, true, cx)
    }),
    ("setEnd", 2, |this, args, cx| set_end(this, args, false, cx)),
    ("setStartBefore", 1, |this, args, cx| {
        set_beside(this, args, true, false, cx)
    }),
    ("setStartAfter", 1, |this, args, cx| {
        set_beside(this, args, true, true, cx)
    }),
    ("setEndBefore", 1, |this, args, cx| {
        set_beside(this, args, false, false, cx)
    }),
    ("setEndAfter", 1, |this, args, cx| {
        set_beside(this, args, false, true, cx)
    }),
    ("selectNode", 1, |this, args, cx| {
        select(this, args, false, cx)
    }),
    ("selectNodeContents", 1, |this, args, cx| {
        select(this, args, true, cx)
    }),
    ("collapse", 0, |this, args, cx| {
        let slot = slot_of(this)?;
        let to_start = arg(args, 0).to_boolean();
        with_dom(cx, |dom| {
            if let Some(r) = dom.range(slot) {
                dom.collapse_range(slot, if to_start { r.start } else { r.end });
            }
        });
        Ok(JsValue::undefined())
    }),
    ("cloneRange", 0, |this, _, cx| {
        let data = data_of(this, cx)?;
        Ok(range_of(cx, data))
    }),
    ("compareBoundaryPoints", 2, |this, args, cx| {
        let how = arg(args, 0).to_u32(cx)? as u16;
        let this_data = data_of(this, cx)?;
        let other = data_of(&arg(args, 1), cx)?;
        if how > 3 {
            return Err(dom_error(
                cx,
                "The comparison method provided is not supported.",
                "NotSupportedError",
            ));
        }
        match read_dom(cx, |dom| dom.compare_range_points(this_data, how, other)) {
            Ok(n) => Ok(n.into()),
            Err(e) => Err(fail(cx, e)),
        }
    }),
    ("comparePoint", 2, |this, args, cx| {
        compare_to(this, args, cx)
    }),
    ("isPointInRange", 2, |this, args, cx| {
        let point = point_args(args, 0, "isPointInRange", cx)?;
        let range = data_of(this, cx)?;
        match read_dom(cx, |dom| dom.compare_point(range, point)) {
            Ok(found) => Ok((found == 0).into()),
            Err(RangeError::WrongDocument) => Ok(false.into()),
            Err(error) => Err(fail(cx, error)),
        }
    }),
    ("intersectsNode", 1, |this, args, cx| {
        let node = node_arg(args, 0, "intersectsNode", "Range")?;
        let range = data_of(this, cx)?;
        Ok(read_dom(cx, |dom| dom.range_intersects(range, node)).into())
    }),
    ("deleteContents", 0, |this, _, cx| {
        contents(this, Contents::Delete, cx)
    }),
    ("extractContents", 0, |this, _, cx| {
        contents(this, Contents::Extract, cx)
    }),
    ("cloneContents", 0, |this, _, cx| {
        contents(this, Contents::Clone, cx)
    }),
    ("insertNode", 1, |this, args, cx| {
        let slot = slot_of(this)?;
        let node = node_arg(args, 0, "insertNode", "Range")?;
        with_dom(cx, |dom| dom.insert_at(slot, node)).map_err(|e| fail(cx, e))?;
        Ok(JsValue::undefined())
    }),
    ("surroundContents", 1, |this, args, cx| {
        let slot = slot_of(this)?;
        let node = node_arg(args, 0, "surroundContents", "Range")?;
        with_dom(cx, |dom| dom.surround(slot, node)).map_err(|e| fail(cx, e))?;
        Ok(JsValue::undefined())
    }),
    ("toString", 0, |this, _, cx| {
        let range = data_of(this, cx)?;
        Ok(js_text(&read_dom(cx, |dom| dom.range_text(range))))
    }),
    ("getBoundingClientRect", 0, |this, _, cx| {
        super::range_rects::bounding(this, cx)
    }),
    ("getClientRects", 0, |this, _, cx| {
        super::range_rects::client_rects(this, cx)
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
    ("detach", 0, |_, _, _| Ok(JsValue::undefined())),
];

fn container(this: &JsValue, start: bool, cx: &mut Context) -> JsResult<JsValue> {
    let data = data_of(this, cx)?;
    Ok(wrap(cx, if start { data.start.0 } else { data.end.0 }))
}

const RANGE_ACCESSORS: &[Accessor] = &[
    (
        "startContainer",
        Some(|this, _, cx| container(this, true, cx)),
        None,
    ),
    (
        "endContainer",
        Some(|this, _, cx| container(this, false, cx)),
        None,
    ),
    (
        "startOffset",
        Some(|this, _, cx| Ok((data_of(this, cx)?.start.1 as u32).into())),
        None,
    ),
    (
        "endOffset",
        Some(|this, _, cx| Ok((data_of(this, cx)?.end.1 as u32).into())),
        None,
    ),
    (
        "collapsed",
        Some(|this, _, cx| {
            let data = data_of(this, cx)?;
            Ok((data.start == data.end).into())
        }),
        None,
    ),
    (
        "commonAncestorContainer",
        Some(|this, _, cx| {
            let data = data_of(this, cx)?;
            let common = read_dom(cx, |dom| dom.common_ancestor(data.start.0, data.end.0));
            Ok(wrap(cx, common))
        }),
        None,
    ),
];

// ─── StaticRange ────────────────────────────────────────────────────────────────────────────

/// A `StaticRange` for `data`: its ends as they are now, which nothing moves afterwards.
pub fn static_range(context: &mut Context, data: RangeData) -> JsResult<JsValue> {
    let made = native_instance(context, "StaticRange", Kind::Plain, 0);
    for (key, (node, offset)) in [("start", data.start), ("end", data.end)] {
        let value = wrap(context, node);
        set(&made, &format!("{key}Container"), value, context)?;
        set(&made, &format!("{key}Offset"), offset as u32, context)?;
    }
    set(&made, "collapsed", data.start == data.end, context)?;
    Ok(made.into())
}

fn construct_static_range(
    new_target: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let init = arg(args, 0);
    let Some(init) = init.as_object() else {
        return Err(type_error(
            "Failed to construct 'StaticRange': 1 argument required.",
        ));
    };
    let mut points = Vec::new();
    for key in ["start", "end"] {
        let node = init.get(js_string!(format!("{key}Container")), context)?;
        let offset = init
            .get(js_string!(format!("{key}Offset")), context)?
            .to_u32(context)?;
        points.push((node_of(&node)?, offset as usize));
    }
    if read_dom(context, |dom| {
        points
            .iter()
            .any(|p| matches!(dom.get(p.0).map(|n| &n.kind), Some(NodeKind::Doctype(_))))
    }) {
        return Err(dom_error(
            context,
            "The node is a doctype.",
            "InvalidNodeTypeError",
        ));
    }
    let made = static_range(
        context,
        RangeData {
            start: points[0],
            end: points[1],
        },
    )?;
    if let (Some(object), Ok(prototype)) = (
        made.as_object(),
        super::prototype_of_target(new_target, context),
    ) {
        object.set_prototype(Some(prototype));
    }
    Ok(made)
}

// ─── Selection ──────────────────────────────────────────────────────────────────────────────

/// The window's one selection.
pub fn selection(context: &mut Context) -> JsResult<JsValue> {
    let global = context.global_object();
    let key = js_string!("__selection");
    let known = global.get(key.clone(), context)?;
    if known.is_object() {
        return Ok(known);
    }
    let made = native_instance(context, "Selection", Kind::Plain, 0);
    hidden(&global, key, made.clone(), context)?;
    Ok(made.into())
}

/// The `Range` object of the selection's range: the one `addRange` was given, or a new one that
/// follows the selection's slot.
fn selection_range(this: &JsValue, cx: &mut Context) -> JsResult<Option<JsValue>> {
    let Some(slot) = read_dom(cx, |dom| dom.ranges.selection_slot()) else {
        return Ok(None);
    };
    let cached = verve::native::get(this, "__range", cx)?;
    if slot_of(&cached).is_ok_and(|s| s == slot) {
        return Ok(Some(cached));
    }
    with_dom(cx, |dom| dom.ranges.retain(slot));
    let prototype = class_prototype(cx, "Range");
    let object: JsValue = range_object(cx, slot, prototype).into();
    if let Some(this) = this.as_object() {
        hidden(&this, js_string!("__range"), object.clone(), cx)?;
    }
    Ok(Some(object))
}

fn in_document(dom: &Dom, node: NodeId) -> bool {
    dom.root_of(node) == dom.document
}

fn checked(cx: &mut Context, point: Boundary) -> JsResult<()> {
    read_dom(cx, |dom| dom.check_boundary(point)).map_err(|e| fail(cx, e))
}

fn selection_state(cx: &Context) -> Option<(RangeData, Direction)> {
    read_dom(cx, Dom::selection)
}

fn require_selection(cx: &mut Context) -> JsResult<(RangeData, Direction)> {
    selection_state(cx)
        .ok_or_else(|| dom_error(cx, "The Selection has no range.", "InvalidStateError"))
}

fn anchor_focus(cx: &mut Context, anchor: bool, node: bool) -> JsResult<JsValue> {
    let at = read_dom(cx, |dom| {
        if anchor {
            dom.selection_anchor()
        } else {
            dom.selection_focus()
        }
    });
    Ok(match (at, node) {
        (Some((n, _)), true) => wrap(cx, n),
        (Some((_, o)), false) => (o as u32).into(),
        (None, true) => JsValue::null(),
        (None, false) => 0.into(),
    })
}

const SELECTION_ACCESSORS: &[Accessor] = &[
    (
        "rangeCount",
        Some(|_, _, cx| Ok(u32::from(selection_state(cx).is_some()).into())),
        None,
    ),
    (
        "isCollapsed",
        Some(|_, _, cx| {
            Ok(selection_state(cx)
                .is_none_or(|(r, _)| r.start == r.end)
                .into())
        }),
        None,
    ),
    (
        "type",
        Some(|_, _, cx| {
            Ok(js_text(match selection_state(cx) {
                None => "None",
                Some((r, _)) if r.start == r.end => "Caret",
                Some(_) => "Range",
            }))
        }),
        None,
    ),
    (
        "direction",
        Some(|_, _, cx| {
            Ok(js_text(match selection_state(cx) {
                Some((r, d)) if r.start != r.end => match d {
                    Direction::Forward => "forward",
                    Direction::Backward => "backward",
                },
                _ => "none",
            }))
        }),
        None,
    ),
    (
        "anchorNode",
        Some(|_, _, cx| anchor_focus(cx, true, true)),
        None,
    ),
    (
        "anchorOffset",
        Some(|_, _, cx| anchor_focus(cx, true, false)),
        None,
    ),
    (
        "focusNode",
        Some(|_, _, cx| anchor_focus(cx, false, true)),
        None,
    ),
    (
        "focusOffset",
        Some(|_, _, cx| anchor_focus(cx, false, false)),
        None,
    ),
];

/// The granularity and direction `modify` was asked for.
fn modify_args(args: &[JsValue], cx: &mut Context) -> JsResult<Option<(bool, bool, Granularity)>> {
    let alter = arg_text(args, 0, cx)?.to_ascii_lowercase();
    let direction = arg_text(args, 1, cx)?.to_ascii_lowercase();
    let unit = arg_text(args, 2, cx)?.to_ascii_lowercase();
    let forward = match direction.as_str() {
        "forward" | "right" => true,
        "backward" | "left" => false,
        _ => return Ok(None),
    };
    let unit = match unit.as_str() {
        "character" => Granularity::Character,
        "word" => Granularity::Word,
        "line" | "lineboundary" => Granularity::LineBoundary,
        "sentence" | "sentenceboundary" | "paragraph" | "paragraphboundary" => {
            Granularity::Paragraph
        }
        "documentboundary" => Granularity::Document,
        _ => return Ok(None),
    };
    Ok(match alter.as_str() {
        "move" => Some((false, forward, unit)),
        "extend" => Some((true, forward, unit)),
        _ => None,
    })
}

const SELECTION_METHODS: &[Method] = &[
    ("getRangeAt", 1, |this, args, cx| {
        let at = arg(args, 0).to_u32(cx)?;
        match selection_range(this, cx)? {
            Some(range) if at == 0 => Ok(range),
            _ => Err(dom_error(
                cx,
                "Failed to execute 'getRangeAt' on 'Selection': The index provided is out of range.",
                "IndexSizeError",
            )),
        }
    }),
    ("addRange", 1, |this, args, cx| {
        let slot = slot_of(&arg(args, 0))?;
        let data = data_of(&arg(args, 0), cx)?;
        let take = read_dom(cx, |dom| {
            dom.ranges.selection_slot().is_none() && in_document(dom, data.start.0)
        });
        if take {
            with_dom(cx, |dom| dom.ranges.select(Some(slot), Direction::Forward));
            if let Some(this) = this.as_object() {
                hidden(&this, js_string!("__range"), arg(args, 0), cx)?;
            }
        }
        Ok(JsValue::undefined())
    }),
    ("removeRange", 1, |_, args, cx| {
        let slot = slot_of(&arg(args, 0))?;
        if read_dom(cx, |dom| dom.ranges.selection_slot()) != Some(slot) {
            return Err(dom_error(
                cx,
                "The range is not in the selection.",
                "NotFoundError",
            ));
        }
        with_dom(cx, Dom::clear_selection);
        Ok(JsValue::undefined())
    }),
    ("removeAllRanges", 0, |_, _, cx| {
        with_dom(cx, Dom::clear_selection);
        Ok(JsValue::undefined())
    }),
    ("empty", 0, |_, _, cx| {
        with_dom(cx, Dom::clear_selection);
        Ok(JsValue::undefined())
    }),
    ("collapse", 2, |_, args, cx| collapse(args, cx)),
    ("setPosition", 2, |_, args, cx| collapse(args, cx)),
    ("collapseToStart", 0, |_, _, cx| {
        let (r, _) = require_selection(cx)?;
        with_dom(cx, |dom| dom.select_between(r.start, r.start));
        Ok(JsValue::undefined())
    }),
    ("collapseToEnd", 0, |_, _, cx| {
        let (r, _) = require_selection(cx)?;
        with_dom(cx, |dom| dom.select_between(r.end, r.end));
        Ok(JsValue::undefined())
    }),
    ("extend", 2, |_, args, cx| {
        let (_, _) = require_selection(cx)?;
        let point = point_args(args, 0, "extend", cx)?;
        checked(cx, point)?;
        with_dom(cx, |dom| {
            if !in_document(dom, point.0) {
                return;
            }
            let anchor = dom.selection_anchor().unwrap_or(point);
            if dom.root_of(anchor.0) == dom.root_of(point.0) {
                dom.select_between(anchor, point);
            } else {
                dom.select_between(point, point);
            }
        });
        Ok(JsValue::undefined())
    }),
    ("setBaseAndExtent", 4, |_, args, cx| {
        let (anchor, focus) = (
            point_args(args, 0, "setBaseAndExtent", cx)?,
            point_args(args, 2, "setBaseAndExtent", cx)?,
        );
        checked(cx, anchor)?;
        checked(cx, focus)?;
        with_dom(cx, |dom| {
            if in_document(dom, anchor.0) && in_document(dom, focus.0) {
                dom.select_between(anchor, focus);
            }
        });
        Ok(JsValue::undefined())
    }),
    ("selectAllChildren", 1, |_, args, cx| {
        let node = node_arg(args, 0, "selectAllChildren", "Selection")?;
        if read_dom(cx, |dom| {
            matches!(dom.get(node).map(|n| &n.kind), Some(NodeKind::Doctype(_)))
        }) {
            return Err(fail(cx, RangeError::InvalidNodeType));
        }
        with_dom(cx, |dom| {
            let end = (node, dom.node_length(node));
            if in_document(dom, node) {
                dom.select_between((node, 0), end);
            }
        });
        Ok(JsValue::undefined())
    }),
    ("deleteFromDocument", 0, |_, _, cx| {
        with_dom(cx, |dom| {
            if let Some((r, _)) = dom.selection()
                && r.start != r.end
            {
                let (fragment, after) = dom.copy_contents(r, Contents::Delete);
                dom.empty(fragment);
                dom.select_between(after.start, after.start);
            }
        });
        Ok(JsValue::undefined())
    }),
    ("containsNode", 2, |_, args, cx| {
        let node = node_arg(args, 0, "containsNode", "Selection")?;
        let partial = arg(args, 1).to_boolean();
        let found = read_dom(cx, |dom| {
            let (r, _) = dom.selection()?;
            if dom.root_of(node) != dom.root_of(r.start.0) {
                return Some(false);
            }
            Some(if partial {
                dom.range_intersects(r, node)
            } else {
                dom.range_contains(r, node)
            })
        });
        Ok(found.unwrap_or(false).into())
    }),
    ("modify", 3, |_, args, cx| {
        if let Some((extend, forward, unit)) = modify_args(args, cx)? {
            with_dom(cx, |dom| dom.modify_selection(extend, forward, unit));
        }
        Ok(JsValue::undefined())
    }),
    ("getComposedRanges", 0, |_, _, cx| {
        let items = match selection_state(cx) {
            Some((r, _)) => vec![static_range(cx, r)?],
            None => Vec::new(),
        };
        Ok(native::array(cx, items))
    }),
    ("toString", 0, |_, _, cx| {
        Ok(js_text(&read_dom(cx, Dom::selection_text)))
    }),
];

fn collapse(args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if arg(args, 0).is_null_or_undefined() {
        with_dom(cx, Dom::clear_selection);
        return Ok(JsValue::undefined());
    }
    let point = point_args(args, 0, "collapse", cx)?;
    checked(cx, point)?;
    with_dom(cx, |dom| {
        if in_document(dom, point.0) {
            dom.select_between(point, point);
        }
    });
    Ok(JsValue::undefined())
}

// ─── selectionchange ────────────────────────────────────────────────────────────────────────

/// Queues `selectionchange` if the selection moved since the page last heard of it; any number of
/// moves before the page runs again make one event.
pub(super) fn announce(context: &mut Context) {
    let state = shared(context);
    {
        let mut state = state.borrow_mut();
        if state.selection_queued || state.dom.ranges.revision == state.selection_seen {
            return;
        }
        state.selection_queued = true;
    }
    super::queue_microtask(context, "selectionchange", |_, _, cx| {
        let state = shared(cx);
        {
            let mut state = state.borrow_mut();
            state.selection_queued = false;
            state.selection_seen = state.dom.ranges.revision;
        }
        let document = read_dom(cx, |dom| dom.document);
        super::events::fire_at(
            cx,
            document,
            "Event",
            "selectionchange",
            &[("bubbles", false.into())],
            true,
        )?;
        Ok(JsValue::undefined())
    });
}

/// The two ends of the selection's range, for a drawer.
pub(crate) fn selection_bounds(context: &Context) -> Option<(Boundary, Boundary)> {
    selection_state(context).map(|(r, _)| (r.start, r.end))
}

pub fn install(context: &mut Context) -> JsResult<()> {
    let _ = super::platform::simple_class(
        context,
        "Range",
        construct_range,
        None,
        RANGE_METHODS,
        RANGE_ACCESSORS,
        true,
    )?;
    let _ = super::platform::simple_class(
        context,
        "StaticRange",
        construct_static_range,
        None,
        &[],
        &[],
        true,
    )?;
    let _ = super::platform::simple_class(
        context,
        "Selection",
        native::illegal_constructor,
        None,
        SELECTION_METHODS,
        SELECTION_ACCESSORS,
        true,
    )?;
    Ok(())
}
