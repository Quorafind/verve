//! Node's `url` module: the WHATWG `URL` and `URLSearchParams` (the globals), the file-URL
//! conversions, and the legacy API older npm code still calls — `url.parse`, `url.format`,
//! `url.resolve` and the `Url` objects they answer.
//!
//! `url.parse` follows Node's own `lib/url.js`: it never throws, it keeps what the WHATWG parser
//! would rewrite, and its result has Node's fields (`protocol`, `slashes`, `auth`, `host`,
//! `port`, `hostname`, `hash`, `search`, `query`, `pathname`, `path`, `href`), `null` where a
//! part is absent. `url.resolve` resolves with the WHATWG rules, which agree with Node's for
//! everything but malformed input.

use boa_engine::{
    Context, JsObject, JsResult, JsString, JsValue, js_string,
    native_function::NativeFunctionPointer,
};

use crate::native::{
    ClassSpec, arg, class, coded_error, exports, get, invoke, js_text, object, text_of,
};

/// The protocols whose URLs have `//` and a host: `http://x/` is written so even when the
/// object being formatted forgot its `slashes`.
fn slashed(protocol: &str) -> bool {
    matches!(
        protocol.trim_end_matches(':').to_ascii_lowercase().as_str(),
        "http" | "https" | "ftp" | "gopher" | "file" | "ws" | "wss"
    )
}

fn hostless(protocol: &str) -> bool {
    matches!(
        protocol.trim_end_matches(':').to_ascii_lowercase().as_str(),
        "javascript"
    )
}

/// The parts of a URL as `url.parse` finds them.
#[derive(Default, Debug, Clone, PartialEq)]
pub(crate) struct Legacy {
    pub protocol: Option<String>,
    pub slashes: bool,
    pub auth: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub hostname: Option<String>,
    pub hash: Option<String>,
    pub search: Option<String>,
    pub query: Option<String>,
    pub pathname: Option<String>,
    pub path: Option<String>,
}

/// The characters Node escapes in what is left of a URL after its host: spaces, quotes, angle
/// brackets, braces and the rest that cannot appear in one.
fn auto_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str("%09"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            ' ' => out.push_str("%20"),
            '"' => out.push_str("%22"),
            '\'' => out.push_str("%27"),
            '<' => out.push_str("%3C"),
            '>' => out.push_str("%3E"),
            '\\' => out.push_str("%5C"),
            '^' => out.push_str("%5E"),
            '`' => out.push_str("%60"),
            '{' => out.push_str("%7B"),
            '|' => out.push_str("%7C"),
            '}' => out.push_str("%7D"),
            other => out.push(other),
        }
    }
    out
}

fn decode_component(text: &str) -> String {
    String::from_utf8_lossy(&crate::querystring::unescape_bytes(text, false)).into_owned()
}

