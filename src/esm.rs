//! Native ECMAScript modules: URL identities, Node packages and synthetic CJS/builtin bridges.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::ready;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module, Source,
    js_string,
    module::{ModuleLoader, ModuleRequest, Referrer, SyntheticModuleInitializer},
    object::builtins::{JsFunction, JsPromise},
    property::PropertyKey,
    script::Script,
};
use url::Url;

use crate::esm_resolve::{self as resolve, Mode};
use crate::native::{arg, closure, js_text, text_of};

fn coded_error(context: &mut Context, message: &str, code: &str) -> JsError {
    if matches!(
        code,
        "ERR_INVALID_MODULE_SPECIFIER"
            | "ERR_UNKNOWN_FILE_EXTENSION"
            | "ERR_INVALID_URL"
            | "ERR_INVALID_FILE_URL_PATH"
            | "ERR_UNKNOWN_MODULE_FORMAT"
            | "ERR_IMPORT_ATTRIBUTE_MISSING"
            | "ERR_IMPORT_ATTRIBUTE_UNSUPPORTED"
            | "ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE"
    ) {
        let error = JsNativeError::typ()
            .with_message(message.to_owned())
            .into_opaque(context);
        let _ = error.set(js_string!("code"), JsString::from(code), false, context);
        JsError::from_opaque(error.into())
    } else {
        crate::native::coded_error(context, message, code)
    }
}

#[derive(Default)]
pub(crate) struct Loader {
    /// Full serialized URLs, including query and fragment, are the ESM cache keys.
    modules: RefCell<HashMap<String, Module>>,
    sources: RefCell<HashMap<String, String>>,
    formats: RefCell<HashMap<String, bool>>,
    captured_blobs: RefCell<HashMap<String, Vec<u8>>>,
    builtin_names: RefCell<HashMap<String, Vec<JsString>>>,
    requiring: RefCell<HashSet<String>>,
    require_namespaces: RefCell<HashMap<String, JsObject>>,
    conditions: RefCell<Vec<String>>,
}

fn resolve_error(context: &mut Context, error: resolve::ResolveError) -> JsError {
    coded_error(context, &error.message, error.code)
}

impl Loader {
    pub(crate) fn is_requiring(&self) -> bool {
        !self.requiring.borrow().is_empty()
    }

    pub(crate) fn conditions(&self, conditions: Vec<String>) {
        *self.conditions.borrow_mut() = conditions;
    }

    fn parent(&self, parent: Option<&Path>, context: &mut Context) -> JsResult<Url> {
        if let Some(parent) = parent {
            let text = parent.to_string_lossy();
            if let Ok(url) = Url::parse(&text)
                && matches!(url.scheme(), "file" | "data" | "blob" | "node")
            {
                return Ok(url);
            }
            let path = absolute(parent, context);
            return resolve::file_url(&path).map_err(|e| resolve_error(context, e));
        }
        let cwd = PathBuf::from(crate::shared(context).cwd.borrow().clone());
        resolve::file_url(&cwd.join("__runtime_eval__.js")).map_err(|e| resolve_error(context, e))
    }

    pub(crate) fn resolve(
        &self,
        specifier: &str,
        parent: Option<&Path>,
        mode: Mode,
        check_file: bool,
        context: &mut Context,
    ) -> JsResult<Url> {
        if (!specifier.ends_with('/') || mode == Mode::Require)
            && crate::modules::is_builtin(context, specifier)
        {
            let bare = crate::modules::bare_name(specifier);
            return Url::parse(&format!("node:{bare}")).map_err(|_| {
                coded_error(
                    context,
                    "Invalid builtin module name",
                    "ERR_INVALID_MODULE_SPECIFIER",
                )
            });
        }
        let parent = self.parent(parent, context)?;
        let result = resolve::resolve(specifier, &parent, mode, &self.conditions.borrow());
        let url = result.map_err(|e| resolve_error(context, e))?;
        match url.scheme() {
            "file" | "data" | "blob" => {}
            "node" => {
                if !url.path().ends_with('/') && crate::modules::is_builtin(context, url.as_str()) {
                    return Ok(url);
                }
                return Err(coded_error(
                    context,
                    &format!("No such built-in module: {url}"),
                    "ERR_UNKNOWN_BUILTIN_MODULE",
                ));
            }
            _ => {
                return Err(coded_error(
                    context,
                    &format!(
                        "Only file:, data:, blob: and node: module URLs are supported. Received {url}"
                    ),
                    "ERR_UNSUPPORTED_ESM_URL_SCHEME",
                ));
            }
        }
        if check_file {
            resolve::finalize(url).map_err(|e| resolve_error(context, e))
        } else {
            if url.scheme() == "file" {
                resolve::file_path(&url).map_err(|e| resolve_error(context, e))?;
            }
            Ok(url)
        }
    }

