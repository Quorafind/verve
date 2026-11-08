//! The `electron` binary on the `menu-tray` fixture, with the system's menus, tray and hotkeys on:
//! a menu bar on a real window, a tray icon in the notification area and `Ctrl+Shift+X` taken from
//! the keyboard, for half a second. It touches the desktop, so it is ignored and nobody runs it
//! automatically; the checklist for trying it by hand is in the Status of
//! `docs/electron/packages/P15-menu-tray-shortcut.md`.
#![cfg(any(windows, target_os = "macos"))]

use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
#[ignore = "touches the desktop"]
fn the_menu_tray_fixture_puts_a_menu_bar_a_tray_icon_and_a_hotkey_on_the_desktop() {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps/menu-tray");
    let profile = std::env::temp_dir().join(format!("verve-gpui-menu-tray-{}", std::process::id()));
    let status = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg(&fixture)
        .arg(format!("--user-data-dir={}", profile.display()))
        .env_remove("VERVE_NO_SYSTEM_DIALOGS")
        .env_remove("MENU_TRAY_STAY")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("the electron binary starts");
    let _ = std::fs::remove_dir_all(&profile);
    assert_eq!(status.code(), Some(0));
}