/// `url.parse(text, …, slashesDenoteHost)`.
pub(crate) fn parse_legacy(input: &str, slashes_denote_host: bool) -> Legacy {
    let mut url = Legacy::default();
    let mut rest = input.trim().to_owned();
    // Back slashes before the query are forward ones, as browsers read them.
    let query_at = rest.find(['?', '#']).unwrap_or(rest.len());
    rest = format!(
        "{}{}",
        rest[..query_at].replace('\\', "/"),
        &rest[query_at..]
    );

    // A simple path: no protocol, no host.
    if !rest.starts_with("//") || slashes_denote_host {
        let simple = rest.starts_with('/')
            && !rest.starts_with("//")
            && !rest.contains(['#', ' ', '\t', '\n', '\r']);
        if simple && !slashes_denote_host {
            let (path, search) = match rest.find('?') {
                Some(at) => (rest[..at].to_owned(), Some(rest[at..].to_owned())),
                None => (rest.clone(), None),
            };
            url.path = Some(rest.clone());
            url.pathname = Some(path);
            if let Some(search) = search {
                url.query = Some(search[1..].to_owned());
                url.search = Some(search);
            }
            return url;
        }
    }

    let proto_len = rest
        .find(':')
        .filter(|&at| {
            at > 0
                && rest[..at]
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c))
        })
        .map(|at| at + 1);
    let mut lower_proto = String::new();
    if let Some(len) = proto_len {
        let proto = &rest[..len];
        lower_proto = proto.to_ascii_lowercase();
        url.protocol = Some(lower_proto.clone());
        rest = rest[len..].to_owned();
    }

    let mut slashes = false;
    let has_auth_at_start = rest.starts_with("//") && {
        let after = &rest[2..];
        let end = after.find('/').unwrap_or(after.len());
        after[..end].contains('@')
    };
    if slashes_denote_host || proto_len.is_some() || has_auth_at_start {
        slashes = rest.starts_with("//");
        if slashes && !(proto_len.is_some() && hostless(&lower_proto)) {
            rest = rest[2..].to_owned();
            url.slashes = true;
        }
    }

    if !hostless(&lower_proto) && (slashes || (proto_len.is_some() && !slashed(&lower_proto))) {
        // The host ends at the first of these; an `@` before it ends the credentials.
        let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let at_sign = rest[..host_end].rfind('@');
        if let Some(at) = at_sign {
            url.auth = Some(decode_component(&rest[..at]));
            rest = rest[at + 1..].to_owned();
        }
        let host_end = rest
            .find([
                '/', '?', '#', ' ', '\t', '\r', '\n', '<', '>', '"', '`', '{', '}', '|', '\\', '^',
                '\'',
            ])
            .unwrap_or(rest.len());
        let host = rest[..host_end].to_owned();
        rest = rest[host_end..].to_owned();
        // `host:port`.
        let (mut hostname, port) = match host.rfind(':') {
            Some(at)
                if host[at + 1..].bytes().all(|c| c.is_ascii_digit())
                    && !host[at..].contains(']') =>
            {
                (
                    host[..at].to_owned(),
                    Some(host[at + 1..].to_owned()).filter(|p| !p.is_empty()),
                )
            }
            _ => (host.clone(), None),
        };
        let ipv6 = hostname.starts_with('[') && hostname.ends_with(']');
        if !ipv6 {
            hostname = hostname.to_lowercase();
            if !hostname.is_ascii() {
                hostname = crate::querystring::to_ascii(&hostname).unwrap_or(hostname);
            }
        }
        let host = match &port {
            Some(port) => format!("{hostname}:{port}"),
            None => hostname.clone(),
        };
        if ipv6 {
            hostname = hostname[1..hostname.len() - 1].to_owned();
            if !rest.starts_with('/') {
                rest = format!("/{rest}");
            }
        }
        url.port = port;
        url.hostname = Some(hostname);
        url.host = Some(host);
    }

    if !hostless(&lower_proto) {
        rest = auto_escape(&rest);
    }
    if let Some(at) = rest.find('#') {
        url.hash = Some(rest[at..].to_owned());
        rest.truncate(at);
    }
    if let Some(at) = rest.find('?') {
        url.search = Some(rest[at..].to_owned());
        url.query = Some(rest[at + 1..].to_owned());
        rest.truncate(at);
    }
    if !rest.is_empty() {
        url.pathname = Some(rest);
    }
    if slashed(&lower_proto)
        && url.hostname.as_ref().is_some_and(|h| !h.is_empty())
        && url.pathname.is_none()
    {
        url.pathname = Some("/".into());
    }
    if url.pathname.is_some() || url.search.is_some() {
        url.path = Some(format!(
            "{}{}",
            url.pathname.as_deref().unwrap_or(""),
            url.search.as_deref().unwrap_or("")
        ));
    }
    url
}

/// `url.format` of the parts: what Node writes for a `Url` or a plain object.
#[allow(clippy::too_many_arguments)]
fn format_parts(
    protocol: &str,
    slashes: bool,
    auth: &str,
    host: Option<&str>,
    hostname: &str,
    port: &str,
    pathname: &str,
    search: &str,
    hash: &str,
) -> String {
    let auth = if auth.is_empty() {
        String::new()
    } else {
        format!("{}@", crate::querystring::escape(auth).replace("%3A", ":"))
    };
    let mut host = match host {
        Some(host) if !host.is_empty() => format!("{auth}{host}"),
        _ if !hostname.is_empty() => {
            let name = if hostname.contains(':') && !hostname.starts_with('[') {
                format!("[{hostname}]")
            } else {
                hostname.to_owned()
            };
            let port = if port.is_empty() {
                String::new()
            } else {
                format!(":{port}")
            };
            format!("{auth}{name}{port}")
        }
        _ => String::new(),
    };
    let mut protocol = protocol.to_owned();
    if !protocol.is_empty() && !protocol.ends_with(':') {
        protocol.push(':');
    }
    let mut pathname = pathname.replace('#', "%23").replace('?', "%3F");
    if slashes || protocol.is_empty() || slashed(&protocol) {
        if slashes || !host.is_empty() {
            if !pathname.is_empty() && !pathname.starts_with('/') {
                pathname = format!("/{pathname}");
            }
            host = format!("//{host}");
        } else if protocol == "file:" {
            host = "//".into();
        }
    }
    let search = search.replace('#', "%23");
    let search = if !search.is_empty() && !search.starts_with('?') {
        format!("?{search}")
    } else {
        search
    };
    let hash = if !hash.is_empty() && !hash.starts_with('#') {
        format!("#{hash}")
    } else {
        hash.to_owned()
    };
    format!("{protocol}{host}{pathname}{search}{hash}")
}

