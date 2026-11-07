//! What the `electron` binary says on the terminal for a developer (P43): the unsupported
//! debugging switches, a page's console, Verve's own diagnostics and the errors nobody caught.
//! It runs `--headless`, so no window opens and no box is shown.

use std::process::Output;

mod support;

/// A page that writes to its console on lines 2 and 3.
const PAGE: &str = "<script>\nconsole.warn('careful');\nconsole.log('plain');\n</script>";

/// An app that loads `page.html`, sends one message to the main process, and ends.
const APP: &str = r"const { app, BrowserWindow, ipcMain } = require('electron');
ipcMain.on('ping', () => {});
app.whenReady().then(async () => {
  const win = new BrowserWindow({ show: false, webPreferences: { nodeIntegration: true, contextIsolation: false } });
  await win.loadFile('page.html');
  await win.webContents.executeJavaScript(`require('electron').ipcRenderer.send('ping')`);
  setTimeout(() => app.exit(0), 200);
});";

/// Runs `main_js` as an app, with `args` after its path and `env` set, beside the page.
fn run(name: &str, main_js: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    support::run_headless(name, main_js, &[("page.html", PAGE)], args, env)
}

fn stderr_of(output: &Output) -> Vec<String> {
    let text = String::from_utf8_lossy(&output.stderr);
    text.lines().map(str::to_owned).collect()
}

/// The lines of `lines` that start with `prefix`.
fn starting<'a>(lines: &'a [String], prefix: &str) -> Vec<&'a str> {
    let found = lines.iter().filter(|line| line.starts_with(prefix));
    found.map(String::as_str).collect()
}

#[test]
fn an_inspect_switch_is_accepted_with_exactly_one_warning_line() {
    let output = run("inspect", APP, &["--inspect=9229"], &[]);
    assert_eq!(output.status.code(), Some(0));
    let lines = stderr_of(&output);
    assert_eq!(
        starting(&lines, "Verve:"),
        [
            "Verve: --inspect is not supported; see VERVE_ELECTRON_LOG and webContents.openDevTools()"
        ],
        "{lines:?}"
    );
}

#[test]
fn a_pages_console_is_on_stderr_only_when_logging_is_asked_for() {
    let quiet = stderr_of(&run("quiet", APP, &[], &[]));
    assert!(starting(&quiet, "[renderer").is_empty(), "{quiet:?}");

    let expected = [
        "[renderer:1 page.html:2] careful",
        "[renderer:1 page.html:3] plain",
    ];
    let env = stderr_of(&run(
        "env-log",
        APP,
        &[],
        &[("ELECTRON_ENABLE_LOGGING", "1")],
    ));
    assert_eq!(starting(&env, "[renderer"), expected, "{env:?}");
    let switch = stderr_of(&run("switch-log", APP, &["--enable-logging"], &[]));
    assert_eq!(starting(&switch, "[renderer"), expected, "{switch:?}");
    assert!(starting(&switch, "Verve:").is_empty(), "{switch:?}");
}

#[test]
fn a_forced_colour_wraps_a_warning_in_yellow() {
    let env = [("ELECTRON_ENABLE_LOGGING", "1"), ("FORCE_COLOR", "1")];
    let lines = stderr_of(&run("colour", APP, &[], &env));
    let warned = starting(&lines, "\x1b[33m[renderer");
    assert_eq!(
        warned,
        ["\x1b[33m[renderer:1 page.html:2] careful\x1b[0m"],
        "{lines:?}"
    );
}

#[test]
fn verve_electron_log_names_the_categories_it_writes() {
    let lines = stderr_of(&run("ipc", APP, &[], &[("VERVE_ELECTRON_LOG", "ipc")]));
    let ipc = starting(&lines, "[verve:ipc]");
    assert_eq!(
        ipc,
        ["[verve:ipc] renderer 1 -> main send \"ping\" (0 args)"],
        "{lines:?}"
    );
    let off = stderr_of(&run("unset", APP, &[], &[]));
    assert!(starting(&off, "[verve:").is_empty(), "{off:?}");
}

#[test]
fn an_error_nobody_caught_in_the_main_process_prints_its_stack_and_the_app_goes_on() {
    // Electron 39.8.10's own `lib/browser/init.ts`: with no other `uncaughtException` listener it
    // shows "A JavaScript error occurred in the main process" with "Uncaught Exception:\n" and
    // the error's stack. Headless, the box is a line of the trace, and the app keeps running.
    let script = "setTimeout(() => { throw new Error('main boom'); }, 20);
                  setTimeout(() => process.exit(3), 300);";
    let output = run("uncaught", script, &[], &[]);
    assert_eq!(output.status.code(), Some(3));
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(text.contains("Error: main boom\n    at "), "{text}");
    let boxed = "[app] error box \"A JavaScript error occurred in the main process\" \"Uncaught Exception:\\nError: main boom";
    assert!(text.contains(boxed), "{text}");
}
