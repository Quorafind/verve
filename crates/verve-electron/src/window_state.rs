//! A window as the main process knows it: the state `BrowserWindow`'s getters answer from.
//!
//! The shell owns the real window, so a getter cannot ask it. Each setter changes this cache first
//! ([`WindowState::predict`]), so `getBounds()` right after `setBounds()` is the new value, and the
//! shell's events correct it afterwards ([`WindowState::observe`]). Nothing here touches
//! JavaScript: it is plain data, so the rules are tested apart from the API.

use std::collections::BTreeSet;

use crate::msg::{Display, Rect, WindowEvent, WindowOp, WindowOptions};

/// What the frame of a window takes from its outside size, as Electron 39.8.10 on Windows 10
/// reported it (`getContentSize()` of a 800 x 600 window is 784 x 561): 8 pixels on each side
/// and the 31 of the title bar. A window with no frame, or full screen, has none.
const FRAME: Extents = Extents {
    left: 8.,
    top: 31.,
    right: 8.,
    bottom: 8.,
};

/// The smallest a window is made, in each direction (Windows' `SM_CXMIN`, `SM_CYMIN`).
const SMALLEST: (f32, f32) = (136., 39.);

#[derive(Clone, Copy)]
struct Extents {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

/// The on/off members of a window that need nothing but remembering, or one message to the shell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Flag {
    Resizable,
    Movable,
    Minimizable,
    Maximizable,
    FullScreenable,
    Closable,
    Focusable,
    AlwaysOnTop,
    SimpleFullScreen,
    SkipTaskbar,
    ContentProtected,
    VisibleOnAllWorkspaces,
    HasShadow,
    MenuBarAutoHide,
    MenuBarVisible,
    DocumentEdited,
    Enabled,
    HiddenInMissionControl,
}

impl Flag {
    /// Whether a window starts with the flag on: Electron's defaults, observed on Windows (a
    /// window has no `hasShadow` there).
    fn on_at_start(self) -> bool {
        match self {
            Flag::AlwaysOnTop
            | Flag::SimpleFullScreen
            | Flag::SkipTaskbar
            | Flag::ContentProtected
            | Flag::VisibleOnAllWorkspaces
            | Flag::MenuBarAutoHide
            | Flag::DocumentEdited
            | Flag::HiddenInMissionControl => false,
            Flag::HasShadow => !cfg!(windows),
            _ => true,
        }
    }

    fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A rectangle whose sides may be missing: what `setBounds` is given.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Sides {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
}

/// One window's cached state.
#[derive(Clone, Debug)]
pub(crate) struct WindowState {
    pub bounds: Rect,
    /// The bounds the window goes back to when it is no longer maximized or full screen.
    pub normal: Rect,
    pub min_size: (f32, f32),
    pub max_size: (f32, f32),
    pub visible: bool,
    pub focused: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub title: String,
    pub opacity: f64,
    pub background: [u8; 3],
    /// Whether the system draws a frame around the content.
    pub frame: bool,
    /// The system's handle of the window, once the shell has reported it.
    pub handle: u64,
    pub represented_filename: String,
    /// The messages `hookWindowMessage` was asked to watch.
    pub hooked: BTreeSet<u32>,
    flags: u32,
    work_area: Rect,
    screen: Rect,
}

impl WindowState {
    /// The state of a window made with `options` on `display`: where the shell puts it.
    pub(crate) fn new(options: &WindowOptions, display: &Display) -> Self {
        let screen = display.bounds;
        let area = display.work_area;
        let (width, height) = (options.width, options.height);
        let bounds = if options.fullscreen {
            screen
        } else {
            Rect {
                x: options.x.unwrap_or(area.x + (area.width - width) / 2.),
                y: options.y.unwrap_or(area.y + (area.height - height) / 2.),
                width,
                height,
            }
        };
        let mut state = Self {
            bounds,
            normal: bounds,
            min_size: options.min_size.unwrap_or_default(),
            max_size: options.max_size.unwrap_or_default(),
            visible: options.show,
            focused: options.show && options.focusable,
            minimized: false,
            maximized: false,
            fullscreen: options.fullscreen,
            title: options.title.clone(),
            opacity: 1.,
            background: [255; 3],
            frame: options.frame,
            handle: 0,
            represented_filename: String::new(),
            hooked: BTreeSet::new(),
            flags: 0,
            work_area: area,
            screen,
        };
        for flag in ALL {
            state.set_flag(flag, flag.on_at_start());
        }
        state.set_flag(Flag::Resizable, options.resizable);
        state.set_flag(Flag::Movable, options.movable);
        state.set_flag(Flag::Minimizable, options.minimizable);
        state.set_flag(Flag::Maximizable, options.maximizable);
        state.set_flag(Flag::Closable, options.closable);
        state.set_flag(Flag::Focusable, options.focusable);
        state.set_flag(Flag::AlwaysOnTop, options.always_on_top);
        state.set_flag(Flag::SkipTaskbar, options.skip_taskbar);
        state
    }

