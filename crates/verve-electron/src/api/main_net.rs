//! The browser's network for the main process's own requests (`net.fetch`, `net.request`,
//! `session.fetch`): a [`BrowserNet`] of one session, handed to the kernel's `fetch` for each
//! request rather than installed on the realm, since the main process's global `fetch` is Node's
//! and does not go through a session.
//!
//! A scheme the session's `protocol.handle` took is answered by its handler, on this thread; a
//! `file:` address nobody took is read from the disk or `app.asar`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use boa_engine::Context;
use verve::browser_net::{
    BrowserNet, Decider, Decision, FailureStyle, NetError, NetEvent, RequestHead, Responder,
    ResponseDecision, ResponseHead,
};

use super::protocol_serve::{self, Asker, Job, ToResponder};
use super::web_request::MainListeners;
use crate::hub::Session;
use crate::msg::ProtoRequest;
use crate::net_watch::Watch;

/// The main process asking through `session`.
pub(crate) struct MainNet {
    session: Arc<Session>,
    /// The handlers of the session are passed over (`bypassCustomProtocolHandlers`): `file:`
    /// is read from the disk, and no scheme is answered by a handler.
    bypass: bool,
    /// What the requests send as `user-agent`.
    agent: String,
    /// The session's cookies and `webRequest` listeners, which each request passes.
    watch: Rc<Watch>,
}

impl MainNet {
    /// The main process asking through `session`, as `net.fetch` and `net.request` do. With
    /// `cookies`, the session's cookies go with the request and the ones that come back are kept.
    pub(crate) fn for_request(
        context: &Context,
        session: Arc<Session>,
        (bypass, cookies): (bool, bool),
    ) -> Self {
        let agent = session
            .user_agent()
            .unwrap_or_else(|| super::app::user_agent(context));
        let jar = cookies.then(|| super::shared(context).hub.cookies_of(&session));
        let watch = Rc::new(Watch {
            session: Arc::clone(&session),
            jar,
            listeners: Rc::new(MainListeners {
                session: session.id(),
            }),
            page: RefCell::default(),
            uploads: RefCell::default(),
        });
        Self {
            session,
            bypass,
            agent,
            watch,
        }
    }

    /// Whether a handler of the session answers `scheme` for this request.
    fn handled(&self, scheme: &str) -> bool {
        !self.bypass && self.session.handles(scheme)
    }
}

impl BrowserNet for MainNet {
    fn custom(&self, url: &url::Url) -> bool {
        url.scheme() == "file" || self.handled(url.scheme())
    }

    fn handle(
        &self,
        request: RequestHead,
        body: Option<Vec<u8>>,
        respond: Responder,
        context: &mut Context,
    ) {
        let scheme = url::Url::parse(&request.url)
            .map(|url| url.scheme().to_owned())
            .unwrap_or_default();
        if !self.handled(&scheme) {
            return read_file(context, &request.url, respond);
        }
        let call = Some((Asker::Main, request.id));
        let job = Job {
            session: self.session.id(),
            request: ProtoRequest::of(request, body),
            call,
            auto: false,
        };
        protocol_serve::start(context, job, Box::new(ToResponder(Some(respond))));
    }

    fn pull(&self, id: u64, context: &mut Context) {
        protocol_serve::pull(context, Asker::Main, id);
    }

    fn cancel(&self, id: u64, context: &mut Context) {
        protocol_serve::cancel(context, Asker::Main, id);
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
        FailureStyle::Net
    }

    fn user_agent(&self) -> Option<String> {
        Some(self.agent.clone())
    }
}

/// A `file:` address, answered from the app's virtual filesystem and the disk.
fn read_file(context: &mut Context, address: &str, respond: Responder) {
    let path = url::Url::parse(address)
        .ok()
        .and_then(|url| url.to_file_path().ok());
    let Some(path) = path else {
        return respond.fail(NetError::from_code(crate::msg::ERR_FILE_NOT_FOUND));
    };
    let overlay = super::shared(context).hub.overlay();
    match crate::files::read(overlay.as_ref(), &path) {
        Ok(bytes) => {
            let kind = super::mime::of_path(&path).to_owned();
            respond.head(200, "OK".into(), vec![("content-type".into(), kind)]);
            for piece in bytes.chunks(crate::msg::BODY_CHUNK) {
                respond.chunk(piece.to_vec());
            }
            respond.end();
        }
        Err(error) => respond.fail(error),
    }
}
