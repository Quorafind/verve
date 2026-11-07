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

/// The stages of a request `webRequest` has a listener for, in the order a request goes through
/// them (`onBeforeRequest` … `onErrorOccurred`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WebRequestStage {
    BeforeRequest,
    BeforeSendHeaders,
    SendHeaders,
    HeadersReceived,
    ResponseStarted,
    BeforeRedirect,
    Completed,
    ErrorOccurred,
}

impl WebRequestStage {
    /// Every stage, in order.
    pub const ALL: [Self; 8] = [
        Self::BeforeRequest,
        Self::BeforeSendHeaders,
        Self::SendHeaders,
        Self::HeadersReceived,
        Self::ResponseStarted,
        Self::BeforeRedirect,
        Self::Completed,
        Self::ErrorOccurred,
    ];

    /// The stage's place in [`WebRequestStage::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// The name of the method that sets its listener.
    pub fn method(self) -> &'static str {
        match self {
            Self::BeforeRequest => "onBeforeRequest",
            Self::BeforeSendHeaders => "onBeforeSendHeaders",
            Self::SendHeaders => "onSendHeaders",
            Self::HeadersReceived => "onHeadersReceived",
            Self::ResponseStarted => "onResponseStarted",
            Self::BeforeRedirect => "onBeforeRedirect",
            Self::Completed => "onCompleted",
            Self::ErrorOccurred => "onErrorOccurred",
        }
    }

    /// Whether the request waits for the listener's callback.
    pub fn blocks(self) -> bool {
        matches!(
            self,
            Self::BeforeRequest | Self::BeforeSendHeaders | Self::HeadersReceived
        )
    }
}

/// A response as a listener is told of it.
#[derive(Clone, Debug, PartialEq)]
pub struct WebResponse {
    pub status: u16,
    /// `HTTP/1.1 200 OK`.
    pub status_line: String,
    pub headers: Vec<(String, String)>,
}

/// What a `webRequest` listener is told of a request: the fields of `details`, which the main
/// process turns into the object the listener gets.
#[derive(Clone, Debug, PartialEq)]
pub struct WebRequestDetails {
    pub id: u64,
    pub url: String,
    pub method: String,
    pub resource_type: ResourceType,
    /// The page the request is made from; empty for the main process's own.
    pub referrer: String,
    /// Milliseconds since the Unix epoch.
    pub timestamp: f64,
    /// The request's body.
    pub upload: Option<Vec<u8>>,
    /// `requestHeaders`, for the stages that have them.
    pub request_headers: Option<Vec<(String, String)>>,
    pub response: Option<WebResponse>,
    /// `ip`, when the address is one the request names.
    pub ip: Option<String>,
    /// `redirectURL`, for the stages that have it.
    pub redirect_url: Option<String>,
    /// `error`, for `onCompleted` (`net::OK`) and `onErrorOccurred`.
    pub error: Option<String>,
}

/// What a listener's callback said: the fields of `callback({ ... })`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebRequestReply {
    pub cancel: bool,
    pub redirect_url: Option<String>,
    pub request_headers: Option<Vec<(String, String)>>,
    pub response_headers: Option<Vec<(String, String)>>,
    pub status_line: Option<String>,
}
