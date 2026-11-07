//! A session's proxy (`session.setProxy`): Chromium's `proxyRules` and `proxyBypassRules` read
//! into the route of each request ([`Proxy::route`], which the session's requests ask for one by
//! one) and into what `resolveProxy` answers from the same decision. Observed in Electron
//! 39.8.10 (`probe7`), see P16's contract; the grammar's finer points (`socks=` defaulting to
//! SOCKS4 on port 1080, a bare entry counting only first, `direct://` in a list, case-insensitive
//! schemes, the implicit bypass of link-local addresses) are Chromium's `proxy_config.cc` and
//! `proxy_string_util.cc` as read, not observed.

use std::net::IpAddr;

use verve::browser_net::Route;

/// One proxy of a rule: `http://host:port`, `https://…`, `socks4://…` or `socks5://…`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub scheme: &'static str,
    pub host: String,
    pub port: u16,
}

/// What one entry of a rule's list says: a proxy, or a direct connection (`direct://`).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pick {
    Direct,
    Proxy(Server),
}

impl Server {
    /// `[scheme://]host[:port]`, with the scheme a missing one defaults to (`bare`) and the port
    /// each scheme has; `direct://` is the direct connection. Schemes are not case-sensitive.
    fn parse(text: &str, bare: &'static str) -> Option<Pick> {
        let text = text.trim();
        let (scheme, rest) = match text.split_once("://") {
            Some((scheme, rest)) => {
                let scheme = match scheme.to_ascii_lowercase().as_str() {
                    "direct" => return Some(Pick::Direct),
                    "http" => "http",
                    "https" => "https",
                    "socks4" | "socks" => "socks4",
                    "socks5" => "socks5",
                    _ => return None,
                };
                (scheme, rest)
            }
            None => (bare, text),
        };
        let rest = rest.trim_end_matches('/');
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) if !host.ends_with(':') => (host, Some(port.parse().ok()?)),
            _ => (rest, None),
        };
        if host.is_empty() {
            return None;
        }
        let port = port.unwrap_or(match scheme {
            "https" => 443,
            "socks4" | "socks5" => 1080,
            _ => 80,
        });
        Some(Pick::Proxy(Self {
            scheme,
            host: host.to_owned(),
            port,
        }))
    }

    /// As `resolveProxy` answers it.
    fn pac(&self) -> String {
        let kind = match self.scheme {
            "https" => "HTTPS",
            "socks4" => "SOCKS",
            "socks5" => "SOCKS5",
            _ => "PROXY",
        };
        format!("{kind} {}:{}", self.host, self.port)
    }
}

/// What `proxyRules` says: one proxy for everything, or one for each scheme.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Rules {
    all: Option<Pick>,
    http: Option<Pick>,
    https: Option<Pick>,
    ftp: Option<Pick>,
    socks: Option<Pick>,
}

impl Rules {
    fn parse(text: &str) -> Self {
        let mut rules = Self::default();
        let parts = text.split(';').map(str::trim).filter(|p| !p.is_empty());
        for (index, part) in parts.enumerate() {
            let (scheme, list) = match part.split_once('=') {
                Some((scheme, list)) => (Some(scheme.trim().to_ascii_lowercase()), list),
                None => (None, part),
            };
            // A bare entry stands for every scheme only as the first one; a `socks=` entry's
            // bare servers are SOCKS4, the others' are `http:`.
            let bare = if scheme.as_deref() == Some("socks") {
                "socks4"
            } else {
                "http"
            };
            let server = list.split(',').find_map(|entry| Server::parse(entry, bare));
            match scheme.as_deref() {
                None if index > 0 => {}
                None => rules.all = server,
                Some("http") => rules.http = server,
                Some("https") => rules.https = server,
                Some("ftp") => rules.ftp = server,
                Some("socks") => rules.socks = server,
                Some(_) => {}
            }
        }
        rules
    }

    /// The proxy for requests of `scheme`.
    fn for_scheme(&self, scheme: &str) -> Option<&Pick> {
        let own = match scheme {
            "http" | "ws" => &self.http,
            "https" | "wss" => &self.https,
            "ftp" => &self.ftp,
            _ => &None,
        };
        own.as_ref().or(self.all.as_ref()).or(self.socks.as_ref())
    }
}

