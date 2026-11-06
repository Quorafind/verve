//! CommonJS, as Node has it: `require` and how it finds what it is asked for, `module`,
//! `require.cache`, `require.resolve`, and `require('module')`.
//!
//! A name is looked for, in order:
//!
//! 1. Among the built-in modules — what the host defined ([`crate::Runtime::define_builtin`]),
//!    then Node's own ([`crate::node::MODULES`], each made the first time it is asked for), then
//!    the host's families. Each answers to its bare name and to `node:` before it
//!    (`require('node:crypto')`), and to a stray `/` after it (`require('string_decoder/')`, as
//!    `readable-stream` writes it).
//! 2. As a file, when it starts with `./`, `../` or `/` (or a drive): the file itself, then with
//!    `.js`, `.json` or `.cjs` after it, then as a folder — its `package.json`'s `main`, or its
//!    `index.js`.
//! 3. In a `node_modules` folder, in the module's own folder and each one above it.
//!
//! What has run is kept in `require.cache`, one module object for each file, under its path:
//! a second `require` answers the same `module.exports`, and deleting the entry makes the next
//! `require` run the file afresh, as in Node.

use std::path::{Path, PathBuf};

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Module, Source,
    js_string, object::builtins::JsArray, script::Script,
};

use crate::fs::sys;
use crate::native::{
    ClassSpec, arg, array, class, closure, function, get, hidden, js_text, object, text_of,
};
use crate::vfs::View;

/// What a name names: a built-in module, or a file.
enum Resolved {
    Builtin(String),
    File(PathBuf),
}

/// The name Node's built-in module answers to, as `require` was given it: `node:` and a
/// trailing `/` taken off.
pub(crate) fn bare_name(name: &str) -> &str {
    let bare = name.strip_prefix("node:").unwrap_or(name);
    bare.strip_suffix('/').unwrap_or(bare)
}

/// Whether Node's table answers `name`: every module of it does to its bare name, but those
/// Node only has under `node:` (`node:sqlite`, `node:test`) do not — a bare `sqlite` is a package.
fn node_table_answers(name: &str) -> bool {
    name.starts_with("node:") || !crate::node::PREFIX_ONLY.contains(&bare_name(name))
}

/// Whether `name` is a built-in module, without making it.
pub(crate) fn is_builtin(context: &Context, name: &str) -> bool {
    let bare = bare_name(name);
    let state = crate::shared(context);
    state.builtins.borrow().contains_key(bare)
        || state.lazy.borrow().contains_key(bare)
        || (node_table_answers(name) && crate::node::MODULES.iter().any(|(n, _)| *n == bare))
        || state
            .families
            .borrow()
            .iter()
            .any(|(prefix, _)| bare.starts_with(prefix.as_str()))
}

/// The key `bare` is kept under. `events` is made for each world, its `EventEmitter` among that
/// world's own built-ins, so that what extends it (a `process`, an `ipcRenderer`) does not reach
/// the page's `Object.prototype` through it.
fn per_world(bare: &str, context: &Context) -> String {
    match crate::current_world(context) {
        crate::MAIN_WORLD => bare.to_owned(),
        world if bare == "events" => format!("{bare}:{world}"),
        _ => bare.to_owned(),
    }
}

/// Forgets what was made per world for `world`, which is gone: a world made later with its id must
/// not find it.
pub(crate) fn forget_world(context: &Context, world: crate::WorldId) {
    let key = format!("events:{world}");
    crate::shared(context).builtins.borrow_mut().remove(&key);
}

