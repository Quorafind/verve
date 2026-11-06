//! Tray icons: the icon in the notification area (Windows) or the menu bar (macOS), made with
//! `tray-icon` from what the main process sends and reporting what a person does to it.
//!
//! The balloon (`displayBalloon`) has no counterpart in `tray-icon`, so it is not shown and no
//! `'balloon-*'` event is ever sent; `setPressedImage` and `focus` do nothing. Linux has no tray
//! here (`tray-icon` needs GTK there), and a Linux shell answers every call as done. Nothing is
//! made when `VERVE_NO_SYSTEM_DIALOGS` is set: no test may put an icon in a desktop's tray.

use crate::menu::Menus;

#[cfg(any(windows, target_os = "macos"))]
pub(crate) use real::{Touch, Trays, listen};

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) use stub::Trays;

#[cfg(not(any(windows, target_os = "macos")))]
mod stub {
    use verve_electron::msg::{MenuId, MenuOp};

    use super::Menus;

    /// A shell with no tray.
    pub(crate) struct Trays;

    impl Trays {
        pub fn new(_: bool) -> Self {
            Trays
        }

        pub fn apply(&mut self, _: &MenuOp, _: &Menus) {}

        pub fn refresh(&mut self, _: MenuId, _: &Menus) {}
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod real {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };
    use verve_electron::NativeImage;
    use verve_electron::msg::{MenuId, MenuOp, Rect, TrayEvent, TrayId, TrayOp};

    use super::*;

    /// What a person did to a tray icon, from the thread that heard it.
    pub(crate) struct Touch {
        pub tray: TrayId,
        pub event: TrayEvent,
        pub bounds: Rect,
        pub position: Option<(f32, f32)>,
    }

    /// The icons the shell has made.
    pub(crate) struct Trays {
        on: bool,
        icons: BTreeMap<TrayId, TrayIcon>,
        /// The menu each icon shows, to be given again when the menu is sent anew.
        menus: BTreeMap<TrayId, MenuId>,
        /// Icons just double-clicked: the button-up that ends the double click is not a click.
        doubled: std::collections::BTreeSet<TrayId>,
    }

