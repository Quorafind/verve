//! What the threads of one Electron app say to each other, and nothing else: the main process's
//! isolate, the isolate of each rendered page, and the shell — the OS thread — that draws their
//! windows.
//!
//! These types are the contract every other package compiles against, so they live here, in one
//! module. A package that needs a variant adds it at the end of a message and records it in
//! `docs/electron/packages/P02-skeleton-and-contracts.md`.

use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, TrySendError};

pub use crate::ids::{ContentsId, WindowId};

/// A JavaScript value copied out of its realm, to cross to another thread: the kernel's
/// structured clone. `Clone`, because [`ToShell`] and the pages' replies are copied by whoever
/// observes them.
pub use verve::clone::Cloned;

// ---------- Main process to shell ----------

/// What the main process asks of the OS thread.
#[derive(Clone, Debug)]
pub enum ToShell {
    CreateWindow {
        window: WindowId,
        contents: ContentsId,
        options: WindowOptions,
    },
    Window {
        window: WindowId,
        op: WindowOp,
    },
    /// The application menu, context menus, the tray and global shortcuts: P15 defines them.
    Menu(MenuOp),
    /// The app is ending: the shell closes what it has and leaves with this code.
    Quit {
        exit_code: i32,
    },
    /// `dialog.showErrorBox`: the box Electron shows for an error nobody caught in the main
    /// process. P14 draws it; until then a shell may only record it.
    ErrorBox {
        title: String,
        content: String,
    },
    /// A dialog (P14): the shell shows it over `parent` and answers `call` as `reply` says.
    Dialog {
        call: u64,
        parent: Option<WindowId>,
        spec: DialogSpec,
        reply: DialogReply,
    },
    /// `nativeTheme.themeSource`, for the shell to draw its windows' frames by (P14).
    SetThemeSource(ThemeSource),
}

/// What `nativeTheme.themeSource` is set to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeSource {
    /// Follow the system.
    #[default]
    System,
    Light,
    Dark,
}

/// A file dialog's filter: `{ name, extensions }`, where `['*']` is every file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// A dialog, as Electron's options say it.
#[derive(Clone, Debug)]
pub enum DialogSpec {
    Open {
        title: String,
        default_path: String,
        button_label: String,
        message: String,
        filters: Vec<FileFilter>,
        properties: OpenProps,
    },
    Save {
        title: String,
        default_path: String,
        button_label: String,
        message: String,
        filters: Vec<FileFilter>,
        name_field_label: String,
        shows_tag_field: bool,
        properties: SaveProps,
    },
    MessageBox {
        kind: MessageKind,
        buttons: Vec<String>,
        /// `-1` when the app named none.
        default_id: i32,
        cancel_id: usize,
        title: String,
        message: String,
        detail: String,
        checkbox_label: String,
        checkbox_checked: bool,
        icon: Option<PathBuf>,
        no_link: bool,
    },
}

/// `showOpenDialog`'s `properties`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OpenProps {
    pub open_file: bool,
    pub open_directory: bool,
    pub multi_selections: bool,
    pub show_hidden_files: bool,
    pub create_directory: bool,
    pub prompt_to_create: bool,
    pub no_resolve_aliases: bool,
    pub treat_package_as_directory: bool,
    pub dont_add_to_recent: bool,
}

/// `showSaveDialog`'s `properties`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SaveProps {
    pub show_hidden_files: bool,
    pub create_directory: bool,
    pub treat_package_as_directory: bool,
    pub show_overwrite_confirmation: bool,
    pub dont_add_to_recent: bool,
}

/// A message box's `type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MessageKind {
    #[default]
    None,
    Info,
    Warning,
    Error,
    Question,
}

/// Where a dialog's answer goes.
#[derive(Clone, Debug)]
pub enum DialogReply {
    /// A [`FromShell::DialogResult`] for the main process to settle a promise with.
    Async,
    /// The main process's thread waits on this channel (`show*Sync`): the only place it waits for
    /// the shell, and the shell never waits for it.
    Sync(SyncSender<DialogResult>),
}

/// What the person answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogResult {
    Open {
        canceled: bool,
        file_paths: Vec<PathBuf>,
    },
    Save {
        canceled: bool,
        file_path: Option<PathBuf>,
    },
    MessageBox {
        response: usize,
        checkbox_checked: bool,
    },
    /// The parent window went, or the app is ending: a cancel.
    Closed,
}