    pub(crate) fn flag(&self, flag: Flag) -> bool {
        self.flags & flag.bit() != 0
    }

    pub(crate) fn set_flag(&mut self, flag: Flag, on: bool) {
        if on {
            self.flags |= flag.bit();
        } else {
            self.flags &= !flag.bit();
        }
    }

    /// Whether the window can be seen: a minimized one cannot.
    pub(crate) fn is_visible(&self) -> bool {
        self.visible && !self.minimized
    }

    fn extents(&self) -> Extents {
        if self.frame && !self.fullscreen {
            FRAME
        } else {
            Extents {
                left: 0.,
                top: 0.,
                right: 0.,
                bottom: 0.,
            }
        }
    }

    /// The window's content: its bounds without the frame.
    pub(crate) fn content_bounds(&self) -> Rect {
        let e = self.extents();
        Rect {
            x: self.bounds.x + e.left,
            y: self.bounds.y + e.top,
            width: (self.bounds.width - e.left - e.right).max(0.),
            height: (self.bounds.height - e.top - e.bottom).max(0.),
        }
    }

    /// The bounds of the window whose content is `content`.
    pub(crate) fn bounds_for_content(&self, content: Rect) -> Rect {
        let e = self.extents();
        Rect {
            x: content.x - e.left,
            y: content.y - e.top,
            width: content.width + e.left + e.right,
            height: content.height + e.top + e.bottom,
        }
    }

    /// The bounds `sides` ask for: the sides left out keep their value, and every one is a whole
    /// number of pixels.
    pub(crate) fn merged(&self, sides: Sides) -> Rect {
        let old = self.bounds;
        Rect {
            x: sides.x.map_or(old.x, f32::round),
            y: sides.y.map_or(old.y, f32::round),
            width: sides.width.map_or(old.width, f32::round),
            height: sides.height.map_or(old.height, f32::round),
        }
    }

    /// Takes the effect `op` has on the window, before the shell says what came of it.
    pub(crate) fn predict(&mut self, op: &WindowOp) {
        match op {
            WindowOp::Show => {
                self.visible = true;
                self.focus_if_possible();
            }
            WindowOp::ShowInactive => self.visible = true,
            WindowOp::Hide => {
                self.visible = false;
                self.focused = false;
            }
            WindowOp::Focus => self.focus_if_possible(),
            WindowOp::Blur => self.focused = false,
            WindowOp::Minimize => {
                self.minimized = true;
                self.focused = false;
            }
            WindowOp::Restore if self.minimized => {
                self.minimized = false;
                self.visible = true;
                self.focus_if_possible();
            }
            WindowOp::Maximize => self.maximize(),
            WindowOp::Restore | WindowOp::Unmaximize => self.unmaximize(),
            WindowOp::SetFullScreen(on) => self.set_fullscreen(*on),
            WindowOp::SetBounds {
                x,
                y,
                width,
                height,
                ..
            } => self.place(self.merged(Sides {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            })),
            WindowOp::SetMinSize(width, height) => self.min_size = (*width, *height),
            WindowOp::SetMaxSize(width, height) => self.max_size = (*width, *height),
            WindowOp::Center => self.place(self.centred()),
            WindowOp::SetTitle(title) => self.title.clone_from(title),
            WindowOp::SetOpacity(opacity) => self.opacity = *opacity,
            WindowOp::SetBackgroundColor([r, g, b, _]) => self.background = [*r, *g, *b],
            _ => {}
        }
    }

