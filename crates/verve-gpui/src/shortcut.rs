//! Global shortcuts: keys the system hands the app while another window has the focus, taken with
//! `global-hotkey` (`RegisterHotKey` on Windows, Carbon on macOS) for `globalShortcut.register`.
//!
//! `register` is the one call the main process waits on the shell for, so [`Shortcuts::apply`]
//! answers it at once with the result of the one system call. Linux answers `false` to every
//! `register`, as Electron does where it cannot take a key (Wayland), and nothing is registered
//! when `VERVE_NO_SYSTEM_DIALOGS` is set: no test may take a key from a desktop.

use verve_electron::msg::MenuOp;

#[cfg(any(windows, target_os = "macos"))]
pub(crate) use real::{Shortcuts, listen};

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) use stub::Shortcuts;

#[cfg(not(any(windows, target_os = "macos")))]
mod stub {
    use super::*;

    /// A shell that can take no global shortcut.
    pub(crate) struct Shortcuts;

    impl Shortcuts {
        pub fn new(_: bool) -> Self {
            Shortcuts
        }

        pub fn apply(&mut self, op: &MenuOp) {
            if let MenuOp::ShortcutRegister { reply, .. } = op {
                // A main process that stopped waiting has nothing left to hear.
                let _ = reply.try_send(false);
            }
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
mod real {
    use std::collections::BTreeMap;

    use global_hotkey::hotkey::HotKey;
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use verve_electron::accelerator::Accelerator;

    use super::*;

    /// The shortcuts the shell has taken, by the number the main process registered each under.
    pub(crate) struct Shortcuts {
        on: bool,
        manager: Option<GlobalHotKeyManager>,
        taken: BTreeMap<u32, HotKey>,
    }

    /// Calls `pressed` with the system's id of a hotkey each time one is pressed.
    pub(crate) fn listen(pressed: impl Fn(u32) + Send + Sync + 'static) {
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state() == HotKeyState::Pressed {
                pressed(event.id());
            }
        }));
    }

    fn hotkey(wanted: &Accelerator) -> Option<HotKey> {
        let (flags, code) = crate::native::key_parts(wanted)?;
        Some(HotKey::new(Some(flags), code))
    }

    impl Shortcuts {
        /// Takes global shortcuts when `on`; otherwise refuses every one.
        pub fn new(on: bool) -> Self {
            Self {
                on,
                manager: None,
                taken: BTreeMap::new(),
            }
        }

        /// Does what `op` asks of the shortcuts; any other op is not for them.
        pub fn apply(&mut self, op: &MenuOp) {
            match op {
                MenuOp::ShortcutRegister {
                    id,
                    accelerator,
                    reply,
                } => {
                    let taken = self.register(*id, accelerator);
                    // A main process that stopped waiting has nothing left to hear.
                    let _ = reply.try_send(taken);
                }
                MenuOp::ShortcutUnregister { id } => self.unregister(*id),
                MenuOp::ShortcutUnregisterAll => {
                    let all: Vec<u32> = self.taken.keys().copied().collect();
                    all.into_iter().for_each(|id| self.unregister(id));
                }
                _ => {}
            }
        }

        fn register(&mut self, id: u32, accelerator: &Accelerator) -> bool {
            let Some(hotkey) = hotkey(accelerator).filter(|_| self.on) else {
                return false;
            };
            if self.manager.is_none() {
                self.manager = GlobalHotKeyManager::new().ok();
            }
            let Some(manager) = &self.manager else {
                return false;
            };
            let registered = manager.register(hotkey).is_ok();
            if registered {
                self.taken.insert(id, hotkey);
            }
            registered
        }

        fn unregister(&mut self, id: u32) {
            if let (Some(hotkey), Some(manager)) = (self.taken.remove(&id), &self.manager) {
                // A key the system no longer has registered is already unregistered.
                let _ = manager.unregister(hotkey);
            }
        }

        /// The number the main process registered the hotkey the system calls `system` under.
        pub fn registered_as(&self, system: u32) -> Option<u32> {
            (self.taken.iter())
                .find(|(_, hotkey)| hotkey.id() == system)
                .map(|(id, _)| *id)
        }

        /// Gives every key back, as the app ends.
        pub fn release_all(&mut self) {
            self.apply(&MenuOp::ShortcutUnregisterAll);
        }
    }
}
