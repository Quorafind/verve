//! WHATWG WebSocket client using HTTP upgrade and tungstenite's incremental RFC 6455 engine.
//! Its transport is the existing native Duplex; no WebSocket creates an I/O thread of its own.
use crate::{
    dom_events::{dispatch, dom, new_event, new_event_of},
    native::{arg, array, closure, get, hidden, invoke, js_text, list_of, object, text_of},
};
use boa_engine::{Context, JsData, JsObject, JsResult, JsValue, js_string};
use boa_gc::{Finalize, Trace};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, Read, Write},
};
use tungstenite::{
    Message,
    protocol::{CloseFrame, Role, frame::coding::CloseCode},
};
#[derive(Debug, Default)]
struct Memory {
    input: VecDeque<u8>,
    output: Vec<u8>,
}
impl Read for Memory {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.input.is_empty() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = bytes.len().min(self.input.len());
        for byte in &mut bytes[..n] {
            if let Some(v) = self.input.pop_front() {
                *byte = v;
            }
        }
        Ok(n)
    }
}
impl Write for Memory {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Trace, Finalize, JsData)]
struct Wire {
    #[unsafe_ignore_trace]
    socket: RefCell<tungstenite::WebSocket<Memory>>,
    transport: JsValue,
}
fn wire(target: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    get(target, "__webSocket", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("WebSocket is not open"))
}
fn flush(target: &JsValue, payload: usize, cx: &mut Context) -> JsResult<()> {
    let native = wire(target, cx)?;
    let (bytes, transport) = {
        let native = native
            .downcast_ref::<Wire>()
            .ok_or_else(|| crate::native::type_error("WebSocket"))?;
        let mut ws = native.socket.borrow_mut();
        let _ = ws.flush();
        (
            std::mem::take(&mut ws.get_mut().output),
            native.transport.clone(),
        )
    };
    if bytes.is_empty() {
        return Ok(());
    }
    let callback = closure(
        cx,
        "",
        0,
        |_, _, (target, payload): &(JsValue, f64), _| {
            crate::messaging::socket_part(target, |_, _, _, buffered| {
                *buffered = (*buffered - *payload).max(0.)
            })?;
            Ok(JsValue::undefined())
        },
        (target.clone(), payload as f64),
    );
    let buffer = crate::encoding::make_buffer(cx, bytes)?;
    invoke(&transport, "write", &[buffer, callback.into()], cx)?;
    Ok(())
}
fn finish(
    target: &JsObject,
    code: u16,
    reason: &str,
    clean: bool,
    cx: &mut Context,
) -> JsResult<()> {
    let value: JsValue = target.clone().into();
    let was_closed = crate::messaging::socket_part(&value, |_, state, _, _| {
        let closed = *state == 3;
        *state = 3;
        closed
    })?;
    if was_closed {
        return Ok(());
    }
    let timer = get(&value, "__wsCloseTimer", cx)?;
    if !timer.is_undefined() {
        let clear = crate::native::global(cx, "clearTimeout")?;
        if let Some(clear) = clear.as_callable() {
            clear.call(&JsValue::undefined(), &[timer], cx)?;
        }
        hidden(
            target,
            js_string!("__wsCloseTimer"),
            JsValue::undefined(),
            cx,
        )?;
    }
    let class = dom(cx)?.close_event.clone();
    let close = new_event_of(
        cx,
        &class,
        "close",
        &[
            ("code", code.into()),
            ("reason", js_text(reason)),
            ("wasClean", clean.into()),
        ],
    )?;
    dispatch(target, &close, cx)?;
    Ok(())
}
fn fail(target: &JsObject, cx: &mut Context) -> JsResult<()> {
    if crate::messaging::socket_part(&target.clone().into(), |_, state, _, _| *state == 3)? {
        return Ok(());
    }
    let event = new_event(cx, "error")?;
    dispatch(target, &event, cx)?;
    finish(target, 1006, "", false, cx)
}
fn receive(target: &JsObject, bytes: Vec<u8>, cx: &mut Context) -> JsResult<()> {
    let value: JsValue = target.clone().into();
    let native = wire(&value, cx)?;
    {
        let native = native
            .downcast_ref::<Wire>()
            .ok_or_else(|| crate::native::type_error("WebSocket"))?;
        native.socket.borrow_mut().get_mut().input.extend(bytes);
    }
    for _ in 0..1024 {
        let message = {
            let native = native
                .downcast_ref::<Wire>()
                .ok_or_else(|| crate::native::type_error("WebSocket"))?;
            native.socket.borrow_mut().read()
        };
        match message {
            Ok(Message::Text(text)) => {
                let class = dom(cx)?.message_event.clone();
                let event =
                    new_event_of(cx, &class, "message", &[("data", js_text(text.as_str()))])?;
                dispatch(target, &event, cx)?;
            }
            Ok(Message::Binary(bytes)) => {
                let binary = crate::messaging::socket_part(&value, |_, _, kind, _| kind.clone())?;
                let buffer = crate::encoding::make_buffer(cx, bytes.to_vec())?;
                let data = if binary == "arraybuffer" {
                    get(&buffer, "buffer", cx)?
                } else {
                    let blob = crate::native::global(cx, "Blob")?
                        .as_object()
                        .ok_or_else(|| crate::native::type_error("Blob"))?;
                    let parts = array(cx, vec![buffer]);
                    blob.construct(&[parts], None, cx)?.into()
                };
                let class = dom(cx)?.message_event.clone();
                let event = new_event_of(cx, &class, "message", &[("data", data)])?;
                dispatch(target, &event, cx)?;
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {}
            Ok(Message::Close(frame)) => {
                flush(&value, 0, cx)?;
                let (code, reason) = frame
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((1005, String::new()));
                finish(target, code, &reason, true, cx)?;
                let transport = get(&value, "__wsTransport", cx)?;
                invoke(&transport, "end", &[], cx)?;
                break;
            }
            Ok(Message::Frame(_)) => {}
            Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                finish(target, 1006, "", false, cx)?;
                break;
            }
            Err(_) => {
                fail(target, cx)?;
                let transport = get(&value, "__wsTransport", cx)?;
                invoke(&transport, "destroy", &[], cx)?;
                break;
            }
        }
    }
    flush(&value, 0, cx)?;
    Ok(())
}
pub(crate) fn connect(
    target: &JsObject,
    url: &url::Url,
    protocols: &JsValue,
    cx: &mut Context,
) -> JsResult<bool> {
    let host = url
        .host_str()
        .unwrap_or("localhost")
        .trim_start_matches('[')
        .trim_end_matches(']');
    let https = url.scheme() == "wss";
    let port = url.port().unwrap_or(if https { 443 } else { 80 });
    if !crate::shared(cx)
        .network
        .borrow()
        .permissions
        .allows_outbound(host, port)
    {
        return Ok(false);
    }
    let mut offered = Vec::new();
    if protocols.is_string() {
        offered.push(text_of(protocols, cx)?);
    } else if !protocols.is_undefined() {
        for value in list_of(protocols, cx)? {
            offered.push(text_of(&value, cx)?);
        }
    }
    let mut seen = std::collections::HashSet::new();
    for protocol in &offered {
        if protocol.is_empty()
            || !protocol
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            || !seen.insert(protocol.clone())
        {
            return Err(crate::dom_events::dom_exception_error(
                cx,
                "Invalid or duplicate subprotocol",
                "SyntaxError",
            ));
        }
    }
    let key = tungstenite::handshake::client::generate_key();
    let expected = tungstenite::handshake::derive_accept_key(key.as_bytes());
    let headers = object(cx);
    for (name, value) in [
        ("Connection", "Upgrade"),
        ("Upgrade", "websocket"),
        ("Sec-WebSocket-Version", "13"),
        ("Sec-WebSocket-Key", key.as_str()),
    ] {
        headers.set(boa_engine::JsString::from(name), js_text(value), false, cx)?;
    }
    if !offered.is_empty() {
        headers.set(
            js_string!("Sec-WebSocket-Protocol"),
            js_text(&offered.join(", ")),
            false,
            cx,
        )?;
    }
    let options = object(cx);
    options.set(js_string!("host"), js_text(host), false, cx)?;
    options.set(js_string!("port"), port, false, cx)?;
    options.set(js_string!("agent"), false, false, cx)?;
    options.set(js_string!("headers"), headers, false, cx)?;
    let path = format!(
        "{}{}",
        url.path(),
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    );
    options.set(js_string!("path"), js_text(&path), false, cx)?;
    // Undici's WebSocket takes the realm's proxy as `fetch` does, through a CONNECT tunnel for
    // `ws:` as for `wss:`.
    if let Some(config) = super::proxy_config(cx) {
        match config.route(https, true, host, port) {
            Some(Ok(proxy)) => {
                let via = super::proxy::Via {
                    proxy,
                    tunnel: true,
                };
                let route = super::proxy::via_object(via);
                hidden(&options, js_string!("__verveVia"), route, cx)?;
            }
            Some(Err(_)) => return Ok(false),
            None => {}
        }
    }
    let http = crate::modules::node_module(cx, if https { "https" } else { "http" })?;
    let request = invoke(&http, "request", &[options.into()], cx)?;
    hidden(target, js_string!("__wsRequest"), request.clone(), cx)?;
    let upgrade = closure(
        cx,
        "",
        3,
        |_, args, (target, expected, offered): &(JsObject, String, Vec<String>), cx| {
            let response = arg(args, 0);
            let transport = arg(args, 1);
            let h = get(&response, "headers", cx)?;
            let accept = text_of(&get(&h, "sec-websocket-accept", cx)?, cx)?;
            let protocol = get(&h, "sec-websocket-protocol", cx)?;
            let selected = if protocol.is_undefined() {
                String::new()
            } else {
                text_of(&protocol, cx)?
            };
            if accept != *expected
                || !text_of(&get(&h, "upgrade", cx)?, cx)?.eq_ignore_ascii_case("websocket")
                || !text_of(&get(&h, "connection", cx)?, cx)?
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
                || (!selected.is_empty() && !offered.contains(&selected))
                || (!offered.is_empty() && selected.is_empty())
                || !get(&h, "sec-websocket-extensions", cx)?.is_undefined()
            {
                fail(target, cx)?;
                invoke(&transport, "destroy", &[], cx)?;
                return Ok(JsValue::undefined());
            }
            let ws = tungstenite::WebSocket::from_raw_socket(
                Memory::default(),
                Role::Client,
                Some(
                    tungstenite::protocol::WebSocketConfig::default()
                        .max_message_size(Some(16 * 1024 * 1024))
                        .max_frame_size(Some(16 * 1024 * 1024)),
                ),
            );
            let native = JsObject::from_proto_and_data(
                None,
                Wire {
                    socket: RefCell::new(ws),
                    transport: transport.clone(),
                },
            );
            hidden(target, js_string!("__webSocket"), native, cx)?;
            hidden(target, js_string!("__wsTransport"), transport.clone(), cx)?;
            hidden(target, js_string!("__wsProtocol"), js_text(&selected), cx)?;
            crate::messaging::socket_part(&target.clone().into(), |_, state, _, _| *state = 1)?;
            let data = closure(
                cx,
                "",
                1,
                |_, args, target: &JsObject, cx| {
                    let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
                    receive(target, bytes, cx)?;
                    Ok(JsValue::undefined())
                },
                target.clone(),
            );
            invoke(&transport, "on", &[js_text("data"), data.into()], cx)?;
            let close = closure(
                cx,
                "",
                0,
                |_, _, target: &JsObject, cx| {
                    finish(target, 1006, "", false, cx)?;
                    Ok(JsValue::undefined())
                },
                target.clone(),
            );
            invoke(&transport, "once", &[js_text("close"), close.into()], cx)?;
            let event = new_event(cx, "open")?;
            dispatch(target, &event, cx)?;
            let head = crate::encoding::bytes_of(&arg(args, 2), cx)?.unwrap_or_default();
            if !head.is_empty() {
                receive(target, head, cx)?;
            }
            Ok(JsValue::undefined())
        },
        (target.clone(), expected, offered),
    );
    invoke(&request, "once", &[js_text("upgrade"), upgrade.into()], cx)?;
    for name in ["error", "response"] {
        let error = closure(
            cx,
            "",
            0,
            |_, _, target: &JsObject, cx| {
                fail(target, cx)?;
                let request = get(&target.clone().into(), "__wsRequest", cx)?;
                let socket = get(&request, "socket", cx)?;
                if socket.is_object() {
                    invoke(&socket, "destroy", &[], cx)?;
                }
                Ok(JsValue::undefined())
            },
            target.clone(),
        );
        invoke(&request, "once", &[js_text(name), error.into()], cx)?;
    }
    invoke(&request, "end", &[], cx)?;
    Ok(true)
}
pub(crate) fn send(target: &JsValue, data: &JsValue, cx: &mut Context) -> JsResult<bool> {
    if !get(target, "__webSocket", cx)?.is_object() {
        return Ok(false);
    }
    let message = if data.is_string() {
        Message::Text(text_of(data, cx)?.into())
    } else if let Some(bytes) = crate::encoding::bytes_of(data, cx)? {
        Message::Binary(bytes.into())
    } else if get(data, "__bytes", cx)?.is_object() {
        Message::Binary(crate::web::blob_bytes(data, cx)?.into())
    } else {
        Message::Text(text_of(data, cx)?.into())
    };
    let size = message.len();
    let native = wire(target, cx)?;
    {
        let native = native
            .downcast_ref::<Wire>()
            .ok_or_else(|| crate::native::type_error("WebSocket"))?;
        native
            .socket
            .borrow_mut()
            .send(message)
            .map_err(|e| crate::native::type_error(e.to_string()))?;
    }
    crate::messaging::socket_part(target, |_, _, _, buffered| *buffered += size as f64)?;
    flush(target, size, cx)?;
    Ok(true)
}
pub(crate) fn close(
    target: &JsValue,
    code: u16,
    reason: String,
    cx: &mut Context,
) -> JsResult<bool> {
    if get(target, "__webSocket", cx)?.is_object() {
        let native = wire(target, cx)?;
        {
            let native = native
                .downcast_ref::<Wire>()
                .ok_or_else(|| crate::native::type_error("WebSocket"))?;
            native
                .socket
                .borrow_mut()
                .close(Some(CloseFrame {
                    code: CloseCode::from(code),
                    reason: reason.into(),
                }))
                .map_err(|e| crate::native::type_error(e.to_string()))?;
        }
        crate::messaging::socket_part(target, |_, state, _, _| *state = 2)?;
        flush(target, 0, cx)?;
        let timeout = closure(
            cx,
            "",
            0,
            |_, _, target: &JsValue, cx| {
                if crate::messaging::socket_part(target, |_, state, _, _| *state == 2)? {
                    if let Some(object) = target.as_object() {
                        finish(&object, 1006, "", false, cx)?;
                    }
                    let transport = get(target, "__wsTransport", cx)?;
                    invoke(&transport, "destroy", &[], cx)?;
                }
                Ok(JsValue::undefined())
            },
            target.clone(),
        );
        let set = crate::native::global(cx, "setTimeout")?;
        if let Some(set) = set.as_callable() {
            let timer = set.call(&JsValue::undefined(), &[timeout.into(), 30_000.into()], cx)?;
            if let Some(target) = target.as_object() {
                hidden(&target, js_string!("__wsCloseTimer"), timer, cx)?;
            }
        }
        return Ok(true);
    }
    let request = get(target, "__wsRequest", cx)?;
    if request.is_object() {
        invoke(&request, "abort", &[], cx)?;
        if let Some(target) = target.as_object() {
            fail(&target, cx)?;
        }
        return Ok(true);
    }
    Ok(false)
}
