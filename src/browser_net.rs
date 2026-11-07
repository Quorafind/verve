//! The browser's network: a hook that lets an embedder answer, watch and change what the
//! WHATWG `fetch` sends, and nothing else. Chromium keeps a page's network apart from Node's, and
//! so does this: `http`, `https` and `net` never consult a [`BrowserNet`], only `fetch` (and what is
//! built on it, a page's `XMLHttpRequest` and subresources) does.
//!
//! A realm with no `BrowserNet` fetches exactly as it always has. With one, a request is first
//! asked of [`BrowserNet::custom`]: an address the host answers itself (a scheme registered with
//! `protocol.handle`, or an intercepted `https:`) goes to [`BrowserNet::handle`], and the answer
//! comes back through a [`Responder`], head first and then the body a chunk at a time. The body is
//! pulled, not pushed: the response's `ReadableStream` calls [`BrowserNet::pull`] when its reader
//! wants a chunk, and the host sends the next one only then, so a reader that stops reading stops
//! the host's producer. Any other `http(s)` request is offered to [`BrowserNet::before_send`] before it goes
//! out and [`BrowserNet::headers_received`] when its head is back, each answering through a
//! [`Decider`] that may change, cancel or (before sending) redirect it, and the host is told how it
//! ended with [`BrowserNet::notify`].
//!
//! Nothing here waits. A [`Responder`] or [`Decider`] may be used from any thread, at any time
//! after it was handed over, even inside the call that handed it; what it says is queued for the
//! realm and heard on the host's next beat ([`crate::Runtime::run_due`]), which the realm's waker
//! asks for.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use boa_engine::Context;

use crate::WakeSlot;

mod custom;
pub(crate) mod gate;

pub use crate::network::fetch::{InternalFetch, fetch_internal};
pub(crate) use custom::begin as begin_custom;
pub(crate) use gate::Gate;

/// What a request is for, by the names of Electron's `webRequest` `resourceType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceType {
    MainFrame,
    SubFrame,
    Stylesheet,
    Script,
    Image,
    Font,
    Object,
    Xhr,
    Ping,
    CspReport,
    Media,
    WebSocket,
    Other,
}

impl ResourceType {
    /// The name `details.resourceType` carries.
    pub fn name(self) -> &'static str {
        match self {
            Self::MainFrame => "mainFrame",
            Self::SubFrame => "subFrame",
            Self::Stylesheet => "stylesheet",
            Self::Script => "script",
            Self::Image => "image",
            Self::Font => "font",
            Self::Object => "object",
            Self::Xhr => "xhr",
            Self::Ping => "ping",
            Self::CspReport => "cspReport",
            Self::Media => "media",
            Self::WebSocket => "webSocket",
            Self::Other => "other",
        }
    }
}

/// A request as the host sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestHead {
    /// Names the request for its whole life, redirects included; unique in the process.
    pub id: u64,
    pub method: String,
    pub url: String,
    /// The headers the request is sent with, in order: those the caller set, and for a request
    /// that goes to the network the ones `fetch` adds (`accept`, `user-agent`, ...).
    pub headers: Vec<(String, String)>,
    pub resource_type: ResourceType,
    /// The address of the page that made the request; empty when the kernel does not know it.
    pub referrer: String,
    /// The origin of the page that made the request, when the host said which.
    pub initiator: Option<String>,
}

/// A response's status line and headers.
#[derive(Clone, Debug, PartialEq)]
pub struct ResponseHead {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
}

/// A Chromium network error: its number (`-6`) and name (`net::ERR_FILE_NOT_FOUND`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetError {
    pub code: i32,
    pub name: String,
}

