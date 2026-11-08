//! Chrome extension match patterns (`<all_urls>`, `*://*.example.com/*`, `https://*/*.png`), the
//! filter language of `webRequest`. The texts of the errors are the ones Electron 39.8.10 throws
//! (`Invalid url pattern <pattern>: <why>`), read from its `URLPattern` string table and
//! observed with `session.webRequest.onBeforeRequest({ urls: [...] }, ...)`.

use url::Url;

/// The schemes a pattern may name besides `*`: Chromium's list of valid ones. A custom scheme
/// is `Wrong scheme type.`
const SCHEMES: &[&str] = &[
    "http",
    "https",
    "file",
    "ftp",
    "ws",
    "wss",
    "data",
    "chrome",
    "chrome-extension",
    "chrome-untrusted",
    "filesystem",
    "urn",
];

/// What the scheme part of a pattern accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Scheme {
    /// `*`: `http`, `https`, `ws` and `wss`.
    Web,
    Named(String),
}

/// What the host part accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Host {
    Any,
    /// `*.name`: `name` and its subdomains.
    Below(String),
    Exactly(String),
}

/// One pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    /// `None` for `<all_urls>`.
    parts: Option<Parts>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Parts {
    scheme: Scheme,
    host: Host,
    /// `None`: any port.
    port: Option<u16>,
    path: String,
}

impl Pattern {
    /// Reads `text`; `Err` is the reason Chromium gives.
    ///
    /// # Errors
    ///
    /// Why the text is not a pattern.
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        if text == "<all_urls>" {
            return Ok(Self { parts: None });
        }
        if let Some(rest) = text.strip_prefix("data:") {
            return Ok(Self::of(
                Scheme::Named("data".into()),
                Host::Any,
                None,
                rest,
            ));
        }
        let (scheme, rest) = text.split_once("://").ok_or("Missing scheme separator.")?;
        let scheme = scheme.to_ascii_lowercase();
        let scheme = if scheme == "*" {
            Scheme::Web
        } else if SCHEMES.contains(&scheme.as_str()) {
            Scheme::Named(scheme)
        } else {
            return Err("Wrong scheme type.");
        };
        let file = scheme == Scheme::Named("file".into());
        let slash = rest.find('/');
        // A `file` pattern has no host: `file:///*` is the slash straight away.
        let (authority, path) = match slash {
            Some(at) => (&rest[..at], &rest[at..]),
            None => return Err("Empty path."),
        };
        if authority.is_empty() && !file {
            return Err("Host can not be empty.");
        }
        let (host, port) = split_port(authority)?;
        let host = parse_host(host)?;
        Ok(Self::of(scheme, host, port, path))
    }

    fn of(scheme: Scheme, host: Host, port: Option<u16>, path: &str) -> Self {
        Self {
            parts: Some(Parts {
                scheme,
                host,
                port,
                path: path.to_owned(),
            }),
        }
    }

    /// Whether the pattern matches `url`.
    pub fn matches(&self, url: &Url) -> bool {
        let Some(parts) = &self.parts else {
            return true;
        };
        let scheme = url.scheme();
        let scheme_ok = match &parts.scheme {
            Scheme::Web => matches!(scheme, "http" | "https" | "ws" | "wss"),
            Scheme::Named(name) => name == scheme,
        };
        let host = url.host_str().unwrap_or("").to_ascii_lowercase();
        let host_ok = match &parts.host {
            Host::Any => true,
            Host::Exactly(name) => *name == host,
            Host::Below(name) => host == *name || host.ends_with(&format!(".{name}")),
        };
        let port_ok = parts
            .port
            .is_none_or(|port| url.port_or_known_default() == Some(port));
        let target = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        scheme_ok && host_ok && port_ok && glob(&parts.path, &target)
    }
}

/// The host and the port of `authority` (`host:port`, `[::1]:80`).
fn split_port(authority: &str) -> Result<(&str, Option<u16>), &'static str> {
    let colon = authority.rfind(':').filter(|at| {
        // The colons inside an IPv6 address's brackets are not a port's.
        !authority[*at..].contains(']')
    });
    let Some(at) = colon else {
        return Ok((authority, None));
    };
    let port = &authority[at + 1..];
    let port = match port {
        "*" => None,
        digits => Some(digits.parse::<u16>().map_err(|_| "Invalid port.")?),
    };
    Ok((&authority[..at], port))
}

fn parse_host(host: &str) -> Result<Host, &'static str> {
    let host = host.to_ascii_lowercase();
    if host == "*" {
        return Ok(Host::Any);
    }
    if let Some(rest) = host.strip_prefix("*.") {
        return if rest.contains('*') {
            Err("Invalid host wildcard.")
        } else {
            Ok(Host::Below(rest.to_owned()))
        };
    }
    if host.contains('*') {
        return Err("Invalid host wildcard.");
    }
    Ok(Host::Exactly(host))
}

