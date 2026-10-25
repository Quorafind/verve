//! Node 22's shared package resolution rules. ESM never probes extensions or directory indexes.

use std::path::{Path, PathBuf};

use url::Url;

pub(crate) use crate::package_json::{Json as Value, Object};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Import,
    Require,
}

#[derive(Debug)]
pub(crate) struct ResolveError {
    pub code: &'static str,
    pub message: String,
}

pub(crate) type Result<T> = std::result::Result<T, ResolveError>;

fn error(code: &'static str, message: impl Into<String>) -> ResolveError {
    ResolveError {
        code,
        message: message.into(),
    }
}

pub(crate) fn file_url(path: &Path) -> Result<Url> {
    Url::from_file_path(crate::modules::clean(path)).map_err(|()| {
        error(
            "ERR_INVALID_FILE_URL_PATH",
            format!("Invalid absolute path: {}", path.display()),
        )
    })
}

pub(crate) fn file_path(url: &Url) -> Result<PathBuf> {
    let lower = url.path().to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return Err(error(
            "ERR_INVALID_MODULE_SPECIFIER",
            "Encoded path separators are not allowed",
        ));
    }
    url.to_file_path().map_err(|()| {
        error(
            "ERR_INVALID_FILE_URL_PATH",
            format!("Invalid file URL: {url}"),
        )
    })
}

pub(crate) fn manifest(dir: &Path) -> Result<Option<Value>> {
    let path = dir.join("package.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|err| {
            error(
                "ERR_INVALID_PACKAGE_CONFIG",
                format!("Invalid package config {}: {err}", path.display()),
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(error(
            "ERR_INVALID_PACKAGE_CONFIG",
            format!("{}: {err}", path.display()),
        )),
    }
}

pub(crate) fn scope(path: &Path) -> Result<Option<(PathBuf, Value)>> {
    let mut dir = path.parent();
    while let Some(at) = dir {
        if at.file_name().is_some_and(|s| s == "node_modules") {
            break;
        }
        if let Some(json) = manifest(at)? {
            return Ok(Some((at.to_path_buf(), json)));
        }
        dir = at.parent();
    }
    Ok(None)
}

/// ESM_RESOLVE, also used by CJS for package exports, imports and self references.
pub(crate) fn resolve(
    specifier: &str,
    parent: &Url,
    mode: Mode,
    conditions: &[String],
) -> Result<Url> {
    resolve_inner(specifier, parent, mode, conditions, 0)
}

fn resolve_inner(
    specifier: &str,
    parent: &Url,
    mode: Mode,
    conditions: &[String],
    depth: usize,
) -> Result<Url> {
    if depth > 64 {
        return Err(error(
            "ERR_INVALID_PACKAGE_TARGET",
            "Package imports mapping is recursive",
        ));
    }
    if specifier.is_empty() || specifier.contains('\\') {
        return Err(error(
            "ERR_INVALID_MODULE_SPECIFIER",
            format!("Invalid module specifier '{specifier}'"),
        ));
    }
    if crate::node::MODULES
        .iter()
        .any(|(name, _)| *name == specifier)
        && !crate::node::PREFIX_ONLY.contains(&specifier)
    {
        return Url::parse(&format!("node:{specifier}"))
            .map_err(|e| error("ERR_INVALID_MODULE_SPECIFIER", e.to_string()));
    }
    if let Ok(url) = Url::parse(specifier) {
        return Ok(url);
    }
    if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        return parent
            .join(specifier)
            .map_err(|e| error("ERR_UNSUPPORTED_RESOLVE_REQUEST", e.to_string()));
    }
    if specifier.starts_with('#') {
        if specifier == "#" || specifier.starts_with("#/") {
            return Err(error(
                "ERR_INVALID_MODULE_SPECIFIER",
                format!("Invalid package import '{specifier}'"),
            ));
        }
        let path = file_path(parent)?;
        if let Some((root, json)) = scope(&path)?
            && let Some(map) = json.get("imports").and_then(Value::as_object)
            && let Some(target) = map_match(map, specifier, &root, true, mode, conditions, depth)?
        {
            return Ok(target);
        }
        return Err(error(
            "ERR_PACKAGE_IMPORT_NOT_DEFINED",
            format!("Package import specifier '{specifier}' is not defined in {parent}"),
        ));
    }
    let (name, subpath) = package_name(specifier)?;
    let path = file_path(parent).map_err(|_| {
        error(
            "ERR_UNSUPPORTED_RESOLVE_REQUEST",
            format!("Cannot resolve '{specifier}' from {parent}"),
        )
    })?;
    if let Some((root, json)) = scope(&path)?
        && json.get("name").and_then(Value::as_str) == Some(name)
        && let Some(exports) = json.get("exports").filter(|v| !v.is_null())
    {
        return exports_resolve(&root, exports, &subpath, mode, conditions, depth);
    }
    let mut at = path.parent();
    while let Some(dir) = at {
        if dir.file_name().is_none_or(|s| s != "node_modules") {
            let root = dir.join("node_modules").join(name);
            if root.is_dir() {
                let json = manifest(&root)?;
                if let Some(exports) = json
                    .as_ref()
                    .and_then(|j| j.get("exports"))
                    .filter(|v| !v.is_null())
                {
                    return exports_resolve(&root, exports, &subpath, mode, conditions, depth);
                }
                if subpath != "." {
                    let candidate = root.join(&subpath[2..]);
                    if mode == Mode::Require
                        && let Some(file) = legacy_main(&candidate)
                    {
                        return file_url(&file);
                    }
                    return file_url(&candidate);
                }
                let main = json
                    .as_ref()
                    .and_then(|j| j.get("main"))
                    .and_then(Value::as_str);
                if let Some(main) = main {
                    let candidate = root.join(main);
                    if let Some(file) = legacy_main(&candidate) {
                        return file_url(&file);
                    }
                }
                if let Some(file) = legacy_main(&root.join("index")) {
                    return file_url(&file);
                }
                return Err(error(
                    "ERR_MODULE_NOT_FOUND",
                    format!("Cannot find package entry '{specifier}' imported from {parent}"),
                ));
            }
        }
        at = dir.parent();
    }
    Err(error(
        "ERR_MODULE_NOT_FOUND",
        format!("Cannot find package '{name}' imported from {parent}"),
    ))
}

