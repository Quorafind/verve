//! `protocol`: the schemes the main process answers for the pages of a session.
//!
//! `protocol.handle(scheme, handler)` and the older `register*Protocol`/`intercept*Protocol`
//! calls (`protocol.ts` and `protocol.cc` of Electron 39.8.10) put a handler in one table per
//! session, and the session's pages — and the main process's own `net.fetch` — ask it. A scheme
//! the app *registers* is its own (`app:`); one it *intercepts* is a built-in's (`http`, `https`,
//! `file`), whose handler stands in for the network. `handle` picks between the two by the
//! scheme's name. The module's own functions (`protocol.handle`) are the default session's.
//!
//! `registerSchemesAsPrivileged` is the app's, not a session's: it says which custom schemes have
//! an origin (`standard`), are secure contexts (`secure`), may be `fetch`ed (`supportFetchAPI`),
//! and is only allowed before the app is ready.

use std::cell::RefCell;
use std::collections::HashMap;

use boa_engine::{Context, JsObject, JsResult, JsValue};
use verve::native::{self, arg, js_text};

use super::define::{self, Args, Fields, Kind};
use crate::hub::{Privileged, Privileges};
use crate::msg::SessionId;

/// How a handler is called and what it answers with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandlerKind {
    /// `protocol.handle`: `(request) => Response`.
    Handle,
    /// `register*Protocol`: `(request, callback)`, the callback given a path.
    File,
    Buffer,
    String,
    Stream,
    /// The callback is given the address to ask instead.
    Http,
}

impl boa_gc::Finalize for HandlerKind {}
// SAFETY: a plain enum holds no garbage-collected value.
unsafe impl boa_gc::Trace for HandlerKind {
    boa_gc::empty_trace!();
}

/// One scheme's handler.
#[derive(Clone)]
pub(crate) struct Entry {
    pub kind: HandlerKind,
    pub handler: JsValue,
    /// Stands in for a built-in scheme, instead of being a scheme of its own.
    pub intercept: bool,
}

/// The handlers of every session.
#[derive(Default)]
pub(crate) struct Registry {
    entries: RefCell<HashMap<(SessionId, String), Entry>>,
}

impl Registry {
    pub(crate) fn entry(&self, session: SessionId, scheme: &str) -> Option<Entry> {
        let key = (session, scheme.to_owned());
        self.entries.borrow().get(&key).cloned()
    }

    fn has(&self, session: SessionId, scheme: &str, intercept: bool) -> bool {
        self.entry(session, scheme)
            .is_some_and(|entry| entry.intercept == intercept)
    }

    fn insert(&self, session: SessionId, scheme: &str, entry: Entry) -> bool {
        let mut entries = self.entries.borrow_mut();
        let key = (session, scheme.to_owned());
        if entries.contains_key(&key) {
            return false;
        }
        entries.insert(key, entry);
        true
    }

    fn remove(&self, session: SessionId, scheme: &str, intercept: bool) -> bool {
        if !self.has(session, scheme, intercept) {
            return false;
        }
        self.entries
            .borrow_mut()
            .remove(&(session, scheme.to_owned()));
        true
    }
}

/// What a method of the class does.
#[derive(Clone, Copy)]
enum Op {
    Handle,
    Unhandle,
    IsHandled,
    Register(HandlerKind),
    Unregister,
    IsRegistered,
    Intercept(HandlerKind),
    Unintercept,
    IsIntercepted,
}

/// Defines the functions of the class, each running its [`Op`] on the session `this` is of.
macro_rules! methods {
    ($($function:ident => $op:expr),* $(,)?) => {
        $(
            fn $function(this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
                run(this, args, $op, context)
            }
        )*
    };
}

methods! {
    handle => Op::Handle,
    unhandle => Op::Unhandle,
    is_protocol_handled => Op::IsHandled,
    register_file_protocol => Op::Register(HandlerKind::File),
    register_buffer_protocol => Op::Register(HandlerKind::Buffer),
    register_string_protocol => Op::Register(HandlerKind::String),
    register_stream_protocol => Op::Register(HandlerKind::Stream),
    register_http_protocol => Op::Register(HandlerKind::Http),
    register_protocol => Op::Register(HandlerKind::Stream),
    unregister_protocol => Op::Unregister,
    is_protocol_registered => Op::IsRegistered,
    intercept_file_protocol => Op::Intercept(HandlerKind::File),
    intercept_buffer_protocol => Op::Intercept(HandlerKind::Buffer),
    intercept_string_protocol => Op::Intercept(HandlerKind::String),
    intercept_stream_protocol => Op::Intercept(HandlerKind::Stream),
    intercept_http_protocol => Op::Intercept(HandlerKind::Http),
    intercept_protocol => Op::Intercept(HandlerKind::Stream),
    unintercept_protocol => Op::Unintercept,
    is_protocol_intercepted => Op::IsIntercepted,
}

