//! How a request the host handles rejects `fetch`'s promise when it fails.

use boa_engine::{Context, JsValue};

use super::{FailureStyle, NetError};
use crate::network::fetch::{fetch_failed, plain_error};

/// The rejection for a request that failed with `error`, `cause` being what Verve found
/// underneath, in the shape the host's [`FailureStyle`] asks for.
pub(crate) fn failure(
    cx: &mut Context,
    style: FailureStyle,
    cause: JsValue,
    error: &NetError,
) -> JsValue {
    match style {
        FailureStyle::Undici => fetch_failed(cx, cause),
        FailureStyle::Chromium => crate::native::construct_global(
            cx,
            "TypeError",
            &[crate::native::js_text("Failed to fetch")],
        )
        .map(JsValue::from)
        .unwrap_or_default(),
        FailureStyle::Net => plain_error(cx, &error.name, None),
    }
}
