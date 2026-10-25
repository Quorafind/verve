//! Node's network and process modules. Networking is default-denied and may be granted by
//! the embedder; the native implementation lives in `network`. Process execution, cluster,
//! IPC sockets and HTTP/2 remain refused. The refusal paths here retain their original errors.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
};

use crate::native::{
    ClassSpec, arg, array, class, closure, coded_error_value, exports, function, get, invoke,
    js_text, object,
};

pub(crate) fn denied_socket(
    this: &JsValue,
    name: &str,
    args: &[JsValue],
    cx: &mut Context,
) -> JsResult<JsValue> {
    match SOCKET_METHODS.iter().find(|(key, _, _)| *key == name) {
        Some((_, _, f)) => f(this, args, cx),
        None => Ok(this.clone()),
    }
}
pub(crate) fn denied_server(
    this: &JsValue,
    name: &str,
    args: &[JsValue],
    cx: &mut Context,
) -> JsResult<JsValue> {
    match SERVER_METHODS.iter().find(|(key, _, _)| *key == name) {
        Some((_, _, f)) => f(this, args, cx),
        None => Ok(this.clone()),
    }
}
pub(crate) fn denied_dns(
    args: &[JsValue],
    kind: &str,
    promise: bool,
    cx: &mut Context,
) -> JsResult<JsValue> {
    match (kind, promise) {
        ("lookup", false) => lookup(&JsValue::undefined(), args, cx),
        ("lookup", true) => lookup_promise(&JsValue::undefined(), args, cx),
        (_, false) => resolve_failing(&JsValue::undefined(), args, cx),
        (_, true) => resolve_failing_promise(&JsValue::undefined(), args, cx),
    }
}

const NO_NETWORK: &str =
    "Verve does not let plugins open network connections with Node's modules: use requestUrl";

fn emitter_class(context: &mut Context) -> JsResult<JsObject> {
    crate::modules::node_module(context, "events")?
        .as_object()
        .ok_or_else(|| crate::native::type_error("EventEmitter is not a class"))
}

/// `EventEmitter.call(this)`: an object of one of these classes set up as an emitter.
fn emitter_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    let emitter = emitter_class(context)?;
    crate::call(context, &emitter.into(), &this.clone().into(), &[])?;
    Ok(())
}

/// A microtask from now: `error` to whoever listens for it, then — if `close` says so — a
/// socket's `close`, with `hadError`.
fn fail_later(context: &mut Context, target: &JsObject, error: JsValue, close: bool) {
    let close = close.then(|| vec![true.into()]);
    fail_later_with(context, target, error, close);
}

/// [`fail_later`], with what `close` is emitted with given: a child process's is
/// `(code, signal)`.
fn fail_later_with(
    context: &mut Context,
    target: &JsObject,
    error: JsValue,
    close: Option<Vec<JsValue>>,
) {
    let close = match close {
        Some(args) => crate::native::array(context, args),
        None => JsValue::undefined(),
    };
    let fire = closure(
        context,
        "",
        0,
        |_, _, (target, error, close): &(JsObject, JsValue, JsValue), cx| {
            let target: JsValue = target.clone().into();
            let listening = invoke(&target, "listenerCount", &[js_text("error")], cx)?;
            if listening.to_number(cx)? > 0. {
                invoke(&target, "emit", &[js_text("error"), error.clone()], cx)?;
            }
            if close.is_object() {
                if let Some(t) = target.as_object() {
                    t.set(js_string!("destroyed"), true, false, cx)?;
                }
                let mut args = vec![js_text("close")];
                args.extend(crate::native::list_of(close, cx)?);
                invoke(&target, "emit", &args, cx)?;
            }
            Ok(JsValue::undefined())
        },
        (target.clone(), error, close),
    );
    crate::microtask(context, fire.into(), Vec::new());
}

/// Calls `callback`, if it is one, a microtask from now with `args`.
fn call_later(context: &mut Context, callback: Option<&JsValue>, args: Vec<JsValue>) {
    if let Some(callback) = callback.filter(|c| c.is_callable()) {
        crate::microtask(context, callback.clone(), args);
    }
}

fn last_callback(args: &[JsValue]) -> Option<&JsValue> {
    args.iter().rev().find(|a| a.is_callable())
}

fn returns_this(this: &JsValue, _: &[JsValue], _: &mut Context) -> JsResult<JsValue> {
    Ok(this.clone())
}

/// Makes `name` from the module `module` with `args`: `new net.Socket()`.
fn make(context: &mut Context, module: &str, name: &str, args: &[JsValue]) -> JsResult<JsObject> {
    let module = crate::modules::node_module(context, module)?;
    let class = get(&module, name, context)?;
    let class = class
        .as_object()
        .ok_or_else(|| crate::native::type_error(format!("{name} is not a class")))?;
    class.construct(args, None, context)
}

fn define(
    context: &mut Context,
    module: &JsObject,
    name: &str,
    parent: &JsObject,
    init: crate::native::Init,
    methods: &[(&str, usize, NativeFunctionPointer)],
) -> JsResult<JsObject> {
    let (class, _) = class(
        context,
        ClassSpec {
            name,
            parent: Some(parent),
            init,
            methods,
            ..ClassSpec::default()
        },
    )?;
    module.set(JsString::from(name), class.clone(), false, context)?;
    Ok(class)
}

// ─── net ────────────────────────────────────────────────────────────────────────────────────

fn socket_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    for (key, value) in [
        ("connecting", false),
        ("destroyed", false),
        ("readable", false),
        ("writable", false),
        ("pending", true),
    ] {
        this.set(JsString::from(key), value, false, context)?;
    }
    this.set(js_string!("bytesRead"), 0, false, context)?;
    this.set(js_string!("bytesWritten"), 0, false, context)?;
    Ok(())
}

