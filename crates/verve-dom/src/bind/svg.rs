//! The members SVG elements have beyond `Element`'s: `ownerSVGElement`, `getBBox`, `createSVGPoint`
//! and the rest, answered as a document without a renderer can (zero lengths, identity matrices).

use boa_engine::JsValue;
use verve::native::{self};

use super::element::{painted, rect_object};
use super::node::{read_dom, wrap_opt};
use super::{Accessor, Method, node_of, set};
use crate::Rect;
use crate::dom::Namespace;

pub const SVG_ACCESSORS: &[Accessor] = &[
    (
        "ownerSVGElement",
        Some(|this, _, cx| {
            let id = node_of(this)?;
            let owner = read_dom(cx, |dom| {
                let mut at = dom.parent(id);
                while let Some(node) = at {
                    if dom
                        .get(node)
                        .is_some_and(|n| n.tag() == Some("svg") && n.namespace == Namespace::Svg)
                    {
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

pub const SVG_GRAPHICS_METHODS: &[Method] = &[
    ("getBBox", 0, |this, _, cx| {
        let id = node_of(this)?;
        // The layout measured the drawing: the element's box in the drawing's own pixels.
        let measured = crate::layout_provider::fresh(cx)
            .borrow()
            .svg_boxes
            .get(&id)
            .copied();
        if let Some(rect) = measured {
            return rect_object(cx, rect);
        }
        let rect = painted(cx, id);
        rect_object(
            cx,
            Rect {
                x: 0.,
                y: 0.,
                ..rect
            },
        )
    }),
    ("getCTM", 0, |_, _, cx| super::platform::identity_matrix(cx)),
    ("getScreenCTM", 0, |_, _, cx| {
        super::platform::identity_matrix(cx)
    }),
    ("getTotalLength", 0, |_, _, _| Ok(0.into())),
    ("getPointAtLength", 1, |_, _, cx| {
        let point = native::object(cx);
        set(&point, "x", 0, cx)?;
        set(&point, "y", 0, cx)?;
        Ok(point.into())
    }),
];

pub const SVG_SVG_METHODS: &[Method] = &[
    ("createSVGPoint", 0, |_, _, cx| {
        super::platform::new_point(cx)
    }),
    ("createSVGMatrix", 0, |_, _, cx| {
        super::platform::identity_matrix(cx)
    }),
    ("suspendRedraw", 1, |_, _, _| Ok(0.into())),
    ("unsuspendRedraw", 1, |_, _, _| Ok(JsValue::undefined())),
    ("forceRedraw", 0, |_, _, _| Ok(JsValue::undefined())),
];