/// Which hosts are reached without the proxy.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bypass {
    /// The loopback and the link-local addresses are never proxied, unless `<-loopback>` says
    /// they are.
    loopback: bool,
    local: bool,
    rules: Vec<String>,
}

impl Bypass {
    fn parse(text: &str) -> Self {
        let mut bypass = Self {
            loopback: true,
            local: false,
            rules: Vec::new(),
        };
        let entries = text
            .split([',', ';'])
            .map(str::trim)
            .filter(|e| !e.is_empty());
        for entry in entries {
            match entry {
                "<local>" => bypass.local = true,
                "<-loopback>" => bypass.loopback = false,
                other => bypass.rules.push(other.to_ascii_lowercase()),
            }
        }
        bypass
    }

    /// Whether a request to `host`:`port` goes direct.
    fn applies(&self, host: &str, port: Option<u16>) -> bool {
        let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
        let address = host.parse::<IpAddr>().ok();
        if self.loopback && is_loopback(&host, address) {
            return true;
        }
        if self.local && address.is_none() && !host.contains('.') {
            return true;
        }
        self.rules
            .iter()
            .any(|rule| rule_matches(rule, &host, address, port))
    }
}

fn is_loopback(host: &str, address: Option<IpAddr>) -> bool {
    match address {
        Some(IpAddr::V4(address)) => address.is_loopback() || address.is_link_local(),
        Some(IpAddr::V6(address)) => {
            address.is_loopback() || address.segments()[0] & 0xffc0 == 0xfe80
        }
        None => host == "localhost" || host.ends_with(".localhost"),
    }
}

/// One bypass rule against a host: `[scheme://]host-pattern[:port]`, a pattern with `*` wildcards,
/// or an address block `a.b.c.d/n`.
fn rule_matches(rule: &str, host: &str, address: Option<IpAddr>, port: Option<u16>) -> bool {
    let rule = rule.split_once("://").map_or(rule, |(_, rest)| rest);
    if let (Some((net, bits)), Some(address)) = (rule.split_once('/'), address) {
        return net
            .parse::<IpAddr>()
            .ok()
            .zip(bits.parse::<u32>().ok())
            .is_some_and(|(net, bits)| in_block(address, net, bits));
    }
    let (pattern, wanted) = match rule.rsplit_once(':') {
        Some((pattern, port)) if port.parse::<u16>().is_ok() && !pattern.ends_with(':') => {
            (pattern, port.parse::<u16>().ok())
        }
        _ => (rule, None),
    };
    if wanted.is_some() && wanted != port {
        return false;
    }
    let pattern = pattern.trim_matches(['[', ']']);
    match pattern.strip_prefix('.') {
        Some(suffix) => host.ends_with(&format!(".{suffix}")),
        None => glob(pattern, host),
    }
}

fn in_block(address: IpAddr, net: IpAddr, bits: u32) -> bool {
    match (address, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
            u32::from(a) & mask == u32::from(n) & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
            u128::from(a) & mask == u128::from(n) & mask
        }
        _ => false,
    }
}

/// `text` against `pattern`, where `*` stands for any run of characters.
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let [first, .., last] = parts.as_slice() else {
        return pattern == text;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// How a session reaches the network.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Proxy {
    rules: Rules,
    bypass: Option<Bypass>,
}

/// What `setProxy` was given for `mode`, or why it was not taken.
pub struct Config<'a> {
    pub mode: Option<&'a str>,
    pub rules: Option<&'a str>,
    pub bypass: Option<&'a str>,
}

/// Electron's modes, in the order its error names them.
const MODES: &str = "direct, auto_detect, pac_script, fixed_servers or system";

