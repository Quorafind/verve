//! A session's cookies: the jar RFC 6265 describes, and the file a persistent session keeps it in.
//!
//! The jar is plain logic over [`Cookie`]s, with the clock a parameter; every method that changes
//! it answers the [`CookieChange`]s it made, for whoever has to tell the main process's
//! `cookies.on('changed')`. [`CookieStore`] puts a jar behind a mutex, where a page's thread and
//! the main process's both reach it, and writes it to `Cookies.json`.
//!
//! There is no public-suffix list: a `Domain` of a single label is taken only when it is the
//! host itself, which keeps `Domain=com` out but lets `Domain=co.uk` in (P41).

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use url::Url;

use crate::msg::{Cookie, CookieCause, CookieChange, SameSite};

/// How long a changed jar waits before it is written again.
const WRITE_EVERY: Duration = Duration::from_secs(30);

/// The most a cookie's name and value may hold together, in bytes.
const MAX_PAIR: usize = 4096;

/// Seconds since the Unix epoch.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64())
}

/// Who is writing a cookie: what each may do differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// A `Set-Cookie` header.
    Http,
    /// `document.cookie`: it can neither write an `HttpOnly` cookie nor replace one.
    Script,
}

/// Who is reading, as far as it decides which cookies are seen.
#[derive(Clone, Copy, Debug, Default)]
pub struct Access {
    /// `document.cookie`: the `HttpOnly` cookies are not seen.
    pub scripted: bool,
    /// The request is made from another site than the one it goes to: a cookie that is not
    /// `SameSite=None` stays home.
    pub cross_site: bool,
}

/// The host of `url`, lowercase, without the brackets of an IPv6 address.
fn host_of(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    Some(
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned(),
    )
}

/// Whether `host` is this machine: `localhost`, or a loopback address.
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Whether a `Secure` cookie may travel with `url`: over TLS, or to this machine, which Chromium
/// treats as secure.
fn secure_transport(url: &Url, host: &str) -> bool {
    matches!(url.scheme(), "https" | "wss") || is_loopback(host)
}

/// RFC 6265 5.1.3: `host` is `domain` or a subdomain of it (never, for an address).
fn domain_match(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    host.strip_suffix(domain)
        .is_some_and(|rest| rest.ends_with('.') && host.parse::<IpAddr>().is_err())
}

/// RFC 6265 5.1.4: `path` is `cookie` or lies below it.
fn path_match(path: &str, cookie: &str) -> bool {
    path == cookie
        || (path.starts_with(cookie)
            && (cookie.ends_with('/') || path[cookie.len()..].starts_with('/')))
}

/// RFC 6265 5.1.4: the path a cookie gets when it names none.
fn default_path(url: &Url) -> String {
    let path = url.path();
    match path.rfind('/') {
        Some(0) | None => "/".to_owned(),
        Some(at) => path[..at].to_owned(),
    }
}

/// The site of `host`, as far as there is no public-suffix list to say: the last two labels,
/// or an address or a single label whole.
pub fn site_of(host: &str) -> String {
    if host.parse::<IpAddr>().is_ok() {
        return host.to_owned();
    }
    let labels: Vec<&str> = host.split('.').collect();
    labels[labels.len().saturating_sub(2)..].join(".")
}

/// Whether a request from the page at `page` to `target` crosses sites. An empty or unreadable
/// page address does not.
pub fn cross_site(page: &str, target: &Url) -> bool {
    let Some(from) = Url::parse(page).ok().as_ref().and_then(host_of) else {
        return false;
    };
    let to = host_of(target).unwrap_or_default();
    site_of(&from) != site_of(&to)
}

/// What `cookies.get` selects by. A field that is `None` selects everything.
#[derive(Debug, Default)]
pub struct Filter {
    /// The cookies that would be sent to this address.
    pub url: Option<Url>,
    pub name: Option<String>,
    /// Cookies of this domain or a subdomain of it.
    pub domain: Option<String>,
    pub path: Option<String>,
    pub secure: Option<bool>,
    pub session: Option<bool>,
    pub http_only: Option<bool>,
}

