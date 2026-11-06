//! `webFrame`: the renderer's view of its frame — zoom, injected CSS, running script. A page has
//! one frame, so the others are `null`. Behaviour is Electron 39.8.10's, observed.

use std::cell::RefCell;
use std::collections::HashMap;

use boa_engine::object::builtins::JsPromise;
use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, Source, js_string,
};
use verve::native::{self, ClassSpec, NewOnly, arg};
use verve::{MAIN_WORLD, current_world};

use super::context_bridge::bridge_value;
use super::define::{Args, Fields};
use crate::msg::FromRenderer;
use crate::preload::{self, setup};

/// What `executeJavaScript` rejects with when the script throws.
const SCRIPT_FAILED: &str = "Script failed to execute, this normally means an error was thrown. Check the renderer console for the error.";

/// The ratio between zoom levels.
const LEVEL_STEP: f64 = 1.2;

/// `require('electron').webFrame`.
pub(crate) fn make(context: &mut Context) -> JsResult<JsObject> {
    let class = frame_class(context)?;
    class.construct(&[], None, context)
}

/// The `WebFrame` class of the running world, made the first time it is asked for.
fn frame_class(context: &mut Context) -> JsResult<JsObject> {
    let world = current_world(context);
    if let Some(Classes(kept)) = context.get_data::<Classes>()
        && let Some(class) = kept.borrow().get(&world)
    {
        return Ok(class.clone());
    }
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "WebFrame",
            methods: &[
                ("setZoomFactor", 1, set_zoom_factor),
                ("getZoomFactor", 0, |_, _, cx| {
                    Ok(setup(cx).zoom.get().into())
                }),
                ("setZoomLevel", 1, set_zoom_level),
                ("getZoomLevel", 0, |_, _, cx| {
                    Ok((setup(cx).zoom.get().ln() / LEVEL_STEP.ln()).into())
                }),
                ("setVisualZoomLevelLimits", 2, |_, _, _| {
                    Ok(JsValue::undefined())
                }),
                ("insertCSS", 1, insert_css),
                ("removeInsertedCSS", 1, remove_css),
                ("executeJavaScript", 1, execute_javascript),
                ("executeJavaScriptInIsolatedWorld", 2, execute_in_world),
                (
                    "setIsolatedWorldInfo",
                    2,
                    |_, _, _| Ok(JsValue::undefined()),
                ),
                ("getResourceUsage", 0, resource_usage),
                ("clearCache", 0, |_, _, _| Ok(JsValue::undefined())),
                ("setName", 1, |_, _, _| Ok(JsValue::undefined())),
                ("allowGuestViewElementDefinition", 2, |_, _, _| {
                    Ok(JsValue::undefined())
                }),
                ("findFrameByName", 1, |_, _, _| Ok(JsValue::null())),
                ("findFrameByRoutingId", 1, |_, _, _| Ok(JsValue::null())),
                ("findFrameByToken", 1, |_, _, _| Ok(JsValue::null())),
                ("getFrameForSelector", 1, |_, _, _| Ok(JsValue::null())),
                ("setSpellCheckProvider", 2, unsupported),
                ("isWordMisspelled", 1, unsupported),
                ("getWordSuggestions", 1, unsupported),
            ],
            accessors: &[
                ("top", |_, _, cx| frame(cx), None),
                ("parent", |_, _, _| Ok(JsValue::null()), None),
                ("firstChild", |_, _, _| Ok(JsValue::null()), None),
                ("nextSibling", |_, _, _| Ok(JsValue::null()), None),
                ("opener", |_, _, _| Ok(JsValue::null()), None),
                ("routingId", routing_id, None),
                (
                    "frameToken",
                    |_, _, _| Ok(JsString::from("verve-main-frame").into()),
                    None,
                ),
            ],
            new_only: Some(NewOnly::Class),
            ..ClassSpec::default()
        },
    )?;
    if context.get_data::<Classes>().is_none() {
        context.insert_data(Classes(RefCell::default()));
    }
    if let Some(Classes(kept)) = context.get_data::<Classes>() {
        kept.borrow_mut().insert(world, class.clone());
    }
    Ok(class)
}

/// The `WebFrame` class of each world.
struct Classes(RefCell<HashMap<verve::WorldId, JsObject>>);

/// A new object for the frame, as `webFrame.top` answers.
fn frame(context: &mut Context) -> JsResult<JsValue> {
    Ok(make(context)?.into())
}

fn unsupported(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Err(super::define::unsupported(
        context,
        "webFrame spell checking",
    ))
}

/// `routingId`, which Electron deprecates in favour of `frameToken` and says so on the console.
fn routing_id(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let console = native::global(context, "console")?;
    let said = JsString::from(
        "(electron) 'webFrame.routingId' is deprecated and will be removed. Please use 'webFrame.frameToken' instead.",
    );
    native::invoke(&console, "warn", &[said.into()], context)?;
    Ok(1.into())
}

// ---------- zoom ----------

fn set_zoom_factor(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let factor = Args::new(args).f64(0, "factor", context)?;
    zoom_to(factor, context);
    Ok(JsValue::undefined())
}

fn set_zoom_level(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let level = Args::new(args).f64(0, "level", context)?;
    zoom_to(LEVEL_STEP.powf(level), context);
    Ok(JsValue::undefined())
}

