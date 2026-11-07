//! What the main process and a page say to each other: loads, edits, replies, lifecycle, protocol
//! requests.

use std::sync::mpsc::{SyncSender, TrySendError};

use super::{
    Cloned, ContentsId, CookieChange, NetError, ProtoRequest, SessionId, WebRequestDetails,
    WebRequestReply, WebRequestStage,
};

/// What the main process asks of a page.
#[derive(Debug)]
pub enum ToRenderer {
    /// `loadURL` / `loadFile` / `reload`.
    Load {
        url: String,
        options: LoadOptions,
    },
    Ipc {
        channel: String,
        args: Vec<Cloned>,
        /// `0` for the main process.
        sender_id: ContentsId,
    },
    InvokeReply {
        call: u64,
        /// `Err` is the error the handler threw, cloned.
        result: Result<Cloned, Cloned>,
    },
    SendSyncReply {
        call: u64,
        value: Cloned,
    },
    /// Runs `code` in the page and answers with [`FromRenderer::ExecuteJavaScriptReply`]; `world` is `0`
    /// for the page itself, else the isolated world `executeJavaScriptInIsolatedWorld` named.
    ExecuteJavaScript {
        call: u64,
        code: String,
        user_gesture: bool,
        world: i32,
    },
    SetZoomFactor(f64),
    /// `webContents.postMessage`: a message with the ports the page is given, as `MessagePort`s.
    Post {
        channel: String,
        message: Cloned,
        ports: Vec<verve::messaging::PortEnd>,
    },
    /// The main process's answer to a [`FromRenderer::ProtocolRequest`].
    ProtocolResponse {
        call: u64,
        response: ProtocolResponse,
    },
    /// The window is closing: dispatch `beforeunload` and answer with
    /// [`FromRenderer::BeforeUnloadReply`].
    BeforeUnload {
        call: u64,
    },
    /// A line for the page's own console, as an error (what Chromium writes when it refuses a
    /// `window.open`).
    ConsoleError {
        message: String,
    },
    /// The close goes ahead: dispatch `pagehide` and `unload`, then answer with
    /// [`FromRenderer::UnloadReply`].
    Unload {
        call: u64,
    },
    /// `webContents.insertCSS`: the sheet goes in under `key`, which the main process chose; `user`
    /// is `cssOrigin: 'user'`.
    InsertCss {
        key: String,
        css: String,
        user: bool,
    },
    /// `webContents.removeInsertedCSS(key)`.
    RemoveCss {
        key: String,
    },
    /// One of `webContents`' editing methods, done to what the page has selected or focused.
    EditCommand(EditCommand),
    /// `webContents.openDevTools()`, `inspectElement(x, y)`: the page writes the element tree to the
    /// terminal, or with `at` the line of the element under that point. Nothing is said back.
    InspectTree {
        at: Option<(f32, f32)>,
    },
    /// The head of the main process's answer to a [`FromRenderer::Protocol`].
    ProtocolHead {
        call: u64,
        status: u16,
        status_text: String,
        headers: Vec<(String, String)>,
    },
    /// A piece (at most 64 KiB) of the body of the answer to [`FromRenderer::Protocol`] `call`,
    /// sent when the page pulled: one read of the handler's body answers a pull, and a read
    /// longer than 64 KiB is several pieces.
    ProtocolChunk {
        call: u64,
        bytes: Vec<u8>,
    },
    /// The end of the answer to [`FromRenderer::Protocol`] `call`: complete, or failed with `error`.
    ProtocolEnd {
        call: u64,
        error: Option<NetError>,
    },
    /// What the listener of a [`FromRenderer::WebRequest`] `call` answered (P16).
    WebRequestReply {
        call: u64,
        reply: WebRequestReply,
    },
    /// What the app's permission handler answered to a [`FromRenderer::Permission`] `call`.
    PermissionReply {
        call: u64,
        granted: bool,
    },
    /// The inspector window's REPL (P43): the page runs `code` as a script in its own world, the
    /// main one, and answers with [`FromRenderer::InspectorEvalReply`] `call`. Nothing is written
    /// to the page's console for it.
    InspectorEval {
        call: u64,
        code: String,
    },
}

