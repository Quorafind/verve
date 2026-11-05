//! Messages within the realm: `MessageChannel` and its two `MessagePort`s, `BroadcastChannel`
//! — and the permissioned native `WebSocket` transport.
//!
//! A message is copied as `structuredClone` copies it, and heard as a task of its own: on the
//! runtime's clock, at the next [`crate::Runtime::run_due`], as a page hears it after the
//! script that sent it has finished. Bundles schedule work this way — React's scheduler posts
//! itself a message to yield — so it is a timer, not a microtask, that delivers it.
//!
//! `WebSocket` uses the realm's HTTP/TLS reactor when outbound access is granted. Without a
//! matching capability it retains the original asynchronous `error`, then `close` (1006),
//! refusal. The host's `requestUrl` remains independent of these capabilities.

mod remote;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
};

use crate::clone::Cloned;
use crate::dom_events::{
    Kind, TargetData, dispatch, dom, dom_exception_error, handler_accessors, new_event,
    new_event_of, set_handler, web_class, with_target,
};
use crate::native::{arg, closure, get, hidden, js_text, object, text_of, type_error};

pub use remote::{PortEnd, PortEvent, remote_port_pair};

/// The classes, and the broadcast channels open in this realm.
pub(crate) struct Messaging {
    pub(crate) message_channel: JsObject,
    pub(crate) message_port: JsObject,
    port_proto: JsObject,
    pub(crate) broadcast_channel: JsObject,
    pub(crate) websocket: JsObject,
    channels: RefCell<Vec<JsObject>>,
    /// The ports driven by an end that lives in another isolate; see [`adopt_port`].
    remotes: RefCell<Vec<(JsObject, PortEnd)>>,
}

pub(crate) fn make_classes(
    context: &mut Context,
    event_target: &JsObject,
    event_target_proto: &JsObject,
) -> JsResult<Messaging> {
    let parent = Some((event_target, event_target_proto));
    let (message_port, port_proto) = web_class(
        context,
        "MessagePort",
        0,
        |_, _, _| Err(type_error("Illegal constructor")),
        parent,
        PORT_METHODS,
        &[],
    )?;
    handler_accessors(context, &port_proto, &["messageerror"])?;
    // Setting `onmessage` starts the port, as in a page.
    let get_handler = crate::native::function(context, "get onmessage", 0, |this, _, _| {
        crate::dom_events::handler_of(this, "message")
    });
    let set = crate::native::function(context, "set onmessage", 1, |this, args, cx| {
        set_handler(this, "message", arg(args, 0))?;
        start_port(this, cx)?;
        Ok(JsValue::undefined())
    });
    port_proto.define_property_or_throw(
        js_string!("onmessage"),
        boa_engine::property::PropertyDescriptor::builder()
            .get(get_handler)
            .set(set)
            .enumerable(true)
            .configurable(true),
        context,
    )?;
    let (message_channel, _) = web_class(
        context,
        "MessageChannel",
        0,
        |prototype, _, cx| {
            let port_proto = dom(cx)?.messaging.port_proto.clone();
            let (one, two) = (new_port(&port_proto), new_port(&port_proto));
            entangle(&one, &two)?;
            entangle(&two, &one)?;
            let channel = object(cx);
            channel.set_prototype(Some(prototype.clone()));
            hidden(&channel, js_string!("__port1"), one, cx)?;
            hidden(&channel, js_string!("__port2"), two, cx)?;
            Ok(channel)
        },
        None,
        &[],
        &[
            ("port1", |this, _, cx| get(this, "__port1", cx), None),
            ("port2", |this, _, cx| get(this, "__port2", cx), None),
        ],
    )?;
    let (broadcast_channel, channel_proto) = web_class(
        context,
        "BroadcastChannel",
        1,
        make_channel,
        parent,
        CHANNEL_METHODS,
        CHANNEL_ACCESSORS,
    )?;
    handler_accessors(context, &channel_proto, &["message", "messageerror"])?;
    let (websocket, socket_proto) = web_class(
        context,
        "WebSocket",
        1,
        make_socket,
        parent,
        SOCKET_METHODS,
        SOCKET_ACCESSORS,
    )?;
    handler_accessors(
        context,
        &socket_proto,
        &["open", "message", "error", "close"],
    )?;
    for target in [&websocket, &socket_proto] {
        for (name, value) in [
            ("CONNECTING", 0),
            ("OPEN", 1),
            ("CLOSING", 2),
            ("CLOSED", 3),
        ] {
            target.define_property_or_throw(
                JsString::from(name),
                boa_engine::property::PropertyDescriptor::builder()
                    .value(value)
                    .writable(false)
                    .enumerable(true)
                    .configurable(false),
                context,
            )?;
        }
    }
    Ok(Messaging {
        message_channel,
        message_port,
        port_proto,
        broadcast_channel,
        websocket,
        channels: RefCell::new(Vec::new()),
        remotes: RefCell::new(Vec::new()),
    })
}

