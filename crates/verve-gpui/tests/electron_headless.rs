//! `electron --headless`: the same binary on a shell with no display, which opens no window and
//! prints what the app's windows would do. It runs anywhere, a display or not.

use std::path::PathBuf;
use std::process::Command;

/// Runs `main_js` as an app with `--headless` and answers its exit code and standard error.
fn run_headless(name: &str, main_js: &str) -> (Option<i32>, String) {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-headless-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the app");
    let main = dir.join("main.js");
    std::fs::write(&main, main_js).expect("the app's script");
    let output = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg("--headless")
        .arg(&main)
        .output()
        .expect("the electron binary runs");
    let _ = std::fs::remove_dir_all(&dir);
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn the_windows_of_an_app_are_printed_and_its_exit_code_is_the_processs() {
    let (code, stderr) = run_headless(
        "windows",
        r#"
        const { BrowserWindow } = require('electron');
        const win = new BrowserWindow({ width: 640, height: 480, title: 'traced' });
        win.on('closed', () => process.exit(6));
        setTimeout(() => win.close(), 50);
        "#,
    );
    assert_eq!(code, Some(6));
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines[0], r#"[window 1] create 640x480 "traced""#,
        "{stderr}"
    );
    assert!(lines.contains(&"[window 1] Close"), "{stderr}");
    assert_eq!(lines.last(), Some(&"[app] quit 6"), "{stderr}");
}
