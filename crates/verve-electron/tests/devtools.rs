//! What replaces DevTools (P43): a page's console as `'console-message'` and on the terminal, the
//! errors a page does not catch, and `openDevTools()` as a text dump.
//!
//! The signatures and messages are what Electron 39.8.10 (Windows) did with the same page, run by
//! `tests/electron/devtools/console/observe` and `devtools-off/observe` and recorded in
//! `tests/electron/devtools/expected/`;
//! where Verve differs, the test says so.
#![allow(clippy::unwrap_used)]

mod support;

use std::sync::Arc;

use support::{App, TIMEOUT};
use verve_electron::log::{Filter, Log};

/// A page that writes one line at each level, the first on line 2 as the recording's page does.
const CONSOLE_PAGE: &str = "<script>
console.log('first');
console.info('second');
console.warn('x');
console.error('bad');
console.debug('quiet');
</script>";

/// A page whose script throws, and rejects twice, after it has loaded.
const ERROR_PAGE: &str = "<script>
setTimeout(() => { throw new Error('page boom'); }, 10);
setTimeout(() => { Promise.reject(new Error('page reject')); }, 30);
setTimeout(() => { Promise.reject('plain reason'); }, 50);
</script>";

/// The main process of an app with one hidden window on `page.html`, which hears `console-message`
/// as `listener` says, and waits for the page to have run.
fn app_script(listener: &str) -> String {
    format!(
        r#"const {{ app, BrowserWindow }} = require('electron');
app.on('window-all-closed', () => {{}});
const log = (...parts) => console.log(parts.join(' '));
app.whenReady().then(async () => {{
  const win = new BrowserWindow({{ show: false }});
  const wc = win.webContents;
  wc.on('console-message', {listener});
  await win.loadFile('page.html');
  await new Promise((resolve) => setTimeout(resolve, 400));
  log('DONE');
}});"#
    )
}

/// Runs the app of [`app_script`] on `page` with a terminal of its own, and answers the main
/// process's console lines up to the `DONE` that ends it.
fn run_app(page: &str, listener: &str, log: Option<&Arc<Log>>) -> Vec<String> {
    let script = app_script(listener);
    let files = [("page.html", page)];
    let app = match log {
        Some(log) => App::launch_script_logged(&script, &files, Arc::clone(log)),
        None => App::launch_script(&script, &files),
    };
    assert!(
        app.run_until(|app| app.console().iter().any(|l| l == "DONE"), TIMEOUT),
        "{:?}",
        app.console()
    );
    app.console()
}

/// What the listener logged with a `msg ` first, without it.
fn heard(page: &str, listener: &str) -> Vec<String> {
    let lines = run_app(page, listener, None);
    let heard = lines.iter().filter_map(|line| line.strip_prefix("msg "));
    heard.map(str::to_owned).collect()
}

#[test]
fn a_console_line_is_heard_as_the_event_object_and_as_the_deprecated_arguments() {
    // Electron 39.8.10: `level` is 'info', 'warning', 'error' or 'debug' on the event and the
    // numbers 1, 2, 3 and 0 in the positional form; the event's own keys are exactly the six
    // below; `frame` is `webContents.mainFrame`; `lineNumber` and `sourceId` are where the page
    // wrote the line.
    let lines = heard(
        CONSOLE_PAGE,
        "(event, level, message, line, source) => log('msg', event.level, event._level, \
         JSON.stringify(event.message), event.lineNumber, event.sourceId.split('/').pop(), \
         event.frame === wc.mainFrame, Object.keys(event).sort().join(), '|', level, message, \
         line, source.split('/').pop())",
    );
    let keys = "_level,frame,level,lineNumber,message,sourceId";
    let line = |level: &str, number: u8, text: &str, at: u32| {
        format!(
            "{level} {number} \"{text}\" {at} page.html true {keys} | {number} {text} {at} page.html"
        )
    };
    assert_eq!(
        lines,
        [
            line("info", 1, "first", 2),
            line("info", 1, "second", 3),
            line("warning", 2, "x", 4),
            line("error", 3, "bad", 5),
            line("debug", 0, "quiet", 6),
        ]
    );
}

/// How many times the main process's console says the positional arguments are deprecated, for an
/// app whose listener is `listener`.
fn deprecation_warnings(listener: &str) -> usize {
    let lines = run_app(CONSOLE_PAGE, listener, None);
    let said = |line: &&String| line.contains("'console-message' arguments are deprecated");
    lines.iter().filter(said).count()
}