// ─── MessagePort ────────────────────────────────────────────────────────────────────────────

fn new_port(proto: &JsObject) -> JsObject {
    JsObject::from_proto_and_data(
        proto.clone(),
        TargetData::new(Kind::Port {
            other: None,
            started: false,
            closed: false,
            queue: Vec::new(),
        }),
    )
}

fn entangle(port: &JsObject, with: &JsObject) -> JsResult<()> {
    with_target(&port.clone().into(), |data| {
        if let Kind::Port { other, .. } = &mut *data.kind.borrow_mut() {
            *other = Some(with.clone());
        }
    })
}

/// `postMessage`'s second argument: a transfer list, or `{ transfer }`.
fn transfer_of(value: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    if value.is_null_or_undefined() {
        return Ok(JsValue::undefined());
    }
    if value.as_object().is_some_and(|o| o.is_array()) {
        return Ok(value.clone());
    }
    get(value, "transfer", context)
}

/// A copy of `message`, as it crosses to whoever hears it.
fn copy(message: &JsValue, transfer: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    crate::web::structured_clone(message, transfer, context)
}

/// Hears `data` on `port` as a task of its own: dispatched now if the port is started, kept
/// until it is otherwise. `args` is the message, then the ports that came with it, if any.
fn deliver(
    _: &JsValue,
    args: &[JsValue],
    port: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let (data, ports) = (arg(args, 0), arg(args, 1));
    let ready = with_target(&port.clone().into(), |target| {
        match &mut *target.kind.borrow_mut() {
            Kind::Port { closed: true, .. } => false,
            Kind::Port {
                started: false,
                queue,
                ..
            } => {
                queue.push((data.clone(), ports.clone()));
                false
            }
            _ => true,
        }
    })?;
    if ready {
        let class = dom(context)?.message_event.clone();
        let mut fields = vec![("data", data)];
        if !ports.is_undefined() {
            fields.push(("ports", ports));
        }
        let event = new_event_of(context, &class, "message", &fields)?;
        dispatch(port, &event, context)?;
    }
    Ok(JsValue::undefined())
}

fn later(context: &mut Context, port: &JsObject, data: JsValue, ports: JsValue) {
    let fire = closure(context, "", 1, deliver, port.clone());
    crate::set_timeout(context, fire.into(), 0., false, vec![data, ports]);
}

fn start_port(this: &JsValue, context: &mut Context) -> JsResult<()> {
    let queued = with_target(this, |data| match &mut *data.kind.borrow_mut() {
        Kind::Port { started, queue, .. } if !*started => {
            *started = true;
            std::mem::take(queue)
        }
        _ => Vec::new(),
    })?;
    let port = this.as_object().expect("a port");
    for (data, ports) in queued {
        later(context, &port, data, ports);
    }
    Ok(())
}

