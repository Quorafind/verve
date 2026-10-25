//! System getaddrinfo and DNS records run on the reactor's fixed resolver pool, never on the
//! JavaScript thread. Unsupported record types fail with ENOSYS rather than fabricated data.
use super::{Id, reactor::Command};
use crate::native::{
    arg, array, closure, coded_error_value, get, invoke, js_text, object, text_of,
};
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string, object::builtins::JsPromise};
use std::{
    collections::HashMap,
    net::{SocketAddr, ToSocketAddrs},
};

#[derive(Clone, Debug)]
pub(crate) struct Query {
    pub host: String,
    pub kind: String,
    pub family: u8,
    pub servers: Vec<SocketAddr>,
    pub ttl: bool,
}
#[derive(Debug)]
pub(crate) enum Answer {
    Connect(Vec<SocketAddr>),
    Lookup(Vec<(String, u8)>),
    Records(serde_json::Value),
}
#[derive(Default)]
pub(crate) struct State {
    pub pending: HashMap<Id, Pending>,
    pub servers: Vec<SocketAddr>,
    pub order: String,
}
pub(crate) struct Pending {
    callback: JsValue,
    reject: JsValue,
    all: bool,
    promise: bool,
    host: String,
    kind: String,
    order: String,
}

pub(crate) fn resolve(query: Query) -> Result<Answer, String> {
    if let Some(port) = query.kind.strip_prefix("connect:") {
        let port = port.parse::<u16>().map_err(|_| "EINVAL".to_owned())?;
        let addresses = (query.host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| "ENOTFOUND".to_owned())?
            .filter(|a| query.family == 0 || (query.family == 4) == a.is_ipv4())
            .collect();
        return Ok(Answer::Connect(addresses));
    }
    if query.kind == "lookup" {
        let mut addresses = Vec::new();
        for address in (query.host.as_str(), 0)
            .to_socket_addrs()
            .map_err(|_| "ENOTFOUND".to_owned())?
        {
            let ip = address.ip();
            let family = if ip.is_ipv4() { 4 } else { 6 };
            let entry = (ip.to_string(), family);
            if (query.family == 0 || query.family == family) && !addresses.contains(&entry) {
                addresses.push(entry);
            }
        }
        if addresses.is_empty() {
            return Err("ENOTFOUND".into());
        }
        return Ok(Answer::Lookup(addresses));
    }
    if !["resolve4", "resolve6", "resolveTxt", "resolveSrv"].contains(&query.kind.as_str()) {
        return Err("ENOSYS".into());
    }
    use hickory_resolver::{
        Resolver,
        config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts},
    };
    let resolver = if query.servers.is_empty() {
        Resolver::from_system_conf()
    } else {
        let mut config = ResolverConfig::new();
        for address in query.servers {
            config.add_name_server(NameServerConfig::new(address, Protocol::Udp));
        }
        Resolver::new(config, ResolverOpts::default())
    }
    .map_err(|_| "ESERVFAIL".to_owned())?;
    let failed = |_| "ENOTFOUND".to_owned();
    if query.ttl && ["resolve4", "resolve6"].contains(&query.kind.as_str()) {
        use hickory_resolver::proto::rr::{RData, RecordType};
        let kind = if query.kind == "resolve4" {
            RecordType::A
        } else {
            RecordType::AAAA
        };
        let lookup = retry(|| resolver.lookup(query.host.as_str(), kind)).map_err(failed)?;
        let records = lookup
            .record_iter()
            .filter_map(|record| {
                let address = match record.data()? {
                    RData::A(ip) => ip.to_string(),
                    RData::AAAA(ip) => ip.to_string(),
                    _ => return None,
                };
                Some(serde_json::json!({"address":address,"ttl":record.ttl()}))
            })
            .collect::<Vec<_>>();
        return Ok(Answer::Records(serde_json::json!(records)));
    }
    let value = match query.kind.as_str() {
        "resolve4" => serde_json::json!(retry(|| resolver.ipv4_lookup(query.host.as_str())).map_err(failed)?.iter().map(|ip| ip.to_string()).collect::<Vec<_>>()),
        "resolve6" => serde_json::json!(retry(|| resolver.ipv6_lookup(query.host.as_str())).map_err(failed)?.iter().map(|ip| ip.to_string()).collect::<Vec<_>>()),
        "resolveTxt" => serde_json::json!(retry(|| resolver.txt_lookup(query.host.as_str())).map_err(failed)?.iter().map(|r| r.txt_data().iter().map(|v| String::from_utf8_lossy(v).into_owned()).collect::<Vec<_>>()).collect::<Vec<_>>()),
        "resolveSrv" => serde_json::json!(retry(|| resolver.srv_lookup(query.host.as_str())).map_err(failed)?.iter().map(|r| serde_json::json!({"name":r.target().to_utf8().trim_end_matches('.'),"port":r.port(),"priority":r.priority(),"weight":r.weight()})).collect::<Vec<_>>()),
        _ => return Err("ENOSYS".into()),
    };
    Ok(Answer::Records(value))
}

