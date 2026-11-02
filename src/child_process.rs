//! Node's `child_process`: programs started from a realm that was granted that, under the
//! permission model's `child` bit.

mod denied;

use boa_engine::{Context, JsResult, JsValue};

/// `require('child_process')`.
pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    denied::module(context)
}
