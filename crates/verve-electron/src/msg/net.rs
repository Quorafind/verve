//! What a page and the main process say about the network: the requests of a page's `fetch` for
//! a scheme the main process answers (`protocol.handle`).

pub use verve::browser_net::{NetError, ResourceType};

/// The most bytes of a body that one message or one write carries.
pub const BODY_CHUNK: usize = 64 * 1024;

/// Chromium's `net::ERR_FAILED`: a failure with no more precise cause.
pub const ERR_FAILED: i32 = -2;
/// `net::ERR_ACCESS_DENIED`: the file may not be read.
pub const ERR_ACCESS_DENIED: i32 = -10;
/// `net::ERR_FILE_NOT_FOUND`.
pub const ERR_FILE_NOT_FOUND: i32 = -6;
/// `net::ERR_UNEXPECTED`: a handler that threw, rejected or answered something that is no `Response`.
pub const ERR_UNEXPECTED: i32 = -9;
/// `net::ERR_UNKNOWN_URL_SCHEME`: nobody answers for the scheme.
pub const ERR_UNKNOWN_URL_SCHEME: i32 = -302;

/// Whether `status` is one a client follows to a `Location`.
pub fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Names a session (`session.fromPartition`) in a message.
pub type SessionId = u32;

impl ProtoRequest {
    /// What a handler is shown of the kernel's `request` and the `body` that came with it.
    pub fn of(request: verve::browser_net::RequestHead, body: Option<Vec<u8>>) -> Self {
        Self {
            url: request.url,
            method: request.method,
            headers: request.headers,
            referrer: request.referrer,
            body,
            resource_type: request.resource_type,
        }
    }
}

/// A page's `fetch` of an address the main process answers.
#[derive(Clone, Debug)]
pub struct ProtoRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    /// The request's referrer: the kernel gives a request none, so this is always `''`, as a
    /// handler of Electron 39.8.10 sees (probe3).
    pub referrer: String,
    /// The request's body, whole.
    pub body: Option<Vec<u8>>,
    pub resource_type: ResourceType,
}
