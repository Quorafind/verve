//! The shell: what the main process asks of the OS thread, and the windows that answer.
//!
//! A message is applied on the OS thread that received it and never blocks. The two that take a
//! round trip (a close a page may cancel, an app that is quitting) are asked of the main process
//! and answered by a message of their own, so no thread here ever waits for JavaScript. Window
//! management is [`window`](crate::window)'s and the system's [`platform`](crate::platform);
//! this only says which window a message is for.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::AsyncApp;
use verve_electron::Hub;
use verve_electron::ids::WindowId;
use verve_electron::msg::{AppEvent, FromShell, ToRendererInput, ToShell, WindowOp};

use crate::app_region::AppRegion;
use crate::capture::Capture;
use crate::dialog;
use crate::native::{self, Native};
use crate::platform;
use crate::window::{self, Opening, Window};

/// What lands in the shell's queue.
pub(crate) enum Item {
    /// The main process asked for something.
    Msg(ToShell),
    /// The system theme is dark (`true`) or light.
    Theme(bool),
    /// The displays changed.
    Displays,
    /// The system destroyed the window; its entry goes, and the main process has been told.
    Gone(WindowId),
    /// P30 laid out a page that has `-webkit-app-region` boxes.
    Regions(WindowId, Vec<AppRegion>),
    /// A menu item was chosen, a tray icon touched or a global hotkey pressed.
    #[cfg(any(windows, target_os = "macos"))]
    Native(native::Event),
}

/// The queue, shared by the Hub's threads that fill it and the OS thread that empties it.
#[derive(Clone, Default)]
pub(crate) struct Messages(Arc<Mutex<Vec<Item>>>);

impl Messages {
    fn push(&self, item: Item) {
        self.lock().push(item);
    }

