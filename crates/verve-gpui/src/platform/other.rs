//! macOS and Linux: the window operations GPUI has an API for. The rest (showing and hiding a
//! made window, limits, opacity, the taskbar) are not done here and are listed in P34's status.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gpui::{AsyncApp, Window};
use verve_electron::msg::{Display, Rect, WindowOptions};

use super::Target;
use crate::report::Observed;

/// One window, by GPUI's handle.
pub(crate) struct Native {
    target: Target,
    cx: RefCell<AsyncApp>,
    /// The last look, which GPUI's observers keep current.
    seen: Rc<Cell<Observed>>,
}

/// A look at `window` through what GPUI says of it.
fn look(window: &Window) -> Observed {
    let bounds = window.bounds();
    let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    Observed {
        visible: true,
        focused: window.is_window_active(),
        minimized: !window.is_visible(),
        maximized: window.is_maximized(),
        fullscreen: window.is_fullscreen(),
        bounds: Rect {
            x,
            y,
            width,
            height,
        },
        content: (width, height),
        scale: window.scale_factor(),
    }
}

impl Native {
    pub fn attach(
        target: Target,
        _: &WindowOptions,
        _: Option<&Native>,
        cx: &AsyncApp,
    ) -> Rc<Self> {
        let native = Rc::new(Self {
            target,
            cx: RefCell::new(cx.clone()),
            seen: Rc::new(Cell::new(Observed::default())),
        });
        native.run(|window| native.seen.set(look(window)));
        native
    }

    /// Runs `job` on the window, outside any update of it.
    fn run<R>(&self, job: impl FnOnce(&mut Window) -> R) -> Option<R> {
        use gpui::AppContext as _;
        let mut cx = self.cx.borrow_mut();
        let done = cx.update_window(self.target.any, |_, window, _| {
            let done = job(window);
            self.seen.set(look(window));
            done
        });
        done.ok()
    }

    pub fn handle(&self) -> u64 {
        self.target.raw as u64
    }

    /// Windows tell a window's observers nothing a draw does not: the frame a change makes
    /// is the look, which the draw reports.
    pub fn watch(&self, _: Rc<dyn Fn()>) {}

    /// The look a frame of `window` finds.
    pub fn looked(&self, window: &Window) -> Option<Observed> {
        let now = look(window);
        self.seen.set(now);
        Some(now)
    }

    pub fn observe(&self) -> Observed {
        self.seen.get()
    }

    pub fn work_area(&self) -> Rect {
        self.seen.get().bounds
    }

    pub fn show(&self, _: bool) {}

    pub fn hide(&self) {}

    pub fn focus(&self) {
        self.run(|window| window.activate_window());
    }

    pub fn blur(&self) {}

    pub fn minimize(&self) {
        self.run(|window| window.minimize_window());
    }

    pub fn maximize(&self) {
        self.run(|window| (!window.is_maximized()).then(|| window.zoom_window()));
    }

    pub fn restore(&self) {
        self.run(|window| window.is_maximized().then(|| window.zoom_window()));
    }

    pub fn place(&self, bounds: Rect) {
        self.run(|window| {
            window.resize(gpui::size(gpui::px(bounds.width), gpui::px(bounds.height)))
        });
    }

    pub fn limit(&self, _: Option<(f32, f32)>, _: Option<(f32, f32)>) {}

    pub fn set_resizable(&self, _: bool) {}

    pub fn set_always_on_top(&self, _: bool) {}

    pub fn set_opacity(&self, _: f64) {}

    pub fn set_ignore_mouse(&self, _: bool) {}

    pub fn set_skip_taskbar(&self, _: bool) {}

    pub fn set_title(&self, title: &str) {
        self.run(|window| window.set_window_title(title));
    }

    pub fn set_progress(&self, _: f64) {}

    pub fn flash(&self, _: bool) {}

    pub fn set_icon(&self, _: &Path) -> bool {
        false
    }

    pub fn set_fullscreen(&self, on: bool) {
        self.run(|window| (window.is_fullscreen() != on).then(|| window.toggle_fullscreen()));
    }

    pub fn release(&self) {}

    pub fn block_owner(&self) {}
}

/// The displays GPUI knows of.
pub(crate) fn displays(cx: &AsyncApp) -> Vec<Display> {
    let primary = cx.update(|cx| cx.primary_display().map(|display| display.id()));
    cx.update(|cx| {
        cx.displays()
            .into_iter()
            .map(|display| {
                let rect = |bounds: gpui::Bounds<gpui::Pixels>| Rect {
                    x: f32::from(bounds.origin.x),
                    y: f32::from(bounds.origin.y),
                    width: f32::from(bounds.size.width),
                    height: f32::from(bounds.size.height),
                };
                Display {
                    id: u64::from(display.id()) as u32,
                    bounds: rect(display.bounds()),
                    work_area: rect(display.visible_bounds()),
                    scale_factor: 1.,
                    primary: Some(display.id()) == primary,
                }
            })
            .collect()
    })
}
