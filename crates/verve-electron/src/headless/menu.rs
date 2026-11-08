//! What the headless shell does with menus, trays and global shortcuts: it keeps the menus it was
//! sent, answers a popup at once as a person who chooses nothing would, takes every global shortcut
//! unless told otherwise, and keeps the tray icons as records a test can read. Nothing is shown,
//! and no hotkey is ever registered with the system.

use std::collections::BTreeMap;

use super::Outcome;
use crate::accelerator::Accelerator;
use crate::menu::MenuBook;
use crate::msg::{FromShell, MenuId, MenuOp, MenuSpec, TrayId, TrayOp};

/// A menu popped up from a tray icon: the menu (none is the icon's own) and where.
pub type Popped = (Option<MenuId>, Option<(f32, f32)>);

/// A tray icon as the headless shell has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadlessTray {
    /// The size of the icon in DIPs.
    pub image_size: (u32, u32),
    pub tool_tip: String,
    pub title: String,
    pub context_menu: Option<MenuId>,
    pub has_pressed_image: bool,
    pub ignore_double_click: bool,
    pub guid: Option<String>,
    /// The balloon last shown: its title and content, until it is removed.
    pub balloon: Option<(String, String)>,
    /// Menus popped up from the icon, `(menu, position)`.
    pub popped_up: Vec<Popped>,
    pub destroyed: bool,
}

/// A popup the shell was asked for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadlessPopup {
    pub call: u64,
    pub menu: MenuId,
    pub window: crate::ids::WindowId,
    pub at: Option<(f32, f32)>,
}

#[derive(Default)]
pub(super) struct Desk {
    pub(super) book: MenuBook,
    pub(super) popups: Vec<HeadlessPopup>,
    pub(super) shortcuts: BTreeMap<u32, Accelerator>,
    pub(super) refuse_shortcuts: bool,
    pub(super) trays: BTreeMap<TrayId, HeadlessTray>,
}

impl Desk {
    /// Takes `op`, and says in `out` what a person would have done.
    pub(super) fn apply(&mut self, op: &MenuOp, out: &mut Outcome) {
        self.book.apply(op);
        match op {
            MenuOp::Popup {
                menu,
                window,
                at,
                call,
                ..
            } => {
                let popup = HeadlessPopup {
                    call: *call,
                    menu: *menu,
                    window: *window,
                    at: *at,
                };
                self.popups.push(popup);
                out.events.push(FromShell::MenuWillShow { menu: *menu });
                out.events.push(FromShell::MenuWillClose {
                    menu: *menu,
                    call: Some(*call),
                });
            }
            MenuOp::TrayCreate { tray, image, guid } => {
                let icon = HeadlessTray {
                    image_size: image.size(),
                    guid: guid.clone(),
                    ..HeadlessTray::default()
                };
                self.trays.insert(*tray, icon);
            }
            MenuOp::TraySet { tray, op } => {
                if let Some(icon) = self.trays.get_mut(tray) {
                    set_tray(icon, op);
                }
            }
            MenuOp::TrayDestroy { tray } => {
                if let Some(icon) = self.trays.get_mut(tray) {
                    icon.destroyed = true;
                }
            }
            MenuOp::ShortcutRegister {
                id,
                accelerator,
                reply,
            } => {
                let taken = !self.refuse_shortcuts
                    && !self.shortcuts.values().any(|have| have == accelerator);
                if taken {
                    self.shortcuts.insert(*id, *accelerator);
                }
                // The main process waits for this and gives up when the answer is not there.
                let _ = reply.try_send(taken);
            }
            MenuOp::ShortcutUnregister { id } => {
                self.shortcuts.remove(id);
            }
            MenuOp::ShortcutUnregisterAll => self.shortcuts.clear(),
            _ => {}
        }
    }

    pub(super) fn application_menu(&self) -> Option<MenuSpec> {
        self.book.application_menu().cloned()
    }
}

fn set_tray(icon: &mut HeadlessTray, op: &TrayOp) {
    match op {
        TrayOp::Image(image) => icon.image_size = image.size(),
        TrayOp::PressedImage(image) => icon.has_pressed_image = image.is_some(),
        TrayOp::ToolTip(text) => icon.tool_tip.clone_from(text),
        TrayOp::Title(text) => icon.title.clone_from(text),
        TrayOp::ContextMenu(menu) => icon.context_menu = *menu,
        TrayOp::PopUpContextMenu { menu, at } => icon.popped_up.push((*menu, *at)),
        TrayOp::IgnoreDoubleClick(on) => icon.ignore_double_click = *on,
        TrayOp::DisplayBalloon { title, content, .. } => {
            icon.balloon = Some((title.clone(), content.clone()));
        }
        TrayOp::RemoveBalloon => icon.balloon = None,
        TrayOp::CloseContextMenu | TrayOp::Focus => {}
    }
}

/// A message to the shell about menus as one line of the trace; a menu is its id and size, not
/// every item.
pub(super) fn describe(op: &MenuOp) -> String {
    // Written out, not `{op:?}`: Debug of every menu and tray message would be code in the binary.
    match op {
        MenuOp::Upsert(spec) => format!("upsert menu {} ({} items)", spec.id, spec.items.len()),
        MenuOp::Drop(id) => format!("drop menu {id}"),
        MenuOp::SetApplicationMenu(menu) => format!("application menu {menu:?}"),
        MenuOp::SetWindowMenu { window, menu } => format!("window {window} menu {menu:?}"),
        MenuOp::SetMenuBarVisibility {
            window, visible, ..
        } => {
            format!("window {window} menu bar visible {visible}")
        }
        MenuOp::Popup { menu, window, .. } => format!("popup menu {menu} over window {window}"),
        MenuOp::ClosePopup { .. } => "close popup".into(),
        MenuOp::TrayCreate { tray, .. } => format!("create tray {tray}"),
        MenuOp::TraySet { tray, .. } => format!("set tray {tray}"),
        MenuOp::TrayDestroy { tray } => format!("destroy tray {tray}"),
        MenuOp::ShortcutRegister {
            id, accelerator, ..
        } => format!("register shortcut {id} {accelerator}"),
        MenuOp::ShortcutUnregister { id } => format!("unregister shortcut {id}"),
        MenuOp::ShortcutUnregisterAll => "unregister all shortcuts".into(),
    }
}
