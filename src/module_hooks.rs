//! `module.registerHooks`: synchronous resolve and load hooks, run in the module's own thread, as
//! Node 22 has them — how a host maps bare names to files of its own (an import map in Node's
//! vocabulary) or hands the loader source it made (TypeScript with its types stripped).
//!
//! The hooks are a chain. The hook registered last runs first, and the `next` it is handed calls
//! the one registered before it; at the bottom of the chain is the runtime's own resolution and
//! loading. A resolve hook answers `{ url, format? }`; a load hook answers `{ format, source }`,
//! where `format` is `'module'`, `'commonjs'`, `'json'` or `'builtin'` and `source` a string or
//! bytes. A format a resolve hook gives is what the load chain's `context.format` starts as, and
//! the runtime's own load honours a format it is handed rather than detecting one.
//!
//! What they apply to: `import` and `import()`, and `import.meta.resolve`. `require()` keeps
//! Node's CommonJS resolution, unhooked; and a load hook's source for a `'commonjs'` module is
//! not what runs — the file is, as `require` reads it — though its named exports are read from
//! that source.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use boa_engine::{Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, js_string};
use url::Url;

use crate::esm_resolve::Mode;
use crate::native::{arg, array, closure, get, js_text, object, text_of};

/// One `registerHooks` call's hooks.
struct Hook {
    id: u32,
    resolve: Option<JsObject>,
    load: Option<JsObject>,
}

#[derive(Default)]
struct Chain {
    hooks: RefCell<Vec<Hook>>,
    next_id: Cell<u32>,
    /// The format a resolve hook gave each URL, for the load chain to start from.
    formats: RefCell<HashMap<String, String>>,
}

fn chain(context: &mut Context) -> Rc<Chain> {
    if let Some(chain) = context.get_data::<Rc<Chain>>() {
        return chain.clone();
    }
    let chain = Rc::new(Chain::default());
    context.insert_data(chain.clone());
    chain
}

/// Whether any resolve hook is registered.
pub(crate) fn resolving(context: &Context) -> bool {
    context
        .get_data::<Rc<Chain>>()
        .is_some_and(|chain| chain.hooks.borrow().iter().any(|h| h.resolve.is_some()))
}

/// Whether any load hook is registered.
pub(crate) fn loading(context: &Context) -> bool {
    context
        .get_data::<Rc<Chain>>()
        .is_some_and(|chain| chain.hooks.borrow().iter().any(|h| h.load.is_some()))
}

fn invalid(context: &mut Context, message: &str, code: &str) -> JsError {
    let error = JsNativeError::typ()
        .with_message(message.to_owned())
        .into_opaque(context);
    let _ = error.set(js_string!("code"), JsString::from(code), false, context);
    JsError::from_opaque(error.into())
}

/// A hook function, or none; anything else is refused.
fn hook_function(options: &JsValue, name: &str, context: &mut Context) -> JsResult<Option<JsObject>> {
    let value = get(options, name, context)?;
    if value.is_undefined() {
        return Ok(None);
    }
    match value.as_callable() {
        Some(f) => Ok(Some(f.clone())),
        None => Err(invalid(
            context,
            &format!("The \"options.{name}\" property must be of type function"),
            "ERR_INVALID_ARG_TYPE",
        )),
    }
}

/// `registerHooks({ resolve, load })`: adds the hooks to the top of the chain, and answers a
/// handle whose `deregister()` takes them out again.
pub(crate) fn register_hooks(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let options = arg(args, 0);
    if !options.is_object() {
        return Err(invalid(
            context,
            "The \"options\" argument must be of type object",
            "ERR_INVALID_ARG_TYPE",
        ));
    }
    let resolve = hook_function(&options, "resolve", context)?;
    let load = hook_function(&options, "load", context)?;
    let registered = chain(context);
    let id = registered.next_id.get();
    registered.next_id.set(id + 1);
    registered.hooks.borrow_mut().push(Hook { id, resolve, load });
    let handle = object(context);
    let deregister = closure(
        context,
        "deregister",
        0,
        |_, _, id: &u32, context| {
            let chain = chain(context);
            chain.hooks.borrow_mut().retain(|hook| hook.id != *id);
            Ok(JsValue::undefined())
        },
        id,
    );
    handle.set(js_string!("deregister"), deregister, false, context)?;
    Ok(handle.into())
}

// ─── Resolving ──────────────────────────────────────────────────────────────────────────────

/// What the runtime's own resolution needs besides the specifier: carried down the chain.
#[derive(Clone, boa_gc::Trace, boa_gc::Finalize)]
struct ResolveBase {
    parent: JsString,
    check_file: bool,
}

