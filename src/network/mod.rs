//! Permissioned networking. The realm owns only queues and weak references; all system I/O
//! belongs to one lazy reactor and two DNS workers. A host beat never waits for either.

pub(crate) mod dns;
pub(crate) mod fetch;
pub(crate) mod http;
pub(crate) mod reactor;
pub(crate) mod tcp;
pub(crate) mod tls;
pub(crate) mod udp;
pub(crate) mod websocket;

use crate::native::{get, invoke};
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use reactor::{Command, Event, Id, Reactor};
use std::collections::HashMap;

pub use crate::permission::Permissions;

/// An outbound rule matches both the host and port. An empty list grants no connections.
/// `host: None` matches every host; names match case-insensitively, without wildcard expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkRule {
    pub host: Option<String>,
    pub port: Option<u16>,
}

/// Listening and connecting are deliberately independent capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListenPermission {
    #[default]
    Deny,
    Loopback,
    Any,
}

/// Nonblocking transport counters, intended for host observability and acceptance tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NetworkDiagnostics {
    pub reactor_threads: usize,
    pub dns_threads: usize,
    pub open_sockets: usize,
    pub queued_read_bytes: usize,
}

#[derive(Default)]
pub(crate) struct State {
    pub permissions: Permissions,
    /// The filesystem half of `permissions`, resolved for checking.
    pub grants: std::rc::Rc<crate::permission::Grants>,
    /// What the workers this realm starts are granted: nothing, unless the embedder says so.
    pub worker_permissions: Permissions,
    /// PEM certificates TLS clients trust besides the system's, as Node's
    /// `NODE_EXTRA_CA_CERTS` adds them: what `fetch` and a `tls.connect` without `ca` verify with.
    pub trusted: Vec<Vec<u8>>,
    pub reactor: Option<Reactor>,
    pub next: Id,
    // WeakRef objects, not the sockets themselves. Dropping the realm closes the reactor.
    pub targets: HashMap<Id, JsObject>,
    pub dns: dns::State,
    pub revoked: std::collections::VecDeque<(Id, Event)>,
}

pub(crate) fn id(target: &JsValue, cx: &mut Context) -> JsResult<Id> {
    Ok(get(target, "__networkId", cx)?.as_number().unwrap_or(0.) as Id)
}

pub(crate) fn register(target: &JsObject, cx: &mut Context) -> JsResult<Id> {
    let weak_class = cx.global_object().get(js_string!("WeakRef"), cx)?;
    let weak = weak_class
        .as_object()
        .ok_or_else(|| crate::native::type_error("WeakRef"))?
        .construct(&[target.clone().into()], None, cx)?;
    let shared = crate::shared(cx);
    let mut state = shared.network.borrow_mut();
    if state.reactor.is_none() {
        state.reactor = Some(
            Reactor::new(state.permissions.clone(), shared.wake.clone())
                .map_err(|e| crate::native::type_error(e.to_string()))?,
        );
    }
    state.next += 1;
    let id = state.next;
    state.targets.insert(id, weak);
    drop(state);
    crate::native::hidden(target, js_string!("__networkId"), id as f64, cx)?;
    Ok(id)
}

pub(crate) fn send(cx: &Context, cmd: Command) {
    if let Some(reactor) = &crate::shared(cx).network.borrow().reactor {
        reactor.send(cmd);
    }
}

pub(crate) fn diagnostics(cx: &Context) -> NetworkDiagnostics {
    use std::sync::atomic::Ordering::Relaxed;
    let shared = crate::shared(cx);
    let state = shared.network.borrow();
    let Some(reactor) = &state.reactor else {
        return NetworkDiagnostics::default();
    };
    let c = &reactor.counters;
    NetworkDiagnostics {
        reactor_threads: c.reactor_threads.load(Relaxed),
        dns_threads: c.dns_threads.load(Relaxed),
        open_sockets: c.open_sockets.load(Relaxed),
        queued_read_bytes: c.queued_read_bytes.load(Relaxed),
    }
}