const SOCKET_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("connect", 3, |this, args, cx| {
        if let Some(socket) = this.as_object() {
            if let Some(listener) = last_callback(args) {
                invoke(this, "once", &[js_text("connect"), listener.clone()], cx)?;
            }
            socket.set(js_string!("connecting"), true, false, cx)?;
            let error = coded_error_value(
                cx,
                &format!("connect ECONNREFUSED: {NO_NETWORK}"),
                "ECONNREFUSED",
            );
            if let Some(e) = error.as_object() {
                e.set(js_string!("syscall"), js_text("connect"), false, cx)?;
            }
            fail_later(cx, &socket, error, true);
        }
        Ok(this.clone())
    }),
    ("write", 3, |_, args, cx| {
        let error = coded_error_value(
            cx,
            "Cannot call write after a stream was destroyed",
            "ERR_STREAM_DESTROYED",
        );
        call_later(cx, last_callback(args), vec![error]);
        Ok(false.into())
    }),
    ("end", 3, |this, args, cx| {
        call_later(cx, last_callback(args), Vec::new());
        Ok(this.clone())
    }),
    ("destroy", 1, |this, args, cx| {
        if let Some(socket) = this.as_object()
            && !socket.get(js_string!("destroyed"), cx)?.to_boolean()
        {
            socket.set(js_string!("destroyed"), true, false, cx)?;
            let error = arg(args, 0);
            if error.is_object() {
                fail_later(cx, &socket, error, true);
            } else {
                let fire = closure(
                    cx,
                    "",
                    0,
                    |_, _, s: &JsObject, cx| {
                        invoke(
                            &s.clone().into(),
                            "emit",
                            &[js_text("close"), false.into()],
                            cx,
                        )
                    },
                    socket.clone(),
                );
                crate::microtask(cx, fire.into(), Vec::new());
            }
        }
        Ok(this.clone())
    }),
    ("destroySoon", 0, returns_this),
    ("resetAndDestroy", 0, returns_this),
    ("setTimeout", 2, returns_this),
    ("setNoDelay", 1, returns_this),
    ("setKeepAlive", 2, returns_this),
    ("setEncoding", 1, returns_this),
    ("pause", 0, returns_this),
    ("resume", 0, returns_this),
    ("ref", 0, returns_this),
    ("unref", 0, returns_this),
    ("cork", 0, returns_this),
    ("uncork", 0, returns_this),
    ("address", 0, |_, _, cx| Ok(object(cx).into())),
    ("pipe", 1, |_, args, _| Ok(arg(args, 0))),
];

fn server_init(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    this.set(js_string!("listening"), false, false, context)?;
    this.set(
        js_string!("maxConnections"),
        JsValue::undefined(),
        false,
        context,
    )?;
    if let Some(listener) = last_callback(args) {
        invoke(
            &this.clone().into(),
            "on",
            &[js_text("connection"), listener.clone()],
            context,
        )?;
    }
    Ok(())
}

const SERVER_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("listen", 3, |this, _, cx| {
        if let Some(server) = this.as_object() {
            let error = coded_error_value(
                cx,
                "listen EACCES: Verve does not let plugins serve on a port",
                "EACCES",
            );
            if let Some(e) = error.as_object() {
                e.set(js_string!("syscall"), js_text("listen"), false, cx)?;
            }
            fail_later(cx, &server, error, false);
        }
        Ok(this.clone())
    }),
    ("close", 1, |this, args, cx| {
        call_later(cx, last_callback(args), Vec::new());
        Ok(this.clone())
    }),
    ("address", 0, |_, _, _| Ok(JsValue::null())),
    ("getConnections", 1, |this, args, cx| {
        call_later(cx, last_callback(args), vec![JsValue::null(), 0.into()]);
        Ok(this.clone())
    }),
    ("ref", 0, returns_this),
    ("unref", 0, returns_this),
    ("setTimeout", 2, returns_this),
];

/// `net.isIP`: 4, 6, or 0 — an IPv6 address may carry a zone (`fe80::1%eth0`).
fn ip_version(text: &str) -> u8 {
    if text.parse::<std::net::Ipv4Addr>().is_ok() {
        return 4;
    }
    let bare = text.split('%').next().unwrap_or("");
    if bare.parse::<std::net::Ipv6Addr>().is_ok() {
        6
    } else {
        0
    }
}

fn connect(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let socket = make(context, "net", "Socket", &[])?;
    invoke(&socket.clone().into(), "connect", args, context)?;
    Ok(socket.into())
}