/// The built-in module `name`, made now if it has not been yet; `None` when there is none.
pub(crate) fn builtin(context: &mut Context, name: &str) -> JsResult<Option<JsValue>> {
    let bare = bare_name(name);
    let scoped = per_world(bare, context);
    if let Some(exports) = crate::shared(context).builtins.borrow().get(&scoped) {
        return Ok(Some(exports.clone()));
    }
    // One the host said to make on first use: made now, and kept with the rest.
    let lazy = crate::shared(context).lazy.borrow().get(bare).copied();
    if let Some(make) = lazy {
        let exports = make(context, bare)?;
        crate::shared(context)
            .builtins
            .borrow_mut()
            .insert(bare.to_owned(), exports.clone());
        return Ok(Some(exports));
    }
    // One Node has only under `node:` is kept under that name, so that making it does not make
    // a bare `require('sqlite')` find it.
    let key = if crate::node::PREFIX_ONLY.contains(&bare) {
        format!("node:{bare}")
    } else {
        per_world(bare, context)
    };
    if let Some(exports) = crate::shared(context)
        .builtins
        .borrow()
        .get(&key)
        .filter(|_| node_table_answers(name))
    {
        return Ok(Some(exports.clone()));
    }
    let maker = crate::node::MODULES
        .iter()
        .filter(|_| node_table_answers(name))
        .find(|(n, _)| *n == bare)
        .map(|(_, make)| *make)
        .or_else(|| {
            crate::shared(context)
                .families
                .borrow()
                .iter()
                .find(|(prefix, _)| bare.starts_with(prefix.as_str()))
                .map(|(_, make)| *make)
        });
    let Some(make) = maker else { return Ok(None) };
    let exports = make(context, bare)?;
    crate::shared(context)
        .builtins
        .borrow_mut()
        .insert(key, exports.clone());
    Ok(Some(exports))
}

/// The built-in module `name`, which the runtime itself defines: for one module reaching
/// another (`stream` reaching `events`).
pub(crate) fn node_module(context: &mut Context, name: &str) -> JsResult<JsValue> {
    builtin(context, name)?.ok_or_else(|| type_error_not_found(name))
}

fn type_error_not_found(name: &str) -> JsError {
    JsNativeError::error()
        .with_message(format!("Cannot find module '{name}'"))
        .into()
}

fn not_found(context: &mut Context, name: &str) -> JsError {
    let error = JsNativeError::error()
        .with_message(format!("Cannot find module '{name}'"))
        .into_opaque(context);
    let _ = error.set(
        js_string!("code"),
        js_string!("MODULE_NOT_FOUND"),
        false,
        context,
    );
    JsError::from_opaque(error.into())
}

