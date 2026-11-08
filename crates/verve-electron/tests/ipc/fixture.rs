//! The `ipc` fixture app (`tests/electron/apps/ipc`), as far as it can run until P13 and P20 bring
//! `webContents` events and the page loader: its own handlers and its own page script, with the two
//! lines that need them replaced, held to the console of real Electron 39.8.10.

use crate::support::{App, TIMEOUT, console_of, fixture, recorded_console, written_by};

fn source(file: &str) -> String {
    std::fs::read_to_string(fixture("ipc").join(file)).expect("the fixture is there")
}

/// A console line without its stack frames, as the recording keeps them.
fn plain(text: &str) -> String {
    let lines: Vec<_> = text
        .lines()
        .filter(|line| !line.trim_start().starts_with("at "))
        .collect();
    lines.join("\n")
}

#[test]
fn the_fixtures_conversation_prints_what_electron_printed() {
    let main = source("main.js")
        .replace(
            "win.loadFile(path.join(__dirname, 'index.html'));",
            "win.loadURL('data:text/html,<p>ipc fixture</p>');",
        )
        .replace(
            "win.webContents.on('did-finish-load', () => win.webContents.send('hello-from-main', { n: 1 }));",
            "setTimeout(() => win.webContents.send('hello-from-main', { n: 1 }), 300);",
        );
    assert!(!main.contains("loadFile") && !main.contains("did-finish-load"));
    let app = App::launch_script(&main, &[]);
    assert!(app.run_until(|app| !app.shell.windows().is_empty(), TIMEOUT));
    let page = app.shell.windows()[0].contents;
    app.eval_renderer(page, &source("renderer.js"));
    assert_eq!(app.exit_code(TIMEOUT), Some(0), "{:?}", app.console());

    let actual: Vec<_> = console_of(&app)
        .into_iter()
        .map(|(from, text)| (from, plain(&text)))
        .collect();
    let recorded = recorded_console("ipc");
    for from in ["main", "renderer"] {
        assert_eq!(
            written_by(&actual, from),
            written_by(&recorded, from),
            "{from}"
        );
    }
}
