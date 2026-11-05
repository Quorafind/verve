//! `electron [--headless] [app-directory | main.js]`: the app's processes on Verve, in one process
//! of their own. `--headless` runs them on a shell with no display that prints what the windows do.
//!
//! What an app directory means — its `package.json`'s `main`, the flags Electron accepts, single
//! instances — is P10's; P02 takes the directory's `main.js` or `index.js`, or the file named.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use verve_electron::{ElectronOptions, Hub};

const USAGE: &str = "usage: electron [--headless] [app-directory | main.js]";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // P10: `--headless` becomes a field of its `Launch`.
    let headless = args.first().is_some_and(|arg| arg == "--headless");
    if headless {
        args.remove(0);
    }
    let (app_path, entry) = match app(&args) {
        Ok(app) => app,
        Err(message) => {
            eprintln!("electron: {message}\n{USAGE}");
            return ExitCode::from(9);
        }
    };
    let hub = Hub::new(ElectronOptions {
        app_path,
        dark: false,
    });
    let code = if headless {
        verve_electron::headless::run(&hub, entry)
    } else {
        verve_gpui::run(hub, entry)
    };
    // The process ends with the app's code, whole: Windows takes 32 bits, and GPUI's own quit
    // would have ended a macOS process with 0.
    let _ = (std::io::stdout().flush(), std::io::stderr().flush());
    std::process::exit(code)
}

/// The app's directory, and the script that runs. A directory is the app; a file is the script,
/// with the directory it is in for the app's.
fn app(args: &[String]) -> Result<(PathBuf, PathBuf), String> {
    let Some(path) = args.first().map(PathBuf::from) else {
        return Err("no app given".into());
    };
    let path =
        std::path::absolute(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if path.is_dir() {
        let entry = ["main.js", "index.js"]
            .iter()
            .map(|name| path.join(name))
            .find(|entry| entry.is_file())
            .ok_or_else(|| format!("{} has no main.js or index.js", path.display()))?;
        return Ok((path, entry));
    }
    let app_path = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok((app_path, path))
}
