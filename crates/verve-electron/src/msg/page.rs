//! What the main process and a page say to each other: loads, edits, replies, lifecycle, protocol
//! requests.

use std::sync::mpsc::{SyncSender, TrySendError};

use super::{Cloned, ContentsId};

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