    fn take(&self) -> Vec<Item> {
        std::mem::take(&mut self.lock())
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Item>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The shell's queue with the wake that tells its OS thread to look.
#[derive(Clone)]
pub(crate) struct Inbox {
    messages: Messages,
    wake: async_channel::Sender<()>,
    /// The menus the main process sent, which a key is looked up in.
    book: native::Book,
}

impl Inbox {
    pub fn new(wake: async_channel::Sender<()>) -> Self {
        Self {
            messages: Messages::default(),
            wake,
            book: native::Book::default(),
        }
    }

    /// The menus the main process sent.
    pub fn book(&self) -> native::Book {
        native::Book::clone(&self.book)
    }

    /// Queues `msg` without waking the shell: the Hub wakes it after a send.
    pub fn queue(&self, msg: ToShell) {
        self.messages.push(Item::Msg(msg));
    }

    /// Queues `item` and wakes the shell. One pending wake is all it needs: it takes everything
    /// queued when it runs, so a wake that finds the slot full has been coalesced into the one
    /// waiting.
    pub fn push_item(&self, item: Item) {
        self.messages.push(item);
        self.wake();
    }

    /// Queues `msg` and wakes the shell.
    pub fn push(&self, msg: ToShell) {
        self.push_item(Item::Msg(msg));
    }

    pub fn wake(&self) {
        let _ = self.wake.try_send(());
    }
}

/// The shell, as the OS thread keeps it.
pub(crate) struct Shell {
    hub: Arc<Hub>,
    inbox: Inbox,
    messages: Messages,
    /// Each window the shell has open.
    windows: BTreeMap<WindowId, Window>,
    /// Whether the system theme is dark, as the pages were last told.
    dark: Option<bool>,
    /// Menus, tray icons and global hotkeys.
    native: Native,
    /// The screenshot `--screenshot` asked for, until it is taken.
    capture: Option<Capture>,
}

impl Shell {
    /// A shell for the app `hub` routes, taking its messages from `inbox`.
    pub fn new(hub: Arc<Hub>, inbox: Inbox) -> Self {
        let messages = inbox.messages.clone();
        Self {
            hub,
            native: Native::new(&inbox),
            inbox,
            messages,
            windows: BTreeMap::new(),
            dark: None,
            capture: None,
        }
    }

    /// Takes a picture of the first window's page once it has settled, then ends the app.
    pub fn capture(&mut self, capture: Capture) {
        self.capture = Some(capture);
    }

    /// Does what the main process asked since the last wake, hands each window what its page
    /// published, and ends the app when the main process says it is ending.
    pub fn process(&mut self, cx: &mut AsyncApp) {
        for item in self.messages.take() {
            self.apply(item, cx);
        }
        self.draw(cx);
        if let Some(capture) = &mut self.capture
            && capture.poll(self.windows.values().next(), cx)
        {
            self.capture = None;
            cx.update(|cx| cx.quit());
        }
        if self.hub.exit_code().is_some() {
            self.native.release();
            cx.update(|cx| cx.quit());
        }
    }

    fn apply(&mut self, item: Item, cx: &mut AsyncApp) {
        match item {
            Item::Msg(msg) => self.message(msg, cx),
            Item::Theme(dark) => self.theme(dark),
            // The system destroyed it: letting go of what it held (a modal block) and of GPUI's
            // window is `destroy`'s, and saying `Closed` again is a no-op.
            Item::Gone(id) => self.destroy(id, cx),
            Item::Displays => self
                .hub
                .from_shell(FromShell::Displays(platform::displays(cx))),
            Item::Regions(id, regions) => {
                if let Some(window) = self.windows.get(&id) {
                    window.set_app_regions(regions, cx);
                }
            }
            #[cfg(any(windows, target_os = "macos"))]
            Item::Native(event) => self.native.heard(event, &self.hub),
        }
    }

    fn message(&mut self, msg: ToShell, cx: &mut AsyncApp) {
        match msg {
            ToShell::CreateWindow {
                window,
                contents,
                options,
            } => self.create(window, contents, &options, cx),
            ToShell::Window { window, op } => self.window_op(window, op, cx),
            ToShell::Menu(op) => self.native.op(&op, &self.windows, &self.hub),
            ToShell::ErrorBox { title, content } => {
                dialog::error_box(&title, &content, self.windows.values().next(), cx);
            }
            ToShell::Dialog {
                call,
                parent,
                spec,
                reply,
            } => {
                let window = match parent {
                    Some(id) => self.windows.get(&id),
                    None => self.windows.values().next(),
                };
                if parent.is_some() && window.is_none() {
                    dialog::closed(&self.hub, call, reply);
                } else {
                    dialog::show(&self.hub, call, window, spec, reply, cx);
                }
            }
            // The frames follow the system's theme until P34 draws them by the source.
            ToShell::SetThemeSource(_) => {}
            ToShell::Quit { .. } => {
                self.native.release();
                cx.update(|cx| cx.quit());
            }
        }
    }

    /// Opens a window; one that cannot open is a window that closed, to the app's script.
    fn create(
        &mut self,
        id: WindowId,
        contents: verve_electron::ids::ContentsId,
        options: &verve_electron::msg::WindowOptions,
        cx: &mut AsyncApp,
    ) {
        let owner = options.parent.and_then(|parent| self.windows.get(&parent));
        let opening = Opening {
            hub: &self.hub,
            inbox: &self.inbox,
            id,
            contents,
            options,
            owner,
        };
        let opened = window::primary(cx).and_then(|display| window::open(opening, &display, cx));
        match opened {
            Some(window) => {
                self.windows.insert(id, window);
            }
            None => self.hub.from_shell(FromShell::Window {
                window: id,
                event: verve_electron::msg::WindowEvent::Closed,
            }),
        }
    }

    fn window_op(&mut self, id: WindowId, op: WindowOp, cx: &mut AsyncApp) {
        let Some(window) = self.windows.get(&id) else {
            return;
        };
        match op {
            // Asked of the page, as a person's click is.
            WindowOp::Close => window.ask_to_close(),
            WindowOp::CloseAnswer { prevented } => {
                window.answered();
                if !prevented {
                    self.destroy(id, cx);
                }
            }
            WindowOp::Destroy => self.destroy(id, cx),
            op => window.apply(op, cx),
        }
    }

    /// The window goes, and the main process is told once. Its children go first: the system
    /// would take them with it without a word.
    fn destroy(&mut self, id: WindowId, cx: &mut AsyncApp) {
        let children: Vec<_> = (self.windows.iter())
            .filter(|(_, window)| window.parent == Some(id))
            .map(|(child, _)| *child)
            .collect();
        for child in children {
            self.destroy(child, cx);
        }
        if let Some(window) = self.windows.remove(&id) {
            window.destroy(cx);
        }
    }

    /// The system theme changed: the app and every page are told, once.
    fn theme(&mut self, dark: bool) {
        if self.dark.replace(dark) == Some(dark) {
            return;
        }
        self.hub
            .from_shell(FromShell::App(AppEvent::ThemeChanged { dark }));
        for window in self.windows.values() {
            self.hub
                .input(window.contents, ToRendererInput::ColorScheme { dark });
        }
    }

    /// Hands each window what its page published since the last turn.
    fn draw(&mut self, cx: &mut AsyncApp) {
        for window in self.windows.values() {
            let Some(view) = self.hub.renderer_view(window.contents) else {
                continue;
            };
            // The pictures first: the list that names them is drawn with them.
            window.show_images(view.images, cx);
            if let Some(list) = view.display_list {
                window.show_list(list, cx);
            }
        }
    }
}
