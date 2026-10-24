//! HTTP/1.1 over native Duplex sockets. Parsing is incremental and body chunks inherit stream
//! backpressure. Agents pool only completely consumed responses; redirects are never followed.
use super::tcp::{self, emit, put};
use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, function, get, hidden, invoke,
    js_text, list_of, object, text_of,
};
use boa_engine::{
    Context, JsData, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
};
use boa_gc::{Finalize, Trace};
use std::cell::RefCell;

#[derive(Debug)]
struct Head {
    method: String,
    path: String,
    status: u16,
    reason: String,
    version: u8,
    headers: Vec<(String, String)>,
    upgrade: bool,
}
enum Action {
    Head(Head),
    Body(Vec<u8>),
    Trailers(Vec<(String, String)>),
    End,
}
enum Mode {
    Head,
    Length(usize),
    Size,
    Chunk(usize),
    ChunkEnd,
    Trailers,
    UntilClose,
    End,
    Upgraded,
}
struct Parser {
    buffer: Vec<u8>,
    mode: Mode,
    response: bool,
    head_request: bool,
}
impl Parser {
    fn new(response: bool) -> Self {
        Self {
            buffer: Vec::new(),
            mode: Mode::Head,
            response,
            head_request: false,
        }
    }
    fn step(&mut self) -> Result<Option<Action>, String> {
        match self.mode {
            Mode::Head => {
                let Some(end) = find(&self.buffer, b"\r\n\r\n") else {
                    if self.buffer.len() > 16384 {
                        return Err("HPE_HEADER_OVERFLOW".into());
                    }
                    return Ok(None);
                };
                if end + 4 > 16384 {
                    return Err("HPE_HEADER_OVERFLOW".into());
                }
                let mut slots = [httparse::EMPTY_HEADER; 128];
                let (method, path, status, reason, version, headers) = if self.response {
                    let mut parsed = httparse::Response::new(&mut slots);
                    parsed
                        .parse(&self.buffer[..end + 4])
                        .map_err(|_| "HPE_INVALID_HEADER_TOKEN")?;
                    (
                        String::new(),
                        String::new(),
                        parsed.code.unwrap_or(0),
                        parsed.reason.unwrap_or("").to_owned(),
                        parsed.version.unwrap_or(1),
                        parsed
                            .headers
                            .iter()
                            .map(|h| {
                                (
                                    h.name.to_owned(),
                                    String::from_utf8_lossy(h.value).into_owned(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                } else {
                    let mut parsed = httparse::Request::new(&mut slots);
                    parsed
                        .parse(&self.buffer[..end + 4])
                        .map_err(|_| "HPE_INVALID_HEADER_TOKEN")?;
                    (
                        parsed.method.unwrap_or("").to_owned(),
                        parsed.path.unwrap_or("").to_owned(),
                        0,
                        String::new(),
                        parsed.version.unwrap_or(1),
                        parsed
                            .headers
                            .iter()
                            .map(|h| {
                                (
                                    h.name.to_owned(),
                                    String::from_utf8_lossy(h.value).into_owned(),
                                )
                            })
                            .collect::<Vec<_>>(),
                    )
                };
                self.buffer.drain(..end + 4);
                let lengths: Vec<_> = headers
                    .iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .collect();
                if lengths.len() > 1 {
                    return Err("HPE_UNEXPECTED_CONTENT_LENGTH".into());
                }
                let length = lengths
                    .first()
                    .map(|(_, v)| v.parse::<usize>().map_err(|_| "HPE_INVALID_CONTENT_LENGTH"))
                    .transpose()?;
                let transfer = headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"))
                    .map(|(_, v)| v.to_ascii_lowercase());
                if transfer.is_some() && length.is_some() {
                    return Err("HPE_UNEXPECTED_CONTENT_LENGTH".into());
                }
                if transfer.as_ref().is_some_and(|v| !v.ends_with("chunked")) {
                    return Err("HPE_INVALID_TRANSFER_ENCODING".into());
                }
                let upgrade = (self.response && status == 101)
                    || (!self.response
                        && headers
                            .iter()
                            .any(|(k, _)| k.eq_ignore_ascii_case("upgrade")));
                self.mode = if upgrade {
                    Mode::Upgraded
                } else if self.response
                    && (self.head_request
                        || (100..200).contains(&status)
                        || status == 204
                        || status == 304)
                {
                    Mode::End
                } else if transfer.is_some() {
                    Mode::Size
                } else if let Some(length) = length {
                    if length == 0 {
                        Mode::End
                    } else {
                        Mode::Length(length)
                    }
                } else if self.response {
                    Mode::UntilClose
                } else {
                    Mode::End
                };
                Ok(Some(Action::Head(Head {
                    method,
                    path,
                    status,
                    reason,
                    version,
                    headers,
                    upgrade,
                })))
            }
            Mode::Length(left) => {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let size = left.min(self.buffer.len()).min(65536);
                let bytes = self.buffer.drain(..size).collect();
                self.mode = if size == left {
                    Mode::End
                } else {
                    Mode::Length(left - size)
                };
                Ok(Some(Action::Body(bytes)))
            }
            Mode::Size => {
                let Some(end) = find(&self.buffer, b"\r\n") else {
                    if self.buffer.len() > 8192 {
                        return Err("HPE_INVALID_CHUNK_SIZE".into());
                    }
                    return Ok(None);
                };
                let line = String::from_utf8_lossy(&self.buffer[..end]);
                let size = usize::from_str_radix(line.split(';').next().unwrap_or(""), 16)
                    .map_err(|_| "HPE_INVALID_CHUNK_SIZE")?;
                self.buffer.drain(..end + 2);
                self.mode = if size == 0 {
                    Mode::Trailers
                } else {
                    Mode::Chunk(size)
                };
                self.step()
            }
            Mode::Chunk(left) => {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let size = left.min(self.buffer.len()).min(65536);
                let bytes = self.buffer.drain(..size).collect();
                self.mode = if left == size {
                    Mode::ChunkEnd
                } else {
                    Mode::Chunk(left - size)
                };
                Ok(Some(Action::Body(bytes)))
            }
            Mode::ChunkEnd => {
                if self.buffer.len() < 2 {
                    return Ok(None);
                }
                if !self.buffer.starts_with(b"\r\n") {
                    return Err("HPE_INVALID_CHUNK_SIZE".into());
                }
                self.buffer.drain(..2);
                self.mode = Mode::Size;
                self.step()
            }
            Mode::Trailers => {
                if self.buffer.starts_with(b"\r\n") {
                    self.buffer.drain(..2);
                    self.mode = Mode::End;
                    return self.step();
                }
                let Some(end) = find(&self.buffer, b"\r\n\r\n") else {
                    if self.buffer.len() > 16384 {
                        return Err("HPE_HEADER_OVERFLOW".into());
                    }
                    return Ok(None);
                };
                let text = String::from_utf8_lossy(&self.buffer[..end]);
                let mut headers = Vec::new();
                for line in text.split("\r\n") {
                    let Some((name, value)) = line.split_once(':') else {
                        return Err("HPE_INVALID_HEADER_TOKEN".into());
                    };
                    headers.push((name.to_owned(), value.trim().to_owned()));
                }
                self.buffer.drain(..end + 4);
                self.mode = Mode::End;
                Ok(Some(Action::Trailers(headers)))
            }
            Mode::UntilClose => {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                let size = self.buffer.len().min(65536);
                Ok(Some(Action::Body(self.buffer.drain(..size).collect())))
            }
            Mode::End => {
                self.mode = Mode::Head;
                Ok(Some(Action::End))
            }
            Mode::Upgraded => Ok(None),
        }
    }
}
fn find(bytes: &[u8], pattern: &[u8]) -> Option<usize> {
    bytes.windows(pattern.len()).position(|v| v == pattern)
}
#[derive(Trace, Finalize, JsData)]
struct Data {
    #[unsafe_ignore_trace]
    parser: RefCell<Parser>,
}
fn parser(socket: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    get(socket, "__httpParser", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("HTTP parser"))
}
fn init_readable(target: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let readable = tcp::stream_class("Readable", cx)?;
    crate::call(cx, &readable.into(), &target.clone().into(), &[])?;
    target.set(js_string!("socket"), arg(args, 0), false, cx)?;
    target.set(js_string!("complete"), false, false, cx)?;
    let headers = object(cx);
    target.set(js_string!("headers"), headers, false, cx)?;
    let raw = array(cx, Vec::new());
    target.set(js_string!("rawHeaders"), raw, false, cx)?;
    Ok(())
}
fn init_outgoing(target: &JsObject, _: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let writable = tcp::stream_class("Writable", cx)?;
    let options = object(cx);
    options.set(js_string!("autoDestroy"), false, false, cx)?;
    crate::call(
        cx,
        &writable.into(),
        &target.clone().into(),
        &[options.into()],
    )?;
    target.set(js_string!("headersSent"), false, false, cx)?;
    target.set(js_string!("statusCode"), 200, false, cx)?;
    let headers = JsObject::with_null_proto();
    hidden(target, js_string!("__headers"), headers, cx)?;
    let names = object(cx);
    hidden(target, js_string!("__headerNames"), names, cx)?;
    Ok(())
}
fn headers(this: &JsValue, cx: &mut Context) -> JsResult<JsObject> {
    get(this, "__headers", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("HTTP headers"))
}
fn set_header(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    if get(this, "headersSent", cx)?.to_boolean() {
        return Err(crate::native::coded_error(
            cx,
            "Cannot set headers after they are sent to the client",
            "ERR_HTTP_HEADERS_SENT",
        ));
    }
    crate::sockets::validate_header_name(&JsValue::undefined(), args, cx)?;
    crate::sockets::validate_header_value(&JsValue::undefined(), args, cx)?;
    let name = text_of(&arg(args, 0), cx)?;
    let key = JsString::from(name.to_ascii_lowercase());
    headers(this, cx)?.set(key.clone(), arg(args, 1), false, cx)?;
    if let Some(names) = get(this, "__headerNames", cx)?.as_object() {
        names.set(key, js_text(&name), false, cx)?;
    }
    Ok(this.clone())
}
fn write_head(this: &JsValue, args: &[JsValue], cx: &mut Context) -> JsResult<JsValue> {
    let code = arg(args, 0).to_number(cx)?;
    if !(100. ..=999.).contains(&code) {
        return Err(crate::native::coded_error(
            cx,
            "Invalid status code",
            "ERR_HTTP_INVALID_STATUS_CODE",
        ));
    }
    put(this, "statusCode", code, cx)?;
    let reason = arg(args, 1);
    if reason.is_string() {
        put(this, "statusMessage", reason, cx)?;
    }
    let values = if arg(args, 1).is_object() {
        arg(args, 1)
    } else {
        arg(args, 2)
    };
    if let Some(values) = values.as_object() {
        if values.is_array() {
            let list = list_of(&values.into(), cx)?;
            for pair in list.chunks_exact(2) {
                set_header(this, pair, cx)?;
            }
        } else {
            for key in values.own_property_keys(cx)? {
                let value = values.get(key.clone(), cx)?;
                set_header(this, &[js_text(&key.to_string()), value], cx)?;
            }
        }
    }
    Ok(this.clone())
}
fn header_value(this: &JsValue, key: &str, cx: &mut Context) -> JsResult<JsValue> {
    headers(this, cx)?.get(JsString::from(key), cx)
}
fn start_head(this: &JsValue, body: bool, cx: &mut Context) -> JsResult<Vec<u8>> {
    if get(this, "headersSent", cx)?.to_boolean() {
        return Ok(Vec::new());
    }
    let client = get(this, "__httpClient", cx)?.to_boolean();
    let request = get(this, "req", cx)?;
    let code = get(this, "statusCode", cx)?.as_number().unwrap_or(200.) as u16;
    let no_body = !client
        && (code == 204
            || code == 304
            || (100..200).contains(&code)
            || crate::print(&get(&request, "method", cx)?) == "HEAD");
    put(this, "__httpNoBody", no_body, cx)?;
    let mut line = if client {
        format!(
            "{} {} HTTP/1.1\r\n",
            crate::print(&get(this, "method", cx)?),
            crate::print(&get(this, "path", cx)?)
        )
    } else {
        let reason = get(this, "statusMessage", cx)?;
        let reason = if reason.is_undefined() {
            crate::sockets::status_text(code).to_owned()
        } else {
            text_of(&reason, cx)?
        };
        format!("HTTP/1.1 {code} {reason}\r\n")
    };
    if header_value(this, "connection", cx)?.is_undefined() {
        let keep = get(this, "__httpKeepAlive", cx)?.to_boolean();
        set_header(
            this,
            &[
                js_text("Connection"),
                js_text(if keep { "keep-alive" } else { "close" }),
            ],
            cx,
        )?;
    }
    if !no_body
        && header_value(this, "content-length", cx)?.is_undefined()
        && header_value(this, "transfer-encoding", cx)?.is_undefined()
        && (body || !client)
    {
        set_header(
            this,
            &[js_text("Transfer-Encoding"), js_text("chunked")],
            cx,
        )?;
    }
    let chunked =
        crate::print(&header_value(this, "transfer-encoding", cx)?).eq_ignore_ascii_case("chunked");
    put(this, "__httpChunked", chunked, cx)?;
    let values = headers(this, cx)?;
    let names = get(this, "__headerNames", cx)?;
    for key in values.own_property_keys(cx)? {
        let value = values.get(key.clone(), cx)?;
        let name = get(&names, &key.to_string(), cx)?;
        let name = if name.is_undefined() {
            key.to_string()
        } else {
            crate::print(&name)
        };
        let values = if value.as_object().is_some_and(|v| v.is_array()) {
            list_of(&value, cx)?
        } else {
            vec![value]
        };
        for value in values {
            line.push_str(&format!("{name}: {}\r\n", text_of(&value, cx)?));
        }
    }
    line.push_str("\r\n");
    put(this, "headersSent", true, cx)?;
    put(this, "_header", js_text(&line), cx)?;
    Ok(line.into_bytes())
}
fn wire(this: &JsValue, bytes: Vec<u8>, callback: JsValue, cx: &mut Context) -> JsResult<()> {
    let socket = get(this, "socket", cx)?;
    if !socket.is_object() {
        return Err(crate::native::coded_error(
            cx,
            "Socket is not assigned",
            "ERR_SOCKET_CLOSED",
        ));
    }
    let bytes = crate::encoding::make_buffer(cx, bytes)?;
    invoke(&socket, "write", &[bytes, callback], cx)?;
    Ok(())
}
const OUTGOING: &[(&str, usize, NativeFunctionPointer)] = &[
    ("setHeader", 2, set_header),
    ("getHeader", 1, |this, args, cx| {
        header_value(this, &text_of(&arg(args, 0), cx)?.to_ascii_lowercase(), cx)
    }),
    ("hasHeader", 1, |this, args, cx| {
        Ok(
            (!header_value(this, &text_of(&arg(args, 0), cx)?.to_ascii_lowercase(), cx)?
                .is_undefined())
            .into(),
        )
    }),
    ("removeHeader", 1, |this, args, cx| {
        let key = text_of(&arg(args, 0), cx)?.to_ascii_lowercase();
        headers(this, cx)?.delete_property_or_throw(JsString::from(key), cx)?;
        Ok(JsValue::undefined())
    }),
    ("getHeaders", 0, |this, _, cx| Ok(headers(this, cx)?.into())),
    ("getHeaderNames", 0, |this, _, cx| {
        let values = headers(this, cx)?
            .own_property_keys(cx)?
            .iter()
            .map(|k| js_text(&k.to_string()))
            .collect();
        Ok(array(cx, values))
    }),
    ("getRawHeaderNames", 0, |this, _, cx| {
        let names = get(this, "__headerNames", cx)?;
        let mut values = Vec::new();
        for key in headers(this, cx)?.own_property_keys(cx)? {
            values.push(get(&names, &key.to_string(), cx)?);
        }
        Ok(array(cx, values))
    }),
    ("appendHeader", 2, |this, args, cx| {
        let old = header_value(this, &text_of(&arg(args, 0), cx)?.to_ascii_lowercase(), cx)?;
        if old.is_undefined() {
            return set_header(this, args, cx);
        }
        let values = if old.as_object().is_some_and(|v| v.is_array()) {
            old
        } else {
            array(cx, vec![old])
        };
        invoke(&values, "push", &[arg(args, 1)], cx)?;
        set_header(this, &[arg(args, 0), values], cx)
    }),
    ("writeHead", 3, write_head),
    ("flushHeaders", 0, |this, _, cx| {
        let bytes = start_head(this, true, cx)?;
        if !bytes.is_empty() {
            wire(this, bytes, JsValue::undefined(), cx)?;
        }
        Ok(JsValue::undefined())
    }),
    ("_write", 3, |this, args, cx| {
        let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?
            .unwrap_or_else(|| crate::print(&arg(args, 0)).into_bytes());
        let mut data = start_head(this, true, cx)?;
        if !get(this, "__httpNoBody", cx)?.to_boolean() {
            if get(this, "__httpChunked", cx)?.to_boolean() {
                data.extend_from_slice(format!("{:x}\r\n", bytes.len()).as_bytes());
                data.extend(bytes);
                data.extend_from_slice(b"\r\n");
            } else {
                data.extend(bytes);
            }
        }
        wire(this, data, arg(args, 2), cx)?;
        Ok(JsValue::undefined())
    }),
    ("_final", 1, |this, args, cx| {
        let mut bytes = start_head(this, false, cx)?;
        if get(this, "__httpChunked", cx)?.to_boolean()
            && !get(this, "__httpNoBody", cx)?.to_boolean()
        {
            bytes.extend_from_slice(b"0\r\n");
            let trailers = get(this, "__httpTrailers", cx)?;
            if trailers.is_object() {
                let keys = trailers
                    .as_object()
                    .map(|o| o.own_property_keys(cx))
                    .transpose()?
                    .unwrap_or_default();
                for key in keys {
                    let value = get(&trailers, &key.to_string(), cx)?;
                    bytes.extend_from_slice(
                        format!("{}: {}\r\n", key, text_of(&value, cx)?).as_bytes(),
                    );
                }
            }
            bytes.extend_from_slice(b"\r\n");
        }
        let callback = arg(args, 0);
        let target = this.clone();
        let done = closure(
            cx,
            "",
            0,
            |_, args, (target, cb): &(JsValue, JsValue), cx| {
                put(target, "finished", true, cx)?;
                if !get(target, "__httpClient", cx)?.to_boolean() {
                    let socket = get(target, "socket", cx)?;
                    let server = get(&socket, "server", cx)?;
                    if !get(target, "__httpKeepAlive", cx)?.to_boolean()
                        || get(&server, "__networkClosing", cx)?.to_boolean()
                    {
                        invoke(&socket, "end", &[], cx)?;
                    }
                }
                if cb.is_callable() {
                    crate::call(cx, cb, &JsValue::undefined(), args)?;
                }
                Ok(JsValue::undefined())
            },
            (target, callback),
        );
        wire(this, bytes, done.into(), cx)?;
        Ok(JsValue::undefined())
    }),
    ("_destroy", 2, |this, args, cx| {
        let error = arg(args, 0);
        if error.is_object() {
            let socket = get(this, "socket", cx)?;
            if socket.is_object() {
                invoke(&socket, "destroy", std::slice::from_ref(&error), cx)?;
            }
        }
        let cb = arg(args, 1);
        if cb.is_callable() {
            crate::call(cx, &cb, &JsValue::undefined(), &[error])?;
        }
        Ok(JsValue::undefined())
    }),
    ("setTimeout", 2, |this, args, cx| {
        let socket = get(this, "socket", cx)?;
        invoke(&socket, "setTimeout", args, cx)?;
        Ok(this.clone())
    }),
    ("setNoDelay", 1, |this, args, cx| {
        let socket = get(this, "socket", cx)?;
        invoke(&socket, "setNoDelay", args, cx)?;
        Ok(this.clone())
    }),
    ("setSocketKeepAlive", 2, |this, args, cx| {
        let socket = get(this, "socket", cx)?;
        invoke(&socket, "setKeepAlive", args, cx)?;
        Ok(this.clone())
    }),
    ("abort", 0, |this, _, cx| {
        put(this, "aborted", true, cx)?;
        emit(this, "abort", &[], cx)?;
        let socket = get(this, "socket", cx)?;
        invoke(&socket, "destroy", &[], cx)?;
        Ok(JsValue::undefined())
    }),
    ("addTrailers", 1, |this, args, cx| {
        put(this, "__httpTrailers", arg(args, 0), cx)?;
        Ok(JsValue::undefined())
    }),
];

fn assign_headers(
    message: &JsValue,
    values: &[(String, String)],
    trailers: bool,
    join: bool,
    cx: &mut Context,
) -> JsResult<()> {
    let headers = JsObject::with_null_proto();
    let distinct = JsObject::with_null_proto();
    let mut raw = Vec::new();
    const SINGLE: &[&str] = &[
        "age",
        "authorization",
        "content-length",
        "content-type",
        "etag",
        "expires",
        "from",
        "host",
        "if-modified-since",
        "if-unmodified-since",
        "last-modified",
        "location",
        "max-forwards",
        "proxy-authorization",
        "referer",
        "retry-after",
        "server",
        "user-agent",
    ];
    for (name, value) in values {
        raw.push(js_text(name));
        raw.push(js_text(value));
        let key = name.to_ascii_lowercase();
        let old = headers.get(JsString::from(key.as_str()), cx)?;
        let all = distinct.get(JsString::from(key.as_str()), cx)?;
        let all = if all.is_undefined() {
            array(cx, Vec::new())
        } else {
            all
        };
        invoke(&all, "push", &[js_text(value)], cx)?;
        distinct.set(JsString::from(key.as_str()), all, false, cx)?;
        let new = if key == "set-cookie" {
            let values = if old.is_undefined() {
                array(cx, Vec::new())
            } else {
                old
            };
            invoke(&values, "push", &[js_text(value)], cx)?;
            values
        } else if old.is_undefined() {
            js_text(value)
        } else if SINGLE.contains(&key.as_str()) && !join {
            old
        } else {
            js_text(&format!(
                "{}{}{value}",
                crate::print(&old),
                if key == "cookie" { "; " } else { ", " }
            ))
        };
        headers.set(JsString::from(key), new, false, cx)?;
    }
    let raw = array(cx, raw);
    put(
        message,
        if trailers { "trailers" } else { "headers" },
        headers,
        cx,
    )?;
    put(
        message,
        if trailers {
            "rawTrailers"
        } else {
            "rawHeaders"
        },
        raw,
        cx,
    )?;
    put(
        message,
        if trailers {
            "trailersDistinct"
        } else {
            "headersDistinct"
        },
        distinct,
        cx,
    )?;
    Ok(())
}
fn new_message(socket: &JsValue, cx: &mut Context) -> JsResult<JsValue> {
    let module = crate::modules::node_module(cx, "http")?;
    let ctor = get(&module, "IncomingMessage", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("IncomingMessage"))?;
    Ok(ctor
        .construct(std::slice::from_ref(socket), None, cx)?
        .into())
}
fn on_data(socket: &JsValue, data: Vec<u8>, cx: &mut Context) -> JsResult<()> {
    let native = parser(socket, cx)?;
    {
        let native = native
            .downcast_ref::<Data>()
            .ok_or_else(|| crate::native::type_error("HTTP parser"))?;
        native.parser.borrow_mut().buffer.extend(data);
    }
    for _ in 0..1024 {
        let action = {
            let native = native
                .downcast_ref::<Data>()
                .ok_or_else(|| crate::native::type_error("HTTP parser"))?;
            native.parser.borrow_mut().step()
        };
        let action = match action {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(code) => {
                let e = coded_error_value(cx, &format!("Parse Error: {code}"), &code);
                let server = get(socket, "__httpServer", cx)?;
                if server.is_object() {
                    emit(&server, "clientError", &[e.clone(), socket.clone()], cx)?;
                }
                invoke(socket, "destroy", &[e], cx)?;
                break;
            }
        };
        match action {
            Action::Head(head) => {
                let request = get(socket, "__httpRequest", cx)?;
                let server = get(socket, "__httpServer", cx)?;
                let message = new_message(socket, cx)?;
                put(socket, "__httpMessage", message.clone(), cx)?;
                put(
                    &message,
                    "httpVersion",
                    js_text(&format!("1.{}", head.version)),
                    cx,
                )?;
                put(&message, "httpVersionMajor", 1, cx)?;
                put(&message, "httpVersionMinor", head.version, cx)?;
                let options = if request.is_object() {
                    get(&request, "__httpOptions", cx)?
                } else {
                    get(&server, "__networkOptions", cx)?
                };
                let join = get(&options, "joinDuplicateHeaders", cx)?.to_boolean();
                assign_headers(&message, &head.headers, false, join, cx)?;
                if request.is_object() {
                    put(&message, "statusCode", head.status, cx)?;
                    put(&message, "statusMessage", js_text(&head.reason), cx)?;
                    put(&request, "res", message.clone(), cx)?;
                    put(&message, "req", request.clone(), cx)?;
                } else {
                    put(&message, "method", js_text(&head.method), cx)?;
                    put(&message, "url", js_text(&head.path), cx)?;
                    put(&message, "connection", socket.clone(), cx)?;
                }
                if head.upgrade {
                    if request.is_object() {
                        let agent = get(&request, "agent", cx)?;
                        let key = text_of(&get(&request, "__httpOrigin", cx)?, cx)?;
                        pool_remove(&agent, "sockets", &key, socket, cx)?;
                    }
                    put(&message, "complete", true, cx)?;
                    put(socket, "__httpMessage", JsValue::undefined(), cx)?;
                    put(socket, "__httpUpgraded", true, cx)?;
                    let bytes = {
                        let native = native
                            .downcast_ref::<Data>()
                            .ok_or_else(|| crate::native::type_error("HTTP parser"))?;
                        let mut parser = native.parser.borrow_mut();
                        std::mem::take(&mut parser.buffer)
                    };
                    let bytes = crate::encoding::make_buffer(cx, bytes)?;
                    let handler = get(socket, "__httpDataHandler", cx)?;
                    invoke(socket, "removeListener", &[js_text("data"), handler], cx)?;
                    emit(
                        if request.is_object() {
                            &request
                        } else {
                            &server
                        },
                        "upgrade",
                        &[message, socket.clone(), bytes],
                        cx,
                    )?;
                    break;
                }
                if request.is_object() {
                    if (100..200).contains(&head.status) {
                        emit(&request, "information", &[message], cx)?;
                        continue;
                    }
                    let done = closure(
                        cx,
                        "",
                        0,
                        |_, _, (request, socket): &(JsValue, JsValue), cx| {
                            release(request, socket, cx)?;
                            Ok(JsValue::undefined())
                        },
                        (request.clone(), socket.clone()),
                    );
                    invoke(&message, "once", &[js_text("end"), done.into()], cx)?;
                    emit(&request, "response", &[message], cx)?;
                } else {
                    let module = crate::modules::node_module(cx, "http")?;
                    let ctor = get(&module, "ServerResponse", cx)?
                        .as_object()
                        .ok_or_else(|| crate::native::type_error("ServerResponse"))?;
                    let response: JsValue = ctor
                        .construct(std::slice::from_ref(&message), None, cx)?
                        .into();
                    put(&response, "socket", socket.clone(), cx)?;
                    put(&response, "connection", socket.clone(), cx)?;
                    put(&response, "req", message.clone(), cx)?;
                    put(socket, "__httpResponse", response.clone(), cx)?;
                    put(&message, "res", response.clone(), cx)?;
                    let connection = head
                        .headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("connection"))
                        .map(|(_, v)| v.to_ascii_lowercase());
                    let keep = connection.as_deref() != Some("close")
                        && (head.version == 1 || connection.as_deref() == Some("keep-alive"));
                    put(&response, "__httpKeepAlive", keep, cx)?;
                    emit(&server, "request", &[message, response], cx)?;
                }
            }
            Action::Body(bytes) => {
                let message = get(socket, "__httpMessage", cx)?;
                let bytes = crate::encoding::make_buffer(cx, bytes)?;
                if !invoke(&message, "push", &[bytes], cx)?.to_boolean() {
                    invoke(socket, "pause", &[], cx)?;
                    break;
                }
            }
            Action::Trailers(values) => {
                let message = get(socket, "__httpMessage", cx)?;
                assign_headers(&message, &values, true, false, cx)?;
            }
            Action::End => {
                let message = get(socket, "__httpMessage", cx)?;
                if message.is_object() {
                    put(&message, "complete", true, cx)?;
                    invoke(&message, "push", &[JsValue::null()], cx)?;
                }
                put(socket, "__httpMessage", JsValue::undefined(), cx)?;
            }
        }
    }
    Ok(())
}
fn attach(socket: &JsValue, response: bool, server: JsValue, cx: &mut Context) -> JsResult<()> {
    if get(socket, "__httpParser", cx)?.is_object() {
        return Ok(());
    }
    let native = JsObject::from_proto_and_data(
        None,
        Data {
            parser: RefCell::new(Parser::new(response)),
        },
    );
    put(socket, "__httpParser", native, cx)?;
    put(socket, "__httpServer", server, cx)?;
    let data = closure(
        cx,
        "",
        1,
        |_, args, socket: &JsValue, cx| {
            let bytes = crate::encoding::bytes_of(&arg(args, 0), cx)?.unwrap_or_default();
            on_data(socket, bytes, cx)?;
            Ok(JsValue::undefined())
        },
        socket.clone(),
    );
    put(socket, "__httpDataHandler", data.clone(), cx)?;
    invoke(socket, "on", &[js_text("data"), data.into()], cx)?;
    let end = closure(
        cx,
        "",
        0,
        |_, _, socket: &JsValue, cx| {
            let native = parser(socket, cx)?;
            let until_close = native
                .downcast_ref::<Data>()
                .is_some_and(|p| matches!(p.parser.borrow().mode, Mode::UntilClose));
            let message = get(socket, "__httpMessage", cx)?;
            if message.is_object() {
                if until_close {
                    put(&message, "complete", true, cx)?;
                    invoke(&message, "push", &[JsValue::null()], cx)?;
                } else if !get(&message, "complete", cx)?.to_boolean() {
                    put(&message, "aborted", true, cx)?;
                    emit(&message, "aborted", &[], cx)?;
                    let e = coded_error_value(cx, "aborted", "ECONNRESET");
                    invoke(&message, "destroy", &[e], cx)?;
                }
            }
            Ok(JsValue::undefined())
        },
        socket.clone(),
    );
    invoke(socket, "on", &[js_text("end"), end.into()], cx)?;
    let errors = closure(
        cx,
        "",
        1,
        |_, args, socket: &JsValue, cx| {
            let request = get(socket, "__httpRequest", cx)?;
            if request.is_object() {
                put(&request, "__httpErrored", true, cx)?;
                emit(&request, "error", &[arg(args, 0)], cx)?;
            }
            Ok(JsValue::undefined())
        },
        socket.clone(),
    );
    invoke(socket, "on", &[js_text("error"), errors.into()], cx)?;
    let closed = closure(
        cx,
        "",
        0,
        |_, _, socket: &JsValue, cx| {
            let agent = get(socket, "__httpAgent", cx)?;
            let key = text_of(&get(socket, "__httpPoolKey", cx)?, cx)?;
            for map in ["sockets", "freeSockets"] {
                pool_remove(&agent, map, &key, socket, cx)?;
            }
            let message = get(socket, "__httpMessage", cx)?;
            if message.is_object()
                && !get(&message, "complete", cx)?.to_boolean()
                && !get(&message, "destroyed", cx)?.to_boolean()
            {
                put(&message, "aborted", true, cx)?;
                emit(&message, "aborted", &[], cx)?;
                let error = coded_error_value(cx, "aborted", "ECONNRESET");
                invoke(&message, "destroy", &[error], cx)?;
            }
            let request = get(socket, "__httpRequest", cx)?;
            if request.is_object()
                && !get(socket, "__httpUpgraded", cx)?.to_boolean()
                && !get(&request, "__httpReleased", cx)?.to_boolean()
            {
                put(&request, "__httpReleased", true, cx)?;
                if get(&request, "res", cx)?.is_undefined()
                    && !get(&request, "destroyed", cx)?.to_boolean()
                    && !get(&request, "__httpErrored", cx)?.to_boolean()
                {
                    let error = coded_error_value(cx, "socket hang up", "ECONNRESET");
                    emit(&request, "error", &[error], cx)?;
                }
                emit(&request, "close", &[], cx)?;
            }
            Ok(JsValue::undefined())
        },
        socket.clone(),
    );
    invoke(socket, "on", &[js_text("close"), closed.into()], cx)?;
    Ok(())
}
fn agent_init(target: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let emitter = crate::events::emitter_class(cx)?;
    crate::call(cx, &emitter.into(), &target.clone().into(), &[])?;
    let options = arg(args, 0);
    target.set(js_string!("options"), options.clone(), false, cx)?;
    target.set(
        js_string!("keepAlive"),
        get(&options, "keepAlive", cx)?.to_boolean(),
        false,
        cx,
    )?;
    for (key, default) in [
        ("maxSockets", f64::INFINITY),
        ("maxTotalSockets", f64::INFINITY),
        ("maxFreeSockets", 256.),
    ] {
        let value = get(&options, key, cx)?;
        target.set(
            JsString::from(key),
            if value.is_undefined() {
                default.into()
            } else {
                value
            },
            false,
            cx,
        )?;
    }
    for key in ["sockets", "freeSockets", "requests"] {
        target.set(JsString::from(key), JsObject::with_null_proto(), false, cx)?;
    }
    Ok(())
}
fn origin(options: &JsValue, cx: &mut Context) -> JsResult<String> {
    use sha2::Digest;
    let mut digest = sha2::Sha256::new();
    for name in [
        "ca",
        "cert",
        "key",
        "ciphers",
        "minVersion",
        "maxVersion",
        "ALPNProtocols",
        "family",
        "localAddress",
        "localPort",
    ] {
        let value = get(options, name, cx)?;
        let values = if value.as_object().is_some_and(|o| o.is_array()) {
            list_of(&value, cx)?
        } else {
            vec![value]
        };
        digest.update(name.as_bytes());
        for value in values {
            let bytes = crate::encoding::bytes_of(&value, cx)?
                .unwrap_or_else(|| crate::print(&value).into_bytes());
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
    }
    // Custom identity/transport callbacks and opaque contexts must not share a connection with
    // a different policy. Conservatively use a fresh key rather than stringify a function.
    let mut nonce = 0;
    for name in ["checkServerIdentity", "secureContext", "createConnection"] {
        if get(options, name, cx)?.is_object() {
            let shared = crate::shared(cx);
            let mut net = shared.network.borrow_mut();
            net.next += 1;
            nonce = net.next;
            break;
        }
    }
    Ok(format!(
        "{}:{}:{}:{}:{}:{:x}:{nonce}",
        crate::print(&get(options, "protocol", cx)?),
        crate::print(&get(options, "hostname", cx)?),
        crate::print(&get(options, "port", cx)?),
        crate::print(&get(options, "servername", cx)?),
        crate::print(&get(options, "rejectUnauthorized", cx)?),
        digest.finalize()
    ))
}
fn pool_remove(
    agent: &JsValue,
    collection: &str,
    key: &str,
    socket: &JsValue,
    cx: &mut Context,
) -> JsResult<()> {
    let map = get(agent, collection, cx)?;
    let list = get(&map, key, cx)?;
    if list.is_object() {
        let index = invoke(&list, "indexOf", std::slice::from_ref(socket), cx)?;
        if index.as_number().is_some_and(|i| i >= 0.) {
            invoke(&list, "splice", &[index, 1.into()], cx)?;
        }
        if get(&list, "length", cx)?.as_number() == Some(0.)
            && let Some(map) = map.as_object()
        {
            map.delete_property_or_throw(JsString::from(key), cx)?;
        }
    }
    Ok(())
}
fn release(request: &JsValue, socket: &JsValue, cx: &mut Context) -> JsResult<()> {
    if get(request, "__httpReleased", cx)?.to_boolean() {
        return Ok(());
    }
    put(request, "__httpReleased", true, cx)?;
    let message = get(request, "res", cx)?;
    let incoming = get(&message, "headers", cx)?;
    let connection = get(&incoming, "connection", cx)?;
    let agent = get(request, "agent", cx)?;
    let key = text_of(&get(request, "__httpOrigin", cx)?, cx)?;
    pool_remove(&agent, "sockets", &key, socket, cx)?;
    let keep = get(&agent, "keepAlive", cx)?.to_boolean()
        && !crate::print(&connection).eq_ignore_ascii_case("close")
        && get(request, "finished", cx)?.to_boolean();
    put(socket, "__httpRequest", JsValue::undefined(), cx)?;
    emit(request, "close", &[], cx)?;
    if keep && !get(socket, "destroyed", cx)?.to_boolean() {
        let key = text_of(&get(request, "__httpOrigin", cx)?, cx)?;
        let free = get(&agent, "freeSockets", cx)?;
        let list = get(&free, &key, cx)?;
        let list = if list.is_undefined() {
            array(cx, Vec::new())
        } else {
            list
        };
        let max = get(&agent, "maxFreeSockets", cx)?
            .as_number()
            .unwrap_or(256.);
        if get(&list, "length", cx)?.as_number().unwrap_or(0.) < max {
            invoke(&list, "push", std::slice::from_ref(socket), cx)?;
            put(&free, &key, list, cx)?;
            emit(
                &agent,
                "free",
                &[socket.clone(), get(request, "__httpOptions", cx)?],
                cx,
            )?;
            return Ok(());
        }
    }
    invoke(socket, "end", &[], cx)?;
    Ok(())
}
fn normalize(args: &[JsValue], https: bool, cx: &mut Context) -> JsResult<JsValue> {
    let first = arg(args, 0);
    let options = object(cx);
    let input = if first.is_string() || get(&first, "href", cx)?.is_string() {
        let href = if first.is_string() {
            text_of(&first, cx)?
        } else {
            text_of(&get(&first, "href", cx)?, cx)?
        };
        let url = url::Url::parse(&href).map_err(|_| crate::native::type_error("Invalid URL"))?;
        options.set(
            js_string!("protocol"),
            js_text(&format!("{}:", url.scheme())),
            false,
            cx,
        )?;
        options.set(
            js_string!("hostname"),
            js_text(
                url.host_str()
                    .unwrap_or("localhost")
                    .trim_matches(['[', ']']),
            ),
            false,
            cx,
        )?;
        options.set(
            js_string!("port"),
            url.port().unwrap_or(if https { 443 } else { 80 }),
            false,
            cx,
        )?;
        let path = format!(
            "{}{}",
            url.path(),
            url.query().map(|q| format!("?{q}")).unwrap_or_default()
        );
        options.set(js_string!("path"), js_text(&path), false, cx)?;
        arg(args, 1)
    } else {
        first
    };
    if let Some(input) = input.as_object() {
        for key in input.own_property_keys(cx)? {
            let value = input.get(key.clone(), cx)?;
            options.set(key, value, false, cx)?;
        }
    }
    if options.get(js_string!("hostname"), cx)?.is_undefined() {
        let host = options.get(js_string!("host"), cx)?;
        options.set(
            js_string!("hostname"),
            if host.is_undefined() {
                js_text("localhost")
            } else {
                host
            },
            false,
            cx,
        )?;
    }
    if options.get(js_string!("port"), cx)?.is_undefined() {
        options.set(js_string!("port"), if https { 443 } else { 80 }, false, cx)?;
    }
    if options.get(js_string!("path"), cx)?.is_undefined() {
        options.set(js_string!("path"), js_text("/"), false, cx)?;
    }
    let protocol = options.get(js_string!("protocol"), cx)?;
    if !protocol.is_undefined() && crate::print(&protocol) != if https { "https:" } else { "http:" }
    {
        return Err(crate::native::coded_error(
            cx,
            "Protocol not supported by this transport",
            "ERR_INVALID_PROTOCOL",
        ));
    }
    if protocol.is_undefined() {
        options.set(
            js_string!("protocol"),
            js_text(if https { "https:" } else { "http:" }),
            false,
            cx,
        )?;
    }
    let host = options.get(js_string!("hostname"), cx)?;
    let host = js_text(text_of(&host, cx)?.trim_matches(['[', ']']));
    options.set(js_string!("hostname"), host.clone(), false, cx)?;
    options.set(js_string!("host"), host, false, cx)?;
    Ok(options.into())
}
fn request(args: &[JsValue], https: bool, cx: &mut Context) -> JsResult<JsValue> {
    let options = normalize(args, https, cx)?;
    let module = crate::modules::node_module(cx, if https { "https" } else { "http" })?;
    let configured = get(&options, "agent", cx)?;
    let agent = if configured.is_undefined() {
        get(&module, "globalAgent", cx)?
    } else {
        configured
    };
    if let Some(defaults) = get(&agent, "options", cx)?.as_object() {
        for key in defaults.own_property_keys(cx)? {
            if let Some(options) = options.as_object()
                && options.get(key.clone(), cx)?.is_undefined()
            {
                let value = defaults.get(key.clone(), cx)?;
                options.set(key, value, false, cx)?;
            }
        }
    }
    let host = text_of(&get(&options, "hostname", cx)?, cx)?;
    let number = get(&options, "port", cx)?.to_number(cx)?;
    if !number.is_finite() || number.fract() != 0. || !(0. ..=65535.).contains(&number) {
        return Err(crate::native::coded_error(
            cx,
            "Port should be >= 0 and < 65536.",
            "ERR_SOCKET_BAD_PORT",
        ));
    }
    let port = number as u16;
    if !crate::shared(cx)
        .network
        .borrow()
        .permissions
        .allows_outbound(&host, port)
    {
        return crate::sockets::denied_http(cx);
    }
    let ctor = get(&module, "ClientRequest", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("ClientRequest"))?;
    let req: JsValue = ctor.construct(&[], None, cx)?.into();
    put(&req, "__httpClient", true, cx)?;
    put(&req, "__httpOptions", options.clone(), cx)?;
    put(&req, "path", get(&options, "path", cx)?, cx)?;
    let method = get(&options, "method", cx)?;
    put(
        &req,
        "method",
        if method.is_undefined() {
            js_text("GET")
        } else {
            js_text(&text_of(&method, cx)?.to_ascii_uppercase())
        },
        cx,
    )?;
    put(&req, "agent", agent.clone(), cx)?;
    let keep = get(&agent, "keepAlive", cx)?.to_boolean();
    put(&req, "__httpKeepAlive", keep, cx)?;
    let key = origin(&options, cx)?;
    put(&req, "__httpOrigin", js_text(&key), cx)?;
    let free = get(&agent, "freeSockets", cx)?;
    let list = get(&free, &key, cx)?;
    let mut reused = JsValue::undefined();
    if list.is_object() {
        loop {
            let candidate = invoke(&list, "pop", &[], cx)?;
            if candidate.is_undefined() {
                break;
            }
            if !get(&candidate, "destroyed", cx)?.to_boolean() {
                reused = candidate;
                break;
            }
        }
    }
    let socket = if reused.is_object() {
        put(&req, "reusedSocket", true, cx)?;
        reused
    } else {
        put(&req, "reusedSocket", false, cx)?;
        // HTTP's request path is not net.Socket's IPC path. Only socketPath is a transport path;
        // leaving `/resource` here would incorrectly route every HTTP request into the IPC refusal.
        let connection_options = object(cx);
        if let Some(options) = options.as_object() {
            for property in options.own_property_keys(cx)? {
                if property.to_string() != "path" {
                    let value = options.get(property.clone(), cx)?;
                    connection_options.set(property, value, false, cx)?;
                }
            }
        }
        let socket_path = get(&options, "socketPath", cx)?;
        if !socket_path.is_undefined() {
            connection_options.set(js_string!("path"), socket_path, false, cx)?;
        }
        let connection_options: JsValue = connection_options.into();
        let create = get(&options, "createConnection", cx)?;
        if create.is_callable() {
            crate::call(cx, &create, &JsValue::undefined(), &[connection_options])?
        } else {
            let transport = crate::modules::node_module(cx, if https { "tls" } else { "net" })?;
            invoke(&transport, "connect", &[connection_options], cx)?
        }
    };
    put(&req, "socket", socket.clone(), cx)?;
    put(&socket, "__httpAgent", agent.clone(), cx)?;
    put(&socket, "__httpPoolKey", js_text(&key), cx)?;
    if agent.is_object() {
        let sockets = get(&agent, "sockets", cx)?;
        let list = get(&sockets, &key, cx)?;
        let list = if list.is_object() {
            list
        } else {
            array(cx, Vec::new())
        };
        invoke(&list, "push", std::slice::from_ref(&socket), cx)?;
        put(&sockets, &key, list, cx)?;
    }
    put(&req, "connection", socket.clone(), cx)?;
    put(&socket, "__httpRequest", req.clone(), cx)?;
    attach(&socket, true, JsValue::undefined(), cx)?;
    let native = parser(&socket, cx)?;
    let head_request = crate::print(&get(&req, "method", cx)?) == "HEAD";
    if let Some(p) = native.downcast_ref::<Data>() {
        p.parser.borrow_mut().head_request = head_request;
    }
    let h = get(&options, "headers", cx)?;
    if let Some(h) = h.as_object() {
        for key in h.own_property_keys(cx)? {
            let value = h.get(key.clone(), cx)?;
            set_header(&req, &[js_text(&key.to_string()), value], cx)?;
        }
    }
    if header_value(&req, "host", cx)?.is_undefined() {
        set_header(
            &req,
            &[
                js_text("Host"),
                js_text(&format!(
                    "{}{suffix}",
                    if host.contains(':') {
                        format!("[{host}]")
                    } else {
                        host.clone()
                    },
                    suffix = if port == if https { 443 } else { 80 } {
                        String::new()
                    } else {
                        format!(":{port}")
                    }
                )),
            ],
            cx,
        )?;
    }
    if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
        invoke(&req, "once", &[js_text("response"), cb.clone()], cx)?;
    }
    let fire = closure(
        cx,
        "",
        0,
        |_, _, (req, socket): &(JsValue, JsValue), cx| {
            emit(req, "socket", std::slice::from_ref(socket), cx)?;
            Ok(JsValue::undefined())
        },
        (req.clone(), socket),
    );
    crate::microtask(cx, fire.into(), Vec::new());
    Ok(req)
}

fn init_server(target: &JsObject, args: &[JsValue], cx: &mut Context) -> JsResult<()> {
    let server: JsValue = target.clone().into();
    let secure = get(&server, "__httpSecure", cx)?.to_boolean();
    let transport = crate::modules::node_module(cx, if secure { "tls" } else { "net" })?;
    let ctor = get(&transport, "Server", cx)?;
    let options = if arg(args, 0).is_callable() {
        object(cx).into()
    } else {
        arg(args, 0)
    };
    crate::call(cx, &ctor, &server, &[options])?;
    for (name, value) in [
        ("timeout", 0),
        ("requestTimeout", 300_000),
        ("headersTimeout", 60_000),
        ("keepAliveTimeout", 5_000),
        ("maxHeadersCount", 2_000),
    ] {
        put(&server, name, value, cx)?;
    }
    let accept = closure(
        cx,
        "",
        1,
        |_, args, server: &JsValue, cx| {
            let socket = arg(args, 0);
            attach(&socket, false, server.clone(), cx)?;
            let timeout = get(server, "timeout", cx)?;
            invoke(&socket, "setTimeout", &[timeout], cx)?;
            Ok(JsValue::undefined())
        },
        server.clone(),
    );
    invoke(
        &server,
        "on",
        &[
            js_text(if secure {
                "secureConnection"
            } else {
                "connection"
            }),
            accept.into(),
        ],
        cx,
    )?;
    if let Some(cb) = args.iter().rev().find(|v| v.is_callable()) {
        invoke(&server, "on", &[js_text("request"), cb.clone()], cx)?;
    }
    let close_all = function(cx, "closeAllConnections", 0, |this, _, cx| {
        for socket in list_of(&get(this, "__networkConnections", cx)?, cx)? {
            // Node deliberately leaves upgraded transports to their protocol owner.
            if !get(&socket, "__httpUpgraded", cx)?.to_boolean() {
                invoke(&socket, "destroy", &[], cx)?;
            }
        }
        Ok(JsValue::undefined())
    });
    put(&server, "closeAllConnections", close_all, cx)?;
    let close_idle = function(cx, "closeIdleConnections", 0, |this, _, cx| {
        for socket in list_of(&get(this, "__networkConnections", cx)?, cx)? {
            let response = get(&socket, "__httpResponse", cx)?;
            if !get(&socket, "__httpUpgraded", cx)?.to_boolean()
                && (response.is_undefined() || get(&response, "finished", cx)?.to_boolean())
            {
                invoke(&socket, "destroy", &[], cx)?;
            }
        }
        Ok(JsValue::undefined())
    });
    put(&server, "closeIdleConnections", close_idle, cx)?;
    let original = get(&server, "close", cx)?;
    let close = closure(
        cx,
        "close",
        1,
        |this, args, original: &JsValue, cx| {
            invoke(this, "closeIdleConnections", &[], cx)?;
            crate::call(cx, original, this, args)
        },
        original,
    );
    put(&server, "close", close, cx)?;
    let timeout = function(cx, "setTimeout", 2, |this, args, cx| {
        put(this, "timeout", arg(args, 0), cx)?;
        if arg(args, 1).is_callable() {
            invoke(this, "on", &[js_text("timeout"), arg(args, 1)], cx)?;
        }
        for socket in list_of(&get(this, "__networkConnections", cx)?, cx)? {
            invoke(&socket, "setTimeout", &[arg(args, 0)], cx)?;
        }
        Ok(this.clone())
    });
    put(&server, "setTimeout", timeout, cx)?;
    Ok(())
}

pub(crate) fn install(module: &JsObject, https: bool, cx: &mut Context) -> JsResult<()> {
    let writable = tcp::stream_class("Writable", cx)?;
    let (outgoing, _) = class(
        cx,
        ClassSpec {
            name: "OutgoingMessage",
            parent: Some(&writable),
            init: init_outgoing,
            methods: OUTGOING,
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("OutgoingMessage"), outgoing.clone(), false, cx)?;
    for name in ["ClientRequest", "ServerResponse"] {
        let (ctor, _) = class(
            cx,
            ClassSpec {
                name,
                parent: Some(&outgoing),
                init: init_outgoing,
                ..ClassSpec::default()
            },
        )?;
        module.set(JsString::from(name), ctor, false, cx)?;
    }
    let readable = tcp::stream_class("Readable", cx)?;
    let (incoming, _) = class(
        cx,
        ClassSpec {
            name: "IncomingMessage",
            parent: Some(&readable),
            init: init_readable,
            methods: &[
                ("_read", 1, |this, _, cx| {
                    let socket = get(this, "socket", cx)?;
                    invoke(&socket, "resume", &[], cx)?;
                    on_data(&socket, Vec::new(), cx)?;
                    Ok(JsValue::undefined())
                }),
                ("setTimeout", 2, |this, args, cx| {
                    invoke(&get(this, "socket", cx)?, "setTimeout", args, cx)?;
                    Ok(this.clone())
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("IncomingMessage"), incoming, false, cx)?;
    let parent = if https {
        let http = crate::modules::node_module(cx, "http")?;
        get(&http, "Agent", cx)?
            .as_object()
            .ok_or_else(|| crate::native::type_error("http.Agent"))?
    } else {
        crate::events::emitter_class(cx)?
    };
    let (agent, _) = class(
        cx,
        ClassSpec {
            name: "Agent",
            parent: Some(&parent),
            init: agent_init,
            methods: &[
                ("destroy", 0, |this, _, cx| {
                    for key in ["freeSockets", "sockets"] {
                        if let Some(map) = get(this, key, cx)?.as_object() {
                            for key in map.own_property_keys(cx)? {
                                let list = map.get(key, cx)?;
                                for socket in list_of(&list, cx)? {
                                    invoke(&socket, "destroy", &[], cx)?;
                                }
                            }
                        }
                    }
                    Ok(JsValue::undefined())
                }),
                ("getName", 1, |_, args, cx| {
                    Ok(js_text(&origin(&arg(args, 0), cx)?))
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Agent"), agent.clone(), false, cx)?;
    let global_options = object(cx);
    global_options.set(js_string!("keepAlive"), true, false, cx)?;
    let global_agent = agent.construct(&[global_options.into()], None, cx)?;
    module.set(js_string!("globalAgent"), global_agent, false, cx)?;
    for (name, get_request) in [("request", false), ("get", true)] {
        let f = closure(
            cx,
            name,
            3,
            |_, args, (https, get_request): &(bool, bool), cx| {
                let req = request(args, *https, cx)?;
                if *get_request && super::id(&get(&req, "socket", cx)?, cx)? != 0 {
                    invoke(&req, "end", &[], cx)?;
                }
                Ok(req)
            },
            (https, get_request),
        );
        module.set(JsString::from(name), f, false, cx)?;
    }
    let transport = crate::modules::node_module(cx, if https { "tls" } else { "net" })?;
    let parent = get(&transport, "Server", cx)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("Server"))?;
    let (server, proto) = class(
        cx,
        ClassSpec {
            name: "Server",
            parent: Some(&parent),
            init: init_server,
            ..ClassSpec::default()
        },
    )?;
    hidden(&proto, js_string!("__httpSecure"), https, cx)?;
    module.set(js_string!("Server"), server.clone(), false, cx)?;
    let create = closure(
        cx,
        "createServer",
        2,
        |_, args, server: &JsObject, cx| Ok(server.construct(args, None, cx)?.into()),
        server,
    );
    module.set(js_string!("createServer"), create, false, cx)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_streams_chunked_and_trailers() {
        let mut p = Parser::new(true);
        p.buffer.extend_from_slice(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-A: one\r\nX-A: two\r\n\r\n5\r\nhello\r\n0\r\nX-End: yes\r\n\r\n");
        assert!(matches!(p.step().unwrap(), Some(Action::Head(_))));
        assert!(matches!(p.step().unwrap(),Some(Action::Body(v)) if v==b"hello"));
        assert!(matches!(p.step().unwrap(), Some(Action::Trailers(_))));
        assert!(matches!(p.step().unwrap(), Some(Action::End)));
    }
    #[test]
    fn parser_rejects_ambiguous_framing() {
        let mut p = Parser::new(false);
        p.buffer.extend_from_slice(
            b"POST / HTTP/1.1\r\nContent-Length: 4\r\nTransfer-Encoding: chunked\r\n\r\n",
        );
        assert!(p.step().is_err());
    }
}
