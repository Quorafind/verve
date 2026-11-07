//! A request that goes to the network with the host watching: it is shown to the host before it
//! is sent and again when its response head is back, and the host decides each time.

use std::rc::Rc;

use boa_engine::{Context, JsValue};

use super::{
    BrowserNet, Decider, Decision, FailureStyle, Inbound, NetError, NetEvent, RequestHead,
    ResponseDecision, ResponseHead, Waiting, fresh_id, inbox,
};
use crate::native::get;
use crate::network::fetch::{fetch_failed, plain_error};

/// What `fetch` keeps of the host's watch over one request: the head it last sent and the
/// response head it last heard.
pub(crate) struct Gate {
    pub net: Rc<dyn BrowserNet>,
    pub head: RequestHead,
    pub response: Option<ResponseHead>,
}

impl Gate {
    /// The request has been put on the wire with the headers `head` has.
    pub fn sent(&mut self) -> Option<NetEvent> {
        Some(NetEvent::Sent(self.head.clone()))
    }

    /// The response head is in and goes on to the caller.
    pub fn started(&mut self, response: ResponseHead) -> Option<NetEvent> {
        self.response = Some(response.clone());
        let request = self.head.clone();
        Some(NetEvent::Started { request, response })
    }

    /// The response was a redirect, followed to `to`.
    pub fn redirected(&mut self, response: ResponseHead, to: &str) -> Option<NetEvent> {
        let request = self.head.clone();
        let to = to.to_owned();
        Some(NetEvent::Redirected {
            request,
            response,
            to,
        })
    }

    /// The body has been read to its end.
    pub fn completed(&mut self) -> Option<NetEvent> {
        let response = self.response.clone()?;
        let request = self.head.clone();
        Some(NetEvent::Completed { request, response })
    }

    /// The request failed or was given up on.
    pub fn failed(&mut self, error: NetError) -> Option<NetEvent> {
        let request = self.head.clone();
        Some(NetEvent::Failed { request, error })
    }
}

/// The error a request the host cancelled fails with.
pub(crate) fn blocked(cx: &mut Context) -> JsValue {
    let error = NetError::from_code(-20);
    plain_error(cx, &error.name, Some(error.short_name()))
}

/// The rejection for a request that failed with `error`, `cause` being what Verve found
/// underneath, in the shape the host's [`FailureStyle`] asks for.
pub(crate) fn failure(
    cx: &mut Context,
    style: FailureStyle,
    cause: JsValue,
    error: &NetError,
) -> JsValue {
    match style {
        FailureStyle::Undici => fetch_failed(cx, cause),
        FailureStyle::Chromium => crate::native::construct_global(
            cx,
            "TypeError",
            &[crate::native::js_text("Failed to fetch")],
        )
        .map(JsValue::from)
        .unwrap_or_default(),
        FailureStyle::Net => plain_error(cx, &error.name, None),
    }
}

/// The Chromium error a failure stands for, by the Node `code` of the error underneath.
pub(crate) fn net_error_of(cause: &JsValue, cx: &mut Context) -> NetError {
    let code = get(cause, "code", cx).map(|code| crate::print(&code));
    NetError::from_node_code(code.as_deref().unwrap_or_default())
}

/// Shows the host `head` before it is sent; `then` is called, on a later beat, with its decision
/// (`None` when the host let go of it without one).
pub(crate) fn ask_request(
    cx: &mut Context,
    net: &Rc<dyn BrowserNet>,
    (head, body): (RequestHead, Option<Vec<u8>>),
    then: impl FnOnce(Option<Decision>, &mut Context) + 'static,
) {
    let ticket = fresh_id();
    let decider = Decider {
        ticket,
        inbox: inbox(cx),
        wrap: Inbound::Request,
        decided: false,
    };
    crate::shared(cx)
        .browser
        .borrow_mut()
        .waiting
        .insert(ticket, Waiting::Request(Box::new(then)));
    net.before_send(head, body.as_deref(), decider, cx);
}

/// Shows the host a response head that has come back for `request`; `then` is called, on a later
/// beat, with its decision.
pub(crate) fn ask_response(
    cx: &mut Context,
    net: &Rc<dyn BrowserNet>,
    request: &RequestHead,
    response: ResponseHead,
    then: impl FnOnce(Option<ResponseDecision>, &mut Context) + 'static,
) {
    let ticket = fresh_id();
    let decider = Decider {
        ticket,
        inbox: inbox(cx),
        wrap: Inbound::Response,
        decided: false,
    };
    crate::shared(cx)
        .browser
        .borrow_mut()
        .waiting
        .insert(ticket, Waiting::Response(Box::new(then)));
    net.headers_received(request, response, decider, cx);
}
