//! What happens to a request of a session on its way out and back, for a page's `fetch` and for
//! the main process's `net`: its session's cookies go with it and come back, and the session's
//! `webRequest` listeners see it and may change it.
//!
//! The same [`Watch`] serves both. What differs is how a listener is reached ([`Listeners`]): a
//! page sends a message to the main process, where the listeners live, and waits for the reply;
//! the main process runs them where it stands. Asked of nobody, which is the common case, a
//! request goes through without a message.
//!
//! The order is Chromium's, as observed in Electron 39.8.10: the `Cookie` header is attached
//! first, so `onBeforeSendHeaders` sees it; `onHeadersReceived` comes before the response's
//! `Set-Cookie` is stored, so a listener may change what is stored.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use boa_engine::Context;
use url::Url;
use verve::browser_net::{
    Decider, Decision, NetEvent, RequestHead, ResponseDecision, ResponseHead,
};

use crate::hub::Session;
use crate::hub::cookies::{Access, CookieStore, cross_site};
use crate::msg::{CookieChange, WebRequestDetails, WebRequestReply, WebRequestStage, WebResponse};

/// What happens with a listener's answer.
pub(crate) type Then = Box<dyn FnOnce(WebRequestReply, &mut Context)>;

/// How the listeners of a session's `webRequest` are reached.
pub(crate) trait Listeners {
    /// Asks the listener of a stage that blocks; `then` is given its answer, whenever it comes.
    fn ask(
        &self,
        stage: WebRequestStage,
        details: WebRequestDetails,
        then: Then,
        context: &mut Context,
    );

    /// Tells the listener of a stage that does not block.
    fn tell(&self, stage: WebRequestStage, details: WebRequestDetails, context: &mut Context);

    /// Cookies of the session changed: for `cookies.on('changed')`.
    fn changed(&self, changes: Vec<CookieChange>, context: &mut Context);
}

/// A request's cookies and listeners.
pub(crate) struct Watch {
    pub session: Arc<Session>,
    /// The jar the requests use; `None` for requests that use no cookies.
    pub jar: Option<Arc<CookieStore>>,
    pub listeners: Rc<dyn Listeners>,
    /// The page the requests are made from; empty for the main process's own.
    pub page: RefCell<String>,
    /// The bodies of the requests in flight that a listener may be told of, by request id.
    pub uploads: RefCell<HashMap<u64, Vec<u8>>>,
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |since| since.as_secs_f64() * 1000.)
}

/// `User-Agent` for `user-agent`: how Chromium writes the names it sends.
fn title_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut start = true;
    for c in name.chars() {
        out.push(if start { c.to_ascii_uppercase() } else { c });
        start = c == '-';
    }
    out
}

/// `HTTP/1.1 200 OK`.
fn status_line(response: &ResponseHead) -> String {
    format!("HTTP/1.1 {} {}", response.status, response.status_text)
        .trim_end()
        .to_owned()
}

/// The status and reason of a status line a listener gave back.
fn read_status(line: &str) -> Option<(u16, String)> {
    let mut words = line.splitn(3, ' ');
    words.next()?;
    let status = words.next()?.parse().ok()?;
    Some((status, words.next().unwrap_or_default().to_owned()))
}

impl Watch {
    fn url(head: &RequestHead) -> Option<Url> {
        Url::parse(&head.url).ok()
    }

    fn wants(&self, stage: WebRequestStage, url: &Url, head: &RequestHead) -> bool {
        self.session
            .web_request_wants(stage, url, head.resource_type.name())
    }

