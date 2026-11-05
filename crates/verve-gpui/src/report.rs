//! What a window does, as the main process hears of it. The OS says that something changed, not
//! in which order, so a window is looked at ([`Observed`]) and the events are what differs from
//! the last look, in the one order Electron 39.8.10 sends them and the headless shell keeps
//! (`headless/window.rs`): the same sequence whether a person or the app caused the change.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use verve_electron::Hub;
use verve_electron::ids::{ContentsId, WindowId};
use verve_electron::msg::{FromShell, Rect, ToRendererInput, WindowEvent};

/// A window as one look at it finds it. Sizes are in device-independent pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Observed {
    pub visible: bool,
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    /// The window with its frame, in screen coordinates.
    pub bounds: Rect,
    /// What the page is drawn in: the window without its frame.
    pub content: (f32, f32),
    pub scale: f32,
}

/// Pushes `on` or `off` when a flag went from `before` to `after`.
fn flip(
    events: &mut Vec<WindowEvent>,
    (before, after): (bool, bool),
    on: WindowEvent,
    off: WindowEvent,
) {
    if before != after {
        events.push(if after { on } else { off });
    }
}

/// The events that took `old` to `new`, in Electron's order: shown or hidden, full screen, minimized
/// or restored, `Resized` then `Moved`, maximized, the scale factor, and focus last. A window that
/// leaves maximized is not `Moved`, though its place changed: Electron says only `Resized`.
pub(crate) fn diff(old: &Observed, new: &Observed) -> Vec<WindowEvent> {
    let mut events = Vec::new();
    let pair = |get: fn(&Observed) -> bool| (get(old), get(new));
    flip(
        &mut events,
        pair(|o| o.visible),
        WindowEvent::Shown,
        WindowEvent::Hidden,
    );
    flip(
        &mut events,
        pair(|o| o.fullscreen),
        WindowEvent::EnterFullScreen,
        WindowEvent::LeaveFullScreen,
    );
    flip(
        &mut events,
        pair(|o| o.minimized),
        WindowEvent::Minimized,
        WindowEvent::Restored,
    );
    let (before, after) = (old.bounds, new.bounds);
    if (before.width, before.height) != (after.width, after.height) {
        events.push(WindowEvent::Resized {
            width: after.width,
            height: after.height,
        });
    }
    // A minimized window is not zoomed to the system, but it is not unmaximized either.
    let unmaximized = old.maximized && !new.maximized;
    let minimizing = new.minimized && !old.minimized;
    if (before.x, before.y) != (after.x, after.y) && !unmaximized {
        events.push(WindowEvent::Moved {
            x: after.x,
            y: after.y,
        });
    }
    flip(
        &mut events,
        pair(|o| o.maximized),
        WindowEvent::Maximized,
        WindowEvent::Unmaximized,
    );
    if minimizing && unmaximized {
        events.retain(|event| !matches!(event, WindowEvent::Unmaximized));
    }
    if old.scale != new.scale && old.scale != 0. {
        events.push(WindowEvent::ScaleFactor(new.scale));
    }
    flip(
        &mut events,
        pair(|o| o.focused),
        WindowEvent::Focus,
        WindowEvent::Blur,
    );
    events
}

/// One window's reports to the main process, and to the page under it.
pub(crate) struct Reporter {
    hub: Arc<Hub>,
    id: WindowId,
    contents: ContentsId,
    last: Cell<Observed>,
    closed: Cell<bool>,
    painted: Cell<bool>,
}

impl Reporter {
    /// The reporter of window `id` as `first` finds it: what is already so is not reported.
    pub fn new(hub: Arc<Hub>, id: WindowId, contents: ContentsId, first: Observed) -> Rc<Self> {
        Rc::new(Self {
            hub,
            id,
            contents,
            last: Cell::new(first),
            closed: Cell::new(false),
            painted: Cell::new(false),
        })
    }

    /// Looks at the window again: what is new is told to the page first and the main process after.
    pub fn sync(&self, mut now: Observed) {
        if self.closed.get() {
            return;
        }
        let old = self.last.get();
        if now.minimized {
            // A minimized window has no place on the screen: it keeps the one it had.
            (now.bounds, now.content) = (old.bounds, old.content);
        }
        self.last.set(now);
        for input in inputs(&old, &now) {
            self.hub.input(self.contents, input);
        }
        for event in diff(&old, &now) {
            self.say(event);
        }
    }

    /// Tells the main process `event`, unless the window is closed.
    pub fn say(&self, event: WindowEvent) {
        if !self.closed.get() {
            self.hub.from_shell(FromShell::Window {
                window: self.id,
                event,
            });
        }
    }

    /// The window is gone: said once, and nothing after it.
    pub fn close(&self) {
        if !self.closed.replace(true) {
            self.hub.from_shell(FromShell::Window {
                window: self.id,
                event: WindowEvent::Closed,
            });
        }
    }

    /// The window is complete, as `first` finds it: nothing in it is news, but the page is told
    /// what it needs to know to draw.
    pub fn begin(&self, first: Observed) {
        self.last.set(first);
        for input in inputs(&Observed::default(), &first) {
            self.hub.input(self.contents, input);
        }
    }

