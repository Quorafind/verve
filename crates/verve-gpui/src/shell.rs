//! The shell's windows: one GPUI window per window the main process asked for, what each of them
//! draws, and the answers they send back.
//!
//! A message is applied on the OS thread that received it and never blocks. The two that take a
//! round trip — a close a page may cancel — are asked of the main process and answered by a
//! message of their own, so no thread here ever waits for JavaScript.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use gpui::{
    AppContext as _, Bounds, Context, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions as GpuiOptions, point, px, size,
};
use verve_electron::Hub;
use verve_electron::ids::{ContentsId, WindowId};
use verve_electron::msg::{
    FromShell, ToRendererInput, ToShell, WindowEvent, WindowOp, WindowOptions,
};

use crate::dom_view::DomView;

/// The messages the Hub has sent the shell, from whichever thread sent them.
#[derive(Clone, Default)]
pub(crate) struct Messages(Arc<Mutex<Vec<ToShell>>>);

impl Messages {
    pub(crate) fn push(&self, msg: ToShell) {
        self.lock().push(msg);
    }

    fn take(&self) -> Vec<ToShell> {
        std::mem::take(&mut self.lock())
    }

    fn lock(&self) -> MutexGuard<'_, Vec<ToShell>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The shell, as the OS thread keeps it.
pub(crate) struct Shell {
    hub: Arc<Hub>,
    messages: Messages,
    /// Each window the shell has open, with the page it shows.
    windows: BTreeMap<WindowId, (ContentsId, WindowHandle<DomView>)>,
}

impl Shell {
    /// A shell for the app `hub` routes, taking its messages from `messages`.
    pub fn new(hub: Arc<Hub>, messages: Messages) -> Self {
        Self {
            hub,
            messages,
            windows: BTreeMap::new(),
        }
    }

    /// Does what the main process asked since the last wake, hands each window what its page
    /// published, and ends the app when the main process says it is ending.
    pub fn process(&mut self, cx: &mut Context<Self>) {
        for msg in self.messages.take() {
            self.apply(msg, cx);
        }
        self.draw(cx);
        if self.hub.exit_code().is_some() {
            cx.quit();
        }
    }

    fn apply(&mut self, msg: ToShell, cx: &mut Context<Self>) {
        match msg {
            ToShell::CreateWindow {
                window,
                contents,
                options,
            } => self.create(window, contents, options, cx),
            ToShell::Window { window, op } => self.window_op(window, op, cx),
            // Menus, trays and global shortcuts are P15's.
            ToShell::Menu(_) => {}
            ToShell::Quit { .. } => cx.quit(),
        }
    }

    /// Opens a window, and tells its page how big what it draws in is. The options this does not
    /// use — frames, transparency, always-on-top, DPI, parent and modal windows — are P34's.
    fn create(
        &mut self,
        id: WindowId,
        contents: ContentsId,
        options: WindowOptions,
        cx: &mut Context<Self>,
    ) {
        let area = size(px(options.width), px(options.height));
        let bounds = match (options.x, options.y) {
            (Some(x), Some(y)) => Bounds::new(point(px(x), px(y)), area),
            _ => Bounds::centered(None, area, cx),
        };
        let hub = Arc::clone(&self.hub);
        let opened = cx.open_window(
            GpuiOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(options.title.clone().into()),
                    ..Default::default()
                }),
                show: options.show,
                focus: options.focusable,
                is_resizable: options.resizable,
                is_movable: options.movable,
                is_minimizable: options.minimizable,
                window_min_size: options
                    .min_size
                    .map(|(width, height)| size(px(width), px(height))),
                ..Default::default()
            },
            move |window, cx| {
                // A person asking for the window to close is the page's to answer: the main
                // process asks it, and says back whether a listener kept it open.
                window.on_window_should_close(cx, move |_, _| {
                    hub.from_shell(FromShell::Window {
                        window: id,
                        event: WindowEvent::CloseRequested,
                    });
                    false
                });
                cx.new(|_| DomView::new())
            },
        );
        match opened {
            Ok(handle) => {
                let scale = handle
                    .update(cx, |_, window, _| window.scale_factor())
                    .unwrap_or(1.);
                self.windows.insert(id, (contents, handle));
                self.hub.input(
                    contents,
                    ToRendererInput::Viewport {
                        width: options.width,
                        height: options.height,
                        scale,
                    },
                );
            }
            Err(error) => {
                // A window that could not open is a window that closed, to the app's script.
                eprintln!("electron: no window for {id}: {error}");
                self.hub.from_shell(FromShell::Window {
                    window: id,
                    event: WindowEvent::Closed,
                });
            }
        }
    }

    fn window_op(&mut self, window: WindowId, op: WindowOp, cx: &mut Context<Self>) {
        match op {
            // Asked of the page, as a person's click is.
            WindowOp::Close => self.ask_to_close(window),
            WindowOp::CloseAnswer { prevented: false } | WindowOp::Destroy => {
                self.destroy(window, cx)
            }
            WindowOp::CloseAnswer { prevented: true } => {}
            WindowOp::SetTitle(title) => {
                if let Some((_, handle)) = self.windows.get(&window) {
                    let _ = handle.update(cx, |_, window, _| window.set_window_title(&title));
                }
            }
            // Showing, hiding, sizing, opacity, icons, taskbar and the rest are P34's.
            _ => {}
        }
    }

    fn ask_to_close(&self, window: WindowId) {
        self.hub.from_shell(FromShell::Window {
            window,
            event: WindowEvent::CloseRequested,
        });
    }

    /// The window goes, and the main process is told — once whatever was asked about it is done.
    fn destroy(&mut self, window: WindowId, cx: &mut Context<Self>) {
        let Some((_, handle)) = self.windows.remove(&window) else {
            return;
        };
        let _ = handle.update(cx, |_, window, _| window.remove_window());
        self.hub.from_shell(FromShell::Window {
            window,
            event: WindowEvent::Closed,
        });
    }

    /// Hands each window what its page published since the last turn.
    fn draw(&mut self, cx: &mut Context<Self>) {
        for (contents, handle) in self.windows.values() {
            let Some(update) = self
                .hub
                .renderer_view(*contents)
                .and_then(|view| view.update)
            else {
                continue;
            };
            let _ = handle.update(cx, |view, _, cx| {
                view.show(update.snapshot);
                cx.notify();
            });
        }
    }
}