/// What `webContents` does to the text a page has selected or focused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditCommand {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    PasteAndMatchStyle,
    Delete,
    SelectAll,
    Unselect,
    ScrollToTop,
    ScrollToBottom,
    InsertText(String),
}

/// What a page knows of where a context menu was asked for, as `'context-menu'` reports it: the
/// rest of its `params` the main process fills from the page's address.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextMenuParams {
    pub x: f32,
    pub y: f32,
    /// The `href` of the link the pointer is over, resolved.
    pub link_url: String,
    pub link_text: String,
    /// The `src` of an image or media element under the pointer, resolved.
    pub src_url: String,
    /// `none`, `image`, `audio`, `video` or `canvas`.
    pub media_type: String,
    pub is_editable: bool,
    pub selection_text: String,
    pub title_text: String,
    pub alt_text: String,
}

/// The answer to a `sendSync`, sent once.
///
/// The page that asked waits on the other end, so this is one-shot and non-blocking by type:
/// [`SyncReply::send`] consumes it and never waits, and dropping it unanswered wakes the page
/// with an error, which is what a main process that ended or ignored the message owes it.
#[derive(Debug)]
pub struct SyncReply(SyncSender<Cloned>);

impl SyncReply {
    /// A reply, and the end the page waits on.
    pub fn channel() -> (Self, std::sync::mpsc::Receiver<Cloned>) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        (Self(sender), receiver)
    }

    /// Answers the page. `false` when it is no longer waiting.
    pub fn send(self, value: Cloned) -> bool {
        !matches!(
            self.0.try_send(value),
            Err(TrySendError::Disconnected(_) | TrySendError::Full(_))
        )
    }
}

/// What a page's `loadURL` carries besides its address. P20 sends them with the request.
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// `httpReferrer`.
    pub http_referrer: Option<String>,
    /// `userAgent`.
    pub user_agent: Option<String>,
    /// `extraHeaders`, as given: lines of `Name: value`.
    pub extra_headers: Option<String>,
}

