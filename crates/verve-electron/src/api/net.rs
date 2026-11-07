//! `net`: the main process's own network (`lib/browser/api/net.ts` and `net-fetch.ts` of
//! Electron 39.8.10). `net.request` is [`ClientRequest`](super::net_request); `net.fetch` is the
//! kernel's `fetch` asked of a session's [`MainNet`](super::main_net), answering a `Response` the
//! way Electron builds it — a plain `new Response(body, { status, statusText, headers })`, so with
//! no `url`, of type `default`, and not `redirected`; `redirect: 'manual'` fails with `Redirect
//! was cancelled`.

use std::rc::Rc;
use std::sync::Arc;

use boa_engine::{Context, JsResult, JsValue, js_string};
use verve::browser_net::{BrowserNet, InternalFetch, ResourceType, fetch_internal};
use verve::native::{self, arg, get, invoke, js_text};

use super::define::{self, Fields};
use super::main_net::MainNet;
use super::net_request;
use crate::hub::Session;

/// The `net` module.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    native::methods(
        context,
        &module,
        &[
            ("request", 2, request),
            ("fetch", 1, |_, args, cx| {
                let session = super::shared(cx).hub.session_for(None);
                fetch(cx, &session, &arg(args, 0), &arg(args, 1))
            }),
            ("resolveHost", 1, |_, args, cx| {
                let host = define::Args::new(args).string(0, "host", cx)?;
                resolve_host(cx, &host)
            }),
            ("isOnline", 0, |_, _, cx| Ok(is_online(cx)?.into())),
        ],
    )?;
    native::accessors(
        context,
        &module,
        &[("online", |_, _, cx| Ok(is_online(cx)?.into()), None)],
    )?;
    Ok(module.into())
}

/// `net.request(options, callback)`.
fn request(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    if !super::shared(context).app.is_ready() {
        return Err(define::error(
            "net module can only be used after app is ready",
        ));
    }
    Ok(net_request::new_request(context, &arg(args, 0), &arg(args, 1))?.into())
}

/// `net.fetch(input, init)` through `session`.
pub(crate) fn fetch(
    context: &mut Context,
    session: &Arc<Session>,
    input: &JsValue,
    init: &JsValue,
) -> JsResult<JsValue> {
    let init_object = init.as_object();
    let fields = Fields::new(init_object.as_ref());
    let bypass = fields.bool("bypassCustomProtocolHandlers", false, context)?;
    let how = InternalFetch {
        net: Some(
            Rc::new(MainNet::for_request(context, Arc::clone(session), bypass))
                as Rc<dyn BrowserNet>,
        ),
        resource_type: ResourceType::Other,
        initiator: None,
    };
    let manual = fields.string("redirect", context)?.as_deref() == Some("manual")
        || input_redirect_is_manual(input, context)?;
    let promise = fetch_internal(context, input, init, how);
    let wrap = native::closure(context, "", 1, rebuild, manual);
    invoke(&promise.into(), "then", &[wrap.into()], context)
}

/// Whether a `Request` given as the input asks not to follow redirects.
fn input_redirect_is_manual(input: &JsValue, context: &mut Context) -> JsResult<bool> {
    if !input.is_object() {
        return Ok(false);
    }
    let redirect = get(input, "redirect", context)?;
    Ok(redirect
        .as_string()
        .is_some_and(|r| r.to_std_string_escaped() == "manual"))
}

/// The response as Electron builds it from the loader's.
fn rebuild(
    _: &JsValue,
    args: &[JsValue],
    manual: &bool,
    context: &mut Context,
) -> JsResult<JsValue> {
    let response = arg(args, 0);
    let status = get(&response, "status", context)?
        .as_number()
        .unwrap_or(200.);
    let location = invoke(
        &get(&response, "headers", context)?,
        "has",
        &[js_text("location")],
        context,
    )?;
    if *manual && crate::msg::is_redirect(status as u16) && location.to_boolean() {
        return Err(define::error("Redirect was cancelled"));
    }
    let text = get(&response, "statusText", context)?;
    let init = native::object(context);
    init.set(js_string!("status"), status, false, context)?;
    init.set(js_string!("statusText"), text, false, context)?;
    init.set(
        js_string!("headers"),
        get(&response, "headers", context)?,
        false,
        context,
    )?;
    let body = get(&response, "body", context)?;
    Ok(native::construct_global(context, "Response", &[body, init.into()])?.into())
}

/// `resolveHost(host)`: `{ endpoints: [{ address, family }] }`, or `net::ERR_NAME_NOT_RESOLVED`.
pub(crate) fn resolve_host(context: &mut Context, host: &str) -> JsResult<JsValue> {
    let dns = verve::require(context, "dns")?;
    let promises = get(&dns, "promises", context)?;
    let options = native::object(context);
    options.set(js_string!("all"), true, false, context)?;
    let lookup = invoke(
        &promises,
        "lookup",
        &[js_text(host), options.into()],
        context,
    )?;
    let ok = native::function(context, "", 1, |_, args, cx| {
        let mut endpoints = Vec::new();
        for entry in native::list_of(&arg(args, 0), cx)? {
            let address = get(&entry, "address", cx)?;
            let family = get(&entry, "family", cx)?.as_number().unwrap_or(4.);
            let family = if family == 6. { "ipv6" } else { "ipv4" };
            endpoints.push(
                define::record(cx, &[("address", address), ("family", js_text(family))])?.into(),
            );
        }
        let list = native::array(cx, endpoints);
        Ok(define::record(cx, &[("endpoints", list)])?.into())
    });
    let err = native::function(context, "", 1, |_, _, _| {
        Err(define::error("net::ERR_NAME_NOT_RESOLVED"))
    });
    invoke(&lookup, "then", &[ok.into(), err.into()], context)
}

/// Whether some network interface that is up, is not the loopback and has an address exists.
fn is_online(context: &mut Context) -> JsResult<bool> {
    let os = verve::require(context, "os")?;
    let all = invoke(&os, "networkInterfaces", &[], context)?;
    let names = invoke(
        &native::global(context, "Object")?,
        "values",
        &[all],
        context,
    )?;
    for addresses in native::list_of(&names, context)? {
        for address in native::list_of(&addresses, context)? {
            if !get(&address, "internal", context)?.to_boolean() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
