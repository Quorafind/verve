//! Node's IPC channel between a parent and a child it `fork`ed: `child.send`, `child.on('message')`,
//! `process.send`, `process.on('message')`, and `disconnect` on both sides.
//!
//! The channel is one two-way pipe, the descriptor `NODE_CHANNEL_FD` names in the child. With
//! `serialization: 'json'` — Node's default, and byte-for-byte Node's — each message is
//! `JSON.stringify(message) + '\n'`, so a real Node child and a Verve one understand each other.
//! `'advanced'` is Node's V8 serialization there; here it is the structured clone's own byte
//! encoding, behind a 4-byte length, and works between Verve processes only. A message whose
//! `cmd` starts with `NODE_` is Node's own, and goes to `internalMessage`, not `message`.
//!
//! Handles (`send(message, socket)`) cannot be passed: a socket here is the reactor's, not a
//! descriptor.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use boa_gc::{Finalize, Trace};

use super::launch::{self, Flow};
use super::spec::Serialization;
use crate::clone::Cloned;
use crate::native::{
    arg, closure, coded_error_value, function, get, global, hidden, invoke, js_text, object,
};
use crate::network::reactor::Command;
use crate::network::{self, tcp};
use crate::sockets::make;

/// Turns the bytes of a channel into whole messages.
pub(super) struct Decoder {
    mode: Serialization,
    pending: Vec<u8>,
}

impl Decoder {
    pub(super) fn new(mode: Serialization) -> Self {
        Self {
            mode,
            pending: Vec::new(),
        }
    }

    /// Takes what arrived and answers the messages it completed; the rest is kept.
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some((message, used)) = self.next() {
            self.pending.drain(..used);
            if !message.is_empty() {
                messages.push(message);
            }
        }
        messages
    }

    fn next(&self) -> Option<(Vec<u8>, usize)> {
        match self.mode {
            Serialization::Json => {
                let end = self.pending.iter().position(|&b| b == b'\n')?;
                Some((self.pending[..end].to_vec(), end + 1))
            }
            Serialization::Advanced => {
                let length = u32::from_be_bytes(self.pending.get(..4)?.try_into().ok()?) as usize;
                let message = self.pending.get(4..4 + length)?;
                Some((message.to_vec(), 4 + length))
            }
        }
    }
}

/// One message as it goes on the wire.
fn frame(mode: Serialization, payload: &[u8]) -> Vec<u8> {
    match mode {
        Serialization::Json => [payload, b"\n"].concat(),
        Serialization::Advanced => [&(payload.len() as u32).to_be_bytes(), payload].concat(),
    }
}

/// What the data handler of one channel holds.
#[derive(Clone, Trace, Finalize)]
struct Receiver {
    owner: JsObject,
    #[unsafe_ignore_trace]
    decoder: Rc<RefCell<Decoder>>,
    #[unsafe_ignore_trace]
    mode: Serialization,
}

/// Makes `socket` the channel of `owner` — a `ChildProcess` or `process`: `connected`, `channel`,
/// the `message` and `disconnect` events.
pub(super) fn attach(
    owner: &JsObject,
    socket: &JsObject,
    mode: Serialization,
    context: &mut Context,
) -> JsResult<()> {
    hidden(owner, js_string!("__ipc"), socket.clone(), context)?;
    hidden(
        owner,
        js_string!("__ipcMode"),
        js_text(if mode == Serialization::Json {
            "json"
        } else {
            "advanced"
        }),
        context,
    )?;
    owner.set(js_string!("connected"), true, false, context)?;
    let channel = object(context);
    for (name, refed) in [("ref", true), ("unref", false)] {
        let toggle = closure(
            context,
            name,
            0,
            |_, _, (socket, refed): &(JsObject, bool), cx| {
                network::set_ref(&socket.clone().into(), *refed, cx)
            },
            (socket.clone(), refed),
        );
        channel.set(boa_engine::JsString::from(name), toggle, false, context)?;
    }
    owner.set(js_string!("channel"), channel, false, context)?;
    let receiver = Receiver {
        owner: owner.clone(),
        decoder: Rc::new(RefCell::new(Decoder::new(mode))),
        mode,
    };
    let on_data = closure(context, "", 1, received, receiver);
    let this: JsValue = socket.clone().into();
    invoke(&this, "on", &[js_text("data"), on_data.into()], context)?;
    let on_close = closure(
        context,
        "",
        0,
        |_, _, owner: &JsObject, cx| {
            disconnected(owner, cx)?;
            Ok(JsValue::undefined())
        },
        owner.clone(),
    );
    invoke(&this, "once", &[js_text("close"), on_close.into()], context)?;
    // An error on a channel is a disconnect, not a crash.
    let on_error = function(context, "", 1, |_, _, _| Ok(JsValue::undefined()));
    invoke(&this, "on", &[js_text("error"), on_error.into()], context)?;
    Ok(())
}

