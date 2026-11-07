//! A page's share of the browser's network (`verve::browser_net`): the schemes its session's
//! main process answers are asked of the main process, one message each way for every piece of
//! the answer, and the page reads the answer at its own pace.
//!
//! The page pulls: its response stream asks for a chunk ([`BrowserNet::pull`]) and the main
//! process answers each ask with one read of the handler's body, which it sends as one piece or
//! several of at most 64 KiB, so a page that stops reading stops the handler's producer. A
//! page's failures read as Chromium's do (`TypeError: Failed to fetch`).
//!
//! The session's cookies go with each request and its `Set-Cookie` lines are stored without a
//! round trip to the main process, which is told only that they changed. A `webRequest` listener
//! lives in the main process; a request is shown to it by message, and only if the session's
//! filter for that stage wants it ([`crate::net_watch`]).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::Context;
use url::Url;
use verve::Runtime;
use verve::browser_net::{
    BrowserNet, Decider, Decision, FailureStyle, NetError, NetEvent, RequestHead, Responder,
    ResponseDecision, ResponseHead,
};
use verve_dom::CookieBackend;

use crate::api::ipc_renderer::Outbox;
use crate::hub::cookies::{Access, CookieStore, Source};
use crate::hub::{Hub, Privileged, Session};
use crate::msg::{
    CookieChange, FromRenderer, ProtoRequest, SessionId, WebPreferences, WebRequestDetails,
    WebRequestReply, WebRequestStage,
};
use crate::net_watch::{Listeners, Then, Watch};

/// The `webRequest` listeners of a page's session, which are in the main process: a request
/// is sent there and waits for the answer.
struct Asks {
    owner: Arc<Session>,
    session: SessionId,
    outbox: Outbox,
    next: Cell<u64>,
    pending: RefCell<HashMap<u64, Then>>,
}

impl Listeners for Asks {
    fn ask(&self, stage: WebRequestStage, details: WebRequestDetails, then: Then, _: &mut Context) {
        let call = self.next.get() + 1;
        self.next.set(call);
        self.pending.borrow_mut().insert(call, then);
        self.send(call, stage, details);
    }

    fn tell(&self, stage: WebRequestStage, details: WebRequestDetails, _: &mut Context) {
        self.send(0, stage, details);
    }

    fn changed(&self, changes: Vec<CookieChange>, _: &mut Context) {
        let session = self.session;
        self.outbox
            .borrow_mut()
            .push(FromRenderer::Cookies { session, changes });
    }
}

impl Asks {
    fn send(&self, call: u64, stage: WebRequestStage, details: WebRequestDetails) {
        self.owner.note_web_request();
        let message = FromRenderer::WebRequest {
            call,
            session: self.session,
            stage,
            details: Box::new(details),
        };
        self.outbox.borrow_mut().push(message);
    }
}

/// What a page's `fetch` asks of its session.
pub(crate) struct RendererNet {
    session: Arc<Session>,
    outbox: Outbox,
    /// The answers the main process owes, by request.
    open: RefCell<HashMap<u64, Responder>>,
    watch: Rc<Watch>,
    asks: Rc<Asks>,
}

impl RendererNet {
    pub(crate) fn new(session: Arc<Session>, outbox: Outbox, jar: Arc<CookieStore>) -> Rc<Self> {
        let asks = Rc::new(Asks {
            owner: Arc::clone(&session),
            session: session.id(),
            outbox: Rc::clone(&outbox),
            next: Cell::new(0),
            pending: RefCell::default(),
        });
        let watch = Rc::new(Watch {
            session: Arc::clone(&session),
            jar: Some(jar),
            listeners: Rc::clone(&asks) as Rc<dyn Listeners>,
            page: RefCell::default(),
            uploads: RefCell::default(),
        });
        Rc::new(Self {
            session,
            outbox,
            open: RefCell::default(),
            watch,
            asks,
        })
    }

    /// The page is now at `url`: what its requests come from.
    pub(crate) fn set_page(&self, url: &str) {
        self.watch.page.replace(url.to_owned());
    }

    /// A listener of the main process answered `call`.
    pub(crate) fn web_reply(&self, call: u64, reply: WebRequestReply, context: &mut Context) {
        let then = self.asks.pending.borrow_mut().remove(&call);
        if let Some(then) = then {
            then(reply, context);
        }
    }

    /// The page is loading another document: the requests of the last one that wait on the main
    /// process are given up (a request a listener held goes no further), and the main process is
    /// told so, if there was anything to tell (`asked`: the loader had asks out too).
    pub(crate) fn abandon(&self, asked: bool, context: &mut Context) {
        let held: Vec<Then> = self
            .asks
            .pending
            .borrow_mut()
            .drain()
            .map(|(_, t)| t)
            .collect();
        let cancel = WebRequestReply {
            cancel: true,
            ..WebRequestReply::default()
        };
        for then in held {
            then(cancel.clone(), context);
        }
        let streaming = std::mem::take(&mut *self.open.borrow_mut());
        if asked || !streaming.is_empty() {
            self.outbox.borrow_mut().push(FromRenderer::Navigating);
        }
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
    /// What `document.cookie` reads and writes.
    pub cookies: Box<dyn CookieBackend>,
}

/// `document.cookie` of a page: the session's jar, as a script sees it.
struct PageCookies {
    jar: Arc<CookieStore>,
    session: SessionId,
    outbox: Outbox,
}

impl CookieBackend for PageCookies {
    fn get(&self, url: &str) -> String {
        let access = Access {
            scripted: true,
            cross_site: false,
        };
        Url::parse(url).map_or_else(|_| String::new(), |url| self.jar.header(&url, access))
    }

    fn set(&self, url: &str, line: &str) {
        let Ok(url) = Url::parse(url) else { return };
        let changes = self.jar.store(&url, line, Source::Script, false);
        if !changes.is_empty() && self.jar.watched() {
            let session = self.session;
            self.outbox
                .borrow_mut()
                .push(FromRenderer::Cookies { session, changes });
        }
    }
}

/// Gives `runtime` the browser's network of the session `prefs.partition` names, and answers it.
pub(crate) fn install(
    runtime: &mut Runtime,
    hub: &Hub,
    prefs: &WebPreferences,
    outbox: &Outbox,
) -> Network {
    let session = hub.session_for(prefs.partition.as_deref());
    let jar = hub.cookies_of(&session);
    let net = RendererNet::new(Arc::clone(&session), Rc::clone(outbox), Arc::clone(&jar));
    runtime.set_browser_net(Some(Rc::clone(&net) as Rc<dyn BrowserNet>));
    let cookies = Box::new(PageCookies {
        jar,
        session: session.id(),
        outbox: Rc::clone(outbox),
    });
    Network {
        net,
        session,
        privileged: hub.privileged(),
        cookies,
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

    fn before_send(
        &self,
        head: RequestHead,
        body: Option<&[u8]>,
        decide: Decider<Decision>,
        context: &mut Context,
    ) {
        self.watch.before_send(head, body, decide, context);
    }

    fn headers_received(
        &self,
        request: &RequestHead,
        response: ResponseHead,
        decide: Decider<ResponseDecision>,
        context: &mut Context,
    ) {
        self.watch
            .headers_received(request, response, decide, context);
    }

    fn notify(&self, event: NetEvent, context: &mut Context) {
        self.watch.notify(event, context);
    }

    fn failure_style(&self) -> FailureStyle {
        FailureStyle::Chromium
    }

    fn user_agent(&self) -> Option<String> {
        self.session.user_agent()
    }
}