/// A record query, tried again when binding its UDP source port was refused. Hickory picks a
/// random port for each query, and Windows answers WSAEACCES for ports in its excluded ranges
/// (Hyper-V and WinNAT reserve blocks of them): that is the system's port choice failing, not
/// the name server's answer.
fn retry<T>(
    mut query: impl FnMut() -> Result<T, hickory_resolver::error::ResolveError>,
) -> Result<T, hickory_resolver::error::ResolveError> {
    use hickory_resolver::{error::ResolveErrorKind, proto::error::ProtoErrorKind};
    let mut attempts = 0;
    loop {
        match query() {
            Err(error)
                if attempts < 4
                    && matches!(error.kind(), ResolveErrorKind::Proto(proto)
                        if matches!(proto.kind(), ProtoErrorKind::Io(io)
                            if io.kind() == std::io::ErrorKind::PermissionDenied)) =>
            {
                attempts += 1;
            }
            result => return result,
        }
    }
}

fn start(args: &[JsValue], kind: &str, promise: bool, cx: &mut Context) -> JsResult<JsValue> {
    let host = text_of(&arg(args, 0), cx)?;
    let options = arg(args, 1);
    let granted = crate::shared(cx).network.borrow().permissions.dns;
    if !granted {
        return crate::sockets::denied_dns(args, kind, promise, cx);
    }
    // Only lookup takes a family. In lookupService the numeric argument is a port;
    // unsupported record APIs must reach their asynchronous ENOSYS result instead.
    let family = if kind != "lookup" {
        0.
    } else if options.is_number() {
        options.to_number(cx)?
    } else {
        get(&options, "family", cx)?.as_number().unwrap_or(0.)
    };
    if ![0., 4., 6.].contains(&family) {
        return Err(crate::native::coded_error(
            cx,
            "The property 'options.family' must be one of: 0, 4, 6. Received invalid family",
            "ERR_INVALID_ARG_VALUE",
        ));
    }
    let family = family as u8;
    let all = get(&options, "all", cx)?.to_boolean();
    let ttl = get(&options, "ttl", cx)?.to_boolean();
    let (result, callback, reject) = if promise {
        let (p, r) = JsPromise::new_pending(cx);
        (p.into(), r.resolve.into(), r.reject.into())
    } else {
        (
            JsValue::undefined(),
            args.iter()
                .rev()
                .find(|v| v.is_callable())
                .cloned()
                .unwrap_or_default(),
            JsValue::undefined(),
        )
    };
    // DNS callbacks must stay alive until completion; the small marker is only used to allocate
    // an id from the same monotonically increasing realm namespace.
    let marker = object(cx);
    let id = super::register(&marker, cx)?;
    let servers = crate::shared(cx).network.borrow().dns.servers.clone();
    crate::shared(cx).network.borrow_mut().targets.remove(&id);
    let requested_order = get(&options, "order", cx)?;
    let order = if !requested_order.is_undefined() {
        text_of(&requested_order, cx)?
    } else if get(&options, "verbatim", cx)?.strict_equals(&false.into()) {
        "ipv4first".into()
    } else {
        crate::shared(cx).network.borrow().dns.order.clone()
    };
    if !["", "verbatim", "ipv4first", "ipv6first"].contains(&order.as_str()) {
        return Err(crate::native::coded_error(
            cx,
            "Invalid DNS result order",
            "ERR_INVALID_ARG_VALUE",
        ));
    }
    crate::shared(cx).network.borrow_mut().dns.pending.insert(
        id,
        Pending {
            callback,
            reject,
            all,
            promise,
            host: host.clone(),
            kind: kind.into(),
            order,
        },
    );
    super::send(
        cx,
        Command::Dns(
            id,
            Query {
                host,
                kind: kind.into(),
                family,
                servers,
                ttl,
            },
        ),
    );
    Ok(result)
}