/// Bytes came in: the messages they complete go to `message` listeners.
fn received(
    _: &JsValue,
    args: &[JsValue],
    receiver: &Receiver,
    context: &mut Context,
) -> JsResult<JsValue> {
    let Some(bytes) = crate::encoding::bytes_of(&arg(args, 0), context)? else {
        return Ok(JsValue::undefined());
    };
    let frames = receiver.decoder.borrow_mut().push(&bytes);
    let owner: JsValue = receiver.owner.clone().into();
    for payload in frames {
        match decode(receiver.mode, &payload, context) {
            Ok(message) if is_internal(&message, context)? => {
                invoke(
                    &owner,
                    "emit",
                    &[js_text("internalMessage"), message],
                    context,
                )?;
            }
            Ok(message) => {
                invoke(&owner, "emit", &[js_text("message"), message], context)?;
            }
            Err(error) => {
                invoke(&owner, "emit", &[js_text("error"), error], context)?;
            }
        }
    }
    Ok(JsValue::undefined())
}

fn decode(mode: Serialization, payload: &[u8], context: &mut Context) -> Result<JsValue, JsValue> {
    let fail = |context: &mut Context, why: &str| {
        coded_error_value(context, why, "ERR_IPC_CHANNEL_CLOSED")
    };
    match mode {
        Serialization::Json => {
            let text = js_text(&String::from_utf8_lossy(payload));
            let json = global(context, "JSON").map_err(|_| JsValue::undefined())?;
            invoke(&json, "parse", &[text], context)
                .map_err(|e| e.into_opaque(context).unwrap_or_default())
        }
        Serialization::Advanced => Cloned::from_bytes(payload)
            .map_err(|_| {
                fail(
                    context,
                    "advanced serialization is only supported between Verve processes",
                )
            })?
            .to_value(context)
            .map_err(|e| e.into_opaque(context).unwrap_or_default()),
    }
}

/// Node's own messages: objects whose `cmd` starts with `NODE_`.
fn is_internal(message: &JsValue, context: &mut Context) -> JsResult<bool> {
    let cmd = get(message, "cmd", context)?;
    Ok(cmd.is_string() && crate::print(&cmd).starts_with("NODE_"))
}

fn disconnected(owner: &JsObject, context: &mut Context) -> JsResult<()> {
    let this: JsValue = owner.clone().into();
    if get(&this, "__ipcGone", context)?.to_boolean() {
        return Ok(());
    }
    hidden(owner, js_string!("__ipcGone"), true, context)?;
    owner.set(js_string!("connected"), false, false, context)?;
    invoke(&this, "emit", &[js_text("disconnect")], context)?;
    Ok(())
}