fn legacy_href(url: &Legacy) -> String {
    format_parts(
        url.protocol.as_deref().unwrap_or(""),
        url.slashes,
        url.auth.as_deref().unwrap_or(""),
        url.host.as_deref(),
        url.hostname.as_deref().unwrap_or(""),
        url.port.as_deref().unwrap_or(""),
        url.pathname.as_deref().unwrap_or(""),
        url.search.as_deref().unwrap_or(""),
        url.hash.as_deref().unwrap_or(""),
    )
}

fn url_prototype(context: &mut Context) -> JsResult<JsObject> {
    let module = crate::modules::node_module(context, "url")?;
    let class = get(&module, "Url", context)?;
    get(&class, "prototype", context)?
        .as_object()
        .ok_or_else(|| crate::native::type_error("url.Url is not a class"))
}

fn optional(text: &Option<String>) -> JsValue {
    text.as_deref().map_or(JsValue::null(), js_text)
}

/// A `Url` object of `url`'s parts; `query` parsed when `parse_query` says so.
fn legacy_object(url: &Legacy, parse_query: bool, context: &mut Context) -> JsResult<JsValue> {
    let out = object(context);
    out.set_prototype(Some(url_prototype(context)?));
    fill_legacy(&out, url, parse_query, context)?;
    Ok(out.into())
}

fn fill_legacy(
    out: &JsObject,
    url: &Legacy,
    parse_query: bool,
    context: &mut Context,
) -> JsResult<()> {
    let query = if parse_query {
        let querystring = crate::modules::node_module(context, "querystring")?;
        invoke(
            &querystring,
            "parse",
            &[url.query.as_deref().map_or(js_text(""), js_text)],
            context,
        )?
    } else {
        optional(&url.query)
    };
    let search = if parse_query && url.search.is_none() {
        js_text("")
    } else {
        optional(&url.search)
    };
    let fields: [(&str, JsValue); 12] = [
        ("protocol", optional(&url.protocol)),
        (
            "slashes",
            if url.slashes {
                true.into()
            } else {
                JsValue::null()
            },
        ),
        ("auth", optional(&url.auth)),
        ("host", optional(&url.host)),
        ("port", optional(&url.port)),
        ("hostname", optional(&url.hostname)),
        ("hash", optional(&url.hash)),
        ("search", search),
        ("query", query),
        ("pathname", optional(&url.pathname)),
        ("path", optional(&url.path)),
        ("href", js_text(&legacy_href(url))),
    ];
    for (key, value) in fields {
        out.set(JsString::from(key), value, false, context)?;
    }
    Ok(())
}

/// A field of an object as text, empty when absent.
fn field(value: &JsValue, name: &str, context: &mut Context) -> JsResult<String> {
    let v = get(value, name, context)?;
    if v.is_null_or_undefined() {
        Ok(String::new())
    } else {
        text_of(&v, context)
    }
}

fn is_whatwg(value: &JsValue, context: &mut Context) -> JsResult<bool> {
    let class = crate::native::global(context, "URL")?;
    if !value.is_object() || !class.is_object() {
        return Ok(false);
    }
    value.instance_of(&class, context)
}

