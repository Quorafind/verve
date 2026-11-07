//! What the embedder says of its custom schemes: which have an origin (`standard`) and which are
//! secure contexts. Chromium's own rules for the web's schemes apply to the rest.

use url::Url;

/// The custom schemes `registerSchemesAsPrivileged` made standard and secure.
#[derive(Clone, Debug, Default)]
pub struct Schemes {
    standard: Vec<String>,
    secure: Vec<String>,
}

impl Schemes {
    /// The schemes `registerSchemesAsPrivileged` made standard (they have an origin) and secure
    /// (their pages are secure contexts), by name.
    pub fn new(standard: &[String], secure: &[String]) -> Self {
        let lower = |list: &[String]| list.iter().map(|s| s.to_ascii_lowercase()).collect();
        Self {
            standard: lower(standard),
            secure: lower(secure),
        }
    }

    /// The origin of `url` as `location.origin` writes it: `scheme://host[:port]` for the web's
    /// schemes and the embedder's standard ones, `"null"` for any other.
    pub fn origin_of(&self, url: &Url) -> String {
        if !url.origin().is_tuple() && self.standard.iter().any(|s| s == url.scheme()) {
            return match (url.host_str(), url.port()) {
                (Some(host), Some(port)) => format!("{}://{host}:{port}", url.scheme()),
                (Some(host), None) => format!("{}://{host}", url.scheme()),
                _ => "null".to_owned(),
            };
        }
        url.origin().ascii_serialization()
    }

    /// Whether two addresses have the same origin; an opaque origin is the same as none.
    pub fn same_origin(&self, a: &Url, b: &Url) -> bool {
        let origin = self.origin_of(a);
        origin != "null" && origin == self.origin_of(b)
    }

    /// Whether a document at `url` is a secure context: `https`, a loopback `http`, local
    /// documents, and the embedder's secure schemes.
    pub fn is_secure(&self, url: &Url) -> bool {
        match url.scheme() {
            "https" | "wss" | "file" | "about" | "blob" | "data" => true,
            "http" | "ws" => url.host_str().is_some_and(is_loopback),
            scheme => self.secure.iter().any(|s| s == scheme),
        }
    }
}

/// `localhost`, `*.localhost`, `127.0.0.0/8` and `::1`: the hosts Chromium trusts over plain `http`.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_matches(['[', ']']);
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schemes() -> Schemes {
        Schemes::new(&["App".into()], &["app".into()])
    }

    #[test]
    fn a_standard_custom_scheme_has_an_origin_and_the_others_do_not() {
        let at = |text: &str| Url::parse(text).unwrap();
        let schemes = schemes();
        assert_eq!(
            schemes.origin_of(&at("app://bundle/index.html")),
            "app://bundle"
        );
        assert_eq!(
            schemes.origin_of(&at("app://bundle:81/x")),
            "app://bundle:81"
        );
        assert_eq!(schemes.origin_of(&at("other://bundle/x")), "null");
        assert_eq!(
            schemes.origin_of(&at("https://example.com/x")),
            "https://example.com"
        );
    }

    #[test]
    fn only_loopback_http_and_the_secure_schemes_are_secure() {
        let at = |text: &str| Url::parse(text).unwrap();
        let schemes = schemes();
        for (url, secure) in [
            ("https://example.com/", true),
            ("http://example.com/", false),
            ("http://localhost:3000/", true),
            ("http://127.0.0.1/", true),
            ("app://bundle/", true),
            ("other://bundle/", false),
            ("file:///index.html", true),
        ] {
            assert_eq!(schemes.is_secure(&at(url)), secure, "{url}");
        }
    }
}
