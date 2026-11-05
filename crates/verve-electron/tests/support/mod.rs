//! What every end-to-end test of an Electron app shares: the app running on a headless shell, the
//! questions a test asks of it, and a wait for the answer. No display, no GPU, no GPUI.
//!
//! ```ignore
//! let app = App::launch_script("new (require('electron').BrowserWindow)()", &[]);
//! assert!(app.run_until(|app| !app.shell.windows().is_empty(), TIMEOUT));
//! ```
#![allow(dead_code)] // Not every test file uses every helper.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use verve_electron::ids::{ContentsId, WindowId};
use verve_electron::{ElectronOptions, HeadlessOptions, HeadlessShell, HeadlessWindow, Hub};

/// How long a test waits for something that has to happen.
pub const TIMEOUT: Duration = Duration::from_secs(10);

static NEXT: AtomicU32 = AtomicU32::new(1);

/// An app running headless: the Hub its threads share, the shell that answers for the screen,
/// and the directory it was written to if it was made by [`App::launch_script`].
pub struct App {
    pub hub: Arc<Hub>,
    pub shell: Arc<HeadlessShell>,
    scratch: Option<PathBuf>,
}

impl App {
    /// Runs the app in `app_dir`, as `electron <app_dir>` does.
    // P10: this takes the entry from `launcher::parse` once that merges.
    pub fn launch(app_dir: &Path) -> App {
        Self::start(app_dir, entry_of(app_dir), None, false)
    }

    /// Writes `main.js` and `files` (paths relative to it) into a directory of their own and
    /// runs that.
    pub fn launch_script(main_js: &str, files: &[(&str, &str)]) -> App {
        Self::launch_script_in(main_js, files, false)
    }

    /// [`App::launch_script`] in a system theme that is dark or not.
    pub fn launch_script_in(main_js: &str, files: &[(&str, &str)], dark: bool) -> App {
        let dir = std::env::temp_dir().join(format!(
            "verve-electron-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for (name, text) in [("main.js", main_js)]
            .into_iter()
            .chain(files.iter().copied())
        {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().expect("a file is in a directory"))
                .expect("the app's directory is made");
            std::fs::write(path, text).expect("the app's file is written");
        }
        Self::start(&dir, dir.join("main.js"), Some(dir.clone()), dark)
    }

    fn start(app_dir: &Path, entry: PathBuf, scratch: Option<PathBuf>, dark: bool) -> App {
        let hub = Hub::new(ElectronOptions {
            app_path: app_dir.to_path_buf(),
            dark,
        });
        let options = HeadlessOptions {
            dark,
            ..HeadlessOptions::default()
        };
        let shell = HeadlessShell::new(&hub, options);
        hub.start_main(entry);
        App {
            hub,
            shell,
            scratch,
        }
    }

    /// Waits until `cond` holds, polling; `false` if `timeout` passes first.
    pub fn run_until(&self, cond: impl Fn(&App) -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cond(self)
    }

    /// The window titled `title`, once the shell has made it.
    pub fn window(&self, title: &str) -> Option<HeadlessWindow> {
        let find = |app: &App| app.shell.windows().into_iter().find(|w| w.title == title);
        self.run_until(|app| find(app).is_some(), TIMEOUT);
        find(self)
    }

    /// The text of `window`'s page body as the shell last saw it.
    pub fn page_text(&self, window: WindowId) -> Option<String> {
        let snapshot = self.shell.snapshot(window)?;
        Some(snapshot.dom.text_content(snapshot.dom.body))
    }

    /// Evaluates `js` in the main process's global scope and answers the result as JSON: what a
    /// test wants to read, the app has set on `globalThis`. A throw fails the test.
    pub fn eval_main(&self, js: &str) -> String {
        let (answer, wait) = mpsc::channel();
        let code = js.to_owned();
        self.hub.to_main(move |guest| {
            let _ = answer.send(guest.eval(&code));
        });
        self.answered(wait, js)
    }

    /// Evaluates `js` in the page `contents` and answers the result as JSON.
    pub fn eval_renderer(&self, contents: ContentsId, js: &str) -> String {
        let (answer, wait) = mpsc::channel();
        let code = js.to_owned();
        self.hub.on_renderer(contents, move |guest| {
            let _ = answer.send(guest.eval(&code));
        });
        self.answered(wait, js)
    }

    fn answered(&self, wait: mpsc::Receiver<Result<String, String>>, js: &str) -> String {
        match wait.recv_timeout(TIMEOUT) {
            Ok(Ok(json)) => json,
            Ok(Err(error)) => panic!("`{js}` threw: {error}"),
            Err(_) => panic!("`{js}` was not answered; console: {:?}", self.console()),
        }
    }

    /// Every console line of the main process and the pages, in the order they were written.
    pub fn console(&self) -> Vec<String> {
        let lines = self.hub.console_lines();
        lines.into_iter().map(|line| line.text).collect()
    }

    /// The code the app ended with, if it does within `timeout`.
    pub fn exit_code(&self, timeout: Duration) -> Option<i32> {
        self.run_until(|app| app.shell.exit_code().is_some(), timeout);
        self.shell.exit_code()
    }

    /// Waits for the app to end and answers the code it ended with.
    pub fn wait_exit(self, timeout: Duration) -> i32 {
        self.exit_code(timeout)
            .unwrap_or_else(|| panic!("the app did not end; console: {:?}", self.console()))
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.hub.shutdown(0);
        if let Some(dir) = &self.scratch {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The script `electron <dir>` runs: `main.js` or else `index.js`.
fn entry_of(dir: &Path) -> PathBuf {
    ["main.js", "index.js"]
        .iter()
        .map(|name| dir.join(name))
        .find(|entry| entry.is_file())
        .unwrap_or_else(|| panic!("{} has no main.js or index.js", dir.display()))
}
