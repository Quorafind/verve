//! CommonJS, as Node has it: `require` and how it finds what it is asked for, `module`,
//! `require.cache`, `require.resolve`, and `require('module')`.
//!
//! A name is looked for, in order:
//!
//! 1. Among the built-in modules — what the host defined ([`crate::RuntimeX::define_builtin`]),
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
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Source, js_string,
    object::builtins::JsArray,
};

use crate::native::{
    ClassSpec, arg, array, class, closure, function, get, hidden, js_text, object, text_of,
};

/// What a name names: a built-in module, or a file.
enum Resolved {
    Builtin(String),
    File(PathBuf),
}

/// The name Node's built-in module answers to, as `require` was given it: `node:` and a
/// trailing `/` taken off, and Electron's `original-fs` read as `fs`.
pub(crate) fn bare_name(name: &str) -> &str {
    let bare = name.strip_prefix("node:").unwrap_or(name);
    let bare = bare.strip_suffix('/').unwrap_or(bare);
    match bare {
        "original-fs" => "fs",
        other => other,
    }
}

/// Whether Node's table answers `name`: every module of it does to its bare name, but those
/// Node only has under `node:` (`node:sqlite`, `node:test`) do not — a bare `sqlite` is a package.
fn node_table_answers(name: &str) -> bool {
    name.starts_with("node:") || !crate::node::PREFIX_ONLY.contains(&bare_name(name))
}

/// Whether `name` is a built-in module, without making it.
fn is_builtin(context: &Context, name: &str) -> bool {
    let bare = bare_name(name);
    let state = crate::shared(context);
    state.builtins.borrow().contains_key(bare)
        || (node_table_answers(name) && crate::node::MODULES.iter().any(|(n, _)| *n == bare))
        || state
            .families
            .borrow()
            .iter()
            .any(|(prefix, _)| bare.starts_with(prefix.as_str()))
}