    /// Calls `heard` for each thing a person does to a tray icon, on the thread that hears it.
    pub(crate) fn listen(heard: impl Fn(Touch) + Send + Sync + 'static) {
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let Some(touch) = touch_of(&event) {
                heard(touch);
            }
        }));
    }

    /// Device pixels per DIP, for the system as a whole: the icon's place is in device pixels.
    fn scale() -> f32 {
        #[cfg(windows)]
        // SAFETY: a call with no arguments and no side effects.
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() };
        #[cfg(not(windows))]
        let dpi = 96;
        if dpi == 0 { 1. } else { dpi as f32 / 96. }
    }

    fn touch_of(event: &TrayIconEvent) -> Option<Touch> {
        let (id, rect, position, kind) = match event {
            TrayIconEvent::Click {
                id,
                rect,
                position,
                button,
                button_state: MouseButtonState::Up,
            } => {
                let kind = match button {
                    MouseButton::Left => TrayEvent::Click,
                    MouseButton::Right => TrayEvent::RightClick,
                    MouseButton::Middle => TrayEvent::MiddleClick,
                };
                (id, rect, position, kind)
            }
            // The system reports double clicks of the other buttons too; Electron's NotifyIcon only
            // reports the left button's.
            TrayIconEvent::DoubleClick {
                id,
                rect,
                position,
                button: MouseButton::Left,
            } => (id, rect, position, TrayEvent::DoubleClick),
            TrayIconEvent::Enter { id, rect, position } => {
                (id, rect, position, TrayEvent::MouseEnter)
            }
            TrayIconEvent::Move { id, rect, position } => {
                (id, rect, position, TrayEvent::MouseMove)
            }
            TrayIconEvent::Leave { id, rect, position } => {
                (id, rect, position, TrayEvent::MouseLeave)
            }
            _ => return None,
        };
        let scale = scale();
        Some(Touch {
            tray: id.as_ref().parse().ok()?,
            event: kind,
            bounds: Rect {
                x: rect.position.x as f32 / scale,
                y: rect.position.y as f32 / scale,
                width: rect.size.width as f32 / scale,
                height: rect.size.height as f32 / scale,
            },
            position: Some((position.x as f32 / scale, position.y as f32 / scale)),
        })
    }

    fn icon_of(image: &NativeImage) -> Option<Icon> {
        let rep = image.representation(2.0)?;
        Icon::from_rgba(rep.rgba.to_vec(), rep.width, rep.height).ok()
    }

    impl Trays {
        /// Tray icons when `on`; otherwise a shell that makes none.
        pub fn new(on: bool) -> Self {
            Self {
                on,
                icons: BTreeMap::new(),
                menus: BTreeMap::new(),
                doubled: std::collections::BTreeSet::new(),
            }
        }

        /// Does what `op` asks of the tray; any other op is not for it.
        pub fn apply(&mut self, op: &MenuOp, menus: &Menus) {
            if !self.on {
                return;
            }
            match op {
                MenuOp::TrayCreate { tray, image, guid } => {
                    self.create(*tray, image, guid.as_deref())
                }
                MenuOp::TraySet { tray, op } => {
                    if let TrayOp::ContextMenu(menu) = op {
                        match menu {
                            Some(menu) => self.menus.insert(*tray, *menu),
                            None => self.menus.remove(tray),
                        };
                    }
                    if let Some(icon) = self.icons.get(tray) {
                        set(icon, op, menus);
                    }
                }
                MenuOp::TrayDestroy { tray } => {
                    self.icons.remove(tray);
                    self.menus.remove(tray);
                }
                _ => {}
            }
        }

        fn create(&mut self, tray: TrayId, image: &Arc<NativeImage>, guid: Option<&str>) {
            let mut builder = TrayIconBuilder::new()
                .with_id(tray.to_string())
                .with_menu_on_left_click(false)
                // The menu is shown from the shell's task (`touched`), not from tray-icon's
                // window procedure, where `set_menu` and `TrayDestroy` could take it away.
                .with_menu_on_right_click(false);
            if let Some(icon) = icon_of(image) {
                builder = builder.with_icon(icon);
            }
            #[cfg(windows)]
            if let Some(guid) =
                guid.and_then(|g| u128::from_str_radix(&g.replace('-', ""), 16).ok())
            {
                builder = builder.with_guid(guid);
            }
            // Only Windows names an icon by a GUID; the argument is unused elsewhere.
            #[cfg(not(windows))]
            let _ = guid;
            // An icon that cannot be made is a tray the person never sees; the script is told of
            // nothing, as Electron's `new Tray` is not.
            if let Ok(icon) = builder.build() {
                self.icons.insert(tray, icon);
            }
        }

        /// What a touch of an icon comes to: a right click on an icon with a menu shows the menu
        /// instead of being an event, as Electron's `NotifyIcon` does, and the button-up that
        /// follows a double click is not a click.
        pub fn touched(&mut self, touch: Touch) -> Option<Touch> {
            match touch.event {
                TrayEvent::DoubleClick => {
                    self.doubled.insert(touch.tray);
                }
                TrayEvent::Click if self.doubled.remove(&touch.tray) => return None,
                TrayEvent::RightClick if self.menus.contains_key(&touch.tray) => {
                    if let Some(icon) = self.icons.get(&touch.tray) {
                        icon.show_menu();
                    }
                    return None;
                }
                _ => {}
            }
            Some(touch)
        }

        /// Menu `menu` was sent anew: the icons that show it are given the new one.
        pub fn refresh(&mut self, menu: MenuId, menus: &Menus) {
            for (tray, shown) in self.menus.iter() {
                if let (true, Some(icon)) = (*shown == menu, self.icons.get(tray)) {
                    set(icon, &TrayOp::ContextMenu(Some(menu)), menus);
                }
            }
        }

        /// Takes every icon away, as Windows leaves a ghost of one that is not removed.
        pub fn destroy_all(&mut self) {
            self.icons.clear();
        }
    }

    fn set(icon: &TrayIcon, op: &TrayOp, menus: &Menus) {
        match op {
            // A failed update leaves the icon as it was, which is all there is to do about it.
            TrayOp::Image(image) => {
                let _ = icon.set_icon(icon_of(image));
            }
            TrayOp::ToolTip(text) => {
                let _ = icon.set_tooltip(Some(text));
            }
            TrayOp::Title(text) => icon.set_title(Some(text)),
            TrayOp::ContextMenu(menu) => {
                let native = menu.and_then(|id| menus.menu(id)).cloned();
                icon.set_menu(
                    native.map(|menu| Box::new(menu) as Box<dyn tray_icon::menu::ContextMenu>),
                );
            }
            TrayOp::IgnoreDoubleClick(_) => {}
            TrayOp::PopUpContextMenu { .. } => icon.show_menu(),
            TrayOp::PressedImage(_)
            | TrayOp::CloseContextMenu
            | TrayOp::DisplayBalloon { .. }
            | TrayOp::RemoveBalloon
            | TrayOp::Focus => {}
        }
    }
}
