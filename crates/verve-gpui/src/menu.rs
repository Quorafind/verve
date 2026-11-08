//! Native menus: a window's menu bar and the menus popped up over it, built with `muda` from the
//! [`MenuSpec`]s the main process sends and reporting the item chosen.
//!
//! Windows attaches a menu bar to each window's `HWND` (`SetWindowMenu`); macOS has one menu bar
//! for the app (`SetApplicationMenu`). Linux has none: `muda` there needs GTK, which this shell
//! does not link, so a Linux shell shows no menu bar and answers a popup as closed at once.
//!
//! Accelerators are not muda's to handle. GPUI's Windows message loop (`gpui_windows`'s
//! `platform.rs`, `run` and `run_foreground_task`) never calls `TranslateAcceleratorW`: it sends
//! each `WM_KEYDOWN` to the window as `WM_GPUI_KEYDOWN` and, when GPUI did not take the key,
//! translates and dispatches it plainly. muda's accelerator table (`Menu::haccel`) is therefore
//! never consulted, and the keys of a menu are looked up in the shell's [`MenuBook`] before a key
//! goes to a page (`docs/electron/packages/P15-menu-tray-shortcut.md`, notes for P32).
//!
//! Nothing here is made when `VERVE_NO_SYSTEM_DIALOGS` is set: the tests of this crate run real
//! windows, and no test may put a menu on a desktop.

use std::collections::BTreeMap;
use std::sync::Arc;

use verve_electron::Hub;
use verve_electron::ids::WindowId;
use verve_electron::msg::{FromShell, MenuId, MenuOp};

use crate::window::Window;

/// The item that muda reports as chosen, named by the id this module gave it.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn item_of(id: &str) -> Option<verve_electron::msg::MenuItemId> {
    id.parse().ok()
}

/// What a menu in the middle of a popup tells the main process.
pub(crate) fn popup_answered(hub: &Arc<Hub>, menu: MenuId, call: u64) {
    hub.from_shell(FromShell::MenuWillShow { menu });
    hub.from_shell(FromShell::MenuWillClose {
        menu,
        call: Some(call),
    });
}

/// Whether a popup is open (the statics are process-wide), and the item it was closed with: muda reports the choice
/// from inside the call that shows the menu, and the main process must hear it before it hears
/// that the menu closed.
#[cfg(any(windows, target_os = "macos"))]
static POPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(any(windows, target_os = "macos"))]
static CHOSEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Keeps muda's `id` for the popup that is open; `false` when none is, and the id is to be
/// queued as any other.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn capture(id: &str) -> bool {
    use std::sync::atomic::Ordering;
    if !POPPING.load(Ordering::SeqCst) {
        return false;
    }
    *CHOSEN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(id.to_owned());
    true
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) use real::Menus;

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) use stub::Menus;

#[cfg(not(any(windows, target_os = "macos")))]
mod stub {
    use super::*;

    /// A shell with no native menus: popups close at once.
    pub(crate) struct Menus;

    impl Menus {
        pub fn new(_: bool) -> Self {
            Menus
        }

