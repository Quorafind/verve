//! The inspector window of P43's second phase, on a real display: `openDevTools()` opens a GPUI
//! window and `closeDevTools()` closes it. That a window really opened and closed is read from the
//! lines the shell writes in the `shell` category of the log (`VERVE_ELECTRON_LOG=shell`), which
//! only the window code writes; the events the main process says are the ones the headless shell
//! is held to in `crates/verve-electron/tests/inspector.rs`, and it says them whether a window
//! opens or not.
//!
//! The `electron` binary runs the app as a child process, as the product does, and the test reads
//! its exit status, its standard output and the shell log. (Running the app in the test's own
//! process, with the Hub's threads still alive when the test ended, crashed the process after the
//! test body with `0xc0000005`.)
//!
//! It opens real windows on the screen of whoever runs it, for a few seconds, so it is
//! `#[ignore]`d; run it with
//! `cargo test -p verve-gpui --features jit --test inspector_window -- --ignored --test-threads=1`.
//! What a person checks by hand (the tree, the highlight, the picker, the REPL) is the list in
//! `docs/electron/packages/P43-devtools.md`.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

mod support;

/// How long the run may take before the test ends the child, windows and all.
const LIMIT: u64 = 60;

const PAGE: &str = "<title>Inspected</title><body><div id=box class=wide style='margin:10px;padding:6px;border:2px solid #36c'>hello</div><script>console.log('from the page')</script>";

const APP: &str = r"const { app, BrowserWindow } = require('electron');
app.on('window-all-closed', () => {});
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, width: 640, height: 480 });
  const wc = win.webContents;
  wc.on('devtools-opened', () => console.log('event opened', wc.isDevToolsOpened()));
  wc.on('devtools-focused', () => console.log('event focused', wc.isDevToolsFocused()));
  wc.on('devtools-closed', () => console.log('event closed', wc.isDevToolsOpened()));
  await win.loadFile('page.html');
  wc.openDevTools();
  console.log('asked to open');
  setTimeout(() => { wc.closeDevTools(); console.log('asked to close'); }, 3000);
  setTimeout(() => { console.log('DONE'); app.quit(); }, 4500);
});";

#[test]
#[ignore = "opens real windows"]
fn the_inspector_window_opens_and_closes_with_the_devtools() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-inspector-window-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the app");
    let log_file = dir.join("shell.log");
    let main = dir.join("main.js");
    std::fs::write(&main, APP).expect("the app's script");
    std::fs::write(dir.join("page.html"), PAGE).expect("the page");
    let child = Command::new(env!("CARGO_BIN_EXE_electron"))
        .arg(&main)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .env("VERVE_ELECTRON_LOG", "shell")
        .env("VERVE_ELECTRON_LOG_FILE", &log_file)
        .env_remove("ELECTRON_ENABLE_LOGGING")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the electron binary starts");
    let id = child.id();
    let (done, wait) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(LIMIT)).is_err() {
            support::kill_by_pid(id);
        }
    });
    let started = std::time::Instant::now();
    let output = child.wait_with_output().expect("the binary ends");
    let _ = done.send(());
    let _ = watchdog.join();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let shell_log = std::fs::read_to_string(&log_file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        output.status.success(),
        "{:?}
{stdout}
{stderr}",
        output.status
    );
    assert!(started.elapsed() > Duration::from_secs(3), "{stdout}");
    let has = |text: &str| lines.contains(&text);
    assert!(has("event opened true"), "{stdout}");
    assert!(has("event closed false"), "{stdout}");
    assert!(has("DONE"), "{stdout}");
    // Only the shell's window code writes these: a window was made, and closed when asked, and
    // GPUI did not refuse it.
    let shell_says = |verb: &str| {
        let window = |line: &str| line.contains("inspector window of page ");
        shell_log
            .lines()
            .any(|line| window(line) && line.ends_with(verb))
    };
    assert!(shell_says(" opened"), "{shell_log}");
    assert!(shell_says(" closed"), "{shell_log}");
    assert!(
        !shell_log.contains("no inspector window for page"),
        "{shell_log}"
    );
    // Whether the window got the focus depends on the desktop (Windows may keep a background
    // process from taking it), so the `event focused` line is read by whoever runs this, not held to.
    eprintln!(
        "{stdout}
{shell_log}"
    );
}