/// `send(message, sendHandle?, options?, callback?)`, for a child and for `process`.
pub(super) fn send(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let message = arg(args, 0);
    if message.is_undefined() {
        return Err(crate::events::coded_type_error(
            context,
            "The \"message\" argument must be specified",
            "ERR_MISSING_ARGS",
        ));
    }
    if message.is_callable() || message.is_symbol() {
        return Err(crate::native::invalid_arg_type(
            context,
            "message",
            "one of type string, object, number, or boolean",
            &message,
        ));
    }
    let handle = arg(args, 1);
    if !handle.is_null_or_undefined() && !handle.is_callable() {
        return Err(crate::native::coded_error(
            context,
            "Sending handles over IPC is not supported by Verve: a socket here belongs to the reactor, not to a descriptor",
            "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
        ));
    }
    let callback = args.iter().skip(1).rev().find(|a| a.is_callable()).cloned();
    let socket = get(this, "__ipc", context)?;
    if !get(this, "connected", context)?.to_boolean() || !socket.is_object() {
        let error = coded_error_value(context, "Channel closed", "ERR_IPC_CHANNEL_CLOSED");
        match callback {
            Some(callback) => crate::microtask(context, callback, vec![error]),
            None => {
                let emit = closure(
                    context,
                    "",
                    0,
                    |_, _, (owner, error): &(JsValue, JsValue), cx| {
                        invoke(owner, "emit", &[js_text("error"), error.clone()], cx)
                    },
                    (this.clone(), error),
                );
                crate::microtask(context, emit.into(), Vec::new());
            }
        }
        return Ok(false.into());
    }
    let mode = get(this, "__ipcMode", context)?;
    let mode = if crate::print(&mode) == "advanced" {
        Serialization::Advanced
    } else {
        Serialization::Json
    };
    let payload = encode(mode, &message, context)?;
    let buffer = crate::encoding::make_buffer(context, frame(mode, &payload))?;
    // The process stays while the message is on its way, whether or not anyone listens.
    network::set_ref(&socket, true, context)?;
    let done = closure(
        context,
        "",
        1,
        |_, args, (callback, owner, socket): &(Option<JsValue>, JsValue, JsValue), cx| {
            if get(socket, "writableLength", cx)?.as_number() == Some(0.) {
                let mut listeners = 0.;
                for event in ["message", "disconnect"] {
                    let count = invoke(owner, "listenerCount", &[js_text(event)], cx)?;
                    listeners += count.as_number().unwrap_or(0.);
                }
                if listeners == 0. {
                    network::set_ref(socket, false, cx)?;
                }
            }
            if let Some(callback) = callback {
                let error = arg(args, 0);
                let error = if error.is_null_or_undefined() {
                    JsValue::null()
                } else {
                    error
                };
                crate::call(cx, callback, &JsValue::undefined(), &[error])?;
            }
            Ok(JsValue::undefined())
        },
        (callback, this.clone(), socket.clone()),
    );
    invoke(&socket, "write", &[buffer, done.into()], context)
}

fn encode(mode: Serialization, message: &JsValue, context: &mut Context) -> JsResult<Vec<u8>> {
    match mode {
        Serialization::Json => {
            let json = global(context, "JSON")?;
            let text = invoke(&json, "stringify", std::slice::from_ref(message), context)?;
            Ok(crate::print(&text).into_bytes())
        }
        Serialization::Advanced => Cloned::from_value(message, context)?
            .to_bytes()
            .map_err(|e| crate::native::type_error(e.to_string())),
    }
}

/// `disconnect()`, for a child and for `process`.
pub(super) fn disconnect(
    this: &JsValue,
    _: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    if !get(this, "connected", context)?.to_boolean() {
        let error = coded_error_value(
            context,
            "IPC channel is already disconnected",
            "ERR_IPC_DISCONNECTED",
        );
        invoke(this, "emit", &[js_text("error"), error], context)?;
        return Ok(JsValue::undefined());
    }
    if let Some(owner) = this.as_object() {
        owner.set(js_string!("connected"), false, false, context)?;
    }
    // What was sent is still on its way: the channel is ended, and closed once that is done.
    let socket = get(this, "__ipc", context)?;
    let close = closure(
        context,
        "",
        0,
        |_, _, socket: &JsValue, cx| invoke(socket, "destroy", &[], cx),
        socket.clone(),
    );
    invoke(&socket, "once", &[js_text("finish"), close.into()], context)?;
    invoke(&socket, "end", &[], context)?;
    Ok(JsValue::undefined())
}

// ─── The child's side ────────────────────────────────────────────────────────────────────────