impl Filter {
    fn selects(&self, cookie: &Cookie) -> bool {
        let domain = self.domain.as_deref().map(|d| d.trim_start_matches('.'));
        self.name.as_ref().is_none_or(|n| *n == cookie.name)
            && domain.is_none_or(|d| domain_match(&cookie.domain, &d.to_ascii_lowercase()))
            && self.path.as_ref().is_none_or(|p| *p == cookie.path)
            && self.secure.is_none_or(|s| s == cookie.secure)
            && self.session.is_none_or(|s| s == cookie.expires.is_none())
            && self.http_only.is_none_or(|h| h == cookie.http_only)
            && self.url.as_ref().is_none_or(|url| reaches(cookie, url))
    }
}

/// Whether `cookie` would be sent to `url`, whatever the kind of request.
fn reaches(cookie: &Cookie, url: &Url) -> bool {
    let Some(host) = host_of(url) else {
        return false;
    };
    let same_host = if cookie.host_only {
        host == cookie.domain
    } else {
        domain_match(&host, &cookie.domain)
    };
    same_host
        && path_match(url.path(), &cookie.path)
        && (!cookie.secure || secure_transport(url, &host))
}

/// The order cookies are sent in (RFC 6265 5.4): longer paths first, then the older.
fn order(cookies: &mut [&Cookie]) {
    cookies.sort_by(|a, b| {
        (b.path.len(), std::cmp::Reverse(b.created))
            .cmp(&(a.path.len(), std::cmp::Reverse(a.created)))
    });
}

/// The cookies of one session.
#[derive(Debug, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    next: u64,
}

fn change(cookie: &Cookie, cause: CookieCause, removed: bool) -> CookieChange {
    CookieChange {
        cookie: cookie.clone(),
        cause,
        removed,
    }
}

impl Jar {
    /// A jar holding `cookies`, as they were written.
    pub fn with(cookies: Vec<Cookie>) -> Self {
        let next = cookies.iter().map(|c| c.created).max().map_or(0, |n| n + 1);
        Self { cookies, next }
    }

    /// The cookies that outlive the app: those with an expiry that has not passed.
    pub fn persistent(&self, now: f64) -> Vec<&Cookie> {
        let keep = |c: &&Cookie| c.expires.is_some_and(|e| e > now);
        self.cookies.iter().filter(keep).collect()
    }

    fn expired(cookie: &Cookie, now: f64) -> bool {
        cookie.expires.is_some_and(|e| e <= now)
    }