pub(crate) fn net_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let emitter = emitter_class(context)?;
    define(
        context,
        &module,
        "Socket",
        &emitter,
        socket_init,
        SOCKET_METHODS,
    )?;
    define(
        context,
        &module,
        "Server",
        &emitter,
        server_init,
        SERVER_METHODS,
    )?;
    let stream = get(&module.clone().into(), "Socket", context)?;
    module.set(js_string!("Stream"), stream, false, context)?;
    exports(
        context,
        &module,
        &[
            ("createServer", 2, |_, args, cx| {
                Ok(make(cx, "net", "Server", args)?.into())
            }),
            ("connect", 3, connect),
            ("createConnection", 3, connect),
            ("isIP", 1, |_, args, cx| {
                Ok(ip_version(&crate::native::text_of(&arg(args, 0), cx)?).into())
            }),
            ("isIPv4", 1, |_, args, cx| {
                Ok((ip_version(&crate::native::text_of(&arg(args, 0), cx)?) == 4).into())
            }),
            ("isIPv6", 1, |_, args, cx| {
                Ok((ip_version(&crate::native::text_of(&arg(args, 0), cx)?) == 6).into())
            }),
            ("getDefaultAutoSelectFamily", 0, |_, _, _| Ok(true.into())),
            ("setDefaultAutoSelectFamily", 1, |_, _, _| {
                Ok(JsValue::undefined())
            }),
            ("getDefaultAutoSelectFamilyAttemptTimeout", 0, |_, _, _| {
                Ok(250.into())
            }),
            ("setDefaultAutoSelectFamilyAttemptTimeout", 1, |_, _, _| {
                Ok(JsValue::undefined())
            }),
        ],
    )?;
    let (block_list, _) = class(
        context,
        ClassSpec {
            name: "BlockList",
            methods: &[
                ("addAddress", 2, |_, _, _| Ok(JsValue::undefined())),
                ("addRange", 3, |_, _, _| Ok(JsValue::undefined())),
                ("addSubnet", 3, |_, _, _| Ok(JsValue::undefined())),
                ("check", 2, |_, _, _| Ok(false.into())),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("BlockList"), block_list, false, context)?;
    let (address, _) = class(
        context,
        ClassSpec {
            name: "SocketAddress",
            init: |this, args, cx| {
                let options = arg(args, 0);
                let family = get(&options, "family", cx)?;
                let family = if family.is_undefined() {
                    js_text("ipv4")
                } else {
                    family
                };
                let address = get(&options, "address", cx)?;
                let v4 = crate::print(&family) == "ipv4";
                let address = if address.is_undefined() {
                    js_text(if v4 { "127.0.0.1" } else { "::" })
                } else {
                    address
                };
                let port = get(&options, "port", cx)?;
                this.set(js_string!("address"), address, false, cx)?;
                this.set(
                    js_string!("port"),
                    if port.is_undefined() { 0.into() } else { port },
                    false,
                    cx,
                )?;
                this.set(js_string!("family"), family, false, cx)?;
                this.set(js_string!("flowlabel"), 0, false, cx)?;
                Ok(())
            },
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("SocketAddress"), address, false, context)?;
    crate::network::tcp::install(&module, context)?;
    Ok(module.into())
}

// ─── tls ────────────────────────────────────────────────────────────────────────────────────

pub(crate) fn tls_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let net = crate::modules::node_module(context, "net")?;
    let socket = get(&net, "Socket", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("net.Socket"))?;
    let server = get(&net, "Server", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("net.Server"))?;
    define(
        context,
        &module,
        "TLSSocket",
        &socket,
        socket_init,
        &[("getPeerCertificate", 1, |_, _, cx| Ok(object(cx).into()))],
    )?;
    define(context, &module, "Server", &server, server_init, &[])?;
    let (secure, _) = class(
        context,
        ClassSpec {
            name: "SecureContext",
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("SecureContext"), secure, false, context)?;
    exports(
        context,
        &module,
        &[
            ("connect", 3, |_, args, cx| {
                let socket = make(cx, "tls", "TLSSocket", &[])?;
                invoke(&socket.clone().into(), "connect", args, cx)?;
                Ok(socket.into())
            }),
            ("createServer", 2, |_, args, cx| {
                Ok(make(cx, "tls", "Server", args)?.into())
            }),
            ("createSecureContext", 1, |_, _, cx| {
                let context = make(cx, "tls", "SecureContext", &[])?;
                let inner = object(cx);
                context.set(js_string!("context"), inner, false, cx)?;
                Ok(context.into())
            }),
            ("checkServerIdentity", 2, |_, _, _| Ok(JsValue::undefined())),
            ("getCiphers", 0, |_, _, cx| {
                let list = [
                    "aes128-gcm-sha256",
                    "aes256-gcm-sha384",
                    "chacha20-poly1305-sha256",
                ]
                .iter()
                .map(|c| js_text(c))
                .collect();
                Ok(array(cx, list))
            }),
            (
                "convertALPNProtocols",
                2,
                |_, _, _| Ok(JsValue::undefined()),
            ),
        ],
    )?;
    let roots = array(context, Vec::new());
    module.set(js_string!("rootCertificates"), roots, false, context)?;
    for (key, value) in [
        ("DEFAULT_ECDH_CURVE", "auto"),
        ("DEFAULT_MIN_VERSION", "TLSv1.2"),
        ("DEFAULT_MAX_VERSION", "TLSv1.3"),
        (
            "DEFAULT_CIPHERS",
            "TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256",
        ),
    ] {
        module.set(JsString::from(key), js_text(value), false, context)?;
    }
    module.set(js_string!("CLIENT_RENEG_LIMIT"), 3, false, context)?;
    module.set(js_string!("CLIENT_RENEG_WINDOW"), 600, false, context)?;
    crate::network::tls::install(&module, context)?;
    Ok(module.into())
}

// ─── dns ────────────────────────────────────────────────────────────────────────────────────

/// What a name resolves to here: `localhost` and literal addresses, and nothing else.
fn local_address(name: &str, family: u8) -> Option<(String, u8)> {
    match ip_version(name) {
        0 if name.eq_ignore_ascii_case("localhost") => Some(if family == 6 {
            ("::1".into(), 6)
        } else {
            ("127.0.0.1".into(), 4)
        }),
        0 => None,
        v => Some((name.to_owned(), v)),
    }
}

fn not_found(context: &mut Context, syscall: &str, hostname: &str) -> JsValue {
    let error = coded_error_value(
        context,
        &format!("{syscall} ENOTFOUND {hostname}"),
        "ENOTFOUND",
    );
    if let Some(e) = error.as_object() {
        let _ = e.set(js_string!("syscall"), js_text(syscall), false, context);
        let _ = e.set(js_string!("hostname"), js_text(hostname), false, context);
        let _ = e.set(js_string!("errno"), -3008, false, context);
    }
    error
}

/// `dns.lookup(hostname, options, callback)`, answered as `(error, address, family)` or, with
/// `{ all: true }`, `(error, [{ address, family }])`.
fn lookup_outcome(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<Result<(String, u8, bool), JsValue>> {
    let name = crate::native::text_of(&arg(args, 0), context)?;
    let options = arg(args, 1);
    let family = if options.is_number() {
        options.to_number(context)? as u8
    } else {
        get(&options, "family", context)?.as_number().unwrap_or(0.) as u8
    };
    let all = get(&options, "all", context)?.to_boolean();
    Ok(match local_address(&name, family) {
        Some((address, family)) => Ok((address, family, all)),
        None => Err(not_found(context, "getaddrinfo", &name)),
    })
}

fn lookup_result(address: &str, family: u8, context: &mut Context) -> JsResult<JsValue> {
    let entry = object(context);
    entry.set(js_string!("address"), js_text(address), false, context)?;
    entry.set(js_string!("family"), family, false, context)?;
    Ok(entry.into())
}

fn lookup(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let callback = last_callback(args).cloned();
    let outcome = lookup_outcome(args, context)?;
    let callback_args = match outcome {
        Ok((address, family, true)) => {
            let entry = lookup_result(&address, family, context)?;
            vec![JsValue::null(), array(context, vec![entry])]
        }
        Ok((address, family, false)) => vec![JsValue::null(), js_text(&address), family.into()],
        Err(error) => vec![error],
    };
    call_later(context, callback.as_ref(), callback_args);
    Ok(JsValue::undefined())
}

fn lookup_promise(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let outcome = match lookup_outcome(args, context)? {
        Ok((address, family, true)) => {
            let entry = lookup_result(&address, family, context)?;
            Ok(array(context, vec![entry]))
        }
        Ok((address, family, false)) => lookup_result(&address, family, context),
        Err(error) => Err(boa_engine::JsError::from_opaque(error)),
    };
    crate::native::settled(outcome, context)
}

const RESOLVERS: &[&str] = &[
    "resolve",
    "resolve4",
    "resolve6",
    "resolveAny",
    "resolveCaa",
    "resolveCname",
    "resolveMx",
    "resolveNaptr",
    "resolveNs",
    "resolvePtr",
    "resolveSoa",
    "resolveSrv",
    "resolveTxt",
    "reverse",
    "lookupService",
];

fn resolve_failing(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let name = crate::native::text_of(&arg(args, 0), context)?;
    let error = not_found(context, "queryA", &name);
    call_later(context, last_callback(args), vec![error]);
    Ok(JsValue::undefined())
}

fn resolve_failing_promise(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = crate::native::text_of(&arg(args, 0), context)?;
    let error = not_found(context, "queryA", &name);
    crate::native::settled(Err(boa_engine::JsError::from_opaque(error)), context)
}

const DNS_COMMON: &[(&str, usize, NativeFunctionPointer)] = &[
    ("getServers", 0, |_, _, cx| Ok(array(cx, Vec::new()))),
    ("setServers", 1, |_, _, _| Ok(JsValue::undefined())),
    ("setDefaultResultOrder", 1, |_, _, _| {
        Ok(JsValue::undefined())
    }),
    ("getDefaultResultOrder", 0, |_, _, _| {
        Ok(js_text("verbatim"))
    }),
];

fn resolver_class(context: &mut Context, promises: bool) -> JsResult<JsObject> {
    let (class, prototype) = class(
        context,
        ClassSpec {
            name: "Resolver",
            methods: DNS_COMMON,
            ..ClassSpec::default()
        },
    )?;
    let cancel = function(context, "cancel", 0, |_, _, _| Ok(JsValue::undefined()));
    prototype.set(js_string!("cancel"), cancel, false, context)?;
    let set_local = function(context, "setLocalAddress", 2, |_, _, _| {
        Ok(JsValue::undefined())
    });
    prototype.set(js_string!("setLocalAddress"), set_local, false, context)?;
    for name in RESOLVERS {
        let f = function(
            context,
            name,
            2,
            if promises {
                resolve_failing_promise
            } else {
                resolve_failing
            },
        );
        prototype.set(JsString::from(*name), f, false, context)?;
    }
    Ok(class)
}

fn dns_constants(context: &mut Context, module: &JsObject) -> JsResult<()> {
    for (key, value) in [("ADDRCONFIG", 1024.), ("V4MAPPED", 2048.), ("ALL", 256.)] {
        module.set(JsString::from(key), value, false, context)?;
    }
    for code in [
        "NODATA",
        "FORMERR",
        "SERVFAIL",
        "NOTFOUND",
        "NOTIMP",
        "REFUSED",
        "BADQUERY",
        "BADNAME",
        "BADFAMILY",
        "BADRESP",
        "CONNREFUSED",
        "TIMEOUT",
        "EOF",
        "FILE",
        "NOMEM",
        "DESTRUCTION",
        "BADSTR",
        "BADFLAGS",
        "NONAME",
        "BADHINTS",
        "NOTINITIALIZED",
        "LOADIPHLPAPI",
        "ADDRGETNETWORKPARAMS",
        "CANCELLED",
    ] {
        module.set(
            JsString::from(code),
            js_text(&format!("E{code}")),
            false,
            context,
        )?;
    }
    Ok(())
}

pub(crate) fn dns_promises_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, DNS_COMMON)?;
    let f = function(context, "lookup", 2, lookup_promise);
    module.set(js_string!("lookup"), f, false, context)?;
    for name in RESOLVERS {
        let f = function(context, name, 2, resolve_failing_promise);
        module.set(JsString::from(*name), f, false, context)?;
    }
    let resolver = resolver_class(context, true)?;
    module.set(js_string!("Resolver"), resolver, false, context)?;
    dns_constants(context, &module)?;
    crate::network::dns::install(&module, true, context)?;
    Ok(module.into())
}

pub(crate) fn dns_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    exports(context, &module, DNS_COMMON)?;
    let f = function(context, "lookup", 3, lookup);
    module.set(js_string!("lookup"), f, false, context)?;
    for name in RESOLVERS {
        let f = function(context, name, 3, resolve_failing);
        module.set(JsString::from(*name), f, false, context)?;
    }
    let resolver = resolver_class(context, false)?;
    module.set(js_string!("Resolver"), resolver, false, context)?;
    dns_constants(context, &module)?;
    let promises = crate::modules::node_module(context, "dns/promises")?;
    module.set(js_string!("promises"), promises, false, context)?;
    crate::network::dns::install(&module, false, context)?;
    Ok(module.into())
}

