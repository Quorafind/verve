//! Windows: what GPUI's window does not do, done to its `HWND`.
//!
//! GPUI makes its windows without a caption and leaves the rest of what Electron's windows are to
//! the app, so this adds the caption, the limits, the owner, the styles and the taskbar's part, and
//! watches the window's messages to tell the reporter that something changed. Nothing here is
//! called inside a GPUI update that holds the app: a Win32 call that changes a window sends the
//! window procedure messages, whose callbacks need the app, so the shell calls these from its own
//! task. (Reading a window from a frame, as `observe` does, sends none.)

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use verve_electron::msg::{Display, Rect, WindowOptions};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, ScreenToClient,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging as w;

use gpui::AsyncApp;

use super::{Target, taskbar};
use crate::report::Observed;

/// A Win32 call that takes handles and plain values (no pointer but to locals the call fills in).
/// The handle is `Native::hwnd`: a `Native` lives in its `Window`, which the shell drops when the
/// window is destroyed (`Shell::destroy`). When the system destroys the window by itself,
/// `WM_DESTROY` queues `Gone`, and until the shell takes that item, operations already queued
/// still call into a destroyed handle, which the system may have given to a new window. That is
/// memory-safe (a handle is an integer the system validates) but such a call can reach the wrong
/// window; the window is almost never recreated in that gap, and the reporter ignores a closed
/// window. Pointers handed to the system are checked where they are made.
macro_rules! sys {
    ($call:expr) => {
        // SAFETY: as the macro's documentation says.
        unsafe { $call }
    };
}

/// Posted to a window when something about it changed: the look is taken once the messages of
/// the change are all through, so a minimize is seen whole and not between its deactivation and
/// its `WM_SIZE`.
const LOOK: u32 = w::WM_APP + 0x34;

/// The limits `minWidth` and the rest put on the sizes of a window, in device-independent pixels.
#[derive(Clone, Copy, Default)]
struct Limits {
    min: Option<(f32, f32)>,
    max: Option<(f32, f32)>,
}

/// What the subclassed window procedure of one window needs.
struct Hook {
    previous: isize,
    limits: Cell<Limits>,
    looking: Cell<bool>,
    look: RefCell<Option<Rc<dyn Fn()>>>,
    /// Called once, when the system destroys the window, whoever asked it to.
    destroyed: RefCell<Option<Rc<dyn Fn()>>>,
    /// Whether the window covers its display: it is then not dragged, resized or maximized.
    fullscreen: Cell<bool>,
    resizable: Cell<bool>,
    maximizable: Cell<bool>,
    /// What the page says the window is dragged by.
    drag: RefCell<Vec<Rect>>,
}

thread_local! {
    /// How many shown modal children each owner has: it is disabled while there is one.
    static BLOCKS: RefCell<HashMap<isize, u32>> = RefCell::new(HashMap::new());
}

thread_local! {
    static HOOKS: RefCell<HashMap<isize, Rc<Hook>>> = RefCell::new(HashMap::new());
}

fn hook_of(hwnd: HWND) -> Option<Rc<Hook>> {
    HOOKS.with(|hooks| hooks.borrow().get(&(hwnd as isize)).cloned())
}

/// One window, by its handle.
pub(crate) struct Native {
    hwnd: HWND,
    owner: Option<HWND>,
    modal: bool,
    /// Whether this modal child is shown, and so counted in its owner's `BLOCKS`.
    blocking: Cell<bool>,
    /// The icon the window has now, which the next one replaces and destroys.
    icon: Cell<w::HICON>,
    /// The style, the place and whether it was maximized before it went full screen.
    before_fullscreen: Cell<Option<(isize, RECT, bool)>>,
}

impl Native {
    /// Takes over the window `hwnd`, which GPUI made hidden, as `options` ask: its frame, styles,
    /// owner, limits and place. It is not shown.
    pub fn attach(
        target: Target,
        options: &WindowOptions,
        owner: Option<&Native>,
        _: &AsyncApp,
    ) -> Rc<Self> {
        let native = Rc::new(Self {
            hwnd: target.raw as HWND,
            owner: owner.map(|owner| owner.hwnd),
            modal: options.modal,
            blocking: Cell::new(false),
            icon: Cell::new(std::ptr::null_mut()),
            before_fullscreen: Cell::new(None),
        });
        native.restyle(options);
        native.hook(options);
        if let Some(owner) = native.owner {
            sys!(w::SetWindowLongPtrW(
                native.hwnd,
                w::GWLP_HWNDPARENT,
                owner as isize
            ));
        }
        native
    }

