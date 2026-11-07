//! `session`: the partitions an app's pages live in (`lib/browser/api/session.ts` and
//! `session.cc` of Electron 39.8.10).
//!
//! A [`Session`] is the part of a partition every thread reads (the schemes its main process
//! answers, its user agent); the object a script holds is made here, the first time the session
//! is asked for, and is the same one thereafter. This file has what a session answers by itself —
//! its user agent, where it keeps its files, its `protocol`, its `fetch` — the rest of Electron's
//! session (cookies, `webRequest`, permissions, proxy, downloads) is added by the packages that
//! carry it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};

use super::define::{self, Args, Kind};
use super::permissions::{self, Which};
use super::{app, cookies, download_item, net, protocol, session_proxy, web_request};
use crate::hub::Session;
use crate::msg::SessionId;

/// The class of a session's object.
pub(crate) const SESSION: Kind = Kind("Session");

/// The objects scripts hold for the sessions, by session.
#[derive(Default)]
pub(crate) struct Objects {
    class: RefCell<Option<JsObject>>,
    sessions: RefCell<HashMap<SessionId, JsObject>>,
    pub(crate) protocols: RefCell<HashMap<SessionId, JsObject>>,
    pub(crate) protocol_class: RefCell<Option<JsObject>>,
}

/// The default session's number.
pub(crate) fn default_id(context: &Context) -> SessionId {
    super::shared(context).hub.session_for(None).id()
}

/// The object of `session`, made the first time, with `'session-created'` told to the app.
pub(crate) fn object_of(context: &mut Context, session: &Arc<Session>) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(found) = state.net.objects.sessions.borrow().get(&session.id()) {
        return Ok(found.clone());
    }
    let class = session_class(context)?;
    let made = class.construct(&[session.id().into()], None, context)?;
    state
        .net
        .objects
        .sessions
        .borrow_mut()
        .insert(session.id(), made.clone());
    app::emit_quietly(context, "session-created", &[made.clone().into()]);
    Ok(made)
}

fn session_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.objects.class.borrow().clone() {
        return Ok(class);
    }
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "Session",
            init: |this, args, cx| {
                let id = arg(args, 0).as_number().unwrap_or(0.) as SessionId;
                define::bind(this, SESSION, id, cx);
                verve::events::emitter_init(this, &[], cx)
            },
            parent: Some(&emitter),
            methods: &[
                ("getUserAgent", 0, get_user_agent),
                ("setUserAgent", 1, set_user_agent),
                ("getStoragePath", 0, get_storage_path),
                ("isPersistent", 0, is_persistent),
                ("fetch", 1, fetch),
                ("resolveHost", 1, resolve_host),
                ("downloadURL", 1, download_item::session_download_url),
                ("setProxy", 1, session_proxy::set_proxy),
                ("resolveProxy", 1, session_proxy::resolve_proxy),
                ("setPermissionRequestHandler", 1, |this, args, cx| {
                    permissions::set_handler(cx, this, args, Which::Request)
                }),
                ("setPermissionCheckHandler", 1, |this, args, cx| {
                    permissions::set_handler(cx, this, args, Which::Check)
                }),
                ("setDevicePermissionHandler", 1, |this, args, cx| {
                    permissions::set_handler(cx, this, args, Which::Device)
                }),
            ],
            accessors: &[
                ("protocol", get_protocol, None),
                ("cookies", get_cookies, None),
                ("webRequest", get_web_request, None),
            ],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    *state.net.objects.class.borrow_mut() = Some(class.clone());
    Ok(class)
}

/// The session `this` stands for.
pub(crate) fn session_of(this: &JsValue, context: &mut Context) -> JsResult<Arc<Session>> {
    let hub = Arc::clone(&super::shared(context).hub);
    let (_, found) = define::live(this, SESSION, context, |id| hub.session_by_id(id))?;
    Ok(found)
}

fn get_user_agent(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    let agent = session
        .user_agent()
        .unwrap_or_else(|| app::user_agent(context));
    Ok(js_text(&agent))
}

fn set_user_agent(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    let agent = Args::new(args).string(0, "userAgent", context)?;
    session.set_user_agent(Some(agent));
    Ok(JsValue::undefined())
}

/// `getStoragePath()`: where the session keeps its files; `null` for one in memory.
fn get_storage_path(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    let user_data = super::shared(context).app.user_data();
    Ok(session
        .storage_path(&user_data)
        .map_or(JsValue::null(), |path| js_text(&path.to_string_lossy())))
}

fn is_persistent(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Ok(session_of(this, context)?.is_persistent().into())
}

/// `session.fetch(input, init)`: `net.fetch` through this session.
fn fetch(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    net::fetch(context, &session, &arg(args, 0), &arg(args, 1))
}

fn resolve_host(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    session_of(this, context)?;
    let host = Args::new(args).string(0, "host", context)?;
    net::resolve_host(context, &host)
}

fn get_protocol(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    Ok(protocol::object_of(context, session.id())?.into())
}

fn get_cookies(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    Ok(cookies::object_of(context, &session)?.into())
}

fn get_web_request(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = session_of(this, context)?;
    Ok(web_request::object_of(context, &session)?.into())
}

// ─── The module ─────────────────────────────────────────────────────────────────────────────

/// The `session` module: `defaultSession`, `fromPartition`, `fromPath` and the class.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    let class = session_class(context)?;
    module.set(js_string!("Session"), class, false, context)?;
    native::methods(
        context,
        &module,
        &[
            ("fromPartition", 1, from_partition),
            ("fromPath", 1, from_path),
        ],
    )?;
    native::accessors(
        context,
        &module,
        &[("defaultSession", default_session, None)],
    )?;
    Ok(module.into())
}

fn default_session(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let session = super::shared(context).hub.session_for(None);
    Ok(object_of(context, &session)?.into())
}

/// `fromPartition(partition, { cache })`: `persist:name` keeps its files, any other name does
/// not, and `''` is the default session.
fn from_partition(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let partition = Args::new(args).string(0, "partition", context)?;
    let session = super::shared(context).hub.session_for(Some(&partition));
    Ok(object_of(context, &session)?.into())
}

/// `fromPath(path)`: a session that keeps its files in `path`.
fn from_path(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let path = Args::new(args).string(0, "path", context)?;
    if !std::path::Path::new(&path).is_absolute() {
        return Err(define::error("An absolute path must be provided"));
    }
    let session = super::shared(context)
        .hub
        .session_at(std::path::Path::new(&path));
    Ok(object_of(context, &session)?.into())
}
