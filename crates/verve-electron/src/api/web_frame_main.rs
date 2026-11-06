//! `WebFrameMain`, as far as IPC needs it: a page's main frame, which can be sent messages and
//! has an `ipc` of its own. A page has no other frame in v1. P13 gives it the rest of its members
//! in this file.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, ClassSpec, NewOnly};

use super::contents_ipc;
use super::define::{self, Kind};
use crate::ids::ContentsId;
use crate::ipc::{self, MAIN_FRAME};

/// The kind of object a page's main frame is, in [`define`]'s table.
pub(crate) const FRAME: Kind = Kind("WebFrameMain");

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("send", 1, |this, args, cx| {
        // A frame whose page is gone takes a message and drops it.
        if let Some(id) = open_frame(this, cx) {
            contents_ipc::send(cx, id, args)?;
        }
        Ok(JsValue::undefined())
    }),
    ("postMessage", 2, |this, args, cx| {
        if let Some(id) = open_frame(this, cx) {
            contents_ipc::post(cx, id, args)?;
        }
        Ok(JsValue::undefined())
    }),
];

const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    (
        "processId",
        |this, _, cx| frame_number(this, cx, ipc::process_id),
        None,
    ),
    (
        "routingId",
        |this, _, cx| frame_number(this, cx, |_| MAIN_FRAME),
        None,
    ),
    (
        "frameTreeNodeId",
        |this, _, cx| frame_number(this, cx, |_| MAIN_FRAME),
        None,
    ),
    ("ipc", ipc_of, None),
];

/// The `WebFrameMain` class, which a script never constructs.
pub(crate) fn make_class(context: &mut Context) -> JsResult<JsObject> {
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "WebFrameMain",
            methods: METHODS,
            accessors: ACCESSORS,
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    Ok(class)
}

/// The main frame object of page `id`.
pub(crate) fn make(context: &mut Context, id: ContentsId) -> JsResult<JsObject> {
    let class = super::ipc_main::state(context)?.frame_class.clone();
    let frame = class.construct(&[], None, context)?;
    define::bind(&frame, FRAME, id, context);
    Ok(frame)
}

/// The page `this` frame belongs to, while it has its window.
fn open_frame(this: &JsValue, context: &mut Context) -> Option<ContentsId> {
    let id = define::id_of(this, FRAME, context)?;
    contents_ipc::is_open(context, id).then_some(id)
}

fn frame_number(
    this: &JsValue,
    context: &mut Context,
    number: fn(ContentsId) -> u32,
) -> JsResult<JsValue> {
    let (id, ()) = define::live(this, FRAME, context, |_| Some(()))?;
    Ok(number(id).into())
}

/// `webFrameMain.ipc`.
fn ipc_of(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (id, ()) = define::live(this, FRAME, context, |_| Some(()))?;
    match contents_ipc::page(context, id)? {
        Some(page) => Ok(page.frame_ipc.into()),
        None => Err(define::destroyed()),
    }
}

/// The page whose main frame `frame` is.
pub(crate) fn contents_of(frame: &JsValue, context: &mut Context) -> Option<ContentsId> {
    define::id_of(frame, FRAME, context)
}
