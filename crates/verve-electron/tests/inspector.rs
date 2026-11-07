//! The inspector window's side of P43's second phase, without a window: the REPL's path from the
//! shell to the page and back, the events the window raises, and the main process's unhandled
//! rejection.
//!
//! The headless shell stands in for the window: it records what it was asked and can close the
//! inspector or give it the focus as a person would. The order of the events and the state the
//! methods answer are what Electron 39.8.10 (Windows) did with `show: false` windows, recorded by
//! `tests/electron/devtools/events` in `expected/events-right.txt` and `events-detach.txt`; where
//! Verve differs, the test says so.
#![allow(clippy::unwrap_used)]

mod support;

use support::{App, TIMEOUT};
use verve_electron::ids::ContentsId;
use verve_electron::msg::ToRenderer;

/// The page of the REPL tests: a title, a number and an element.
const PAGE: &str = "<title>Hello</title><body><p id=p>text</p><script>window.answer = 42;</script>";

/// The preload: a value in the isolated world and one the main world is given.
const PRELOAD: &str = "const { contextBridge } = require('electron');
window.onlyHere = 'isolated';
contextBridge.exposeInMainWorld('bridge', { n: 7 });";

const REPL_APP: &str = r"const { app, BrowserWindow } = require('electron');
const path = require('path');
app.on('window-all-closed', () => {});
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, title: 'repl',
    webPreferences: { preload: path.join(__dirname, 'preload.js') } });
  win.webContents.on('console-message', (event) => console.log('heard ' + event.message));
  await win.loadFile('page.html');
  console.log('LOADED');
});";

/// An app with one page, loaded, and the page's id.
fn repl_app() -> (App, ContentsId) {
    let files = [("page.html", PAGE), ("preload.js", PRELOAD)];
    let app = App::launch_script(REPL_APP, &files);
    assert!(
        app.run_until(|app| app.console().iter().any(|l| l == "LOADED"), TIMEOUT),
        "{:?}",
        app.console()
    );
    // The page's `<title>` replaced the title the window was made with.
    let contents = app.window("Hello").expect("the window").contents;
    (app, contents)
}

/// Types `code` into the inspector of `contents` as call `call`, and waits for the answer.
fn ask(app: &App, contents: ContentsId, call: u64, code: &str) -> (String, bool) {
    let message = ToRenderer::InspectorEval {
        call,
        code: code.to_owned(),
    };
    app.hub.to_renderer(contents, message);
    let answered = |app: &App| {
        let replies = app.shell.inspector_replies();
        replies.iter().any(|(_, answered, _, _)| *answered == call)
    };
    assert!(app.run_until(answered, TIMEOUT), "no answer to `{code}`");
    let replies = app.shell.inspector_replies();
    let (page, _, text, is_error) = replies.into_iter().find(|r| r.1 == call).unwrap();
    assert_eq!(page, contents);
    (text, is_error)
}

#[test]
fn the_repl_evaluates_in_the_pages_main_world_and_answers_in_inspected_text() {
    let (app, contents) = repl_app();
    let said = |code: &str, call: u64| ask(&app, contents, call, code);
    assert_eq!(said("document.title", 1), ("'Hello'".to_owned(), false));
    assert_eq!(said("1 + 1", 2), ("2".to_owned(), false));
    assert_eq!(said("window.answer", 3), ("42".to_owned(), false));
    assert_eq!(
        said("({ a: 1, b: [1, 2] })", 4),
        ("{ a: 1, b: [ 1, 2 ] }".to_owned(), false)
    );
    assert_eq!(said("undefined", 5), ("undefined".to_owned(), false));
    let element = said("document.getElementById('p').textContent", 6);
    assert_eq!(element, ("'text'".to_owned(), false));
    // The script is the page's: what it declares stays for the next line.
    assert_eq!(said("var counter = 5", 7), ("undefined".to_owned(), false));
    assert_eq!(said("counter + 1", 8), ("6".to_owned(), false));
}

#[test]
fn the_repl_is_in_the_main_world_and_not_the_preloads_isolated_one() {
    let (app, contents) = repl_app();
    let (text, is_error) = ask(&app, contents, 1, "typeof bridge + ' ' + typeof onlyHere");
    // The bridge was exposed to the main world; `window.onlyHere` is the isolated world's own.
    assert_eq!((text.as_str(), is_error), ("'object undefined'", false));
}