    fn details(&self, head: &RequestHead) -> WebRequestDetails {
        let ip = Url::parse(&head.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .filter(|host| host.parse::<std::net::IpAddr>().is_ok());
        WebRequestDetails {
            id: head.id,
            url: head.url.clone(),
            method: head.method.clone(),
            resource_type: head.resource_type,
            referrer: self.page.borrow().clone(),
            timestamp: now_ms(),
            upload: self.uploads.borrow().get(&head.id).cloned(),
            request_headers: None,
            response: None,
            ip,
            redirect_url: None,
            error: None,
        }
    }

    fn with_request_headers(
        &self,
        mut details: WebRequestDetails,
        head: &RequestHead,
    ) -> WebRequestDetails {
        let shown = head.headers.iter();
        details.request_headers = Some(shown.map(|(n, v)| (title_case(n), v.clone())).collect());
        details
    }

    fn with_response(mut details: WebRequestDetails, response: &ResponseHead) -> WebRequestDetails {
        details.response = Some(WebResponse {
            status: response.status,
            status_line: status_line(response),
            headers: response.headers.clone(),
        });
        details
    }

    /// Whether the request is made from another site than it goes to.
    fn crosses(&self, url: &Url) -> bool {
        cross_site(&self.page.borrow(), url)
    }

    /// The request is about to be sent: its cookies are attached, and the listeners of
    /// `onBeforeRequest` and `onBeforeSendHeaders` may cancel it, send it elsewhere or change
    /// its headers.
    pub(crate) fn before_send(
        self: &Rc<Self>,
        mut head: RequestHead,
        body: Option<&[u8]>,
        decide: Decider<Decision>,
        context: &mut Context,
    ) {
        let Some(url) = Self::url(&head) else {
            return decide.decide(Decision::Continue(head));
        };
        if let Some(jar) = &self.jar
            && !head.headers.iter().any(|(name, _)| name == "cookie")
        {
            let access = Access {
                scripted: false,
                cross_site: self.crosses(&url),
            };
            let cookie = jar.header(&url, access);
            if !cookie.is_empty() {
                head.headers.push(("cookie".to_owned(), cookie));
            }
        }
        let watched = WebRequestStage::ALL
            .iter()
            .any(|stage| self.wants(*stage, &url, &head));
        match body.filter(|body| watched && !body.is_empty()) {
            Some(body) => {
                self.uploads.borrow_mut().insert(head.id, body.to_vec());
            }
            None => {
                self.uploads.borrow_mut().remove(&head.id);
            }
        }
        if !self.wants(WebRequestStage::BeforeRequest, &url, &head)
            && !self.wants(WebRequestStage::BeforeSendHeaders, &url, &head)
        {
            return decide.decide(Decision::Continue(head));
        }
        self.before_request(head, decide, context);
    }

    fn before_request(
        self: &Rc<Self>,
        head: RequestHead,
        decide: Decider<Decision>,
        context: &mut Context,
    ) {
        let Some(url) = Self::url(&head) else {
            return decide.decide(Decision::Continue(head));
        };
        if !self.wants(WebRequestStage::BeforeRequest, &url, &head) {
            return self.before_headers(head, decide, context);
        }
        let details = self.details(&head);
        let watch = Rc::clone(self);
        let then: Then = Box::new(move |reply, cx| {
            if reply.cancel {
                decide.decide(Decision::Cancel);
            } else if let Some(to) = reply.redirect_url.filter(|to| !to.is_empty()) {
                decide.decide(Decision::Redirect(to));
            } else {
                watch.before_headers(head, decide, cx);
            }
        });
        self.listeners
            .ask(WebRequestStage::BeforeRequest, details, then, context);
    }

    fn before_headers(
        self: &Rc<Self>,
        mut head: RequestHead,
        decide: Decider<Decision>,
        context: &mut Context,
    ) {
        let url = Self::url(&head);
        let wanted = url
            .as_ref()
            .is_some_and(|url| self.wants(WebRequestStage::BeforeSendHeaders, url, &head));
        if !wanted {
            return decide.decide(Decision::Continue(head));
        }
        let details = self.details(&head);
        let details = self.with_request_headers(details, &head);
        let then: Then = Box::new(move |reply, _| {
            if reply.cancel {
                return decide.decide(Decision::Cancel);
            }
            if let Some(headers) = reply.request_headers {
                head.headers = headers;
            }
            decide.decide(Decision::Continue(head));
        });
        self.listeners
            .ask(WebRequestStage::BeforeSendHeaders, details, then, context);
    }

    /// A response's head is in: the listener of `onHeadersReceived` may change it, and its
    /// `Set-Cookie` lines are stored, as changed.
    pub(crate) fn headers_received(
        self: &Rc<Self>,
        request: &RequestHead,
        response: ResponseHead,
        decide: Decider<ResponseDecision>,
        context: &mut Context,
    ) {
        let url = Self::url(request);
        let wanted = url
            .as_ref()
            .is_some_and(|url| self.wants(WebRequestStage::HeadersReceived, url, request));
        if !wanted {
            self.keep_cookies(request, &response, context);
            return decide.decide(ResponseDecision::Continue(response));
        }
        let details = Self::with_response(self.details(request), &response);
        let (watch, request) = (Rc::clone(self), request.clone());
        let then: Then = Box::new(move |reply, cx| {
            if reply.cancel {
                return decide.decide(ResponseDecision::Cancel);
            }
            let mut response = response;
            if let Some(headers) = reply.response_headers {
                response.headers = headers;
            }
            if let Some((status, text)) = reply.status_line.as_deref().and_then(read_status) {
                response.status = status;
                response.status_text = text;
            }
            watch.keep_cookies(&request, &response, cx);
            decide.decide(ResponseDecision::Continue(response));
        });
        self.listeners
            .ask(WebRequestStage::HeadersReceived, details, then, context);
    }

    /// Stores the `Set-Cookie` lines of `response`, when the request uses the session's cookies.
    fn keep_cookies(&self, request: &RequestHead, response: &ResponseHead, context: &mut Context) {
        let (Some(jar), Some(url)) = (&self.jar, Self::url(request)) else {
            return;
        };
        let lines = response
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value.as_str());
        let changes = jar.store_all(&url, lines, self.crosses(&url));
        if !changes.is_empty() && jar.watched() {
            self.listeners.changed(changes, context);
        }
    }