    /// Drops the cookies whose time has passed.
    pub fn sweep(&mut self, now: f64) -> Vec<CookieChange> {
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.cookies)
            .into_iter()
            .partition(|c| Self::expired(c, now));
        self.cookies = kept;
        gone.iter()
            .map(|c| change(c, CookieCause::Expired, true))
            .collect()
    }

    /// The cookies `url` is sent, in the order they are sent in.
    fn sent_to(&self, url: &Url, access: Access, now: f64) -> Vec<&Cookie> {
        let mut found: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| !Self::expired(c, now) && reaches(c, url))
            .filter(|c| !(access.scripted && c.http_only))
            .filter(|c| !access.cross_site || c.same_site == SameSite::NoRestriction)
            .collect();
        order(&mut found);
        found
    }

    /// The `Cookie` header for a request to `url`; empty when there is none to send.
    pub fn header(&self, url: &Url, access: Access, now: f64) -> String {
        let pair = |c: &&Cookie| {
            if c.name.is_empty() {
                c.value.clone()
            } else {
                format!("{}={}", c.name, c.value)
            }
        };
        let sent = self.sent_to(url, access, now);
        sent.iter().map(pair).collect::<Vec<_>>().join("; ")
    }

    /// The cookies `filter` selects, in the order they are sent in.
    pub fn query(&self, filter: &Filter, now: f64) -> Vec<Cookie> {
        let mut found: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| !Self::expired(c, now) && filter.selects(c))
            .collect();
        order(&mut found);
        found.into_iter().cloned().collect()
    }

    /// Puts `cookie` in the jar, replacing the one of the same name, domain and path. A cookie
    /// that is already expired only removes what it would replace.
    pub fn insert(&mut self, mut cookie: Cookie, now: f64) -> Vec<CookieChange> {
        let mut changes = self.sweep(now);
        let same = |c: &Cookie| {
            (&c.name, &c.domain, &c.path, c.host_only)
                == (&cookie.name, &cookie.domain, &cookie.path, cookie.host_only)
        };
        let expired = Self::expired(&cookie, now);
        if let Some(at) = self.cookies.iter().position(same) {
            let old = self.cookies.remove(at);
            let cause = if expired {
                CookieCause::ExpiredOverwrite
            } else {
                CookieCause::Overwrite
            };
            changes.push(change(&old, cause, true));
        }
        if !expired {
            cookie.created = self.next;
            self.next += 1;
            changes.push(change(&cookie, CookieCause::Explicit, false));
            self.cookies.push(cookie);
        }
        changes
    }

    /// Removes the cookies named `name` that `url` is sent (`cookies.remove`).
    pub fn remove(&mut self, url: &Url, name: &str, now: f64) -> Vec<CookieChange> {
        let mut changes = self.sweep(now);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.cookies)
            .into_iter()
            .partition(|c| c.name == name && reaches(c, url));
        self.cookies = kept;
        changes.extend(gone.iter().map(|c| change(c, CookieCause::Explicit, true)));
        changes
    }

    /// Reads a `Set-Cookie` line received for `url` as RFC 6265 5.3 says and stores the cookie,
    /// unless the jar refuses it. `cross_site`: the response is to a request from another site.
    pub fn store(
        &mut self,
        url: &Url,
        line: &str,
        source: Source,
        cross_site: bool,
        now: f64,
    ) -> Vec<CookieChange> {
        let Some(cookie) = parse(url, line, source, cross_site, now) else {
            return Vec::new();
        };
        let host = host_of(url).unwrap_or_default();
        let old = self.cookies.iter().find(|c| {
            (&c.name, &c.domain, &c.path, c.host_only)
                == (&cookie.name, &cookie.domain, &cookie.path, cookie.host_only)
        });
        // A script does not replace what only a server may write; a page that is not secure does
        // not replace what was written for a secure one.
        let protected = old.is_some_and(|old| {
            (source == Source::Script && old.http_only)
                || (old.secure && !secure_transport(url, &host))
        });
        if protected {
            return Vec::new();
        }
        self.insert(cookie, now)
    }
}

/// The cookie a `Set-Cookie` line makes of itself for `url`, or `None` when it is refused.
fn parse(url: &Url, line: &str, source: Source, cross_site: bool, now: f64) -> Option<Cookie> {
    let parsed = cookie::Cookie::parse(line.trim()).ok()?;
    let host = host_of(url)?;
    if parsed.name().len() + parsed.value().len() > MAX_PAIR {
        return None;
    }
    let http_only = parsed.http_only().unwrap_or(false);
    let secure = parsed.secure().unwrap_or(false);
    if (source == Source::Script && http_only) || (secure && !secure_transport(url, &host)) {
        return None;
    }
    let (domain, host_only) = match parsed.domain().filter(|d| !d.is_empty()) {
        None => (host.clone(), true),
        Some(domain) => {
            let domain = domain.to_ascii_lowercase();
            let single = !domain.contains('.') && domain != host;
            if !domain_match(&host, &domain) || single {
                return None;
            }
            (domain, false)
        }
    };
    let path = match parsed.path() {
        Some(path) if path.starts_with('/') => path.to_owned(),
        _ => default_path(url),
    };
    let same_site = match parsed.same_site() {
        Some(cookie::SameSite::Strict) => SameSite::Strict,
        Some(cookie::SameSite::Lax) => SameSite::Lax,
        Some(cookie::SameSite::None) => SameSite::NoRestriction,
        None => SameSite::Unspecified,
    };
    let blocked = cross_site && matches!(same_site, SameSite::Strict | SameSite::Lax);
    if blocked || (same_site == SameSite::NoRestriction && !secure) {
        return None;
    }
    // The prefixes make a promise about where the cookie came from (RFC 6265bis 4.1.3).
    let name = parsed.name();
    if (name.starts_with("__Secure-") && !secure)
        || (name.starts_with("__Host-") && !(secure && host_only && path == "/"))
    {
        return None;
    }
    // Max-Age wins over Expires.
    let expires = match parsed.max_age() {
        Some(age) => Some(now + age.whole_seconds() as f64),
        None => parsed
            .expires_datetime()
            .map(|at| at.unix_timestamp() as f64),
    };
    Some(Cookie {
        name: name.to_owned(),
        value: parsed.value().to_owned(),
        domain,
        host_only,
        path,
        secure,
        http_only,
        same_site,
        expires,
        created: 0,
    })
}