const PORT_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("postMessage", 2, |this, args, cx| {
        let transfer = transfer_of(&arg(args, 1), cx)?;
        if post_remote(this, &arg(args, 0), &transfer, cx)? {
            return Ok(JsValue::undefined());
        }
        let data = copy(&arg(args, 0), &transfer, cx)?;
        let other = with_target(this, |data| match &*data.kind.borrow() {
            Kind::Port {
                other,
                closed: false,
                ..
            } => other.clone(),
            _ => None,
        })?;
        if let Some(other) = other {
            later(cx, &other, data, JsValue::undefined());
        }
        Ok(JsValue::undefined())
    }),
    ("start", 0, |this, _, cx| {
        start_port(this, cx)?;
        Ok(JsValue::undefined())
    }),
    ("close", 0, |this, _, cx| {
        let other = with_target(this, |data| match &mut *data.kind.borrow_mut() {
            Kind::Port { other, closed, .. } => {
                *closed = true;
                other.take()
            }
            _ => None,
        })?;
        with_end(this, cx, PortEnd::close);
        if let Some(other) = other {
            with_target(&other.into(), |data| {
                if let Kind::Port { other, .. } = &mut *data.kind.borrow_mut() {
                    *other = None;
                }
            })?;
        }
        Ok(JsValue::undefined())
    }),
];

// ─── Ports whose other end is in another isolate ────────────────────────────────────────────

/// A `MessagePort` in this realm driven by `end`: the way a page holds one half of a channel
/// whose other half is in the main process. What the other end sends is heard on the host's
/// beat ([`crate::Runtime::run_workers`]), each message as an event of its own with the
/// microtasks it leaves behind; the realm's waker is what the other end calls when one arrives.
///
/// # Errors
///
/// The realm's classes could not be made.
pub fn adopt_port(end: PortEnd, context: &mut Context) -> JsResult<JsValue> {
    let proto = dom(context)?.messaging.port_proto.clone();
    let port = new_port(&proto);
    attach(&port, end, context)?;
    Ok(port.into())
}

/// Has `port` driven by the remote `end`.
fn attach(port: &JsObject, end: PortEnd, context: &mut Context) -> JsResult<()> {
    let wake = Arc::clone(&crate::shared(context).wake);
    end.set_waker(Arc::new(move || wake.wake()));
    let messaging = &dom(context)?.messaging;
    messaging.remotes.borrow_mut().push((port.clone(), end));
    Ok(())
}

/// Runs `use_end` on the remote end driving `port`, if one does.
fn with_end<T>(
    port: &JsValue,
    context: &mut Context,
    use_end: impl FnOnce(&PortEnd) -> T,
) -> Option<T> {
    let object = port.as_object()?;
    let messaging = &dom(context).ok()?.messaging;
    let remotes = messaging.remotes.borrow();
    let (_, end) = remotes
        .iter()
        .find(|(driven, _)| JsObject::equals(driven, &object))?;
    Some(use_end(end))
}

/// The remote end that drove `port`, if one did, now taken from it.
fn take_end(port: &JsValue, context: &mut Context) -> Option<PortEnd> {
    let object = port.as_object()?;
    let messaging = &dom(context).ok()?.messaging;
    let mut remotes = messaging.remotes.borrow_mut();
    let at = remotes
        .iter()
        .position(|(driven, _)| JsObject::equals(driven, &object))?;
    Some(remotes.remove(at).1)
}