// ─── http, https ────────────────────────────────────────────────────────────────────────────

pub(crate) fn denied_http(cx: &mut Context) -> JsResult<JsValue> {
    request(&JsValue::undefined(), &[], cx)
}
pub(crate) fn status_text(code: u16) -> &'static str {
    STATUS_CODES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, s)| *s)
        .unwrap_or("unknown")
}

const STATUS_CODES: &[(u16, &str)] = &[
    (100, "Continue"),
    (101, "Switching Protocols"),
    (102, "Processing"),
    (103, "Early Hints"),
    (200, "OK"),
    (201, "Created"),
    (202, "Accepted"),
    (203, "Non-Authoritative Information"),
    (204, "No Content"),
    (205, "Reset Content"),
    (206, "Partial Content"),
    (207, "Multi-Status"),
    (208, "Already Reported"),
    (226, "IM Used"),
    (300, "Multiple Choices"),
    (301, "Moved Permanently"),
    (302, "Found"),
    (303, "See Other"),
    (304, "Not Modified"),
    (305, "Use Proxy"),
    (307, "Temporary Redirect"),
    (308, "Permanent Redirect"),
    (400, "Bad Request"),
    (401, "Unauthorized"),
    (402, "Payment Required"),
    (403, "Forbidden"),
    (404, "Not Found"),
    (405, "Method Not Allowed"),
    (406, "Not Acceptable"),
    (407, "Proxy Authentication Required"),
    (408, "Request Timeout"),
    (409, "Conflict"),
    (410, "Gone"),
    (411, "Length Required"),
    (412, "Precondition Failed"),
    (413, "Payload Too Large"),
    (414, "URI Too Long"),
    (415, "Unsupported Media Type"),
    (416, "Range Not Satisfiable"),
    (417, "Expectation Failed"),
    (418, "I'm a Teapot"),
    (421, "Misdirected Request"),
    (422, "Unprocessable Entity"),
    (423, "Locked"),
    (424, "Failed Dependency"),
    (425, "Too Early"),
    (426, "Upgrade Required"),
    (428, "Precondition Required"),
    (429, "Too Many Requests"),
    (431, "Request Header Fields Too Large"),
    (451, "Unavailable For Legal Reasons"),
    (500, "Internal Server Error"),
    (501, "Not Implemented"),
    (502, "Bad Gateway"),
    (503, "Service Unavailable"),
    (504, "Gateway Timeout"),
    (505, "HTTP Version Not Supported"),
    (506, "Variant Also Negotiates"),
    (507, "Insufficient Storage"),
    (508, "Loop Detected"),
    (509, "Bandwidth Limit Exceeded"),
    (510, "Not Extended"),
    (511, "Network Authentication Required"),
];