/// The methods of the `Protocol` class: name, `length` and function.
const METHODS: &[(
    &str,
    usize,
    boa_engine::native_function::NativeFunctionPointer,
)] = &[
    ("handle", 2, handle),
    ("unhandle", 1, unhandle),
    ("isProtocolHandled", 1, is_protocol_handled),
    ("registerFileProtocol", 2, register_file_protocol),
    ("registerBufferProtocol", 2, register_buffer_protocol),
    ("registerStringProtocol", 2, register_string_protocol),
    ("registerStreamProtocol", 2, register_stream_protocol),
    ("registerHttpProtocol", 2, register_http_protocol),
    ("registerProtocol", 2, register_protocol),
    ("unregisterProtocol", 1, unregister_protocol),
    ("isProtocolRegistered", 1, is_protocol_registered),
    ("interceptFileProtocol", 2, intercept_file_protocol),
    ("interceptBufferProtocol", 2, intercept_buffer_protocol),
    ("interceptStringProtocol", 2, intercept_string_protocol),
    ("interceptStreamProtocol", 2, intercept_stream_protocol),
    ("interceptHttpProtocol", 2, intercept_http_protocol),
    ("interceptProtocol", 2, intercept_protocol),
    ("uninterceptProtocol", 1, unintercept_protocol),
    ("isProtocolIntercepted", 1, is_protocol_intercepted),
];

/// The class of `session.protocol`.
pub(crate) const PROTOCOL: Kind = Kind("Protocol");

/// Schemes Chromium has a network stack for: `handle` takes them over rather than adding one.
fn is_built_in(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "file")
}

/// The `Protocol` object of session `id`, made the first time.
pub(crate) fn object_of(context: &mut Context, id: SessionId) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(found) = state.net.objects.protocols.borrow().get(&id) {
        return Ok(found.clone());
    }
    let class = protocol_class(context)?;
    let made = class.construct(&[id.into()], None, context)?;
    state
        .net
        .objects
        .protocols
        .borrow_mut()
        .insert(id, made.clone());
    Ok(made)
}

/// The `Protocol` class, made the first time: `session.protocol` is an instance of it, as
/// Electron's is.
fn protocol_class(context: &mut Context) -> JsResult<JsObject> {
    let state = super::shared(context);
    if let Some(class) = state.net.objects.protocol_class.borrow().clone() {
        return Ok(class);
    }
    let (class, _) = native::class(
        context,
        native::ClassSpec {
            name: "Protocol",
            init: |this, args, cx| {
                let id = arg(args, 0).as_number().unwrap_or(0.) as SessionId;
                define::bind(this, PROTOCOL, id, cx);
                Ok(())
            },
            methods: METHODS,
            new_only: Some(native::NewOnly::Native),
            ..native::ClassSpec::default()
        },
    )?;
    *state.net.objects.protocol_class.borrow_mut() = Some(class.clone());
    Ok(class)
}

fn run(this: &JsValue, args: &[JsValue], op: Op, context: &mut Context) -> JsResult<JsValue> {
    let (id, ()) = define::live(this, PROTOCOL, context, |_| Some(()))?;
    let scheme = Args::new(args).string(0, "scheme", context)?;
    let state = super::shared(context);
    let registry = &state.net.protocols;
    let session_id = id;
    let handler = arg(args, 1);
    let outcome = match op {
        Op::Handle => {
            let intercept = is_built_in(&scheme);
            let entry = Entry {
                kind: HandlerKind::Handle,
                handler,
                intercept,
            };
            if !add(context, session_id, &scheme, entry) {
                return Err(define::error(&format!(
                    "Failed to register protocol: {scheme}"
                )));
            }
            true
        }
        Op::Unhandle => {
            if !drop_entry(context, session_id, &scheme, is_built_in(&scheme)) {
                return Err(define::error(&format!(
                    "Failed to unhandle protocol: {scheme}"
                )));
            }
            true
        }
        Op::IsHandled => registry.has(session_id, &scheme, is_built_in(&scheme)),
        Op::Register(kind) => add(
            context,
            session_id,
            &scheme,
            Entry {
                kind,
                handler,
                intercept: false,
            },
        ),
        Op::Intercept(kind) => add(
            context,
            session_id,
            &scheme,
            Entry {
                kind,
                handler,
                intercept: true,
            },
        ),
        Op::Unregister => drop_entry(context, session_id, &scheme, false),
        Op::Unintercept => drop_entry(context, session_id, &scheme, true),
        Op::IsRegistered => registry.has(session_id, &scheme, false),
        Op::IsIntercepted => registry.has(session_id, &scheme, true),
    };
    Ok(match op {
        Op::Handle | Op::Unhandle => JsValue::undefined(),
        _ => outcome.into(),
    })
}