/// If this process was started by `fork`, joins the channel it was given: `process.send`,
/// `process.connected`, `process.disconnect`, `process.channel`, and `process`'s `message` and
/// `disconnect` events. `NODE_CHANNEL_FD` is taken out of the environment, as Node does.
pub(crate) fn join_parent(context: &mut Context) -> JsResult<()> {
    let process = global(context, "process")?;
    let env = get(&process, "env", context)?;
    let fd = get(&env, "NODE_CHANNEL_FD", context)?;
    let mode = get(&env, "NODE_CHANNEL_SERIALIZATION_MODE", context)?;
    if !fd.is_string() {
        return Ok(());
    }
    let fd: Option<i32> = crate::print(&fd).parse().ok();
    if let Some(env) = env.as_object() {
        for name in ["NODE_CHANNEL_FD", "NODE_CHANNEL_SERIALIZATION_MODE"] {
            env.delete_property_or_throw(boa_engine::JsString::from(name), context)?;
        }
    }
    let (Some(fd), Some(process)) = (fd, process.as_object()) else {
        return Ok(());
    };
    let Some(io) = launch::inherited(fd) else {
        return Ok(());
    };
    let options = object(context);
    options.set(js_string!("allowHalfOpen"), false, false, context)?;
    let socket = make(context, "net", "Socket", &[options.into()])?;
    let id = tcp::attach(&socket, context)?;
    network::send(context, Command::Pipe(id, io, Flow::Both));
    socket.set(js_string!("pending"), false, false, context)?;
    let mode = if mode.is_string() && crate::print(&mode) == "advanced" {
        Serialization::Advanced
    } else {
        Serialization::Json
    };
    attach(&process, &socket, mode, context)?;
    for (name, f, length) in [
        (
            "send",
            send as boa_engine::native_function::NativeFunctionPointer,
            4,
        ),
        ("disconnect", disconnect, 0),
    ] {
        let f = function(context, name, length, f);
        process.set(boa_engine::JsString::from(name), f, false, context)?;
    }
    // The channel holds the process open only while somebody listens on it.
    network::set_ref(&socket.clone().into(), false, context)?;
    let process: JsValue = process.into();
    let watch = closure(
        context,
        "",
        1,
        |_, args, socket: &JsObject, cx| {
            let event = crate::print(&arg(args, 0));
            if event == "message" || event == "disconnect" {
                network::set_ref(&socket.clone().into(), true, cx)?;
            }
            Ok(JsValue::undefined())
        },
        socket.clone(),
    );
    // And lets it go again once nobody does.
    let unwatch = closure(
        context,
        "",
        1,
        |_, _, (process, socket): &(JsValue, JsObject), cx| {
            let mut listeners = 0.;
            for event in ["message", "disconnect"] {
                let count = invoke(process, "listenerCount", &[js_text(event)], cx)?;
                listeners += count.as_number().unwrap_or(0.);
            }
            if listeners == 0. {
                network::set_ref(&socket.clone().into(), false, cx)?;
            }
            Ok(JsValue::undefined())
        },
        (process.clone(), socket),
    );
    invoke(
        &process,
        "on",
        &[js_text("removeListener"), unwatch.into()],
        context,
    )?;
    invoke(
        &process,
        "on",
        &[js_text("newListener"), watch.into()],
        context,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_messages_are_lines_and_arrive_whole_or_not_at_all() {
        let mut decoder = Decoder::new(Serialization::Json);
        assert!(decoder.push(b"{\"a\":").is_empty());
        let messages = decoder.push(b"1}\n\"two\"\n{\"b\"");
        assert_eq!(messages, [b"{\"a\":1}".to_vec(), b"\"two\"".to_vec()]);
        assert_eq!(decoder.push(b":3}\n"), [b"{\"b\":3}".to_vec()]);
    }

    #[test]
    fn advanced_messages_carry_their_length() {
        let mut decoder = Decoder::new(Serialization::Advanced);
        let wire = [
            frame(Serialization::Advanced, b"abc"),
            frame(Serialization::Advanced, b"de"),
        ]
        .concat();
        assert!(decoder.push(&wire[..5]).is_empty());
        assert_eq!(decoder.push(&wire[5..]), [b"abc".to_vec(), b"de".to_vec()]);
    }
}