/// `path` with `.` and `..` worked out and its separators made the system's, so a file has one
/// key in the cache however it was named — `C:/vault/a.js`, `C:\vault\lib\..\a.js`.
pub(crate) fn clean(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn path_text(path: &Path) -> JsValue {
    js_text(&path.to_string_lossy())
}

/// The file `base` names: itself, with an extension added, or as a folder.
fn as_file_or_folder(view: &View, base: &Path) -> Option<PathBuf> {
    if sys::is_file(view, base) {
        return Some(base.to_path_buf());
    }
    for ext in ["js", "json", "cjs"] {
        let path = PathBuf::from(format!("{}.{ext}", base.display()));
        if sys::is_file(view, &path) {
            return Some(path);
        }
    }
    if sys::is_dir(view, base) {
        let manifest = base.join("package.json");
        if crate::permission::resolution_may_read(&manifest)
            && let Ok(text) = sys::read_to_string(view, &manifest)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        {
            let main = json.get("main").and_then(|m| m.as_str());
            if let Some(main) = main {
                // `main` is written as the package's author wrote it: `./main.js`, `lib/x.js`.
                let target = clean(&base.join(main));
                if let Some(found) = as_file_or_folder_plain(view, &target) {
                    return Some(found);
                }
            }
        }
        for index in ["index.js", "index.json", "index.cjs"] {
            let path = base.join(index);
            if sys::is_file(view, &path) {
                return Some(path);
            }
        }
    }
    None
}

/// A `main` named in a `package.json`: the file, with an extension added, or its folder's
/// `index.js` — but not that folder's own `package.json` again.
fn as_file_or_folder_plain(view: &View, base: &Path) -> Option<PathBuf> {
    if sys::is_file(view, base) {
        return Some(base.to_path_buf());
    }
    for ext in ["js", "json", "cjs"] {
        let path = PathBuf::from(format!("{}.{ext}", base.display()));
        if sys::is_file(view, &path) {
            return Some(path);
        }
    }
    let index = base.join("index.js");
    sys::is_file(view, &index).then_some(index)
}

fn is_path(name: &str) -> bool {
    name.starts_with("./")
        || name.starts_with("../")
        || name.starts_with('/')
        || name.starts_with(".\\")
        || name.starts_with("..\\")
        || name == "."
        || name == ".."
        || Path::new(name).is_absolute()
}

/// What `name`, required from a module in `folder`, names.
fn resolve(context: &mut Context, folder: &Path, name: &str) -> JsResult<Resolved> {
    let _resolving = crate::permission::Resolving::enter(context);
    if is_builtin(context, name) && !is_path(name) {
        return Ok(Resolved::Builtin(name.to_owned()));
    }
    if is_path(name) {
        return as_file_or_folder(&View::of(context), &clean(&folder.join(name)))
            .map(Resolved::File)
            .ok_or_else(|| not_found(context, name));
    }
    let loader = crate::shared(context).esm.clone();
    let url = loader
        .resolve(
            name,
            Some(&folder.join("__require__.cjs")),
            crate::esm_resolve::Mode::Require,
            true,
            context,
        )
        .map_err(|error| {
            // CJS preserves its historical missing-module code; package encapsulation errors
            // retain their ERR_PACKAGE_* identities shared with ESM.
            let opaque = error.clone().into_opaque(context).unwrap_or_default();
            if opaque
                .as_object()
                .and_then(|o| o.get(js_string!("code"), context).ok())
                .is_some_and(|c| crate::print(&c) == "ERR_MODULE_NOT_FOUND")
            {
                not_found(context, name)
            } else {
                error
            }
        })?;
    if url.scheme() == "node" {
        return Ok(Resolved::Builtin(url.to_string()));
    }
    if url.scheme() != "file" {
        return Err(not_found(context, name));
    }
    let path = crate::esm_resolve::file_path(&url)
        .map_err(|error| crate::esm::resolve_error(context, error))?;
    Ok(Resolved::File(path))
}

/// The folders a package is looked for in from `folder`: `require.resolve.paths`, and
/// `module.paths`.
fn lookup_paths(folder: &Path) -> Vec<JsValue> {
    let mut out = Vec::new();
    let mut at = Some(folder);
    while let Some(dir) = at {
        if dir.file_name().is_some_and(|n| n != "node_modules") || dir.parent().is_none() {
            out.push(path_text(&dir.join("node_modules")));
        }
        at = dir.parent();
    }
    out
}

/// `require(name)` from a module in `folder`; `parent` is that module, when there is one.
pub(crate) fn require(
    context: &mut Context,
    folder: &Path,
    name: &str,
    parent: Option<&JsObject>,
) -> JsResult<JsValue> {
    match resolve(context, folder, name)? {
        Resolved::Builtin(name) => {
            builtin(context, &name)?.ok_or_else(|| not_found(context, &name))
        }
        Resolved::File(path) => load_file(context, &path, parent),
    }
}

/// The cache: `require.cache`, the module objects of every file run, by path.
fn cache(context: &Context) -> JsObject {
    crate::shared(context).module_cache.clone()
}

/// A file required: answered from the cache, or read and run (or, a `.json`, parsed; a
/// `.node` addon, refused).
pub(crate) fn load_file(
    context: &mut Context,
    path: &Path,
    parent: Option<&JsObject>,
) -> JsResult<JsValue> {
    let key = JsString::from(path.to_string_lossy().as_ref());
    let cached = cache(context).get(key.clone(), context)?;
    if cached.is_object() {
        return get(&cached, "exports", context);
    }
    if path.extension().is_some_and(|ext| ext == "node") {
        // What Node says under `--no-addons`: there is no native code to load
        // here, and a package that falls back from its addon checks this code.
        return Err(crate::native::coded_error(
            context,
            "Cannot load native addon because loading addons is disabled.",
            "ERR_DLOPEN_DISABLED",
        ));
    }
    crate::permission::read(context, path)?;
    let view = View::of(context);
    let text = sys::read_to_string(&view, path).map_err(|error| {
        JsNativeError::error().with_message(format!("{}: {error}", path.display()))
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    if path.extension().is_some_and(|ext| ext == "json") {
        let module = module_object(context, path, parent)?;
        let parse = context
            .intrinsics()
            .objects()
            .json()
            .get(js_string!("parse"), context)?;
        let value = crate::call(context, &parse, &JsValue::undefined(), &[js_text(text)]).map_err(
            |error| {
                JsNativeError::syntax().with_message(format!(
                    "{}: {}",
                    path.display(),
                    crate::describe(context, &error)
                ))
            },
        )?;
        module.set(js_string!("exports"), value.clone(), false, context)?;
        module.set(js_string!("loaded"), true, false, context)?;
        cache(context).set(key, module, false, context)?;
        return Ok(value);
    }
    if matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("mjs" | "js") | None
    ) {
        match format(path, text, context)? {
            Format::Module => {
                let loader = crate::shared(context).esm.clone();
                return loader.require(path, context);
            }
            Format::CommonJs(script) => {
                return run_module_with(context, path, text, parent, script);
            }
        }
    }
    run_module(context, path, text, parent)
}

/// What a file is: a CommonJS module — its wrapper compiled, when the source is what decided
/// it — or a module.
pub(crate) enum Format {
    CommonJs(Option<Script>),
    Module,
}

/// What the file at `path`, whose source is `source`, is: the one its extension or its
/// package's `type` names, or, when neither names one, Node's CommonJS wrapper when the source
/// parses as one and a module otherwise. That parse is the one the module runs from, so nothing
/// parses the source twice.
///
/// A source that is neither answers CommonJS, as it always has, without its wrapper: refusing it
/// here would fail the load, and so a whole graph, before the code before it runs. Run as a
/// CommonJS module, it is parsed again and fails where Node fails it — at the run.
pub(crate) fn format(path: &Path, source: &str, context: &mut Context) -> JsResult<Format> {
    let loader = crate::shared(context).esm.clone();
    if let Some(script) = loader.take_cjs_wrapper(path, source) {
        return Ok(Format::CommonJs(Some(script)));
    }
    match crate::esm::named_format(path, context)? {
        Some(true) => return Ok(Format::Module),
        Some(false) => return Ok(Format::CommonJs(None)),
        None => {}
    }
    match crate::code_cache_load::script(&wrapper(source), &source_path_of(&clean(path)), context) {
        Ok(script) => Ok(Format::CommonJs(Some(script))),
        Err(_) if Module::parse(Source::from_bytes(source), None, context).is_ok() => {
            Ok(Format::Module)
        }
        Err(_) => Ok(Format::CommonJs(None)),
    }
}

/// The path a module's code is compiled against: its URL, as Node names it.
fn source_path_of(path: &Path) -> PathBuf {
    crate::esm_resolve::file_url(path)
        .map(|url| PathBuf::from(url.as_str()))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// The CJS side of a synchronous CJS -> ESM -> CJS cycle cannot expose partial exports.
pub(crate) fn import_cjs(context: &mut Context, path: &Path) -> JsResult<JsValue> {
    let key = JsString::from(path.to_string_lossy().as_ref());
    let cached = cache(context).get(key, context)?;
    if cached.is_object()
        && !get(&cached, "loaded", context)?.to_boolean()
        && crate::shared(context).esm.is_requiring()
    {
        return Err(crate::native::coded_error(
            context,
            "Cannot import CommonJS in a require(esm) cycle",
            "ERR_REQUIRE_CYCLE_MODULE",
        ));
    }
    load_file(context, path, None)
}

/// The prototype of every module object: `require('module').prototype`.
fn module_prototype(context: &mut Context) -> JsResult<JsObject> {
    let module = node_module_class(context)?;
    module
        .get(js_string!("prototype"), context)?
        .as_object()
        .ok_or_else(|| type_error_not_found("module"))
}

/// The `Module` class, which `require('module')` answers.
fn node_module_class(context: &mut Context) -> JsResult<JsObject> {
    node_module(context, "module")?
        .as_object()
        .ok_or_else(|| type_error_not_found("module"))
}

/// A module object for the file at `path`: `id`, `filename`, `path`, `exports`, `loaded`,
/// `parent`, `children`, `paths`, and its own `require`.
fn module_object(
    context: &mut Context,
    path: &Path,
    parent: Option<&JsObject>,
) -> JsResult<JsObject> {
    let folder = path.parent().unwrap_or(Path::new("")).to_path_buf();
    let module = object(context);
    if let Ok(prototype) = module_prototype(context) {
        module.set_prototype(Some(prototype));
    }
    let exports = object(context);
    module.set(js_string!("id"), path_text(path), false, context)?;
    module.set(js_string!("filename"), path_text(path), false, context)?;
    module.set(js_string!("path"), path_text(&folder), false, context)?;
    module.set(js_string!("exports"), exports, false, context)?;
    module.set(js_string!("loaded"), false, false, context)?;
    let children = JsArray::new(context)?;
    module.set(js_string!("children"), children, false, context)?;
    let paths = array(context, lookup_paths(&folder));
    module.set(js_string!("paths"), paths, false, context)?;
    match parent {
        Some(parent) => {
            module.set(js_string!("parent"), parent.clone(), false, context)?;
            if let Some(children) = parent.get(js_string!("children"), context)?.as_object()
                && let Ok(children) = JsArray::from_object(children.clone())
            {
                children.push(module.clone(), context)?;
            }
        }
        None => {
            module.set(js_string!("parent"), JsValue::null(), false, context)?;
        }
    }
    let require = require_function(context, &folder, Some(module.clone()))?;
    hidden(&module, js_string!("require"), require, context)?;
    Ok(module)
}

/// Runs `source` as the CommonJS module at `filename`, with Node's module wrapper, and answers
/// its `module.exports` — at once from the cache, when it has run already.
pub(crate) fn run_module(
    context: &mut Context,
    filename: &Path,
    source: &str,
    parent: Option<&JsObject>,
) -> JsResult<JsValue> {
    run_module_with(context, filename, source, parent, None)
}

/// As [`run_module`], with the module's wrapper compiled already, when the loader has it.
pub(crate) fn run_module_with(
    context: &mut Context,
    filename: &Path,
    source: &str,
    parent: Option<&JsObject>,
    script: Option<Script>,
) -> JsResult<JsValue> {
    let filename = &clean(filename);
    let key = JsString::from(filename.to_string_lossy().as_ref());
    let cached = cache(context).get(key.clone(), context)?;
    if cached.is_object() {
        return get(&cached, "exports", context);
    }
    let folder = filename.parent().unwrap_or(Path::new("")).to_path_buf();
    let wrapper = match script {
        Some(script) => script.evaluate(context)?,
        None => {
            crate::code_cache_load::script(&wrapper(source), &source_path_of(filename), context)?
                .evaluate(context)?
        }
    };
    let module = module_object(context, filename, parent)?;
    let exports = module.get(js_string!("exports"), context)?;
    let require = module.get(js_string!("require"), context)?;
    // In the cache before it runs, as Node does, so two modules that require each other finish.
    cache(context).set(key.clone(), module.clone(), false, context)?;
    let ran = crate::call(
        context,
        &wrapper,
        &exports,
        &[
            exports.clone(),
            require,
            module.clone().into(),
            path_text(filename),
            path_text(&folder),
        ],
    );
    if let Err(error) = ran {
        cache(context).delete_property_or_throw(key, context)?;
        return Err(error);
    }
    module.set(js_string!("loaded"), true, false, context)?;
    module.get(js_string!("exports"), context)
}

/// Forgets the module run from `filename`, so the next `require` of it runs it afresh.
pub(crate) fn forget(context: &mut Context, filename: &Path) {
    let path = url::Url::parse(&filename.to_string_lossy())
        .ok()
        .filter(|url| url.scheme() == "file")
        .and_then(|url| crate::esm_resolve::file_path(&url).ok())
        .unwrap_or_else(|| crate::esm::absolute(filename, context));
    let real = sys::realpath(&View::of(context), &path).ok();
    for path in [Some(path), real].into_iter().flatten() {
        let key = JsString::from(clean(&path).to_string_lossy().as_ref());
        let _ = cache(context).delete_property_or_throw(key, context);
    }
    let loader = crate::shared(context).esm.clone();
    loader.forget(filename, context);
}

/// The folder a `require` resolves relative names against, kept as text in the functions that
/// need it.
fn folder_of(captured: &JsValue) -> PathBuf {
    PathBuf::from(crate::print(captured))
}

/// Node's own module wrapper, word for word, around `source`: the script a CommonJS module is.
/// Its first line stays the file's, so a line number in an error is the line in the file. A byte
/// order mark and a `#!` line, which Node reads as nothing, are left out.
pub(crate) fn wrapper(source: &str) -> String {
    let source = source.trim_start_matches('\u{feff}');
    let source = if source.starts_with("#!") {
        source.find('\n').map_or("", |at| &source[at..])
    } else {
        source
    };
    format!("(function (exports, require, module, __filename, __dirname) {{ {source}\n}})")
}

/// A module's own `require`, resolving relative names against `folder`, with `resolve`
/// (and `resolve.paths`), `cache` and `main` on it.
pub(crate) fn require_function(
    context: &mut Context,
    folder: &Path,
    module: Option<JsObject>,
) -> JsResult<JsValue> {
    let folder = &clean(folder);
    let captured = (
        path_text(folder),
        module.map_or(JsValue::undefined(), JsValue::from),
    );
    let require = closure(
        context,
        "require",
        1,
        |_, args, (folder, module): &(JsValue, JsValue), context| {
            let name = arg(args, 0);
            if !name.is_string() {
                return Err(crate::native::coded_error(
                    context,
                    &format!(
                        "The \"id\" argument must be of type string. Received {}",
                        name.type_of()
                    ),
                    "ERR_INVALID_ARG_TYPE",
                ));
            }
            let name = text_of(&name, context)?;
            if name.is_empty() {
                return Err(crate::native::coded_error(
                    context,
                    "The argument 'id' must be a non-empty string. Received ''",
                    "ERR_INVALID_ARG_VALUE",
                ));
            }
            require(
                context,
                &folder_of(folder),
                &name,
                module.as_object().as_ref(),
            )
        },
        captured.clone(),
    );
    let resolve = closure(
        context,
        "resolve",
        2,
        |_, args, (folder, _): &(JsValue, JsValue), context| {
            let name = text_of(&arg(args, 0), context)?;
            // `{ paths: [...] }`: the folders to resolve from instead of the module's own.
            let mut folders = vec![folder_of(folder)];
            if let Some(paths) = get(&arg(args, 1), "paths", context)?.as_object() {
                folders = crate::native::list_of(&paths.into(), context)?
                    .iter()
                    .map(|p| PathBuf::from(crate::print(p)))
                    .collect();
            }
            let mut missing = None;
            for folder in folders {
                match resolve(context, &folder, &name) {
                    Ok(Resolved::Builtin(name)) => return Ok(js_text(&name)),
                    Ok(Resolved::File(path)) => return Ok(path_text(&path)),
                    Err(error) => missing = Some(error),
                }
            }
            Err(missing.unwrap_or_else(|| not_found(context, &name)))
        },
        captured.clone(),
    );
    let paths = closure(
        context,
        "paths",
        1,
        |_, args, (folder, _): &(JsValue, JsValue), context| {
            let name = text_of(&arg(args, 0), context)?;
            if is_builtin(context, &name) && !is_path(&name) {
                return Ok(JsValue::null());
            }
            Ok(array(context, lookup_paths(&folder_of(folder))))
        },
        captured,
    );
    resolve.set(js_string!("paths"), paths, false, context)?;
    require.set(js_string!("resolve"), resolve, false, context)?;
    let cache = cache(context);
    require.set(js_string!("cache"), cache, false, context)?;
    if crate::process::main_path(context).is_some() {
        // The entry module of a process: looked up when asked, since the entry's own
        // `require` is made before its module is in the cache.
        let main = function(context, "main", 0, |_, _, cx| {
            let Some(path) = crate::process::main_path(cx) else {
                return Ok(JsValue::undefined());
            };
            let key = JsString::from(clean(&path).to_string_lossy().as_ref());
            crate::shared(cx).module_cache.clone().get(key, cx)
        });
        require.define_property_or_throw(
            js_string!("main"),
            boa_engine::property::PropertyDescriptor::builder()
                .get(main)
                .enumerable(true)
                .configurable(true),
            context,
        )?;
    } else {
        require.set(js_string!("main"), JsValue::undefined(), false, context)?;
    }
    let extensions = extensions(context)?;
    require.set(js_string!("extensions"), extensions, false, context)?;
    Ok(require.into())
}

/// `require.extensions` / `Module._extensions`: what each kind of file is read as. Only
/// listed — code that reads it to know what is loadable finds `.js`, `.json` and `.cjs`.
fn extensions(context: &mut Context) -> JsResult<JsObject> {
    let extensions = object(context);
    for ext in [".js", ".json", ".cjs"] {
        let f = function(context, "", 2, |_, _, _| Ok(JsValue::undefined()));
        extensions.set(JsString::from(ext), f, false, context)?;
    }
    Ok(extensions)
}

/// A `require` for the file at `filename` (a path or a `file:` URL): `createRequire`.
fn create_require(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let given = arg(args, 0);
    let mut text = match given.as_object() {
        // A `URL`.
        Some(url) => text_of(&url.get(js_string!("href"), context)?, context)?,
        None => text_of(&given, context)?,
    };
    if let Some(rest) = text.strip_prefix("file://") {
        text = rest.to_owned();
        // `file:///C:/x` is `C:/x` on Windows.
        if text.get(2..3) == Some(":") && text.starts_with('/') {
            text.remove(0);
        }
    }
    let path = PathBuf::from(&text);
    // A path ending in a separator is a folder; otherwise it names a file in one.
    let folder = if text.ends_with('/') || text.ends_with('\\') {
        path
    } else {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    require_function(context, &folder, None)
}

/// `require('module')`: the `Module` class, with `createRequire`, `builtinModules`,
/// `isBuiltin`, `_load`, `_resolveFilename`, `_extensions`, `_cache` and `wrap`.
pub(crate) fn module_module(context: &mut Context, _: &str) -> JsResult<JsValue> {
    let (module, prototype) = class(
        context,
        ClassSpec {
            name: "Module",
            length: 2,
            init: |this, args, cx| {
                let id = arg(args, 0);
                this.set(
                    js_string!("id"),
                    if id.is_undefined() { js_text("") } else { id },
                    false,
                    cx,
                )?;
                let exports = object(cx);
                this.set(js_string!("exports"), exports, false, cx)?;
                this.set(js_string!("loaded"), false, false, cx)?;
                let children = JsArray::new(cx)?;
                this.set(js_string!("children"), children, false, cx)?;
                this.set(js_string!("parent"), arg(args, 1), false, cx)?;
                Ok(())
            },
            statics: &[
                ("createRequire", 1, create_require),
                ("isBuiltin", 1, |_, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?;
                    Ok(is_builtin(cx, &name).into())
                }),
                ("_load", 3, |_, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?;
                    let parent = arg(args, 1);
                    let folder = match get(&parent, "filename", cx)? {
                        f if f.is_string() => PathBuf::from(crate::print(&f))
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_default(),
                        _ => PathBuf::from(crate::shared(cx).cwd.borrow().clone()),
                    };
                    require(cx, &folder, &name, parent.as_object().as_ref())
                }),
                ("_resolveFilename", 2, |_, args, cx| {
                    let name = text_of(&arg(args, 0), cx)?;
                    let folder = match get(&arg(args, 1), "filename", cx)? {
                        f if f.is_string() => PathBuf::from(crate::print(&f))
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_default(),
                        _ => PathBuf::from(crate::shared(cx).cwd.borrow().clone()),
                    };
                    match resolve(cx, &folder, &name)? {
                        Resolved::Builtin(name) => Ok(js_text(&name)),
                        Resolved::File(path) => Ok(path_text(&path)),
                    }
                }),
                ("_nodeModulePaths", 1, |_, args, cx| {
                    let folder = PathBuf::from(text_of(&arg(args, 0), cx)?);
                    Ok(array(cx, lookup_paths(&folder)))
                }),
                ("wrap", 1, |_, args, cx| {
                    let source = text_of(&arg(args, 0), cx)?;
                    Ok(js_text(&format!(
                        "(function (exports, require, module, __filename, __dirname) {{ {source}\n}});"
                    )))
                }),
                ("syncBuiltinESMExports", 0, |_, _, context| {
                    let loader = crate::shared(context).esm.clone();
                    loader.sync_builtins(context)?;
                    Ok(JsValue::undefined())
                }),
                ("findSourceMap", 1, |_, _, _| Ok(JsValue::undefined())),
                ("register", 2, |_, _, _| Ok(JsValue::undefined())),
                ("registerHooks", 1, crate::module_hooks::register_hooks),
                ("enableCompileCache", 0, |_, _, cx| Ok(object(cx).into())),
            ],
            methods: &[("require", 1, |this, args, cx| {
                let name = text_of(&arg(args, 0), cx)?;
                let folder = match get(this, "filename", cx)? {
                    f if f.is_string() => PathBuf::from(crate::print(&f))
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default(),
                    _ => PathBuf::from(crate::shared(cx).cwd.borrow().clone()),
                };
                require(cx, &folder, &name, this.as_object().as_ref())
            })],
            ..ClassSpec::default()
        },
    )?;
    let _ = prototype;
    let mut names: Vec<&str> = crate::node::MODULES.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    let builtins: Vec<JsValue> = names.iter().map(|n| js_text(n)).collect();
    let builtins = array(context, builtins);
    module.set(js_string!("builtinModules"), builtins, false, context)?;
    let cache = cache(context);
    module.set(js_string!("_cache"), cache, false, context)?;
    let extensions = extensions(context)?;
    module.set(js_string!("_extensions"), extensions, false, context)?;
    let paths = JsArray::new(context)?;
    module.set(js_string!("globalPaths"), paths, false, context)?;
    #[cfg(feature = "typescript")]
    let strip = crate::typescript::strip_function(context)?;
    #[cfg(not(feature = "typescript"))]
    let strip = crate::native::function(context, "stripTypeScriptTypes", 1, |_, _, cx| {
        Err(crate::native::coded_error(
            cx,
            "module.stripTypeScriptTypes is not supported by Verve",
            "ERR_NOT_SUPPORTED",
        ))
    });
    module.set(js_string!("stripTypeScriptTypes"), strip, false, context)?;
    module.set(js_string!("Module"), module.clone(), false, context)?;
    Ok(module.into())
}