/// Records a handler and tells the session's pages the scheme is answered.
fn add(context: &mut Context, session: SessionId, scheme: &str, entry: Entry) -> bool {
    let state = super::shared(context);
    let added = state.net.protocols.insert(session, scheme, entry);
    if added && let Some(found) = state.hub.session_by_id(session) {
        found.set_handled(scheme, true);
    }
    added
}

fn drop_entry(context: &mut Context, session: SessionId, scheme: &str, intercept: bool) -> bool {
    let state = super::shared(context);
    let removed = state.net.protocols.remove(session, scheme, intercept);
    if removed && let Some(found) = state.hub.session_by_id(session) {
        found.set_handled(scheme, false);
    }
    removed
}

// ─── The module ─────────────────────────────────────────────────────────────────────────────

/// The `protocol` module: the app-wide privileges and the default session's `Protocol`.
pub(crate) fn make(context: &mut Context) -> JsResult<JsValue> {
    let module = native::object(context);
    module.set(
        boa_engine::js_string!("registerSchemesAsPrivileged"),
        native::function(
            context,
            "registerSchemesAsPrivileged",
            1,
            register_privileged,
        ),
        false,
        context,
    )?;
    module.set(
        boa_engine::js_string!("getStandardSchemes"),
        native::function(context, "getStandardSchemes", 0, standard_schemes),
        false,
        context,
    )?;
    for (index, (name, length, _)) in METHODS.iter().enumerate() {
        let method = native::closure(context, name, *length, forward, index);
        module.set(boa_engine::JsString::from(*name), method, false, context)?;
    }
    Ok(module.into())
}

/// A function of the module: the same method of the default session's `Protocol`.
fn forward(
    _: &JsValue,
    args: &[JsValue],
    index: &usize,
    context: &mut Context,
) -> JsResult<JsValue> {
    let default = super::session::default_id(context);
    let protocol = object_of(context, default)?;
    native::invoke(&protocol.into(), METHODS[*index].0, args, context)
}

/// `registerSchemesAsPrivileged([{ scheme, privileges }])`.
fn register_privileged(_: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let state = super::shared(context);
    if state.app.is_ready() {
        return Err(define::error(
            "protocol.registerSchemesAsPrivileged should be called before app is ready",
        ));
    }
    let list = native::list_of(&arg(args, 0), context)
        .map_err(|_| Args::new(args).conversion_failure(0))?;
    let mut granted = state.hub.privileged();
    for item in list {
        let object = item.as_object();
        let fields = Fields::new(object.as_ref());
        let Some(scheme) = fields.string("scheme", context)? else {
            return Err(Args::new(args).conversion_failure(0));
        };
        let privileges = privileges_of(&fields, context)?;
        granted.retain(|known| known.scheme != scheme);
        granted.push(Privileged { scheme, privileges });
    }
    state.hub.set_privileged(granted);
    Ok(JsValue::undefined())
}

fn privileges_of(scheme: &Fields, context: &mut Context) -> JsResult<Privileges> {
    let object = scheme.object("privileges", context)?;
    let fields = Fields::new(object.as_ref());
    Ok(Privileges {
        standard: fields.bool("standard", false, context)?,
        secure: fields.bool("secure", false, context)?,
        bypass_csp: fields.bool("bypassCSP", false, context)?,
        allow_service_workers: fields.bool("allowServiceWorkers", false, context)?,
        support_fetch_api: fields.bool("supportFetchAPI", false, context)?,
        cors_enabled: fields.bool("corsEnabled", false, context)?,
        stream: fields.bool("stream", false, context)?,
        code_cache: fields.bool("codeCache", false, context)?,
    })
}

/// `getStandardSchemes()`: the web's and the app's.
fn standard_schemes(_: &JsValue, _: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let mut names: Vec<String> = ["http", "https", "file", "filesystem", "ftp", "ws", "wss"]
        .map(String::from)
        .into();
    let granted = super::shared(context).hub.privileged();
    names.extend(
        granted
            .into_iter()
            .filter(|p| p.privileges.standard)
            .map(|p| p.scheme),
    );
    let items = names.iter().map(|name| js_text(name)).collect();
    Ok(native::array(context, items))
}