/// Resolves `specifier`, imported from `parent` (a URL), through the resolve hooks.
pub(crate) fn resolve(
    specifier: &str,
    parent: &Url,
    check_file: bool,
    context: &mut Context,
) -> JsResult<Url> {
    let base = ResolveBase {
        parent: JsString::from(parent.as_str()),
        check_file,
    };
    let hook_context = resolve_context(parent.as_str(), context)?;
    let top = chain(context).hooks.borrow().len();
    let answer = run_resolve(top, js_text(specifier), hook_context, &base, context)?;
    let url = get(&answer, "url", context)?;
    if !url.is_string() {
        return Err(invalid(
            context,
            "Expected a string to be returned for the \"url\" from the \"resolve\" hook",
            "ERR_INVALID_RETURN_PROPERTY_VALUE",
        ));
    }
    let url = text_of(&url, context)?;
    let parsed = Url::parse(&url)
        .map_err(|_| invalid(context, &format!("Invalid URL: {url}"), "ERR_INVALID_URL"))?;
    let format = match get(&answer, "format", context)? {
        format if format.is_string() => Some(text_of(&format, context)?),
        _ => None,
    };
    let registered = chain(context);
    let mut formats = registered.formats.borrow_mut();
    match format {
        Some(format) => formats.insert(parsed.to_string(), format),
        None => formats.remove(parsed.as_str()),
    };
    drop(formats);
    Ok(parsed)
}

/// `{ conditions, importAttributes, parentURL }`, as a resolve hook is handed it.
fn resolve_context(parent: &str, context: &mut Context) -> JsResult<JsValue> {
    let conditions = conditions(context);
    let hook_context = object(context);
    hook_context.set(js_string!("conditions"), conditions, false, context)?;
    let attributes = object(context);
    hook_context.set(js_string!("importAttributes"), attributes, false, context)?;
    hook_context.set(js_string!("parentURL"), js_text(parent), false, context)?;
    Ok(hook_context.into())
}

fn conditions(context: &mut Context) -> JsValue {
    let loader = crate::shared(context).esm.clone();
    let names: Vec<JsValue> = loader
        .condition_names()
        .iter()
        .map(|name| js_text(name))
        .collect();
    array(context, names)
}

/// The chain from the hook at `level` down: `level` 0 is the runtime's own resolution.
fn run_resolve(
    level: usize,
    specifier: JsValue,
    hook_context: JsValue,
    base: &ResolveBase,
    context: &mut Context,
) -> JsResult<JsValue> {
    if level == 0 {
        return own_resolve(&specifier, &hook_context, base, context);
    }
    let hook = chain(context).hooks.borrow().get(level - 1).and_then(|h| h.resolve.clone());
    let Some(hook) = hook else {
        return run_resolve(level - 1, specifier, hook_context, base, context);
    };
    let next = closure(
        context,
        "nextResolve",
        2,
        |_, args, (level, hook_context, base): &(u32, JsValue, ResolveBase), context| {
            let passed = arg(args, 1);
            let hook_context = if passed.is_object() { passed } else { hook_context.clone() };
            run_resolve(*level as usize, arg(args, 0), hook_context, base, context)
        },
        ((level - 1) as u32, hook_context.clone(), base.clone()),
    );
    let answer = crate::call(
        context,
        &hook.into(),
        &JsValue::undefined(),
        &[specifier, hook_context, next.into()],
    )?;
    if !answer.is_object() {
        return Err(invalid(
            context,
            "Expected an object to be returned from the \"resolve\" hook",
            "ERR_INVALID_RETURN_VALUE",
        ));
    }
    Ok(answer)
}

/// The runtime's own resolution, answered as a hook's would be: `{ url, format }`.
fn own_resolve(
    specifier: &JsValue,
    hook_context: &JsValue,
    base: &ResolveBase,
    context: &mut Context,
) -> JsResult<JsValue> {
    let specifier = text_of(specifier, context)?;
    let parent = match get(hook_context, "parentURL", context)? {
        parent if parent.is_string() => text_of(&parent, context)?,
        _ => base.parent.to_std_string_escaped(),
    };
    let loader = crate::shared(context).esm.clone();
    let url = loader.own_resolve(
        &specifier,
        Some(Path::new(&parent)),
        Mode::Import,
        base.check_file,
        context,
    )?;
    let answer = object(context);
    answer.set(js_string!("url"), js_text(url.as_str()), false, context)?;
    if let Some(format) = format_of_url(&url) {
        answer.set(js_string!("format"), js_text(format), false, context)?;
    }
    Ok(answer.into())
}