    /// The system's handle of the window.
    pub fn handle(&self) -> u64 {
        self.hwnd as u64
    }

    /// The look a frame of `window` finds: none, since the window is looked at when its messages say so.
    pub fn looked(&self, _: &gpui::Window) -> Option<Observed> {
        None
    }

    /// Calls `look` whenever the window changes, once the messages of the change are through.
    pub fn watch(&self, look: Rc<dyn Fn()>) {
        if let Some(hook) = hook_of(self.hwnd) {
            *hook.look.borrow_mut() = Some(look);
        }
    }

    fn restyle(&self, options: &WindowOptions) {
        let mut style = self.long(w::GWL_STYLE) as u32;
        let mut ex = self.long(w::GWL_EXSTYLE) as u32;
        // `frame: false` and a hidden title bar are the same window here: Electron draws no
        // title bar for either, and GPUI's window has none to begin with.
        let framed = crate::window::framed(options);
        set_flag(&mut style, w::WS_CAPTION, framed);
        if !options.maximizable {
            style &= !w::WS_MAXIMIZEBOX;
        }
        set_flag(&mut ex, w::WS_EX_NOACTIVATE, !options.focusable);
        set_flag(&mut ex, w::WS_EX_TOOLWINDOW, options.skip_taskbar);
        set_flag(&mut ex, w::WS_EX_APPWINDOW, !options.skip_taskbar);
        self.set_long(w::GWL_STYLE, style as isize);
        self.set_long(w::GWL_EXSTYLE, ex as isize);
        self.reframe();
        if !options.closable {
            self.set_closable(false);
        }
        if !options.movable {
            sys!(w::DeleteMenu(
                w::GetSystemMenu(self.hwnd, 0),
                w::SC_MOVE,
                w::MF_BYCOMMAND
            ));
        }
    }

    fn hook(&self, options: &WindowOptions) {
        let previous = self.long(w::GWLP_WNDPROC);
        let limits = Limits {
            min: options.min_size,
            max: options.max_size,
        };
        let hook = Hook {
            previous,
            limits: Cell::new(limits),
            looking: Cell::new(false),
            look: RefCell::new(None),
            destroyed: RefCell::new(None),
            fullscreen: Cell::new(false),
            resizable: Cell::new(options.resizable),
            maximizable: Cell::new(options.maximizable),
            drag: RefCell::new(Vec::new()),
        };
        HOOKS.with(|hooks| hooks.borrow_mut().insert(self.hwnd as isize, Rc::new(hook)));
        self.set_long(w::GWLP_WNDPROC, procedure as *const () as isize);
    }

    /// Looks at the window.
    pub fn observe(&self) -> Observed {
        let (mut outer, mut inner) = (RECT::default(), RECT::default());
        let scale = self.scale();
        sys!({
            w::GetWindowRect(self.hwnd, &mut outer);
            w::GetClientRect(self.hwnd, &mut inner);
        });
        let fullscreen = self.before_fullscreen.get().is_some();
        let dip = |pixels: i32| pixels as f32 / scale;
        Observed {
            visible: sys!(w::IsWindowVisible(self.hwnd)) != 0,
            focused: sys!(w::GetForegroundWindow()) == self.hwnd,
            minimized: sys!(w::IsIconic(self.hwnd)) != 0,
            maximized: !fullscreen && sys!(w::IsZoomed(self.hwnd)) != 0,
            fullscreen,
            bounds: Rect {
                x: dip(outer.left),
                y: dip(outer.top),
                width: dip(outer.right - outer.left),
                height: dip(outer.bottom - outer.top),
            },
            content: (dip(inner.right), dip(inner.bottom)),
            scale,
        }
    }

    /// Device pixels per device-independent pixel on the window's display.
    pub fn scale(&self) -> f32 {
        scale_of(sys!(GetDpiForWindow(self.hwnd)))
    }