/// Whether `text` fits `pattern`, where `*` stands for any run of characters.
fn glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut resume: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && pattern[p] == '*' {
            resume = Some((p + 1, t));
            p += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
            p += 1;
            t += 1;
        } else if let Some((star, at)) = resume {
            resume = Some((star, at + 1));
            p = star;
            t = at + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// The patterns of one `webRequest` filter, and the resource types it is limited to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patterns {
    list: Vec<Pattern>,
    /// `filter.types`; empty for every type.
    types: Vec<String>,
}

impl Patterns {
    /// Every address, every type: what a listener given no filter gets.
    pub fn everything() -> Self {
        Self {
            list: vec![Pattern { parts: None }],
            types: Vec::new(),
        }
    }

    /// Reads `urls` (each a pattern) and the `types` they are limited to.
    ///
    /// # Errors
    ///
    /// The text Electron's `TypeError` carries, for the first pattern that is not one.
    pub fn parse(urls: &[String], types: Vec<String>) -> Result<Self, String> {
        let mut list = Vec::new();
        for url in urls {
            let pattern =
                Pattern::parse(url).map_err(|why| format!("Invalid url pattern {url}: {why}"))?;
            list.push(pattern);
        }
        Ok(Self { list, types })
    }

    /// Whether a request for `url` of `kind` (a `resourceType` name) is one the filter wants.
    pub fn wants(&self, url: &Url, kind: &str) -> bool {
        let typed = self.types.is_empty() || self.types.iter().any(|t| t == kind);
        typed && self.list.iter().any(|pattern| pattern.matches(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(pattern: &str, url: &str) -> bool {
        Pattern::parse(pattern)
            .unwrap()
            .matches(&Url::parse(url).unwrap())
    }

    #[test]
    fn the_documented_shapes_match_what_they_say() {
        assert!(ok("<all_urls>", "https://a.test/x"));
        assert!(ok("*://*.example.com/*", "http://a.example.com/x?y=1"));
        assert!(ok("*://*.example.com/*", "https://example.com/"));
        assert!(!ok("*://*.example.com/*", "https://badexample.com/"));
        assert!(ok("https://*/*.png", "https://h.test/a/b.png"));
        assert!(!ok("https://*/*.png", "http://h.test/a/b.png"));
        assert!(!ok("https://*/*.png", "https://h.test/a/b.jpg"));
        assert!(ok("*://*/blocked*", "http://127.0.0.1:9/blocked1"));
        assert!(!ok("*://*/blocked*", "http://127.0.0.1:9/other"));
        assert!(!ok("*://*/*", "file:///a"));
        assert!(ok("file:///*", "file:///a/b"));
    }

    #[test]
    fn ports_and_queries_take_part() {
        assert!(ok("http://a:80/*", "http://a/x"));
        assert!(!ok("http://a:81/*", "http://a/x"));
        assert!(ok("http://a/*", "http://a:8080/x"));
        assert!(ok("http://a:*/*", "http://a:8080/x"));
        assert!(ok("http://a/x?y=*", "http://a/x?y=2"));
        assert!(!ok("http://a/x", "http://a/x?y=2"));
        assert!(ok("HTTP://A/*", "http://a/"));
    }

    #[test]
    fn a_text_that_is_no_pattern_says_why_in_chromiums_words() {
        let why = |text: &str| Pattern::parse(text).unwrap_err();
        assert_eq!(why("foo"), "Missing scheme separator.");
        assert_eq!(why("*"), "Missing scheme separator.");
        assert_eq!(why("app://*/*"), "Wrong scheme type.");
        assert_eq!(why("http://host"), "Empty path.");
        assert_eq!(why("http:///x"), "Host can not be empty.");
        assert_eq!(why("*://*foo/*"), "Invalid host wildcard.");
        assert_eq!(why("*://foo.*/*"), "Invalid host wildcard.");
        assert_eq!(why("http://a:abc/*"), "Invalid port.");
    }

    #[test]
    fn a_filter_is_limited_by_its_types() {
        let urls = vec!["<all_urls>".to_owned()];
        let filter = Patterns::parse(&urls, vec!["xhr".into()]).unwrap();
        let url = Url::parse("http://a/").unwrap();
        assert!(filter.wants(&url, "xhr") && !filter.wants(&url, "image"));
        let error = Patterns::parse(&["nonsense".to_owned()], Vec::new()).unwrap_err();
        assert_eq!(
            error,
            "Invalid url pattern nonsense: Missing scheme separator."
        );
    }
}