/// The page is zoomed: the frame remembers and the main process is told, since the shell scales.
fn zoom_to(factor: f64, context: &mut Context) {
    if !(factor.is_finite() && factor > 0.) {
        return;
    }
    setup(context).zoom.set(factor);
    crate::api::ipc_renderer::outbox(context)
        .borrow_mut()
        .push(FromRenderer::ZoomChanged(factor));
}

// ---------- CSS ----------

/// `insertCSS(css, { cssOrigin })`: answers the key to take it away with.
fn insert_css(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let css = Args::new(args).string(0, "css", context)?;
    let given = arg(args, 1).as_object();
    let origin = Fields::new(given.as_ref()).string("cssOrigin", context)?;
    let setup = setup(context);
    let number = setup.sheets.get() + 1;
    setup.sheets.set(number);
    let key = format!("webframe-css-{number}");
    verve_dom::insert_stylesheet(context, &key, &css, origin.as_deref() == Some("user"));
    Ok(JsString::from(key).into())
}

fn remove_css(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let key = Args::new(args).string(0, "key", context)?;
    verve_dom::remove_stylesheet(context, &key);
    Ok(JsValue::undefined())
}

// ---------- script ----------

/// Runs `code` in `world` as a script; an error thrown is reported on the console, as a page's
/// own would be, and answered as the failure.
pub(crate) fn evaluate(
    code: &str,
    world: verve::WorldId,
    context: &mut Context,
) -> Result<JsValue, JsError> {
    if verve::world_realm(context, world).is_none() {
        return Err(native::error("no such world"));
    }
    verve::in_world(context, world, |cx| {
        let result = cx.eval(Source::from_bytes(code));
        if let Err(error) = &result {
            verve::report(cx, error);
        }
        result
    })
}

/// `executeJavaScript(code[, userGesture][, callback])`: the script runs in the page's world;
/// what it answers (a promise is waited for) crosses back as the promise's value.
fn execute_javascript(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let code = Args::new(args).string(0, "code", context)?;
    let callback = args.iter().skip(1).find(|a| a.is_callable()).cloned();
    answer(
        evaluate(&code, MAIN_WORLD, context),
        MAIN_WORLD,
        callback,
        context,
    )
}

/// `executeJavaScriptInIsolatedWorld(worldId, scripts[, userGesture][, callback])`.
fn execute_in_world(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let number = arg(args, 0).to_i32(context)?;
    let world = preload::world_numbered(i64::from(number), context)?;
    let scripts = native::list_of(&arg(args, 1), context)?;
    let mut last = Ok(JsValue::undefined());
    for script in scripts {
        let code = match script.as_object() {
            Some(object) => object.get(js_string!("code"), context)?,
            None => JsValue::undefined(),
        };
        let code = native::text_of(&code, context)?;
        last = evaluate(&code, world, context);
        if last.is_err() {
            break;
        }
    }
    let callback = args.iter().skip(2).find(|a| a.is_callable()).cloned();
    answer(last, world, callback, context)
}

/// The promise `executeJavaScript` answers: the result — after a promise it is has settled —
/// crossed to the world that asked.
fn answer(
    outcome: Result<JsValue, JsError>,
    from: verve::WorldId,
    callback: Option<JsValue>,
    context: &mut Context,
) -> JsResult<JsValue> {
    let to = current_world(context);
    let Ok(value) = outcome else {
        return native::settled(
            Err(JsNativeError::error().with_message(SCRIPT_FAILED).into()),
            context,
        );
    };
    let waited = JsPromise::resolve(value, context)?;
    let (promise, resolvers) = JsPromise::new_pending(context);
    let on_value = native::closure(
        context,
        "",
        1,
        |_,
         args,
         (resolve, reject, callback, from, to): &(JsObject, JsObject, JsValue, u32, u32),
         cx| {
            match bridge_value(&arg(args, 0), *from, *to, cx) {
                Ok(value) => {
                    if callback.is_callable() {
                        verve::call(
                            cx,
                            callback,
                            &JsValue::undefined(),
                            std::slice::from_ref(&value),
                        )?;
                    }
                    resolve.call(&JsValue::undefined(), &[value], cx)
                }
                Err(error) => {
                    let thrown = error.into_opaque(cx).unwrap_or_default();
                    reject.call(&JsValue::undefined(), &[thrown], cx)
                }
            }
        },
        (
            JsObject::from(resolvers.resolve.clone()),
            JsObject::from(resolvers.reject.clone()),
            callback.unwrap_or_default(),
            from,
            to,
        ),
    );
    let reject = resolvers.reject.clone();
    let on_error = native::closure(
        context,
        "",
        1,
        |_, _, reject: &JsObject, cx| {
            let error = JsNativeError::error()
                .with_message(SCRIPT_FAILED)
                .into_opaque(cx);
            reject.call(&JsValue::undefined(), &[error.into()], cx)
        },
        JsObject::from(reject),
    );
    waited.then(
        Some(boa_engine::object::builtins::JsFunction::from_object(on_value).expect("a function")),
        Some(boa_engine::object::builtins::JsFunction::from_object(on_error).expect("a function")),
        context,
    )?;
    Ok(promise.into())
}

/// `getResourceUsage()`: the figures Electron answers, which this page does not keep.
fn resource_usage(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let usage = native::object(context);
    for name in [
        "images",
        "scripts",
        "cssStyleSheets",
        "xslStyleSheets",
        "fonts",
        "other",
    ] {
        let record = native::object(context);
        for field in ["count", "size", "liveSize"] {
            record.set(JsString::from(field), 0, false, context)?;
        }
        usage.set(JsString::from(name), record, false, context)?;
    }
    Ok(usage.into())
}
