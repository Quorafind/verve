//! The IPC members of a page's `webContents` object — `send`, `sendToFrame`, `postMessage`, `ipc`
//! and `mainFrame` — and the sending behind them, which `webFrameMain` and `event.reply` share.
//!
//! P13 makes `webContents` a class of its own; its method table takes [`METHODS`] and
//! [`ACCESSORS`] as they are, and keeps [`WEB_CONTENTS`] as the kind of the objects it makes.

use boa_engine::native_function::NativeFunctionPointer;
use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg, js_text};

use super::define::{self, Args, Kind};
use super::ipc_main::{self, console};
use super::message_port_main;
use super::{shared, web_frame_main};
use crate::ids::ContentsId;
use crate::ipc::{self, MAIN_FRAME};
use crate::msg::ToRenderer;

/// The kind of object a page's `webContents` is, in [`define`]'s table.
pub(crate) const WEB_CONTENTS: Kind = Kind("WebContents");

/// The objects a script sees for one page: its `webContents`, its main frame, and the `ipc` each
/// of the two has.
#[derive(Clone)]
pub(crate) struct Contents {
    pub(crate) object: JsObject,
    pub(crate) ipc: JsObject,
    pub(crate) frame: JsObject,
    pub(crate) frame_ipc: JsObject,
}

pub(crate) const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("send", 1, |this, args, cx| {
        let id = open_page(this, cx)?;
        send(cx, id, args)?;
        Ok(JsValue::undefined())
    }),
    ("sendToFrame", 2, send_to_frame),
    ("postMessage", 2, |this, args, cx| {
        let id = open_page(this, cx)?;
        post(cx, id, args)?;
        Ok(JsValue::undefined())
    }),
];

pub(crate) const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[(
    "mainFrame",
    |this, _, cx| {
        let id = open_page(this, cx)?;
        Ok(page(cx, id)?.map_or(JsValue::undefined(), |page| page.frame.into()))
    },
    None,
)];

/// The `webContents` object of page `id`, made the first time something asks for it.
pub(crate) fn contents_object(context: &mut Context, id: ContentsId) -> JsResult<JsObject> {
    if let Some(page) = page(context, id)? {
        return Ok(page.object);
    }
    let object = define::emitter(context)?.construct(&[], None, context)?;
    object.set(boa_engine::js_string!("id"), id, false, context)?;
    define::bind(&object, WEB_CONTENTS, id, context);
    native::methods(context, &object, METHODS)?;
    native::accessors(context, &object, ACCESSORS)?;
    let ipc = ipc_main::new_ipc(context)?;
    native::hidden(&object, boa_engine::js_string!("ipc"), ipc.clone(), context)?;
    let page = Contents {
        object: object.clone(),
        ipc,
        frame: web_frame_main::make(context, id)?,
        frame_ipc: ipc_main::new_ipc(context)?,
    };
    ipc_main::state(context)?
        .pages
        .borrow_mut()
        .insert(id, page);
    Ok(object)
}

/// The objects of page `id`, if they were made and the page has not been forgotten.
pub(crate) fn page(context: &mut Context, id: ContentsId) -> JsResult<Option<Contents>> {
    Ok(ipc_main::state(context)?.pages.borrow().get(&id).cloned())
}

/// The page is gone: its objects are let go of, and so are the handlers they kept.
pub(crate) fn forget(context: &mut Context, id: ContentsId) {
    let Ok(state) = ipc_main::state(context) else {
        return;
    };
    let gone = state.pages.borrow_mut().remove(&id);
    if let Some(page) = gone {
        ipc_main::forget_handlers(context, &[&page.ipc, &page.frame_ipc]);
    }
}

/// Whether page `id` still has its window.
pub(crate) fn is_open(context: &Context, id: ContentsId) -> bool {
    shared(context).hub.window_of(id).is_some()
}

/// The page the `webContents` `this` stands for, which must still have its window:
/// `TypeError: Object has been destroyed` once it is gone.
fn open_page(this: &JsValue, context: &mut Context) -> JsResult<ContentsId> {
    let state = shared(context);
    define::live(this, WEB_CONTENTS, context, |id| {
        state.hub.window_of(id).map(|_| id)
    })
    .map(|(id, _)| id)
}

/// `webContents.sendToFrame(frameId, channel, ...args)`: the main frame is the only frame, `1` or
/// `[processId, 1]`. Answers whether it was found.
fn send_to_frame(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let id = open_page(this, context)?;
    let is_main = match arg(args, 0).as_object() {
        Some(pair) => {
            let pair = native::list_of(&pair.into(), context)?;
            let number = |at: usize| arg(&pair, at).as_number();
            number(0) == Some(f64::from(ipc::process_id(id)))
                && number(1) == Some(f64::from(MAIN_FRAME))
        }
        None => arg(args, 0).as_number() == Some(f64::from(MAIN_FRAME)),
    };
    if is_main {
        send(context, id, args.get(1..).unwrap_or_default())?;
    }
    Ok(is_main.into())
}

/// Sends the main frame of page `id` a message: `args` is the channel and the arguments. One that
/// cannot be cloned is logged, not thrown, as Electron does.
pub(crate) fn send(context: &mut Context, id: ContentsId, args: &[JsValue]) -> JsResult<()> {
    let Some(channel) = arg(args, 0).as_string().map(|c| c.to_std_string_escaped()) else {
        return Err(native::type_error("Missing required channel argument"));
    };
    match ipc::pack(args.get(1..).unwrap_or_default(), context) {
        Ok(args) => shared(context).hub.to_renderer(
            id,
            ToRenderer::Ipc {
                channel,
                args,
                sender_id: 0,
            },
        ),
        Err(_) => {
            let failed = ipc::error_value("Failed to serialize arguments", context)?;
            let said = js_text("Error sending from webFrameMain: ");
            console(context, "error", &[said, failed])?;
        }
    }
    Ok(())
}

/// Sends the main frame of page `id` a message with ports: `args` is the channel, the message and
/// the list of `MessagePortMain`s to give the page.
pub(crate) fn post(context: &mut Context, id: ContentsId, args: &[JsValue]) -> JsResult<()> {
    let channel = Args::new(&[arg(args, 0)]).string(0, "channel", context)?;
    let message = ipc::clone(&arg(args, 1), context)?;
    let ports = message_port_main::take_ports(
        &arg(args, 2),
        message_port_main::Invalid::Contents,
        None,
        context,
    )?;
    shared(context).hub.to_renderer(
        id,
        ToRenderer::Post {
            channel,
            message,
            ports,
        },
    );
    Ok(())
}
