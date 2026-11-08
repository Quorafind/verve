//! `scrollIntoView`'s argument: `true` or `false`, or an options object whose bad values are a
//! `TypeError`, as WebIDL has it.

use boa_engine::{Context, JsError, JsResult, JsValue};
use verve::native::{get, text_of, type_error};

use crate::Align;

/// `scrollIntoView`'s argument: `true` (the default) lines the top up, `false` the bottom, an
/// options object says `block`, `inline` and `behavior`.
pub fn reveal_options(given: &JsValue, context: &mut Context) -> JsResult<(Align, Align, bool)> {
    if given.as_object().is_none() {
        let top = given.is_undefined() || given.to_boolean();
        let block = if top { Align::Start } else { Align::End };
        return Ok((block, Align::Nearest, false));
    }
    let block = reveal_align(get(given, "block", context)?, Align::Start, context)?;
    let inline = reveal_align(get(given, "inline", context)?, Align::Nearest, context)?;
    let behavior = get(given, "behavior", context)?;
    let smooth = match behavior_name(&behavior, context)?.as_deref() {
        Some("smooth") => true,
        Some("auto" | "instant") | None => false,
        Some(other) => return Err(reveal_error(other, "ScrollBehavior")),
    };
    Ok((block, inline, smooth))
}

/// `block` or `inline` of `scrollIntoView`'s options, `default` when absent.
fn reveal_align(value: JsValue, default: Align, context: &mut Context) -> JsResult<Align> {
    Ok(match behavior_name(&value, context)?.as_deref() {
        None => default,
        Some("start") => Align::Start,
        Some("center") => Align::Center,
        Some("end") => Align::End,
        Some("nearest") => Align::Nearest,
        Some(other) => return Err(reveal_error(other, "ScrollLogicalPosition")),
    })
}

/// The string an option holds, `None` when it was left out.
fn behavior_name(value: &JsValue, context: &mut Context) -> JsResult<Option<String>> {
    match value.is_undefined() {
        true => Ok(None),
        false => text_of(value, context).map(Some),
    }
}

fn reveal_error(given: &str, kind: &str) -> JsError {
    type_error(format!(
        "Failed to execute 'scrollIntoView' on 'Element': The provided value '{given}' is not a valid enum value of type {kind}."
    ))
}
