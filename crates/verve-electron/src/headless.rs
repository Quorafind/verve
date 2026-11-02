//! A shell for a machine with no display: it does with the messages it is sent what the GPUI
//! shell does, keeps the window state they add up to, and can be told that a person did
//! something. P00 builds its end-to-end assertions on this, and the state machine here is the one
//! the real shell runs — P34 keeps them in step.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::hub::{Hub, ShellPort};
use crate::ids::{ContentsId, WindowId};
use crate::msg::{FromShell, Rect, ToShell, WindowEvent, WindowOp};

/// A window as the shell has it.
#[derive(Clone, Debug)]
pub struct HeadlessWindow {
    /// The window's id.
    pub id: WindowId,
    /// The page it shows.
    pub contents: ContentsId,
    /// Its title now.
    pub title: String,
    /// Where it is and how big.
    pub bounds: Rect,
    /// Whether it is shown.
    pub visible: bool,
    /// Whether it has focus.
    pub focused: bool,
    /// Whether it was closed; a closed window stays listed.
    pub closed: bool,
}

/// The shell of a machine with no display.
pub struct HeadlessShell {
    /// Held weakly: the Hub holds the shell, and neither keeps the other alive.
    hub: Weak<Hub>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    received: Vec<ToShell>,
    windows: BTreeMap<WindowId, HeadlessWindow>,
    exit: Option<i32>,
}

impl State {
    /// What the shell does with a message, and what it says back to the main process.
    fn apply(&mut self, msg: ToShell) -> Vec<FromShell> {
        let replies = match &msg {
            ToShell::CreateWindow {
                window,
                contents,
                options,
            } => {
                let bounds = Rect {
                    x: options.x.unwrap_or(0.),
                    y: options.y.unwrap_or(0.),
                    width: options.width,
                    height: options.height,
                };
                self.windows.insert(
                    *window,
                    HeadlessWindow {
                        id: *window,
                        contents: *contents,
                        title: options.title.clone(),
                        bounds,
                        visible: options.show,
                        focused: options.show && options.focusable,
                        closed: false,
                    },
                );
                Vec::new()
            }
            ToShell::Window { window, op } => self.apply_op(*window, op),
            ToShell::Menu(_) => Vec::new(),
            ToShell::Quit { exit_code } => {
                self.exit = Some(*exit_code);
                Vec::new()
            }
        };
        self.received.push(msg);
        replies
    }

    fn apply_op(&mut self, window: WindowId, op: &WindowOp) -> Vec<FromShell> {
        match op {
            // Closing is the page's to cancel, so it is asked first.
            WindowOp::Close => {
                return vec![FromShell::Window {
                    window,
                    event: WindowEvent::CloseRequested,
                }];
            }
            WindowOp::Destroy => return self.close(window),
            WindowOp::CloseAnswer { prevented: false } => return self.close(window),
            _ => {}
        }
        let Some(entry) = self.windows.get_mut(&window) else {
            return Vec::new();
        };
        match op {
            WindowOp::Show => entry.visible = true,
            WindowOp::ShowInactive => {
                entry.visible = true;
                entry.focused = false;
            }
            WindowOp::Hide => entry.visible = false,
            WindowOp::Focus => entry.focused = true,
            WindowOp::Blur => entry.focused = false,
            WindowOp::SetTitle(title) => entry.title.clone_from(title),
            WindowOp::SetBounds {
                x,
                y,
                width,
                height,
                ..
            } => {
                if let Some(x) = x {
                    entry.bounds.x = *x;
                }
                if let Some(y) = y {
                    entry.bounds.y = *y;
                }
                if let Some(width) = width {
                    entry.bounds.width = *width;
                }
                if let Some(height) = height {
                    entry.bounds.height = *height;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// The window goes, and that is what the main process is told — once.
    fn close(&mut self, window: WindowId) -> Vec<FromShell> {
        let Some(entry) = self.windows.get_mut(&window) else {
            return Vec::new();
        };
        if entry.closed {
            return Vec::new();
        }
        entry.closed = true;
        entry.visible = false;
        entry.focused = false;
        vec![FromShell::Window {
            window,
            event: WindowEvent::Closed,
        }]
    }
}

impl HeadlessShell {
    /// A shell that takes this app's messages: from now on the Hub sends them here.
    pub fn new(hub: &Arc<Hub>) -> Arc<Self> {
        let shell = Arc::new(Self {
            hub: Arc::downgrade(hub),
            state: Mutex::new(State::default()),
        });
        hub.set_shell(shell.clone());
        shell
    }

    /// Every window this app made, open or closed.
    pub fn windows(&self) -> Vec<HeadlessWindow> {
        self.state().windows.values().cloned().collect()
    }

    /// Everything the main process asked of the shell, in order.
    pub fn received(&self) -> Vec<ToShell> {
        self.state().received.clone()
    }

    /// The code the app ended with, once it has.
    pub fn exit_code(&self) -> Option<i32> {
        self.state().exit
    }

    /// A person clicked `window`'s close button.
    pub fn user_close(&self, window: WindowId) {
        if let Some(hub) = self.hub.upgrade() {
            hub.from_shell(FromShell::Window {
                window,
                event: WindowEvent::CloseRequested,
            });
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl ShellPort for HeadlessShell {
    fn send(&self, msg: ToShell) {
        // Nothing is said back to the main process while the shell's own state is held: a window
        // that closed is reported once this message is done with.
        let replies = self.state().apply(msg);
        if let Some(hub) = self.hub.upgrade() {
            for reply in replies {
                hub.from_shell(reply);
            }
        }
    }

    fn wake(&self) {}
}
