//! `app.commandLine`: the switches the process was started with and the ones the app adds.
//!
//! Chromium keeps a switch's name without its dashes, and a switch with no value answers the
//! empty string. Names are matched exactly (`hasSwitch('--foo')` is false). A few switches
//! mean something here when they are appended before `ready` (`lang`); the rest are only
//! remembered, as Electron remembers the ones for the parts of Chromium Verve does not have.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg, js_text};

use super::shared;

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("appendSwitch", 1, append_switch),
    ("appendArgument", 1, |_, _, _| Ok(JsValue::undefined())),
    ("hasSwitch", 1, has_switch),
    ("getSwitchValue", 1, get_switch_value),
    ("removeSwitch", 1, remove_switch),
];

/// The `commandLine` object.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let object = native::object(context);
    native::methods(context, &object, METHODS)?;
    Ok(object)
}

fn name_of(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    native::text_of(&arg(args, 0), context)
}

fn append_switch(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, context)?;
    let value = match arg(args, 1) {
        value if value.is_undefined() => String::new(),
        value => native::text_of(&value, context)?,
    };
    shared(context)
        .app
        .switches
        .borrow_mut()
        .push((name, value));
    Ok(JsValue::undefined())
}

fn has_switch(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, context)?;
    let found = shared(context)
        .app
        .switches
        .borrow()
        .iter()
        .any(|(n, _)| *n == name);
    Ok(found.into())
}

/// The value of the last switch of that name: Chromium's, where a later one wins.
fn get_switch_value(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, context)?;
    let state = shared(context);
    let switches = state.app.switches.borrow();
    let value = switches.iter().rev().find(|(n, _)| *n == name);
    Ok(js_text(value.map_or("", |(_, value)| value)))
}

fn remove_switch(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, context)?;
    shared(context)
        .app
        .switches
        .borrow_mut()
        .retain(|(n, _)| *n != name);
    Ok(JsValue::undefined())
}
