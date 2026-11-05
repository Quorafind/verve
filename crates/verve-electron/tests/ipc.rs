//! `ipcMain`, `ipcRenderer`, `webContents.send` and `MessageChannelMain`, headless: a main
//! script and a page with `nodeIntegration` talk through the Hub, and the test drives each side
//! from outside (`eval_main`, `eval_renderer`).
//!
//! Every expectation named "Electron 39.8.10" is what `tests/electron/ipc/observe` printed under
//! that Electron on Windows (see its `main.js`). Where Verve differs on purpose the test says so.

mod ipc {
    pub mod clone;
    pub mod fixture;
    pub mod messages;
    pub mod ports;
}
mod support;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use support::{App, TIMEOUT};
use verve_electron::ids::ContentsId;

/// How a canonical path starts on Windows.
const VERBATIM: &str = r"\\?\";

/// The observation app's helpers, which describe a value by what it is.
fn observe(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/ipc/observe");
    let path = path
        .join(file)
        .canonicalize()
        .expect("the observation app is there");
    // Without the verbatim prefix Windows puts on a canonical path: `require` reads plain ones.
    let plain = path
        .to_string_lossy()
        .trim_start_matches(VERBATIM)
        .to_owned();
    serde_json::to_string(&plain).expect("a path is a string")
}

/// What a page has to start with: the helpers of the observation app, `ipc` for `ipcRenderer`,
/// and `both`, which says what a call answers, throws or rejects with.
const PAGE_SETUP: &str = r"
globalThis.shape = require(SHAPE).shape;
globalThis.samples = require(SAMPLES).samples;
globalThis.ipc = require('electron').ipcRenderer;
globalThis.both = async (fn) => {
  let out;
  try { out = fn(); } catch (e) { return { threw: [e.constructor.name, e.message] }; }
  try { return { resolved: await out }; } catch (e) { return { rejected: [e.constructor.name, e.message] }; }
};
1";

/// An app with one window, never shown, whose page has `nodeIntegration`.
pub struct Ipc {
    pub app: App,
    pub page: ContentsId,
}

impl Ipc {
    /// The app, with `extra` run in the main process after the window is made. Main-process
    /// globals: `electron`, `win`, `shape` (of the observation app's `shape.js`), `samples`.
    pub fn new(extra: &str) -> Self {
        let main = format!(
            r#"
const electron = require('electron');
globalThis.electron = electron;
globalThis.shape = require({shape}).shape;
globalThis.samples = require({samples}).samples;
globalThis.win = new electron.BrowserWindow({{ show: false, webPreferences: {{ nodeIntegration: true, contextIsolation: false }} }});
win.loadURL('data:text/html,<p>page</p>');
globalThis.attempt = (fn) => {{
  try {{ const r = fn(); return {{ returned: r === undefined ? '#undefined' : r }}; }}
  catch (e) {{ return {{ threw: [e.constructor.name, e.message] }}; }}
}};
{extra}
"#,
            shape = observe("shape.js"),
            samples = observe("samples.js"),
        );
        let app = App::launch_script(&main, &[]);
        assert!(app.run_until(|app| !app.shell.windows().is_empty(), TIMEOUT));
        let page = app.shell.windows()[0].contents;
        // The page's helpers, and its first turn done.
        let ipc = Self { app, page };
        let setup = PAGE_SETUP
            .replace("SHAPE", &observe("shape.js"))
            .replace("SAMPLES", &observe("samples.js"));
        ipc.page(&setup);
        ipc
    }

    /// `js` evaluated in the main process, as JSON.
    pub fn main(&self, js: &str) -> Value {
        json(&self.app.eval_main(js))
    }

    /// `js` evaluated in the page, as JSON.
    pub fn page(&self, js: &str) -> Value {
        json(&self.app.eval_renderer(self.page, js))
    }

    /// The body of an async function run in the page, answering what it returns (`{ "threw":
    /// message }` if it threw).
    pub fn run(&self, body: &str) -> Value {
        let out = self.app.run_page(self.page, body, TIMEOUT);
        out.get("value").cloned().unwrap_or(out)
    }

    /// Waits until `condition` holds in the main process.
    pub fn until(&self, condition: &str) {
        assert!(
            self.app.run_until(
                |_| self.main(condition) == Value::Bool(true),
                Duration::from_secs(10)
            ),
            "{condition} never held; console: {:?}",
            self.app.console()
        );
    }
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// `[a, b, ...]` as JSON values.
pub fn list<const N: usize>(items: [Value; N]) -> Value {
    Value::Array(items.into())
}