    /// A request moved or ended: the listener of the matching stage is told.
    pub(crate) fn notify(&self, event: NetEvent, context: &mut Context) {
        let (id, over) = match &event {
            NetEvent::Sent(head) => (head.id, false),
            NetEvent::Started { request, .. } | NetEvent::Redirected { request, .. } => {
                (request.id, false)
            }
            NetEvent::Completed { request, .. } | NetEvent::Failed { request, .. } => {
                (request.id, true)
            }
        };
        self.tell(event, context);
        // The body of a request that is over is not kept.
        if over {
            self.uploads.borrow_mut().remove(&id);
        }
    }

    fn tell(&self, event: NetEvent, context: &mut Context) {
        let (stage, request) = match &event {
            NetEvent::Sent(head) => (WebRequestStage::SendHeaders, head),
            NetEvent::Started { request, .. } => (WebRequestStage::ResponseStarted, request),
            NetEvent::Redirected { request, .. } => (WebRequestStage::BeforeRedirect, request),
            NetEvent::Completed { request, .. } => (WebRequestStage::Completed, request),
            NetEvent::Failed { request, .. } => (WebRequestStage::ErrorOccurred, request),
        };
        let Some(url) = Self::url(request) else {
            return;
        };
        if !self.wants(stage, &url, request) {
            return;
        }
        let mut details = self.details(request);
        match event {
            NetEvent::Sent(head) => details = self.with_request_headers(details, &head),
            NetEvent::Started { response, .. } => {
                details = Self::with_response(details, &response);
            }
            NetEvent::Redirected { response, to, .. } => {
                details = Self::with_response(details, &response);
                details.redirect_url = Some(to);
            }
            NetEvent::Completed { response, .. } => {
                details = Self::with_response(details, &response);
                details.error = Some("net::OK".to_owned());
            }
            NetEvent::Failed { error, .. } => details.error = Some(error.name),
        }
        self.listeners.tell(stage, details, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_written_as_chromium_writes_them() {
        assert_eq!(title_case("user-agent"), "User-Agent");
        assert_eq!(title_case("x-t"), "X-T");
        assert_eq!(title_case("cookie"), "Cookie");
    }

    #[test]
    fn a_status_line_is_made_and_read_back() {
        let head = ResponseHead {
            status: 302,
            status_text: "Found".into(),
            headers: Vec::new(),
        };
        assert_eq!(status_line(&head), "HTTP/1.1 302 Found");
        assert_eq!(
            read_status("HTTP/1.1 404 Not Found"),
            Some((404, "Not Found".into()))
        );
        assert_eq!(read_status("HTTP/1.1 204"), Some((204, String::new())));
        assert_eq!(read_status("nonsense"), None);
    }
}
