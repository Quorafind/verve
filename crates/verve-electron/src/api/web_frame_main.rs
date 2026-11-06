//! `WebFrameMain`, as far as IPC needs it: a page's main frame, which can be sent messages and
//! has an `ipc` of its own, a URL and a name, and runs scripts. A page has no other frame in v1, so
//! `parent` is `null`, `frames` is empty and `top` is the frame itself.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, ClassSpec, NewOnly};

use std::rc::Rc;

use verve::native::{arg, js_text};

use super::define::{self, Args, Kind};
use super::windows::WindowRecord;
use super::{contents_ipc, shared};
use crate::ids::ContentsId;
use crate::ipc::{self, MAIN_FRAME};

/// The kind of object a page's main frame is, in [`define`]'s table.
pub(crate) const FRAME: Kind = Kind("WebFrameMain");

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("executeJavaScript", 1, |this, args, cx| {
        let (_, record) = live_frame(this, cx)?;
        let code = native::text_of(&arg(args, 0), cx)?;
        shared(cx)
            .calls
            .execute(cx, record.page.contents, (code, 0))
    }),
    ("reload", 0, |this, _, cx| {
        let (_, record) = live_frame(this, cx)?;
        super::navigation::reload(cx, &record)?;
        Ok(JsValue::undefined())
    }),
    ("collectJavaScriptCallStack", 0, |this, _, cx| {
        live_frame(this, cx)?;
        define::rejected(cx, "webFrameMain.collectJavaScriptCallStack")
    }),
    ("isDestroyed", 0, |this, _, cx| {
        let gone = live_frame(this, cx).is_err();
        Ok(gone.into())
    }),
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
        "url",
        |this, _, cx| {
            let (_, record) = live_frame(this, cx)?;
            Ok(js_text(&record.page_state.url.borrow()))
        },
        None,
    ),
    (
        "origin",
        |this, _, cx| {
            let (_, record) = live_frame(this, cx)?;
            Ok(js_text(&origin_of(&record.page_state.url.borrow())))
        },
        None,
    ),
    (
        "name",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(js_text(""))
        },
        None,
    ),
    (
        "top",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(this.clone())
        },
        None,
    ),
    (
        "parent",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(JsValue::null())
        },
        None,
    ),
    (
        "frames",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(native::array(cx, Vec::new()))
        },
        None,
    ),
    (
        "framesInSubtree",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(native::array(cx, vec![this.clone()]))
        },
        None,
    ),
    (
        "frameToken",
        |this, _, cx| {
            let (id, _) = live_frame(this, cx)?;
            Ok(js_text(&frame_token(id)))
        },
        None,
    ),
    (
        "visibilityState",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(js_text("visible"))
        },
        None,
    ),
    (
        "detached",
        |this, _, cx| {
            live_frame(this, cx)?;
            Ok(false.into())
        },
        None,
    ),
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

/// The page a frame belongs to and its window, while a script can use them.
fn live_frame(this: &JsValue, context: &mut Context) -> JsResult<(ContentsId, Rc<WindowRecord>)> {
    let state = shared(context);
    define::live(this, FRAME, context, |id| state.showing(id))
}

/// The page whose main frame `frame` is.
pub(crate) fn contents_of(frame: &JsValue, context: &mut Context) -> Option<ContentsId> {
    define::id_of(frame, FRAME, context)
}

/// What `frameToken` says of the main frame of page `id`.
fn frame_token(id: ContentsId) -> String {
    format!("frame-token-{id}")
}

/// The origin of an address as `frame.origin` shows it: `file://` for a file, `null` for the opaque
/// (`data:`).
fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(url) if url.scheme() == "file" => "file://".to_owned(),
        Ok(url) => url.origin().ascii_serialization(),
        Err(_) => "null".to_owned(),
    }
}

/// `require('electron').webFrameMain`: the frames found by their ids.
pub(crate) fn make_module(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::exports(
        context,
        &module,
        &[
            ("fromId", 2, |_, args, cx| {
                let [process, routing] = Args::new(args).numbers::<2>(cx)?;
                let frame =
                    (routing == f64::from(MAIN_FRAME)).then(|| frame_by_process(cx, process));
                Ok(frame.flatten().map_or(JsValue::undefined(), Into::into))
            }),
            ("fromFrameToken", 2, |_, args, cx| {
                let process = Args::new(args).f64(0, "processId", cx)?;
                let token = Args::new(&[arg(args, 1)]).string(0, "frameToken", cx)?;
                let frame = frame_by_process(cx, process);
                let found = frame.filter(|frame| {
                    let id = contents_of(&frame.clone().into(), cx);
                    id.is_some_and(|id| frame_token(id) == token)
                });
                Ok(found.map_or(JsValue::undefined(), Into::into))
            }),
        ],
    )?;
    Ok(module.into())
}

/// The main frame of the page whose process id is `process`.
fn frame_by_process(context: &mut Context, process: f64) -> Option<JsObject> {
    let windows = shared(context).windows.newest_first();
    let record = windows
        .iter()
        .find(|w| f64::from(ipc::process_id(w.page.contents)) == process)?;
    contents_ipc::page(context, record.page.contents)
        .ok()
        .flatten()
        .map(|page| page.frame)
}