    fn focus_if_possible(&mut self) {
        if self.visible && !self.minimized && self.flag(Flag::Focusable) {
            self.focused = true;
        }
    }

    /// The window moved to `bounds` by the app: that is where it goes back to as well, unless it
    /// is maximized or full screen.
    fn place(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if !self.maximized && !self.fullscreen {
            self.normal = bounds;
        }
    }

    fn centred(&self) -> Rect {
        let (area, bounds) = (self.work_area, self.bounds);
        Rect {
            x: area.x + (area.width - bounds.width) / 2.,
            y: area.y + (area.height - bounds.height) / 2.,
            ..bounds
        }
    }

    fn maximize(&mut self) {
        if !self.maximized && !self.fullscreen {
            self.normal = self.bounds;
        }
        self.maximized = true;
        self.bounds = self.work_area;
    }

    fn unmaximize(&mut self) {
        if self.maximized {
            self.maximized = false;
            self.bounds = self.normal;
        }
    }

    fn set_fullscreen(&mut self, on: bool) {
        if on == self.fullscreen {
            return;
        }
        if on {
            if !self.maximized {
                self.normal = self.bounds;
            }
            self.bounds = self.screen;
        } else {
            self.bounds = if self.maximized {
                self.work_area
            } else {
                self.normal
            };
        }
        self.fullscreen = on;
    }

    /// Takes what the shell says happened to the window.
    pub(crate) fn observe(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::Shown => self.visible = true,
            WindowEvent::Hidden => self.visible = false,
            WindowEvent::Focus => self.focused = true,
            WindowEvent::Blur => self.focused = false,
            WindowEvent::Resized { width, height } => {
                self.bounds.width = *width;
                self.bounds.height = *height;
                self.remember_normal();
            }
            WindowEvent::Moved { x, y } => {
                self.bounds.x = *x;
                self.bounds.y = *y;
                self.remember_normal();
            }
            WindowEvent::Maximized => self.maximized = true,
            WindowEvent::Unmaximized => self.maximized = false,
            WindowEvent::Minimized => self.minimized = true,
            WindowEvent::Restored => self.minimized = false,
            WindowEvent::EnterFullScreen => self.fullscreen = true,
            WindowEvent::LeaveFullScreen => self.fullscreen = false,
            WindowEvent::NativeHandle(handle) => self.handle = *handle,
            _ => {}
        }
    }

    fn remember_normal(&mut self) {
        if !self.maximized && !self.fullscreen {
            self.normal = self.bounds;
        }
    }

    /// The window's background as `getBackgroundColor()` answers it: `#RRGGBB`.
    pub(crate) fn background_text(&self) -> String {
        let [r, g, b] = self.background;
        format!("#{r:02X}{g:02X}{b:02X}")
    }

    /// The smallest a window of this system is made: a width or height below it is raised.
    pub(crate) fn smallest(size: (f32, f32)) -> (f32, f32) {
        (size.0.max(SMALLEST.0), size.1.max(SMALLEST.1))
    }

    /// The outside size of a window whose content is `width` x `height`.
    pub(crate) fn outside_of(width: f32, height: f32, frame: bool) -> (f32, f32) {
        if frame {
            (
                width + FRAME.left + FRAME.right,
                height + FRAME.top + FRAME.bottom,
            )
        } else {
            (width, height)
        }
    }
}

