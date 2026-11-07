//! The boxes of a `Range`: `getClientRects` and `getBoundingClientRect`.
//!
//! When the page's layout can say where the text between a range's ends is (it installed a range
//! provider, [`crate::Document::set_range_provider`]), a range has one box for each piece of text
//! on each line it covers, and the bounding box is all of them together. Without one, a range is
//! the box of the element it starts in.

use boa_engine::{Context, JsResult, JsValue};
use verve::native;

use super::node::read_dom;
use super::platform::boundary;
use super::{element, node_id};
use crate::Rect;
use crate::layout_provider::{Boundary, fresh};

/// The boxes of the text in the range, or `None` when there is no provider or the range's ends
/// are not nodes.
fn provided(this: &JsValue, context: &mut Context) -> JsResult<Option<Vec<Rect>>> {
    let (start, start_offset) = boundary(this, context, "start")?;
    let (end, end_offset) = boundary(this, context, "end")?;
    let (Some(start), Some(end)) = (node_id(&start), node_id(&end)) else {
        return Ok(None);
    };
    let state = fresh(context);
    let hook = state.borrow().range_hook.clone();
    let from: Boundary = (start, start_offset as usize);
    let to: Boundary = (end, end_offset as usize);
    Ok(hook.map(|hook| hook(from, to)))
}

/// The box of the element the range starts in: what a range answers with no provider.
fn element_box(this: &JsValue, context: &mut Context) -> JsResult<Rect> {
    let (container, _) = boundary(this, context, "start")?;
    let element = read_dom(context, |dom| {
        node_id(&container).map(|id| {
            if dom.get(id).is_some_and(|n| n.is_element()) {
                id
            } else {
                dom.parent(id).unwrap_or(id)
            }
        })
    });
    Ok(element.map_or_else(Rect::default, |id| element::painted(context, id)))
}

pub(super) fn client_rects(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let rects = match provided(this, context)? {
        Some(rects) => rects,
        None => vec![element_box(this, context)?],
    };
    let items = rects
        .into_iter()
        .map(|rect| element::rect_object(context, rect))
        .collect::<JsResult<Vec<_>>>()?;
    Ok(native::array(context, items))
}

pub(super) fn bounding(this: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let rect = match provided(this, context)? {
        Some(rects) => rects
            .into_iter()
            .reduce(|all, rect| all.union(&rect))
            .unwrap_or_default(),
        None => element_box(this, context)?,
    };
    element::rect_object(context, rect)
}
