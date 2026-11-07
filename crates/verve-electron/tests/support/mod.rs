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

use serde_json::Value;
use verve_electron::ids::{ContentsId, WindowId};
use verve_electron::launcher::{self, Env, Launch, Start, Target};
use verve_electron::log::Log;
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
    /// The profile directory (`userData`) of the app, removed with it.
    profile: PathBuf,
}

impl App {
    /// Runs the app in `app_dir`, as `electron <app_dir>` does.
    pub fn launch(app_dir: &Path) -> App {
        let args = vec![app_dir.as_os_str().to_owned()];
        match launcher::parse(args, &launcher::exe_path(), &Env::default()) {
            Ok(Launch::Start(start)) => Self::launch_with(*start, None),
            other => panic!("{} is not an app: {other:?}", app_dir.display()),
        }
    }

    /// Runs a development app as `start` says it was started: its switches and command line are
    /// the app's. `scratch` is a directory to remove when the app is dropped.
    pub fn launch_with(start: Start, scratch: Option<PathBuf>) -> App {
        let Target::App { app_path, entry } = &start.target else {
            panic!("launch_with runs an app from a folder or a script");
        };
        let (app_path, entry) = (app_path.clone(), entry.clone());
        Self::start(&app_path, entry, scratch, false, Some(start), None)
    }

