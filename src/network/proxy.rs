//! Outbound HTTP proxies, as Node 24's `NODE_USE_ENV_PROXY` has them.
//!
//! A host that confines a realm's process — an OS sandbox leaving it no network but a proxy
//! that admits the hosts it was granted — points it at that proxy the standard way:
//! `HTTP_PROXY`, `HTTPS_PROXY` and `NO_PROXY`, and `NODE_USE_ENV_PROXY=1` because Node reads them
//! only when told to. Without this a confined realm could reach nothing at all.
//!
//! **What goes through the proxy**: `fetch`, `WebSocket`, and `http.request`/`https.request` on
//! the global agents — or on an `Agent` made with a `proxyEnv` of its own, as Node's are. A
//! request with an agent of its own, or `agent: false`, goes direct; so do `net.connect` and
//! `tls.connect`. An `http:` request is sent to the proxy whole, its request-target the absolute
//! URL; `https:`, `ws:` and `wss:` open a `CONNECT` tunnel first, and TLS runs inside it to the
//! real host, whose name is the one SNI sends and the certificate is verified against. The
//! credentials in a proxy URL become `Proxy-Authorization: Basic`. The target's name is never
//! resolved here: the proxy resolves it.
//!
//! **Which variables**: the lower-case name first, then the upper-case one, as Node reads them.
//! `https:` requests use `HTTPS_PROXY` and `http:` ones `HTTP_PROXY`; `fetch` and `WebSocket`,
//! which are undici's in Node, fall back to `HTTP_PROXY` for `https:` when `HTTPS_PROXY` is not
//! set. `ALL_PROXY` is not read, as Node does not read it.
//!
//! **`NO_PROXY`** is a comma-separated list, each entry matched against the target:
//! `*` (everything), a domain — `example.com`, `.example.com` or `*.example.com`, each matching
//! the domain and every name under it, as curl has them — an IP address (`10.1.2.3`, `::1`, or
//! `[::1]`), a CIDR block (`10.0.0.0/8`) or a range (`10.0.0.1-10.0.0.9`), any of them with a
//! `:port` to match that port only. Loopback is proxied like any other host unless it is listed.
//!
//! **Permissions** are checked against the target, the destination the script named: a request
//! to a host `Permissions.outbound` does not grant is refused before any connection is made. The
//! proxy the embedder configured ([`crate::Runtime::set_proxy_env`], which `verve run` sets from
//! its environment) is infrastructure and needs no rule of its own. A proxy a script names
//! itself, in an `Agent`'s `proxyEnv`, is not: it must be granted like any host.
//!
//! **Not supported**: a proxy reached over TLS (`https://`) and SOCKS (`socks5://`). A request
//! that would go through one fails with `ERR_PROXY_INVALID_CONFIG` rather than going direct.

use boa_engine::{Context, JsData, JsObject, JsResult, JsValue};
use boa_gc::{Finalize, Trace};
use std::net::IpAddr;
use std::rc::Rc;

use crate::native::get;

/// The proxy variables a realm routes its requests by: Node's `NODE_USE_ENV_PROXY`, for an
/// embedder to set without touching any process's environment. Empty values are unset.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyEnv {
    /// `HTTP_PROXY`: the proxy for `http:` requests.
    pub http_proxy: Option<String>,
    /// `HTTPS_PROXY`: the proxy for `https:` requests and WebSockets.
    pub https_proxy: Option<String>,
    /// `NO_PROXY`: the hosts that are reached directly.
    pub no_proxy: Option<String>,
}

impl ProxyEnv {
    /// The variables among `vars`, the lower-case name preferred, as Node reads them.
    pub fn from_vars<K: AsRef<str>, V: AsRef<str>>(vars: impl IntoIterator<Item = (K, V)>) -> Self {
        let vars: Vec<(String, String)> = vars
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_owned(), v.as_ref().to_owned()))
            .collect();
        let read = |name: &str| {
            let find = |key: &str| {
                vars.iter()
                    .find(|(k, v)| k == key && !v.is_empty())
                    .map(|(_, v)| v.clone())
            };
            find(name).or_else(|| find(&name.to_ascii_uppercase()))
        };
        Self {
            http_proxy: read("http_proxy"),
            https_proxy: read("https_proxy"),
            no_proxy: read("no_proxy"),
        }
    }

    /// The process's own proxy variables when `NODE_USE_ENV_PROXY=1` asks for them, as Node 24
    /// does at startup; `None` otherwise.
    pub fn from_process_env() -> Option<Self> {
        (std::env::var("NODE_USE_ENV_PROXY").as_deref() == Ok("1")).then(Self::from_process_vars)
    }

    /// The process's own proxy variables, whatever `NODE_USE_ENV_PROXY` says.
    pub fn from_process_vars() -> Self {
        Self::from_vars(std::env::vars_os().map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.to_string_lossy().into_owned(),
            )
        }))
    }
}

