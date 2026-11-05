//! An import map as HTML reads one: `imports` and `scopes` that rewrite the specifiers a module
//! graph asks for, before anything resolves them. Parsed and applied here, in Rust; the page's
//! `<script type="importmap">` is the embedder's to find and hand over (see
//! [`Runtime::set_import_map`](crate::Runtime::set_import_map)).

use serde_json::Value;
use url::Url;

/// Whether `specifier` starts like a path (`/`, `./`, `../`), so it is an address, not a name.
pub fn is_relative(specifier: &str) -> bool {
    ["/", "./", "../"].iter().any(|p| specifier.starts_with(p))
}

/// One table: a specifier (or a `prefix/`) and the address it stands for. `None` is an address
/// the map blocked by writing it wrongly.
type Entries = Vec<(String, Option<Url>)>;

/// The `imports` and `scopes` of one import map, their addresses resolved against the map's base.
#[derive(Clone, Debug, Default)]
pub struct ImportMap {
    imports: Entries,
    scopes: Vec<(String, Entries)>,
    /// What the map said wrongly and the reading went on without: for the console.
    warnings: Vec<String>,
}

/// A specifier that starts like a path or is a whole URL, as a URL (a path against `base`);
/// anything else is bare.
pub fn url_like(specifier: &str, base: &Url) -> Option<Url> {
    if is_relative(specifier) {
        return base.join(specifier).ok();
    }
    Url::parse(specifier).ok()
}

/// The schemes whose addresses a prefix of the map may stand for.
fn is_special(url: &Url) -> bool {
    matches!(
        url.scheme(),
        "http" | "https" | "ws" | "wss" | "ftp" | "file"
    )
}

fn table(value: &Value, base: &Url, warnings: &mut Vec<String>) -> Result<Entries, String> {
    let object = value
        .as_object()
        .ok_or("an import map's imports must be an object")?;
    let mut entries = Entries::new();
    for (key, address) in object {
        let key = url_like(key, base).map_or_else(|| key.clone(), String::from);
        // An address that is no string blocks the specifier, as one written wrongly does.
        let Some(address) = address.as_str() else {
            warnings.push(format!("the address of \"{key}\" must be a string"));
            entries.push((key, None));
            continue;
        };
        // A prefix maps to a prefix.
        let address = url_like(address, base)
            .filter(|url| !key.ends_with('/') || url.as_str().ends_with('/'));
        entries.push((key, address));
    }
    // As the specification sorts them: the longest prefix is tried first.
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries)
}

/// What `specifier` is in `entries`: `Ok(None)` when no entry is for it, an error when the map
/// blocks it. A prefix entry applies when `prefixes` (the specifier is no URL, or a special one).
fn lookup(specifier: &str, entries: &Entries, prefixes: bool) -> Result<Option<Url>, String> {
    for (key, address) in entries {
        let exact = key == specifier;
        let prefix = prefixes && key.ends_with('/') && specifier.starts_with(key.as_str());
        if !exact && !prefix {
            continue;
        }
        let Some(address) = address else {
            return Err(format!("the import map blocks \"{specifier}\""));
        };
        if exact {
            return Ok(Some(address.clone()));
        }
        let joined = address
            .join(&specifier[key.len()..])
            .map_err(|_| format!("\"{specifier}\" is not an address under {address}"))?;
        // `pkg/../x` may not leave the package.
        if !joined.as_str().starts_with(address.as_str()) {
            return Err(format!("\"{specifier}\" backtracks out of {address}"));
        }
        return Ok(Some(joined));
    }
    Ok(None)
}

impl ImportMap {
    /// Reads the map `json` says, its relative addresses resolved against `base` (the page).
    pub fn parse(json: &str, base: &Url) -> Result<Self, String> {
        let value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let object = value
            .as_object()
            .ok_or("an import map must be a JSON object")?;
        let mut warnings = Vec::new();
        let imports = match object.get("imports") {
            Some(imports) => table(imports, base, &mut warnings)?,
            None => Entries::new(),
        };
        let mut scopes = Vec::new();
        if let Some(found) = object.get("scopes") {
            let found = found
                .as_object()
                .ok_or("an import map's scopes must be an object")?;
            for (prefix, entries) in found {
                if let Ok(prefix) = base.join(prefix) {
                    scopes.push((prefix.to_string(), table(entries, base, &mut warnings)?));
                }
            }
        }
        scopes.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(Self {
            imports,
            scopes,
            warnings,
        })
    }

    /// Adds what a map that came after this one says: an entry for a specifier this one
    /// already maps stays as it was, as Chromium merges maps.
    pub fn merge(&mut self, later: ImportMap) {
        fn add(into: &mut Entries, from: Entries) {
            for (key, address) in from {
                if !into.iter().any(|(known, _)| *known == key) {
                    into.push((key, address));
                }
            }
            into.sort_by(|a, b| b.0.cmp(&a.0));
        }
        add(&mut self.imports, later.imports);
        for (prefix, entries) in later.scopes {
            match self.scopes.iter_mut().find(|(known, _)| *known == prefix) {
                Some((_, into)) => add(into, entries),
                None => self.scopes.push((prefix, entries)),
            }
        }
        self.scopes.sort_by(|a, b| b.0.cmp(&a.0));
        self.warnings.extend(later.warnings);
    }

    /// What the map got wrong and was read without, for the console.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// What `specifier`, asked for by the module at `referrer`, is mapped to; `None` when the
    /// map says nothing of it (it resolves as it would without a map), an error when the map
    /// blocks it.
    pub fn resolve(&self, specifier: &str, referrer: &Url) -> Result<Option<Url>, String> {
        // A scope is matched by the address of the page or module, never by a fragment of it
        // (the page's inline modules are told apart by one).
        let mut referrer = referrer.clone();
        referrer.set_fragment(None);
        let referrer = &referrer;
        let as_url = url_like(specifier, referrer);
        let prefixes = as_url.as_ref().is_none_or(is_special);
        let normalized = as_url.map_or_else(|| specifier.to_owned(), String::from);
        let at = referrer.as_str();
        for (prefix, entries) in &self.scopes {
            let inside = prefix == at || (prefix.ends_with('/') && at.starts_with(prefix.as_str()));
            if inside && let Some(found) = lookup(&normalized, entries, prefixes)? {
                return Ok(Some(found));
            }
        }
        lookup(&normalized, &self.imports, prefixes)
    }
}
