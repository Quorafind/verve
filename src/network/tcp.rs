//! TCP sockets are real Duplex streams. The Writable owns write buffering; its native write
//! callback is completed only after the reactor has handed the complete chunk to the kernel.
use super::{
    Id,
    reactor::{Command, Event},
};
use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, function, get, hidden, invoke,
    js_text, object, text_of,
};
use boa_engine::{
    Context, JsData, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
};
use boa_gc::Trace;
use std::net::{IpAddr, SocketAddr};

#[derive(Trace, JsData)]
struct Handle {
    #[unsafe_ignore_trace]
    id: Id,
    #[unsafe_ignore_trace]
    sender: super::reactor::Sender,
}
impl boa_gc::Finalize for Handle {
    fn finalize(&self) {
        self.sender.send(Command::Close(self.id));
    }
}

pub(crate) fn attach(target: &JsObject, cx: &mut Context) -> JsResult<Id> {
    let id = super::register(target, cx)?;
    if let Some(sender) = crate::shared(cx)
        .network
        .borrow()
        .reactor
        .as_ref()
        .map(|r| r.sender.clone())
    {
        let handle = JsObject::from_proto_and_data(None, Handle { id, sender });
        hidden(target, js_string!("__networkHandle"), handle, cx)?;
    }
    Ok(id)
}
pub(crate) fn put(
    target: &JsValue,
    key: &str,
    value: impl Into<JsValue>,
    cx: &mut Context,
) -> JsResult<()> {
    if let Some(o) = target.as_object() {
        o.set(JsString::from(key), value, false, cx)?;
    }
    Ok(())
}
pub(crate) fn emit(
    target: &JsValue,
    name: &str,
    args: &[JsValue],
    cx: &mut Context,
) -> JsResult<()> {
    let mut values = vec![js_text(name)];
    values.extend_from_slice(args);
    invoke(target, "emit", &values, cx)?;
    Ok(())
}
pub(crate) fn stream_class(name: &str, cx: &mut Context) -> JsResult<JsObject> {
    get(&crate::modules::node_module(cx, "stream")?, name, cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("stream class"))
}
pub(crate) fn init_socket(target: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let duplex = stream_class("Duplex", cx)?;
    crate::call(cx, &duplex.into(), &target.clone().into(), args)?;
    for (key, value) in [("connecting", false), ("pending", true)] {
        target.set(JsString::from(key), value, false, cx)?;
    }
    for key in ["bytesRead", "bytesWritten"] {
        target.set(JsString::from(key), 0, false, cx)?;
    }
    hidden(target, js_string!("__networkPaused"), false, cx)?;
    Ok(())
}
fn stream_method(
    this: &JsValue,
    name: &str,
    args: &[JsValue],
    cx: &mut Context,
) -> JsResult<JsValue> {
    let parent = stream_class("Duplex", cx)?;
    let proto = parent.get(js_string!("prototype"), cx)?;
    let method = get(&proto, name, cx)?;
    crate::call(cx, &method, this, args)
}
fn denied(this: &JsValue, name: &str, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    crate::sockets::denied_socket(this, name, args, cx)
}
pub(crate) fn endpoint(
    args: &[JsValue],
    listen: bool,
    cx: &mut Context,
) -> JsResult<Option<(String, u16, u8)>> {
    let first = arg(args, 0);
    let options = if first.is_object() && !first.is_callable() {
        first.clone()
    } else {
        JsValue::undefined()
    };
    let path = get(&options, "path", cx)?;
    if !path.is_undefined() || (first.is_string() && crate::print(&first).parse::<u16>().is_err()) {
        return Ok(None);
    }
    let port = if options.is_object() {
        get(&options, "port", cx)?
    } else {
        first
    };
    let number = port.to_number(cx)?;
    if !number.is_finite() || number.fract() != 0. || !(0. ..=65535.).contains(&number) {
        return Err(crate::native::coded_error(
            cx,
            "Port should be >= 0 and < 65536.",
            "ERR_SOCKET_BAD_PORT",
        ));
    }
    let hostname = get(&options, "host", cx)?;
    let hostname = if hostname.is_undefined() && arg(args, 1).is_string() {
        arg(args, 1)
    } else {
        hostname
    };
    let host = if hostname.is_undefined() {
        if listen {
            "::".into()
        } else {
            "localhost".into()
        }
    } else {
        text_of(&hostname, cx)?
    };
    let family = get(&options, "family", cx)?.as_number().unwrap_or(0.) as u8;
    Ok(Some((
        host.trim_matches(['[', ']']).to_owned(),
        number as u16,
        family,
    )))
}
pub(crate) fn connect(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let Some((host, port, family)) = endpoint(args, false, cx)? else {
        return denied(this, "connect", args, cx);
    };
    if !crate::shared(cx)
        .network
        .borrow()
        .permissions
        .allows_outbound(&host, port)
    {
        return denied(this, "connect", args, cx);
    }
    if let Some(callback) = args.iter().rev().find(|a| a.is_callable()) {
        invoke(this, "once", &[js_text("connect"), callback.clone()], cx)?;
    }
    let target = this
        .as_object()
        .ok_or_else(|| crate::native::type_error("Socket"))?;
    let id = attach(&target, cx)?;
    put(this, "connecting", true, cx)?;
    put(this, "__networkHost", js_text(&host), cx)?;
    super::send(cx, Command::Connect(id, host, port, family));
    let timeout = get(this, "timeout", cx)?.as_number().unwrap_or(0.) as u64;
    if timeout != 0 {
        super::send(cx, Command::Timeout(id, timeout));
    }
    Ok(this.clone())
}
pub(crate) const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("connect", 3, connect),
    ("_read", 1, |this, _, cx| {
        let id = super::id(this, cx)?;
        if id != 0 && !get(this, "__networkPaused", cx)?.to_boolean() {
            super::send(cx, Command::Read(id, true));
        }
        Ok(JsValue::undefined())
    }),
    ("_write", 3, |this, args, cx| {
        let id = super::id(this, cx)?;
        if id == 0 {
            return denied(this, "write", args, cx);
        }
        let data = crate::encoding::bytes_of(&arg(args, 0), cx)?
            .unwrap_or_else(|| crate::print(&arg(args, 0)).into_bytes());
        put(this, "__networkWriteCallback", arg(args, 2), cx)?;
        super::send(cx, Command::Write(id, data));
        Ok(JsValue::undefined())
    }),
    ("_final", 1, |this, args, cx| {
        let id = super::id(this, cx)?;
        super::send(cx, Command::End(id));
        let cb = arg(args, 0);
        if cb.is_callable() {
            crate::call(cx, &cb, &JsValue::undefined(), &[])?;
        }
        Ok(JsValue::undefined())
    }),
    ("_destroy", 2, |this, args, cx| {
        let id = super::id(this, cx)?;
        if id != 0 {
            super::send(cx, Command::Close(id));
        }
        let cb = arg(args, 1);
        if cb.is_callable() {
            crate::call(cx, &cb, &JsValue::undefined(), &[arg(args, 0)])?;
        }
        Ok(JsValue::undefined())
    }),
    ("pause", 0, |this, args, cx| {
        put(this, "__networkPaused", true, cx)?;
        let id = super::id(this, cx)?;
        super::send(cx, Command::Read(id, false));
        stream_method(this, "pause", args, cx)
    }),
    ("resume", 0, |this, args, cx| {
        put(this, "__networkPaused", false, cx)?;
        let result = stream_method(this, "resume", args, cx)?;
        let id = super::id(this, cx)?;
        super::send(cx, Command::Read(id, true));
        Ok(result)
    }),
    ("setNoDelay", 1, |this, args, cx| {
        let id = super::id(this, cx)?;
        let enabled = arg(args, 0).is_undefined() || arg(args, 0).to_boolean();
        put(this, "__networkNoDelay", enabled, cx)?;
        super::send(cx, Command::NoDelay(id, enabled));
        Ok(this.clone())
    }),
    ("setKeepAlive", 2, |this, args, cx| {
        let id = super::id(this, cx)?;
        let enabled = arg(args, 0).to_boolean();
        let delay = arg(args, 1).as_number().unwrap_or(0.).max(0.) as u64;
        put(this, "__networkKeepAlive", enabled, cx)?;
        put(this, "__networkKeepDelay", delay as f64, cx)?;
        super::send(cx, Command::KeepAlive(id, enabled, delay));
        Ok(this.clone())
    }),
    ("setTimeout", 2, |this, args, cx| {
        let ms = arg(args, 0).to_number(cx)?;
        if ms < 0. || !ms.is_finite() {
            return Err(crate::native::coded_error(
                cx,
                "The value of \"msecs\" is out of range.",
                "ERR_OUT_OF_RANGE",
            ));
        }
        put(this, "timeout", ms, cx)?;
        if arg(args, 1).is_callable() {
            invoke(this, "once", &[js_text("timeout"), arg(args, 1)], cx)?;
        }
        let id = super::id(this, cx)?;
        super::send(cx, Command::Timeout(id, ms as u64));
        Ok(this.clone())
    }),
    ("address", 0, |this, _, cx| {
        get(this, "__networkAddress", cx)
    }),
    ("destroySoon", 0, |this, _, cx| invoke(this, "end", &[], cx)),
    ("resetAndDestroy", 0, |this, _, cx| {
        invoke(this, "destroy", &[], cx)
    }),
    ("ref", 0, |this, _, _| Ok(this.clone())),
    ("unref", 0, |this, _, _| Ok(this.clone())),
];
pub(crate) fn init_server(target: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let emitter = crate::events::emitter_class(cx)?;
    crate::call(cx, &emitter.into(), &target.clone().into(), &[])?;
    let options = arg(args, 0);
    hidden(target, js_string!("__networkOptions"), options, cx)?;
    target.set(js_string!("listening"), false, false, cx)?;
    let connections = array(cx, Vec::new());
    hidden(target, js_string!("__networkConnections"), connections, cx)?;
    if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
        invoke(
            &target.clone().into(),
            "on",
            &[js_text("connection"), cb.clone()],
            cx,
        )?;
    }
    Ok(())
}
pub(crate) const SERVER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("listen", 3, |this, args, cx| {
        let Some((host, port, _)) = endpoint(args, true, cx)? else {
            return crate::sockets::denied_server(this, "listen", args, cx);
        };
        let ip = if host.eq_ignore_ascii_case("localhost") {
            "127.0.0.1".parse::<IpAddr>().ok()
        } else {
            host.parse().ok()
        };
        let permitted = ip.is_some_and(|ip| {
            crate::shared(cx)
                .network
                .borrow()
                .permissions
                .allows_listen(ip)
        });
        if !permitted {
            return crate::sockets::denied_server(this, "listen", args, cx);
        }
        if get(this, "listening", cx)?.to_boolean() {
            return Err(crate::native::coded_error(
                cx,
                "Listen method has been called more than once without closing.",
                "ERR_SERVER_ALREADY_LISTEN",
            ));
        }
        if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
            invoke(this, "once", &[js_text("listening"), cb.clone()], cx)?;
        }
        if let (Some(target), Some(ip)) = (this.as_object(), ip) {
            let id = attach(&target, cx)?;
            super::send(cx, Command::Listen(id, SocketAddr::new(ip, port)));
        }
        Ok(this.clone())
    }),
    ("address", 0, |this, _, cx| {
        let address = get(this, "__networkAddress", cx)?;
        Ok(if address.is_undefined() {
            JsValue::null()
        } else {
            address
        })
    }),
    ("close", 1, |this, args, cx| {
        if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
            invoke(this, "once", &[js_text("close"), cb.clone()], cx)?;
        }
        let id = super::id(this, cx)?;
        if id == 0 {
            return crate::sockets::denied_server(this, "close", args, cx);
        }
        put(this, "listening", false, cx)?;
        put(this, "__networkClosing", true, cx)?;
        super::send(cx, Command::Close(id));
        Ok(this.clone())
    }),
    ("getConnections", 1, |this, args, cx| {
        let connections = get(this, "__networkConnections", cx)?;
        let n = get(&connections, "length", cx)?;
        if arg(args, 0).is_callable() {
            crate::microtask(cx, arg(args, 0), vec![JsValue::null(), n]);
        }
        Ok(this.clone())
    }),
    ("ref", 0, |this, _, _| Ok(this.clone())),
    ("unref", 0, |this, _, _| Ok(this.clone())),
];
pub(crate) fn install(module: &JsObject, cx: &mut Context) -> JsResult<()> {
    let parent = stream_class("Duplex", cx)?;
    let (socket, _) = class(
        cx,
        ClassSpec {
            name: "Socket",
            parent: Some(&parent),
            init: init_socket,
            methods: METHODS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Socket"), socket.clone(), false, cx)?;
    module.set(js_string!("Stream"), socket, false, cx)?;
    let emitter = crate::events::emitter_class(cx)?;
    let (server, _) = class(
        cx,
        ClassSpec {
            name: "Server",
            parent: Some(&emitter),
            init: init_server,
            methods: SERVER_METHODS,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Server"), server, false, cx)?;
    Ok(())
}
pub(crate) fn address(address: SocketAddr, cx: &mut Context) -> JsResult<JsValue> {
    let o = object(cx);
    o.set(
        js_string!("address"),
        js_text(&address.ip().to_string()),
        false,
        cx,
    )?;
    o.set(
        js_string!("family"),
        js_text(if address.is_ipv4() { "IPv4" } else { "IPv6" }),
        false,
        cx,
    )?;
    o.set(js_string!("port"), address.port(), false, cx)?;
    Ok(o.into())
}
pub(crate) fn endpoints(
    target: &JsValue,
    local: SocketAddr,
    peer: SocketAddr,
    cx: &mut Context,
) -> JsResult<()> {
    let addr = address(local, cx)?;
    put(target, "__networkAddress", addr, cx)?;
    for (key, value) in [
        ("localAddress", js_text(&local.ip().to_string())),
        ("localPort", local.port().into()),
        (
            "localFamily",
            js_text(if local.is_ipv4() { "IPv4" } else { "IPv6" }),
        ),
        ("remoteAddress", js_text(&peer.ip().to_string())),
        ("remotePort", peer.port().into()),
        (
            "remoteFamily",
            js_text(if peer.is_ipv4() { "IPv4" } else { "IPv6" }),
        ),
    ] {
        put(target, key, value, cx)?;
    }
    Ok(())
}
pub(crate) fn deliver(id: Id, target: &JsObject, event: Event, cx: &mut Context) -> JsResult<()> {
    let this: JsValue = target.clone().into();
    match event {
        Event::Secure(protocol, alpn, cipher, cert) => {
            super::tls::secured(&this, protocol, alpn, cipher, cert, cx)?
        }
        Event::Dns(result) => super::dns::deliver(id, result, cx)?,
        Event::Connected(local, peer) => {
            endpoints(&this, local, peer, cx)?;
            put(&this, "connecting", false, cx)?;
            put(&this, "pending", false, cx)?;
            let no_delay = get(&this, "__networkNoDelay", cx)?;
            if !no_delay.is_undefined() {
                super::send(cx, Command::NoDelay(id, no_delay.to_boolean()));
            }
            let keep = get(&this, "__networkKeepAlive", cx)?;
            if !keep.is_undefined() {
                let delay = get(&this, "__networkKeepDelay", cx)?
                    .as_number()
                    .unwrap_or(0.) as u64;
                super::send(cx, Command::KeepAlive(id, keep.to_boolean(), delay));
            }
            emit(&this, "connect", &[], cx)?;
            emit(&this, "ready", &[], cx)?;
            if !get(&this, "__networkPaused", cx)?.to_boolean() {
                super::send(cx, Command::Read(id, true));
            }
        }
        Event::Listening(local) => {
            let addr = address(local, cx)?;
            put(&this, "__networkAddress", addr, cx)?;
            put(&this, "listening", true, cx)?;
            emit(&this, "listening", &[], cx)?;
        }
        Event::Accepted(ticket, local, peer) => {
            let secure = !get(&this, "__tlsOptions", cx)?.is_undefined();
            let module = crate::modules::node_module(cx, if secure { "tls" } else { "net" })?;
            let class = get(&module, if secure { "TLSSocket" } else { "Socket" }, cx)?
                .as_object()
                .ok_or_else(|| crate::native::type_error("Socket"))?;
            let options = get(&this, "__networkOptions", cx)?;
            let socket = class.construct(&[options], None, cx)?;
            let socket_value: JsValue = socket.clone().into();
            let child = attach(&socket, cx)?;
            endpoints(&socket_value, local, peer, cx)?;
            put(&socket_value, "pending", false, cx)?;
            super::send(cx, Command::Adopt(ticket, child));
            put(&socket_value, "server", this.clone(), cx)?;
            super::tls::accepted(&this, &socket_value, child, cx)?;
            let connections = get(&this, "__networkConnections", cx)?;
            invoke(&connections, "push", &[socket_value.clone()], cx)?;
            let close = closure(
                cx,
                "",
                0,
                |_, _, (server, socket): &(JsValue, JsValue), cx| {
                    let connections = get(server, "__networkConnections", cx)?;
                    let index = invoke(&connections, "indexOf", &[socket.clone()], cx)?;
                    invoke(&connections, "splice", &[index, 1.into()], cx)?;
                    if get(server, "__networkClosing", cx)?.to_boolean()
                        && get(&connections, "length", cx)?.as_number() == Some(0.)
                    {
                        emit(server, "close", &[], cx)?;
                    }
                    Ok(JsValue::undefined())
                },
                (this.clone(), socket_value.clone()),
            );
            invoke(&socket_value, "once", &[js_text("close"), close.into()], cx)?;
            emit(&this, "connection", &[socket_value.clone()], cx)?;
            if !get(&socket_value, "__networkPaused", cx)?.to_boolean() {
                super::send(cx, Command::Read(child, true));
            }
            super::send(cx, Command::Accept(id));
        }
        Event::Data(data) => {
            let n = data.len();
            let read = get(&this, "bytesRead", cx)?.as_number().unwrap_or(0.);
            put(&this, "bytesRead", read + n as f64, cx)?;
            let buffer = crate::encoding::make_buffer(cx, data)?;
            let more = invoke(&this, "push", &[buffer], cx)?.to_boolean();
            if more && !get(&this, "__networkPaused", cx)?.to_boolean() {
                super::send(cx, Command::Read(id, true));
            }
        }
        Event::Written(n) => {
            let written = get(&this, "bytesWritten", cx)?.as_number().unwrap_or(0.);
            put(&this, "bytesWritten", written + n as f64, cx)?;
            let callback = get(&this, "__networkWriteCallback", cx)?;
            put(&this, "__networkWriteCallback", JsValue::undefined(), cx)?;
            if callback.is_callable() {
                crate::call(cx, &callback, &JsValue::undefined(), &[])?;
            }
        }
        Event::End => {
            invoke(&this, "push", &[JsValue::null()], cx)?;
        }
        Event::Closed => {
            crate::shared(cx).network.borrow_mut().targets.remove(&id);
            if get(&this, "__networkClosing", cx)?.to_boolean() {
                let list = get(&this, "__networkConnections", cx)?;
                if get(&list, "length", cx)?.as_number() == Some(0.) {
                    emit(&this, "close", &[], cx)?;
                }
            } else if !get(&this, "destroyed", cx)?.to_boolean() {
                invoke(&this, "destroy", &[], cx)?;
            }
        }
        Event::Timeout => emit(&this, "timeout", &[], cx)?,
        Event::Error(code, message) => {
            let error = coded_error_value(cx, &format!("{code}: {message}"), &code);
            if get(&this, "_readableState", cx)?.is_object() {
                invoke(&this, "destroy", &[error], cx)?;
            } else {
                emit(&this, "error", &[error], cx)?;
            }
        }
    }
    Ok(())
}