/// Takes the `MessagePort` `port` out of this realm to send it to another isolate, and answers
/// the end that carries it there; the port is closed from now on, as a transferred one is. A
/// port driven by a remote end gives that end up. One that is half of a channel in this realm
/// is cut from it: its partner becomes driven by a new remote pair's other end, and the answer
/// is this one.
///
/// # Errors
///
/// `port` is not a `MessagePort`, or was transferred or closed already (`DataCloneError`).
pub fn release_port(port: &JsValue, context: &mut Context) -> JsResult<PortEnd> {
    if !port
        .as_object()
        .is_some_and(|o| crate::dom_events::is_port(&o))
    {
        return Err(type_error("Value is not a MessagePort"));
    }
    let open = with_target(port, |data| match &mut *data.kind.borrow_mut() {
        Kind::Port { closed, other, .. } if !*closed => {
            *closed = true;
            Some(other.take())
        }
        _ => None,
    })?;
    let Some(peer) = open else {
        return Err(dom_exception_error(
            context,
            "A MessagePort in the transfer list has already been transferred or closed.",
            "DataCloneError",
        ));
    };
    if let Some(held) = take_end(port, context) {
        return Ok(held);
    }
    let (kept, sent) = remote_port_pair();
    if let Some(peer) = peer {
        attach(&peer, kept, context)?;
    }
    Ok(sent)
}

/// The first of `items` that cannot be given away with [`release_port`] — not a port, a port that
/// is closed or already given away, the port `except` is posting on, or one listed twice — and
/// why. A transfer list is checked whole before any port in it is taken, so one that is refused
/// leaves every port as it was.
fn unreleasable(items: &[JsValue], except: Option<&JsValue>) -> Option<(usize, &'static str)> {
    for (at, item) in items.iter().enumerate() {
        let Some(port) = item.as_object().filter(crate::dom_events::is_port) else {
            return Some((at, "does not have a transferable type"));
        };
        let open = with_target(item, |data| {
            matches!(&*data.kind.borrow(), Kind::Port { closed: false, .. })
        });
        if !open.unwrap_or(false) {
            return Some((at, "has already been transferred or closed"));
        }
        if except
            .and_then(JsValue::as_object)
            .is_some_and(|me| JsObject::equals(&me, &port))
        {
            return Some((at, "is the port it is posted on"));
        }
        if items[..at]
            .iter()
            .filter_map(JsValue::as_object)
            .any(|earlier| JsObject::equals(&earlier, &port))
        {
            return Some((at, "is a duplicate of an earlier port"));
        }
    }
    None
}

/// Whether every one of `items` can be given away with [`release_port`] together: all ports, open,
/// and no port twice.
pub fn can_release(items: &[JsValue]) -> bool {
    unreleasable(items, None).is_none()
}

/// `postMessage` on a port driven by a remote end: the message cloned out and sent with the
/// ports of the transfer list. `false` when the port is not one.
fn post_remote(
    this: &JsValue,
    message: &JsValue,
    transfer: &JsValue,
    context: &mut Context,
) -> JsResult<bool> {
    if with_end(this, context, |_| ()).is_none() {
        return Ok(false);
    }
    let data = Cloned::from_value(message, context)?;
    let items = if transfer.is_null_or_undefined() {
        Vec::new()
    } else {
        crate::dom_events::iterate(transfer, context)?
    };
    if let Some((at, why)) = unreleasable(&items, Some(this)) {
        let message = format!("Value at index {at} {why}.");
        return Err(dom_exception_error(context, &message, "DataCloneError"));
    }
    let mut ports = Vec::new();
    for item in &items {
        ports.push(release_port(item, context)?);
    }
    with_end(this, context, |end| end.send(data, ports));
    Ok(true)
}

/// Whether any port of this realm is driven by a remote end.
pub(crate) fn active(context: &Context) -> bool {
    context
        .get_data::<Rc<crate::dom_events::Dom>>()
        .is_some_and(|dom| !dom.messaging.remotes.borrow().is_empty())
}

/// Hears what the remote ends have sent the ports of this realm. Answers whether any message
/// came.
pub(crate) fn poll(context: &mut Context) -> bool {
    let Some(dom) = context.get_data::<Rc<crate::dom_events::Dom>>().cloned() else {
        return false;
    };
    let mut heard = Vec::new();
    dom.messaging.remotes.borrow_mut().retain(|(port, end)| {
        while let Some(event) = end.receive() {
            let closed = matches!(event, PortEvent::Closed);
            heard.push((port.clone(), event));
            if closed {
                return false;
            }
        }
        !end.is_closed()
    });
    let any = !heard.is_empty();
    for (port, event) in heard {
        match event {
            PortEvent::Message(data, ends) => hear(&port, &data, ends, context),
            PortEvent::Closed => close_port(&port),
        }
    }
    any
}

