//! A session's cookies, as they cross threads: the cookie itself, and what happened to one.

/// `sameSite`, by the names `Cookie.sameSite` uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SameSite {
    #[default]
    Unspecified,
    NoRestriction,
    Lax,
    Strict,
}

impl SameSite {
    /// The name `Cookie.sameSite` carries.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::NoRestriction => "no_restriction",
            Self::Lax => "lax",
            Self::Strict => "strict",
        }
    }

    /// The value of `name`; `None` for any other text.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "unspecified" => Self::Unspecified,
            "no_restriction" => Self::NoRestriction,
            "lax" => Self::Lax,
            "strict" => Self::Strict,
            _ => return None,
        })
    }
}

/// One cookie of a jar.
#[derive(Clone, Debug, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The host or domain it belongs to, lowercase and without the leading dot that
    /// `Cookie.domain` shows for a domain cookie.
    pub domain: String,
    /// Sent to exactly `domain`, not to its subdomains.
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    /// When it expires, in seconds since the Unix epoch; `None` for a session cookie.
    pub expires: Option<f64>,
    /// Orders the cookies of one path by when they were made: the jar's own counter.
    pub created: u64,
}

/// Why a cookie changed, by the names `changed`'s `cause` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CookieCause {
    Explicit,
    Overwrite,
    Expired,
    Evicted,
    ExpiredOverwrite,
}

impl CookieCause {
    pub fn name(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Overwrite => "overwrite",
            Self::Expired => "expired",
            Self::Evicted => "evicted",
            Self::ExpiredOverwrite => "expired-overwrite",
        }
    }
}

/// A cookie that was added, or taken away, and why: one `changed` event.
#[derive(Clone, Debug, PartialEq)]
pub struct CookieChange {
    pub cookie: Cookie,
    pub cause: CookieCause,
    pub removed: bool,
}
