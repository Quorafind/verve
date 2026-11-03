//! What the module loader asks the code cache: a module's code, or a CommonJS module's wrapper —
//! Node's own wrapper is a classic script that declares nothing in the global scope, so it is
//! kept between runs as a module's code is (`docs/startup.md`).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::{Context, JsResult, Module, Source, script::Script};

use crate::code_cache::CodeCache;

/// What an entry's code is: a module's, or a script's — a CommonJS module's wrapper.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Module,
    Script,
}

impl Kind {
    /// What a key says the code is: a module's and a script's code of one source text differ.
    pub(super) const fn domain(self) -> &'static [u8] {
        match self {
            Self::Module => b"module\0",
            Self::Script => b"script\0",
        }
    }
}

/// The cache this runtime keeps, or asks the default directory whether `path` has been loaded
/// before: its mark naming the file is what makes the second run of a program the one that
/// compiles everything and keeps it.
fn cache_of(context: &Context, path: &Path, len: u64) -> Option<Rc<CodeCache>> {
    let cache = crate::shared(context).code_cache.borrow().clone();
    match cache {
        Some(cache) => Some(cache),
        None => crate::code_cache_mark::deferred(context, &file_of(path), len),
    }
}

/// The file `path` names on disk: a module's and a wrapper's path is the `file:` URL their code
/// is compiled against, and the mark records files.
fn file_of(path: &Path) -> PathBuf {
    path.to_str()
        .and_then(|text| url::Url::parse(text).ok())
        .and_then(|url| url.to_file_path().ok())
        .unwrap_or_else(|| path.to_path_buf())
}

/// The module `source` (at `path`) is: read back from the runtime's code cache if it keeps one
/// with an entry for it, parsed otherwise, and then kept in the cache.
pub(crate) fn module(source: &str, path: &Path, context: &mut Context) -> JsResult<Module> {
    let Some(cache) = cache_of(context, path, source.len() as u64) else {
        return Module::parse(Source::from_bytes(source).with_path(path), None, context);
    };
    let global = boa_engine::code_cache::global_scope_fingerprint(context);
    match cache.load(Kind::Module, source, global) {
        Ok(Some(bytes)) => {
            if let Ok(module) = Module::from_code_cache(&bytes, source, Some(path), None, context) {
                cache.count(|stats| stats.hits += 1);
                return Ok(module);
            }
            cache.count(|stats| stats.refused += 1);
        }
        Ok(None) => cache.count(|stats| stats.misses += 1),
        Err(()) => cache.count(|stats| stats.refused += 1),
    }
    let module = Module::parse(Source::from_bytes(source).with_path(path), None, context)?;
    // A module that fails to compile fails when it is linked, as without the cache.
    if let Ok(Some(bytes)) = module.code_cache(context) {
        cache.store(Kind::Module, source, global, bytes);
        cache.count(|stats| stats.stored += 1);
    }
    Ok(module)
}

/// The script `source` (at `path`) is — a CommonJS module's wrapper: read back from the
/// runtime's code cache if it keeps one with an entry for it, parsed otherwise, and then kept in
/// the cache. A source that is not a script is refused as parsing it refuses it, and nothing
/// about it is counted: the caller decides what else it is.
pub(crate) fn script(source: &str, path: &Path, context: &mut Context) -> JsResult<Script> {
    let Some(cache) = cache_of(context, path, source.len() as u64) else {
        return Script::parse(Source::from_bytes(source).with_path(path), None, context);
    };
    let global = boa_engine::code_cache::global_scope_fingerprint(context);
    let missed = match read_script(&cache, source, path, global, context) {
        Ok(Some(script)) => {
            cache.count(|stats| stats.cjs_hits += 1);
            return Ok(script);
        }
        Ok(None) => true,
        Err(()) => {
            cache.count(|stats| stats.cjs_refused += 1);
            false
        }
    };
    let script = Script::parse(Source::from_bytes(source).with_path(path), None, context)?;
    if missed {
        cache.count(|stats| stats.cjs_misses += 1);
    }
    match script.code_cache(context) {
        Ok(Some(bytes)) => {
            cache.store(Kind::Script, source, global, bytes);
            cache.count(|stats| stats.cjs_stored += 1);
        }
        // Its wrapper compiles to code that cannot be written down; see `cjs_skipped`.
        Ok(None) => cache.count(|stats| stats.cjs_skipped += 1),
        // Compiling failed, and running it fails as it would without a cache.
        Err(_) => {}
    }
    Ok(script)
}

/// The script the cache keeps for `source`: `Ok(None)` without an entry, `Err` with one that
/// cannot be read back.
fn read_script(
    cache: &CodeCache,
    source: &str,
    path: &Path,
    global: u64,
    context: &mut Context,
) -> Result<Option<Script>, ()> {
    let bytes = match cache.load(Kind::Script, source, global) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return Ok(None),
        Err(()) => return Err(()),
    };
    Script::from_code_cache(&bytes, source, Some(path), None, context)
        .map(Some)
        .map_err(|_| ())
}