/// The other end closed: nothing comes through this port again.
fn close_port(port: &JsObject) {
    with_target(&port.clone().into(), |data| {
        if let Kind::Port { closed, .. } = &mut *data.kind.borrow_mut() {
            *closed = true;
        }
    })
    .expect("a port is an event target");
}

/// `data` and the ports that came with it, heard on `port`.
fn hear(port: &JsObject, data: &Cloned, ends: Vec<PortEnd>, context: &mut Context) {
    let heard = (|| {
        let data = data.to_value(context)?;
        let mut ports = Vec::new();
        for end in ends {
            ports.push(adopt_port(end, context)?);
        }
        let ports = crate::native::array(context, ports);
        deliver(&JsValue::undefined(), &[data, ports], port, context)
    })();
    if let Err(error) = heard {
        crate::report(context, &error);
    }
    // The microtasks the message left, as `Runtime::run_microtasks` runs them: the job executor
    // reports a job that threw itself, so there is no error left to do anything with.
    let _ = context.run_jobs();
}

// ─── BroadcastChannel ───────────────────────────────────────────────────────────────────────

fn make_channel(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    if args.is_empty() {
        return Err(type_error(
            "Failed to construct 'BroadcastChannel': 1 argument required, but only 0 present.",
        ));
    }
    let name = text_of(&arg(args, 0), context)?;
    let channel = JsObject::from_proto_and_data(
        prototype.clone(),
        TargetData::new(Kind::Channel {
            name,
            closed: false,
        }),
    );
    dom(context)?
        .messaging
        .channels
        .borrow_mut()
        .push(channel.clone());
    Ok(channel)
}

fn channel_state(this: &JsValue) -> JsResult<(String, bool)> {
    with_target(this, |data| match &*data.kind.borrow() {
        Kind::Channel { name, closed } => Ok((name.clone(), *closed)),
        _ => Err(type_error("Illegal invocation")),
    })?
}

fn deliver_broadcast(
    _: &JsValue,
    args: &[JsValue],
    channel: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    if channel_state(&channel.clone().into())?.1 {
        return Ok(JsValue::undefined());
    }
    let class = dom(context)?.message_event.clone();
    let event = new_event_of(context, &class, "message", &[("data", arg(args, 0))])?;
    dispatch(channel, &event, context)?;
    Ok(JsValue::undefined())
}

const CHANNEL_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("postMessage", 1, |this, args, cx| {
        let (name, closed) = channel_state(this)?;
        if closed {
            return Err(dom_exception_error(
                cx,
                "Failed to execute 'postMessage' on 'BroadcastChannel': Channel is closed",
                "InvalidStateError",
            ));
        }
        let me = this.as_object().expect("a channel");
        let listeners: Vec<JsObject> = dom(cx)?
            .messaging
            .channels
            .borrow()
            .iter()
            .filter(|c| !JsObject::equals(c, &me))
            .cloned()
            .collect();
        for channel in listeners {
            if channel_state(&channel.clone().into())?.0 != name {
                continue;
            }
            let data = copy(&arg(args, 0), &JsValue::undefined(), cx)?;
            let fire = closure(cx, "", 1, deliver_broadcast, channel);
            crate::set_timeout(cx, fire.into(), 0., false, vec![data]);
        }
        Ok(JsValue::undefined())
    }),
    ("close", 0, |this, _, cx| {
        with_target(this, |data| {
            if let Kind::Channel { closed, .. } = &mut *data.kind.borrow_mut() {
                *closed = true;
            }
        })?;
        let me = this.as_object().expect("a channel");
        dom(cx)?
            .messaging
            .channels
            .borrow_mut()
            .retain(|c| !JsObject::equals(c, &me));
        Ok(JsValue::undefined())
    }),
];

