//! `session.cookies`: the jar of a session, read and written by a script (`cookies.cc` of
//! Electron 39.8.10; its errors and the shape of a `Cookie` are observed, see P16's contract).
//!
//! The jar itself is [`CookieStore`], where every thread reaches it; this is the object a script
//! holds, and the `changed` event: a change made in the main process is emitted here directly,
//! a change a page made arrives as [`FromRenderer::Cookies`](crate::msg::FromRenderer).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use url::Url;
use verve::native::{self, ClassSpec, NewOnly, arg, js_text};

use super::define::{self, Args, Fields, Kind};
use crate::hub::Session;
use crate::hub::cookies::{CookieStore, Filter, default_path, domain_match, host_of};
use crate::msg::{Cookie, CookieChange, SameSite, SessionId};

/// The class of a session's `cookies`.
const COOKIES: Kind = Kind("Cookies");

/// The objects scripts hold for the sessions' cookies.
#[derive(Default)]
pub(crate) struct Objects {
    class: RefCell<Option<JsObject>>,
    by_session: RefCell<HashMap<SessionId, JsObject>>,
}

/// What `set` answers a name or value it will not take (`net::CookieInclusionStatus`, joined
/// with the text before it as Electron does, without a space).
const CONTROL: &str = "Failed to set cookie - The cookie contains ASCII control charactersEXCLUDE_DISALLOWED_CHARACTER, DO_NOT_WARN, NO_EXEMPTION";
const DOMAIN: &str = "Failed to set cookie - The cookie was set with an invalid Domain attribute.EXCLUDE_INVALID_DOMAIN, DO_NOT_WARN, NO_EXEMPTION";

/// The `cookies` of `session`, made the first time. From then on the session's changes are told.
pub(crate) fn object_of(context: &mut Context, session: &Arc<Session>) -> JsResult<JsObject> {
    let state = super::shared(context);
    state.hub.cookies_of(session).watch();
    if let Some(found) = state.net.cookies.by_session.borrow().get(&session.id()) {
        return Ok(found.clone());
    }
    let class = cookies_class(context)?;
    let made = class.construct(&[session.id().into()], None, context)?;
    state
        .net
        .cookies
        .by_session
        .borrow_mut()
        .insert(session.id(), made.clone());
    Ok(made)
}

fn cookies_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.cookies.class.borrow().clone() {
        return Ok(class);
    }
    let emitter = define::emitter(context)?;
    let (class, _) = native::class(
        context,
        ClassSpec {
            name: "Cookies",
            init: |this, args, cx| {
                let id = arg(args, 0).as_number().unwrap_or(0.) as SessionId;
                define::bind(this, COOKIES, id, cx);
                verve::events::emitter_init(this, &[], cx)
            },
            parent: Some(&emitter),
            methods: &[
                ("get", 1, find_cookies),
                ("set", 1, put_cookie),
                ("remove", 2, delete_cookie),
                ("flushStore", 0, flush_cookies),
            ],
            new_only: Some(NewOnly::Native),
            ..ClassSpec::default()
        },
    )?;
    *state.net.cookies.class.borrow_mut() = Some(class.clone());
    Ok(class)
}

/// The session `this` stands for, and its cookies.
fn store_of(this: &JsValue, context: &mut Context) -> JsResult<(Arc<Session>, Arc<CookieStore>)> {
    let hub = Arc::clone(&super::shared(context).hub);
    let (_, session) = define::live(this, COOKIES, context, |id| hub.session_by_id(id))?;
    let store = hub.cookies_of(&session);
    Ok((session, store))
}

/// The object a script sees for `cookie`: the fields of Electron's `Cookie`, in its order.
fn cookie_object(context: &mut Context, cookie: &Cookie) -> JsResult<JsValue> {
    let domain = if cookie.host_only {
        cookie.domain.clone()
    } else {
        format!(".{}", cookie.domain)
    };
    let mut fields = vec![
        ("name", js_text(&cookie.name)),
        ("value", js_text(&cookie.value)),
        ("domain", js_text(&domain)),
        ("hostOnly", cookie.host_only.into()),
        ("path", js_text(&cookie.path)),
        ("secure", cookie.secure.into()),
        ("httpOnly", cookie.http_only.into()),
        ("session", cookie.expires.is_none().into()),
    ];
    if let Some(at) = cookie.expires {
        fields.push(("expirationDate", at.into()));
    }
    fields.push(("sameSite", js_text(cookie.same_site.name())));
    Ok(define::record(context, &fields)?.into())
}

/// Tells `changed` listeners of the cookies of session `id` what happened.
pub(crate) fn emit_changes(context: &mut Context, id: SessionId, changes: Vec<CookieChange>) {
    let found = super::shared(context)
        .net
        .cookies
        .by_session
        .borrow()
        .get(&id)
        .cloned();
    let Some(object) = found else { return };
    for change in changes {
        let made = cookie_object(context, &change.cookie);
        let Some(cookie) = define::report(context, made) else {
            continue;
        };
        let args = [cookie, js_text(change.cause.name()), change.removed.into()];
        define::emit_reporting(context, &object, "changed", &args);
    }
}

