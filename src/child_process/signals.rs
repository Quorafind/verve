//! Signals by name and number, as `os.constants.signals` has them and `kill` accepts them.

use boa_engine::{Context, JsResult, JsValue};

use crate::events::coded_type_error;
use crate::native::text_of;
use crate::os::SIGNALS;

pub(crate) const SIGKILL: i32 = 9;
pub(crate) const SIGTERM: i32 = 15;

/// The number of the signal `name`, if there is one.
pub(crate) fn number(name: &str) -> Option<i32> {
    SIGNALS
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, number)| *number as i32)
}

/// The name of signal `number`: the first the table has for it.
pub(crate) fn name(number: i32) -> Option<&'static str> {
    SIGNALS
        .iter()
        .find(|(_, known)| *known == i64::from(number))
        .map(|(name, _)| *name)
}

/// A signal as `kill` is given one: a name, or a number the system knows. Anything else is
/// Node's `ERR_UNKNOWN_SIGNAL`.
pub(crate) fn from_js(value: &JsValue, context: &mut Context) -> JsResult<i32> {
    let known = match value.as_number() {
        Some(n) if n.fract() == 0. && (0. ..=64.).contains(&n) => Some(n as i32),
        Some(_) => None,
        None if value.is_string() => number(&text_of(value, context)?),
        None => None,
    };
    known.ok_or_else(|| {
        let shown = text_of(value, context).unwrap_or_default();
        coded_type_error(
            context,
            &format!("Unknown signal: {shown}"),
            "ERR_UNKNOWN_SIGNAL",
        )
    })
}
