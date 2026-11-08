//! Menus, trays and global shortcuts, as the main process and the shell speak of them: the
//! menu a script built, whole, and what a person did to the native copy of it.
//!
//! The main process holds the truth: a `Menu` and its `MenuItem`s are objects there, and the
//! shell is sent the whole [`MenuSpec`] of a menu whenever any of it changes (menus are small,
//! so there is no diff). The shell keeps the native menus it built from them, and says only
//! which item was chosen.

use std::sync::Arc;

use super::WindowId;
use crate::accelerator::Accelerator;
use crate::pixels::NativeImage;

/// One `Menu` instance.
pub type MenuId = u32;

/// One `MenuItem` instance, unique across all menus: what [`super::FromShell::MenuClicked`] names.
pub type MenuItemId = u32;

/// One `Tray` instance.
pub type TrayId = u32;

/// What a menu item is.
#[derive(Clone, Debug, PartialEq)]
pub enum MenuItemKind {
    Normal,
    Separator,
    /// An item that opens `MenuSpec`.
    Submenu(MenuSpec),
    Checkbox,
    Radio,
    /// Drawn as `Normal` until a macOS 14 header is: Electron's `header` and `palette`.
    Header,
    Palette,
}

/// A menu item, as the shell builds it.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuItemSpec {
    pub id: MenuItemId,
    pub kind: MenuItemKind,
    /// The item's `role`, lower-cased as Electron keeps it.
    pub role: Option<String>,
    /// The label, with the role's default where the script gave none and with its `&`
    /// mnemonics as written (`&&` is a literal `&`).
    pub label: String,
    pub sublabel: String,
    pub tool_tip: String,
    pub accelerator: Option<Accelerator>,
    pub icon: Option<Arc<NativeImage>>,
    pub enabled: bool,
    pub visible: bool,
    pub checked: bool,
    /// Whether the system is given the accelerator to handle; `false` for the edit roles, whose
    /// shortcut the page's own editing handles.
    pub register_accelerator: bool,
    /// macOS only: whether the accelerator of an item with `visible: false` still works.
    pub accelerator_works_when_hidden: bool,
}

/// A menu and its items, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuSpec {
    pub id: MenuId,
    pub items: Vec<MenuItemSpec>,
}

/// What the main process asks of the shell about menus, trays and global shortcuts.
#[derive(Clone, Debug)]
pub enum MenuOp {
    /// A menu is new or changed: this is all of it now.
    Upsert(MenuSpec),
    /// The main process no longer has the menu.
    Drop(MenuId),
    /// `Menu.setApplicationMenu`; macOS shows it as the menu bar, elsewhere each window has its
    /// own copy ([`MenuOp::SetWindowMenu`]).
    SetApplicationMenu(Option<MenuId>),
    /// `win.setMenu(menu)` and `win.removeMenu()`.
    SetWindowMenu {
        window: WindowId,
        menu: Option<MenuId>,
    },
    /// `setMenuBarVisibility` and `setAutoHideMenuBar`.
    SetMenuBarVisibility {
        window: WindowId,
        visible: bool,
        auto_hide: bool,
    },
    /// `menu.popup(...)`: shown at `at` (window coordinates; `None` is where the pointer is) and
    /// answered with [`super::FromShell::MenuWillClose`] carrying `call`.
    Popup {
        menu: MenuId,
        window: WindowId,
        at: Option<(f32, f32)>,
        positioning_item: Option<usize>,
        call: u64,
    },
    /// `menu.closePopup()`.
    ClosePopup {
        window: Option<WindowId>,
    },
    TrayCreate {
        tray: TrayId,
        image: Arc<NativeImage>,
        guid: Option<String>,
    },
    TraySet {
        tray: TrayId,
        op: TrayOp,
    },
    TrayDestroy {
        tray: TrayId,
    },
    /// `globalShortcut.register`: `id` names the registration, and `reply` is told whether the
    /// system accepted it. The main process waits for the answer, for at most 500 ms.
    ShortcutRegister {
        id: u32,
        accelerator: Accelerator,
        reply: std::sync::mpsc::SyncSender<bool>,
    },
    ShortcutUnregister {
        id: u32,
    },
    ShortcutUnregisterAll,
}

/// What `Tray`'s methods ask of the icon.
#[derive(Clone, Debug)]
pub enum TrayOp {
    Image(Arc<NativeImage>),
    PressedImage(Option<Arc<NativeImage>>),
    ToolTip(String),
    Title(String),
    ContextMenu(Option<MenuId>),
    PopUpContextMenu {
        menu: Option<MenuId>,
        at: Option<(f32, f32)>,
    },
    CloseContextMenu,
    IgnoreDoubleClick(bool),
    DisplayBalloon {
        icon: Option<Arc<NativeImage>>,
        icon_type: String,
        title: String,
        content: String,
        large_icon: bool,
        no_sound: bool,
        respect_quiet_time: bool,
    },
    RemoveBalloon,
    Focus,
}

/// What a person did to a tray icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    Click,
    RightClick,
    DoubleClick,
    MiddleClick,
    BalloonShow,
    BalloonClick,
    BalloonClosed,
    MouseEnter,
    MouseLeave,
    MouseMove,
    /// macOS: files dropped on the icon.
    DropFiles(Vec<String>),
}
