//! Where text is under a point, and where the selection is: `document.caretRangeFromPoint`,
//! `document.caretPositionFromPoint` and what a drawer reads of the window's selection.
//!
//! The text position under a point is the page's layout's to answer (see
//! [`crate::Document::set_position_provider`]); without one there is none.

use boa_engine::{Context, JsResult, JsValue, js_string};
use verve::native::{self, arg};

use super::node::wrap;
use super::platform::{boundary, new_range, selection_ranges, set_boundary};
use super::{node_id, set};
use crate::Boundary;
use crate::layout_provider::fresh;

/// The text position nearest `(x, y)` of the viewport, from the layout brought up to date.
fn position_at(context: &mut Context, args: &[JsValue]) -> JsResult<Option<Boundary>> {
    let x = arg(args, 0).to_number(context)? as f32;
    let y = arg(args, 1).to_number(context)? as f32;
    let hook = fresh(context).borrow().position_hook.clone();
    Ok(hook.and_then(|hook| hook(x, y)))
}

/// `document.caretRangeFromPoint(x, y)`: a collapsed `Range` at the text position, `null` where
/// there is no text to be near.
pub(super) fn range_from_point(context: &mut Context, args: &[JsValue]) -> JsResult<JsValue> {
    let Some((node, offset)) = position_at(context, args)? else {
        return Ok(JsValue::null());
    };
    let range = new_range(context)?;
    let container = wrap(context, node);
    for which in ["start", "end"] {
        set_boundary(&range, context, which, container.clone(), offset as u32)?;
    }
    Ok(range)
}

/// `document.caretPositionFromPoint(x, y)`: a `CaretPosition` (`offsetNode` and `offset`), `null`
/// where there is no text to be near.
pub(super) fn position_from_point(context: &mut Context, args: &[JsValue]) -> JsResult<JsValue> {
    let Some((node, offset)) = position_at(context, args)? else {
        return Ok(JsValue::null());
    };
    let position = native::object(context);
    let container = wrap(context, node);
    set(&position, "offsetNode", container, context)?;
    set(&position, "offset", offset as u32, context)?;
    Ok(position.into())
}

/// The two ends of the first range of the window's selection; `None` when it has none.
pub(crate) fn selection_bounds(context: &mut Context) -> JsResult<Option<(Boundary, Boundary)>> {
    let global = context.global_object();
    let known = global.get(js_string!("__selection"), context)?;
    if !known.is_object() {
        return Ok(None);
    }
    let Some(range) = selection_ranges(&known, context)?.into_iter().next() else {
        return Ok(None);
    };
    let (start, start_offset) = boundary(&range, context, "start")?;
    let (end, end_offset) = boundary(&range, context, "end")?;
    let (Some(start), Some(end)) = (node_id(&start), node_id(&end)) else {
        return Ok(None);
    };
    Ok(Some((
        (start, start_offset as usize),
        (end, end_offset as usize),
    )))
}