/// `new BrowserWindow(options)`, as the shell needs it. The defaults are Electron's.
#[derive(Clone, Debug)]
pub struct WindowOptions {
    /// 800 × 600, in CSS pixels.
    pub width: f32,
    /// Its height, in CSS pixels.
    pub height: f32,
    /// Its left edge in screen coordinates; `None` centres it.
    pub x: Option<f32>,
    /// Its top edge in screen coordinates; `None` centres it.
    pub y: Option<f32>,
    /// `minWidth` and `minHeight`.
    pub min_size: Option<(f32, f32)>,
    /// `maxWidth` and `maxHeight`.
    pub max_size: Option<(f32, f32)>,
    /// Whether the user may resize it.
    pub resizable: bool,
    /// Whether the user may move it.
    pub movable: bool,
    /// Whether it has a minimize button.
    pub minimizable: bool,
    /// Whether it has a maximize button.
    pub maximizable: bool,
    /// Whether the user may close it.
    pub closable: bool,
    /// Whether it may take focus.
    pub focusable: bool,
    /// Whether it stays above other windows.
    pub always_on_top: bool,
    /// Whether it starts full screen.
    pub fullscreen: bool,
    /// `false` creates the window hidden; `show()` or P13's `ready-to-show` shows it.
    pub show: bool,
    /// `false` is a window without system decoration, dragged by `-webkit-app-region` (P34).
    pub frame: bool,
    /// How the title bar is drawn.
    pub title_bar_style: TitleBarStyle,
    /// Whether the window itself is transparent.
    pub transparent: bool,
    /// The colour the window is cleared to before the page's first frame; `None` is white, as
    /// Electron's windows are.
    pub background_color: Option<[u8; 4]>,
    /// `"Electron"` unless given: Electron's default, ignored once the page has a `<title>`.
    pub title: String,
    /// The window icon, a file.
    pub icon: Option<PathBuf>,
    /// The window this one is a child of.
    pub parent: Option<WindowId>,
    /// Whether it blocks its parent while it is open.
    pub modal: bool,
    /// Whether the taskbar leaves it out.
    pub skip_taskbar: bool,
    /// Whether it starts in kiosk mode.
    pub kiosk: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            width: 800.0,
            height: 600.0,
            x: None,
            y: None,
            min_size: None,
            max_size: None,
            resizable: true,
            movable: true,
            minimizable: true,
            maximizable: true,
            closable: true,
            focusable: true,
            always_on_top: false,
            fullscreen: false,
            show: true,
            frame: true,
            title_bar_style: TitleBarStyle::Default,
            transparent: false,
            background_color: None,
            title: "Electron".into(),
            icon: None,
            parent: None,
            modal: false,
            skip_taskbar: false,
            kiosk: false,
        }
    }
}

/// `titleBarStyle`, minus the overlay P34 does not do yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TitleBarStyle {
    #[default]
    Default,
    Hidden,
    HiddenInset,
}

/// What the main process does to a window.
#[derive(Clone, Debug)]
pub enum WindowOp {
    Show,
    ShowInactive,
    Hide,
    /// As a user clicking the close button: the page may cancel it, so this asks first.
    Close,
    Destroy,
    Focus,
    Blur,
    Minimize,
    Maximize,
    Unmaximize,
    Restore,
    SetFullScreen(bool),
    SetBounds {
        x: Option<f32>,
        y: Option<f32>,
        width: Option<f32>,
        height: Option<f32>,
        animate: bool,
    },
    SetMinSize(f32, f32),
    SetMaxSize(f32, f32),
    Center,
    SetTitle(String),
    SetResizable(bool),
    SetAlwaysOnTop(bool),
    SetBackgroundColor([u8; 4]),
    /// `-1` removes the bar; `2` is indeterminate.
    SetProgressBar(f64),
    SetIcon(PathBuf),
    FlashFrame(bool),
    SetOpacity(f64),
    SetSkipTaskbar(bool),
    SetIgnoreMouseEvents(bool),
    /// The page's answer to `'close'`: `true` when a listener kept the window open.
    CloseAnswer {
        prevented: bool,
    },
    /// `setClosable`: whether the window's close button works.
    SetClosable(bool),
    /// `setMovable`.
    SetMovable(bool),
    /// `setMinimizable`.
    SetMinimizable(bool),
    /// `setMaximizable`.
    SetMaximizable(bool),
    /// `setFocusable`: whether the window may take the keyboard focus.
    SetFocusable(bool),
    /// `setEnabled`: `false` makes the window take no input, as the parent of a shown modal one.
    SetEnabled(bool),
    /// `setParentWindow`: the window the system keeps this one above.
    SetParent(Option<WindowId>),
    /// `moveTop`: above every other window, without the focus.
    MoveTop,
}

