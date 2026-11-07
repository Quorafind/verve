//! The browser's network for the main process's own requests (`net.fetch`, `session.fetch`): a
//! [`BrowserNet`] of one session, handed to the kernel's `fetch` for each request rather than
//! installed on the realm, since the main process's global `fetch` is Node's and does not go
//! through a session.
//!
//! A `file:` address is read from the disk or `app.asar`.

use boa_engine::Context;
use verve::browser_net::{BrowserNet, FailureStyle, NetError, RequestHead, Responder};

use crate::hub::Session;

/// The main process asking through a session.
pub(crate) struct MainNet {
    /// What the requests send as `user-agent`.
    agent: String,
}

impl MainNet {
    /// The main process asking through `session`, as `net.fetch` does.
    pub(crate) fn for_request(context: &Context, session: &Session) -> Self {
        let agent = session
            .user_agent()
            .unwrap_or_else(|| super::app::user_agent(context));
        Self { agent }
    }
}

impl BrowserNet for MainNet {
    fn custom(&self, url: &url::Url) -> bool {
        url.scheme() == "file"
    }

    fn handle(
        &self,
        request: RequestHead,
        _: Option<Vec<u8>>,
        respond: Responder,
        context: &mut Context,
    ) {
        read_file(context, &request.url, respond);
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