    pub(crate) fn prepare(
        &self,
        referrer: &Referrer,
        request: &ModuleRequest,
        context: &mut Context,
    ) -> JsResult<()> {
        let name = request.specifier().to_std_string_escaped();
        let url = self.resolve(&name, referrer.path(), Mode::Import, true, context)?;
        if url.scheme() == "blob"
            && !self.modules.borrow().contains_key(url.as_str())
            && !self.captured_blobs.borrow().contains_key(url.as_str())
        {
            // This hook runs while import() executes, before script code can revoke the URL.
            let bytes = crate::shared(context)
                .blobs
                .borrow()
                .get(url.as_str())
                .cloned();
            let Some(bytes) = bytes else {
                return Err(coded_error(
                    context,
                    &format!("Cannot find module '{url}'"),
                    "ERR_MODULE_NOT_FOUND",
                ));
            };
            self.captured_blobs
                .borrow_mut()
                .insert(url.to_string(), bytes);
        }
        Ok(())
    }

    fn request(
        &self,
        referrer: &Referrer,
        request: &ModuleRequest,
        context: &mut Context,
    ) -> JsResult<Module> {
        let url = self.resolve(
            &request.specifier().to_std_string_escaped(),
            referrer.path(),
            Mode::Import,
            true,
            context,
        )?;
        self.get(&url, Some(request), context)
    }

    pub(crate) fn insert_source(
        &self,
        filename: &Path,
        source: &str,
        context: &mut Context,
    ) -> JsResult<Url> {
        let text = filename.to_string_lossy();
        let url = match Url::parse(&text) {
            Ok(url) if matches!(url.scheme(), "file" | "data" | "blob") => url,
            _ => resolve::file_url(&absolute(filename, context))
                .map_err(|e| resolve_error(context, e))?,
        };
        let url = if url.scheme() == "file"
            && resolve::file_path(&url).is_ok_and(|path| path.is_file())
        {
            resolve::finalize(url).map_err(|e| resolve_error(context, e))?
        } else {
            url
        };
        self.sources
            .borrow_mut()
            .insert(url.to_string(), source.to_owned());
        Ok(url)
    }