#[test]
fn a_throw_is_answered_as_the_errors_inspected_text() {
    let (app, contents) = repl_app();
    let (text, is_error) = ask(&app, contents, 1, "throw new TypeError('no good')");
    assert!(is_error);
    assert!(text.starts_with("TypeError: no good"), "{text}");
    let (text, is_error) = ask(&app, contents, 2, "missing.property");
    assert!(is_error);
    assert!(
        text.starts_with("ReferenceError: missing is not defined"),
        "{text}"
    );
    let (text, is_error) = ask(&app, contents, 3, "this is not javascript");
    assert!(is_error);
    assert!(text.contains("SyntaxError"), "{text}");
    // The page is still there for the next line.
    assert_eq!(ask(&app, contents, 4, "'ok'"), ("'ok'".to_owned(), false));
}

#[test]
fn the_repl_writes_nothing_to_the_pages_console_and_the_main_process_hears_nothing() {
    let (app, contents) = repl_app();
    ask(&app, contents, 1, "throw new Error('quiet')");
    ask(&app, contents, 2, "'also quiet'");
    // A line the page does write is heard, so a silent console is not a deaf test.
    ask(&app, contents, 3, "console.log('loud'); 0");
    assert!(app.run_until(
        |app| app.console().iter().any(|l| l == "heard loud"),
        TIMEOUT
    ));
    let lines = app.console();
    assert!(!lines.iter().any(|l| l.contains("quiet")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("Uncaught")), "{lines:?}");
}

#[test]
fn a_value_that_is_a_promise_is_shown_as_it_is_when_it_returns() {
    let (app, contents) = repl_app();
    let (text, _) = ask(&app, contents, 1, "Promise.resolve(3)");
    assert_eq!(text, "Promise { 3 }");
    let (text, _) = ask(&app, contents, 2, "new Promise(() => {})");
    assert_eq!(text, "Promise { <pending> }");
}

/// The app of the event tests: one page with its DevTools toggled from the test through `globalThis`.
const EVENTS_APP: &str = r"const { app, BrowserWindow } = require('electron');
app.on('window-all-closed', () => {});
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, title: 'events' });
  const wc = win.webContents;
  globalThis.wc = wc;
  globalThis.events = [];
  const state = (tag) => globalThis.events.push(`${tag} opened=${wc.isDevToolsOpened()} focused=${wc.isDevToolsFocused()} wc=${String(wc.devToolsWebContents)}`);
  for (const name of ['devtools-opened', 'devtools-closed', 'devtools-focused']) {
    wc.on(name, () => state('event ' + name));
  }
  await win.loadURL('data:text/html,<title>t</title>hello');
  console.log('LOADED');
});";

fn events_app() -> (App, ContentsId) {
    let app = App::launch_script(EVENTS_APP, &[]);
    assert!(app.run_until(|app| app.console().iter().any(|l| l == "LOADED"), TIMEOUT));
    let contents = app.window("t").expect("the window").contents;
    (app, contents)
}

fn events_of(app: &App) -> Vec<String> {
    serde_json::from_str(&app.eval_main("globalThis.events")).unwrap()
}

fn wait_for_events(app: &App, count: usize) -> Vec<String> {
    assert!(
        app.run_until(|app| events_of(app).len() >= count, TIMEOUT),
        "{:?}",
        events_of(app)
    );
    events_of(app)
}

#[test]
fn opening_the_devtools_asks_the_shell_for_an_inspector_and_says_devtools_opened() {
    let (app, contents) = events_app();
    assert!(app.shell.open_inspectors().is_empty());
    app.eval_main("wc.openDevTools()");
    assert_eq!(app.shell.open_inspectors(), [contents]);
    // Opening them again does not make a second window.
    app.eval_main("wc.openDevTools()");
    assert_eq!(app.shell.open_inspectors(), [contents]);
    // Electron 39.8.10 says `isDevToolsOpened()` is still false when `openDevTools()` returns and
    // true in `'devtools-opened'`; Verve's is true at once (P43 phase 1), and `devToolsWebContents`
    // stays null where Electron's is an object.
    let events = wait_for_events(&app, 1);
    assert_eq!(
        events,
        ["event devtools-opened opened=true focused=false wc=null"]
    );
}