#[test]
fn a_listener_that_takes_the_arguments_is_told_once_they_are_deprecated() {
    // Electron 39.8.10 writes `(electron) 'console-message' arguments are deprecated and will be
    // removed. Please use Event<WebContentsConsoleMessageEventParams> object instead.` the first
    // time, when a listener declares more than one parameter; a listener of the event alone
    // hears nothing of it.
    assert_eq!(deprecation_warnings("(event, level) => {}"), 1);
    assert_eq!(deprecation_warnings("(event) => {}"), 0);
}

#[test]
fn an_error_the_page_does_not_catch_is_a_console_message_at_the_line_it_was_thrown() {
    // Electron 39.8.10: `Uncaught Error: page boom`, and for a rejection `Uncaught (in promise)
    // Error: page reject`, both level 'error' with the position of the `new Error`; the page goes
    // on. A rejection with a string says `Uncaught (in promise) plain reason`; Electron gives it
    // the line of the rejection, which Verve does not know.
    let lines = heard(
        ERROR_PAGE,
        "(event) => log('msg', event.level, JSON.stringify(event.message), event.lineNumber, \
         event.sourceId.split('/').pop())",
    );
    assert_eq!(
        &lines[..2],
        [
            "error \"Uncaught Error: page boom\" 2 page.html",
            "error \"Uncaught (in promise) Error: page reject\" 3 page.html",
        ]
    );
    let last = "error \"Uncaught (in promise) plain reason\"";
    assert!(lines[2].starts_with(last), "{lines:?}");
    assert_eq!(lines.len(), 3);
}

/// Runs the console page in an app whose terminal is `log`.
fn run_console_page(log: &Arc<Log>) {
    run_app(CONSOLE_PAGE, "() => {}", Some(log));
}

#[test]
fn a_pages_console_is_not_on_the_terminal_unless_logging_is_on() {
    // Electron 39.8.10 writes nothing of a page's console to stderr or stdout by default, and with
    // `--enable-logging` or `ELECTRON_ENABLE_LOGGING=1` a line of Chromium's log for each. Verve
    // writes `[renderer:<id> <file>:<line>] <message>`.
    let quiet = Arc::new(Log::to_memory(Filter::default()));
    run_console_page(&quiet);
    assert_eq!(quiet.written(), "");

    let logging = Arc::new(Log::to_memory(Filter::default()));
    logging.set_console(true);
    run_console_page(&logging);
    let expected = ["first", "second", "x", "bad", "quiet"]
        .iter()
        .zip(2..)
        .map(|(text, at)| format!("[renderer:1 page.html:{at}] {text}\n"))
        .collect::<String>();
    assert_eq!(logging.written(), expected);
}

#[test]
fn the_console_category_of_verve_electron_log_writes_the_consoles_too() {
    let log = Arc::new(Log::to_memory(Filter::parse("console")));
    run_console_page(&log);
    assert!(
        log.written()
            .starts_with("[renderer:1 page.html:2] first\n")
    );
}

/// An app whose page sends `ping` to the main process, which sends `hello` back.
const IPC_APP: &str = r"const { app, BrowserWindow, ipcMain } = require('electron');
app.on('window-all-closed', () => {});
ipcMain.on('ping', () => {});
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, webPreferences: { nodeIntegration: true, contextIsolation: false } });
  await win.loadURL('data:text/html,<p>x</p>');
  await win.webContents.executeJavaScript(`require('electron').ipcRenderer.send('ping', 1)`);
  win.webContents.send('hello', 1, 2);
  await new Promise((resolve) => setTimeout(resolve, 200));
  console.log('DONE');
});";

/// What an app that sends one message each way wrote on the terminal under `filter`.
fn ipc_diagnostics(filter: &str) -> String {
    let log = Arc::new(Log::to_memory(Filter::parse(filter)));
    let app = App::launch_script_logged(IPC_APP, &[], Arc::clone(&log));
    assert!(app.run_until(|app| app.console().iter().any(|l| l == "DONE"), TIMEOUT));
    log.written()
}

#[test]
fn the_ipc_category_writes_a_line_for_each_message_and_nothing_when_it_is_off() {
    assert_eq!(
        ipc_diagnostics("ipc"),
        "[verve:ipc] renderer 1 -> main send \"ping\" (1 args)\n\
         [verve:ipc] main -> renderer 1 send \"hello\" (2 args)\n"
    );
    assert_eq!(ipc_diagnostics(""), "");
    let loads = ipc_diagnostics("load");
    assert!(
        loads.starts_with("[verve:load] renderer 1 loads data:text/html"),
        "{loads}"
    );
    assert!(!loads.contains("[verve:ipc]"), "{loads}");
}
