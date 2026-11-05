//! One window's state machine: what `ToShell` operations do to it and which events a shell tells
//! the main process because of them. It is a pure function of the window and the operation, so
//! the real shell (P34) can be held to it by the same tests.
//!
//! The rules, all of them:
//!
//! - A window that is closed ignores every operation and says `Closed` once.
//! - It is created centred on the primary display's work area unless the options give a place
//!   (the display's whole area if `fullscreen`), shown if `show`, and focused if it is shown
//!   and focusable. Creating sends no event.
//! - `Show` shows and focuses; `ShowInactive` shows; `Hide` takes focus away and hides. Each
//!   sends an event only for the state that changed, `Shown` before `Focus`, `Blur` before
//!   `Hidden`.
//! - `Focus` focuses a shown, focusable window; `Blur` takes focus away.
//! - Sizes are held between the window's `SetMinSize` and `SetMaxSize`. A change of size sends
//!   `Resized` and a change of place `Moved`, in that order, once, with the new values.
//! - `Maximize` and `SetFullScreen(true)` remember the bounds and fill the work area and the
//!   display; undoing them restores the bounds. Each sends its event, then the `Resized` and
//!   `Moved` the bounds made.
//! - `Minimize` blurs and sends `Minimized`; `Restore` undoes a minimize (`Restored`) or else a
//!   maximize.
//! - `Close` sends `CloseRequested` and changes nothing; the answer `CloseAnswer { prevented:
//!   false }`, like `Destroy`, closes the window. `prevented: true` keeps it as it is.
//!
//! Operations not listed — progress bars, icons, opacity, taskbar and mouse flags — are recorded
//! by the shell and change nothing here.
//!
//! Where Electron 39's documentation does not fix the order of two events, it is the order
//! above; P34 confirms each against a real window.

use crate::ids::{ContentsId, WindowId};
use crate::msg::{Display, Rect, WindowEvent, WindowOp, WindowOptions};

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
    /// Whether it is minimized.
    pub minimized: bool,
    /// Whether it is maximized.
    pub maximized: bool,
    /// Whether it is full screen.
    pub fullscreen: bool,
    /// Whether it was closed; a closed window stays listed.
    pub closed: bool,
}

/// A window and what its operations need to remember besides what [`HeadlessWindow`] shows.
pub(super) struct Window {
    pub(super) shown: HeadlessWindow,
    focusable: bool,
    resizable: bool,
    min_size: Option<(f32, f32)>,
    max_size: Option<(f32, f32)>,
    /// The bounds `Unmaximize` and leaving full screen go back to.
    normal: Rect,
}

impl Window {
    /// The window `options` ask for, on `display`.
    pub(super) fn new(
        id: WindowId,
        contents: ContentsId,
        options: &WindowOptions,
        display: &Display,
    ) -> Self {
        let (width, height) = (options.width, options.height);
        let area = display.work_area;
        let fullscreen = options.fullscreen;
        let bounds = if fullscreen {
            display.bounds
        } else {
            Rect {
                x: options.x.unwrap_or(area.x + (area.width - width) / 2.),
                y: options.y.unwrap_or(area.y + (area.height - height) / 2.),
                width,
                height,
            }
        };
        Self {
            shown: HeadlessWindow {
                id,
                contents,
                title: options.title.clone(),
                bounds,
                visible: options.show,
                focused: options.show && options.focusable,
                minimized: false,
                maximized: false,
                fullscreen,
                closed: false,
            },
            focusable: options.focusable,
            resizable: options.resizable,
            min_size: options.min_size,
            max_size: options.max_size,
            normal: bounds,
        }
    }