    fn source(&self, url: &Url, context: &mut Context) -> JsResult<(String, bool)> {
        if let Some(source) = self.sources.borrow().get(url.as_str()) {
            return Ok((source.clone(), url.path().ends_with(".json")));
        }
        match url.scheme() {
            "file" => {
                let path = resolve::file_path(url).map_err(|e| resolve_error(context, e))?;
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| !matches!(ext, "js" | "mjs" | "cjs" | "json"))
                {
                    return Err(coded_error(
                        context,
                        &format!("Unknown file extension for {}", path.display()),
                        "ERR_UNKNOWN_FILE_EXTENSION",
                    ));
                }
                let text = std::fs::read_to_string(&path).map_err(|e| {
                    coded_error(
                        context,
                        &format!("{}: {e}", path.display()),
                        "ERR_MODULE_NOT_FOUND",
                    )
                })?;
                Ok((text, path.extension().is_some_and(|ext| ext == "json")))
            }
            "data" => data_source(url, context),
            "blob" => {
                let bytes = self
                    .captured_blobs
                    .borrow()
                    .get(url.as_str())
                    .cloned()
                    .or_else(|| {
                        crate::shared(context)
                            .blobs
                            .borrow()
                            .get(url.as_str())
                            .cloned()
                    });
                let bytes = bytes.ok_or_else(|| {
                    coded_error(
                        context,
                        &format!("Cannot find module '{url}'"),
                        "ERR_MODULE_NOT_FOUND",
                    )
                })?;
                let text = String::from_utf8(bytes).map_err(|_| {
                    coded_error(
                        context,
                        "Module source is not UTF-8",
                        "ERR_INVALID_MODULE_SPECIFIER",
                    )
                })?;
                Ok((text, false))
            }
            _ => Err(coded_error(
                context,
                "Unsupported ESM URL",
                "ERR_UNSUPPORTED_ESM_URL_SCHEME",
            )),
        }
    }

    fn get(
        &self,
        url: &Url,
        request: Option<&ModuleRequest>,
        context: &mut Context,
    ) -> JsResult<Module> {
        if let Some(module) = self.modules.borrow().get(url.as_str()).cloned() {
            let json = self
                .formats
                .borrow()
                .get(url.as_str())
                .copied()
                .unwrap_or(false);
            attributes(request, json, context)?;
            return Ok(module);
        }
        let is_builtin = url.scheme() == "node";
        let (source, json) = if is_builtin {
            (String::new(), false)
        } else {
            self.source(url, context)?
        };
        attributes(request, json, context)?;
        self.formats.borrow_mut().insert(url.to_string(), json);
        let module = if is_builtin {
            let exports = crate::modules::builtin(context, url.as_str())?.ok_or_else(|| {
                coded_error(
                    context,
                    "Unknown builtin module",
                    "ERR_UNKNOWN_BUILTIN_MODULE",
                )
            })?;
            let mut names: Vec<_> = match exports.as_object() {
                Some(object) => object
                    .own_property_keys(context)?
                    .into_iter()
                    .filter_map(|key| match key {
                        PropertyKey::String(name)
                            if !matches!(
                                name.to_std_string_escaped().as_str(),
                                "name"
                                    | "length"
                                    | "prototype"
                                    | "caller"
                                    | "arguments"
                                    | "default"
                            ) =>
                        {
                            Some(name)
                        }
                        _ => None,
                    })
                    .collect(),
                None => Vec::new(),
            };
            names.sort_by_key(JsString::to_std_string_escaped);
            names.dedup();
            self.builtin_names
                .borrow_mut()
                .insert(url.to_string(), names.clone());
            synthetic(url, names, true, context)
        } else if json {
            let value =
                if url.scheme() == "file" && url.query().is_none() && url.fragment().is_none() {
                    let path = resolve::file_path(url).map_err(|e| resolve_error(context, e))?;
                    crate::modules::load_file(context, &path, None)?
                } else {
                    let parse = context
                        .intrinsics()
                        .objects()
                        .json()
                        .get(js_string!("parse"), context)?;
                    crate::call(
                        context,
                        &parse,
                        &JsValue::undefined(),
                        &[js_text(source.trim_start_matches('\u{feff}'))],
                    )?
                };
            Module::from_value_as_default(value, context)
        } else if url.scheme() == "file"
            && !self.sources.borrow().contains_key(url.as_str())
            && !is_esm(
                &resolve::file_path(url).map_err(|e| resolve_error(context, e))?,
                &source,
                context,
            )?
        {
            let path = resolve::file_path(url).map_err(|e| resolve_error(context, e))?;
            let mut names = BTreeSet::new();
            cjs_names(&path, &source, &mut HashSet::new(), &mut names, context)?;
            synthetic(
                url,
                names.into_iter().map(JsString::from).collect(),
                false,
                context,
            )
        } else {
            let path = PathBuf::from(url.as_str());
            Module::parse(
                Source::from_bytes(source.trim_start_matches('\u{feff}')).with_path(&path),
                None,
                context,
            )?
        };
        self.modules
            .borrow_mut()
            .insert(url.to_string(), module.clone());
        self.captured_blobs.borrow_mut().remove(url.as_str());
        Ok(module)
    }

    pub(crate) fn start(&self, url: &Url, context: &mut Context) -> JsResult<JsPromise> {
        let module = self.get(url, None, context)?;
        let evaluated = module.load_link_evaluate(context);
        let fulfill = closure(
            context,
            "",
            0,
            |_, _, module: &Module, context| Ok(module.namespace(context).into()),
            module,
        );
        evaluated.then(JsFunction::from_object(fulfill), None, context)
    }

    pub(crate) fn require(&self, path: &Path, context: &mut Context) -> JsResult<JsValue> {
        let url =
            resolve::file_url(&absolute(path, context)).map_err(|e| resolve_error(context, e))?;
        let url = if self.sources.borrow().contains_key(url.as_str()) {
            url
        } else {
            resolve::finalize(url).map_err(|e| resolve_error(context, e))?
        };
        let key = url.to_string();
        if !self.requiring.borrow_mut().insert(key.clone()) {
            return Err(coded_error(
                context,
                "Cannot require() an ES module in a cycle",
                "ERR_REQUIRE_CYCLE_MODULE",
            ));
        }
        let result = (|| {
            let module = self.get(&url, None, context)?;
            module.load_link_evaluate_sync(context).map_err(|error| {
                let message = error.to_string();
                if message.contains("top-level await") {
                    coded_error(context, "require() cannot be used on an ESM graph with top-level await. Use import() instead.", "ERR_REQUIRE_ASYNC_MODULE")
                } else if message.contains("already evaluating") {
                    coded_error(context, "Cannot require() an ES module in a cycle", "ERR_REQUIRE_CYCLE_MODULE")
                } else if message.contains("did not complete synchronously") {
                    coded_error(context, "The ESM graph did not load synchronously", "ERR_REQUIRE_ASYNC_MODULE")
                } else { error }
            })?;
            let namespace = module.namespace(context);
            if namespace.has_property(js_string!("module.exports"), context)? {
                return namespace.get(js_string!("module.exports"), context);
            }
            if namespace.has_property(js_string!("default"), context)?
                && !namespace.has_property(js_string!("__esModule"), context)?
            {
                if let Some(namespace) = self.require_namespaces.borrow().get(&key) {
                    return Ok(namespace.clone().into());
                }
                let namespace = module.namespace_with_marker(
                    js_string!("__esModule"),
                    JsValue::from(true),
                    context,
                );
                self.require_namespaces
                    .borrow_mut()
                    .insert(key.clone(), namespace.clone());
                return Ok(namespace.into());
            }
            // The namespace stays live; a default export never unwraps automatically.
            Ok(namespace.into())
        })();
        self.requiring.borrow_mut().remove(&key);
        result
    }

    pub(crate) fn sync_builtins(&self, context: &mut Context) -> JsResult<()> {
        let names = self.builtin_names.borrow().clone();
        for (url, names) in names {
            let module = self.modules.borrow().get(&url).cloned();
            let Some(module) = module else { continue };
            let Some(exports) = crate::modules::builtin(context, &url)? else {
                continue;
            };
            for name in names {
                let value = match exports.as_object() {
                    Some(object) => object.get(name.clone(), context)?,
                    None => JsValue::undefined(),
                };
                module.set_synthetic_export(&name, value)?;
            }
        }
        Ok(())
    }

    pub(crate) fn forget(&self, filename: &Path, context: &mut Context) {
        let text = filename.to_string_lossy();
        let url = Url::parse(&text)
            .ok()
            .filter(|u| matches!(u.scheme(), "file" | "data" | "blob" | "node"))
            .or_else(|| resolve::file_url(&absolute(filename, context)).ok());
        if let Some(url) = url {
            let canonical = resolve::finalize(url.clone()).unwrap_or(url.clone());
            for key in [url.as_str(), canonical.as_str()] {
                self.modules.borrow_mut().remove(key);
                self.sources.borrow_mut().remove(key);
                self.formats.borrow_mut().remove(key);
                self.require_namespaces.borrow_mut().remove(key);
                self.captured_blobs.borrow_mut().remove(key);
            }
        }
    }
}

