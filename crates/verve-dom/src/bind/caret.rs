//! Where text is under a point: `document.caretRangeFromPoint` and
//! `document.caretPositionFromPoint`. The window's selection is in `range.rs`.
//!
//! The text position under a point is the page's layout's to answer (see
//! [`crate::Document::set_position_provider`]); without one there is none.

use boa_engine::{Context, JsResult, JsValue};
use verve::native::{self, arg};

use super::node::wrap;
use super::set;
use crate::Boundary;
use crate::dom::RangeData;
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
    Ok(super::range::range_of(
        context,
        RangeData {
            start: (node, offset),
            end: (node, offset),
        },
    ))
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