    /// The work area of the display the window is on.
    pub fn work_area(&self) -> Rect {
        let (_, work, scale, _) =
            monitor(sys!(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST)));
        dip_rect(work, scale)
    }

    /// The window's whole display.
    fn screen(&self) -> RECT {
        monitor(sys!(MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST))).0
    }

    /// Calls `gone` once when the system destroys the window, as it does when GPUI's close
    /// callback could not answer, or when the system ends the session.
    pub fn on_destroy(&self, gone: Rc<dyn Fn()>) {
        if let Some(hook) = hook_of(self.hwnd) {
            *hook.destroyed.borrow_mut() = Some(gone);
        }
    }

    pub fn show(&self, activate: bool) {
        self.block_owner();
        let command = if activate { w::SW_SHOW } else { w::SW_SHOWNA };
        sys!(w::ShowWindow(self.hwnd, command));
        if activate && self.long(w::GWL_EXSTYLE) as u32 & w::WS_EX_NOACTIVATE == 0 {
            sys!(w::SetForegroundWindow(self.hwnd));
        }
    }

    pub fn hide(&self) {
        // The owner is enabled before the child goes, as Electron does, so that the system gives
        // the activation to it and not to another application.
        self.unblock_owner();
        sys!(w::ShowWindow(self.hwnd, w::SW_HIDE));
    }

    /// Takes the focus, if the window can have it.
    pub fn focus(&self) {
        let shown = sys!(w::IsWindowVisible(self.hwnd) != 0 && w::IsIconic(self.hwnd) == 0);
        if shown && self.long(w::GWL_EXSTYLE) as u32 & w::WS_EX_NOACTIVATE == 0 {
            sys!(w::SetForegroundWindow(self.hwnd));
        }
    }

    /// Gives the focus to the next window down, as Chromium's `Deactivate` does.
    pub fn blur(&self) {
        if sys!(w::GetForegroundWindow()) != self.hwnd {
            return;
        }
        let mut next = sys!(w::GetWindow(self.hwnd, w::GW_HWNDNEXT));
        while !next.is_null() {
            if sys!(w::IsWindowVisible(next)) != 0 {
                sys!(w::SetForegroundWindow(next));
                return;
            }
            next = sys!(w::GetWindow(next, w::GW_HWNDNEXT));
        }
    }

    pub fn minimize(&self) {
        sys!(w::ShowWindow(self.hwnd, w::SW_MINIMIZE));
    }

    pub fn maximize(&self) {
        if sys!(w::IsWindowVisible(self.hwnd)) != 0 {
            sys!(w::ShowWindow(self.hwnd, w::SW_MAXIMIZE));
        }
    }

    /// Back from minimized, or else from maximized.
    pub fn restore(&self) {
        let (iconic, zoomed) = sys!((w::IsIconic(self.hwnd), w::IsZoomed(self.hwnd)));
        if iconic != 0 || zoomed != 0 {
            sys!(w::ShowWindow(self.hwnd, w::SW_RESTORE));
        }
    }

    /// Puts the window's frame at `bounds`, held within its limits.
    pub fn place(&self, bounds: Rect) {
        let scale = self.scale();
        let (width, height) = self.clamped(bounds.width, bounds.height);
        let pixels = |dip: f32| (dip * scale).round() as i32;
        sys!({
            w::SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                pixels(bounds.x),
                pixels(bounds.y),
                pixels(width),
                pixels(height),
                w::SWP_NOZORDER | w::SWP_NOACTIVATE,
            );
        })
    }

    fn clamped(&self, mut width: f32, mut height: f32) -> (f32, f32) {
        let limits = hook_of(self.hwnd)
            .map(|hook| hook.limits.get())
            .unwrap_or_default();
        if let Some((min_width, min_height)) = limits.min {
            (width, height) = (width.max(min_width), height.max(min_height));
        }
        if let Some((max_width, max_height)) = limits.max {
            // A maximum of 0 is Electron's "no limit".
            width = if max_width > 0. {
                width.min(max_width)
            } else {
                width
            };
            height = if max_height > 0. {
                height.min(max_height)
            } else {
                height
            };
        }
        (width, height)
    }

    /// Changes the limits, and the size if it is outside them now.
    pub fn limit(&self, min: Option<(f32, f32)>, max: Option<(f32, f32)>) {
        let Some(hook) = hook_of(self.hwnd) else {
            return;
        };
        let old = hook.limits.get();
        hook.limits.set(Limits {
            min: min.or(old.min),
            max: max.or(old.max),
        });
        let now = self.observe().bounds;
        self.place(now);
    }

    pub fn set_resizable(&self, resizable: bool) {
        let hook = hook_of(self.hwnd);
        let maximizable = hook.as_ref().is_some_and(|hook| hook.maximizable.get());
        hook.iter().for_each(|hook| hook.resizable.set(resizable));
        let change = |style: isize| {
            let mut style = style as u32;
            set_flag(&mut style, w::WS_THICKFRAME, resizable);
            set_flag(&mut style, w::WS_MAXIMIZEBOX, resizable && maximizable);
            style as isize
        };
        // A full-screen window keeps its covering style; the change waits in the saved one.
        if let Some((style, rect, zoomed)) = self.before_fullscreen.get() {
            self.before_fullscreen
                .set(Some((change(style), rect, zoomed)));
            return;
        }
        self.set_long(w::GWL_STYLE, change(self.long(w::GWL_STYLE)));
        self.reframe();
    }

    pub fn set_closable(&self, closable: bool) {
        let flags = if closable {
            w::MF_ENABLED
        } else {
            w::MF_GRAYED
        };
        let menu = sys!(w::GetSystemMenu(self.hwnd, 0));
        sys!(w::EnableMenuItem(
            menu,
            w::SC_CLOSE,
            w::MF_BYCOMMAND | flags
        ));
    }

    pub fn set_always_on_top(&self, on: bool) {
        let level = if on {
            w::HWND_TOPMOST
        } else {
            w::HWND_NOTOPMOST
        };
        let flags = w::SWP_NOMOVE | w::SWP_NOSIZE | w::SWP_NOACTIVATE;
        sys!(w::SetWindowPos(self.hwnd, level, 0, 0, 0, 0, flags));
    }

    /// `opacity` from 0 to 1.
    pub fn set_opacity(&self, opacity: f64) {
        let alpha = (opacity.clamp(0., 1.) * 255.).round() as u8;
        self.layered(|| {
            sys!(w::SetLayeredWindowAttributes(
                self.hwnd,
                0,
                alpha,
                w::LWA_ALPHA
            ))
        });
    }

    /// Whether the mouse passes through the window to what is under it.
    pub fn set_ignore_mouse(&self, ignore: bool) {
        let mut ex = self.long(w::GWL_EXSTYLE) as u32;
        set_flag(&mut ex, w::WS_EX_TRANSPARENT | w::WS_EX_LAYERED, ignore);
        self.set_long(w::GWL_EXSTYLE, ex as isize);
        if ignore {
            self.layered(|| {
                sys!(w::SetLayeredWindowAttributes(
                    self.hwnd,
                    0,
                    255,
                    w::LWA_ALPHA
                ))
            });
        }
    }

    pub fn set_skip_taskbar(&self, skip: bool) {
        let visible = sys!(w::IsWindowVisible(self.hwnd)) != 0;
        let focused = sys!(w::GetForegroundWindow()) == self.hwnd;
        if visible {
            sys!(w::ShowWindow(self.hwnd, w::SW_HIDE));
        }
        let mut ex = self.long(w::GWL_EXSTYLE) as u32;
        set_flag(&mut ex, w::WS_EX_TOOLWINDOW, skip);
        set_flag(&mut ex, w::WS_EX_APPWINDOW, !skip);
        self.set_long(w::GWL_EXSTYLE, ex as isize);
        if visible {
            sys!(w::ShowWindow(self.hwnd, w::SW_SHOWNA));
            if focused {
                sys!(w::SetForegroundWindow(self.hwnd));
            }
        }
    }

    /// The window's title, which is the caption's text.
    pub fn set_title(&self, title: &str) {
        let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
        sys!(w::SetWindowTextW(self.hwnd, wide.as_ptr()));
    }

    /// `-1` removes the bar; a value from 0 to 1 fills it; anything above 1 is indeterminate.
    pub fn set_progress(&self, progress: f64) {
        taskbar::progress(self.hwnd, progress);
    }

    pub fn flash(&self, on: bool) {
        let info = w::FLASHWINFO {
            cbSize: std::mem::size_of::<w::FLASHWINFO>() as u32,
            hwnd: self.hwnd,
            dwFlags: if on {
                w::FLASHW_ALL | w::FLASHW_TIMERNOFG
            } else {
                w::FLASHW_STOP
            },
            uCount: u32::MAX,
            dwTimeout: 0,
        };
        sys!(w::FlashWindowEx(&info));
    }

    /// The icon of the window's caption and taskbar button, from a `.ico` or a `.png`; a file that
    /// is neither leaves the icon as it was.
    pub fn set_icon(&self, path: &Path) {
        let Some(icon) = load_icon(path) else {
            return;
        };
        for size in [w::ICON_BIG, w::ICON_SMALL] {
            sys!(w::SendMessageW(
                self.hwnd,
                w::WM_SETICON,
                size as WPARAM,
                icon as LPARAM
            ));
        }
        // The window has the new icon now: the one it had is nobody's.
        let old = self.icon.replace(icon);
        if !old.is_null() {
            sys!(w::DestroyIcon(old));
        }
    }

    /// Covers the display with the window, or gives back the frame and the place it had.
    pub fn set_fullscreen(&self, on: bool) {
        match (on, self.before_fullscreen.get()) {
            (true, None) => self.enter_fullscreen(),
            (false, Some(saved)) => self.leave_fullscreen(saved),
            _ => {}
        }
    }

    fn enter_fullscreen(&self) {
        let (mut rect, style) = (RECT::default(), self.long(w::GWL_STYLE));
        let zoomed = sys!(w::IsZoomed(self.hwnd)) != 0;
        if zoomed {
            sys!(w::ShowWindow(self.hwnd, w::SW_RESTORE));
        }
        sys!(w::GetWindowRect(self.hwnd, &mut rect));
        self.before_fullscreen.set(Some((style, rect, zoomed)));
        hook_of(self.hwnd)
            .iter()
            .for_each(|hook| hook.fullscreen.set(true));
        let covered = (w::WS_CAPTION | w::WS_THICKFRAME | w::WS_MAXIMIZEBOX) as isize;
        self.set_long(w::GWL_STYLE, style & !covered);
        self.cover(self.screen());
    }

    fn leave_fullscreen(&self, (style, rect, zoomed): (isize, RECT, bool)) {
        self.before_fullscreen.set(None);
        hook_of(self.hwnd)
            .iter()
            .for_each(|hook| hook.fullscreen.set(false));
        self.set_long(w::GWL_STYLE, style);
        self.cover(rect);
        if zoomed {
            sys!(w::ShowWindow(self.hwnd, w::SW_MAXIMIZE));
        }
    }

    fn cover(&self, rect: RECT) {
        let flags = w::SWP_FRAMECHANGED | w::SWP_NOACTIVATE | w::SWP_NOZORDER;
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        sys!({
            w::SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                rect.left,
                rect.top,
                width,
                height,
                flags,
            )
        });
    }

    /// Lets go of what the window held: the owner it blocked.
    pub fn release(&self) {
        self.unblock_owner();
    }

    /// The parts of the window it is dragged by, in device-independent pixels of its content.
    pub fn set_drag_area(&self, area: Vec<Rect>) {
        if let Some(hook) = hook_of(self.hwnd) {
            *hook.drag.borrow_mut() = area;
        }
    }

    /// A modal child that is shown stops its owner taking input; the owner is disabled while any
    /// of its modal children is shown, as Electron's `IncrementChildModals` does.
    fn block_owner(&self) {
        let (true, Some(owner)) = (self.modal, self.owner) else {
            return;
        };
        if !self.blocking.replace(true) {
            BLOCKS.with(|blocks| *blocks.borrow_mut().entry(owner as isize).or_insert(0) += 1);
            sys!(EnableWindow(owner, 0));
        }
    }

    fn unblock_owner(&self) {
        let (true, Some(owner)) = (self.modal, self.owner) else {
            return;
        };
        if !self.blocking.replace(false) {
            return;
        }
        let left = BLOCKS.with(|blocks| {
            let mut blocks = blocks.borrow_mut();
            let count = blocks.entry(owner as isize).or_insert(1);
            *count = count.saturating_sub(1);
            *count
        });
        if left == 0 {
            sys!(EnableWindow(owner, 1));
            sys!(w::SetForegroundWindow(owner));
        }
    }

    fn layered(&self, set: impl FnOnce() -> i32) {
        let mut ex = self.long(w::GWL_EXSTYLE) as u32;
        ex |= w::WS_EX_LAYERED;
        self.set_long(w::GWL_EXSTYLE, ex as isize);
        set();
    }

    fn reframe(&self) {
        let flags = w::SWP_FRAMECHANGED
            | w::SWP_NOMOVE
            | w::SWP_NOSIZE
            | w::SWP_NOZORDER
            | w::SWP_NOACTIVATE;
        sys!(w::SetWindowPos(
            self.hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            flags
        ));
    }

    fn long(&self, index: w::WINDOW_LONG_PTR_INDEX) -> isize {
        sys!(w::GetWindowLongPtrW(self.hwnd, index))
    }

    fn set_long(&self, index: w::WINDOW_LONG_PTR_INDEX, value: isize) {
        sys!(w::SetWindowLongPtrW(self.hwnd, index, value));
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        // The window goes with this: the icon it was given is nobody's now.
        let icon = self.icon.replace(std::ptr::null_mut());
        if !icon.is_null() {
            sys!(w::DestroyIcon(icon));
        }
    }
}

