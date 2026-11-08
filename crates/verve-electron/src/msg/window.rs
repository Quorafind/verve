//! What a `BrowserWindow` is made with, what is done to it, and what happens to it.

use std::path::PathBuf;

use super::WindowId;

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