// ─── The store ──────────────────────────────────────────────────────────────────────────────

/// When the jar was last written, and whether it has changed since.
struct Written {
    dirty: bool,
    at: Option<Instant>,
}

/// A session's jar behind a lock, with the file it is kept in.
pub struct CookieStore {
    jar: Mutex<Jar>,
    /// `Cookies.json`; `None` for a session that keeps nothing.
    file: Option<PathBuf>,
    written: Mutex<Written>,
    /// Whether a script asked for `session.cookies`: until then nobody hears `changed`.
    watched: AtomicBool,
    /// The app is over: the file is not written again.
    closed: AtomicBool,
}

impl CookieStore {
    /// A store reading and writing `file`; with `None`, in memory only.
    pub fn open(file: Option<PathBuf>) -> Self {
        let jar = file.as_deref().map(read_file).unwrap_or_default();
        Self {
            jar: Mutex::new(Jar::with(jar)),
            file,
            written: Mutex::new(Written {
                dirty: false,
                at: None,
            }),
            watched: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    fn jar(&self) -> std::sync::MutexGuard<'_, Jar> {
        self.jar.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Something changed: the file is rewritten if it has been long enough.
    fn touched(&self, changes: &[CookieChange]) {
        if changes.is_empty() || self.file.is_none() {
            return;
        }
        let due = {
            let mut written = self.written.lock().unwrap_or_else(|p| p.into_inner());
            written.dirty = true;
            written.at.is_none_or(|at| at.elapsed() >= WRITE_EVERY)
        };
        if due {
            self.flush();
        }
    }

    /// Whether a script asked for the session's cookies.
    pub fn watched(&self) -> bool {
        self.watched.load(Ordering::Relaxed)
    }

    /// From now on the changes are told to the main process.
    pub fn watch(&self) {
        self.watched.store(true, Ordering::Relaxed);
    }

    /// The `Cookie` header for a request to `url`.
    pub fn header(&self, url: &Url, access: Access) -> String {
        self.jar().header(url, access, now())
    }

    /// Stores a `Set-Cookie` line received for `url`.
    pub fn store(
        &self,
        url: &Url,
        line: &str,
        source: Source,
        cross_site: bool,
    ) -> Vec<CookieChange> {
        let changes = self.jar().store(url, line, source, cross_site, now());
        self.touched(&changes);
        changes
    }

    /// Stores all the `Set-Cookie` lines of one response.
    pub fn store_all<'a>(
        &self,
        url: &Url,
        lines: impl Iterator<Item = &'a str>,
        cross_site: bool,
    ) -> Vec<CookieChange> {
        lines
            .flat_map(|line| self.store(url, line, Source::Http, cross_site))
            .collect()
    }

    /// Puts `cookie` in the jar (`cookies.set`).
    pub fn set(&self, cookie: Cookie) -> Vec<CookieChange> {
        let changes = self.jar().insert(cookie, now());
        self.touched(&changes);
        changes
    }

    /// Removes what `url` is sent under `name` (`cookies.remove`).
    pub fn remove(&self, url: &Url, name: &str) -> Vec<CookieChange> {
        let changes = self.jar().remove(url, name, now());
        self.touched(&changes);
        changes
    }

    /// The cookies `filter` selects (`cookies.get`), and the changes sweeping made on the way.
    pub fn query(&self, filter: &Filter) -> (Vec<Cookie>, Vec<CookieChange>) {
        let mut jar = self.jar();
        let swept = jar.sweep(now());
        let found = jar.query(filter, now());
        drop(jar);
        self.touched(&swept);
        (found, swept)
    }

    /// The app is over: the file is written a last time if `keep`, and never again.
    pub fn close(&self, keep: bool) {
        if keep {
            self.flush();
        }
        self.closed.store(true, Ordering::Relaxed);
    }

    /// Writes the file now, if the jar has changed since it was last written.
    pub fn flush(&self) {
        let Some(file) = self
            .file
            .as_ref()
            .filter(|_| !self.closed.load(Ordering::Relaxed))
        else {
            return;
        };
        let mut written = self.written.lock().unwrap_or_else(|p| p.into_inner());
        if !written.dirty {
            return;
        }
        let jar = self.jar();
        let list: Vec<Value> = jar.persistent(now()).into_iter().map(to_json).collect();
        drop(jar);
        let text = json!({ "version": 1, "cookies": list }).to_string();
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let partial = file.with_extension("json.tmp");
        if std::fs::write(&partial, text).is_ok() && std::fs::rename(&partial, file).is_ok() {
            written.dirty = false;
            written.at = Some(Instant::now());
        }
    }
}

impl Drop for CookieStore {
    fn drop(&mut self) {
        self.flush();
    }
}

fn to_json(c: &Cookie) -> Value {
    json!({
        "name": c.name, "value": c.value, "domain": c.domain, "hostOnly": c.host_only,
        "path": c.path, "secure": c.secure, "httpOnly": c.http_only,
        "sameSite": c.same_site.name(), "expires": c.expires, "created": c.created,
    })
}

fn from_json(value: &Value) -> Option<Cookie> {
    let text = |key: &str| value.get(key)?.as_str().map(str::to_owned);
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    Some(Cookie {
        name: text("name")?,
        value: text("value")?,
        domain: text("domain")?,
        host_only: flag("hostOnly"),
        path: text("path")?,
        secure: flag("secure"),
        http_only: flag("httpOnly"),
        same_site: SameSite::from_name(&text("sameSite")?).unwrap_or_default(),
        expires: Some(value.get("expires")?.as_f64()?),
        created: value.get("created")?.as_u64()?,
    })
}

/// The cookies in `file`; none when it is missing or is not what this wrote.
fn read_file(file: &std::path::Path) -> Vec<Cookie> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let list = parsed.get("cookies").and_then(Value::as_array);
    list.map_or_else(Vec::new, |list| list.iter().filter_map(from_json).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: f64 = 1_000_000.;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn jar_with(lines: &[(&str, &str)]) -> Jar {
        let mut jar = Jar::default();
        for (address, line) in lines {
            jar.store(&url(address), line, Source::Http, false, NOW);
        }
        jar
    }

    fn header(jar: &Jar, address: &str) -> String {
        jar.header(&url(address), Access::default(), NOW)
    }

    #[test]
    fn a_host_only_cookie_goes_to_its_host_and_a_domain_cookie_to_its_subdomains() {
        let jar = jar_with(&[
            ("http://a.example.com/", "h=1"),
            ("http://a.example.com/", "d=2; Domain=example.com"),
        ]);
        assert_eq!(header(&jar, "http://a.example.com/"), "h=1; d=2");
        assert_eq!(header(&jar, "http://b.example.com/"), "d=2");
        assert_eq!(header(&jar, "http://example.com/"), "d=2");
        assert_eq!(header(&jar, "http://notexample.com/"), "");
    }

    #[test]
    fn a_domain_the_host_is_not_in_is_refused_and_so_is_a_single_label() {
        let jar = jar_with(&[
            ("http://a.example.com/", "x=1; Domain=other.com"),
            ("http://a.example.com/", "y=1; Domain=com"),
            ("http://localhost/", "z=1; Domain=localhost"),
        ]);
        assert_eq!(header(&jar, "http://a.example.com/"), "");
        assert_eq!(header(&jar, "http://localhost/"), "z=1");
    }

    #[test]
    fn paths_match_on_a_segment_and_the_longer_path_goes_first() {
        let jar = jar_with(&[
            ("http://h.test/a/b", "root=1; Path=/"),
            ("http://h.test/a/b", "deep=1; Path=/a"),
            ("http://h.test/a/b", "default=1"),
        ]);
        assert_eq!(
            header(&jar, "http://h.test/a/b"),
            "deep=1; default=1; root=1"
        );
        assert_eq!(header(&jar, "http://h.test/ab"), "root=1");
        // The cookie that named no path got `/a`, the default path of `/a/b` (RFC 6265 5.1.4),
        // which is the path `/a` itself.
        assert_eq!(header(&jar, "http://h.test/a"), "deep=1; default=1; root=1");
    }

    #[test]
    fn secure_cookies_need_a_secure_transport_except_on_this_machine() {
        let mut jar = jar_with(&[
            ("https://h.test/", "s=1; Secure"),
            ("http://h.test/", "t=1; Secure"),
        ]);
        assert_eq!(header(&jar, "https://h.test/"), "s=1");
        assert_eq!(header(&jar, "http://h.test/"), "");
        jar.store(
            &url("http://127.0.0.1:9/"),
            "l=1; Secure",
            Source::Http,
            false,
            NOW,
        );
        assert_eq!(header(&jar, "http://127.0.0.1:9/"), "l=1");
    }

    #[test]
    fn a_script_neither_sees_nor_writes_an_httponly_cookie() {
        let mut jar = jar_with(&[
            ("http://h.test/", "h=1; HttpOnly"),
            ("http://h.test/", "v=1"),
        ]);
        let script = Access {
            scripted: true,
            cross_site: false,
        };
        assert_eq!(jar.header(&url("http://h.test/"), script, NOW), "v=1");
        let attempt = jar.store(&url("http://h.test/"), "h=2", Source::Script, false, NOW);
        assert!(attempt.is_empty());
        assert!(
            jar.store(
                &url("http://h.test/"),
                "n=1; HttpOnly",
                Source::Script,
                false,
                NOW
            )
            .is_empty()
        );
        assert_eq!(header(&jar, "http://h.test/"), "h=1; v=1");
    }

    #[test]
    fn replacing_a_cookie_says_overwrite_then_adds_the_new_one_and_expiry_deletes() {
        let mut jar = jar_with(&[("http://h.test/", "a=1")]);
        let changes = jar.store(&url("http://h.test/"), "a=2", Source::Http, false, NOW);
        let seen: Vec<_> = changes
            .iter()
            .map(|c| (c.cookie.value.as_str(), c.cause, c.removed))
            .collect();
        assert_eq!(
            seen,
            [
                ("1", CookieCause::Overwrite, true),
                ("2", CookieCause::Explicit, false)
            ]
        );
        let gone = jar.store(
            &url("http://h.test/"),
            "a=2; Max-Age=0",
            Source::Http,
            false,
            NOW,
        );
        assert_eq!(gone.len(), 1);
        assert_eq!(
            (gone[0].cause, gone[0].removed),
            (CookieCause::ExpiredOverwrite, true)
        );
        assert_eq!(header(&jar, "http://h.test/"), "");
        // Expired on arrival and nothing to replace: nothing happens.
        assert!(
            jar.store(
                &url("http://h.test/"),
                "b=1; Max-Age=0",
                Source::Http,
                false,
                NOW
            )
            .is_empty()
        );
    }

    #[test]
    fn max_age_wins_over_expires_and_time_takes_cookies_away() {
        let mut jar = jar_with(&[(
            "http://h.test/",
            "a=1; Max-Age=10; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
        )]);
        assert_eq!(header(&jar, "http://h.test/"), "a=1");
        assert_eq!(
            jar.header(&url("http://h.test/"), Access::default(), NOW + 11.),
            ""
        );
        let swept = jar.sweep(NOW + 11.);
        assert_eq!(
            (swept[0].cause, swept[0].removed),
            (CookieCause::Expired, true)
        );
    }

    #[test]
    fn cross_site_requests_carry_only_samesite_none_cookies() {
        let mut jar = jar_with(&[
            ("https://h.test/", "n=1; SameSite=None; Secure"),
            ("https://h.test/", "l=1; SameSite=Lax"),
            ("https://h.test/", "u=1"),
        ]);
        let away = Access {
            scripted: false,
            cross_site: true,
        };
        assert_eq!(jar.header(&url("https://h.test/"), away, NOW), "n=1");
        assert_eq!(header(&jar, "https://h.test/"), "n=1; l=1; u=1");
        // A cross-site response cannot set a Lax cookie, and SameSite=None needs Secure.
        assert!(
            jar.store(
                &url("https://h.test/"),
                "x=1; SameSite=Lax",
                Source::Http,
                true,
                NOW
            )
            .is_empty()
        );
        assert!(
            jar.store(
                &url("https://h.test/"),
                "y=1; SameSite=None",
                Source::Http,
                false,
                NOW
            )
            .is_empty()
        );
    }

    #[test]
    fn the_prefixes_keep_their_promise() {
        let jar = jar_with(&[
            ("http://h.test/", "__Secure-a=1"),
            ("https://h.test/", "__Secure-b=1; Secure"),
            (
                "https://h.test/",
                "__Host-c=1; Secure; Domain=h.test; Path=/",
            ),
            ("https://h.test/", "__Host-d=1; Secure; Path=/"),
        ]);
        assert_eq!(header(&jar, "https://h.test/"), "__Secure-b=1; __Host-d=1");
    }

    #[test]
    fn filters_select_by_url_name_domain_and_kind() {
        let jar = jar_with(&[
            ("http://a.example.com/x/y", "p=1; Path=/x"),
            ("http://a.example.com/", "q=1; Domain=example.com; HttpOnly"),
            ("http://a.example.com/", "r=1; Max-Age=100"),
        ]);
        let names = |filter: Filter| -> Vec<String> {
            jar.query(&filter, NOW)
                .into_iter()
                .map(|c| c.name)
                .collect()
        };
        assert_eq!(names(Filter::default()), ["p", "q", "r"]);
        let at = |a: &str| Filter {
            url: Some(url(a)),
            ..Filter::default()
        };
        assert_eq!(names(at("http://a.example.com/x/y")), ["p", "q", "r"]);
        assert_eq!(names(at("http://a.example.com/")), ["q", "r"]);
        let domain = |d: &str| Filter {
            domain: Some(d.into()),
            ..Filter::default()
        };
        assert_eq!(names(domain("example.com")), ["p", "q", "r"]);
        assert_eq!(names(domain("b.example.com")), Vec::<String>::new());
        let kind = Filter {
            session: Some(false),
            ..Filter::default()
        };
        assert_eq!(names(kind), ["r"]);
        let hidden = Filter {
            http_only: Some(true),
            ..Filter::default()
        };
        assert_eq!(names(hidden), ["q"]);
    }

    #[test]
    fn remove_takes_away_what_the_address_is_sent_under_that_name() {
        let mut jar = jar_with(&[
            ("http://h.test/", "a=1"),
            ("http://h.test/", "a=2; Path=/x"),
            ("http://h.test/", "b=1"),
        ]);
        let gone = jar.remove(&url("http://h.test/x"), "a", NOW);
        assert_eq!(gone.len(), 2);
        assert!(
            gone.iter()
                .all(|c| c.removed && c.cause == CookieCause::Explicit)
        );
        assert_eq!(header(&jar, "http://h.test/x"), "b=1");
        assert!(jar.remove(&url("http://h.test/"), "zzz", NOW).is_empty());
    }

    #[test]
    fn a_cookie_without_a_name_is_sent_as_its_value() {
        let mut jar = Jar::default();
        let cookie = Cookie {
            name: String::new(),
            value: "1".into(),
            domain: "h.test".into(),
            host_only: true,
            path: "/".into(),
            secure: false,
            http_only: false,
            same_site: SameSite::Lax,
            expires: None,
            created: 0,
        };
        jar.insert(cookie, NOW);
        assert_eq!(header(&jar, "http://h.test/"), "1");
    }

    #[test]
    fn the_file_keeps_the_cookies_that_expire_and_not_the_session_ones() {
        let dir = std::env::temp_dir().join(format!("verve-cookies-{}", std::process::id()));
        let file = dir.join("Cookies.json");
        {
            let store = CookieStore::open(Some(file.clone()));
            let at = url("http://h.test/");
            store.store(&at, "keep=1; Max-Age=100000", Source::Http, false);
            store.store(&at, "session=1", Source::Http, false);
            store.flush();
        }
        let again = CookieStore::open(Some(file));
        let (found, _) = again.query(&Filter::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "keep");
        assert_eq!(
            again.header(&url("http://h.test/"), Access::default()),
            "keep=1"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_site_is_the_last_two_labels_or_the_whole_address() {
        assert_eq!(site_of("a.b.example.com"), "example.com");
        assert_eq!(site_of("127.0.0.1"), "127.0.0.1");
        assert_eq!(site_of("localhost"), "localhost");
        assert!(cross_site("http://a.test/", &url("http://b.test/")));
        assert!(!cross_site("http://x.a.test/", &url("http://y.a.test/")));
        assert!(!cross_site("", &url("http://b.test/")));
    }
}
