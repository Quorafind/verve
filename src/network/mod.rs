//! Permissioned networking. The realm owns only queues and weak references; all system I/O
//! belongs to one lazy reactor and two DNS workers. A host beat never waits for either.

pub(crate) mod dns;
pub(crate) mod http;
pub(crate) mod reactor;
pub(crate) mod tcp;
pub(crate) mod tls;

use crate::native::{get, invoke};
use boa_engine::{Context, JsObject, JsResult, JsValue, js_string};
use reactor::{Command, Event, Id, Reactor};
use std::collections::HashMap;

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

/// Capabilities are realm-local, default denied and never inherited by workers. Changing the
/// policy closes existing resources. `child_process` is reserved: it does not enable execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Permissions {
    pub outbound: Vec<NetworkRule>,
    pub listen: ListenPermission,
    pub dns: bool,
    pub child_process: bool,
}

impl Permissions {
    pub fn allows_outbound(&self, host: &str, port: u16) -> bool {
        self.outbound.iter().any(|rule| {
            rule.port.is_none_or(|p| p == port)
                && rule
                    .host
                    .as_ref()
                    .is_none_or(|h| h.eq_ignore_ascii_case(host))
        })
    }
    pub fn allows_listen(&self, address: std::net::IpAddr) -> bool {
        self.listen == ListenPermission::Any
            || (self.listen == ListenPermission::Loopback && address.is_loopback())
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub permissions: Permissions,
    pub reactor: Option<Reactor>,
    pub next: Id,
    // WeakRef objects, not the sockets themselves. Dropping the realm closes the reactor.
    pub targets: HashMap<Id, JsObject>,
    pub dns: dns::State,
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
        state.reactor = Some(Reactor::new().map_err(|e| crate::native::type_error(e.to_string()))?);
    }
    state.next += 1;
    let id = state.next;
    state.targets.insert(id, weak);
    crate::native::hidden(target, js_string!("__networkId"), (id as f64).into(), cx)?;
    Ok(id)
}

pub(crate) fn send(cx: &Context, cmd: Command) {
    if let Some(reactor) = &crate::shared(cx).network.borrow().reactor {
        reactor.send(cmd);
    }
}

pub(crate) fn poll(cx: &mut Context, deadline: Option<std::time::Instant>) -> usize {
    let mut count = 0;
    // Bound a beat even when JS and I/O keep producing work.
    for _ in 0..1024 {
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        let event = crate::shared(cx)
            .network
            .borrow_mut()
            .reactor
            .as_mut()
            .and_then(|r| r.events.try_recv().ok());
        let Some((id, event)) = event else {
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
            if let Err(e) = tcp::deliver(id, &target, event, cx) {
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
    count + dns::poll(cx, deadline)
}

pub(crate) fn active(cx: &Context) -> bool {
    let shared = crate::shared(cx);
    let state = shared.network.borrow();
    !state.targets.is_empty() || !state.dns.pending.is_empty()
}
