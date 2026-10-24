//! UDP uses the same readiness reactor and DNS pool as TCP. One datagram credit prevents a
//! paused host from accumulating an unbounded receive queue; implicit binds admit replies only.
use super::{
    reactor::{Command, Event, Id},
    tcp::{self, emit, put},
};
use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, function, get, hidden, invoke,
    js_text, list_of, object, text_of,
};
use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
};
use std::net::{IpAddr, SocketAddr};

fn denied(this: &JsValue, name: &str, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let module = crate::modules::node_module(cx, "dgram")?;
    let proto = get(&module, "__deniedUdp", cx)?;
    let method = get(&proto, name, cx)?;
    crate::call(cx, &method, this, args)
}
fn family(this: &JsValue, cx: &mut Context) -> JsResult<bool> {
    Ok(crate::print(&get(this, "type", cx)?) == "udp6")
}
fn bind(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let options = arg(args, 0);
    let port = if options.is_object() {
        get(&options, "port", cx)?
    } else {
        options.clone()
    };
    let port = if port.is_undefined() {
        0.
    } else {
        port.to_number(cx)?
    };
    if !port.is_finite() || port.fract() != 0. || !(0. ..=65535.).contains(&port) {
        return Err(crate::native::coded_error(
            cx,
            "Port should be >= 0 and < 65536.",
            "ERR_SOCKET_BAD_PORT",
        ));
    }
    let v6 = family(this, cx)?;
    let address = if options.is_object() {
        get(&options, "address", cx)?
    } else {
        arg(args, 1)
    };
    let address = if address.is_string() {
        text_of(&address, cx)?
    } else {
        if v6 { "::" } else { "0.0.0.0" }.into()
    };
    let ip = address.parse::<IpAddr>().ok();
    if !ip.is_some_and(|ip| {
        crate::shared(cx)
            .network
            .borrow()
            .permissions
            .allows_listen(ip)
    }) {
        return denied(this, "bind", args, cx);
    }
    if super::id(this, cx)? != 0 {
        return Err(crate::native::coded_error(
            cx,
            "Socket is already bound",
            "ERR_SOCKET_ALREADY_BOUND",
        ));
    }
    if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
        invoke(this, "once", &[js_text("listening"), cb.clone()], cx)?;
    }
    if let (Some(target), Some(ip)) = (this.as_object(), ip) {
        let id = tcp::attach(&target, cx)?;
        super::send(
            cx,
            Command::UdpBind(id, SocketAddr::new(ip, port as u16), true),
        );
    }
    Ok(this.clone())
}
fn ensure(this: &JsValue, cx: &mut Context) -> JsResult<Id> {
    let id = super::id(this, cx)?;
    if id != 0 {
        return Ok(id);
    }
    let target = this
        .as_object()
        .ok_or_else(|| crate::native::type_error("Socket"))?;
    let id = tcp::attach(&target, cx)?;
    let address = if family(this, cx)? {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    }
    .parse::<SocketAddr>()
    .map_err(|_| crate::native::type_error("Invalid UDP address"))?;
    super::send(cx, Command::UdpBind(id, address, false));
    Ok(id)
}
fn send(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let input = arg(args, 0);
    let mut bytes = if input.as_object().is_some_and(|v| v.is_array()) {
        let mut bytes = Vec::new();
        for v in list_of(&input, cx)? {
            bytes.extend(
                crate::encoding::bytes_of(&v, cx)?.unwrap_or_else(|| crate::print(&v).into_bytes()),
            );
        }
        bytes
    } else {
        crate::encoding::bytes_of(&input, cx)?.unwrap_or_else(|| crate::print(&input).into_bytes())
    };
    let connected = get(this, "__udpPeer", cx)?;
    let (port, host) = if connected.is_object() {
        (
            get(&connected, "port", cx)?.to_number(cx)? as u16,
            text_of(&get(&connected, "address", cx)?, cx)?,
        )
    } else {
        let offset_form =
            arg(args, 1).is_number() && arg(args, 2).is_number() && arg(args, 3).is_number();
        let (port, address) = if offset_form {
            let offset = arg(args, 1).to_number(cx)?;
            let length = arg(args, 2).to_number(cx)?;
            if !offset.is_finite()
                || !length.is_finite()
                || offset.fract() != 0.
                || length.fract() != 0.
                || offset < 0.
                || length < 0.
                || offset + length > bytes.len() as f64
            {
                return Err(crate::native::coded_error(
                    cx,
                    "Offset or length is outside the bounds of the Buffer",
                    "ERR_BUFFER_OUT_OF_BOUNDS",
                ));
            }
            bytes = bytes[offset as usize..(offset + length) as usize].to_vec();
            (arg(args, 3), arg(args, 4))
        } else {
            (arg(args, 1), arg(args, 2))
        };
        let number = port.to_number(cx)?;
        if !number.is_finite() || number.fract() != 0. || !(1. ..=65535.).contains(&number) {
            return Err(crate::native::coded_error(
                cx,
                "Port should be > 0 and < 65536.",
                "ERR_SOCKET_BAD_PORT",
            ));
        }
        let host = if address.is_string() {
            text_of(&address, cx)?
        } else {
            if family(this, cx)? {
                "::1"
            } else {
                "127.0.0.1"
            }
            .into()
        };
        (number as u16, host)
    };
    let allowed = crate::shared(cx)
        .network
        .borrow()
        .permissions
        .allows_outbound(&host, port)
        || connected.is_object();
    if !allowed {
        return denied(this, "send", args, cx);
    }
    let id = ensure(this, cx)?;
    let request = {
        let shared = crate::shared(cx);
        let mut state = shared.network.borrow_mut();
        state.next += 1;
        state.next
    };
    let callbacks = get(this, "__udpCallbacks", cx)?;
    let callback = args
        .iter()
        .rev()
        .find(|v| v.is_callable())
        .cloned()
        .unwrap_or_default();
    put(&callbacks, &request.to_string(), callback, cx)?;
    super::send(cx, Command::UdpSend(id, request, host, port, bytes));
    Ok(JsValue::undefined())
}
fn connect(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let port = arg(args, 0).to_number(cx)?;
    if port <= 0. || port > 65535. || port.fract() != 0. {
        return Err(crate::native::coded_error(
            cx,
            "Port should be > 0 and < 65536.",
            "ERR_SOCKET_BAD_PORT",
        ));
    }
    let host = if arg(args, 1).is_string() {
        text_of(&arg(args, 1), cx)?
    } else {
        if family(this, cx)? {
            "::1"
        } else {
            "127.0.0.1"
        }
        .into()
    };
    if !crate::shared(cx)
        .network
        .borrow()
        .permissions
        .allows_outbound(&host, port as u16)
    {
        return denied(this, "connect", args, cx);
    }
    if get(this, "__udpPeer", cx)?.is_object() {
        return Err(crate::native::coded_error(
            cx,
            "Already connected",
            "ERR_SOCKET_DGRAM_IS_CONNECTED",
        ));
    }
    if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
        invoke(this, "once", &[js_text("connect"), cb.clone()], cx)?;
    }
    let id = ensure(this, cx)?;
    let request = {
        let shared = crate::shared(cx);
        let mut state = shared.network.borrow_mut();
        state.next += 1;
        state.next
    };
    super::send(cx, Command::UdpConnect(id, request, host, port as u16));
    Ok(JsValue::undefined())
}
fn option(this: &JsValue, name: &str, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let id = super::id(this, cx)?;
    if id == 0 {
        return denied(this, name, args, cx);
    }
    let value = arg(args, 0);
    let text = if value.is_undefined() {
        String::new()
    } else {
        text_of(&value, cx)?
    };
    let extra = arg(args, 1);
    let extra = if extra.is_undefined() {
        String::new()
    } else {
        text_of(&extra, cx)?
    };
    super::send(cx, Command::UdpOption(id, name.into(), text, extra));
    Ok(
        if name == "setTTL" || name == "setMulticastTTL" || name == "setMulticastLoopback" {
            value
        } else {
            JsValue::undefined()
        },
    )
}
const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("bind", 3, bind),
    ("send", 6, send),
    ("connect", 3, connect),
    ("disconnect", 0, |this, _, cx| {
        if !get(this, "__udpPeer", cx)?.is_object() {
            return Err(crate::native::coded_error(
                cx,
                "Not connected",
                "ERR_SOCKET_DGRAM_NOT_CONNECTED",
            ));
        }
        let id = super::id(this, cx)?;
        super::send(
            cx,
            Command::UdpOption(id, "disconnect".into(), String::new(), String::new()),
        );
        put(this, "__udpPeer", JsValue::undefined(), cx)?;
        Ok(JsValue::undefined())
    }),
    ("close", 1, |this, args, cx| {
        let id = super::id(this, cx)?;
        if id == 0 {
            return denied(this, "close", args, cx);
        }
        if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
            invoke(this, "once", &[js_text("close"), cb.clone()], cx)?;
        }
        super::send(cx, Command::Close(id));
        Ok(this.clone())
    }),
    ("address", 0, |this, args, cx| {
        let address = get(this, "__networkAddress", cx)?;
        if address.is_undefined() {
            denied(this, "address", args, cx)
        } else {
            Ok(address)
        }
    }),
    ("remoteAddress", 0, |this, _, cx| {
        let address = get(this, "__udpPeer", cx)?;
        if address.is_undefined() {
            return Err(crate::native::coded_error(
                cx,
                "Not connected",
                "ERR_SOCKET_DGRAM_NOT_CONNECTED",
            ));
        }
        Ok(address)
    }),
    ("ref", 0, |this, _, _| Ok(this.clone())),
    ("unref", 0, |this, _, _| Ok(this.clone())),
];
pub(crate) fn install(module: &JsObject, cx: &mut Context) -> JsResult<()> {
    let legacy = get(&module.clone().into(), "Socket", cx)?;
    let proto = get(&legacy, "prototype", cx)?;
    hidden(module, js_string!("__deniedUdp"), proto, cx)?;
    let emitter = crate::events::emitter_class(cx)?;
    let (socket, proto) = class(
        cx,
        ClassSpec {
            name: "Socket",
            parent: Some(&emitter),
            init: |target, args, cx| {
                let emitter = crate::events::emitter_class(cx)?;
                crate::call(cx, &emitter.into(), &target.clone().into(), &[])?;
                let options = arg(args, 0);
                let kind = if options.is_object() {
                    get(&options, "type", cx)?
                } else {
                    options
                };
                let kind = if kind.is_undefined() {
                    js_text("udp4")
                } else {
                    kind
                };
                if !["udp4", "udp6"].contains(&crate::print(&kind).as_str()) {
                    return Err(crate::native::coded_error(
                        cx,
                        "Bad socket type specified. Valid types are: udp4, udp6",
                        "ERR_SOCKET_BAD_TYPE",
                    ));
                }
                target.set(js_string!("type"), kind, false, cx)?;
                hidden(target, js_string!("__udp"), true, cx)?;
                hidden(
                    target,
                    js_string!("__udpCallbacks"),
                    JsObject::with_null_proto(),
                    cx,
                )?;
                if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
                    invoke(
                        &target.clone().into(),
                        "on",
                        &[js_text("message"), cb.clone()],
                        cx,
                    )?;
                }
                Ok(())
            },
            methods: METHODS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Socket"), socket, false, cx)?;
    for name in [
        "setBroadcast",
        "setTTL",
        "setMulticastTTL",
        "setMulticastLoopback",
        "setMulticastInterface",
        "addMembership",
        "dropMembership",
        "setRecvBufferSize",
        "setSendBufferSize",
    ] {
        let f = closure(
            cx,
            name,
            2,
            |this, args, name: &String, cx| option(this, name, args, cx),
            name.to_owned(),
        );
        proto.set(JsString::from(name), f, false, cx)?;
    }
    for (name, key) in [
        ("getRecvBufferSize", "__udpRecvBuffer"),
        ("getSendBufferSize", "__udpSendBuffer"),
    ] {
        let f = closure(
            cx,
            name,
            0,
            |this, _, key: &String, cx| {
                let v = get(this, key, cx)?;
                Ok(if v.is_undefined() { 65536.into() } else { v })
            },
            key.to_owned(),
        );
        proto.set(JsString::from(name), f, false, cx)?;
    }
    let create = function(cx, "createSocket", 2, |_, args, cx| {
        let module = crate::modules::node_module(cx, "dgram")?;
        let ctor = get(&module, "Socket", cx)?
            .as_object()
            .ok_or_else(|| crate::native::type_error("Socket"))?;
        Ok(ctor.construct(args, None, cx)?.into())
    });
    module.set(js_string!("createSocket"), create, false, cx)?;
    Ok(())
}
pub(crate) fn deliver(id: Id, target: &JsObject, event: Event, cx: &mut Context) -> JsResult<()> {
    let this: JsValue = target.clone().into();
    match event {
        Event::Listening(local) => {
            let address = tcp::address(local, cx)?;
            put(&this, "__networkAddress", address, cx)?;
            emit(&this, "listening", &[], cx)?;
        }
        Event::Datagram(bytes, peer) => {
            let length = bytes.len();
            let info = tcp::address(peer, cx)?;
            put(&info, "size", length as f64, cx)?;
            let buffer = crate::encoding::make_buffer(cx, bytes)?;
            emit(&this, "message", &[buffer, info], cx)?;
            super::send(cx, Command::Read(id, true));
        }
        Event::DatagramSent(request, result) => {
            let callbacks = get(&this, "__udpCallbacks", cx)?;
            let callback = get(&callbacks, &request.to_string(), cx)?;
            if let Some(callbacks) = callbacks.as_object() {
                callbacks.delete_property_or_throw(JsString::from(request.to_string()), cx)?;
            }
            let args = match result {
                Ok(n) => vec![JsValue::null(), (n as f64).into()],
                Err(code) => vec![coded_error_value(cx, &format!("send {code}"), &code)],
            };
            if callback.is_callable() {
                crate::call(cx, &callback, &JsValue::undefined(), &args)?;
            } else if !args[0].is_null() {
                emit(&this, "error", &args[..1], cx)?;
            }
        }
        Event::DatagramConnected(local, peer) => {
            let address = tcp::address(local, cx)?;
            put(&this, "__networkAddress", address, cx)?;
            let address = tcp::address(peer, cx)?;
            put(&this, "__udpPeer", address, cx)?;
            emit(&this, "connect", &[], cx)?;
        }
        Event::Closed => {
            crate::shared(cx).network.borrow_mut().targets.remove(&id);
            put(&this, "__networkAddress", JsValue::undefined(), cx)?;
            emit(&this, "close", &[], cx)?;
        }
        Event::Error(code, message) => {
            let e = coded_error_value(cx, &message, &code);
            emit(&this, "error", &[e], cx)?;
        }
        _ => {}
    }
    Ok(())
}