/// Menus, trays and global shortcuts. Empty until P15 gives them their first variant.
#[derive(Clone, Debug)]
pub enum MenuOp {}

// ---------- Shell to main process ----------

/// What the shell tells the main process.
#[derive(Clone, Debug)]
pub enum FromShell {
    Window {
        window: WindowId,
        event: WindowEvent,
    },
    App(AppEvent),
    MenuClicked {
        menu_item: u32,
        window: Option<WindowId>,
    },
    /// The displays as they are now, for `screen` (P14). The Hub keeps the latest, so what is
    /// sent before the main process is up is not lost.
    Displays(Vec<Display>),
    /// A dialog was answered (P14), for one asked with [`DialogReply::Async`].
    DialogResult {
        call: u64,
        result: DialogResult,
    },
}

/// What happened to a window.
#[derive(Clone, Debug)]
pub enum WindowEvent {
    Shown,
    Hidden,
    Focus,
    Blur,
    /// A person asked to close the window: the main process emits `'close'`, then answers with
    /// [`WindowOp::CloseAnswer`].
    CloseRequested,
    Closed,
    Resized {
        width: f32,
        height: f32,
    },
    Moved {
        x: f32,
        y: f32,
    },
    Maximized,
    Unmaximized,
    Minimized,
    Restored,
    EnterFullScreen,
    LeaveFullScreen,
    ScaleFactor(f32),
    /// The first frame drawn while the window was visible, sent once: a window that is shown
    /// gets one even if its page never changes (the headless shell sends it when a visible window
    /// has a snapshot, at the snapshot or at the `Show`). A window created with `show: false` has none
    /// until it is shown, so P13's `'ready-to-show'` for a hidden window comes from the
    /// renderer's first snapshot, not from this.
    FirstPaint,
    /// The system's handle of the window (`HWND`, `NSView*` as Electron's is), once, after it opens: what
    /// `getNativeWindowHandle()` answers and the dialogs and menus of P14 and P15 attach to.
    NativeHandle(u64),
}

/// What happened to the app as a whole.
#[derive(Clone, Debug)]
pub enum AppEvent {
    /// The shell is up and the app's entry has run: `app` is ready. Sent once, by the Hub, right
    /// after it starts the main process when a shell is already registered.
    Ready,
    /// macOS clicking the Dock icon; Windows has none.
    Activate {
        has_visible_windows: bool,
    },
    OpenFile(PathBuf),
    OpenUrl(String),
    ThemeChanged {
        dark: bool,
    },
    /// A second launch of the app (P10's single-instance lock).
    SecondInstance {
        argv: Vec<String>,
        cwd: PathBuf,
        /// The `additionalData` the second instance gave, as JSON text: it crossed a process.
        data: String,
    },
    /// The system asks the app to quit (a shutdown, a Cmd+Q no menu took): the same as `app.quit()`.
    QuitRequested,
}

/// A display, as `screen` reports it.
#[derive(Clone, Debug)]
pub struct Display {
    /// `Display.id`.
    pub id: u32,
    /// The display's whole area.
    pub bounds: Rect,
    /// The area the system leaves to windows.
    pub work_area: Rect,
    /// Device pixels per CSS pixel.
    pub scale_factor: f32,
    /// Whether it is the primary display.
    pub primary: bool,
}

impl Default for Display {
    /// The screen a machine with none reports: 1920 x 1080 at 1x, and the primary one.
    fn default() -> Self {
        let screen = Rect {
            x: 0.,
            y: 0.,
            width: 1920.,
            height: 1080.,
        };
        Self {
            id: 1,
            bounds: screen,
            work_area: screen,
            scale_factor: 1.,
            primary: true,
        }
    }
}

/// A box in screen coordinates, as `screen` and `getBounds` read it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// The left edge.
    pub x: f32,
    /// The top edge.
    pub y: f32,
    /// The width.
    pub width: f32,
    /// The height.
    pub height: f32,
}

// ---------- Main process to page ----------

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
    ExecuteJavaScript {
        call: u64,
        code: String,
        user_gesture: bool,
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
    /// The close goes ahead: dispatch `pagehide` and `unload`, then answer with
    /// [`FromRenderer::UnloadReply`].
    Unload {
        call: u64,
    },
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

// ---------- Page to main process ----------

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
}