const METHODS: &[&str] = &[
    "ACL",
    "BIND",
    "CHECKOUT",
    "CONNECT",
    "COPY",
    "DELETE",
    "GET",
    "HEAD",
    "LINK",
    "LOCK",
    "M-SEARCH",
    "MERGE",
    "MKACTIVITY",
    "MKCALENDAR",
    "MKCOL",
    "MOVE",
    "NOTIFY",
    "OPTIONS",
    "PATCH",
    "POST",
    "PROPFIND",
    "PROPPATCH",
    "PURGE",
    "PUT",
    "QUERY",
    "REBIND",
    "REPORT",
    "SEARCH",
    "SOURCE",
    "SUBSCRIBE",
    "TRACE",
    "UNBIND",
    "UNLINK",
    "UNLOCK",
    "UNSUBSCRIBE",
];

/// The headers of an outgoing message, kept by lower-case name.
fn headers(this: &JsValue, context: &mut Context) -> JsResult<JsObject> {
    let existing = get(this, "__headers", context)?;
    if let Some(headers) = existing.as_object() {
        return Ok(headers);
    }
    let headers = JsObject::with_null_proto();
    if let Some(target) = this.as_object() {
        crate::native::hidden(&target, js_string!("__headers"), headers.clone(), context)?;
    }
    Ok(headers)
}

fn header_key(args: &[JsValue], context: &mut Context) -> JsResult<JsString> {
    Ok(JsString::from(
        crate::native::text_of(&arg(args, 0), context)?.to_ascii_lowercase(),
    ))
}

const OUTGOING_METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("setHeader", 2, |this, args, cx| {
        let key = header_key(args, cx)?;
        headers(this, cx)?.set(key, arg(args, 1), false, cx)?;
        Ok(this.clone())
    }),
    ("getHeader", 1, |this, args, cx| {
        let key = header_key(args, cx)?;
        headers(this, cx)?.get(key, cx)
    }),
    ("hasHeader", 1, |this, args, cx| {
        let key = header_key(args, cx)?;
        Ok(headers(this, cx)?.has_own_property(key, cx)?.into())
    }),
    ("removeHeader", 1, |this, args, cx| {
        let key = header_key(args, cx)?;
        headers(this, cx)?.delete_property_or_throw(key, cx)?;
        Ok(JsValue::undefined())
    }),
    ("getHeaders", 0, |this, _, cx| {
        let own = headers(this, cx)?;
        let copy = JsObject::with_null_proto();
        for key in own.own_property_keys(cx)? {
            let value = own.get(key.clone(), cx)?;
            copy.set(key, value, false, cx)?;
        }
        Ok(copy.into())
    }),
    ("getHeaderNames", 0, |this, _, cx| {
        let names = headers(this, cx)?
            .own_property_keys(cx)?
            .into_iter()
            .map(|k| js_text(&k.to_string()))
            .collect();
        Ok(array(cx, names))
    }),
    ("flushHeaders", 0, |_, _, _| Ok(JsValue::undefined())),
    ("writeHead", 3, returns_this),
    ("write", 3, |_, args, cx| {
        let error = coded_error_value(cx, NO_NETWORK, "ENOSYS");
        call_later(cx, last_callback(args), vec![error]);
        Ok(false.into())
    }),
    ("end", 3, |this, args, cx| {
        call_later(cx, last_callback(args), Vec::new());
        Ok(this.clone())
    }),
    ("setTimeout", 2, returns_this),
    ("abort", 0, returns_this),
    ("destroy", 1, |this, _, cx| {
        if let Some(t) = this.as_object() {
            t.set(js_string!("destroyed"), true, false, cx)?;
        }
        Ok(this.clone())
    }),
    ("setNoDelay", 1, returns_this),
    ("setSocketKeepAlive", 2, returns_this),
    ("cork", 0, returns_this),
    ("uncork", 0, returns_this),
];

fn message_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    let headers = object(context);
    this.set(js_string!("headers"), headers, false, context)?;
    let raw = array(context, Vec::new());
    this.set(js_string!("rawHeaders"), raw, false, context)?;
    this.set(js_string!("httpVersion"), js_text("1.1"), false, context)?;
    this.set(js_string!("complete"), false, false, context)?;
    Ok(())
}

fn outgoing_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    this.set(js_string!("headersSent"), false, false, context)?;
    this.set(js_string!("finished"), false, false, context)?;
    this.set(js_string!("destroyed"), false, false, context)?;
    this.set(js_string!("statusCode"), 200, false, context)?;
    Ok(())
}

fn agent_init(this: &JsObject, args: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    let options = arg(args, 0);
    this.set(
        js_string!("options"),
        if options.is_object() {
            options.clone()
        } else {
            object(context).into()
        },
        false,
        context,
    )?;
    let max = get(&options, "maxSockets", context)?;
    this.set(
        js_string!("maxSockets"),
        if max.is_undefined() {
            f64::INFINITY.into()
        } else {
            max
        },
        false,
        context,
    )?;
    this.set(js_string!("maxFreeSockets"), 256, false, context)?;
    this.set(
        js_string!("keepAlive"),
        get(&options, "keepAlive", context)?.to_boolean(),
        false,
        context,
    )?;
    for key in ["sockets", "freeSockets", "requests"] {
        let empty = object(context);
        this.set(JsString::from(key), empty, false, context)?;
    }
    Ok(())
}

/// `http.request` and `http.get`: a `ClientRequest` that fails a microtask from now; its
/// callback, if any, is never called with a response.
fn request(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let request = make(context, "http", "ClientRequest", &[])?;
    let error = coded_error_value(
        context,
        &format!("Node's http client is not available: {NO_NETWORK}"),
        "ENOSYS",
    );
    fail_later(context, &request, error, true);
    Ok(request.into())
}

