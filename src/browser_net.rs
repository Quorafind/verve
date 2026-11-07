//! The browser's network: a hook that lets an embedder answer what the WHATWG `fetch` sends,
//! and nothing else. Chromium keeps a page's network apart from Node's, and
//! so does this: `http`, `https` and `net` never consult a [`BrowserNet`], only `fetch` (and what is
//! built on it, a page's `XMLHttpRequest` and subresources) does.
//!
//! A realm with no `BrowserNet` fetches exactly as it always has. With one, a request is first
//! asked of [`BrowserNet::custom`]: an address the host answers itself (a scheme registered with
//! `protocol.handle`, or an intercepted `https:`) goes to [`BrowserNet::handle`], and the answer
//! comes back through a [`Responder`], head first and then the body a chunk at a time. The body is
//! pulled, not pushed: the response's `ReadableStream` calls [`BrowserNet::pull`] when its reader
//! wants a chunk, and the host sends the next one only then, so a reader that stops reading stops
//! the host's producer.
//!
//! Nothing here waits. A [`Responder`] may be used from any thread, at any time after it was
//! handed over, even inside the call that handed it; what it says is queued for the
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

    /// The name without its `net::`: the `code` of the `Error` `fetch` fails with.
    pub fn short_name(&self) -> &str {
        self.name.strip_prefix("net::").unwrap_or(&self.name)
    }
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

    /// How a failure rejects the promise of a request this hook watches.
    fn failure_style(&self) -> FailureStyle {
        FailureStyle::Undici
    }
}

// ─── What a host says back ──────────────────────────────────────────────────────────────────

/// What a host can say about a request, queued for the realm.
pub(crate) enum Inbound {
    Head(ResponseHead),
    Chunk(Vec<u8>),
    End,
    Fail(NetError),
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

// ─── What the realm keeps ───────────────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A number nothing else in the process has: a request's id, or a ticket for one answer.
pub(crate) fn fresh_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A realm's browser network: the hook, and the requests that wait on the host.
#[derive(Default)]
pub(crate) struct State {
    pub net: Option<Rc<dyn BrowserNet>>,
    inbox: Option<Arc<Inbox>>,
    pub custom: HashMap<u64, custom::Custom>,
}

impl State {
    /// How many requests wait on the host.
    pub fn pending(&self) -> usize {
        self.custom.len()
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
        custom::heard(ticket, what, cx);
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