fn set_flag(bits: &mut u32, flag: u32, on: bool) {
    if on {
        *bits |= flag;
    } else {
        *bits &= !flag;
    }
}

/// The window procedure the window has from [`Native::attach`] on: GPUI's, with the limits
/// Electron puts on a window and a note to the reporter that the window changed.
unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(hook) = hook_of(hwnd) else {
        return sys!(w::DefWindowProcW(hwnd, msg, wparam, lparam));
    };
    // SAFETY: `previous` was read from this very window with `GWLP_WNDPROC` in `Native::hook`
    // and is a window procedure, so it has the type `WNDPROC` names.
    let previous: w::WNDPROC = sys!(std::mem::transmute::<isize, w::WNDPROC>(hook.previous));
    let noactivate = sys!(w::GetWindowLongPtrW(hwnd, w::GWL_EXSTYLE)) as u32 & w::WS_EX_NOACTIVATE;
    if msg == w::WM_MOUSEACTIVATE && noactivate != 0 {
        // GPUI answers every click with `MA_ACTIVATE`, which would defeat `focusable: false`.
        return w::MA_NOACTIVATE as LRESULT;
    }
    let result = sys!(w::CallWindowProcW(previous, hwnd, msg, wparam, lparam));
    let sizing = [w::HTTOP, w::HTTOPLEFT, w::HTTOPRIGHT].contains(&(result as u32));
    if msg == w::WM_NCHITTEST && (hook.fullscreen.get() || (!hook.resizable.get() && sizing)) {
        // GPUI's answers (drag area, top edge) are for a window that is not full screen and
        // resizable. GPUI tests the top edge before the drag area, so a drag bar that reaches the
        // top of a window that cannot be resized is answered here.
        let dragged = !hook.fullscreen.get() && in_drag_area(hwnd, lparam, &hook.drag.borrow());
        return if dragged { w::HTCAPTION } else { w::HTCLIENT } as LRESULT;
    }
    match msg {
        w::WM_GETMINMAXINFO => limit_tracking(hwnd, lparam, hook.limits.get()),
        LOOK => {
            hook.looking.set(false);
            let look = hook.look.borrow().clone();
            look.iter().for_each(|look| look());
        }
        w::WM_SIZE | w::WM_MOVE | w::WM_SHOWWINDOW | w::WM_ACTIVATE | w::WM_DPICHANGED
            if !hook.looking.replace(true) =>
        {
            sys!(w::PostMessageW(hwnd, LOOK, 0, 0));
        }
        w::WM_DESTROY => {
            let gone = hook.destroyed.borrow_mut().take();
            gone.iter().for_each(|gone| gone());
        }
        w::WM_NCDESTROY => HOOKS.with(|hooks| drop(hooks.borrow_mut().remove(&(hwnd as isize)))),
        _ => {}
    }
    result
}