        pub fn apply(&mut self, op: &MenuOp, _: &BTreeMap<WindowId, Window>, hub: &Arc<Hub>) {
            if let MenuOp::Popup { menu, call, .. } = op {
                popup_answered(hub, *menu, *call);
            }
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod real {
    use muda::accelerator::Accelerator;
    use muda::dpi::{LogicalPosition, Position};
    use muda::{
        CheckMenuItem, ContextMenu, IconMenuItem, IsMenuItem, Menu, MenuId as NativeId, MenuItem,
        PredefinedMenuItem, Submenu,
    };
    use verve_electron::NativeImage;
    use verve_electron::accelerator::Accelerator as Wanted;
    use verve_electron::keyed::Keyed;
    use verve_electron::msg::{MenuItemId, MenuItemKind, MenuItemSpec, MenuSpec};

    use super::*;

    /// The handle of one native item, for a change that keeps the menu's shape.
    enum Live {
        None,
        Plain(MenuItem),
        Check(CheckMenuItem),
        Icon(IconMenuItem),
        Sub(Submenu),
    }

    /// A native menu, the spec it was last given and the handle of each item in it, in the order
    /// [`visible`] lists them.
    struct Built {
        menu: Menu,
        spec: MenuSpec,
        live: Vec<Live>,
    }

    /// The native menus built from what the main process sent.
    pub(crate) struct Menus {
        on: bool,
        menus: Keyed<Built>,
        /// The menu each window shows as its bar.
        bars: Keyed<MenuId>,
        /// The menu that is the macOS menu bar.
        app: Option<MenuId>,
        /// The windows whose script hid the bar: it stays hidden when the menu is sent anew.
        hidden: std::collections::BTreeSet<WindowId>,
    }

    impl Menus {
        /// Native menus when `on`; otherwise a shell that makes none.
        pub fn new(on: bool) -> Self {
            Self {
                on,
                menus: Keyed::new(),
                bars: Keyed::new(),
                app: None,
                hidden: std::collections::BTreeSet::new(),
            }
        }

        /// Does what `op` asks of the menus; any other op is not for them.
        pub fn apply(&mut self, op: &MenuOp, windows: &BTreeMap<WindowId, Window>, hub: &Arc<Hub>) {
            match op {
                MenuOp::Popup {
                    menu,
                    window,
                    at,
                    call,
                    ..
                } => self.popup(*menu, (*window, windows.get(window)), *at, *call, hub),
                _ if !self.on => {}
                MenuOp::Upsert(spec) => self.upsert(spec, windows),
                MenuOp::Drop(id) => self.forget(*id),
                MenuOp::SetApplicationMenu(menu) => self.set_application_menu(*menu),
                MenuOp::SetWindowMenu { window, menu } => {
                    self.set_window_menu(*window, *menu, windows);
                }
                MenuOp::SetMenuBarVisibility {
                    window, visible, ..
                } => {
                    if *visible {
                        self.hidden.remove(window);
                    } else {
                        self.hidden.insert(*window);
                    }
                    self.set_bar_visible(*window, *visible, windows);
                }
                _ => {}
            }
        }

        fn upsert(&mut self, spec: &MenuSpec, windows: &BTreeMap<WindowId, Window>) {
            // A change that keeps the shape (a label, a check mark, an accelerator) is made on the
            // items: a menu bar taken off its window and put back flickers and resizes the frame.
            if let Some(built) = self.menus.get_mut(&spec.id)
                && shape(&built.spec) == shape(spec)
            {
                update_in_place(built, spec);
                return;
            }
            let built = build_menu(spec);
            let shown_in: Vec<WindowId> = (self.bars.iter())
                .filter(|(_, menu)| **menu == spec.id)
                .map(|(window, _)| *window)
                .collect();
            for window in &shown_in {
                self.detach(*window, windows);
            }
            self.menus.insert(spec.id, built);
            for window in shown_in {
                self.attach(window, spec.id, windows);
            }
            if self.app == Some(spec.id) {
                self.set_application_menu(Some(spec.id));
            }
        }

        /// Sets the check mark of item `item` back to what the main process last said: muda flips a
        /// check item itself when it is chosen, but whether it stays checked is the script's to say
        /// (a chosen radio item is checked, not toggled).
        pub fn restore_check(&self, item: MenuItemId) {
            for built in self.menus.values() {
                let items = visible(&built.spec.items);
                for (spec, live) in items.iter().zip(&built.live) {
                    if let (true, Live::Check(check)) = (spec.id == item, live) {
                        check.set_checked(spec.checked);
                    }
                }
            }
        }

        fn forget(&mut self, id: MenuId) {
            if !self.bars.values().any(|bar| *bar == id) && self.app != Some(id) {
                self.menus.remove(&id);
            }
        }

        fn set_window_menu(
            &mut self,
            window: WindowId,
            menu: Option<MenuId>,
            windows: &BTreeMap<WindowId, Window>,
        ) {
            self.detach(window, windows);
            if let Some(menu) = menu {
                self.attach(window, menu, windows);
            }
        }

        /// Puts menu `id` on `window`'s frame (Windows). macOS has no menu on a window.
        fn attach(&mut self, window: WindowId, id: MenuId, windows: &BTreeMap<WindowId, Window>) {
            // A window without a frame has no menu bar, as in Electron; the book still has its menu.
            #[cfg(windows)]
            if let (Some(built), Some(found)) = (self.menus.get(&id), windows.get(&window))
                && found.framed
            {
                // SAFETY: the handle is that of a window the shell has open.
                if unsafe { built.menu.init_for_hwnd(found.raw_handle()) }.is_ok() {
                    self.bars.insert(window, id);
                    // `init_for_hwnd` shows the bar; a bar the script hid stays hidden.
                    self.set_bar_visible(window, !self.hidden.contains(&window), windows);
                }
            }
            // macOS has no menu on a window; the arguments are unused there.
            #[cfg(not(windows))]
            let _ = (window, id, windows);
        }

        fn detach(&mut self, window: WindowId, windows: &BTreeMap<WindowId, Window>) {
            let shown = self.bars.remove(&window);
            #[cfg(windows)]
            if let (Some(built), Some(found)) = (
                shown.and_then(|id| self.menus.get(&id)),
                windows.get(&window),
            ) {
                // SAFETY: as in `attach`. A menu that was never attached answers an error, and
                // there is nothing to remove then.
                let _ = unsafe { built.menu.remove_for_hwnd(found.raw_handle()) };
            }
            // As in `attach`: nothing to take off a window off Windows.
            #[cfg(not(windows))]
            let _ = (shown, windows);
        }

        fn set_application_menu(&mut self, menu: Option<MenuId>) {
            #[cfg(target_os = "macos")]
            {
                if let Some(old) = self.app.and_then(|id| self.menus.get(&id)) {
                    old.menu.remove_for_nsapp();
                }
                if let Some(new) = menu.and_then(|id| self.menus.get(&id)) {
                    new.menu.init_for_nsapp();
                }
            }
            self.app = menu;
        }

        fn set_bar_visible(
            &self,
            window: WindowId,
            visible: bool,
            windows: &BTreeMap<WindowId, Window>,
        ) {
            // The hidden state is kept in `apply`, which has `&mut self`; this only draws it.
            #[cfg(windows)]
            if let (Some(built), Some(found)) = (
                self.bars.get(&window).and_then(|id| self.menus.get(id)),
                windows.get(&window),
            ) {
                let menu = &built.menu;
                // SAFETY: as in `attach`. An error is a bar the window never had: nothing to show or hide.
                let _ = unsafe {
                    if visible {
                        menu.show_for_hwnd(found.raw_handle())
                    } else {
                        menu.hide_for_hwnd(found.raw_handle())
                    }
                };
            }
            // As in `attach`: only Windows has a bar on a window.
            #[cfg(not(windows))]
            let _ = (window, visible, windows);
        }

        /// Opens menu `id` over `window` and returns when it has closed; the item chosen, if
        /// any, is captured by muda's handler during the call and sent to the main process before
        /// `MenuWillClose`.
        fn popup(
            &self,
            id: MenuId,
            (window_id, window): (WindowId, Option<&Window>),
            at: Option<(f32, f32)>,
            call: u64,
            hub: &Arc<Hub>,
        ) {
            let (Some(built), Some(window), true) = (self.menus.get(&id), window, self.on) else {
                return popup_answered(hub, id, call);
            };
            let menu = &built.menu;
            hub.from_shell(FromShell::MenuWillShow { menu: id });
            POPPING.store(true, std::sync::atomic::Ordering::SeqCst);
            let at = at
                .map(|(x, y)| Position::Logical(LogicalPosition::new(f64::from(x), f64::from(y))));
            // SAFETY: the handle is that of a window the shell has open, and this runs on its thread.
            #[cfg(windows)]
            unsafe {
                menu.show_context_menu_for_hwnd(window.raw_handle(), at);
            }
            #[cfg(target_os = "macos")]
            // SAFETY: as above; on macOS the handle is the window's `NSView`.
            unsafe {
                menu.show_context_menu_for_nsview(window.raw_handle() as _, at);
            }
            POPPING.store(false, std::sync::atomic::Ordering::SeqCst);
            let chosen = (CHOSEN.lock())
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            // The choice first, then the close: a click is heard before the menu is said to be gone.
            if let Some(menu_item) = chosen.as_deref().and_then(item_of) {
                self.restore_check(menu_item);
                hub.from_shell(FromShell::MenuClicked {
                    menu_item,
                    window: Some(window_id),
                    triggered_by_accelerator: false,
                    modifiers: verve_dom::Modifiers::default(),
                });
            }
            hub.from_shell(FromShell::MenuWillClose {
                menu: id,
                call: Some(call),
            });
        }

        /// The native menu `id`, for a tray icon to show.
        pub fn menu(&self, id: MenuId) -> Option<&Menu> {
            self.menus.get(&id).map(|built| &built.menu)
        }
    }

    /// The accelerator muda is given for one of ours.
    pub(crate) fn accelerator(wanted: &Wanted) -> Option<Accelerator> {
        let (flags, code) = crate::native::key_parts(wanted)?;
        Some(Accelerator::new(flags, code))
    }

    /// An icon muda takes, from an image of ours.
    pub(crate) fn icon(image: &NativeImage) -> Option<muda::Icon> {
        let rep = image.representation(1.0)?;
        muda::Icon::from_rgba(rep.rgba.to_vec(), rep.width, rep.height).ok()
    }

    /// The items of `items` that are shown, a submenu before the items in it: the order of
    /// [`Built::live`].
    fn visible(items: &[MenuItemSpec]) -> Vec<&MenuItemSpec> {
        let mut out = Vec::new();
        for item in items.iter().filter(|item| item.visible) {
            out.push(item);
            if let MenuItemKind::Submenu(sub) = &item.kind {
                out.extend(visible(&sub.items));
            }
        }
        out
    }

    /// What of a menu cannot change in place: which items there are, in what nesting, and which
    /// kind of native item each is.
    fn shape(spec: &MenuSpec) -> Vec<(MenuItemId, u8, usize)> {
        fn walk(items: &[MenuItemSpec], depth: usize, out: &mut Vec<(MenuItemId, u8, usize)>) {
            for item in items.iter().filter(|item| item.visible) {
                let kind = match &item.kind {
                    MenuItemKind::Separator => 0,
                    MenuItemKind::Submenu(_) => 1,
                    MenuItemKind::Checkbox | MenuItemKind::Radio => 2,
                    _ => 3 + u8::from(item.icon.is_some()),
                };
                out.push((item.id, kind, depth));
                if let MenuItemKind::Submenu(sub) = &item.kind {
                    walk(&sub.items, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&spec.items, 0, &mut out);
        out
    }

    /// Brings the native items of `built` to what `spec` says, which has the shape they have.
    fn update_in_place(built: &mut Built, spec: &MenuSpec) {
        let before = visible(&built.spec.items);
        for ((old, new), live) in before.iter().zip(visible(&spec.items)).zip(&built.live) {
            let accel = || new.accelerator.as_ref().and_then(accelerator);
            let (text, enabled) = (old.label != new.label, old.enabled != new.enabled);
            // A call that fails leaves the item as it was; the next change tries again.
            match live {
                Live::None => {}
                Live::Sub(item) => {
                    set_if(text, || item.set_text(&new.label));
                    set_if(enabled, || item.set_enabled(new.enabled));
                }
                Live::Plain(item) => {
                    set_if(text, || item.set_text(&new.label));
                    set_if(enabled, || item.set_enabled(new.enabled));
                    set_if(old.accelerator != new.accelerator, || {
                        let _ = item.set_accelerator(accel());
                    });
                }
                Live::Check(item) => {
                    set_if(text, || item.set_text(&new.label));
                    set_if(enabled, || item.set_enabled(new.enabled));
                    // muda flipped the mark itself when the item was chosen, so it is always set.
                    item.set_checked(new.checked);
                    set_if(old.accelerator != new.accelerator, || {
                        let _ = item.set_accelerator(accel());
                    });
                }
                Live::Icon(item) => {
                    set_if(text, || item.set_text(&new.label));
                    set_if(enabled, || item.set_enabled(new.enabled));
                    set_if(old.accelerator != new.accelerator, || {
                        let _ = item.set_accelerator(accel());
                    });
                    set_if(old.icon != new.icon, || {
                        item.set_icon(new.icon.as_deref().and_then(icon));
                    });
                }
            }
        }
        built.spec = spec.clone();
    }

    fn set_if(changed: bool, set: impl FnOnce()) {
        if changed {
            set();
        }
    }

    /// Builds a whole menu: the bar or popup of `spec`.
    fn build_menu(spec: &MenuSpec) -> Built {
        let menu = Menu::new();
        let mut live = Vec::new();
        for item in (spec.items.iter()).filter_map(|item| make_item(item, &mut live)) {
            // An item that cannot be added is left out; the rest of the menu is still a menu.
            let _ = menu.append(&*item);
        }
        Built {
            menu,
            spec: spec.clone(),
            live,
        }
    }

    fn make_item(spec: &MenuItemSpec, live: &mut Vec<Live>) -> Option<Box<dyn IsMenuItem>> {
        if !spec.visible {
            return None;
        }
        let id = NativeId::new(spec.id.to_string());
        let accel = spec.accelerator.as_ref().and_then(accelerator);
        let label = spec.label.as_str();
        let item: Box<dyn IsMenuItem> = match &spec.kind {
            MenuItemKind::Separator => {
                live.push(Live::None);
                Box::new(PredefinedMenuItem::separator())
            }
            MenuItemKind::Submenu(sub) => {
                let submenu = Submenu::with_id(format!("menu-{}", sub.id), label, spec.enabled);
                live.push(Live::Sub(submenu.clone()));
                for child in sub.items.iter().filter_map(|item| make_item(item, live)) {
                    // As in `build_menu`.
                    let _ = submenu.append(&*child);
                }
                Box::new(submenu)
            }
            MenuItemKind::Checkbox | MenuItemKind::Radio => {
                let check = CheckMenuItem::with_id(id, label, spec.enabled, spec.checked, accel);
                live.push(Live::Check(check.clone()));
                Box::new(check)
            }
            _ => match spec.icon.as_deref().and_then(icon) {
                Some(icon) => {
                    let item = IconMenuItem::with_id(id, label, spec.enabled, Some(icon), accel);
                    live.push(Live::Icon(item.clone()));
                    Box::new(item)
                }
                None => {
                    let item = MenuItem::with_id(id, label, spec.enabled, accel);
                    live.push(Live::Plain(item.clone()));
                    Box::new(item)
                }
            },
        };
        Some(item)
    }
}