fn package_name(specifier: &str) -> Result<(&str, String)> {
    let end = if specifier.starts_with('@') {
        let Some(first) = specifier.find('/') else {
            return Err(error(
                "ERR_INVALID_MODULE_SPECIFIER",
                format!("Invalid package name '{specifier}'"),
            ));
        };
        specifier[first + 1..]
            .find('/')
            .map_or(specifier.len(), |i| first + 1 + i)
    } else {
        specifier.find('/').unwrap_or(specifier.len())
    };
    let name = &specifier[..end];
    if name.starts_with('.') || name.contains('%') || name.ends_with('/') || name == "@" {
        return Err(error(
            "ERR_INVALID_MODULE_SPECIFIER",
            format!("Invalid package name '{specifier}'"),
        ));
    }
    Ok((
        name,
        if end == specifier.len() {
            ".".into()
        } else {
            format!(".{}", &specifier[end..])
        },
    ))
}

fn legacy_main(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.into());
    }
    for ext in ["js", "json", "node"] {
        let candidate = PathBuf::from(format!("{}.{ext}", path.display()));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for name in ["index.js", "index.json", "index.node"] {
        let candidate = path.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn exports_resolve(
    root: &Path,
    exports: &Value,
    subpath: &str,
    mode: Mode,
    conditions: &[String],
    depth: usize,
) -> Result<Url> {
    let target = if let Some(map) = exports.as_object() {
        let dots = map.keys().filter(|k| k.starts_with('.')).count();
        if dots != 0 && dots != map.len() {
            return Err(error(
                "ERR_INVALID_PACKAGE_CONFIG",
                format!("Mixed package exports keys in {}", root.display()),
            ));
        }
        if dots != 0 {
            map_match(map, subpath, root, false, mode, conditions, depth)?
        } else if subpath == "." {
            package_target(exports, root, None, false, mode, conditions, depth)?.url()
        } else {
            None
        }
    } else if subpath == "." {
        package_target(exports, root, None, false, mode, conditions, depth)?.url()
    } else {
        None
    };
    target.ok_or_else(|| {
        error(
            "ERR_PACKAGE_PATH_NOT_EXPORTED",
            format!(
                "Package subpath '{subpath}' is not defined by exports in {}/package.json",
                root.display()
            ),
        )
    })
}

fn map_match(
    map: &Object,
    name: &str,
    root: &Path,
    imports: bool,
    mode: Mode,
    conditions: &[String],
    depth: usize,
) -> Result<Option<Url>> {
    if let Some(value) = map.get(name).filter(|_| !name.contains('*')) {
        return package_target(value, root, None, imports, mode, conditions, depth)
            .map(Target::url);
    }
    let mut matches: Vec<_> = map
        .iter()
        .filter_map(|(key, value)| {
            let (prefix, suffix) = key.split_once('*')?;
            if suffix.contains('*')
                || !name.starts_with(prefix)
                || !name.ends_with(suffix)
                || name.len() < prefix.len() + suffix.len()
            {
                return None;
            }
            Some((
                prefix.len(),
                key.len(),
                &name[prefix.len()..name.len() - suffix.len()],
                value,
            ))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    if let Some((_, _, matched, value)) = matches.first() {
        return package_target(value, root, Some(matched), imports, mode, conditions, depth)
            .map(Target::url);
    }
    Ok(None)
}

fn invalid_segments(path: &str) -> bool {
    let decoded = percent_decode(path)
        .unwrap_or_else(|_| path.into())
        .replace('\\', "/");
    decoded
        .split('/')
        .any(|part| part == "." || part == ".." || part.eq_ignore_ascii_case("node_modules"))
}

enum Target {
    Unmatched,
    Blocked,
    Url(Url),
}

impl Target {
    fn url(self) -> Option<Url> {
        match self {
            Self::Url(url) => Some(url),
            Self::Unmatched | Self::Blocked => None,
        }
    }
}

fn package_target(
    target: &Value,
    root: &Path,
    matched: Option<&str>,
    imports: bool,
    mode: Mode,
    conditions: &[String],
    depth: usize,
) -> Result<Target> {
    match target {
        Value::Null => Ok(Target::Blocked),
        Value::String(target) => {
            let replacement = matched.map_or_else(|| target.clone(), |s| target.replace('*', s));
            if !target.starts_with("./") {
                if imports
                    && !target.starts_with("../")
                    && !target.starts_with('/')
                    && Url::parse(target).is_err()
                {
                    let base = file_url(&root.join("package.json"))?;
                    return resolve_inner(&replacement, &base, mode, conditions, depth + 1)
                        .map(Target::Url);
                }
                return Err(error(
                    "ERR_INVALID_PACKAGE_TARGET",
                    format!("Invalid package target '{target}' in {}", root.display()),
                ));
            }
            if invalid_segments(&target[2..]) || matched.is_some_and(invalid_segments) {
                return Err(error(
                    "ERR_INVALID_PACKAGE_TARGET",
                    format!("Invalid package target '{replacement}'"),
                ));
            }
            let base = file_url(&root.join("package.json"))?;
            base.join(&replacement)
                .map(Target::Url)
                .map_err(|e| error("ERR_INVALID_PACKAGE_TARGET", e.to_string()))
        }
        Value::Array(targets) => {
            let mut last = None;
            for item in targets {
                match package_target(item, root, matched, imports, mode, conditions, depth) {
                    Ok(Target::Url(url)) => return Ok(Target::Url(url)),
                    Ok(Target::Blocked) => last = None,
                    Ok(Target::Unmatched) => {}
                    Err(err) if err.code == "ERR_INVALID_PACKAGE_TARGET" => last = Some(err),
                    Err(err) => return Err(err),
                }
            }
            match last {
                Some(err) => Err(err),
                None => Ok(Target::Blocked),
            }
        }
        Value::Object(map) => {
            if map.keys().any(|key| {
                key.parse::<u32>()
                    .is_ok_and(|index| index != u32::MAX && index.to_string() == *key)
            }) {
                return Err(error(
                    "ERR_INVALID_PACKAGE_CONFIG",
                    "Numeric package condition keys are not allowed",
                ));
            }
            // Conditions are tried in source order, which `Object` keeps.
            for (condition, target) in map {
                if condition == "default"
                    || condition == "node"
                    || condition == "node-addons"
                    || condition
                        == if mode == Mode::Import {
                            "import"
                        } else {
                            "require"
                        }
                    || condition == "module-sync"
                    || conditions.contains(condition)
                {
                    match package_target(target, root, matched, imports, mode, conditions, depth)? {
                        Target::Unmatched => {}
                        other => return Ok(other),
                    }
                }
            }
            Ok(Target::Unmatched)
        }
        _ => Err(error(
            "ERR_INVALID_PACKAGE_TARGET",
            format!("Invalid package target in {}", root.display()),
        )),
    }
}

pub(crate) fn finalize(mut url: Url) -> Result<Url> {
    if url.scheme() != "file" {
        return Ok(url);
    }
    let path = file_path(&url)?;
    if path.is_dir() {
        return Err(error(
            "ERR_UNSUPPORTED_DIR_IMPORT",
            format!("Directory import '{}' is not supported", path.display()),
        ));
    }
    if !path.is_file() {
        return Err(error(
            "ERR_MODULE_NOT_FOUND",
            format!("Cannot find module '{}'", path.display()),
        ));
    }
    if let Ok(real) = std::fs::canonicalize(&path) {
        let canonical = file_url(&real)?;
        url.set_path(canonical.path());
    }
    Ok(url)
}

pub(crate) fn percent_decode(text: &str) -> Result<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if at + 2 >= bytes.len() {
                return Err(error("ERR_INVALID_URL", "Incomplete percent escape"));
            }
            let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).unwrap_or("");
            out.push(
                u8::from_str_radix(hex, 16)
                    .map_err(|_| error("ERR_INVALID_URL", "Invalid percent escape"))?,
            );
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).map_err(|_| error("ERR_INVALID_URL", "URL is not UTF-8"))
}