/// The format a URL says by itself, as Node's resolution reports it; `None` for a `.js` file,
/// whose format its package and its syntax decide when it is loaded.
fn format_of_url(url: &Url) -> Option<&'static str> {
    if url.scheme() == "node" {
        return Some("builtin");
    }
    let path = url.path();
    if path.ends_with(".mjs") {
        Some("module")
    } else if path.ends_with(".cjs") {
        Some("commonjs")
    } else if path.ends_with(".json") {
        Some("json")
    } else {
        None
    }
}

// ─── Loading ────────────────────────────────────────────────────────────────────────────────

/// What a module is made from: its format, and its source (none for a builtin).
pub(crate) struct Loaded {
    pub(crate) format: String,
    pub(crate) source: Option<String>,
}

/// Loads `url` through the load hooks.
pub(crate) fn load(url: &Url, context: &mut Context) -> JsResult<Loaded> {
    let format = chain(context).formats.borrow().get(url.as_str()).cloned();
    let conditions = conditions(context);
    let hook_context = object(context);
    hook_context.set(js_string!("conditions"), conditions, false, context)?;
    hook_context.set(
        js_string!("format"),
        format.as_deref().map_or_else(JsValue::undefined, js_text),
        false,
        context,
    )?;
    let attributes = object(context);
    hook_context.set(js_string!("importAttributes"), attributes, false, context)?;
    let top = chain(context).hooks.borrow().len();
    let answer = run_load(top, js_text(url.as_str()), hook_context.into(), context)?;
    let format = get(&answer, "format", context)?;
    if !format.is_string() {
        return Err(invalid(
            context,
            &format!("Unknown module format: {} for URL {url}", crate::print(&format)),
            "ERR_UNKNOWN_MODULE_FORMAT",
        ));
    }
    let format = text_of(&format, context)?;
    let source = source_text(&get(&answer, "source", context)?, context)?;
    Ok(Loaded { format, source })
}

/// A source as text: a string, or bytes read as UTF-8.
fn source_text(source: &JsValue, context: &mut Context) -> JsResult<Option<String>> {
    if source.is_null_or_undefined() {
        return Ok(None);
    }
    if source.is_string() {
        return text_of(source, context).map(Some);
    }
    match crate::encoding::bytes_of(source, context)? {
        Some(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| {
            invalid(context, "Module source is not UTF-8", "ERR_INVALID_RETURN_PROPERTY_VALUE")
        }),
        None => Err(invalid(
            context,
            "Expected a string, an ArrayBuffer or a TypedArray to be returned for the \"source\" from the \"load\" hook",
            "ERR_INVALID_RETURN_PROPERTY_VALUE",
        )),
    }
}

/// The chain from the hook at `level` down: `level` 0 is the runtime's own loading.
fn run_load(
    level: usize,
    url: JsValue,
    hook_context: JsValue,
    context: &mut Context,
) -> JsResult<JsValue> {
    if level == 0 {
        return own_load(&url, &hook_context, context);
    }
    let hook = chain(context).hooks.borrow().get(level - 1).and_then(|h| h.load.clone());
    let Some(hook) = hook else {
        return run_load(level - 1, url, hook_context, context);
    };
    let next = closure(
        context,
        "nextLoad",
        2,
        |_, args, (level, hook_context): &(u32, JsValue), context| {
            let passed = arg(args, 1);
            let hook_context = if passed.is_object() { passed } else { hook_context.clone() };
            run_load(*level as usize, arg(args, 0), hook_context, context)
        },
        ((level - 1) as u32, hook_context.clone()),
    );
    let answer = crate::call(
        context,
        &hook.into(),
        &JsValue::undefined(),
        &[url, hook_context, next.into()],
    )?;
    if !answer.is_object() {
        return Err(invalid(
            context,
            "Expected an object to be returned from the \"load\" hook",
            "ERR_INVALID_RETURN_VALUE",
        ));
    }
    Ok(answer)
}

/// The runtime's own loading, answered as a hook's would be: `{ format, source }`.
fn own_load(url: &JsValue, hook_context: &JsValue, context: &mut Context) -> JsResult<JsValue> {
    let url = text_of(url, context)?;
    let url = Url::parse(&url)
        .map_err(|_| invalid(context, &format!("Invalid URL: {url}"), "ERR_INVALID_URL"))?;
    let format = match get(hook_context, "format", context)? {
        format if format.is_string() => Some(text_of(&format, context)?),
        _ => None,
    };
    let loader = crate::shared(context).esm.clone();
    let loaded = loader.own_load(&url, format, context)?;
    let answer = object(context);
    answer.set(js_string!("format"), js_text(&loaded.format), false, context)?;
    let source = loaded.source.as_deref().map_or_else(JsValue::null, js_text);
    answer.set(js_string!("source"), source, false, context)?;
    Ok(answer.into())
}
