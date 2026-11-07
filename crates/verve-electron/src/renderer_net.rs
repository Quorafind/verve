//! A page's share of the browser's network (`verve::browser_net`): the schemes its session's
//! main process answers are asked of the main process, one message each way for every piece of
//! the answer, and the page reads the answer at its own pace.
//!
//! The page pulls: its response stream asks for a chunk ([`BrowserNet::pull`]) and the main
//! process answers each ask with one read of the handler's body, which it sends as one piece or
//! several of at most 64 KiB, so a page that stops reading stops the handler's producer. A
//! page's failures read as Chromium's do (`TypeError: Failed to fetch`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::Context;
use verve::Runtime;
use verve::browser_net::{BrowserNet, FailureStyle, NetError, RequestHead, Responder};

use crate::api::ipc_renderer::Outbox;
use crate::hub::{Hub, Privileged, Session};
use crate::msg::{FromRenderer, ProtoRequest, WebPreferences};

/// What a page's `fetch` asks of its session.
pub(crate) struct RendererNet {
    session: Arc<Session>,
    outbox: Outbox,
    /// The answers the main process owes, by request.
    open: RefCell<HashMap<u64, Responder>>,
}

impl RendererNet {
    pub(crate) fn new(session: Arc<Session>, outbox: Outbox) -> Rc<Self> {
        Rc::new(Self {
            session,
            outbox,
            open: RefCell::default(),
        })
    }

    /// The main process sent the head of request `call`'s answer.
    pub(crate) fn head(
        &self,
        call: u64,
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
    ) {
        if let Some(respond) = self.open.borrow().get(&call) {
            respond.head(status, status_text, headers);
        }
    }

    /// A piece of the body.
    pub(crate) fn chunk(&self, call: u64, bytes: Vec<u8>) {
        if let Some(respond) = self.open.borrow().get(&call) {
            respond.chunk(bytes);
        }
    }

    /// The answer is over: whole, or failed with `error`.
    pub(crate) fn end(&self, call: u64, error: Option<NetError>) {
        let respond = self.open.borrow_mut().remove(&call);
        match (respond, error) {
            (Some(respond), None) => respond.end(),
            (Some(respond), Some(error)) => respond.fail(error),
            (None, _) => {}
        }
    }
}

/// What a page's network is made of, from its session.
pub(crate) struct Network {
    pub net: Rc<RendererNet>,
    pub session: Arc<Session>,
    pub privileged: Vec<Privileged>,
}

/// Gives `runtime` the browser's network of the session `prefs.partition` names, and answers it.
pub(crate) fn install(
    runtime: &mut Runtime,
    hub: &Hub,
    prefs: &WebPreferences,
    outbox: &Outbox,
) -> Network {
    let session = hub.session_for(prefs.partition.as_deref());
    let net = RendererNet::new(Arc::clone(&session), Rc::clone(outbox));
    runtime.set_browser_net(Some(Rc::clone(&net) as Rc<dyn BrowserNet>));
    Network {
        net,
        session,
        privileged: hub.privileged(),
    }
}

/// The privileged schemes `pick` selects, by name.
pub(crate) fn scheme_names(
    privileged: &[Privileged],
    pick: fn(&crate::hub::Privileges) -> bool,
) -> Vec<String> {
    let chosen = privileged.iter().filter(|p| pick(&p.privileges));
    chosen.map(|p| p.scheme.clone()).collect()
}

impl BrowserNet for RendererNet {
    fn custom(&self, url: &url::Url) -> bool {
        self.session.handles(url.scheme())
    }

    fn handle(
        &self,
        request: RequestHead,
        body: Option<Vec<u8>>,
        respond: Responder,
        _: &mut Context,
    ) {
        let call = request.id;
        self.open.borrow_mut().insert(call, respond);
        self.outbox.borrow_mut().push(FromRenderer::Protocol {
            call,
            session: self.session.id(),
            request: ProtoRequest::of(request, body),
        });
    }

    fn pull(&self, id: u64, _: &mut Context) {
        if self.open.borrow().contains_key(&id) {
            self.outbox
                .borrow_mut()
                .push(FromRenderer::ProtocolPull { call: id });
        }
    }

    fn cancel(&self, id: u64, _: &mut Context) {
        if self.open.borrow_mut().remove(&id).is_some() {
            self.outbox
                .borrow_mut()
                .push(FromRenderer::ProtocolCancel { call: id });
        }
    }

    fn failure_style(&self) -> FailureStyle {
        FailureStyle::Chromium
    }

    fn user_agent(&self) -> Option<String> {
        self.session.user_agent()
    }
}
