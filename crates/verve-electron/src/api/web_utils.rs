//! `webUtils`: `getPathForFile`, the path on disk a `File` the user picked or dropped has.

use boa_engine::{Context, JsNativeError, JsObject, JsResult, JsString, JsValue};
use verve::native::{self, arg};

/// `require('electron').webUtils`.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let utils = native::object(context);
    native::exports(context, &utils, &[("getPathForFile", 1, get_path_for_file)])?;
    Ok(utils)
}

/// `getPathForFile(file)`: where the host found the file, or `''` for one the page made itself.
fn get_path_for_file(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let file = arg(args, 0);
    if !verve::blob::is_blob(&file, context) {
        return Err(JsNativeError::typ()
            .with_message(
                "getPathForFile expected to receive a File object but one was not provided",
            )
            .into());
    }
    let path = verve::blob::path_of_file(&file, context).unwrap_or_default();
    Ok(JsString::from(path.to_string_lossy().as_ref()).into())
}