impl Proxy {
    /// The proxy `config` asks for; direct connections are `None`. `system` is what `system`
    /// reads from the machine (`rules`, `bypass`).
    pub fn from_config(
        config: &Config<'_>,
        system: impl FnOnce() -> (String, String),
    ) -> Result<Option<Self>, String> {
        let mode = match (config.mode, config.rules) {
            (Some(mode), _) => mode,
            (None, Some(_)) => "fixed_servers",
            (None, None) => "direct",
        };
        let (rules, bypass) = match mode {
            "direct" | "auto_detect" | "pac_script" => return Ok(None),
            "fixed_servers" => (
                config.rules.unwrap_or_default().to_owned(),
                config.bypass.unwrap_or_default().to_owned(),
            ),
            "system" => system(),
            _ => return Err(format!("Invalid mode, must be one of {MODES}")),
        };
        let rules = Rules::parse(&rules);
        let any = [
            &rules.all,
            &rules.http,
            &rules.https,
            &rules.ftp,
            &rules.socks,
        ]
        .iter()
        .any(|pick| matches!(pick, Some(Pick::Proxy(_))));
        Ok(any.then(|| Self {
            rules,
            bypass: Some(Bypass::parse(&bypass)),
        }))
    }

    /// The proxy a request for `url` goes through; `None` for a direct connection (a bypassed
    /// host, a scheme without a proxy, or a `direct://` entry). `resolveProxy` and the requests
    /// both ask this, so they never disagree.
    fn server_for(&self, url: &url::Url) -> Option<&Server> {
        let port = url.port_or_known_default();
        let bypassed = self.bypass.as_ref().is_some_and(|bypass| {
            url.host_str()
                .is_some_and(|host| bypass.applies(host, port))
        });
        match self.rules.for_scheme(url.scheme()) {
            Some(Pick::Proxy(server)) if !bypassed => Some(server),
            _ => None,
        }
    }

    /// `resolveProxy(url)`: `DIRECT`, or the proxy as a PAC script names it.
    pub fn resolve(&self, url: &url::Url) -> String {
        self.server_for(url)
            .map_or_else(|| "DIRECT".to_owned(), Server::pac)
    }