/// Whether the screen point of a `WM_NCHITTEST` is in one of the rectangles of the window's content.
fn in_drag_area(hwnd: HWND, lparam: LPARAM, area: &[Rect]) -> bool {
    let mut at = POINT {
        x: (lparam & 0xffff) as i16 as i32,
        y: ((lparam >> 16) & 0xffff) as i16 as i32,
    };
    sys!(ScreenToClient(hwnd, &mut at));
    let scale = scale_of(sys!(GetDpiForWindow(hwnd)));
    let (x, y) = (at.x as f32 / scale, at.y as f32 / scale);
    area.iter()
        .any(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
}

/// Puts the app's limits on the sizes the system lets a person drag the window to.
fn limit_tracking(hwnd: HWND, lparam: LPARAM, limits: Limits) {
    let scale = scale_of(sys!(GetDpiForWindow(hwnd)));
    // SAFETY: for `WM_GETMINMAXINFO`, `lparam` points at the `MINMAXINFO` to fill in, valid
    // for the length of the message.
    let info = sys!(&mut *(lparam as *mut w::MINMAXINFO));
    if let Some((width, height)) = limits.min {
        info.ptMinTrackSize.x = (width * scale) as i32;
        info.ptMinTrackSize.y = (height * scale) as i32;
    }
    if let Some((width, height)) = limits.max {
        if width > 0. {
            info.ptMaxTrackSize.x = (width * scale) as i32;
        }
        if height > 0. {
            info.ptMaxTrackSize.y = (height * scale) as i32;
        }
    }
}

/// Device pixels per device-independent pixel in a window of `dpi`.
fn scale_of(dpi: u32) -> f32 {
    dpi.max(96) as f32 / 96.
}

/// A display: its whole area, its work area, its scale, and whether it is the primary one.
fn monitor(handle: HMONITOR) -> (RECT, RECT, f32, bool) {
    // SAFETY: `MONITORINFO` is integers and rectangles, for which all zeros is a value.
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..sys!(std::mem::zeroed())
    };
    sys!(GetMonitorInfoW(handle, &mut info));
    let (mut x, mut y) = (96, 96);
    sys!(GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut x, &mut y));
    (
        info.rcMonitor,
        info.rcWork,
        scale_of(x),
        info.dwFlags & 1 != 0,
    )
}

