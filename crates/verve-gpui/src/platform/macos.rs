//! macOS: the `NSWindow` calls GPUI leaves out, on the window of the GPUI view. Everything here
//! runs on the main thread, which is where the shell's task runs.

use objc2::rc::Retained;
use objc2_app_kit::{
    NSFloatingWindowLevel, NSNormalWindowLevel, NSView, NSWindow, NSWindowButton,
    NSWindowOrderingMode,
};
use objc2_foundation::NSSize;
use verve_electron::msg::WindowOptions;

/// The `NSWindow` of the `NSView` GPUI gave out (`Target::raw`).
fn window(view: isize) -> Option<Retained<NSWindow>> {
    // SAFETY: `view` is the content view of a live GPUI window, which `Target::of` read.
    let view = unsafe { (view as *const NSView).as_ref()? };
    view.window()
}

/// Shows the window, key if `activate`, or hides it.
pub(super) fn set_shown(view: isize, shown: bool, activate: bool) {
    let Some(window) = window(view) else { return };
    match (shown, activate) {
        (false, _) => window.orderOut(None),
        (true, true) => window.makeKeyAndOrderFront(None),
        (true, false) => window.orderFront(None),
    }
}

/// Floats the window above the others, or puts it back.
pub(super) fn set_always_on_top(view: isize, on: bool) {
    if let Some(window) = window(view) {
        window.setLevel(if on {
            NSFloatingWindowLevel
        } else {
            NSNormalWindowLevel
        });
    }
}

pub(super) fn set_opacity(view: isize, opacity: f64) {
    if let Some(window) = window(view) {
        window.setAlphaValue(opacity.clamp(0., 1.));
    }
}

/// The smallest and largest the window may be, as `NSWindow` has them (0 is no limit for a maximum).
pub(super) fn limit(view: isize, min: Option<(f32, f32)>, max: Option<(f32, f32)>) {
    let Some(window) = window(view) else { return };
    if let Some((width, height)) = min {
        window.setMinSize(NSSize::new(width.into(), height.into()));
    }
    if let Some((width, height)) = max {
        let unlimited = f64::from(f32::MAX);
        let side = |value: f32| {
            if value > 0. {
                f64::from(value)
            } else {
                unlimited
            }
        };
        window.setMaxSize(NSSize::new(side(width), side(height)));
    }
}

/// Makes `child` follow `parent` in front of it, as a child window does.
pub(super) fn adopt(parent: isize, child: isize) {
    if let (Some(parent), Some(child)) = (window(parent), window(child)) {
        // SAFETY: both are live windows of this process, and `Above` is a valid ordering.
        unsafe { parent.addChildWindow_ordered(&child, NSWindowOrderingMode::Above) };
    }
}

/// Lets `child` go its own way from `parent`.
pub(super) fn detach(parent: isize, child: isize) {
    if let (Some(parent), Some(child)) = (window(parent), window(child)) {
        parent.removeChildWindow(&child);
    }
}

/// The traffic lights of a window that has no frame are not wanted.
pub(super) fn hide_traffic_lights(view: isize, options: &WindowOptions) {
    let Some(window) = window(view) else { return };
    if options.frame {
        return;
    }
    for button in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        if let Some(button) = window.standardWindowButton(button) {
            button.setHidden(true);
        }
    }
}

/// Whether the window is in the Dock.
pub(super) fn is_miniaturized(view: isize) -> bool {
    window(view).is_some_and(|window| window.isMiniaturized())
}