pub(crate) fn validate_header_name(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = crate::native::text_of(&arg(args, 0), context)?;
    let token = |c: char| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c);
    if name.is_empty() || !name.chars().all(token) {
        return Err(crate::native::coded_error(
            context,
            &format!("Header name must be a valid HTTP token [\"{name}\"]"),
            "ERR_INVALID_HTTP_TOKEN",
        ));
    }
    Ok(JsValue::undefined())
}

pub(crate) fn validate_header_value(
    _: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = crate::native::text_of(&arg(args, 0), context)?;
    let value = arg(args, 1);
    if value.is_undefined() {
        return Err(crate::native::coded_error(
            context,
            &format!("Invalid value \"undefined\" for header \"{name}\""),
            "ERR_HTTP_INVALID_HEADER_VALUE",
        ));
    }
    let text = crate::native::text_of(&value, context)?;
    if text
        .chars()
        .any(|c| (c < ' ' && c != '\t') || c == '\u{7f}' || c as u32 > 0xff)
    {
        return Err(crate::native::coded_error(
            context,
            &format!("Invalid character in header content [\"{name}\"]"),
            "ERR_INVALID_CHAR",
        ));
    }
    Ok(JsValue::undefined())
}

fn http_like(context: &mut Context, https: bool) -> JsResult<JsValue> {
    let module = object(context);
    let emitter = emitter_class(context)?;
    let net = crate::modules::node_module(context, "net")?;
    let net_server = get(&net, "Server", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("net.Server"))?;
    define(context, &module, "Server", &net_server, server_init, &[])?;
    let outgoing = define(
        context,
        &module,
        "OutgoingMessage",
        &emitter,
        outgoing_init,
        OUTGOING_METHODS,
    )?;
    define(
        context,
        &module,
        "ServerResponse",
        &outgoing,
        outgoing_init,
        &[],
    )?;
    define(
        context,
        &module,
        "ClientRequest",
        &outgoing,
        outgoing_init,
        &[],
    )?;
    define(
        context,
        &module,
        "IncomingMessage",
        &emitter,
        message_init,
        &[
            ("setTimeout", 2, returns_this),
            ("destroy", 1, returns_this),
        ],
    )?;
    let agent_parent = if https {
        let http = crate::modules::node_module(context, "http")?;
        get(&http, "Agent", context)?
            .as_object()
            .unwrap_or(emitter.clone())
    } else {
        emitter.clone()
    };
    let agent = define(
        context,
        &module,
        "Agent",
        &agent_parent,
        agent_init,
        &[("destroy", 0, |_, _, _| Ok(JsValue::undefined()))],
    )?;
    let global_agent = agent.construct(&[], None, context)?;
    module.set(js_string!("globalAgent"), global_agent, false, context)?;
    exports(
        context,
        &module,
        &[
            ("createServer", 2, |_, args, cx| {
                // `createServer(requestListener)`: the listener hears `request`, which never comes.
                let server = make(cx, "http", "Server", &[])?;
                if let Some(listener) = last_callback(args) {
                    invoke(
                        &server.clone().into(),
                        "on",
                        &[js_text("request"), listener.clone()],
                        cx,
                    )?;
                }
                Ok(server.into())
            }),
            ("request", 3, request),
            ("get", 3, request),
            ("validateHeaderName", 1, validate_header_name),
            ("validateHeaderValue", 2, validate_header_value),
            ("setMaxIdleHTTPParsers", 1, |_, _, _| {
                Ok(JsValue::undefined())
            }),
        ],
    )?;
    let methods = METHODS.iter().map(|m| js_text(m)).collect();
    let methods = array(context, methods);
    module.set(js_string!("METHODS"), methods, false, context)?;
    let codes = object(context);
    for (code, text) in STATUS_CODES {
        codes.set(
            JsString::from(code.to_string()),
            js_text(text),
            false,
            context,
        )?;
    }
    module.set(js_string!("STATUS_CODES"), codes, false, context)?;
    module.set(js_string!("maxHeaderSize"), 16384, false, context)?;
    crate::network::http::install(&module, https, context)?;
    Ok(module.into())
}

pub(crate) fn http_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    http_like(context, false)
}

pub(crate) fn https_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    http_like(context, true)
}

// ─── http2 ──────────────────────────────────────────────────────────────────────────────────