    /// Tells the page under the window something.
    pub fn input(&self, input: ToRendererInput) {
        self.hub.input(self.contents, input);
    }

    /// Whether `FirstPaint` was sent.
    pub fn is_painted(&self) -> bool {
        self.painted.get()
    }

    /// A frame was drawn while the window was shown; the first of them is `FirstPaint`.
    pub fn painted(&self) {
        if !self.painted.replace(true) {
            self.say(WindowEvent::FirstPaint);
        }
    }
}

/// What the page under a window is told of a change.
fn inputs(old: &Observed, new: &Observed) -> Vec<ToRendererInput> {
    let mut inputs = Vec::new();
    if (old.content, old.scale) != (new.content, new.scale) {
        inputs.push(ToRendererInput::Viewport {
            width: new.content.0,
            height: new.content.1,
            scale: new.scale,
        });
    }
    let drawn = |look: &Observed| look.visible && !look.minimized;
    if drawn(old) != drawn(new) {
        inputs.push(ToRendererInput::Visibility(drawn(new)));
    }
    if old.focused != new.focused {
        inputs.push(ToRendererInput::Focus(new.focused));
    }
    inputs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn shown() -> Observed {
        Observed {
            visible: true,
            focused: true,
            bounds: at(560., 220., 800., 600.),
            content: (784., 561.),
            scale: 1.,
            ..Observed::default()
        }
    }

    fn names(events: Vec<WindowEvent>) -> Vec<String> {
        let name = |event| format!("{event:?}");
        let name = |event| {
            name(event)
                .split([' ', '('])
                .next()
                .unwrap_or("")
                .to_owned()
        };
        events.into_iter().map(name).collect()
    }

    fn after(change: impl Fn(&mut Observed)) -> Vec<String> {
        let mut new = shown();
        change(&mut new);
        names(diff(&shown(), &new))
    }

    #[test]
    fn showing_says_shown_before_focus_and_hiding_hidden_before_blur() {
        let hidden = Observed {
            visible: false,
            focused: false,
            ..shown()
        };
        assert_eq!(names(diff(&hidden, &shown())), ["Shown", "Focus"]);
        assert_eq!(names(diff(&shown(), &hidden)), ["Hidden", "Blur"]);
    }

    #[test]
    fn maximizing_says_resized_moved_then_maximized_and_leaving_it_not_moved() {
        let maximized = Observed {
            maximized: true,
            bounds: at(0., 0., 1920., 1040.),
            ..shown()
        };
        let events = names(diff(&shown(), &maximized));
        assert_eq!(events, ["Resized", "Moved", "Maximized"]);
        let events = names(diff(&maximized, &shown()));
        assert_eq!(events, ["Resized", "Unmaximized"]);
    }

    #[test]
    fn minimizing_a_maximized_window_says_only_minimized_and_restoring_it_maximized() {
        let maximized = Observed {
            maximized: true,
            ..shown()
        };
        let minimized = Observed {
            minimized: true,
            maximized: false,
            focused: false,
            ..shown()
        };
        assert_eq!(names(diff(&maximized, &minimized)), ["Minimized", "Blur"]);
        let restored = Observed {
            maximized: true,
            ..shown()
        };
        let events = names(diff(&minimized, &restored));
        assert_eq!(events, ["Restored", "Maximized", "Focus"]);
    }

    #[test]
    fn minimizing_says_minimized_then_blur_and_restoring_restored_then_focus() {
        let minimized = Observed {
            minimized: true,
            focused: false,
            ..shown()
        };
        assert_eq!(names(diff(&shown(), &minimized)), ["Minimized", "Blur"]);
        assert_eq!(names(diff(&minimized, &shown())), ["Restored", "Focus"]);
    }

    #[test]
    fn full_screen_says_so_before_the_bounds_it_changes() {
        let events = after(|now| {
            now.fullscreen = true;
            now.bounds = at(0., 0., 1920., 1080.);
        });
        assert_eq!(events, ["EnterFullScreen", "Resized", "Moved"]);
        let full = Observed {
            fullscreen: true,
            bounds: at(0., 0., 1920., 1080.),
            ..shown()
        };
        let events = names(diff(&full, &shown()));
        assert_eq!(events, ["LeaveFullScreen", "Resized", "Moved"]);
    }

    #[test]
    fn a_new_scale_factor_is_told_and_a_window_that_did_not_change_says_nothing() {
        assert_eq!(after(|now| now.scale = 1.5), ["ScaleFactor"]);
        assert!(after(|_| {}).is_empty());
    }

    #[test]
    fn a_minimized_window_keeps_the_place_it_had() {
        let options = verve_electron::ElectronOptions {
            app_path: ".".into(),
            dark: false,
        };
        let reporter = Reporter::new(Hub::new(options), 1, 1, shown());
        let minimized = Observed {
            minimized: true,
            bounds: at(-32000., -32000., 160., 28.),
            ..shown()
        };
        reporter.sync(minimized);
        assert_eq!(reporter.last.get().bounds, shown().bounds);
    }
}