/// One proxy, as a connection is routed through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Proxy {
    pub host: String,
    pub port: u16,
    /// `Basic …`, from the URL's credentials.
    pub authorization: Option<String>,
}

/// How a connection reaches its target through a proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Via {
    pub proxy: Proxy,
    /// A `CONNECT` tunnel; otherwise the connection is to the proxy itself, for absolute-URI
    /// requests.
    pub tunnel: bool,
}

/// A [`Via`] as a value only Rust makes: the hidden `__verveVia` of a connection's options.
#[derive(Trace, Finalize, JsData)]
struct ViaData {
    #[unsafe_ignore_trace]
    via: Via,
}

pub(crate) fn via_object(via: Via) -> JsObject {
    JsObject::from_proto_and_data(None, ViaData { via })
}

/// The route a connection's options carry, if they carry one.
pub(crate) fn via_of(options: &JsValue, cx: &mut Context) -> JsResult<Option<Via>> {
    let value = get(options, "__verveVia", cx)?;
    Ok(value
        .as_object()
        .and_then(|o| o.downcast_ref::<ViaData>().map(|d| d.via.clone())))
}

/// An agent's own proxy configuration, from its `proxyEnv`.
#[derive(Trace, Finalize, JsData)]
pub(crate) struct ConfigData {
    #[unsafe_ignore_trace]
    pub config: Rc<Config>,
}

/// `proxyEnv` as Node's `Agent` takes it: an object with the variables' names as keys.
pub(crate) fn config_of_env(value: &JsValue, cx: &mut Context) -> JsResult<Config> {
    let mut vars = Vec::new();
    for name in ["http_proxy", "https_proxy", "no_proxy"] {
        for key in [name.to_owned(), name.to_ascii_uppercase()] {
            let v = get(value, &key, cx)?;
            if !v.is_undefined() && !v.is_null() {
                vars.push((key, crate::native::text_of(&v, cx)?));
            }
        }
    }
    Ok(Config::parse(&ProxyEnv::from_vars(vars)))
}

/// [`ProxyEnv`] parsed: the proxies, or why one cannot be used, and the bypass list.
#[derive(Clone, Debug, Default)]
pub(crate) struct Config {
    http: Option<Result<Proxy, String>>,
    https: Option<Result<Proxy, String>>,
    bypass: Vec<Bypass>,
}

impl Config {
    pub fn parse(env: &ProxyEnv) -> Self {
        Self {
            http: env.http_proxy.as_deref().map(proxy),
            https: env.https_proxy.as_deref().map(proxy),
            bypass: env
                .no_proxy
                .as_deref()
                .unwrap_or_default()
                .split(',')
                .filter_map(Bypass::parse)
                .collect(),
        }
    }

    /// The proxy a request to `host:port` goes through, `None` for a direct connection, or the
    /// error that the proxy it should use cannot be. `fallback` is undici's: `https:` uses the
    /// `http:` proxy when there is no `https:` one.
    pub fn route(
        &self,
        secure: bool,
        fallback: bool,
        host: &str,
        port: u16,
    ) -> Option<Result<Proxy, String>> {
        let chosen = if secure {
            self.https
                .as_ref()
                .or(if fallback { self.http.as_ref() } else { None })
        } else {
            self.http.as_ref()
        }?;
        if self.bypasses(host, port) {
            return None;
        }
        Some(chosen.clone())
    }

    /// The proxies this configuration names: what the reactor lets a realm reach without a
    /// rule, when the embedder configured them.
    pub fn endpoints(&self) -> Vec<(String, u16)> {
        [&self.http, &self.https]
            .into_iter()
            .flatten()
            .flatten()
            .map(|p| (p.host.clone(), p.port))
            .collect()
    }

    fn bypasses(&self, host: &str, port: u16) -> bool {
        let host = host
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let ip = host.parse::<IpAddr>().ok();
        self.bypass
            .iter()
            .any(|entry| entry.matches(&host, ip, port))
    }
}