/// Chromium's net error numbers (`net/base/net_error_list.h`) that a host or fetch has a reason
/// to name; any other number is named `net::ERR_UNKNOWN`.
const ERRORS: &[(i32, &str)] = &[
    (-2, "ERR_FAILED"),
    (-3, "ERR_ABORTED"),
    (-4, "ERR_INVALID_ARGUMENT"),
    (-6, "ERR_FILE_NOT_FOUND"),
    (-9, "ERR_UNEXPECTED"),
    (-10, "ERR_ACCESS_DENIED"),
    (-11, "ERR_NOT_IMPLEMENTED"),
    (-20, "ERR_BLOCKED_BY_CLIENT"),
    (-21, "ERR_NETWORK_CHANGED"),
    (-100, "ERR_CONNECTION_CLOSED"),
    (-101, "ERR_CONNECTION_RESET"),
    (-102, "ERR_CONNECTION_REFUSED"),
    (-105, "ERR_NAME_NOT_RESOLVED"),
    (-106, "ERR_INTERNET_DISCONNECTED"),
    (-109, "ERR_ADDRESS_UNREACHABLE"),
    (-118, "ERR_CONNECTION_TIMED_OUT"),
    (-130, "ERR_PROXY_CONNECTION_FAILED"),
    (-300, "ERR_INVALID_URL"),
    (-301, "ERR_DISALLOWED_URL_SCHEME"),
    (-302, "ERR_UNKNOWN_URL_SCHEME"),
    (-310, "ERR_TOO_MANY_REDIRECTS"),
    (-324, "ERR_EMPTY_RESPONSE"),
];

impl NetError {
    /// The error numbered `code`, named as Chromium names it.
    pub fn from_code(code: i32) -> Self {
        let name = ERRORS
            .iter()
            .find(|(number, _)| *number == code)
            .map_or("ERR_UNKNOWN", |(_, name)| name);
        Self {
            code,
            name: format!("net::{name}"),
        }
    }

    /// The error a failure Node reports as `code` stands for: a refused connection, a name that
    /// does not resolve; any other is `net::ERR_FAILED`.
    pub(crate) fn from_node_code(code: &str) -> Self {
        Self::from_code(match code {
            "ECONNREFUSED" => -102,
            "ENOTFOUND" | "EAI_AGAIN" => -105,
            "ECONNRESET" => -101,
            "ETIMEDOUT" => -118,
            "ERR_ACCESS_DENIED" => -10,
            "ERR_BLOCKED_BY_CLIENT" => -20,
            _ => -2,
        })
    }

    /// The name without its `net::`: the `code` of the `Error` `fetch` fails with.
    pub fn short_name(&self) -> &str {
        self.name.strip_prefix("net::").unwrap_or(&self.name)
    }
}

/// What the host decides about a request about to be sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Send it with these headers (the rest of the head is the kernel's and is ignored).
    Continue(RequestHead),
    /// Fail it with `net::ERR_BLOCKED_BY_CLIENT`.
    Cancel,
    /// Send it to this address instead, as a redirect would.
    Redirect(String),
}

/// What the host decides about a response whose head has arrived.
#[derive(Clone, Debug, PartialEq)]
pub enum ResponseDecision {
    /// Go on with this status line and these headers.
    Continue(ResponseHead),
    /// Fail the request with `net::ERR_BLOCKED_BY_CLIENT`.
    Cancel,
}

/// How a request ended or moved, told to the host without waiting for it.
#[derive(Clone, Debug, PartialEq)]
pub enum NetEvent {
    /// The request is on its way, with the headers it is sent with.
    Sent(RequestHead),
    /// The response head is in and goes on to the caller.
    Started {
        request: RequestHead,
        response: ResponseHead,
    },
    /// The response is a redirect, followed to `to`.
    Redirected {
        request: RequestHead,
        response: ResponseHead,
        to: String,
    },
    /// The body has been read to its end.
    Completed {
        request: RequestHead,
        response: ResponseHead,
    },
    /// The request failed or was given up on.
    Failed {
        request: RequestHead,
        error: NetError,
    },
}

/// How a failed request rejects `fetch`'s promise.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FailureStyle {
    /// `TypeError: fetch failed`, its `cause` the error underneath: undici's, and the default.
    #[default]
    Undici,
    /// `TypeError: Failed to fetch`: what a page's `fetch` rejects with in Chromium.
    Chromium,
    /// `Error: net::ERR_FILE_NOT_FOUND`: what Electron's `net.fetch` rejects with.
    Net,
}

/// The host's side of the browser's network. See the module documentation. Each method that
/// may run the host's scripts is given the realm's context.
pub trait BrowserNet {
    /// Whether the host answers `url` itself.
    fn custom(&self, url: &url::Url) -> bool;

    /// Answers a request for an address [`BrowserNet::custom`] claimed. `body` is the request's
    /// body, whole. The answer goes through `respond`.
    fn handle(
        &self,
        request: RequestHead,
        body: Option<Vec<u8>>,
        respond: Responder,
        cx: &mut Context,
    );