const ALL: [Flag; 18] = [
    Flag::Resizable,
    Flag::Movable,
    Flag::Minimizable,
    Flag::Maximizable,
    Flag::FullScreenable,
    Flag::Closable,
    Flag::Focusable,
    Flag::AlwaysOnTop,
    Flag::SimpleFullScreen,
    Flag::SkipTaskbar,
    Flag::ContentProtected,
    Flag::VisibleOnAllWorkspaces,
    Flag::HasShadow,
    Flag::MenuBarAutoHide,
    Flag::MenuBarVisible,
    Flag::DocumentEdited,
    Flag::Enabled,
    Flag::HiddenInMissionControl,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> Display {
        let screen = Rect {
            x: 0.,
            y: 0.,
            width: 1920.,
            height: 1080.,
        };
        Display {
            id: 1,
            bounds: screen,
            work_area: Rect {
                height: 1040.,
                ..screen
            },
            scale_factor: 1.,
            primary: true,
        }
    }

    fn state() -> WindowState {
        WindowState::new(&WindowOptions::default(), &display())
    }

    #[test]
    fn a_new_window_is_centred_with_electrons_flags() {
        let state = state();
        assert_eq!((state.bounds.x, state.bounds.y), (560., 220.));
        assert!(state.flag(Flag::Resizable) && state.flag(Flag::Enabled));
        assert!(!state.flag(Flag::AlwaysOnTop) && !state.flag(Flag::SkipTaskbar));
        assert!(state.visible && state.focused);
    }

    #[test]
    fn the_content_is_the_bounds_without_the_frame() {
        let state = state();
        let content = state.content_bounds();
        assert_eq!((content.width, content.height), (784., 561.));
        assert_eq!(state.bounds_for_content(content), state.bounds);
        let mut bare = state;
        bare.frame = false;
        assert_eq!(bare.content_bounds(), bare.bounds);
    }

    #[test]
    fn sides_left_out_keep_their_value_and_the_rest_is_rounded() {
        let mut state = state();
        state.predict(&WindowOp::SetBounds {
            x: Some(10.6),
            y: None,
            width: Some(450.),
            height: None,
            animate: false,
        });
        assert_eq!(
            (state.bounds.x, state.bounds.y, state.bounds.width),
            (11., 220., 450.)
        );
    }

    #[test]
    fn maximizing_remembers_the_normal_bounds_and_unmaximizing_gives_them_back() {
        let mut state = state();
        let before = state.bounds;
        state.predict(&WindowOp::Maximize);
        assert!(state.maximized);
        assert_eq!(state.bounds.height, 1040.);
        assert_eq!(state.normal, before);
        state.predict(&WindowOp::Unmaximize);
        assert!(!state.maximized);
        assert_eq!(state.bounds, before);
    }

    #[test]
    fn minimizing_hides_and_blurs_and_restoring_comes_back() {
        let mut state = state();
        state.predict(&WindowOp::Minimize);
        assert!(state.minimized && !state.is_visible() && !state.focused);
        state.predict(&WindowOp::Restore);
        assert!(!state.minimized && state.is_visible() && state.focused);
    }

    #[test]
    fn the_shells_events_correct_the_cache() {
        let mut state = state();
        state.observe(&WindowEvent::Resized {
            width: 500.,
            height: 400.,
        });
        state.observe(&WindowEvent::Blur);
        assert_eq!((state.bounds.width, state.normal.width), (500., 500.));
        assert!(!state.focused);
    }

    #[test]
    fn the_background_reads_as_upper_case_hex() {
        let mut state = state();
        state.predict(&WindowOp::SetBackgroundColor([0x12, 0xab, 0x03, 0xff]));
        assert_eq!(state.background_text(), "#12AB03");
    }
}