pub(crate) fn poll(cx: &mut Context, deadline: Option<std::time::Instant>) -> usize {
    let mut count = 0;
    let mut drained = false;
    // Bound a beat even when JS and I/O keep producing work.
    for _ in 0..1024 {
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        let event = {
            let shared = crate::shared(cx);
            let mut state = shared.network.borrow_mut();
            if let Some(event) = state.revoked.pop_front() {
                Some(event)
            } else {
                state.reactor.as_mut().and_then(|r| {
                    let event = r.events.try_recv().ok()?;
                    let n = match &event.1 {
                        Event::Data(bytes) | Event::Datagram(bytes, _) => bytes.len(),
                        _ => 0,
                    };
                    r.counters
                        .queued_read_bytes
                        .fetch_sub(n, std::sync::atomic::Ordering::Relaxed);
                    Some(event)
                })
            }
        };
        let Some((id, event)) = event else {
            drained = true;
            break;
        };
        if let Event::Dns(result) = event {
            if let Err(e) = dns::deliver(id, result, cx) {
                crate::report(cx, &e);
            }
            count += 1;
            let _ = cx.run_jobs();
            continue;
        }
        let closed = matches!(event, Event::Closed);
        let weak = crate::shared(cx).network.borrow().targets.get(&id).cloned();
        let target = weak
            .and_then(|w| invoke(&w.into(), "deref", &[], cx).ok())
            .and_then(|v| v.as_object());
        if let Some(target) = target {
            let delivered = if get(&target.clone().into(), "__udp", cx)
                .map(|v| v.to_boolean())
                .unwrap_or(false)
            {
                udp::deliver(id, &target, event, cx)
            } else {
                tcp::deliver(id, &target, event, cx)
            };
            if let Err(e) = delivered {
                crate::report(cx, &e);
            }
        } else {
            if !closed {
                send(cx, Command::Close(id));
            }
            crate::shared(cx).network.borrow_mut().targets.remove(&id);
        }
        count += 1;
        let _ = cx.run_jobs();
    }
    // Left for a later beat: the wakes for it were spent on this one, so the host is woken
    // again rather than left asleep with I/O waiting.
    if !drained {
        crate::shared(cx).wake.wake();
    }
    count
}

/// Whether anything is waiting to be delivered without the reactor's say: revoked resources.
pub(crate) fn queued(cx: &Context) -> bool {
    !crate::shared(cx).network.borrow().revoked.is_empty()
}

pub(crate) fn active(cx: &Context) -> bool {
    let shared = crate::shared(cx);
    let state = shared.network.borrow();
    !state.targets.is_empty() || !state.dns.pending.is_empty() || !state.revoked.is_empty()
}

/// `ref()` and `unref()` on a socket or server: whether it keeps a process waiting for it.
pub(crate) fn set_ref(this: &JsValue, refed: bool, cx: &mut Context) -> JsResult<JsValue> {
    if let Some(target) = this.as_object() {
        crate::native::hidden(&target, js_string!("__unref"), !refed, cx)?;
    }
    Ok(this.clone())
}

/// Whether a socket, server or lookup that was not `unref()`ed is still open: what keeps a
/// process (`verve run`) from exiting.
pub(crate) fn keeps_alive(cx: &mut Context) -> bool {
    let (targets, waiting) = {
        let shared = crate::shared(cx);
        let state = shared.network.borrow();
        (
            state.targets.values().cloned().collect::<Vec<_>>(),
            !state.dns.pending.is_empty() || !state.revoked.is_empty(),
        )
    };
    waiting
        || targets.into_iter().any(|weak| {
            match invoke(&weak.into(), "deref", &[], cx)
                .ok()
                .and_then(|v| v.as_object())
            {
                Some(target) => !get(&target.into(), "__unref", cx).is_ok_and(|v| v.to_boolean()),
                None => false,
            }
        })
}