const CHANNEL_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[(
    "name",
    |this, _, _| Ok(js_text(&channel_state(this)?.0)),
    None,
)];

// ─── WebSocket ──────────────────────────────────────────────────────────────────────────────

fn make_socket(
    prototype: &JsObject,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsObject> {
    if args.is_empty() {
        return Err(type_error(
            "Failed to construct 'WebSocket': 1 argument required, but only 0 present.",
        ));
    }
    let given = text_of(&arg(args, 0), context)?;
    let mut url = url::Url::parse(&given).map_err(|_| {
        dom_exception_error(
            context,
            &format!("Failed to construct 'WebSocket': The URL '{given}' is invalid."),
            "SyntaxError",
        )
    })?;
    let scheme = url.scheme().to_owned();
    match scheme.as_str() {
        "ws" | "wss" => {}
        "http" => {
            let _ = url.set_scheme("ws");
        }
        "https" => {
            let _ = url.set_scheme("wss");
        }
        other => {
            return Err(dom_exception_error(
                context,
                &format!(
                    "Failed to construct 'WebSocket': The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '{other}:' is not allowed."
                ),
                "SyntaxError",
            ));
        }
    }
    if url.fragment().is_some() {
        return Err(dom_exception_error(
            context,
            &format!(
                "Failed to construct 'WebSocket': The URL contains a fragment identifier ('{}'). Fragment identifiers are not allowed in WebSocket URLs.",
                url.fragment().unwrap_or("")
            ),
            "SyntaxError",
        ));
    }
    let href = url.to_string();
    let socket = JsObject::from_proto_and_data(
        prototype.clone(),
        TargetData::new(Kind::Socket {
            url: href.clone(),
            state: 0,
            binary_type: "blob".into(),
            buffered: 0.,
        }),
    );
    if crate::network::websocket::connect(&socket, &url, &arg(args, 1), context)? {
        return Ok(socket);
    }
    let fail = closure(context, "", 0, fail_socket, socket.clone());
    crate::set_timeout(context, fail.into(), 0., false, Vec::new());
    Ok(socket)
}

/// The connection failing: `error`, then `close` with code 1006.
fn fail_socket(
    _: &JsValue,
    _: &[JsValue],
    socket: &JsObject,
    context: &mut Context,
) -> JsResult<JsValue> {
    let url = with_target(&socket.clone().into(), |data| {
        match &mut *data.kind.borrow_mut() {
            Kind::Socket { state, url, .. } if *state != 3 => {
                *state = 3;
                Some(url.clone())
            }
            _ => None,
        }
    })?;
    let Some(url) = url else {
        return Ok(JsValue::undefined());
    };
    crate::console_line(
        context,
        crate::Level::Warn,
        format!(
            "WebSocket connection to '{url}' failed: this runtime does not open WebSocket connections"
        ),
    );
    let error = new_event(context, "error")?;
    dispatch(socket, &error, context)?;
    let class = dom(context)?.close_event.clone();
    let close = new_event_of(
        context,
        &class,
        "close",
        &[
            ("code", 1006.into()),
            ("reason", js_text("")),
            ("wasClean", false.into()),
        ],
    )?;
    dispatch(socket, &close, context)?;
    Ok(JsValue::undefined())
}

pub(crate) fn socket_part<R>(
    this: &JsValue,
    f: impl FnOnce(&mut String, &mut u16, &mut String, &mut f64) -> R,
) -> JsResult<R> {
    with_target(this, |data| match &mut *data.kind.borrow_mut() {
        Kind::Socket {
            url,
            state,
            binary_type,
            buffered,
        } => Ok(f(url, state, binary_type, buffered)),
        _ => Err(type_error("Illegal invocation")),
    })?
}

const SOCKET_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("send", 1, |this, args, cx| {
        let state = socket_part(this, |_, state, _, _| *state)?;
        if state == 0 {
            return Err(dom_exception_error(
                cx,
                "Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.",
                "InvalidStateError",
            ));
        }
        // Sent after closing: counted, as a browser counts it, and never sent.
        let data = arg(args, 0);
        if state == 1 && crate::network::websocket::send(this, &data, cx)? {
            return Ok(JsValue::undefined());
        }
        let size = match crate::encoding::bytes_of(&data, cx)? {
            Some(bytes) => bytes.len(),
            None => text_of(&data, cx)?.len(),
        };
        socket_part(this, |_, _, _, buffered| *buffered += size as f64)?;
        Ok(JsValue::undefined())
    }),
    ("close", 2, |this, args, cx| {
        let code = arg(args, 0);
        if !code.is_undefined() {
            let code = code.to_number(cx)?;
            if code != 1000. && !(3000. ..=4999.).contains(&code) {
                return Err(dom_exception_error(
                    cx,
                    &format!(
                        "Failed to execute 'close' on 'WebSocket': The close code must be either 1000, or between 3000 and 4999. {code} is neither."
                    ),
                    "InvalidAccessError",
                ));
            }
        }
        let reason = arg(args, 1);
        if !reason.is_undefined() && text_of(&reason, cx)?.len() > 123 {
            return Err(dom_exception_error(
                cx,
                "Failed to execute 'close' on 'WebSocket': The close reason must not be greater than 123 UTF-8 bytes.",
                "SyntaxError",
            ));
        }
        let close_code = arg(args, 0).as_number().unwrap_or(1000.) as u16;
        let close_reason = if reason.is_undefined() {
            String::new()
        } else {
            text_of(&reason, cx)?
        };
        if crate::network::websocket::close(this, close_code, close_reason, cx)? {
            return Ok(JsValue::undefined());
        }
        socket_part(this, |_, state, _, _| {
            if *state < 2 {
                *state = 2;
            }
        })?;
        Ok(JsValue::undefined())
    }),
];

