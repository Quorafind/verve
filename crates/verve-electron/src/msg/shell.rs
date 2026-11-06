//! What the main process asks of the OS thread and what the shell tells it back: windows, the
//! app's own events, displays.

use std::path::PathBuf;

use super::dialog::{DialogReply, DialogResult, DialogSpec};
use super::window::{WindowEvent, WindowOp, WindowOptions};
use super::{ContentsId, WindowId};

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

/// Menus, trays and global shortcuts. Empty until P15 gives them their first variant.
#[derive(Clone, Debug)]
pub enum MenuOp {}

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
    /// A key meant for page `contents`, which the shell sends here and not to the page while the main
    /// process has a `'before-input-event'` listener ([`crate::Hub::intercepts_keys`]); the main process
    /// passes it on with [`crate::Hub::input`] unless a listener prevents it.
    KeyForContents {
        contents: ContentsId,
        down: bool,
        key: verve_dom::Key,
    },
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