/// What `webPreferences` says of a page. A page is made with these and, when P20 makes its
/// isolate anew on a navigation, made with the same ones. The fields from `image_animation_policy`
/// on are the ones only P13 reads: they are kept, and handed to whoever draws or loads.
#[derive(Clone, Debug, PartialEq)]
pub struct WebPreferences {
    /// `devTools`.
    pub dev_tools: bool,
    /// `nodeIntegration`: whether the page's scripts see Node's modules.
    pub node_integration: bool,
    /// `nodeIntegrationInWorker`.
    pub node_integration_in_worker: bool,
    /// `nodeIntegrationInSubFrames`.
    pub node_integration_in_sub_frames: bool,
    /// `preload`: the script run before the page's own, an absolute path.
    pub preload: Option<PathBuf>,
    /// `sandbox`: on unless `nodeIntegration` is, or the app said otherwise.
    pub sandbox: bool,
    /// `partition`: the session's name; `None` is the default session.
    pub partition: Option<String>,
    /// `zoomFactor`.
    pub zoom_factor: f64,
    /// `javascript`: whether the page's scripts run at all.
    pub javascript: bool,
    /// `webSecurity`.
    pub web_security: bool,
    /// `allowRunningInsecureContent`.
    pub allow_running_insecure_content: bool,
    /// `images`: whether the page loads images.
    pub images: bool,
    /// `contextIsolation`: whether the page's scripts run in a world of their own, apart from
    /// the preload's. With it (the default) the page has no `require`, `nodeIntegration` or not.
    pub context_isolation: bool,
    /// `imageAnimationPolicy`: `animate`, `animateOnce` or `noAnimation`.
    pub image_animation_policy: String,
    /// `textAreasAreResizable`.
    pub text_areas_are_resizable: bool,
    /// `webgl`: kept; Verve has none.
    pub webgl: bool,
    /// `plugins`.
    pub plugins: bool,
    /// `experimentalFeatures`.
    pub experimental_features: bool,
    /// `scrollBounce`.
    pub scroll_bounce: bool,
    /// `offscreen`.
    pub offscreen: bool,
    /// `webviewTag`.
    pub webview_tag: bool,
    /// `safeDialogs`.
    pub safe_dialogs: bool,
    /// `safeDialogsMessage`.
    pub safe_dialogs_message: Option<String>,
    /// `disableDialogs`.
    pub disable_dialogs: bool,
    /// `navigateOnDragDrop`.
    pub navigate_on_drag_drop: bool,
    /// `disableHtmlFullscreenWindowResize`.
    pub disable_html_fullscreen_window_resize: bool,
    /// `enablePreferredSizeMode`.
    pub enable_preferred_size_mode: bool,
    /// `enableBlinkFeatures`.
    pub enable_blink_features: String,
    /// `disableBlinkFeatures`.
    pub disable_blink_features: String,
    /// `defaultFontFamily`, for the font fallback of P31.
    pub default_font_family: FontFamilies,
    /// `defaultFontSize`.
    pub default_font_size: i32,
    /// `defaultMonospaceFontSize`.
    pub default_monospace_font_size: i32,
    /// `minimumFontSize`.
    pub minimum_font_size: i32,
    /// `defaultEncoding`.
    pub default_encoding: String,
    /// `backgroundThrottling`: timers at 1 Hz and no animation frames while hidden.
    pub background_throttling: bool,
    /// `autoplayPolicy`.
    pub autoplay_policy: String,
    /// `accessibleTitle`.
    pub accessible_title: Option<String>,
    /// `spellcheck`: kept; Verve has none.
    pub spellcheck: bool,
    /// `enableWebSQL`: kept; Verve has none.
    pub enable_web_sql: bool,
    /// `v8CacheOptions`: `none`, `code`, `bypassHeatCheck`, `bypassHeatCheckAndEagerCompile`.
    pub v8_cache_options: String,
    /// `transparent`.
    pub transparent: bool,
    /// `additionalArguments`: strings added to the renderer's `process.argv`.
    pub additional_arguments: Vec<String>,
}

/// `defaultFontFamily`: the family each generic name of CSS stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontFamilies {
    pub standard: String,
    pub serif: String,
    pub sans_serif: String,
    pub monospace: String,
    pub cursive: String,
    pub fantasy: String,
    pub math: String,
}

impl Default for FontFamilies {
    fn default() -> Self {
        let family = |name: &str| name.to_owned();
        Self {
            standard: family("Times New Roman"),
            serif: family("Times New Roman"),
            sans_serif: family("Arial"),
            monospace: family("Courier New"),
            cursive: family("Script"),
            fantasy: family("Impact"),
            math: family("Latin Modern Math"),
        }
    }
}