    /// How a request for `url` reaches the network: through the `http:` proxy its rules name, or
    /// directly. A proxy of another protocol is one Verve cannot speak.
    pub fn route(&self, url: &url::Url) -> Route {
        match self.server_for(url) {
            None => Route::Direct,
            Some(server) if server.scheme == "http" => Route::Via {
                host: server.host.clone(),
                port: server.port,
            },
            Some(server) => Route::Unsupported(format!(
                "Verve proxies through http: proxies only, not {}: {}:{}",
                server.scheme, server.host, server.port
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(rules: &str, bypass: &str, url: &str) -> String {
        let config = Config {
            mode: Some("fixed_servers"),
            rules: Some(rules),
            bypass: Some(bypass),
        };
        let proxy = Proxy::from_config(&config, || unreachable!()).unwrap();
        proxy.map_or_else(
            || "DIRECT".to_owned(),
            |proxy| proxy.resolve(&url::Url::parse(url).unwrap()),
        )
    }

    #[test]
    fn rules_name_a_proxy_for_everything_or_one_for_each_scheme() {
        // Each value is what Electron 39.8.10 answered (`probe7`).
        assert_eq!(
            resolve("127.0.0.1:9", "", "http://example.com/"),
            "PROXY 127.0.0.1:9"
        );
        assert_eq!(
            resolve("http://127.0.0.1:9", "", "https://example.com/"),
            "PROXY 127.0.0.1:9"
        );
        let split = "http=foo:80;https=bar:81";
        assert_eq!(resolve(split, "", "http://a.com/"), "PROXY foo:80");
        assert_eq!(resolve(split, "", "https://a.com/"), "PROXY bar:81");
        assert_eq!(resolve(split, "", "ftp://a.com/"), "DIRECT");
        assert_eq!(
            resolve("socks5://foo:1080", "", "http://a.com/"),
            "SOCKS5 foo:1080"
        );
        assert_eq!(
            resolve("https://foo:443", "", "http://a.com/"),
            "HTTPS foo:443"
        );
    }

    #[test]
    fn the_loopback_and_the_bypass_rules_go_direct() {
        assert_eq!(resolve("foo:80", "", "http://127.0.0.1:5/"), "DIRECT");
        assert_eq!(resolve("foo:80", "", "http://localhost/"), "DIRECT");
        assert_eq!(resolve("foo:80", "a.com", "http://a.com/"), "DIRECT");
        assert_eq!(resolve("foo:80", "a.com", "http://b.com/"), "PROXY foo:80");
        assert_eq!(
            resolve("foo:80", "*.a.com;10.0.0.0/8", "http://x.a.com/"),
            "DIRECT"
        );
        assert_eq!(
            resolve("foo:80", "*.a.com;10.0.0.0/8", "http://10.2.3.4/"),
            "DIRECT"
        );
        assert_eq!(resolve("foo:80", "<local>", "http://intranet/"), "DIRECT");
        assert_eq!(
            resolve("foo:80", "b.com:8080", "http://b.com:81/"),
            "PROXY foo:80"
        );
        assert_eq!(
            resolve("foo:80", "<-loopback>", "http://127.0.0.1/"),
            "PROXY foo:80"
        );
    }

    #[test]
    fn modes_without_rules_are_direct_and_a_bad_mode_is_refused() {
        let direct = |mode, rules| {
            let config = Config {
                mode,
                rules,
                bypass: None,
            };
            Proxy::from_config(&config, || (String::new(), String::new()))
        };
        assert_eq!(direct(Some("direct"), None), Ok(None));
        assert_eq!(direct(Some("fixed_servers"), None), Ok(None));
        assert_eq!(direct(None, None), Ok(None));
        assert_eq!(direct(Some("auto_detect"), None), Ok(None));
        assert!(direct(None, Some("foo:80")).unwrap().is_some());
        let refused = direct(Some("bogus"), None).unwrap_err();
        assert_eq!(
            refused,
            "Invalid mode, must be one of direct, auto_detect, pac_script, fixed_servers or system"
        );
    }

    #[test]
    fn a_request_is_routed_by_the_rules_and_bypass_resolve_proxy_reads() {
        let route = |rules: &str, bypass: &str, url: &str| {
            let config = Config {
                mode: None,
                rules: Some(rules),
                bypass: Some(bypass),
            };
            let proxy = Proxy::from_config(&config, || unreachable!())
                .unwrap()
                .unwrap();
            proxy.route(&url::Url::parse(url).unwrap())
        };
        let via = |host: &str, port| Route::Via {
            host: host.to_owned(),
            port,
        };
        // Only `http:` has a proxy: `https:` goes direct, with no CONNECT to it.
        assert_eq!(route("http=foo:80", "", "http://a.com/"), via("foo", 80));
        assert_eq!(route("http=foo:80", "", "https://a.com/"), Route::Direct);
        assert_eq!(
            route("foo:80", "x.a.com", "http://b.x.a.com/"),
            via("foo", 80)
        );
        assert_eq!(route("foo:80", "*.a.com", "http://b.a.com/"), Route::Direct);
        assert_eq!(route("foo:80", "", "http://127.0.0.1/"), Route::Direct);
        assert!(matches!(
            route("socks5://foo", "", "http://a.com/"),
            Route::Unsupported(_)
        ));
    }

    #[test]
    fn the_rules_grammar_follows_chromium() {
        // `socks=h` is SOCKS4 on port 1080.
        assert_eq!(resolve("socks=h", "", "http://a.com/"), "SOCKS h:1080");
        // A bare entry stands for every scheme only as the first entry.
        assert_eq!(resolve("foo:80", "", "http://a.com/"), "PROXY foo:80");
        assert_eq!(
            resolve("http=bar:81;foo:80", "", "https://a.com/"),
            "DIRECT"
        );
        // `direct://` is an entry that connects directly; schemes are case-insensitive.
        assert_eq!(
            resolve("http=direct://,foo:80", "", "http://a.com/"),
            "DIRECT"
        );
        assert_eq!(
            resolve("HTTP://foo:80", "", "http://a.com/"),
            "PROXY foo:80"
        );
        // Link-local addresses are never proxied either.
        assert_eq!(resolve("foo:80", "", "http://169.254.1.2/"), "DIRECT");
        assert_eq!(resolve("foo:80", "", "http://[fe80::1]/"), "DIRECT");
    }
}