pub(crate) fn http2_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let emitter = emitter_class(context)?;
    define(
        context,
        &module,
        "Http2Session",
        &emitter,
        emitter_init,
        &[
            ("close", 1, |this, args, cx| {
                call_later(cx, last_callback(args), Vec::new());
                Ok(this.clone())
            }),
            ("destroy", 2, returns_this),
            ("request", 2, |_, _, cx| {
                let stream = make(cx, "http2", "Http2Stream", &[])?;
                let error = coded_error_value(cx, NO_NETWORK, "ERR_HTTP2_STREAM_CANCEL");
                fail_later(cx, &stream, error, true);
                Ok(stream.into())
            }),
            ("ping", 2, |_, args, cx| {
                let error = coded_error_value(cx, NO_NETWORK, "ERR_HTTP2_PING_CANCEL");
                call_later(cx, last_callback(args), vec![error]);
                Ok(false.into())
            }),
            ("setTimeout", 2, returns_this),
            ("ref", 0, returns_this),
            ("unref", 0, returns_this),
        ],
    )?;
    define(
        context,
        &module,
        "Http2Stream",
        &emitter,
        emitter_init,
        &[
            ("close", 2, returns_this),
            ("end", 3, returns_this),
            ("setTimeout", 2, returns_this),
        ],
    )?;
    let net = crate::modules::node_module(context, "net")?;
    let net_server = get(&net, "Server", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("net.Server"))?;
    define(
        context,
        &module,
        "Http2Server",
        &net_server,
        server_init,
        &[],
    )?;
    define(
        context,
        &module,
        "Http2SecureServer",
        &net_server,
        server_init,
        &[],
    )?;
    exports(
        context,
        &module,
        &[
            ("connect", 3, |_, _, cx| {
                let session = make(cx, "http2", "Http2Session", &[])?;
                let error = coded_error_value(
                    cx,
                    &format!("connect ECONNREFUSED: {NO_NETWORK}"),
                    "ECONNREFUSED",
                );
                fail_later(cx, &session, error, true);
                Ok(session.into())
            }),
            ("createServer", 2, |_, args, cx| {
                Ok(make(cx, "http2", "Http2Server", args)?.into())
            }),
            ("createSecureServer", 2, |_, args, cx| {
                Ok(make(cx, "http2", "Http2SecureServer", args)?.into())
            }),
            ("getDefaultSettings", 0, |_, _, cx| Ok(object(cx).into())),
            ("getPackedSettings", 1, |_, _, cx| {
                crate::encoding::make_buffer(cx, Vec::new())
            }),
            ("getUnpackedSettings", 1, |_, _, cx| Ok(object(cx).into())),
        ],
    )?;
    let constants = object(context);
    for (key, value) in [
        ("HTTP2_HEADER_STATUS", ":status"),
        ("HTTP2_HEADER_METHOD", ":method"),
        ("HTTP2_HEADER_AUTHORITY", ":authority"),
        ("HTTP2_HEADER_SCHEME", ":scheme"),
        ("HTTP2_HEADER_PATH", ":path"),
        ("HTTP2_HEADER_PROTOCOL", ":protocol"),
        ("HTTP2_HEADER_CONTENT_TYPE", "content-type"),
        ("HTTP2_HEADER_CONTENT_LENGTH", "content-length"),
        ("HTTP2_HEADER_AUTHORIZATION", "authorization"),
        ("HTTP2_HEADER_ACCEPT", "accept"),
        ("HTTP2_HEADER_USER_AGENT", "user-agent"),
        ("HTTP2_METHOD_GET", "GET"),
        ("HTTP2_METHOD_POST", "POST"),
        ("HTTP2_METHOD_PUT", "PUT"),
        ("HTTP2_METHOD_DELETE", "DELETE"),
        ("HTTP2_METHOD_HEAD", "HEAD"),
        ("HTTP2_METHOD_OPTIONS", "OPTIONS"),
        ("HTTP2_METHOD_PATCH", "PATCH"),
    ] {
        constants.set(JsString::from(key), js_text(value), false, context)?;
    }
    for (key, value) in [
        ("NGHTTP2_NO_ERROR", 0),
        ("NGHTTP2_PROTOCOL_ERROR", 1),
        ("NGHTTP2_INTERNAL_ERROR", 2),
        ("NGHTTP2_CANCEL", 8),
        ("NGHTTP2_REFUSED_STREAM", 7),
        ("HTTP_STATUS_OK", 200),
        ("HTTP_STATUS_NO_CONTENT", 204),
        ("HTTP_STATUS_NOT_FOUND", 404),
        ("HTTP_STATUS_INTERNAL_SERVER_ERROR", 500),
    ] {
        constants.set(JsString::from(key), value, false, context)?;
    }
    module.set(js_string!("constants"), constants, false, context)?;
    let sensitive = boa_engine::JsSymbol::new(Some(js_string!("nodejs.http2.sensitiveHeaders")))
        .map_or(JsValue::undefined(), JsValue::from);
    module.set(js_string!("sensitiveHeaders"), sensitive, false, context)?;
    Ok(module.into())
}

// ─── dgram ──────────────────────────────────────────────────────────────────────────────────

pub(crate) fn dgram_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let emitter = emitter_class(context)?;
    define(
        context,
        &module,
        "Socket",
        &emitter,
        |this, args, cx| {
            emitter_init(this, &[], cx)?;
            if let Some(listener) = last_callback(args) {
                invoke(
                    &this.clone().into(),
                    "on",
                    &[js_text("message"), listener.clone()],
                    cx,
                )?;
            }
            Ok(())
        },
        &[
            ("bind", 3, |this, _, cx| {
                if let Some(socket) = this.as_object() {
                    let error = coded_error_value(
                        cx,
                        "bind EACCES: Verve does not let plugins open sockets",
                        "EACCES",
                    );
                    fail_later(cx, &socket, error, false);
                }
                Ok(this.clone())
            }),
            ("send", 6, |_, args, cx| {
                let error = coded_error_value(cx, &format!("send ENOSYS: {NO_NETWORK}"), "ENOSYS");
                call_later(cx, last_callback(args), vec![error]);
                Ok(JsValue::undefined())
            }),
            ("close", 1, |this, args, cx| {
                call_later(cx, last_callback(args), Vec::new());
                invoke(this, "emit", &[js_text("close")], cx)?;
                Ok(this.clone())
            }),
            ("address", 0, |_, _, cx| {
                Err(crate::native::coded_error(
                    cx,
                    "Not running",
                    "ERR_SOCKET_DGRAM_NOT_RUNNING",
                ))
            }),
            ("connect", 3, |this, _, cx| {
                if let Some(socket) = this.as_object() {
                    let error = coded_error_value(
                        cx,
                        &format!("connect ECONNREFUSED: {NO_NETWORK}"),
                        "ECONNREFUSED",
                    );
                    fail_later(cx, &socket, error, false);
                }
                Ok(JsValue::undefined())
            }),
            ("disconnect", 0, |_, _, _| Ok(JsValue::undefined())),
            ("ref", 0, returns_this),
            ("unref", 0, returns_this),
            ("setBroadcast", 1, |_, _, _| Ok(JsValue::undefined())),
            ("setTTL", 1, |_, args, _| Ok(arg(args, 0))),
            ("setMulticastTTL", 1, |_, args, _| Ok(arg(args, 0))),
            ("setMulticastLoopback", 1, |_, args, _| Ok(arg(args, 0))),
            ("setMulticastInterface", 1, |_, _, _| {
                Ok(JsValue::undefined())
            }),
            ("addMembership", 2, |_, _, _| Ok(JsValue::undefined())),
            ("dropMembership", 2, |_, _, _| Ok(JsValue::undefined())),
            ("setRecvBufferSize", 1, |_, _, _| Ok(JsValue::undefined())),
            ("setSendBufferSize", 1, |_, _, _| Ok(JsValue::undefined())),
            ("getRecvBufferSize", 0, |_, _, _| Ok(65536.into())),
            ("getSendBufferSize", 0, |_, _, _| Ok(65536.into())),
        ],
    )?;
    let create = function(context, "createSocket", 2, |_, args, cx| {
        let socket = make(cx, "dgram", "Socket", &[])?;
        if let Some(listener) = last_callback(args) {
            invoke(
                &socket.clone().into(),
                "on",
                &[js_text("message"), listener.clone()],
                cx,
            )?;
        }
        let kind = arg(args, 0);
        let kind = if kind.is_object() {
            get(&kind, "type", cx)?
        } else {
            kind
        };
        socket.set(js_string!("type"), kind, false, cx)?;
        Ok(socket.into())
    });
    module.set(js_string!("createSocket"), create, false, context)?;
    crate::network::udp::install(&module, context)?;
    Ok(module.into())
}

// ─── cluster ────────────────────────────────────────────────────────────────────────────────

