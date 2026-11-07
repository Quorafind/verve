//! `Notification` (its permission only), `navigator.permissions.query` and the permission the
//! clipboard's `readText` needs: each asks the embedder's [`crate::PermissionBackend`] and
//! answers as a browser does once it has.

use std::collections::HashMap;

use boa_engine::builtins::promise::ResolvingFunctions;
use boa_engine::object::builtins::JsPromise;
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, ClassSpec, NewOnly, arg, arg_text, js_text};

use super::dom_error;
use crate::state::shared;

/// What a question the embedder has yet to answer will settle.
pub(crate) enum Asked {
    /// `Notification.requestPermission()`: the new permission's name.
    Request,
    /// `permissions.query({ name })`: a `PermissionStatus`.
    Query(String),
    /// `clipboard.readText()`: the text, or `NotAllowedError`.
    Read,
}

/// The pending questions, by the number the page gave each, and what the answers so far were.
#[derive(Default)]
pub(crate) struct Pending {
    next: u64,
    asked: HashMap<u64, (ResolvingFunctions, Asked)>,
    /// The last answer to each permission, by its Electron name.
    pub(crate) states: HashMap<String, bool>,
}

/// `granted` or `denied`, as a permission's state is spelled.
fn state_name(granted: bool) -> &'static str {
    if granted { "granted" } else { "denied" }
}

/// The name Electron's handlers know a web API's permission by.
fn electron_name(web: &str) -> &str {
    match web {
        "camera" | "microphone" => "media",
        other => other,
    }
}

/// Asks for `permission`; the promise it answers is settled as `kind` says, now or when the
/// embedder answers.
pub(crate) fn ask(
    context: &mut Context,
    permission: &str,
    check_only: bool,
    kind: Asked,
) -> JsResult<JsValue> {
    let (promise, resolvers) = JsPromise::new_pending(context);
    let (backend, url, id) = {
        let shared = shared(context);
        let mut state = shared.borrow_mut();
        state.permissions_pending.next += 1;
        let id = state.permissions_pending.next;
        (state.permissions.clone(), state.url.to_string(), id)
    };
    let Some(backend) = backend else {
        settle_with(context, &resolvers, kind, false)?;
        return Ok(promise.into());
    };
    match backend.ask(id, permission, &url, check_only) {
        Some(granted) => settle_with(context, &resolvers, kind, granted)?,
        None => {
            let shared = shared(context);
            let mut state = shared.borrow_mut();
            state
                .permissions_pending
                .asked
                .insert(id, (resolvers, kind));
        }
    }
    Ok(promise.into())
}

/// The embedder answered question `id`: whatever waited for it is settled.
pub(crate) fn settle(context: &mut Context, id: u64, granted: bool) -> JsResult<()> {
    let waiting = shared(context)
        .borrow_mut()
        .permissions_pending
        .asked
        .remove(&id);
    match waiting {
        Some((resolvers, kind)) => settle_with(context, &resolvers, kind, granted),
        None => Ok(()),
    }
}

fn settle_with(
    context: &mut Context,
    resolvers: &ResolvingFunctions,
    kind: Asked,
    granted: bool,
) -> JsResult<()> {
    let outcome = match kind {
        Asked::Request => Ok(js_text(state_name(granted))),
        Asked::Query(name) => status(context, &name, granted).map(JsValue::from),
        Asked::Read if granted => Ok(js_text("")),
        Asked::Read => Err(dom_error(
            context,
            "Read permission denied.",
            "NotAllowedError",
        )),
    };
    let (function, value) = match outcome {
        Ok(value) => (&resolvers.resolve, value),
        Err(error) => (&resolvers.reject, error.into_opaque(context)?),
    };
    function.call(&JsValue::undefined(), &[value], context)?;
    Ok(())
}

/// A `PermissionStatus`.
fn status(context: &mut Context, name: &str, granted: bool) -> JsResult<JsObject> {
    let status = native::object(context);
    status.set(js_string!("name"), js_text(name), false, context)?;
    status.set(
        js_string!("state"),
        js_text(state_name(granted)),
        false,
        context,
    )?;
    status.set(js_string!("onchange"), JsValue::null(), false, context)?;
    Ok(status)
}

/// What `Notification.permission` reads: what the embedder says is allowed, else what it last
/// answered, and `default` while neither is known.
fn current(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (backend, last) = {
        let shared = shared(context);
        let state = shared.borrow();
        (
            state.permissions.clone(),
            state
                .permissions_pending
                .states
                .get("notifications")
                .copied(),
        )
    };
    let answer = match backend {
        None => Some(false),
        Some(backend) => backend.known("notifications").or(last),
    };
    Ok(js_text(answer.map_or("default", state_name)))
}

fn request_permission(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    // Only what a check answers is what `Notification.permission` reads: with both handlers set,
    // the request handler's answer does not change it (as in Electron).
    ask(context, "notifications", false, Asked::Request)
}

/// `then` records the answer a permission question got, as the last known of its name.
fn remember(context: &mut Context, name: &str, promise: &JsValue) -> JsResult<JsValue> {
    let name = name.to_owned();
    let note = native::closure(
        context,
        "",
        1,
        |_, args, name: &String, cx| {
            let value = arg(args, 0);
            let granted = match value.as_string() {
                Some(text) => text.to_std_string_escaped() == "granted",
                None => native::get(&value, "state", cx)?
                    .as_string()
                    .is_some_and(|state| state.to_std_string_escaped() == "granted"),
            };
            shared(cx)
                .borrow_mut()
                .permissions_pending
                .states
                .insert(name.clone(), granted);
            Ok(value)
        },
        name,
    );
    native::invoke(promise, "then", &[note.into()], context)
}

fn query(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let descriptor = arg(args, 0);
    let name = native::get(&descriptor, "name", context)?;
    let name = native::text_of(&name, context)?;
    let permission = electron_name(&name).to_owned();
    let asked = ask(context, &permission, true, Asked::Query(name))?;
    remember(context, &permission, &asked)
}

/// What `clipboard.readText()` asks before it reads.
pub(crate) fn ask_to_read(context: &mut Context) -> JsResult<JsValue> {
    ask(context, "clipboard-read", false, Asked::Read)
}

/// Installs `Notification` and `navigator.permissions`.
pub(crate) fn install(context: &mut Context) -> JsResult<()> {
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "Notification",
            length: 1,
            init: |this, args, cx| {
                let title = arg_text(args, 0, cx)?;
                this.set(js_string!("title"), js_text(&title), false, cx)?;
                Ok(())
            },
            statics: &[("requestPermission", 0, request_permission)],
            new_only: Some(NewOnly::Class),
            ..ClassSpec::default()
        },
    )?;
    native::accessors(context, &class, &[("permission", current, None)])?;
    native::define_global(context, "Notification", class)?;
    let navigator = native::global(context, "navigator")?;
    if let Some(navigator) = navigator.as_object() {
        let permissions = native::object(context);
        native::methods(context, &permissions, &[("query", 1, query)])?;
        navigator.set(js_string!("permissions"), permissions, false, context)?;
    }
    Ok(())
}