const SOCKET_ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = &[
    (
        "url",
        |this, _, _| socket_part(this, |url, _, _, _| js_text(url)),
        None,
    ),
    (
        "readyState",
        |this, _, _| socket_part(this, |_, state, _, _| u32::from(*state).into()),
        None,
    ),
    (
        "protocol",
        |this, _, cx| {
            let value = get(this, "__wsProtocol", cx)?;
            Ok(if value.is_undefined() {
                js_text("")
            } else {
                value
            })
        },
        None,
    ),
    ("extensions", |_, _, _| Ok(js_text("")), None),
    (
        "bufferedAmount",
        |this, _, _| socket_part(this, |_, _, _, buffered| (*buffered).into()),
        None,
    ),
    (
        "binaryType",
        |this, _, _| socket_part(this, |_, _, kind, _| js_text(kind)),
        Some(|this, args, cx| {
            let kind = text_of(&arg(args, 0), cx)?;
            if kind == "blob" || kind == "arraybuffer" {
                socket_part(this, |_, _, own, _| *own = kind)?;
            }
            Ok(JsValue::undefined())
        }),
    ),
];

// ─── Installing them ────────────────────────────────────────────────────────────────────────

pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let globals: &[(&str, crate::native::GlobalMaker)] = &[
        ("MessageChannel", |cx| {
            Ok(dom(cx)?.messaging.message_channel.clone().into())
        }),
        ("MessagePort", |cx| {
            Ok(dom(cx)?.messaging.message_port.clone().into())
        }),
        ("BroadcastChannel", |cx| {
            Ok(dom(cx)?.messaging.broadcast_channel.clone().into())
        }),
        ("WebSocket", |cx| {
            Ok(dom(cx)?.messaging.websocket.clone().into())
        }),
    ];
    for (name, make) in globals {
        crate::native::lazy_global(context, name, *make)?;
    }
    Ok(())
}
