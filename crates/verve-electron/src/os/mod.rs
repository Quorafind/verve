//! What the system answers the main process without a window: one module per platform, the same
//! functions in each. The windows that dialogs and displays belong to are the shell's.

mod clipboard;
mod clipboard_memory;
#[cfg(windows)]
mod clipboard_windows;
#[cfg(not(windows))]
mod other;
#[cfg(windows)]
mod windows;

pub(crate) use clipboard::{Clip, Clipboard};
#[cfg(not(windows))]
pub(crate) use other::*;
#[cfg(windows)]
pub(crate) use windows::*;

/// What a `powerSaveBlocker` keeps the machine from doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    /// `prevent-app-suspension`: the system keeps running.
    AppSuspension,
    /// `prevent-display-sleep`: the screen stays on as well.
    DisplaySleep,
}

/// A system colour `systemPreferences.getColor` knows by name.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SystemColor(&'static str);

impl SystemColor {
    const NAMES: [&'static str; 30] = [
        "3d-dark-shadow",
        "3d-face",
        "3d-highlight",
        "3d-light",
        "3d-shadow",
        "active-border",
        "active-caption",
        "active-caption-gradient",
        "app-workspace",
        "button-text",
        "caption-text",
        "desktop",
        "disabled-text",
        "highlight",
        "highlight-text",
        "hotlight",
        "inactive-border",
        "inactive-caption",
        "inactive-caption-gradient",
        "inactive-caption-text",
        "info-background",
        "info-text",
        "menu",
        "menu-highlight",
        "menu-bar",
        "menu-text",
        "scrollbar",
        "window",
        "window-frame",
        "window-text",
    ];

    /// The colour called `name` in Electron's documentation.
    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::NAMES
            .iter()
            .find(|known| **known == name)
            .map(|known| Self(known))
    }

    /// The name Electron's documentation gives it.
    pub(crate) fn name(self) -> &'static str {
        self.0
    }
}
