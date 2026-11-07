//! What the tests that run the `electron` binary share: a headless run of an app, a thread with the
//! stack Boa recurses in, the binary run on a fixture with a profile of its own and a watchdog, and
//! the frame stats it logs.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// Runs `main_js` as an app, with `files` (name and text) beside it, `args` before its path and
/// `env` set, with `--headless`, and answers what it did. Nothing of the environment of whoever
/// runs the test reaches the app: no system dialog and no logging setting.
pub fn run_headless(
    name: &str,
    main_js: &str,
    files: &[(&str, &str)],
    args: &[&str],
    env: &[(&str, &str)],
) -> Output {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the app");
    let main = dir.join("main.js");
    std::fs::write(&main, main_js).expect("the app's script");
    for (file, text) in files {
        std::fs::write(dir.join(file), text).expect("a file of the app");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_electron"))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .env_remove("ELECTRON_ENABLE_LOGGING")
        .env_remove("VERVE_ELECTRON_LOG")
        .env_remove("VERVE_ELECTRON_LOG_FILE")
        .env_remove("FORCE_COLOR")
        .env_remove("NO_COLOR")
        .envs(env.iter().copied())
        .arg("--headless")
        .args(args)
        .arg(&main)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .output()
        .expect("the electron binary runs");
    let _ = std::fs::remove_dir_all(&dir);
    output
}

/// Runs `f` on a thread with a stack Boa can recurse in.
pub fn on_thread<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    const STACK: usize = 64 << 20;
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(move || {
            verve::set_thread_stack(STACK);
            f()
        })
        .expect("a thread")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// The `electron` binary on the fixture's directory `name` (`tests/electron/apps/<name>`), with a
/// profile of its own. The binary ends itself; a watchdog kills this one process after `seconds`
/// if it does not.
pub fn run(name: &str, args: &[&str], env: &[(&str, &str)], seconds: u64) -> std::process::Output {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("verve-gpui-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a directory for the profile");
    let app = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/electron/apps")
        .join(name);
    let mut command = Command::new(env!("CARGO_BIN_EXE_electron"));
    command.args(args);
    command
        .arg(&app)
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .env("VERVE_NO_SYSTEM_DIALOGS", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let child = command.spawn().expect("the electron binary starts");
    let id = child.id();
    let (done, wait) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if wait.recv_timeout(Duration::from_secs(seconds)).is_err() {
            kill_by_pid(id);
        }
    });
    let output = child.wait_with_output().expect("the binary ends");
    let _ = done.send(());
    let _ = watchdog.join();
    let _ = std::fs::remove_dir_all(&dir);
    output
}

/// The 95th percentile of the time a frame took to build, from each `frames:` line the binary
/// logged under `VERVE_FRAME_STATS` (`frames: 240 built p50 0.31 ms p95 0.52 ms, apart ...`), with
/// the line.
pub fn frame_p95s(log: &str) -> Vec<(f32, String)> {
    log.lines()
        .filter(|line| line.starts_with("frames:"))
        .map(|line| {
            let built_p95: f32 = line
                .split("built p50")
                .nth(1)
                .and_then(|rest| rest.split("p95 ").nth(1))
                .and_then(|rest| rest.split(' ').next())
                .and_then(|number| number.parse().ok())
                .expect("a p95 in the line");
            (built_p95, line.to_owned())
        })
        .collect()
}

/// Stops the process `id`, which a test started, and no other.
pub fn kill_by_pid(id: u32) {
    #[cfg(windows)]
    let (program, args) = (
        "taskkill",
        vec!["/PID".to_owned(), id.to_string(), "/F".to_owned()],
    );
    #[cfg(not(windows))]
    let (program, args) = ("kill", vec!["-9".to_owned(), id.to_string()]);
    let _ = Command::new(program).args(args).output();
}