/// What a page tells the main process.
#[derive(Debug)]
pub enum FromRenderer {
    Ipc {
        channel: String,
        args: Vec<Cloned>,
    },
    Invoke {
        call: u64,
        channel: String,
        args: Vec<Cloned>,
    },
    SendSync {
        channel: String,
        args: Vec<Cloned>,
        /// The answer, sent back once by the main process: one slot, no waiting on this side.
        reply: SyncReply,
    },
    /// `webContents.sendTo` / `sendToHost`.
    SendTo {
        target: ContentsId,
        channel: String,
        args: Vec<Cloned>,
    },
    ExecuteJavaScriptReply {
        call: u64,
        result: Result<Cloned, Cloned>,
    },
    Lifecycle(PageEvent),
    /// What the page asked of the window that only the embedder can do — `verve-dom`'s requests.
    Dom(verve_dom::Request),
    Console {
        level: verve::Level,
        message: String,
        line: u32,
        source: String,
    },
    /// The page's thread is gone: `'render-process-gone'`.
    Gone {
        reason: GoneReason,
    },
    /// `ipcRenderer.postMessage`: a message with the ports the main process is given.
    Post {
        channel: String,
        message: Cloned,
        ports: Vec<verve::messaging::PortEnd>,
    },
    /// The page needs something at a custom scheme (`protocol.handle`, P16): the main process
    /// runs the handler and answers with [`ToRenderer::ProtocolResponse`].
    ProtocolRequest {
        call: u64,
        request: ProtocolRequest,
    },
    /// `location.reload()`: the page asks for itself again, at `url`.
    Reload {
        url: String,
    },
    /// The answer to [`ToRenderer::BeforeUnload`]: `false` when a handler prevented the unload
    /// (`preventDefault()`, or a `returnValue` that is not empty).
    BeforeUnloadReply {
        call: u64,
        proceed: bool,
    },
    /// `pagehide` and `unload` have been dispatched: the page is ready to go.
    UnloadReply {
        call: u64,
    },
    /// A preload could not be read or threw (P21): the main process emits `'preload-error'` on
    /// the page's `webContents` with `path` and an `Error` of this `name` and `message`.
    PreloadError {
        path: String,
        name: String,
        message: String,
    },
    /// `webFrame.setZoomFactor` (P21): the page's zoom factor is now this.
    ZoomChanged(f64),
    /// The page's `contextmenu` event was not prevented: the main process says `'context-menu'`.
    ContextMenu(ContextMenuParams),
    /// A request of the page's `fetch` for an address the main process answers (a scheme
    /// `protocol.handle` took, P16). `call` is the kernel's request id; the answer is
    /// [`ToRenderer::ProtocolHead`], then chunks the page pulls, then [`ToRenderer::ProtocolEnd`].
    Protocol {
        call: u64,
        session: SessionId,
        request: ProtoRequest,
    },
    /// The reader of the answer to [`FromRenderer::Protocol`] `call` wants a chunk.
    ProtocolPull {
        call: u64,
    },
    /// The page gave up on the answer to [`FromRenderer::Protocol`] `call`.
    ProtocolCancel {
        call: u64,
    },
    /// The page is about to load another document: whatever it asked of the main process for the
    /// last one is of no use now, and the main process stops producing it.
    Navigating,
    /// Cookies of the session changed in the page's thread (a `Set-Cookie` of a response, a
    /// `document.cookie`), for `cookies.on('changed')`. Sent only once a script has asked for
    /// the session's `cookies`.
    Cookies {
        session: SessionId,
        changes: Vec<CookieChange>,
    },
    /// A request of the page that a `webRequest` listener of its session wants to see (P16).
    /// For a stage that blocks (`BeforeRequest`, `BeforeSendHeaders`, `HeadersReceived`) the
    /// request waits for [`ToRenderer::WebRequestReply`] `call`; the other stages have `call`
    /// `0` and are not answered.
    WebRequest {
        call: u64,
        session: SessionId,
        stage: WebRequestStage,
        details: Box<WebRequestDetails>,
    },
    /// The page wants to use `permission` (Electron's name for it) at `url`, and the app has a
    /// handler for it in its session: `check_only` asks what is allowed (`permissions.query`),
    /// otherwise it is a request. The answer is [`ToRenderer::PermissionReply`] `call`. A session
    /// with no such handler allows it, and the page does not ask.
    Permission {
        call: u64,
        session: SessionId,
        permission: String,
        url: String,
        check_only: bool,
    },
    /// The answer to [`ToRenderer::InspectorEval`] `call`: the value as `util.inspect` shows it,
    /// or the error that was thrown shown the same way (`is_error`). Text only, never an object.
    /// The Hub passes it to the shell's inspector and not to the main process.
    InspectorEvalReply {
        call: u64,
        text: String,
        is_error: bool,
    },
}

/// The page's life, in the order `webContents` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageEvent {
    DidStartLoading,
    DomReady,
    DidFinishLoad,
    DidFailLoad {
        code: i32,
        description: String,
        url: String,
    },
    DidNavigate {
        url: String,
    },
    DidNavigateInPage {
        url: String,
    },
    TitleUpdated(String),
    FaviconUpdated(Vec<String>),
    /// There is something to paint for the first time — one of `ready-to-show`'s conditions, which
    /// P13 settles.
    FirstPaintable,
}

/// Electron's `render-process-gone` reasons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoneReason {
    CleanExit,
    AbnormalExit,
    Killed,
    Crashed,
    OutOfMemory,
    LaunchFailed,
    IntegrityFailure,
}

/// What a page asks of a custom scheme's handler in the main process.
#[derive(Clone, Debug)]
pub struct ProtocolRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// What the page wants it for, by the names of Fetch's `destination`: `document`, `script`
    /// (a module too), `style`, `image` (an icon too) or `font`; empty for a page's own `fetch()`.
    pub destination: String,
}

/// A handler's answer to a [`ProtocolRequest`].
#[derive(Clone, Debug)]
pub struct ProtocolResponse {
    pub status: u16,
    pub mime: Option<String>,
    pub headers: Vec<(String, String)>,
    /// The body, or a Chromium net error (`-6`, `ERR_FILE_NOT_FOUND`) for a handler that failed.
    pub body: Result<Vec<u8>, (i32, String)>,
}
