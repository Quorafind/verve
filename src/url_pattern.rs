//! `URLPattern`, as the WHATWG URL Pattern standard has it (and Chromium, and Node): a pattern for
//! each part of a URL — `/:type(users|groups){/local}?/:id` — matched against URLs with `test`
//! and `exec`.
//!
//! Each part's pattern is read as the standard reads it: fixed text, named groups (`:name`, with
//! a regular expression of their own in parentheses or the part's default — anything but `/` in
//! a pathname, anything but `.` in a hostname), unnamed regular expressions, `*`, and `{…}`
//! groups, any of them made optional or repeated with `?`, `*` or `+`; a `/` before a group in a
//! pathname belongs to it, so `/:id?` matches with or without it. The pattern becomes a regular
//! expression, run by the engine's own `RegExp`.
//!
//! Not here: the standard's canonicalisation of fixed text (a pattern's hostname is compared as
//! written, lowercased only when `ignoreCase` is set), and the full constructor string parser —
//! a pattern string is split into its parts the way a URL is (`https://host:port/path?query#hash`),
//! or read as a pathname against `baseURL`.

use boa_engine::{
    Context, Finalize, JsData, JsObject, JsResult, JsString, JsValue, Trace, js_string,
    native_function::NativeFunctionPointer,
};

use crate::native::{arg, get, invoke, js_text, object, text_of, type_error};

/// The parts of a URL, in the order the standard lists them.
const COMPONENTS: [&str; 8] = [
    "protocol", "username", "password", "hostname", "port", "pathname", "search", "hash",
];

/// One part of a pattern, as the standard's parser makes it.
#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Fixed,
    /// A named or unnamed group with a regular expression of its own.
    Regex(String),
    /// A group matching the part's default: one segment.
    Segment,
    /// `*`: anything.
    Wildcard,
}

#[derive(Debug, Clone)]
struct Part {
    kind: Kind,
    /// The fixed text, for [`Kind::Fixed`].
    value: String,
    name: String,
    prefix: String,
    suffix: String,
    modifier: Option<char>,
}