impl ModuleLoader for Loader {
    fn prepare_import(
        self: Rc<Self>,
        referrer: &Referrer,
        request: &ModuleRequest,
        context: &mut Context,
    ) -> JsResult<()> {
        self.prepare(referrer, request, context)
    }

    fn load_imported_module(
        self: Rc<Self>,
        referrer: Referrer,
        request: ModuleRequest,
        context: &RefCell<&mut Context>,
    ) -> impl std::future::Future<Output = JsResult<Module>> {
        ready(self.request(&referrer, &request, &mut context.borrow_mut()))
    }

    fn init_import_meta(self: Rc<Self>, meta: &JsObject, module: &Module, context: &mut Context) {
        let Some(path) = module.path() else { return };
        let Ok(url) = self.parent(Some(path), context) else {
            return;
        };
        let _ = meta.set(js_string!("url"), js_text(url.as_str()), false, context);
        if url.scheme() == "file"
            && let Ok(path) = resolve::file_path(&url)
        {
            let _ = meta.set(
                js_string!("filename"),
                js_text(&path.to_string_lossy()),
                false,
                context,
            );
            let _ = meta.set(
                js_string!("dirname"),
                js_text(&path.parent().unwrap_or(Path::new("")).to_string_lossy()),
                false,
                context,
            );
        }
        let resolve = closure(
            context,
            "resolve",
            1,
            |_, args, parent: &JsString, context| {
                let name = text_of(&arg(args, 0), context)?;
                let loader = crate::shared(context).esm.clone();
                let url = loader.resolve(
                    &name,
                    Some(Path::new(&parent.to_std_string_escaped())),
                    Mode::Import,
                    false,
                    context,
                )?;
                Ok(js_text(url.as_str()))
            },
            JsString::from(url.as_str()),
        );
        let _ = meta.set(js_string!("resolve"), resolve, false, context);
    }
}