#[test]
fn closing_the_devtools_closes_the_inspector_window_once() {
    let (app, contents) = events_app();
    app.eval_main("wc.openDevTools()");
    wait_for_events(&app, 1);
    app.eval_main("wc.closeDevTools(); wc.closeDevTools()");
    assert!(app.shell.open_inspectors().is_empty());
    let events = wait_for_events(&app, 2);
    assert_eq!(
        events[1],
        "event devtools-closed opened=false focused=false wc=null"
    );
    // The main process has run everything queued before this call, the events too.
    app.eval_main("0");
    assert_eq!(events_of(&app).len(), 2, "closed once");
    // Toggling opens them again, and toggling again closes them.
    app.eval_main("wc.toggleDevTools()");
    assert_eq!(app.shell.open_inspectors(), [contents]);
    wait_for_events(&app, 3);
    app.eval_main("wc.toggleDevTools()");
    wait_for_events(&app, 4);
    assert!(app.shell.open_inspectors().is_empty());
}

#[test]
fn a_person_closing_the_inspector_window_is_devtools_closed() {
    let (app, contents) = events_app();
    app.eval_main("wc.openDevTools()");
    wait_for_events(&app, 1);
    app.shell.close_inspector(contents);
    let events = wait_for_events(&app, 2);
    assert_eq!(
        events[1],
        "event devtools-closed opened=false focused=false wc=null"
    );
    assert_eq!(app.eval_main("wc.isDevToolsOpened()"), "false");
    // The window is gone: the next `closeDevTools()` has nothing to close and says nothing.
    app.eval_main("wc.closeDevTools()");
    app.eval_main("0");
    assert_eq!(events_of(&app).len(), 2);
    assert!(app.shell.open_inspectors().is_empty());
}

#[test]
fn the_inspector_gaining_the_focus_is_devtools_focused_and_is_devtools_focused_follows_it() {
    let (app, contents) = events_app();
    app.eval_main("wc.openDevTools()");
    wait_for_events(&app, 1);
    assert_eq!(app.eval_main("wc.isDevToolsFocused()"), "false");
    app.shell.focus_inspector(contents, true);
    let events = wait_for_events(&app, 2);
    // Electron 39.8.10 detached: `devtools-focused` came first, with `isDevToolsFocused()` true;
    // the window was not up when `devtools-opened` was said. Verve's window is made by the call,
    // and the focus comes after it. Docked in a hidden window, Electron says no focus event.
    assert_eq!(
        events[1],
        "event devtools-focused opened=true focused=true wc=null"
    );
    // Gaining it again says nothing more.
    app.shell.focus_inspector(contents, true);
    app.eval_main("0");
    assert_eq!(events_of(&app).len(), 2);
    app.shell.focus_inspector(contents, false);
    let focused = || app.eval_main("wc.isDevToolsFocused()");
    assert!(app.run_until(|_| focused() == "false", TIMEOUT));
    // Closing it clears the focus.
    app.shell.focus_inspector(contents, true);
    assert!(app.run_until(|_| focused() == "true", TIMEOUT));
    app.eval_main("wc.closeDevTools()");
    assert_eq!(focused(), "false");
}

#[test]
fn a_window_whose_devtools_are_off_gets_no_inspector() {
    let app = App::launch_script(
        r"const { app, BrowserWindow } = require('electron');
app.on('window-all-closed', () => {});
app.whenReady().then(() => {
  const win = new BrowserWindow({ show: false, title: 'off', webPreferences: { devTools: false } });
  win.webContents.openDevTools();
  console.log('ASKED');
});",
        &[],
    );
    assert!(app.run_until(|app| app.console().iter().any(|l| l == "ASKED"), TIMEOUT));
    // The headless shell applies what the main process asked as it is asked, so `ASKED` is after.
    assert!(app.shell.open_inspectors().is_empty());
}

#[test]
fn the_main_process_warns_of_an_unhandled_rejection_as_electron_does() {
    // Electron 39.8.10 (`expected/rejection-error.txt`, `rejection-listener.txt`): two warnings on
    // stderr, `uncaughtException` is not said, no error box, and the app goes on.
    let lines = support::run_main(
        "process.on('uncaughtException', () => log('uncaught'));
         Promise.reject(new Error('main reject'));
         await sleep(100);
         log('after');",
    );
    let warned: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("UnhandledPromiseRejectionWarning"))
        .collect();
    assert_eq!(warned.len(), 2, "{lines:?}");
    assert!(warned[0].contains("Error: main reject"), "{lines:?}");
    assert!(warned[1].contains("(rejection id: "), "{lines:?}");
    assert!(lines.iter().any(|l| l == "after"), "{lines:?}");
    assert!(!lines.iter().any(|l| l == "uncaught"), "{lines:?}");
}