fn dip_rect(rect: RECT, scale: f32) -> Rect {
    Rect {
        x: rect.left as f32 / scale,
        y: rect.top as f32 / scale,
        width: (rect.right - rect.left) as f32 / scale,
        height: (rect.bottom - rect.top) as f32 / scale,
    }
}

unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> i32 {
    // SAFETY: `list` is the `Vec` `displays` passed in, alive for the whole enumeration.
    sys!(&mut *(list as *mut Vec<HMONITOR>)).push(handle);
    1
}

/// The displays the system has now.
pub(crate) fn displays(_: &AsyncApp) -> Vec<Display> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    let list = &mut handles as *mut Vec<HMONITOR> as LPARAM;
    sys!(EnumDisplayMonitors(
        std::ptr::null_mut(),
        std::ptr::null(),
        Some(collect),
        list
    ));
    handles
        .into_iter()
        .map(|handle| {
            let (whole, work, scale, primary) = monitor(handle);
            Display {
                id: handle as usize as u32,
                bounds: dip_rect(whole, scale),
                work_area: dip_rect(work, scale),
                scale_factor: scale,
                primary,
            }
        })
        .collect()
}

/// An icon from a `.ico`, or a `.png` (which the system reads as an icon's image).
fn load_icon(path: &Path) -> Option<w::HICON> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"\x89PNG") {
        let icon = sys!({
            w::CreateIconFromResourceEx(
                bytes.as_ptr(),
                bytes.len() as u32,
                1,
                0x0003_0000,
                0,
                0,
                w::LR_DEFAULTCOLOR,
            )
        });
        return (!icon.is_null()).then_some(icon);
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let flags = w::LR_LOADFROMFILE | w::LR_DEFAULTSIZE;
    let icon = sys!({
        w::LoadImageW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            w::IMAGE_ICON,
            0,
            0,
            flags,
        )
    });
    (!icon.is_null()).then_some(icon as w::HICON)
}