/// `url.format(urlObject | URL | string, options)`.
fn format(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let input = arg(args, 0);
    if input.is_string() {
        let url = parse_legacy(&text_of(&input, context)?, false);
        return Ok(js_text(&legacy_href(&url)));
    }
    if !input.is_object() {
        return Err(coded_error(
            context,
            &format!(
                "The \"urlObject\" argument must be one of type object or string. Received {}",
                input.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    if is_whatwg(&input, context)? {
        // The WHATWG URL, with `auth`, `fragment`, `search` and `unicode` choosing what is kept.
        let options = arg(args, 1);
        let flag = |name: &str, default: bool, cx: &mut Context| -> JsResult<bool> {
            let v = get(&options, name, cx)?;
            Ok(if v.is_undefined() {
                default
            } else {
                v.to_boolean()
            })
        };
        let (auth, fragment, search, unicode) = (
            flag("auth", true, context)?,
            flag("fragment", true, context)?,
            flag("search", true, context)?,
            flag("unicode", false, context)?,
        );
        let protocol = field(&input, "protocol", context)?;
        let username = field(&input, "username", context)?;
        let password = field(&input, "password", context)?;
        let mut hostname = field(&input, "hostname", context)?;
        if unicode {
            hostname = url::quirks::domain_to_unicode(&hostname);
        }
        let port = field(&input, "port", context)?;
        let mut out = protocol.clone();
        let href = field(&input, "href", context)?;
        if href[protocol.len()..].starts_with("//") {
            out.push_str("//");
        }
        if auth && (!username.is_empty() || !password.is_empty()) {
            out.push_str(&username);
            if !password.is_empty() {
                out.push(':');
                out.push_str(&password);
            }
            out.push('@');
        }
        out.push_str(&hostname);
        if !port.is_empty() {
            out.push(':');
            out.push_str(&port);
        }
        out.push_str(&field(&input, "pathname", context)?);
        if search {
            out.push_str(&field(&input, "search", context)?);
        }
        if fragment {
            out.push_str(&field(&input, "hash", context)?);
        }
        return Ok(js_text(&out));
    }
    let query = get(&input, "query", context)?;
    let mut search = field(&input, "search", context)?;
    if search.is_empty() && query.is_object() {
        let querystring = crate::modules::node_module(context, "querystring")?;
        let text = invoke(&querystring, "stringify", &[query], context)?;
        let text = text_of(&text, context)?;
        if !text.is_empty() {
            search = format!("?{text}");
        }
    }
    let host = get(&input, "host", context)?;
    let host = if host.is_null_or_undefined() {
        None
    } else {
        Some(text_of(&host, context)?)
    };
    let slashes = get(&input, "slashes", context)?.to_boolean();
    Ok(js_text(&format_parts(
        &field(&input, "protocol", context)?,
        slashes,
        &field(&input, "auth", context)?,
        host.as_deref(),
        &field(&input, "hostname", context)?,
        &field(&input, "port", context)?,
        &field(&input, "pathname", context)?,
        &search,
        &field(&input, "hash", context)?,
    )))
}

/// `url.resolve(from, to)`: `to` read against `from`. A `from` with no scheme (`/one/two`) is
/// resolved against a stand-in origin, which is then taken off again.
pub(crate) fn resolve(from: &str, to: &str) -> String {
    if let Ok(base) = url::Url::parse(from) {
        return base
            .join(to)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| to.to_owned());
    }
    const ORIGIN: &str = "http://resolve.invalid";
    let relative_base = !from.starts_with('/');
    let protocol_relative = from.starts_with("//");
    let base = if protocol_relative {
        format!("http:{from}")
    } else {
        format!("{ORIGIN}/{}", from.trim_start_matches('/'))
    };
    let Ok(base) = url::Url::parse(&base) else {
        return to.to_owned();
    };
    let Ok(joined) = base.join(to) else {
        return to.to_owned();
    };
    let text = joined.to_string();
    if protocol_relative {
        return text.strip_prefix("http:").unwrap_or(&text).to_owned();
    }
    match text.strip_prefix(ORIGIN) {
        Some(path) if relative_base => path.strip_prefix('/').unwrap_or(path).to_owned(),
        Some(path) => path.to_owned(),
        None => text,
    }
}

// ─── File URLs ──────────────────────────────────────────────────────────────────────────────

/// What went wrong turning a file URL into a path: Node's code, and its message.
pub(crate) struct FileUrlError(pub &'static str, pub String);

/// `fileURLToPath`: a `file:` URL as a path of this system (or of Windows, `windows`) —
/// percent escapes decoded, a Windows drive found, a host made a UNC share.
pub(crate) fn file_url_to_path(href: &str, windows: bool) -> Result<String, FileUrlError> {
    let url = url::Url::parse(href)
        .map_err(|_| FileUrlError("ERR_INVALID_URL", format!("Invalid URL: {href}")))?;
    if url.scheme() != "file" {
        return Err(FileUrlError(
            "ERR_INVALID_URL_SCHEME",
            "The URL must be of scheme file".into(),
        ));
    }
    let hostname = url.host_str().unwrap_or("").to_owned();
    let pathname = url.path();
    let lower = pathname.to_ascii_lowercase();
    if windows {
        if lower.contains("%2f") || lower.contains("%5c") {
            return Err(FileUrlError(
                "ERR_INVALID_FILE_URL_PATH",
                "File URL path must not include encoded \\ or / characters".into(),
            ));
        }
        let decoded = decode_component(&pathname.replace('/', "\\"));
        if !hostname.is_empty() {
            return Ok(format!(
                "\\\\{}{decoded}",
                url::quirks::domain_to_unicode(&hostname)
            ));
        }
        let b = decoded.as_bytes();
        if b.len() < 3 || !b[1].is_ascii_alphabetic() || b[2] != b':' {
            return Err(FileUrlError(
                "ERR_INVALID_FILE_URL_PATH",
                "File URL path must be absolute".into(),
            ));
        }
        return Ok(decoded[1..].to_owned());
    }
    if !hostname.is_empty() {
        return Err(FileUrlError(
            "ERR_INVALID_FILE_URL_HOST",
            format!(
                "File URL host must be \"localhost\" or empty on {}",
                crate::node::platform()
            ),
        ));
    }
    if lower.contains("%2f") {
        return Err(FileUrlError(
            "ERR_INVALID_FILE_URL_PATH",
            "File URL path must not include encoded / characters".into(),
        ));
    }
    Ok(decode_component(pathname))
}

/// The `href` of a value that may be a URL object or a string.
fn href_of(value: &JsValue, context: &mut Context) -> JsResult<String> {
    if value.is_object() && !get(value, "href", context)?.is_undefined() {
        return field(value, "href", context);
    }
    if !value.is_string() {
        return Err(coded_error(
            context,
            &format!(
                "The \"path\" argument must be of type string or an instance of URL. Received {}",
                value.type_of()
            ),
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    text_of(value, context)
}

/// The path a `fs` function was given as a `file:` URL (an object with an `href`, or a string).
pub(crate) fn path_from_url(value: &JsValue, context: &mut Context) -> JsResult<String> {
    let href = href_of(value, context)?;
    file_url_to_path(&href, cfg!(windows))
        .map_err(|FileUrlError(code, message)| coded_error(context, &message, code))
}

fn windows_option(options: &JsValue, context: &mut Context) -> JsResult<bool> {
    let v = get(options, "windows", context)?;
    Ok(if v.is_undefined() {
        cfg!(windows)
    } else {
        v.to_boolean()
    })
}

/// `pathToFileURL`: an absolute path (resolved against the working folder first) as a `file:`
/// URL, with what a path may hold but a URL may not escaped.
fn path_to_file_url(path: &str, windows: bool, context: &mut Context) -> JsResult<JsValue> {
    let href = if windows {
        let resolved = crate::path::win32_resolve(
            &[path.to_owned()],
            &crate::path::cwd(context, crate::path::WIN32),
        );
        let slashed = resolved.replace('\\', "/");
        let keep_trailing =
            (path.ends_with('\\') || path.ends_with('/')) && !slashed.ends_with('/');
        let slashed = if keep_trailing {
            format!("{slashed}/")
        } else {
            slashed
        };
        let encoded = encode_path(&slashed, false);
        match encoded.strip_prefix("//") {
            Some(unc) => format!("file://{unc}"),
            None => format!("file:///{encoded}"),
        }
    } else {
        let resolved = crate::path::posix_resolve(
            &[path.to_owned()],
            &crate::path::cwd(context, crate::path::POSIX),
        );
        let keep_trailing = path.ends_with('/') && !resolved.ends_with('/');
        let resolved = if keep_trailing {
            format!("{resolved}/")
        } else {
            resolved
        };
        format!("file://{}", encode_path(&resolved, true))
    };
    Ok(crate::native::construct_global(context, "URL", &[js_text(&href)])?.into())
}

/// Escapes what the URL parser would otherwise read as something else: `%`, `?`, `#`, line
/// breaks, tabs — and, in a POSIX path, `\`, which is a character of a name there.
fn encode_path(path: &str, posix: bool) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\\' if posix => out.push_str("%5C"),
            '\n' => out.push_str("%0A"),
            '\r' => out.push_str("%0D"),
            '\t' => out.push_str("%09"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            other => out.push(other),
        }
    }
    out
}

const URL_FUNCTIONS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("parse", 3, |_, args, cx| {
        let input = arg(args, 0);
        if !input.is_string() {
            return Err(coded_error(
                cx,
                &format!(
                    "The \"url\" argument must be of type string. Received {}",
                    input.type_of()
                ),
                "ERR_INVALID_ARG_TYPE",
            ));
        }
        let text = text_of(&input, cx)?;
        let url = parse_legacy(&text, arg(args, 2).to_boolean());
        legacy_object(&url, arg(args, 1).to_boolean(), cx)
    }),
    ("format", 2, format),
    ("resolve", 2, |_, args, cx| {
        let from = text_of(&arg(args, 0), cx)?;
        let to = text_of(&arg(args, 1), cx)?;
        Ok(js_text(&resolve(&from, &to)))
    }),
    ("resolveObject", 2, |_, args, cx| {
        let from = text_of(&arg(args, 0), cx)?;
        let to = text_of(&arg(args, 1), cx)?;
        legacy_object(&parse_legacy(&resolve(&from, &to), false), false, cx)
    }),
    ("domainToASCII", 1, |_, args, cx| {
        Ok(js_text(&url::quirks::domain_to_ascii(&text_of(
            &arg(args, 0),
            cx,
        )?)))
    }),
    ("domainToUnicode", 1, |_, args, cx| {
        Ok(js_text(&url::quirks::domain_to_unicode(&text_of(
            &arg(args, 0),
            cx,
        )?)))
    }),
    ("fileURLToPath", 2, |_, args, cx| {
        let href = href_of(&arg(args, 0), cx)?;
        let windows = windows_option(&arg(args, 1), cx)?;
        file_url_to_path(&href, windows)
            .map(|p| js_text(&p))
            .map_err(|FileUrlError(code, message)| coded_error(cx, &message, code))
    }),
    ("pathToFileURL", 2, |_, args, cx| {
        let path = text_of(&arg(args, 0), cx)?;
        let windows = windows_option(&arg(args, 1), cx)?;
        path_to_file_url(&path, windows, cx)
    }),
    ("urlToHttpOptions", 1, |_, args, cx| {
        let url = arg(args, 0);
        let out = object(cx);
        let hostname = field(&url, "hostname", cx)?;
        let hostname = hostname
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .map(str::to_owned)
            .unwrap_or(hostname);
        let pathname = field(&url, "pathname", cx)?;
        let search = field(&url, "search", cx)?;
        for (key, value) in [
            ("protocol", field(&url, "protocol", cx)?),
            ("hostname", hostname),
            ("hash", field(&url, "hash", cx)?),
            ("search", search.clone()),
            ("pathname", pathname.clone()),
            ("path", format!("{pathname}{search}")),
            ("href", field(&url, "href", cx)?),
        ] {
            out.set(JsString::from(key), js_text(&value), false, cx)?;
        }
        let port = field(&url, "port", cx)?;
        if !port.is_empty() {
            out.set(
                js_string!("port"),
                port.parse::<f64>().unwrap_or(0.),
                false,
                cx,
            )?;
        }
        let username = field(&url, "username", cx)?;
        let password = field(&url, "password", cx)?;
        if !username.is_empty() || !password.is_empty() {
            out.set(
                js_string!("auth"),
                js_text(&format!(
                    "{}:{}",
                    decode_component(&username),
                    decode_component(&password)
                )),
                false,
                cx,
            )?;
        }
        Ok(out.into())
    }),
];

pub(crate) fn module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let module = object(context);
    for name in ["URL", "URLSearchParams"] {
        let class = crate::native::global(context, name)?;
        module.set(JsString::from(name), class, false, context)?;
    }
    exports(context, &module, URL_FUNCTIONS)?;
    let (url_class, _) = class(
        context,
        ClassSpec {
            name: "Url",
            init: |this, _, cx| fill_legacy(this, &Legacy::default(), false, cx),
            methods: &[
                ("parse", 3, |this, args, cx| {
                    let text = text_of(&arg(args, 0), cx)?;
                    let url = parse_legacy(&text, arg(args, 2).to_boolean());
                    if let Some(this) = this.as_object() {
                        fill_legacy(&this, &url, arg(args, 1).to_boolean(), cx)?;
                    }
                    Ok(this.clone())
                }),
                ("format", 0, |this, _, cx| {
                    format(&JsValue::undefined(), std::slice::from_ref(this), cx)
                }),
                ("resolve", 1, |this, args, cx| {
                    let from = field(this, "href", cx)?;
                    let to = text_of(&arg(args, 0), cx)?;
                    Ok(js_text(&resolve(&from, &to)))
                }),
                ("resolveObject", 1, |this, args, cx| {
                    let from = field(this, "href", cx)?;
                    let to = text_of(&arg(args, 0), cx)?;
                    legacy_object(&parse_legacy(&resolve(&from, &to), false), false, cx)
                }),
            ],
            ..ClassSpec::default()
        },
    )?;
    module.set(js_string!("Url"), url_class, false, context)?;
    Ok(module.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_as_node() {
        let u = parse_legacy(
            "https://user:pass@sub.Example.com:8080/p/a/t/h?query=string#hash",
            false,
        );
        assert_eq!(u.protocol.as_deref(), Some("https:"));
        assert!(u.slashes);
        assert_eq!(u.auth.as_deref(), Some("user:pass"));
        assert_eq!(u.host.as_deref(), Some("sub.example.com:8080"));
        assert_eq!(u.port.as_deref(), Some("8080"));
        assert_eq!(u.hostname.as_deref(), Some("sub.example.com"));
        assert_eq!(u.hash.as_deref(), Some("#hash"));
        assert_eq!(u.search.as_deref(), Some("?query=string"));
        assert_eq!(u.pathname.as_deref(), Some("/p/a/t/h"));
        assert_eq!(u.path.as_deref(), Some("/p/a/t/h?query=string"));
        assert_eq!(
            legacy_href(&u),
            "https://user:pass@sub.example.com:8080/p/a/t/h?query=string#hash"
        );
        let u = parse_legacy("/a/b?x=1", false);
        assert_eq!(
            (u.pathname.as_deref(), u.query.as_deref(), u.host.as_deref()),
            (Some("/a/b"), Some("x=1"), None)
        );
        let u = parse_legacy("mailto:someone@example.com", false);
        assert_eq!(
            (
                u.protocol.as_deref(),
                u.auth.as_deref(),
                u.hostname.as_deref()
            ),
            (Some("mailto:"), Some("someone"), Some("example.com"))
        );
        let u = parse_legacy("//host/x", true);
        assert_eq!(
            (u.host.as_deref(), u.pathname.as_deref()),
            (Some("host"), Some("/x"))
        );
        let u = parse_legacy("http://[::1]:3000/x", false);
        assert_eq!(
            (u.hostname.as_deref(), u.port.as_deref()),
            (Some("::1"), Some("3000"))
        );
    }

    #[test]
    fn resolves_as_node() {
        assert_eq!(resolve("/one/two/three", "four"), "/one/two/four");
        assert_eq!(
            resolve("http://example.com/", "/one"),
            "http://example.com/one"
        );
        assert_eq!(
            resolve("http://example.com/one", "/two"),
            "http://example.com/two"
        );
        assert_eq!(resolve("one/two", "three"), "one/three");
    }

    #[test]
    fn file_urls() {
        assert_eq!(
            file_url_to_path("file:///C:/path/a%20b.md", true)
                .ok()
                .unwrap(),
            "C:\\path\\a b.md"
        );
        assert_eq!(
            file_url_to_path("file://nas/share/x", true).ok().unwrap(),
            "\\\\nas\\share\\x"
        );
        assert_eq!(
            file_url_to_path("file:///tmp/a%20b", false).ok().unwrap(),
            "/tmp/a b"
        );
        assert_eq!(
            file_url_to_path("http://x/", false).err().unwrap().0,
            "ERR_INVALID_URL_SCHEME"
        );
        assert_eq!(
            file_url_to_path("file:///C:/x%2Fy", true).err().unwrap().0,
            "ERR_INVALID_FILE_URL_PATH"
        );
    }
}