    /// The reader of request `id`'s response wants a chunk: the host sends the next one. The first
    /// pull comes before any chunk has been sent.
    fn pull(&self, _id: u64, _cx: &mut Context) {}

    /// The reader of request `id`'s response, or the code that made it, gave up: the host may stop.
    fn cancel(&self, _id: u64, _cx: &mut Context) {}

    /// A request is about to go to the network: change it, cancel it or send it elsewhere. `body`
    /// is what it will send. The default sends it unchanged.
    fn before_send(
        &self,
        head: RequestHead,
        _body: Option<&[u8]>,
        decide: Decider<Decision>,
        _cx: &mut Context,
    ) {
        decide.decide(Decision::Continue(head));
    }

    /// A response's head is in: change it or fail the request. The default lets it through.
    fn headers_received(
        &self,
        _request: &RequestHead,
        response: ResponseHead,
        decide: Decider<ResponseDecision>,
        _cx: &mut Context,
    ) {
        decide.decide(ResponseDecision::Continue(response));
    }

    /// A request moved or ended. Nothing is awaited.
    fn notify(&self, _event: NetEvent, _cx: &mut Context) {}

    /// How a failure rejects the promise of a request this hook watches.
    fn failure_style(&self) -> FailureStyle {
        FailureStyle::Undici
    }

    /// The `user-agent` a request is sent with when its caller set none; `None` is the runtime's.
    fn user_agent(&self) -> Option<String> {
        None
    }
}

// ─── What a host says back ──────────────────────────────────────────────────────────────────

/// What a host can say about a request, queued for the realm.
pub(crate) enum Inbound {
    Head(ResponseHead),
    Chunk(Vec<u8>),
    End,
    Fail(NetError),
    /// A [`Decision`]; `None` when the [`Decider`] was dropped, which lets the request pass as it was.
    Request(Option<Decision>),
    Response(Option<ResponseDecision>),
}

/// The realm's mailbox: what other threads say about its requests, and a way to wake it.
pub(crate) struct Inbox {
    queue: Mutex<VecDeque<(u64, Inbound)>>,
    wake: Arc<WakeSlot>,
}

impl Inbox {
    fn push(&self, ticket: u64, what: Inbound) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back((ticket, what));
        }
        self.wake.wake();
    }

    fn pop(&self) -> Option<(u64, Inbound)> {
        self.queue.lock().ok()?.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.queue.lock().is_ok_and(|queue| queue.is_empty())
    }
}

/// The answer to one request [`BrowserNet::handle`] was given: a head, chunks, then an end, or a
/// failure. Dropped without either, the request fails with `net::ERR_FAILED`.
pub struct Responder {
    ticket: u64,
    inbox: Arc<Inbox>,
    finished: bool,
}

impl Responder {
    /// The status line and headers. Sent once, first.
    pub fn head(&self, status: u16, status_text: String, headers: Vec<(String, String)>) {
        let head = ResponseHead {
            status,
            status_text,
            headers,
        };
        self.inbox.push(self.ticket, Inbound::Head(head));
    }

    /// A piece of the body.
    pub fn chunk(&self, bytes: Vec<u8>) {
        self.inbox.push(self.ticket, Inbound::Chunk(bytes));
    }

    /// The body is complete.
    pub fn end(mut self) {
        self.finished = true;
        self.inbox.push(self.ticket, Inbound::End);
    }

    /// The request failed. Before the head, `fetch` rejects as the host's [`FailureStyle`] says
    /// (by default `TypeError: fetch failed`, its `cause` the error); after it, the body errors
    /// with `TypeError: terminated`.
    pub fn fail(mut self, error: NetError) {
        self.finished = true;
        self.inbox.push(self.ticket, Inbound::Fail(error));
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if !self.finished {
            let error = NetError::from_code(-2);
            self.inbox.push(self.ticket, Inbound::Fail(error));
        }
    }
}

/// The host's decision about a request or response, given once. Dropped without one, the
/// request goes on as it was.
pub struct Decider<T> {
    ticket: u64,
    inbox: Arc<Inbox>,
    wrap: fn(Option<T>) -> Inbound,
    decided: bool,
}

