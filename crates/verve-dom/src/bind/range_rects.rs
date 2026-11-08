//! The boxes of a `Range`: `getClientRects` and `getBoundingClientRect`.
//!
//! When the page's layout can say where the text between a range's ends is (it installed a range
//! provider, [`crate::Document::set_range_provider`]), a range has one box for each piece of text
//! on each line it covers, and the bounding box is all of them together. Without one, a range is
//! the box of the element it starts in.

use boa_engine::{Context, JsResult, JsValue};
use verve::native;

use super::element;
use super::node::read_dom;
use crate::Rect;
use crate::layout_provider::fresh;

/// The boxes of the text in the range, or `None` when there is no provider.
fn provided(this: &JsValue, context: &mut Context) -> JsResult<Option<Vec<Rect>>> {
    let data = super::range::data_of(this, context)?;
    let state = fresh(context);
    let hook = state.borrow().range_hook.clone();
    Ok(hook.map(|hook| hook(data.start, data.end)))
}

/// The box of the element the range starts in: what a range answers with no provider.
fn element_box(this: &JsValue, context: &mut Context) -> JsResult<Rect> {
    let id = super::range::data_of(this, context)?.start.0;
    let element = read_dom(context, |dom| {
        if dom.get(id).is_some_and(|n| n.is_element()) {
            id
        } else {
            dom.parent(id).unwrap_or(id)
        }
    });
    Ok(element::painted(context, element))
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
