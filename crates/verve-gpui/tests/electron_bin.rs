//! The `electron` binary, run as a person runs it: a real GPUI window opens, and the app ends the
//! way its script says. Only where there is a window system to open one on — a desktop session
//! on Windows or macOS; a Linux CI machine has none, and its style job only type-checks this.
#![cfg(any(windows, target_os = "macos"))]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs `main_js` as an app and answers the code it exits with, or `None` if it had to be killed.
fn run_app(name: &str, main_js: &str) -> Option<i32> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the app");
    let main = dir.join("main.js");
    std::fs::write(&main, main_js).expect("the app's script");
    let mut child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg(&main)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the electron binary starts");
    let deadline = Instant::now() + Duration::from_secs(60);
    let code = loop {
        match child.try_wait().expect("the child can be asked") {
            Some(status) => break status.code(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    code
}

/// GPUI's own quit mode ends an app when its last window closes. This app's `'closed'` listener
/// opens another window and the script exits with 5 a moment later: closing the last window is
/// not the end of the app, and the app's own code is the process's.
#[test]
fn closing_the_last_window_does_not_end_the_app_and_its_exit_code_is_the_processs() {
    let code = run_app(
        "last-window",
        r#"
        const { BrowserWindow } = require('electron');
        const first = new BrowserWindow({ title: 'first' });
        first.on('closed', () => {
            new BrowserWindow({ title: 'second' });
            setTimeout(() => process.exit(5), 700);
        });
        setTimeout(() => first.close(), 700);
        "#,
    );
    assert_eq!(code, Some(5));
}

#[test]
fn an_app_with_no_window_ends_with_zero_and_one_that_throws_with_one() {
    assert_eq!(run_app("no-window", "require('electron');"), Some(0));
    assert_eq!(run_app("throws", "throw new Error('no');"), Some(1));
}