pub(crate) fn absolute(path: &Path, context: &Context) -> PathBuf {
    if path.is_absolute() {
        crate::modules::clean(path)
    } else {
        crate::modules::clean(
            &PathBuf::from(crate::shared(context).cwd.borrow().clone()).join(path),
        )
    }
}

pub(crate) fn is_esm(path: &Path, source: &str, context: &mut Context) -> JsResult<bool> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("mjs") => return Ok(true),
        Some("cjs" | "json" | "node") => return Ok(false),
        Some("js") | None => {}
        _ => {
            return Err(coded_error(
                context,
                &format!("Unknown file extension for {}", path.display()),
                "ERR_UNKNOWN_FILE_EXTENSION",
            ));
        }
    }
    if let Some((_, json)) = resolve::scope(path).map_err(|e| resolve_error(context, e))? {
        match json.get("type").and_then(serde_json::Value::as_str) {
            Some("module") => return Ok(true),
            Some("commonjs") => return Ok(false),
            _ => {}
        }
    }
    let source = source.trim_start_matches('\u{feff}');
    let source = if source.starts_with("#!") {
        source.find('\n').map_or("", |n| &source[n..])
    } else {
        source
    };
    // Parsing the real CJS wrapper also detects lexical redeclarations of its parameters.
    let wrapper = format!("(function(exports,require,module,__filename,__dirname){{{source}\n}})");
    if Script::parse(Source::from_bytes(&wrapper), None, context).is_ok() {
        return Ok(false);
    }
    Ok(Module::parse(Source::from_bytes(source), None, context).is_ok())
}

fn attributes(request: Option<&ModuleRequest>, json: bool, context: &mut Context) -> JsResult<()> {
    let mut kind = None;
    if let Some(request) = request {
        for attr in request.attributes() {
            if attr.key() != &js_string!("type") {
                return Err(coded_error(
                    context,
                    &format!(
                        "Import attribute '{}' is not supported",
                        attr.key().to_std_string_escaped()
                    ),
                    "ERR_IMPORT_ATTRIBUTE_UNSUPPORTED",
                ));
            }
            kind = Some(attr.value().to_std_string_escaped());
        }
    }
    match (json, kind.as_deref()) {
        (true, Some("json")) | (false, None) => Ok(()),
        (true, None) => Err(coded_error(
            context,
            "JSON modules require an import attribute of type json",
            "ERR_IMPORT_ATTRIBUTE_MISSING",
        )),
        (false, Some("json")) => Err(coded_error(
            context,
            "Module is not of type json",
            "ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE",
        )),
        (_, Some(_)) => Err(coded_error(
            context,
            "Unsupported import attribute type",
            "ERR_IMPORT_ATTRIBUTE_UNSUPPORTED",
        )),
    }
}

