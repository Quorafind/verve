//! The system's answers where Verve has none of its own yet: what a plain desktop in a light theme
//! would say.

use super::{Block, SystemColor};

pub(crate) fn cursor_position() -> (i32, i32) {
    (0, 0)
}

pub(crate) fn high_contrast() -> bool {
    false
}

pub(crate) fn reduced_transparency() -> bool {
    false
}

pub(crate) fn animations_enabled() -> bool {
    true
}

pub(crate) fn idle_seconds() -> u64 {
    0
}

pub(crate) fn on_battery() -> bool {
    false
}

pub(crate) fn keep_awake(_: &[Block]) {}

pub(crate) fn accent_color() -> String {
    String::new()
}

pub(crate) fn system_color(color: SystemColor) -> String {
    let text = color.name().ends_with("text");
    (if text { "#000000FF" } else { "#FFFFFFFF" }).to_owned()
}

pub(crate) fn beep() {
    eprint!("\x07");
}

/// No key store yet: `safeStorage` is unavailable.
pub(crate) fn dpapi_protect(_: &[u8]) -> Option<Vec<u8>> {
    None
}

pub(crate) fn dpapi_unprotect(_: &[u8]) -> Option<Vec<u8>> {
    None
}

/// Has the desktop open `target` with `open` (macOS) or `xdg-open`.
pub(crate) fn open_with_shell(target: &str) -> bool {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let Ok(mut child) = std::process::Command::new(opener).arg(target).spawn() else {
        return false;
    };
    // The opener is not waited for here; a thread reaps it so no zombie is left behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}

/// Opens the folder that holds `path`.
pub(crate) fn show_in_folder(path: &str) -> bool {
    let folder = std::path::Path::new(path).parent();
    folder.is_some_and(|folder| open_with_shell(&folder.to_string_lossy()))
}

pub(crate) fn move_to_trash(_: &str) -> bool {
    false
}

/// No system box yet: the line on standard error is all there is.
pub(crate) fn error_box(_: &str, _: &str) {}