impl Default for WebPreferences {
    fn default() -> Self {
        Self {
            dev_tools: true,
            node_integration: false,
            node_integration_in_worker: false,
            node_integration_in_sub_frames: false,
            preload: None,
            sandbox: true,
            partition: None,
            zoom_factor: 1.0,
            javascript: true,
            web_security: true,
            allow_running_insecure_content: false,
            images: true,
            context_isolation: true,
            image_animation_policy: "animate".into(),
            text_areas_are_resizable: true,
            webgl: true,
            plugins: false,
            experimental_features: false,
            scroll_bounce: false,
            offscreen: false,
            webview_tag: false,
            safe_dialogs: false,
            safe_dialogs_message: None,
            disable_dialogs: false,
            navigate_on_drag_drop: false,
            disable_html_fullscreen_window_resize: false,
            enable_preferred_size_mode: false,
            enable_blink_features: String::new(),
            disable_blink_features: String::new(),
            default_font_family: FontFamilies::default(),
            default_font_size: 16,
            default_monospace_font_size: 13,
            minimum_font_size: 0,
            default_encoding: "ISO-8859-1".into(),
            background_throttling: true,
            autoplay_policy: "no-user-gesture-required".into(),
            accessible_title: None,
            spellcheck: true,
            enable_web_sql: true,
            v8_cache_options: "code".into(),
            transparent: true,
            additional_arguments: Vec::new(),
        }
    }
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

// ---------- Shell to page, past the main process (the hot path) ----------

/// What a person did to the window, as the page under it is told.
#[derive(Clone, Debug)]
pub enum ToRendererInput {
    /// The page's viewport, in CSS pixels, and the window's scale factor.
    Viewport {
        width: f32,
        height: f32,
        scale: f32,
    },
    ColorScheme {
        dark: bool,
    },
    /// Deprecated: layout runs on the page's own thread in `verve-layout` (P30) and the GPUI shell
    /// no longer reports it. The headless shell still does, from its fake layout, until P30.
    Layout(Vec<(verve_dom::NodeId, verve_dom::Rect)>),
    ScrollOffsets(Vec<(verve_dom::NodeId, [f32; 2])>),
    Frame {
        now: f64,
    },
    Pointer {
        kind: PointerKind,
        pointer: verve_dom::Pointer,
        target: Option<verve_dom::NodeId>,
    },
    Wheel {
        pointer: verve_dom::Pointer,
        dx: f32,
        dy: f32,
        target: Option<verve_dom::NodeId>,
    },
    Key {
        down: bool,
        key: verve_dom::Key,
    },
    /// An input method's commit, or a paste.
    Text(String),
    /// P24 makes it `Update { text, selection } | Commit | Cancel`.
    Composition(CompositionEvent),
    /// The window gained (`true`) or lost focus.
    Focus(bool),
    /// The page is drawn to a screen (`true`) or not: the window was hidden or minimized.
    Visibility(bool),
}

/// What an input method is doing with a field. Empty until P24 gives it its variants.
#[derive(Clone, Debug)]
pub enum CompositionEvent {}

/// What a pointer did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Up,
    Move,
}

/// What the shell reads of a page to draw it: `Guest::View` of [`crate::RendererGuest`].
pub struct RendererView {
    /// The number of the last job the page ran, for whoever waits on one.
    pub applied: u64,
    /// The document as it is now, with what changed since the last view; `None` when nothing did.
    pub update: Option<verve_dom::Update>,
    /// Whether the page asked for an animation frame: send [`ToRendererInput::Frame`].
    pub wants_frame: bool,
    /// The CSS `cursor` of the element under the pointer.
    pub cursor: Option<String>,
    /// Where a candidate window goes: the caret's box (P24/P32).
    pub ime_rect: Option<verve_dom::Rect>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_reply_answers_once_and_never_waits() {
        let (reply, page) = SyncReply::channel();
        assert!(reply.send(Cloned::undefined()));
        assert!(page.try_recv().is_ok());
    }

    #[test]
    fn a_sync_reply_to_a_page_that_left_is_not_an_error() {
        let (reply, page) = SyncReply::channel();
        drop(page);
        assert!(!reply.send(Cloned::undefined()));
    }

    #[test]
    fn a_sync_reply_dropped_unanswered_wakes_the_page() {
        let (reply, page) = SyncReply::channel();
        drop(reply);
        assert!(page.recv().is_err());
    }

    #[test]
    fn a_pages_preferences_default_as_electrons_do() {
        let prefs = WebPreferences::default();
        assert!(prefs.sandbox && prefs.web_security && prefs.javascript && prefs.dev_tools);
        assert!(!prefs.node_integration && !prefs.allow_running_insecure_content);
        assert_eq!(prefs.zoom_factor, 1.0);
    }
}
