//! What the tests that run the `electron` binary share.

use std::path::PathBuf;
use std::process::{Command, Output};

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