fn synthetic(url: &Url, mut names: Vec<JsString>, builtin: bool, context: &mut Context) -> Module {
    names.retain(|n| n != &js_string!("default"));
    names.push(js_string!("default"));
    let initializer = SyntheticModuleInitializer::from_copy_closure_with_captures(
        |module, (url, names, builtin): &(JsString, Vec<JsString>, bool), context| {
            let url = url.to_std_string_escaped();
            let exports = if *builtin {
                crate::modules::builtin(context, &url)?.ok_or_else(|| {
                    coded_error(context, "Unknown builtin", "ERR_UNKNOWN_BUILTIN_MODULE")
                })?
            } else {
                let url = Url::parse(&url)
                    .map_err(|_| coded_error(context, "Invalid module URL", "ERR_INVALID_URL"))?;
                let path = resolve::file_path(&url).map_err(|e| resolve_error(context, e))?;
                crate::modules::import_cjs(context, &path)?
            };
            for name in names {
                let value = if name == &js_string!("default") {
                    exports.clone()
                } else {
                    match exports.as_object() {
                        Some(object) => object.get(name.clone(), context)?,
                        None => JsValue::undefined(),
                    }
                };
                module.set_export(name, value)?;
            }
            Ok(())
        },
        (JsString::from(url.as_str()), names.clone(), builtin),
    );
    Module::synthetic(
        &names,
        initializer,
        Some(PathBuf::from(url.as_str())),
        None,
        context,
    )
}

fn cjs_names(
    path: &Path,
    source: &str,
    seen: &mut HashSet<PathBuf>,
    names: &mut BTreeSet<String>,
    context: &mut Context,
) -> JsResult<()> {
    let mut pending = vec![(path.to_path_buf(), source.to_owned())];
    while let Some((path, source)) = pending.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let found = crate::cjs_exports::analyze(&source);
        names.extend(found.names);
        for name in found.reexports {
            let loader = crate::shared(context).esm.clone();
            if let Ok(url) = loader.resolve(&name, Some(&path), Mode::Require, true, context)
                && let Ok(path) = resolve::file_path(&url)
                && !seen.contains(&path)
                && let Ok(source) = std::fs::read_to_string(&path)
            {
                pending.push((path, source));
            }
        }
    }
    names.remove("default");
    Ok(())
}

fn data_source(url: &Url, context: &mut Context) -> JsResult<(String, bool)> {
    let Some((header, body)) = url.path().split_once(',') else {
        return Err(coded_error(
            context,
            "Malformed data URL",
            "ERR_INVALID_URL",
        ));
    };
    let mime = header.split(';').next().unwrap_or("").to_ascii_lowercase();
    let json = mime == "application/json";
    if !json && !matches!(mime.as_str(), "text/javascript" | "application/javascript") {
        return Err(coded_error(
            context,
            &format!("Unknown module format: {mime}"),
            "ERR_UNKNOWN_MODULE_FORMAT",
        ));
    }
    let decoded = resolve::percent_decode(body).map_err(|e| resolve_error(context, e))?;
    let source = if header.split(';').any(|s| s.eq_ignore_ascii_case("base64")) {
        let bytes = decode_base64(&decoded)
            .ok_or_else(|| coded_error(context, "Invalid base64 data URL", "ERR_INVALID_URL"))?;
        String::from_utf8(bytes)
            .map_err(|_| coded_error(context, "Module source is not UTF-8", "ERR_INVALID_URL"))?
    } else {
        decoded
    };
    Ok((source, json))
}

fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut bits = 0u32;
    let mut count = 0u32;
    for byte in text.bytes().filter(|b| !b.is_ascii_whitespace()) {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(digit);
        count += 6;
        if count >= 8 {
            count -= 8;
            output.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(output)
}