pub(crate) fn install(module: &JsObject, promises: bool, cx: &mut Context) -> JsResult<()> {
    for name in [
        "lookup",
        "resolve4",
        "resolve6",
        "resolveTxt",
        "resolveSrv",
        "resolve",
        "resolveAny",
        "resolveCaa",
        "resolveCname",
        "resolveMx",
        "resolveNaptr",
        "resolveNs",
        "resolvePtr",
        "resolveSoa",
        "reverse",
        "lookupService",
    ] {
        let f = closure(
            cx,
            name,
            2,
            |_, args, (name, promises): &(String, bool), cx| {
                let mut name = name.clone();
                if name == "resolve" {
                    name = match crate::print(&arg(args, 1)).as_str() {
                        "AAAA" => "resolve6",
                        "TXT" => "resolveTxt",
                        "SRV" => "resolveSrv",
                        "A" | "undefined" => "resolve4",
                        _ if arg(args, 1).is_callable() => "resolve4",
                        _ => "unsupported",
                    }
                    .into();
                }
                start(args, &name, *promises, cx)
            },
            (name.to_owned(), promises),
        );
        module.set(boa_engine::JsString::from(name), f, false, cx)?;
    }
    let set = crate::native::function(cx, "setServers", 1, |_, args, cx| {
        let mut servers = Vec::new();
        for item in crate::native::list_of(&arg(args, 0), cx)? {
            let text = text_of(&item, cx)?;
            let address = text
                .parse::<SocketAddr>()
                .or_else(|_| {
                    text.parse::<std::net::IpAddr>()
                        .map(|ip| SocketAddr::new(ip, 53))
                })
                .map_err(|_| crate::native::type_error("Invalid IP address"))?;
            servers.push(address);
        }
        crate::shared(cx).network.borrow_mut().dns.servers = servers;
        Ok(JsValue::undefined())
    });
    module.set(js_string!("setServers"), set, false, cx)?;
    let get_servers = crate::native::function(cx, "getServers", 0, |_, _, cx| {
        let values = crate::shared(cx)
            .network
            .borrow()
            .dns
            .servers
            .iter()
            .map(|a| {
                js_text(&if a.port() == 53 {
                    a.ip().to_string()
                } else {
                    a.to_string()
                })
            })
            .collect();
        Ok(array(cx, values))
    });
    module.set(js_string!("getServers"), get_servers, false, cx)?;
    let set_order = crate::native::function(cx, "setDefaultResultOrder", 1, |_, args, cx| {
        let order = text_of(&arg(args, 0), cx)?;
        if !["verbatim", "ipv4first", "ipv6first"].contains(&order.as_str()) {
            return Err(crate::native::coded_error(
                cx,
                "Invalid DNS result order",
                "ERR_INVALID_ARG_VALUE",
            ));
        }
        crate::shared(cx).network.borrow_mut().dns.order = order;
        Ok(JsValue::undefined())
    });
    module.set(js_string!("setDefaultResultOrder"), set_order, false, cx)?;
    let get_order = crate::native::function(cx, "getDefaultResultOrder", 0, |_, _, cx| {
        let order = crate::shared(cx).network.borrow().dns.order.clone();
        Ok(js_text(if order.is_empty() { "verbatim" } else { &order }))
    });
    module.set(js_string!("getDefaultResultOrder"), get_order, false, cx)?;
    if let Some(resolver) = get(&module.clone().into(), "Resolver", cx)?.as_object()
        && let Some(proto) = resolver.get(js_string!("prototype"), cx)?.as_object()
    {
        for key in module.own_property_keys(cx)? {
            let name = key.to_string();
            if name.starts_with("resolve")
                || ["reverse", "setServers", "getServers"].contains(&name.as_str())
            {
                let value = module.get(key.clone(), cx)?;
                proto.set(key, value, false, cx)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn deliver(id: Id, result: Result<Answer, String>, cx: &mut Context) -> JsResult<()> {
    let pending = crate::shared(cx)
        .network
        .borrow_mut()
        .dns
        .pending
        .remove(&id);
    let Some(p) = pending else {
        return Ok(());
    };
    let mut family = None;
    let outcome = match result {
        Ok(Answer::Connect(_)) => return Ok(()),
        Ok(Answer::Lookup(mut addresses)) => {
            let order = p.order.as_str();
            if order == "ipv4first" {
                addresses.sort_by_key(|a| a.1);
            }
            if order == "ipv6first" {
                addresses.sort_by_key(|a| std::cmp::Reverse(a.1));
            }
            let mut values = Vec::new();
            for (address, kind) in &addresses {
                let entry = object(cx);
                entry.set(js_string!("address"), js_text(address), false, cx)?;
                entry.set(js_string!("family"), *kind, false, cx)?;
                values.push(entry.into());
            }
            if p.all {
                Ok(array(cx, values))
            } else if p.promise {
                Ok(values.into_iter().next().unwrap_or_default())
            } else {
                family = addresses.first().map(|a| a.1);
                Ok(addresses.first().map(|a| js_text(&a.0)).unwrap_or_default())
            }
        }
        Ok(Answer::Records(value)) => {
            let json = cx.global_object().get(js_string!("JSON"), cx)?;
            let value = invoke(&json, "parse", &[js_text(&value.to_string())], cx)?;
            Ok(value)
        }
        Err(code) => {
            let syscall = if p.kind == "lookup" {
                "getaddrinfo"
            } else {
                "query"
            };
            let e = coded_error_value(cx, &format!("{syscall} {code} {}", p.host), &code);
            if let Some(o) = e.as_object() {
                o.set(js_string!("hostname"), js_text(&p.host), false, cx)?;
                o.set(js_string!("syscall"), js_text(syscall), false, cx)?;
            }
            Err(e)
        }
    };
    match outcome {
        Ok(value) if p.promise => {
            crate::call(cx, &p.callback, &JsValue::undefined(), &[value])?;
        }
        Ok(value) => {
            let mut args = vec![JsValue::null(), value];
            if let Some(family) = family {
                args.push(family.into());
            }
            if p.callback.is_callable() {
                crate::call(cx, &p.callback, &JsValue::undefined(), &args)?;
            }
        }
        Err(e) => {
            let callback = if p.promise { p.reject } else { p.callback };
            if callback.is_callable() {
                crate::call(cx, &callback, &JsValue::undefined(), &[e])?;
            }
        }
    }
    Ok(())
}
