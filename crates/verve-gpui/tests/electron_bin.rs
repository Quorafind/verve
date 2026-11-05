//! The `electron` binary, run as a person runs it: a real GPUI window opens, and the app ends the
//! way its script says. Only where there is a window system to open one on — a desktop session
//! on Windows or macOS; a Linux CI machine has none, and its style job only type-checks this.
#![cfg(any(windows, target_os = "macos"))]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Runs `main_js` as an app, with a profile directory of its own, and answers the code it exits
/// with, or `None` if it was still running after `limit` and had to be killed.
fn run_app_for(name: &str, main_js: &str, limit: Duration) -> Option<i32> {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the app");
    let main = dir.join("main.js");
    std::fs::write(&main, main_js).expect("the app's script");
    let mut child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg(&main)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the electron binary starts");
    let deadline = Instant::now() + limit;
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

fn run_app(name: &str, main_js: &str) -> Option<i32> {
    run_app_for(name, main_js, Duration::from_secs(60))
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

/// Electron 39.8.10 keeps an app with no window and nothing to run up, waiting for events (P02
/// ended it with 0); `app.quit()` is what ends it, and a script that cannot be found ends it with 1.
#[test]
fn an_app_with_no_window_waits_until_it_quits_and_one_that_is_missing_ends_with_one() {
    let idle = run_app_for("idle", "require('electron');", Duration::from_secs(3));
    assert_eq!(idle, None, "an idle main process is still running");
    let quits = run_app(
        "quits",
        "const { app } = require('electron'); app.whenReady().then(() => app.quit());",
    );
    assert_eq!(quits, Some(0));
    assert_eq!(run_app("missing", "require('./not-there');"), Some(1));
}