/// The built-in module `name`, made now if it has not been yet; `None` when there is none.
pub(crate) fn builtin(context: &mut Context, name: &str) -> JsResult<Option<JsValue>> {
    let bare = bare_name(name);
    if let Some(exports) = crate::shared(context).builtins.borrow().get(bare) {
        return Ok(Some(exports.clone()));
    }
    // One Node has only under `node:` is kept under that name, so that making it does not make
    // a bare `require('sqlite')` find it.
    let key = if crate::node::PREFIX_ONLY.contains(&bare) {
        format!("node:{bare}")
    } else {
        bare.to_owned()
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
fn as_file_or_folder(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    for ext in ["js", "json", "cjs"] {
        let path = PathBuf::from(format!("{}.{ext}", base.display()));
        if path.is_file() {
            return Some(path);
        }
    }
    if base.is_dir() {
        let manifest = base.join("package.json");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&text)
        {
            let main = json.get("main").and_then(|m| m.as_str()).or_else(|| {
                // `"exports": "./x.js"`, or `{ ".": "./x.js" }`, or `{ ".": { "require": … } }`.
                let exports = json.get("exports")?;
                let dot = exports.get(".").unwrap_or(exports);
                dot.as_str()
                    .or_else(|| dot.get("require").and_then(|r| r.as_str()))
                    .or_else(|| dot.get("default").and_then(|r| r.as_str()))
            });
            if let Some(main) = main {
                let target = base.join(main);
                if let Some(found) = as_file_or_folder_plain(&target) {
                    return Some(found);
                }
            }
        }
        for index in ["index.js", "index.json", "index.cjs"] {
            let path = base.join(index);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

/// A `main` named in a `package.json`: the file, with an extension added, or its folder's
/// `index.js` — but not that folder's own `package.json` again.
fn as_file_or_folder_plain(base: &Path) -> Option<PathBuf> {
    if base.is_file() {
        return Some(base.to_path_buf());
    }
    for ext in ["js", "json", "cjs"] {
        let path = PathBuf::from(format!("{}.{ext}", base.display()));
        if path.is_file() {
            return Some(path);
        }
    }
    let index = base.join("index.js");
    index.is_file().then_some(index)
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
fn resolve(context: &Context, folder: &Path, name: &str) -> Option<Resolved> {
    if is_builtin(context, name) && !is_path(name) {
        return Some(Resolved::Builtin(name.to_owned()));
    }
    if is_path(name) {
        return as_file_or_folder(&clean(&folder.join(name))).map(Resolved::File);
    }
    // A package in `node_modules`, here or in a folder above.
    let mut at = Some(folder);
    while let Some(dir) = at {
        let candidate = clean(&dir.join("node_modules").join(name));
        if let Some(found) = as_file_or_folder(&candidate) {
            return Some(Resolved::File(found));
        }
        at = dir.parent();
    }
    None
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
    match resolve(context, folder, name) {
        Some(Resolved::Builtin(name)) => {
            builtin(context, &name)?.ok_or_else(|| not_found(context, &name))
        }
        Some(Resolved::File(path)) => load_file(context, &path, parent),
        None => Err(not_found(context, name)),
    }
}

/// The cache: `require.cache`, the module objects of every file run, by path.
fn cache(context: &Context) -> JsObject {
    crate::shared(context).module_cache.clone()
}

/// A file required: answered from the cache, or read and run (or, a `.json`, parsed).
fn load_file(context: &mut Context, path: &Path, parent: Option<&JsObject>) -> JsResult<JsValue> {
    let key = JsString::from(path.to_string_lossy().as_ref());
    let cached = cache(context).get(key.clone(), context)?;
    if cached.is_object() {
        return get(&cached, "exports", context);
    }
    let text = std::fs::read_to_string(path).map_err(|error| {
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
    run_module(context, path, text, parent)
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
    let filename = &clean(filename);
    let key = JsString::from(filename.to_string_lossy().as_ref());
    let cached = cache(context).get(key.clone(), context)?;
    if cached.is_object() {
        return get(&cached, "exports", context);
    }
    let folder = filename.parent().unwrap_or(Path::new("")).to_path_buf();
    // A `#!` line, which Node reads as a comment.
    let source = if source.starts_with("#!") {
        source.find('\n').map_or("", |at| &source[at..])
    } else {
        source
    };
    // Node's own wrapper, word for word, kept on the module's first line so a line number in an
    // error is the line in the file.
    let wrapped =
        format!("(function (exports, require, module, __filename, __dirname) {{ {source}\n}})");
    let source_path = filename.to_path_buf();
    let wrapper = context.eval(Source::from_bytes(&wrapped).with_path(&source_path))?;

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
    let key = JsString::from(clean(filename).to_string_lossy().as_ref());
    let _ = cache(context).delete_property_or_throw(key, context);
}

/// The folder a `require` resolves relative names against, kept as text in the functions that
/// need it.
fn folder_of(captured: &JsValue) -> PathBuf {
    PathBuf::from(crate::print(captured))
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
            for folder in folders {
                match resolve(context, &folder, &name) {
                    Some(Resolved::Builtin(name)) => return Ok(js_text(&name)),
                    Some(Resolved::File(path)) => return Ok(path_text(&path)),
                    None => {}
                }
            }
            Err(not_found(context, &name))
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
    require.set(js_string!("main"), JsValue::undefined(), false, context)?;
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
                    match resolve(cx, &folder, &name) {
                        Some(Resolved::Builtin(name)) => Ok(js_text(&name)),
                        Some(Resolved::File(path)) => Ok(path_text(&path)),
                        None => Err(not_found(cx, &name)),
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
                ("syncBuiltinESMExports", 0, |_, _, _| {
                    Ok(JsValue::undefined())
                }),
                ("findSourceMap", 1, |_, _, _| Ok(JsValue::undefined())),
                ("register", 2, |_, _, _| Ok(JsValue::undefined())),
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
    module.set(js_string!("Module"), module.clone(), false, context)?;
    Ok(module.into())
}