pub(crate) fn cluster_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let cluster = emitter_class(context)?.construct(&[], None, context)?;
    for (key, value) in [("isPrimary", true), ("isMaster", true), ("isWorker", false)] {
        cluster.set(JsString::from(key), value, false, context)?;
    }
    let workers = object(context);
    cluster.set(js_string!("workers"), workers, false, context)?;
    let settings = object(context);
    cluster.set(js_string!("settings"), settings, false, context)?;
    cluster.set(js_string!("SCHED_NONE"), 1, false, context)?;
    cluster.set(js_string!("SCHED_RR"), 2, false, context)?;
    cluster.set(js_string!("schedulingPolicy"), 2, false, context)?;
    exports(
        context,
        &cluster,
        &[
            ("fork", 1, |_, _, cx| {
                Err(crate::native::coded_error(
                    cx,
                    "cluster.fork: a plugin cannot start processes",
                    "ENOSYS",
                ))
            }),
            ("setupPrimary", 1, |_, _, _| Ok(JsValue::undefined())),
            ("setupMaster", 1, |_, _, _| Ok(JsValue::undefined())),
            ("disconnect", 1, |_, args, cx| {
                call_later(cx, last_callback(args), Vec::new());
                Ok(JsValue::undefined())
            }),
        ],
    )?;
    let emitter = emitter_class(context)?;
    let (worker, _) = class(
        context,
        ClassSpec {
            name: "Worker",
            parent: Some(&emitter),
            init: emitter_init,
            ..ClassSpec::default()
        },
    )?;
    cluster.set(js_string!("Worker"), worker, false, context)?;
    Ok(cluster.into())
}

// ─── child_process ──────────────────────────────────────────────────────────────────────────

const NO_PROGRAMS: &str = "child_process is not available in Verve: plugins cannot start programs";

fn spawn_error(context: &mut Context, syscall: &str, file: &JsValue) -> JsValue {
    let file = if file.is_string() {
        crate::print(file)
    } else {
        String::new()
    };
    let error = coded_error_value(
        context,
        &format!("{syscall} {file} ENOSYS: {NO_PROGRAMS}"),
        "ENOSYS",
    );
    if let Some(e) = error.as_object() {
        let _ = e.set(
            js_string!("syscall"),
            js_text(format!("{syscall} {file}").trim_end()),
            false,
            context,
        );
        let _ = e.set(js_string!("path"), js_text(&file), false, context);
        let _ = e.set(js_string!("errno"), -38, false, context);
    }
    error
}

fn child_init(this: &JsObject, _: &[JsValue], context: &mut Context) -> JsResult<()> {
    emitter_init(this, &[], context)?;
    let emitter = emitter_class(context)?;
    for stream in ["stdin", "stdout", "stderr"] {
        let made = emitter.construct(&[], None, context)?;
        exports(
            context,
            &made,
            &[
                ("setEncoding", 1, returns_this),
                ("write", 3, |_, _, _| Ok(false.into())),
                ("end", 3, returns_this),
                ("pipe", 1, |_, args, _| Ok(arg(args, 0))),
                ("destroy", 1, returns_this),
                ("resume", 0, returns_this),
                ("pause", 0, returns_this),
            ],
        )?;
        this.set(JsString::from(stream), made, false, context)?;
    }
    let mut streams = Vec::new();
    for stream in ["stdin", "stdout", "stderr"] {
        streams.push(this.get(JsString::from(stream), context)?);
    }
    let stdio = array(context, streams);
    this.set(js_string!("stdio"), stdio, false, context)?;
    this.set(js_string!("pid"), JsValue::undefined(), false, context)?;
    this.set(js_string!("exitCode"), JsValue::null(), false, context)?;
    this.set(js_string!("signalCode"), JsValue::null(), false, context)?;
    this.set(js_string!("killed"), false, false, context)?;
    this.set(js_string!("connected"), false, false, context)?;
    Ok(())
}

/// `spawn`, `exec`, `execFile`, `fork`: a `ChildProcess` that never started — its callback hears
/// why, and it emits `error` and `close`.
fn refused(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let error = spawn_error(context, "spawn", &arg(args, 0));
    // `exec`'s callback is `(error, stdout, stderr)`.
    call_later(
        context,
        last_callback(args),
        vec![error.clone(), js_text(""), js_text("")],
    );
    let child = make(context, "child_process", "ChildProcess", &[])?;
    // As a program that could not be found closes: Node's negative errno as the code, no signal.
    fail_later_with(
        context,
        &child,
        error,
        Some(vec![JsValue::from(-38), JsValue::null()]),
    );
    Ok(child.into())
}

pub(crate) fn child_process_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    let emitter = emitter_class(context)?;
    define(
        context,
        &module,
        "ChildProcess",
        &emitter,
        child_init,
        &[
            ("kill", 1, |_, _, _| Ok(false.into())),
            ("send", 4, |_, args, cx| {
                let error = coded_error_value(cx, "Channel closed", "ERR_IPC_CHANNEL_CLOSED");
                call_later(cx, last_callback(args), vec![error]);
                Ok(false.into())
            }),
            ("disconnect", 0, |_, _, _| Ok(JsValue::undefined())),
            ("ref", 0, returns_this),
            ("unref", 0, returns_this),
        ],
    )?;
    for name in ["exec", "execFile", "spawn", "fork"] {
        let f = function(context, name, 3, refused);
        module.set(JsString::from(name), f, false, context)?;
    }
    exports(
        context,
        &module,
        &[
            ("execSync", 2, |_, args, cx| {
                Err(boa_engine::JsError::from_opaque(spawn_error(
                    cx,
                    "spawnSync",
                    &arg(args, 0),
                )))
            }),
            ("execFileSync", 3, |_, args, cx| {
                Err(boa_engine::JsError::from_opaque(spawn_error(
                    cx,
                    "spawnSync",
                    &arg(args, 0),
                )))
            }),
            ("spawnSync", 3, |_, args, cx| {
                // As Node answers a program that could not start: no output, no status, the error.
                let result = object(cx);
                let error = spawn_error(cx, "spawnSync", &arg(args, 0));
                for key in ["output", "stdout", "stderr", "status", "signal"] {
                    result.set(JsString::from(key), JsValue::null(), false, cx)?;
                }
                result.set(js_string!("pid"), 0, false, cx)?;
                result.set(js_string!("error"), error, false, cx)?;
                Ok(result.into())
            }),
        ],
    )?;
    Ok(module.into())
}