/// A proxy URL, or why Verve cannot use it. A URL with no scheme is an `http:` one, as curl
/// takes it.
fn proxy(text: &str) -> Result<Proxy, String> {
    let text = text.trim();
    let url = if text.contains("://") {
        url::Url::parse(text)
    } else {
        url::Url::parse(&format!("http://{text}"))
    }
    .map_err(|e| format!("Invalid proxy URL: {text} ({e})"))?;
    if url.scheme() != "http" {
        return Err(format!(
            "Unsupported proxy protocol {}: in {text}: Verve proxies through http: proxies only",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| format!("Invalid proxy URL: {text} (no host)"))?
        .trim_matches(['[', ']'])
        .to_owned();
    let authorization = (!url.username().is_empty() || url.password().is_some()).then(|| {
        let credentials = format!(
            "{}:{}",
            percent_decoded(url.username()),
            percent_decoded(url.password().unwrap_or_default())
        );
        format!(
            "Basic {}",
            crate::encoding::base64(credentials.as_bytes(), false)
        )
    });
    Ok(Proxy {
        host,
        port: url.port().unwrap_or(80),
        authorization,
    })
}

fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| hex(*b)),
                bytes.get(i + 2).and_then(|b| hex(*b)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// One `NO_PROXY` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bypass {
    host: Pattern,
    port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Pattern {
    Any,
    /// The domain and every name under it.
    Domain(String),
    Range(IpAddr, IpAddr),
    Block(IpAddr, u8),
}

impl Bypass {
    fn parse(entry: &str) -> Option<Self> {
        let entry = entry.trim().to_ascii_lowercase();
        if entry.is_empty() {
            return None;
        }
        if entry == "*" {
            return Some(Self {
                host: Pattern::Any,
                port: None,
            });
        }
        // `[v6]:port`, `host:port`, or a bare IPv6 address (more than one colon).
        let (host, port) = if let Some(rest) = entry.strip_prefix('[') {
            let (host, rest) = rest.split_once(']')?;
            (host.to_owned(), rest.strip_prefix(':'))
        } else {
            match entry.rsplit_once(':') {
                Some((host, port)) if !host.contains(':') => (host.to_owned(), Some(port)),
                _ => (entry.clone(), None),
            }
        };
        let port = match port {
            Some(port) => Some(port.parse::<u16>().ok()?),
            None => None,
        };
        let host = if let Ok(ip) = host.parse::<IpAddr>() {
            Pattern::Range(ip, ip)
        } else if let Some((ip, bits)) = host.split_once('/') {
            let ip = ip.parse::<IpAddr>().ok()?;
            let bits = bits.parse::<u8>().ok()?;
            if bits > if ip.is_ipv4() { 32 } else { 128 } {
                return None;
            }
            Pattern::Block(ip, bits)
        } else if let Some((Ok(low), Ok(high))) = host
            .split_once('-')
            .map(|(a, b)| (a.trim().parse::<IpAddr>(), b.trim().parse::<IpAddr>()))
        {
            Pattern::Range(low, high)
        } else {
            let domain = host
                .trim_start_matches('*')
                .trim_start_matches('.')
                .trim_end_matches('.');
            if domain.is_empty() {
                return None;
            }
            Pattern::Domain(domain.to_owned())
        };
        Some(Self { host, port })
    }

    fn matches(&self, host: &str, ip: Option<IpAddr>, port: u16) -> bool {
        if self.port.is_some_and(|p| p != port) {
            return false;
        }
        match (&self.host, ip) {
            (Pattern::Any, _) => true,
            (Pattern::Domain(domain), None) => {
                host == domain
                    || host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|rest| rest.ends_with('.'))
            }
            (Pattern::Range(low, high), Some(ip)) => {
                let (low, high, ip) = (bits(low), bits(high), bits(&ip));
                low.0 == ip.0 && low.1 <= ip.1 && ip.1 <= high.1 && low.0 == high.0
            }
            (Pattern::Block(network, prefix), Some(ip)) => {
                let (network, ip) = (bits(network), bits(&ip));
                let width = if network.0 { 128 } else { 32 };
                let shift = width - u32::from(*prefix);
                network.0 == ip.0 && (shift >= 128 || network.1 >> shift == ip.1 >> shift)
            }
            _ => false,
        }
    }
}

/// An address as (is IPv6, its bits), IPv4-mapped IPv6 taken as the IPv4 address it is.
fn bits(ip: &IpAddr) -> (bool, u128) {
    match ip {
        IpAddr::V4(v4) => (false, u128::from(u32::from(*v4))),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => (false, u128::from(u32::from(v4))),
            None => (true, u128::from(*v6)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(no_proxy: &str) -> Config {
        Config::parse(&ProxyEnv {
            http_proxy: Some("http://127.0.0.1:3128".into()),
            https_proxy: Some("http://127.0.0.1:3128".into()),
            no_proxy: Some(no_proxy.into()),
        })
    }

    fn direct(no_proxy: &str, host: &str, port: u16) -> bool {
        config(no_proxy).route(true, false, host, port).is_none()
    }

    #[test]
    fn lower_case_names_win_and_empty_ones_are_unset() {
        let env = ProxyEnv::from_vars([
            ("HTTPS_PROXY", "http://upper:1"),
            ("https_proxy", "http://lower:2"),
            ("HTTP_PROXY", "http://upper:3"),
            ("http_proxy", ""),
            ("NO_PROXY", "a.test"),
        ]);
        assert_eq!(env.https_proxy.as_deref(), Some("http://lower:2"));
        assert_eq!(env.http_proxy.as_deref(), Some("http://upper:3"));
        assert_eq!(env.no_proxy.as_deref(), Some("a.test"));
    }

    #[test]
    fn domains_match_themselves_and_what_is_under_them() {
        for entry in [
            "example.com",
            ".example.com",
            "*.example.com",
            " Example.COM ",
        ] {
            assert!(direct(entry, "example.com", 443), "{entry}");
            assert!(direct(entry, "api.example.com", 443), "{entry}");
            assert!(direct(entry, "API.Example.com.", 443), "{entry}");
            assert!(!direct(entry, "notexample.com", 443), "{entry}");
            assert!(!direct(entry, "example.com.evil.test", 443), "{entry}");
        }
        assert!(direct("*", "anything.test", 1));
        assert!(direct("a.test, ,b.test", "b.test", 1));
        assert!(!direct("", "a.test", 1));
    }

    #[test]
    fn ports_addresses_ranges_and_blocks() {
        assert!(direct("example.com:8080", "example.com", 8080));
        assert!(!direct("example.com:8080", "example.com", 443));
        assert!(direct("10.1.2.3", "10.1.2.3", 80));
        assert!(!direct("10.1.2.3", "10.1.2.4", 80));
        assert!(direct("10.0.0.0/8", "10.200.0.1", 80));
        assert!(!direct("10.0.0.0/8", "11.0.0.1", 80));
        assert!(direct("0.0.0.0/0", "11.0.0.1", 80));
        assert!(direct("192.168.1.1-192.168.1.100", "192.168.1.50", 80));
        assert!(!direct("192.168.1.1-192.168.1.100", "192.168.1.101", 80));
        assert!(direct("::1", "[::1]", 80));
        assert!(direct("[::1]:8080", "::1", 8080));
        assert!(!direct("[::1]:8080", "::1", 80));
        assert!(direct("fd00::/8", "fd12::1", 80));
        // An address is never a domain's suffix, and loopback is not implied.
        assert!(!direct("0.1", "10.0.0.1", 80));
        assert!(!direct("example.com", "127.0.0.1", 80));
        assert!(!direct("example.com", "localhost", 80));
    }

    #[test]
    fn proxy_urls() {
        let p = proxy("http://us%40er:p%3Ass@proxy.test:8080").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy.test", 8080));
        assert_eq!(
            p.authorization.as_deref(),
            Some(format!("Basic {}", crate::encoding::base64(b"us@er:p:ss", false)).as_str())
        );
        let p = proxy("127.0.0.1:3128").unwrap();
        assert_eq!(
            (p.host.as_str(), p.port, p.authorization),
            ("127.0.0.1", 3128, None)
        );
        assert_eq!(proxy("http://[::1]").unwrap().host, "::1");
        assert!(
            proxy("socks5://127.0.0.1:1080")
                .unwrap_err()
                .contains("socks5")
        );
        assert!(proxy("https://proxy.test").is_err());
    }

    #[test]
    fn https_falls_back_to_the_http_proxy_for_undici_only() {
        let config = Config::parse(&ProxyEnv {
            http_proxy: Some("http://127.0.0.1:1".into()),
            ..ProxyEnv::default()
        });
        assert!(config.route(true, false, "a.test", 443).is_none());
        assert!(config.route(true, true, "a.test", 443).is_some());
        assert!(config.route(false, false, "a.test", 80).is_some());
        assert_eq!(config.endpoints(), vec![("127.0.0.1".to_owned(), 1)]);
    }
}