impl<T> Decider<T> {
    /// Says what to do.
    pub fn decide(mut self, decision: T) {
        self.decided = true;
        self.inbox.push(self.ticket, (self.wrap)(Some(decision)));
    }
}

impl<T> Drop for Decider<T> {
    fn drop(&mut self) {
        if !self.decided {
            self.inbox.push(self.ticket, (self.wrap)(None));
        }
    }
}

// ─── What the realm keeps ───────────────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A number nothing else in the process has: a request's id, or a ticket for one answer.
pub(crate) fn fresh_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// What is waiting for a host's decision.
pub(crate) enum Waiting {
    Request(RequestThen),
    Response(ResponseThen),
}

/// What happens when the host has decided about a request.
pub(crate) type RequestThen = Box<dyn FnOnce(Option<Decision>, &mut Context)>;
/// What happens when the host has decided about a response head.
pub(crate) type ResponseThen = Box<dyn FnOnce(Option<ResponseDecision>, &mut Context)>;

/// A realm's browser network: the hook, and the requests that wait on the host.
#[derive(Default)]
pub(crate) struct State {
    pub net: Option<Rc<dyn BrowserNet>>,
    inbox: Option<Arc<Inbox>>,
    waiting: HashMap<u64, Waiting>,
    pub custom: HashMap<u64, custom::Custom>,
}

impl State {
    /// How many requests wait on the host.
    pub fn pending(&self) -> usize {
        self.waiting.len() + self.custom.len()
    }
}

/// The realm's mailbox, made the first time it is asked for.
fn inbox(cx: &Context) -> Arc<Inbox> {
    let shared = crate::shared(cx);
    let mut state = shared.browser.borrow_mut();
    let wake = &shared.wake;
    Arc::clone(state.inbox.get_or_insert_with(|| {
        Arc::new(Inbox {
            queue: Mutex::new(VecDeque::new()),
            wake: Arc::clone(wake),
        })
    }))
}

/// The hook the realm's own `fetch` goes through.
pub(crate) fn runtime_net(cx: &Context) -> Option<Rc<dyn BrowserNet>> {
    crate::shared(cx).browser.borrow().net.clone()
}

/// Whether anything waits on the host or has been said and not yet heard.
pub(crate) fn active(cx: &Context) -> bool {
    let shared = crate::shared(cx);
    let state = shared.browser.borrow();
    state.pending() > 0 || state.inbox.as_ref().is_some_and(|inbox| !inbox.is_empty())
}

/// The most things heard in one beat; the rest wait for the next, which is asked for.
const PER_BEAT: usize = 256;

/// Hears what hosts said since the last beat. Answers how many things it heard.
pub(crate) fn deliver(cx: &mut Context) -> usize {
    let Some(inbox) = crate::shared(cx).browser.borrow().inbox.clone() else {
        return 0;
    };
    let mut heard = 0;
    while heard < PER_BEAT {
        let Some((ticket, what)) = inbox.pop() else {
            return heard;
        };
        heard += 1;
        match what {
            Inbound::Request(decision) => {
                let waiting = crate::shared(cx)
                    .browser
                    .borrow_mut()
                    .waiting
                    .remove(&ticket);
                if let Some(Waiting::Request(then)) = waiting {
                    then(decision, cx);
                }
            }
            Inbound::Response(decision) => {
                let waiting = crate::shared(cx)
                    .browser
                    .borrow_mut()
                    .waiting
                    .remove(&ticket);
                if let Some(Waiting::Response(then)) = waiting {
                    then(decision, cx);
                }
            }
            reply => custom::heard(ticket, reply, cx),
        }
        // A job that fails reports itself; there is nothing for this beat to do about it.
        let _ = cx.run_jobs();
    }
    if !inbox.is_empty() {
        crate::shared(cx).wake.wake();
    }
    heard
}

impl crate::Runtime {
    /// Puts the browser's network in the host's hands: from now on this realm's `fetch` asks
    /// `net` about each request, as the module documentation describes. `None` takes the hook
    /// away. Requests already under way finish as they began.
    pub fn set_browser_net(&mut self, net: Option<Rc<dyn BrowserNet>>) {
        crate::shared(&self.context).browser.borrow_mut().net = net;
    }
}

/// The header names lowercased, as `fetch` keeps them.
pub(crate) fn lowercase(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value))
        .collect()
}