/// `get(filter)`: the cookies the filter selects.
fn find_cookies(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (session, store) = store_of(this, context)?;
    let wanted = Args::new(args);
    wanted.require(1)?;
    let given = wanted.object_opt(0, "filter", context)?;
    let fields = Fields::new(given.as_ref());
    let flag = |name: &str, cx: &mut Context| -> JsResult<Option<bool>> {
        let value = given.as_ref().map_or(Ok(JsValue::undefined()), |o| {
            o.get(boa_engine::JsString::from(name), cx)
        })?;
        Ok(value.as_boolean())
    };
    let url = fields.string("url", context)?;
    let filter = Filter {
        url: url.as_deref().map(Url::parse).transpose().ok().flatten(),
        name: fields.string("name", context)?,
        domain: fields.string("domain", context)?,
        path: fields.string("path", context)?,
        secure: flag("secure", context)?,
        session: flag("session", context)?,
        http_only: flag("httpOnly", context)?,
    };
    let (found, swept) = if url.is_some() && filter.url.is_none() {
        (Vec::new(), Vec::new())
    } else {
        store.query(&filter)
    };
    emit_changes(context, session.id(), swept);
    let mut list = Vec::new();
    for cookie in &found {
        list.push(cookie_object(context, cookie)?);
    }
    let list = native::array(context, list);
    native::settled(Ok(list), context)
}

/// What `set` makes of its details, or why the promise is rejected.
fn cookie_of(details: &JsObject, context: &mut Context) -> JsResult<Result<Cookie, String>> {
    let fields = Fields::new(Some(details));
    let Some(address) = fields.string("url", context)? else {
        return Ok(Err("Missing required option 'url'".to_owned()));
    };
    let Some(url) = Url::parse(&address).ok().filter(|u| u.host_str().is_some()) else {
        return Ok(Err(DOMAIN.to_owned()));
    };
    let host = host_of(&url).unwrap_or_default();
    let name = fields.string("name", context)?.unwrap_or_default();
    let value = fields.string("value", context)?.unwrap_or_default();
    let bad = |text: &str| {
        text.chars()
            .any(|c| c.is_control() && c != '\t' || c == ';')
    };
    if bad(&name) || bad(&value) || name.contains('=') {
        return Ok(Err(CONTROL.to_owned()));
    }
    let given = fields.string("domain", context)?.filter(|d| !d.is_empty());
    let domain = given.map(|d| d.trim_start_matches('.').to_ascii_lowercase());
    if domain.as_ref().is_some_and(|d| !domain_match(&host, d)) {
        return Ok(Err(DOMAIN.to_owned()));
    }
    let same_site = match fields.string("sameSite", context)? {
        None => SameSite::Lax,
        Some(text) => match SameSite::from_name(&text) {
            Some(same_site) => same_site,
            None => {
                let why =
                    format!("Failed to convert '{text}' to an appropriate cookie same site value");
                return Ok(Err(why));
            }
        },
    };
    let is_address = host.parse::<std::net::IpAddr>().is_ok();
    Ok(Ok(Cookie {
        name,
        value,
        host_only: domain.is_none() || is_address,
        domain: domain.unwrap_or(host),
        path: fields
            .string("path", context)?
            .unwrap_or_else(|| default_path(&url)),
        // A cookie that is sent to every site is sent only over a secure transport.
        secure: fields.bool("secure", false, context)? || same_site == SameSite::NoRestriction,
        http_only: fields.bool("httpOnly", false, context)?,
        same_site,
        expires: fields.f64_opt("expirationDate", context)?,
        created: 0,
    }))
}

/// `set(details)`: puts a cookie in the jar.
fn put_cookie(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (session, store) = store_of(this, context)?;
    let wanted = Args::new(args);
    wanted.require(1)?;
    let details = wanted
        .object_opt(0, "details", context)?
        .ok_or_else(|| wanted.conversion_failure(0))?;
    let outcome = match cookie_of(&details, context)? {
        Ok(cookie) => {
            emit_changes(context, session.id(), store.set(cookie));
            Ok(JsValue::undefined())
        }
        Err(message) => Err(define::error(&message)),
    };
    native::settled(outcome, context)
}

/// `remove(url, name)`: takes away the cookies of that name the address is sent.
fn delete_cookie(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (session, store) = store_of(this, context)?;
    let wanted = Args::new(args);
    wanted.require(2)?;
    let (address, name) = (
        wanted.string(0, "url", context)?,
        wanted.string(1, "name", context)?,
    );
    if let Ok(url) = Url::parse(&address) {
        emit_changes(context, session.id(), store.remove(&url, &name));
    }
    native::settled(Ok(JsValue::undefined()), context)
}

/// `flushStore()`: writes the jar to its file now.
fn flush_cookies(this: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let (_, store) = store_of(this, context)?;
    store.flush();
    native::settled(Ok(JsValue::undefined()), context)
}
