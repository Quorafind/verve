//! macOS and Linux: the window operations GPUI has an API for, and on macOS the `NSWindow` calls
//! of [`super::macos`]. What neither does (on Linux showing and hiding a made window, limits,
//! opacity, the taskbar; on macOS the place of a window and its Dock progress) is listed in P34's
//! status.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

use gpui::{AsyncApp, Window};
use verve_electron::msg::{Display, Rect, WindowOptions};

use super::Target;
#[cfg(target_os = "macos")]
use super::macos;
use crate::report::Observed;

/// One window, by GPUI's handle.
pub(crate) struct Native {
    target: Target,
    cx: RefCell<AsyncApp>,
    /// The window this one is a child of, which it is attached to while it is shown.
    #[cfg(target_os = "macos")]
    owner: Option<isize>,
    /// Whether the window floats above others, which adopting it by a parent undoes.
    on_top: Cell<bool>,
    /// The last look, which GPUI's observers keep current.
    seen: Rc<Cell<Observed>>,
    /// Whether the app has shown the window: GPUI's own `is_visible` says whether a frame can be
    /// seen (a covered window, one on another Space), not whether the window is shown.
    shown: Cell<bool>,
}

/// A look at `window` through what GPUI says of it.
fn look(window: &Window, shown: bool, minimized: bool) -> Observed {
    let bounds = window.bounds();
    let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
    let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    Observed {
        visible: shown,
        focused: window.is_window_active(),
        minimized,
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
        options: &WindowOptions,
        owner: Option<&Native>,
        cx: &AsyncApp,
    ) -> Rc<Self> {
        #[cfg(target_os = "macos")]
        {
            macos::hide_traffic_lights(target.raw, options);
            macos::limit(target.raw, options.min_size, options.max_size);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = (options, owner);
        let native = Rc::new(Self {
            #[cfg(target_os = "macos")]
            owner: owner.map(|owner| owner.target.raw),
            on_top: Cell::new(false),
            target,
            cx: RefCell::new(cx.clone()),
            seen: Rc::new(Cell::new(Observed::default())),
            shown: Cell::new(false),
        });
        native.run(|_| {});
        native
    }

    /// Whether the system has the window in the Dock (macOS) or minimized.
    fn minimized(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            macos::is_miniaturized(self.target.raw)
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Runs `job` on the window, outside any update of it.
    fn run<R>(&self, job: impl FnOnce(&mut Window) -> R) -> Option<R> {
        use gpui::AppContext as _;
        let mut cx = self.cx.borrow_mut();
        let done = cx.update_window(self.target.any, |_, window, _| {
            let done = job(window);
            self.seen
                .set(look(window, self.shown.get(), self.minimized()));
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
        let now = look(window, self.shown.get(), self.minimized());
        self.seen.set(now);
        Some(now)
    }

    pub fn observe(&self) -> Observed {
        self.seen.get()
    }

    /// The visible area of the display the window is on, or else the window's own bounds.
    pub fn work_area(&self) -> Rect {
        let mut cx = self.cx.borrow_mut();
        let area = gpui::AppContext::update_window(&mut *cx, self.target.any, |_, window, cx| {
            window
                .display(cx)
                .map(|display| rect_of(display.visible_bounds()))
        });
        area.ok().flatten().unwrap_or(self.seen.get().bounds)
    }

    pub fn show(&self, activate: bool) {
        self.shown.set(true);
        // Show is told at once: `seen` is read by the sync that follows this call.
        self.run(|_| {});
        #[cfg(target_os = "macos")]
        {
            macos::set_shown(self.target.raw, true, activate);
            // A child follows its parent only while it is shown (adopting a hidden one would
            // show it), and being adopted resets the level, so that is put back.
            if let Some(owner) = self.owner {
                macos::adopt(owner, self.target.raw);
                macos::set_always_on_top(self.target.raw, self.on_top.get());
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = activate;
    }

    pub fn hide(&self) {
        self.shown.set(false);
        self.run(|_| {});
        #[cfg(target_os = "macos")]
        {
            if let Some(owner) = self.owner {
                macos::detach(owner, self.target.raw);
            }
            macos::set_shown(self.target.raw, false, false);
        }
    }

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

    pub fn limit(&self, min: Option<(f32, f32)>, max: Option<(f32, f32)>) {
        #[cfg(target_os = "macos")]
        macos::limit(self.target.raw, min, max);
        #[cfg(not(target_os = "macos"))]
        let _ = (min, max);
    }

    pub fn set_resizable(&self, _: bool) {}

    pub fn set_always_on_top(&self, on: bool) {
        self.on_top.set(on);
        #[cfg(target_os = "macos")]
        macos::set_always_on_top(self.target.raw, on);
        #[cfg(not(target_os = "macos"))]
        let _ = on;
    }

    pub fn set_opacity(&self, opacity: f64) {
        #[cfg(target_os = "macos")]
        macos::set_opacity(self.target.raw, opacity);
        #[cfg(not(target_os = "macos"))]
        let _ = opacity;
    }

    pub fn set_ignore_mouse(&self, _: bool) {}

    pub fn set_skip_taskbar(&self, _: bool) {}

    pub fn set_title(&self, title: &str) {
        self.run(|window| window.set_window_title(title));
    }

    pub fn set_progress(&self, _: f64) {}

    pub fn flash(&self, _: bool) {}

    pub fn set_icon(&self, _: &Path) {}

    pub fn set_fullscreen(&self, on: bool) {
        self.run(|window| (window.is_fullscreen() != on).then(|| window.toggle_fullscreen()));
    }

    pub fn release(&self) {}

    pub fn on_destroy(&self, _: Rc<dyn Fn()>) {}

    /// Only Windows answers the system where the window is dragged by.
    pub fn set_drag_area(&self, _: Vec<Rect>) {}
}

fn rect_of(bounds: gpui::Bounds<gpui::Pixels>) -> Rect {
    Rect {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
    }
}

/// The displays GPUI knows of.
pub(crate) fn displays(cx: &AsyncApp) -> Vec<Display> {
    let primary = cx.update(|cx| cx.primary_display().map(|display| display.id()));
    cx.update(|cx| {
        cx.displays()
            .into_iter()
            .map(|display| Display {
                id: u64::from(display.id()) as u32,
                bounds: rect_of(display.bounds()),
                work_area: rect_of(display.visible_bounds()),
                scale_factor: 1.,
                primary: Some(display.id()) == primary,
            })
            .collect()
    })
}
