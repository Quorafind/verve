//! System getaddrinfo and DNS records run on the reactor's fixed resolver pool, never on the
//! JavaScript thread. Unsupported record types fail with ENOSYS rather than fabricated data.
use super::{
    Id,
    reactor::{Command, Event},
};
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
    ttl: bool,
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
    let value = match query.kind.as_str() {
        "resolve4" => serde_json::json!(resolver.ipv4_lookup(query.host).map_err(failed)?.iter().map(|ip| ip.to_string()).collect::<Vec<_>>()),
        "resolve6" => serde_json::json!(resolver.ipv6_lookup(query.host).map_err(failed)?.iter().map(|ip| ip.to_string()).collect::<Vec<_>>()),
        "resolveTxt" => serde_json::json!(resolver.txt_lookup(query.host).map_err(failed)?.iter().map(|r| r.txt_data().iter().map(|v| String::from_utf8_lossy(v).into_owned()).collect::<Vec<_>>()).collect::<Vec<_>>()),
        "resolveSrv" => serde_json::json!(resolver.srv_lookup(query.host).map_err(failed)?.iter().map(|r| serde_json::json!({"name":r.target().to_utf8().trim_end_matches('.'),"port":r.port(),"priority":r.priority(),"weight":r.weight()})).collect::<Vec<_>>()),
        _ => return Err("ENOSYS".into()),
    };
    Ok(Answer::Records(value))
}

fn start(args: &[JsValue], kind: &str, promise: bool, cx: &mut Context) -> JsResult<JsValue> {
    let host = text_of(&arg(args, 0), cx)?;
    let options = arg(args, 1);
    let family = if options.is_number() {
        options.to_number(cx)? as u8
    } else {
        get(&options, "family", cx)?.as_number().unwrap_or(0.) as u8
    };
    if ![0, 4, 6].contains(&family) {
        return Err(crate::native::coded_error(
            cx,
            "The property 'options.family' must be one of: 0, 4, 6. Received invalid family",
            "ERR_INVALID_ARG_VALUE",
        ));
    }
    let all = get(&options, "all", cx)?.to_boolean();
    let ttl = get(&options, "ttl", cx)?.to_boolean();
    let granted = crate::shared(cx).network.borrow().permissions.dns;
    if !granted {
        return crate::sockets::denied_dns(args, kind, promise, cx);
    }
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
    crate::shared(cx).network.borrow_mut().dns.pending.insert(
        id,
        Pending {
            callback,
            reject,
            all,
            promise,
            host: host.clone(),
            kind: kind.into(),
            ttl,
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
                        _ => "resolve4",
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
            let order = crate::shared(cx).network.borrow().dns.order.clone();
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
            if p.ttl && (p.kind == "resolve4" || p.kind == "resolve6") {
                // TTL is supplied by record lookups below, not synthesized by getaddrinfo.
                Ok(value)
            } else {
                Ok(value)
            }
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
pub(crate) fn poll(_: &mut Context, _: Option<std::time::Instant>) -> usize {
    0
}
