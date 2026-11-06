//! The system's menus, tray icons and global hotkeys, as one thing the shell hands the
//! [`MenuOp`]s of the main process to and the events of the system back from.
//!
//! The three libraries report on whatever thread the system calls them on, so each is given a
//! handler that queues an [`Event`] in the shell's inbox (a popup's choice is kept for
//! `Menus::popup` to send instead); the shell turns it into a message to the main process on its
//! own thread. All three are off when `VERVE_NO_SYSTEM_DIALOGS` is set ([`touches_desktop`]), so
//! that no test of this crate puts a menu, an icon or a hotkey on a desktop.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use verve_electron::Hub;
use verve_electron::ids::WindowId;
use verve_electron::menu::MenuBook;
use verve_electron::msg::MenuOp;

use crate::menu::Menus;
use crate::shell::Inbox;
use crate::shortcut::Shortcuts;
use crate::tray::Trays;
use crate::window::Window;

/// Whether the shell touches the desktop: system dialogs, native menus, tray icons and hotkeys.
/// Not when `VERVE_NO_SYSTEM_DIALOGS` is set, as it is for every test that runs a real shell.
pub(crate) fn touches_desktop() -> bool {
    std::env::var_os("VERVE_NO_SYSTEM_DIALOGS").is_none()
}

/// The modifiers and key of an accelerator, as muda and global-hotkey (which share
/// `keyboard-types`) both take them.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn key_parts(
    wanted: &verve_electron::accelerator::Accelerator,
) -> Option<(muda::accelerator::Modifiers, muda::accelerator::Code)> {
    use muda::accelerator::{Code, Modifiers};
    let code = std::str::FromStr::from_str(wanted.key.code()).ok()?;
    let mods = wanted.mods;
    let mut flags = Modifiers::empty();
    flags.set(Modifiers::CONTROL, mods.ctrl || mods.alt_gr);
    flags.set(Modifiers::ALT, mods.alt || mods.alt_gr);
    flags.set(Modifiers::SHIFT, mods.shift);
    flags.set(Modifiers::META, mods.meta);
    let code: Code = code;
    Some((flags, code))
}

/// What the system said to one of the three.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) enum Event {
    /// A menu item was chosen: muda's id for it.
    Menu(String),
    /// A person did something to a tray icon.
    Tray(crate::tray::Touch),
    /// The system's id of a global hotkey that was pressed.
    Hotkey(u32),
}

/// The menus the main process sent, as the shell reads them for a key: shared with the embedder's
/// [`Control`](crate::Control).
pub(crate) type Book = Arc<Mutex<MenuBook>>;

/// Menus, tray icons and global hotkeys.
pub(crate) struct Native {
    menus: Menus,
    trays: Trays,
    shortcuts: Shortcuts,
    book: Book,
}

impl Native {
    /// The system's part of the shell. When [`touches_desktop`] says so, it listens to the system through `inbox`.
    pub fn new(inbox: &Inbox) -> Self {
        let on = touches_desktop();
        #[cfg(any(windows, target_os = "macos"))]
        if on {
            listen(inbox);
        }
        Self {
            menus: Menus::new(on),
            trays: Trays::new(on),
            shortcuts: Shortcuts::new(on),
            book: inbox.book(),
        }
    }

    /// Does what the main process asked of menus, trays or shortcuts.
    pub fn op(&mut self, op: &MenuOp, windows: &BTreeMap<WindowId, Window>, hub: &Arc<Hub>) {
        self.book
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .apply(op);
        match op {
            MenuOp::TrayCreate { .. } | MenuOp::TraySet { .. } | MenuOp::TrayDestroy { .. } => {
                self.trays.apply(op, &self.menus);
            }
            MenuOp::ShortcutRegister { .. }
            | MenuOp::ShortcutUnregister { .. }
            | MenuOp::ShortcutUnregisterAll => self.shortcuts.apply(op),
            _ => {
                self.menus.apply(op, windows, hub);
                if let MenuOp::Upsert(spec) = op {
                    self.trays.refresh(spec.id, &self.menus);
                }
            }
        }
    }

    /// Tells the main process what the system said.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn heard(&mut self, event: Event, hub: &Arc<Hub>) {
        use verve_electron::msg::FromShell;
        match event {
            Event::Menu(id) => {
                if let Some(menu_item) = crate::menu::item_of(&id) {
                    self.menus.restore_check(menu_item);
                    hub.from_shell(FromShell::MenuClicked {
                        menu_item,
                        window: None,
                        triggered_by_accelerator: false,
                        modifiers: verve_dom::Modifiers::default(),
                    });
                }
            }
            Event::Tray(touch) => {
                if let Some(touch) = self.trays.touched(touch) {
                    hub.from_shell(FromShell::Tray {
                        tray: touch.tray,
                        event: touch.event,
                        bounds: touch.bounds,
                        position: touch.position,
                        modifiers: verve_dom::Modifiers::default(),
                    });
                }
            }
            Event::Hotkey(system) => {
                if let Some(id) = self.shortcuts.registered_as(system) {
                    hub.from_shell(FromShell::Shortcut { id });
                }
            }
        }
    }

    /// The app is ending: the tray icons go, and the keys are given back.
    pub fn release(&mut self) {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.trays.destroy_all();
            self.shortcuts.release_all();
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn listen(inbox: &Inbox) {
    use crate::shell::Item;
    let queue = inbox.clone();
    muda::MenuEvent::set_event_handler(Some(move |event: muda::MenuEvent| {
        if !crate::menu::capture(&event.id.0) {
            queue.push_item(Item::Native(Event::Menu(event.id.0)));
        }
    }));
    let queue = inbox.clone();
    crate::tray::listen(move |touch| queue.push_item(Item::Native(Event::Tray(touch))));
    let queue = inbox.clone();
    crate::shortcut::listen(move |system| queue.push_item(Item::Native(Event::Hotkey(system))));
}