/// Reads a pattern for one part into its parts; `prefix` is the character that belongs to a
/// group it comes right before (`/` in a pathname), `segment` the default regular expression.
fn parse(pattern: &str, prefix_char: Option<char>) -> Result<Vec<Part>, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut parts: Vec<Part> = Vec::new();
    let mut pending = String::new();
    let mut at = 0;
    let mut unnamed = 0usize;

    fn flush(parts: &mut Vec<Part>, pending: &mut String) {
        if !pending.is_empty() {
            parts.push(Part {
                kind: Kind::Fixed,
                value: std::mem::take(pending),
                name: String::new(),
                prefix: String::new(),
                suffix: String::new(),
                modifier: None,
            });
        }
    }

    // A name after `:`, a regular expression in parentheses, or `*`, starting at `at`.
    fn group(
        chars: &[char],
        at: &mut usize,
        unnamed: &mut usize,
    ) -> Result<Option<(Kind, String)>, String> {
        let mut name = String::new();
        if chars.get(*at) == Some(&':') {
            *at += 1;
            while let Some(&c) = chars.get(*at) {
                if c.is_alphanumeric() || c == '_' || c == '$' {
                    name.push(c);
                    *at += 1;
                } else {
                    break;
                }
            }
            if name.is_empty() {
                return Err("a ':' must be followed by a name".into());
            }
        }
        if chars.get(*at) == Some(&'(') {
            let mut depth = 0;
            let mut regex = String::new();
            loop {
                let Some(&c) = chars.get(*at) else {
                    return Err("a regular expression group is not closed".into());
                };
                *at += 1;
                match c {
                    '\\' => {
                        regex.push(c);
                        if let Some(&next) = chars.get(*at) {
                            regex.push(next);
                            *at += 1;
                        }
                        continue;
                    }
                    '(' => {
                        depth += 1;
                        if depth == 1 {
                            continue;
                        }
                    }
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                regex.push(c);
            }
            if name.is_empty() {
                name = unnamed.to_string();
                *unnamed += 1;
            }
            return Ok(Some((Kind::Regex(regex), name)));
        }
        if !name.is_empty() {
            return Ok(Some((Kind::Segment, name)));
        }
        if chars.get(*at) == Some(&'*') {
            *at += 1;
            let name = unnamed.to_string();
            *unnamed += 1;
            return Ok(Some((Kind::Wildcard, name)));
        }
        Ok(None)
    }

    fn modifier(chars: &[char], at: &mut usize) -> Option<char> {
        match chars.get(*at) {
            Some(&c @ ('?' | '*' | '+')) => {
                *at += 1;
                Some(c)
            }
            _ => None,
        }
    }

    while at < chars.len() {
        let c = chars[at];
        if c == '\\' {
            if let Some(&next) = chars.get(at + 1) {
                pending.push(next);
            }
            at += 2;
            continue;
        }
        if c == '{' {
            flush(&mut parts, &mut pending);
            at += 1;
            let mut prefix = String::new();
            while let Some(&c) = chars.get(at) {
                if matches!(c, ':' | '(' | '*' | '}') {
                    break;
                }
                if c == '\\' {
                    at += 1;
                }
                if let Some(&c) = chars.get(at) {
                    prefix.push(c);
                }
                at += 1;
            }
            let found = group(&chars, &mut at, &mut unnamed)?;
            let mut suffix = String::new();
            while let Some(&c) = chars.get(at) {
                if c == '}' {
                    break;
                }
                if c == '\\' {
                    at += 1;
                }
                if let Some(&c) = chars.get(at) {
                    suffix.push(c);
                }
                at += 1;
            }
            if chars.get(at) != Some(&'}') {
                return Err("a '{' group is not closed".into());
            }
            at += 1;
            let modifier = modifier(&chars, &mut at);
            parts.push(match found {
                Some((kind, name)) => Part {
                    kind,
                    value: String::new(),
                    name,
                    prefix,
                    suffix,
                    modifier,
                },
                None => Part {
                    kind: Kind::Fixed,
                    value: prefix + &suffix,
                    name: String::new(),
                    prefix: String::new(),
                    suffix: String::new(),
                    modifier,
                },
            });
            continue;
        }
        if matches!(c, ':' | '(' | '*') {
            let found = group(&chars, &mut at, &mut unnamed)?;
            let Some((kind, name)) = found else {
                pending.push(c);
                at += 1;
                continue;
            };
            // The character before that belongs to the group: `/` in `/:id?`.
            let mut prefix = String::new();
            if let Some(p) = prefix_char
                && pending.ends_with(p)
            {
                pending.pop();
                prefix.push(p);
            }
            flush(&mut parts, &mut pending);
            let modifier = modifier(&chars, &mut at);
            parts.push(Part {
                kind,
                value: String::new(),
                name,
                prefix,
                suffix: String::new(),
                modifier,
            });
            continue;
        }
        pending.push(c);
        at += 1;
    }
    flush(&mut parts, &mut pending);
    Ok(parts)
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\^$.*+?()[]{}|/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The regular expression a part's parts make, and the names of its groups in order.
fn regex_of(parts: &[Part], segment: &str) -> (String, Vec<String>, bool) {
    let mut out = String::from("^");
    let mut names = Vec::new();
    let mut custom = false;
    for part in parts {
        let modifier = part.modifier.map(String::from).unwrap_or_default();
        if part.kind == Kind::Fixed {
            if modifier.is_empty() {
                out.push_str(&escape(&part.value));
            } else {
                out.push_str(&format!("(?:{}){modifier}", escape(&part.value)));
            }
            continue;
        }
        let inner = match &part.kind {
            Kind::Regex(regex) => {
                custom = true;
                regex.clone()
            }
            Kind::Segment => segment.to_owned(),
            Kind::Wildcard => ".*".to_owned(),
            Kind::Fixed => unreachable!(),
        };
        names.push(part.name.clone());
        let (prefix, suffix) = (escape(&part.prefix), escape(&part.suffix));
        let repeated = matches!(part.modifier, Some('*' | '+'));
        if prefix.is_empty() && suffix.is_empty() {
            if repeated {
                let optional = if part.modifier == Some('*') { "?" } else { "" };
                out.push_str(&format!("((?:{inner})+){optional}"));
            } else {
                out.push_str(&format!("({inner}){modifier}"));
            }
        } else if repeated {
            let optional = if part.modifier == Some('*') { "?" } else { "" };
            out.push_str(&format!(
                "(?:{prefix}((?:{inner})(?:{suffix}{prefix}(?:{inner}))*){suffix}){optional}"
            ));
        } else {
            out.push_str(&format!("(?:{prefix}({inner}){suffix}){modifier}"));
        }
    }
    out.push('$');
    (out, names, custom)
}

/// A pattern made: for each part, its pattern as given, its regular expression and the names of
/// its groups.
#[derive(Trace, Finalize, JsData)]
struct Pattern {
    regexes: Vec<JsObject>,
    #[unsafe_ignore_trace]
    patterns: Vec<String>,
    #[unsafe_ignore_trace]
    names: Vec<Vec<String>>,
    #[unsafe_ignore_trace]
    custom: bool,
}

/// The parts of a URL as text, `None` where a value does not name one.
fn url_components(url: &url::Url) -> [String; 8] {
    [
        url.scheme().to_owned(),
        url.username().to_owned(),
        url.password().unwrap_or("").to_owned(),
        url.host_str().unwrap_or("").to_owned(),
        url.port().map(|p| p.to_string()).unwrap_or_default(),
        url.path().to_owned(),
        url.query().unwrap_or("").to_owned(),
        url.fragment().unwrap_or("").to_owned(),
    ]
}

/// What a pattern's (or an input's) dictionary says of each part; `None` where it says nothing.
fn dict_components(
    value: &JsValue,
    context: &mut Context,
) -> JsResult<([Option<String>; 8], Option<String>)> {
    let mut out: [Option<String>; 8] = Default::default();
    for (at, name) in COMPONENTS.iter().enumerate() {
        let v = get(value, name, context)?;
        if !v.is_undefined() {
            let mut text = text_of(&v, context)?;
            // As the standard strips them: `https:` is `https`, `?q` is `q`, `#h` is `h`.
            match *name {
                "protocol" => text = text.strip_suffix(':').map(str::to_owned).unwrap_or(text),
                "search" => text = text.strip_prefix('?').map(str::to_owned).unwrap_or(text),
                "hash" => text = text.strip_prefix('#').map(str::to_owned).unwrap_or(text),
                _ => {}
            }
            out[at] = Some(text);
        }
    }
    let base = get(value, "baseURL", context)?;
    let base = if base.is_undefined() {
        None
    } else {
        Some(text_of(&base, context)?)
    };
    Ok((out, base))
}

/// A pattern string split into its parts the way a URL is: `protocol://host:port/path?query#hash`;
/// without `://`, a pathname (to be read against `baseURL`).
fn split_pattern_string(text: &str) -> [Option<String>; 8] {
    let mut out: [Option<String>; 8] = Default::default();
    let (rest, hash) = match text.find('#') {
        Some(at) => (&text[..at], Some(text[at + 1..].to_owned())),
        None => (text, None),
    };
    let (rest, search) = match find_unbracketed(rest, '?') {
        Some(at) => (&rest[..at], Some(rest[at + 1..].to_owned())),
        None => (rest, None),
    };
    out[6] = search;
    out[7] = hash;
    match rest.find("://") {
        Some(at) => {
            out[0] = Some(rest[..at].to_owned());
            let after = &rest[at + 3..];
            let (authority, path) = match after.find('/') {
                Some(at) => (&after[..at], after[at..].to_owned()),
                None => (after, "/".to_owned()),
            };
            let host = match authority.rfind('@') {
                Some(at) => {
                    let credentials = &authority[..at];
                    match credentials.find(':') {
                        Some(c) => {
                            out[1] = Some(credentials[..c].to_owned());
                            out[2] = Some(credentials[c + 1..].to_owned());
                        }
                        None => out[1] = Some(credentials.to_owned()),
                    }
                    &authority[at + 1..]
                }
                None => authority,
            };
            match host.rfind(':').filter(|at| !host[*at..].contains(']')) {
                Some(at)
                    if host[at + 1..]
                        .chars()
                        .all(|c| c.is_ascii_digit() || "*:()".contains(c)) =>
                {
                    out[3] = Some(host[..at].to_owned());
                    out[4] = Some(host[at + 1..].to_owned());
                }
                _ => {
                    out[3] = Some(host.to_owned());
                    out[4] = Some(String::new());
                }
            }
            out[5] = Some(path);
        }
        None => out[5] = Some(rest.to_owned()),
    }
    out
}

/// Where `c` first comes outside a `(…)` or `{…}` group: a `?` there is a modifier, not a query.
fn find_unbracketed(text: &str, c: char) -> Option<usize> {
    let mut depth = 0i32;
    let mut previous = ' ';
    for (at, ch) in text.char_indices() {
        match ch {
            '(' | '{' => depth += 1,
            ')' | '}' => depth -= 1,
            _ if ch == c
                && depth == 0
                && !matches!(previous, ')' | '}' | '*')
                && !is_name_end(text, at) =>
            {
                return Some(at);
            }
            _ => {}
        }
        previous = ch;
    }
    None
}

/// Whether a `?` at `at` ends a `:name`, which it then makes optional.
fn is_name_end(text: &str, at: usize) -> bool {
    let before = &text[..at];
    let word: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    !word.is_empty() && before[..before.len() - word.len()].ends_with(':')
}

fn new_regexp(source: &str, ignore_case: bool, context: &mut Context) -> JsResult<JsObject> {
    let constructor = context.intrinsics().constructors().regexp().constructor();
    let flags = if ignore_case { "ui" } else { "u" };
    constructor.construct(&[js_text(source), js_text(flags)], None, context)
}

fn construct(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let Some(new_target) = this.as_object() else {
        return Err(type_error(
            "Class constructor URLPattern cannot be invoked without 'new'",
        ));
    };
    let input = arg(args, 0);
    let (base, options) = if arg(args, 1).is_string() {
        (Some(text_of(&arg(args, 1), context)?), arg(args, 2))
    } else {
        (None, arg(args, 1))
    };
    let ignore_case = get(&options, "ignoreCase", context)?.to_boolean();
    let (given, base) = if input.is_undefined() {
        (Default::default(), base)
    } else if input.is_string() {
        (split_pattern_string(&text_of(&input, context)?), base)
    } else if input.is_object() {
        let (given, own_base) = dict_components(&input, context)?;
        (given, own_base.or(base))
    } else {
        return Err(type_error(
            "URLPattern: the input must be a string or a dictionary",
        ));
    };
    // What the pattern does not say is anything — or, with a base URL, the base's (as far as the
    // pathname: a pattern that names its pathname leaves the base's query and hash out).
    let base_parts = match &base {
        Some(base) => {
            Some(url_components(&url::Url::parse(base).map_err(|_| {
                type_error(format!("URLPattern: invalid base URL {base}"))
            })?))
        }
        None => None,
    };
    let mut patterns = Vec::new();
    for at in 0..8 {
        let pattern = match (&given[at], &base_parts) {
            (Some(p), _) => p.clone(),
            (None, Some(base)) if at < 5 || (at == 5 && given[5].is_none()) => {
                escape_fixed(&base[at])
            }
            (None, Some(_)) if given[5].is_none() => "*".to_owned(),
            (None, Some(_)) => String::new(),
            (None, None) => "*".to_owned(),
        };
        patterns.push(pattern);
    }
    // A relative pathname pattern is read against the base's folder.
    if let (Some(base), Some(path)) = (&base_parts, &given[5])
        && !path.starts_with('/')
        && !path.starts_with('*')
        && !path.starts_with(':')
        && !path.starts_with('{')
        && !path.starts_with('(')
    {
        let mut folder = base[5][..base[5].rfind('/').map_or(0, |at| at + 1)].to_owned();
        let mut path = path.as_str();
        loop {
            if let Some(rest) = path.strip_prefix("./") {
                path = rest;
            } else if let Some(rest) = path.strip_prefix("../") {
                path = rest;
                let trimmed = folder.trim_end_matches('/');
                folder.truncate(trimmed.rfind('/').map_or(0, |at| at + 1));
            } else {
                break;
            }
        }
        patterns[5] = format!("{}{path}", escape_fixed(&folder));
    }
    let mut regexes = Vec::new();
    let mut names = Vec::new();
    let mut custom = false;
    for (at, pattern) in patterns.iter().enumerate() {
        let (prefix, segment) = match COMPONENTS[at] {
            "pathname" => (Some('/'), "[^/]+?"),
            "hostname" => (None, "[^.]+?"),
            _ => (None, "[^]+?"),
        };
        let parts = parse(pattern, prefix).map_err(|e| {
            type_error(format!(
                "URLPattern: invalid {} pattern '{pattern}': {e}",
                COMPONENTS[at]
            ))
        })?;
        let (source, group_names, has_regex) = regex_of(&parts, segment);
        custom |= has_regex;
        let regex = new_regexp(&source, ignore_case, context).map_err(|_| {
            type_error(format!(
                "URLPattern: invalid {} pattern '{pattern}'",
                COMPONENTS[at]
            ))
        })?;
        regexes.push(regex);
        names.push(group_names);
    }
    let prototype = new_target
        .get(js_string!("prototype"), context)?
        .as_object()
        .unwrap_or_else(|| object(context));
    Ok(JsObject::from_proto_and_data(
        Some(prototype),
        Pattern {
            regexes,
            patterns,
            names,
            custom,
        },
    )
    .into())
}

/// Fixed text, as a pattern that matches only itself.
fn escape_fixed(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\:{}()*+?".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn pattern_of(this: &JsValue) -> JsResult<JsObject> {
    this.as_object()
        .filter(|o| o.is::<Pattern>())
        .ok_or_else(|| type_error("Illegal invocation"))
}

/// The parts of what `test`/`exec` were given: a URL string (against an optional base) or a
/// dictionary; `None` when a string is not a URL, which matches nothing.
fn input_components(
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<Option<([String; 8], JsValue)>> {
    let input = arg(args, 0);
    let inputs = if arg(args, 1).is_undefined() {
        crate::native::array(context, vec![input.clone()])
    } else {
        crate::native::array(context, vec![input.clone(), arg(args, 1)])
    };
    if input.is_object() {
        let (given, base) = dict_components(&input, context)?;
        let mut parts: [String; 8] = Default::default();
        if let Some(base) = base.and_then(|b| url::Url::parse(&b).ok()) {
            parts = url_components(&base);
        }
        for (at, value) in given.into_iter().enumerate() {
            if let Some(value) = value {
                parts[at] = value;
            }
        }
        return Ok(Some((parts, inputs)));
    }
    let text = if input.is_undefined() {
        String::new()
    } else {
        text_of(&input, context)?
    };
    let base = arg(args, 1);
    let url = if base.is_undefined() {
        url::Url::parse(&text)
    } else {
        let base = text_of(&base, context)?;
        url::Url::parse(&base).and_then(|b| b.join(&text))
    };
    Ok(url.ok().map(|url| (url_components(&url), inputs)))
}

/// Runs the pattern on the input: per part, `{ input, groups }`, or `None` when a part fails.
fn run(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<Option<JsObject>> {
    let pattern = pattern_of(this)?;
    let Some((parts, inputs)) = input_components(args, context)? else {
        return Ok(None);
    };
    let (regexes, names) = {
        let data = pattern.downcast_ref::<Pattern>().expect("a pattern");
        (data.regexes.clone(), data.names.clone())
    };
    let result = object(context);
    result.set(js_string!("inputs"), inputs, false, context)?;
    for at in 0..8 {
        let matched = invoke(
            &regexes[at].clone().into(),
            "exec",
            &[js_text(&parts[at])],
            context,
        )?;
        let Some(matched) = matched.as_object() else {
            return Ok(None);
        };
        let groups = object(context);
        for (index, name) in names[at].iter().enumerate() {
            let value = matched.get(index + 1, context)?;
            groups.set(JsString::from(name.as_str()), value, false, context)?;
        }
        let component = object(context);
        component.set(js_string!("input"), js_text(&parts[at]), false, context)?;
        component.set(js_string!("groups"), groups, false, context)?;
        result.set(JsString::from(COMPONENTS[at]), component, false, context)?;
    }
    Ok(Some(result))
}

const METHODS: &[(&str, usize, NativeFunctionPointer)] = &[
    ("test", 1, |this, args, cx| {
        Ok(run(this, args, cx)?.is_some().into())
    }),
    ("exec", 1, |this, args, cx| {
        Ok(run(this, args, cx)?.map_or(JsValue::null(), JsValue::from))
    }),
];

macro_rules! component_getters {
    ($($at:literal $name:literal),*) => {
        &[$(($name, |this, _, _| {
            let pattern = pattern_of(this)?;
            let data = pattern.downcast_ref::<Pattern>().expect("a pattern");
            Ok(js_text(&data.patterns[$at]))
        }, None)),*]
    };
}

const ACCESSORS: &[(&str, NativeFunctionPointer, Option<NativeFunctionPointer>)] = component_getters!(
    0 "protocol", 1 "username", 2 "password", 3 "hostname", 4 "port", 5 "pathname", 6 "search", 7 "hash"
);

/// Makes `URLPattern` a global, made the first time it is read.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    crate::native::lazy_global(context, "URLPattern", |cx| {
        let (class, prototype) =
            crate::streams::web_class(cx, "URLPattern", 0, construct, METHODS, ACCESSORS, &[])?;
        crate::native::accessors(
            cx,
            &prototype,
            &[(
                "hasRegExpGroups",
                |this, _, _| {
                    let pattern = pattern_of(this)?;
                    let data = pattern.downcast_ref::<Pattern>().expect("a pattern");
                    Ok(data.custom.into())
                },
                None,
            )],
        )?;
        Ok(class.into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_become_the_standards_regular_expressions() {
        let pathname = |p: &str| regex_of(&parse(p, Some('/')).unwrap(), "[^/]+?").0;
        assert_eq!(pathname("/books/:id"), r"^\/books(?:\/([^/]+?))$");
        assert_eq!(pathname("/books/:id?"), r"^\/books(?:\/([^/]+?))?$");
        assert_eq!(
            pathname("/:type(users|groups){/local}?"),
            r"^(?:\/(users|groups))(?:\/local)?$"
        );
        assert_eq!(pathname("/files/*"), r"^\/files(?:\/(.*))$");
        assert_eq!(regex_of(&parse("*", None).unwrap(), "[^]+?").0, "^(.*)$");
        let parts = split_pattern_string("https://:sub.example.com:8080/a/:b/?x=1#top");
        assert_eq!(parts[0].as_deref(), Some("https"));
        assert_eq!(parts[3].as_deref(), Some(":sub.example.com"));
        assert_eq!(parts[4].as_deref(), Some("8080"));
        assert_eq!(parts[5].as_deref(), Some("/a/:b/"));
        assert_eq!(parts[6].as_deref(), Some("x=1"));
        assert_eq!(parts[7].as_deref(), Some("top"));
        assert_eq!(
            split_pattern_string("/a/:id?")[5].as_deref(),
            Some("/a/:id?")
        );
    }
}