    /// Does `op` to the window, answering the events it makes.
    pub(super) fn apply(&mut self, op: &WindowOp, display: &Display) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        if self.shown.closed {
            return events;
        }
        match op {
            WindowOp::Show => {
                self.set_visible(true, &mut events);
                self.set_focus(true, &mut events);
            }
            WindowOp::ShowInactive => self.set_visible(true, &mut events),
            WindowOp::Hide => {
                self.set_focus(false, &mut events);
                self.set_visible(false, &mut events);
            }
            WindowOp::Focus => self.set_focus(true, &mut events),
            WindowOp::Blur => self.set_focus(false, &mut events),
            WindowOp::SetTitle(title) => self.shown.title.clone_from(title),
            WindowOp::SetBounds {
                x,
                y,
                width,
                height,
                ..
            } => {
                let old = self.shown.bounds;
                let wanted = Rect {
                    x: x.unwrap_or(old.x),
                    y: y.unwrap_or(old.y),
                    width: width.unwrap_or(old.width),
                    height: height.unwrap_or(old.height),
                };
                self.move_to(wanted, &mut events);
            }
            WindowOp::SetMinSize(width, height) => self.min_size = Some((*width, *height)),
            WindowOp::SetMaxSize(width, height) => self.max_size = Some((*width, *height)),
            WindowOp::SetResizable(resizable) => self.resizable = *resizable,
            WindowOp::Center => {
                let area = display.work_area;
                let mut centred = self.shown.bounds;
                centred.x = area.x + (area.width - centred.width) / 2.;
                centred.y = area.y + (area.height - centred.height) / 2.;
                self.move_to(centred, &mut events);
            }
            WindowOp::Minimize => self.minimize(&mut events),
            WindowOp::Maximize => self.maximize(display.work_area, &mut events),
            WindowOp::Unmaximize => self.unmaximize(&mut events),
            WindowOp::Restore => self.restore(&mut events),
            WindowOp::SetFullScreen(on) => self.set_fullscreen(*on, display.bounds, &mut events),
            _ => {}
        }
        events
    }

    /// A person resized the window by its edge: nothing happens to one that cannot be.
    pub(super) fn resize(&mut self, width: f32, height: f32) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        if self.resizable && !self.shown.closed {
            let old = self.shown.bounds;
            self.move_to(
                Rect {
                    width,
                    height,
                    ..old
                },
                &mut events,
            );
        }
        events
    }

    /// The window is closed, and says so once.
    pub(super) fn close(&mut self) -> Vec<WindowEvent> {
        if self.shown.closed {
            return Vec::new();
        }
        self.shown.closed = true;
        self.shown.visible = false;
        self.shown.focused = false;
        vec![WindowEvent::Closed]
    }

    fn set_visible(&mut self, visible: bool, events: &mut Vec<WindowEvent>) {
        if self.shown.visible != visible {
            self.shown.visible = visible;
            events.push(if visible {
                WindowEvent::Shown
            } else {
                WindowEvent::Hidden
            });
        }
    }

    fn set_focus(&mut self, focused: bool, events: &mut Vec<WindowEvent>) {
        let possible = self.shown.visible && self.focusable && !self.shown.minimized;
        if self.shown.focused != focused && (possible || !focused) {
            self.shown.focused = focused;
            events.push(if focused {
                WindowEvent::Focus
            } else {
                WindowEvent::Blur
            });
        }
    }

    /// Takes `wanted` as the bounds, its size held within the limits, and says what changed.
    fn move_to(&mut self, wanted: Rect, events: &mut Vec<WindowEvent>) {
        let (width, height) = self.clamped(wanted.width, wanted.height);
        let old = self.shown.bounds;
        let new = Rect {
            width,
            height,
            ..wanted
        };
        self.shown.bounds = new;
        if (new.width, new.height) != (old.width, old.height) {
            events.push(WindowEvent::Resized { width, height });
        }
        if (new.x, new.y) != (old.x, old.y) {
            events.push(WindowEvent::Moved { x: new.x, y: new.y });
        }
    }

    fn clamped(&self, mut width: f32, mut height: f32) -> (f32, f32) {
        if let Some((min_width, min_height)) = self.min_size {
            width = width.max(min_width);
            height = height.max(min_height);
        }
        if let Some((max_width, max_height)) = self.max_size {
            // A maximum of 0 is Electron's "no limit".
            if max_width > 0. {
                width = width.min(max_width);
            }
            if max_height > 0. {
                height = height.min(max_height);
            }
        }
        (width, height)
    }

    fn minimize(&mut self, events: &mut Vec<WindowEvent>) {
        if !self.shown.minimized {
            self.set_focus(false, events);
            self.shown.minimized = true;
            events.push(WindowEvent::Minimized);
        }
    }

    fn maximize(&mut self, area: Rect, events: &mut Vec<WindowEvent>) {
        if self.shown.maximized {
            return;
        }
        if !self.shown.fullscreen {
            self.normal = self.shown.bounds;
        }
        self.shown.maximized = true;
        events.push(WindowEvent::Maximized);
        self.move_to(area, events);
    }

    fn unmaximize(&mut self, events: &mut Vec<WindowEvent>) {
        if self.shown.maximized {
            self.shown.maximized = false;
            events.push(WindowEvent::Unmaximized);
            self.move_to(self.normal, events);
        }
    }

    fn restore(&mut self, events: &mut Vec<WindowEvent>) {
        if self.shown.minimized {
            self.shown.minimized = false;
            events.push(WindowEvent::Restored);
        } else {
            self.unmaximize(events);
        }
    }

    fn set_fullscreen(&mut self, on: bool, screen: Rect, events: &mut Vec<WindowEvent>) {
        if self.shown.fullscreen == on {
            return;
        }
        self.shown.fullscreen = on;
        if on {
            self.normal = self.shown.bounds;
            events.push(WindowEvent::EnterFullScreen);
            self.move_to(screen, events);
        } else {
            events.push(WindowEvent::LeaveFullScreen);
            self.move_to(self.normal, events);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msg::{Display, WindowOptions};

    fn screen() -> Display {
        let bounds = Rect {
            x: 0.,
            y: 0.,
            width: 1920.,
            height: 1080.,
        };
        Display {
            id: 1,
            bounds,
            work_area: Rect {
                height: 1040.,
                ..bounds
            },
            scale_factor: 1.,
            primary: true,
        }
    }

    fn window(options: &WindowOptions) -> Window {
        Window::new(1, 1, options, &screen())
    }

    fn apply(window: &mut Window, op: WindowOp) -> Vec<WindowEvent> {
        window.apply(&op, &screen())
    }

    /// The variants' names, which is what the order of events is about.
    fn names(events: Vec<WindowEvent>) -> Vec<String> {
        let name = |event: WindowEvent| format!("{event:?}");
        let name = |event| name(event).split(' ').next().unwrap_or("").to_owned();
        events.into_iter().map(name).collect()
    }

    #[test]
    fn a_window_opens_centred_on_the_work_area_and_focused() {
        let window = window(&WindowOptions::default());
        let bounds = window.shown.bounds;
        assert_eq!((bounds.x, bounds.y), (560., 220.));
        assert!(window.shown.visible && window.shown.focused);
    }

    #[test]
    fn showing_and_hiding_say_only_what_changed_and_blur_comes_before_hidden() {
        let hidden = WindowOptions {
            show: false,
            ..WindowOptions::default()
        };
        let mut window = window(&hidden);
        assert!(!window.shown.visible && !window.shown.focused);
        assert_eq!(
            names(apply(&mut window, WindowOp::Show)),
            ["Shown", "Focus"]
        );
        assert!(apply(&mut window, WindowOp::Show).is_empty());
        assert_eq!(
            names(apply(&mut window, WindowOp::Hide)),
            ["Blur", "Hidden"]
        );
        assert_eq!(names(apply(&mut window, WindowOp::ShowInactive)), ["Shown"]);
        assert!(!window.shown.focused);
    }

    #[test]
    fn a_window_that_cannot_take_focus_is_shown_without_it() {
        let options = WindowOptions {
            focusable: false,
            ..WindowOptions::default()
        };
        let mut window = window(&options);
        assert!(!window.shown.focused);
        assert!(apply(&mut window, WindowOp::Focus).is_empty());
    }

    #[test]
    fn bounds_are_held_between_the_limits_and_say_resized_then_moved() {
        let mut window = window(&WindowOptions::default());
        apply(&mut window, WindowOp::SetMinSize(300., 200.));
        apply(&mut window, WindowOp::SetMaxSize(1000., 0.));
        let set = WindowOp::SetBounds {
            x: Some(10.),
            y: None,
            width: Some(2000.),
            height: Some(100.),
            animate: false,
        };
        assert_eq!(names(apply(&mut window, set)), ["Resized", "Moved"]);
        let bounds = window.shown.bounds;
        assert_eq!((bounds.width, bounds.height, bounds.x), (1000., 200., 10.));
        assert!(window.resize(1000., 200.).is_empty());
    }

    #[test]
    fn a_window_that_cannot_be_resized_ignores_the_person_but_not_the_app() {
        let options = WindowOptions {
            resizable: false,
            ..WindowOptions::default()
        };
        let mut window = window(&options);
        assert!(window.resize(500., 400.).is_empty());
        let set = WindowOp::SetBounds {
            x: None,
            y: None,
            width: Some(500.),
            height: None,
            animate: false,
        };
        assert_eq!(names(apply(&mut window, set)), ["Resized"]);
    }

    #[test]
    fn maximizing_fills_the_work_area_and_unmaximizing_gives_the_bounds_back() {
        let mut window = window(&WindowOptions::default());
        let before = window.shown.bounds;
        let events = apply(&mut window, WindowOp::Maximize);
        assert_eq!(names(events), ["Maximized", "Resized", "Moved"]);
        assert_eq!(window.shown.bounds.height, 1040.);
        let events = apply(&mut window, WindowOp::Restore);
        assert_eq!(names(events), ["Unmaximized", "Resized", "Moved"]);
        assert_eq!(window.shown.bounds, before);
    }

    #[test]
    fn minimizing_blurs_and_restoring_comes_back() {
        let mut window = window(&WindowOptions::default());
        let minimized = names(apply(&mut window, WindowOp::Minimize));
        assert_eq!(minimized, ["Blur", "Minimized"]);
        assert_eq!(names(apply(&mut window, WindowOp::Restore)), ["Restored"]);
        assert!(window.shown.visible && !window.shown.minimized);
    }

    #[test]
    fn full_screen_covers_the_display_and_leaving_it_restores_the_bounds() {
        let mut window = window(&WindowOptions::default());
        let before = window.shown.bounds;
        let events = apply(&mut window, WindowOp::SetFullScreen(true));
        assert_eq!(names(events), ["EnterFullScreen", "Resized", "Moved"]);
        assert_eq!(window.shown.bounds.height, 1080.);
        apply(&mut window, WindowOp::SetFullScreen(false));
        assert_eq!(window.shown.bounds, before);
    }

    #[test]
    fn a_window_closes_once_and_then_ignores_everything() {
        let mut window = window(&WindowOptions::default());
        assert_eq!(names(window.close()), ["Closed"]);
        assert!(window.close().is_empty());
        assert!(apply(&mut window, WindowOp::Show).is_empty());
        assert!(!window.shown.visible && !window.shown.focused);
    }
}