    /// A directory of its own for a test to use, made.
    pub fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "verve-electron-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory is made");
        dir
    }

    /// Writes `main.js` and `files` (paths relative to it) into `dir`, which may not exist yet.
    fn write_app(dir: &Path, main_js: &str, files: &[(&str, &str)]) {
        for (name, text) in [("main.js", main_js)]
            .into_iter()
            .chain(files.iter().copied())
        {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().expect("a file is in a directory"))
                .expect("the app's directory is made");
            std::fs::write(path, text).expect("the app's file is written");
        }
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
        Self::write_app(&dir, main_js, files);
        Self::start(
            &dir,
            dir.join("main.js"),
            Some(dir.clone()),
            dark,
            None,
            None,
        )
    }

    /// [`App::launch_script`] writing its terminal output to `log`, which the test reads.
    pub fn launch_script_logged(main_js: &str, files: &[(&str, &str)], log: Arc<Log>) -> App {
        let dir = Self::scratch_dir("logged");
        Self::write_app(&dir, main_js, files);
        Self::start(
            &dir,
            dir.join("main.js"),
            Some(dir.clone()),
            false,
            None,
            Some(log),
        )
    }

    fn start(
        app_dir: &Path,
        entry: PathBuf,
        scratch: Option<PathBuf>,
        dark: bool,
        launch: Option<Start>,
        log: Option<Arc<Log>>,
    ) -> App {
        let hub = Hub::new(ElectronOptions {
            app_path: app_dir.to_path_buf(),
            dark,
        });
        if let Some(log) = log {
            hub.set_log(log);
        }
        // Every app of a test has a profile directory of its own in the temporary one — whichever
        // way it was launched — and it is removed with the app. An app that took the user's
        // (`%APPDATA%\<name>`) would leave folders there, take the lock of a real instance, and
        // fail this assertion.
        let mut launch = launch.unwrap_or_else(|| Start::development(app_dir.to_path_buf()));
        let profile = launch.user_data_dir().unwrap_or_else(|| {
            let dir = App::scratch_dir("profile");
            let switch = ("user-data-dir".to_owned(), dir.display().to_string());
            launch.switches.push(switch);
            dir
        });
        assert!(
            profile.starts_with(std::env::temp_dir()),
            "{} is not in the temporary directory: a test must not use a real profile",
            profile.display()
        );
        hub.set_launch(launch);
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
            profile,
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

    /// Runs `body`, the body of an async function, in the page `contents` and waits up to
    /// `timeout` for it to finish: `{ "value": v }` for what it returned, `{ "threw": message }`
    /// for what it threw.
    pub fn run_page(&self, contents: ContentsId, body: &str, timeout: Duration) -> Value {
        self.eval_renderer(
            contents,
            &format!(
                "globalThis.out = undefined; (async () => {{ {body} }})().then( value => {{ globalThis.out = {{ value }}; }}, error => {{ globalThis.out = {{ threw: String(error && error.message) }}; }}); 1"
            ),
        );
        let deadline = Instant::now() + timeout;
        loop {
            let out = serde_json::from_str(&self.eval_renderer(contents, "globalThis.out"))
                .unwrap_or(Value::Null);
            if !out.is_null() {
                return out;
            }
            assert!(
                Instant::now() < deadline,
                "the page's script did not finish; console: {:?}",
                self.console()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
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
        // The directories may not exist: an app that never got ready made no profile.
        let _ = std::fs::remove_dir_all(&self.profile);
        if let Some(dir) = &self.scratch {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ---------- The fixture apps, and what real Electron printed for them ----------

pub fn fixture(name: &str) -> PathBuf {
    let apps = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps");
    apps.join(name)
}

/// The console lines of real Electron, `(from, text)`, without the development-mode security
/// warning every page without a content security policy gets.
pub fn recorded_console(name: &str) -> Vec<(String, String)> {
    let text = std::fs::read_to_string(fixture(name).join("expected.json")).expect("a recording");
    let recording: Value = serde_json::from_str(&text).expect("expected.json is JSON");
    let lines = recording["console"].as_array().expect("a console list");
    let text = |line: &Value, key: &str| line[key].as_str().expect("a string").to_owned();
    let lines = lines
        .iter()
        .map(|line| (text(line, "from"), text(line, "text")));
    lines
        .filter(|(_, text)| !text.contains("Electron Security Warning"))
        .collect()
}

pub fn console_of(app: &App) -> Vec<(String, String)> {
    let lines = app.hub.console_lines();
    let from = |contents: Option<u32>| {
        if contents.is_none() {
            "main"
        } else {
            "renderer"
        }
    };
    lines
        .into_iter()
        .map(|l| (from(l.contents).to_owned(), l.text))
        .collect()
}

/// What one process wrote, in order. Real Electron's two processes run at the same time, so the
/// order of a line of the main process against one of a page is not part of what it recorded.
pub fn written_by(lines: &[(String, String)], from: &str) -> Vec<String> {
    lines
        .iter()
        .filter(|(who, _)| who == from)
        .map(|(_, text)| text.clone())
        .collect()
}

/// Runs `body` — the body of an async function in the main process, with `app`, `BrowserWindow`,
/// `sleep`, `log`, `load(win, html)` and `watch(tag, emitter, names)` at hand — and answers what
/// the app logged, up to the `DONE` it logs when `body` has finished. A throw is logged as
/// `THREW <message>`. `files` are written beside the app's script (`win.loadFile('a.html')`); the
/// body has `timeout` to finish.
pub fn run_sourced(body: &str, files: &[(&str, &str)], timeout: Duration) -> Vec<(String, String)> {
    let script = format!(
        r#"const {{ app, BrowserWindow }} = require('electron');
app.on('window-all-closed', () => {{}});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const log = (...parts) => console.log(parts.map((p) => (typeof p === 'string' ? p : p === undefined ? 'undefined' : JSON.stringify(p))).join(' '));
const load = (win, html) => new Promise((resolve) => {{
  win.webContents.once('did-finish-load', resolve);
  win.loadURL('data:text/html,' + encodeURIComponent(html));
}});
const watch = (tag, emitter, names) => names.forEach((n) => emitter.on(n, () => log(tag, n)));
app.whenReady().then(async () => {{
  try {{
{body}
  }} catch (error) {{ log('THREW', error.message); }}
  log('DONE');
}});"#
    );
    let app = App::launch_script(&script, files);
    assert!(
        app.run_until(
            |app| app.console().iter().any(|line| line == "DONE"),
            timeout
        ),
        "the script did not finish: {:?}",
        app.console()
    );
    let mut lines = console_of(&app);
    let done = lines
        .iter()
        .position(|(who, line)| who == "main" && line == "DONE");
    lines.truncate(done.unwrap_or(lines.len()));
    lines
}

pub fn run_within(body: &str, files: &[(&str, &str)], timeout: Duration) -> Vec<String> {
    let lines = run_sourced(body, files, timeout);
    lines.into_iter().map(|(_, text)| text).collect()
}

/// [`run_within`] for the usual wait.
pub fn run_with(body: &str, files: &[(&str, &str)]) -> Vec<String> {
    run_within(body, files, TIMEOUT)
}

/// [`run_with`] for a test that needs no files.
pub fn run(body: &str) -> Vec<String> {
    run_with(body, &[])
}

/// What the main process logged and nothing else, for a test that must not hear a page's lines
/// (an uncaught error of the page) as the main process's.
pub fn run_main(body: &str) -> Vec<String> {
    let lines = run_sourced(body, &[], TIMEOUT);
    written_by(&lines, "main")
}
