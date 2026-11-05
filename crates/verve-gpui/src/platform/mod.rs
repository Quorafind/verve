//! What the system does to a window that GPUI does not give the app, one file per system.
//!
//! Every system's `Native` has the same methods, called from the shell's own task and never
//! while GPUI holds the app (a Win32 call sends the window procedure messages whose callbacks
//! need the app). Windows has all of them; macOS and Linux have what GPUI offers, and macOS
//! the `NSWindow` calls GPUI leaves out.

#[cfg(windows)]
mod taskbar;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{Native, displays};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(windows))]
mod other;
#[cfg(not(windows))]
pub(crate) use other::{Native, displays};

/// A window as the system names it, and as GPUI does.
pub(crate) struct Target {
    /// The `HWND`, or the `NSView` on macOS; 0 where there is none to give.
    pub raw: isize,
    #[cfg(not(windows))]
    pub any: gpui::AnyWindowHandle,
}

impl Target {
    pub fn of(window: &gpui::Window) -> Self {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let raw = match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
            Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view.as_ptr() as isize,
            _ => 0,
        };
        Self {
            raw,
            #[cfg(not(windows))]
            any: window.window_handle(),
        }
    }
}
