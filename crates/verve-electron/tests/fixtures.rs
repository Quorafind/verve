//! The fixture apps in `tests/electron/apps`, run headless and held to what real Electron did
//! (`expected.json`, recorded by `tests/electron/record.cjs`). Each package turns its tests on
//! when the API its fixture uses exists: the `#[ignore]` names who does.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use support::{App, TIMEOUT};

/// How long a fixture may take to end by itself.
const ENDS_WITHIN: Duration = Duration::from_secs(30);

fn fixture(name: &str) -> PathBuf {
    let apps = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps");
    apps.join(name)
}

/// The console lines of real Electron, `(from, text)`, without the development-mode security
/// warning every page without a content security policy gets.
fn recorded_console(name: &str) -> Vec<(String, String)> {
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

fn console_of(app: &App) -> Vec<(String, String)> {
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
fn written_by(lines: &[(String, String)], from: &str) -> Vec<String> {
    lines
        .iter()
        .filter(|(who, _)| who == from)
        .map(|(_, text)| text.clone())
        .collect()
}

/// Runs the fixture to its end and holds it to the recording: the same lines from each process,
/// and exit code 0.
fn replays(name: &str) {
    let app = App::launch(&fixture(name));
    let code = app.exit_code(ENDS_WITHIN);
    let (actual, recorded) = (console_of(&app), recorded_console(name));
    for from in ["main", "renderer"] {
        assert_eq!(
            written_by(&actual, from),
            written_by(&recorded, from),
            "{name}: {from} console"
        );
    }
    assert_eq!(code, Some(0), "{name} did not end with 0");
}

#[test]
fn hello_shows_the_versions_in_one_window_and_ends_with_zero() {
    let app = App::launch(&fixture("hello"));
    let window = app.window("hello").expect("the app made its window");
    assert_eq!((window.bounds.width, window.bounds.height), (800., 600.));
    // The page's <title> reaches the window as 'page-title-updated' in Electron: P13 and P20.
    let title = |app: &App| app.eval_renderer(window.contents, "document.title");
    assert!(app.run_until(|app| title(app) == r#""Hello Verve""#, TIMEOUT));

    assert!(app.run_until(
        |app| {
            app.page_text(window.id)
                .is_some_and(|t| t.contains("node "))
        },
        TIMEOUT
    ));
    assert_eq!(app.shell.windows().len(), 1);
    let recorded = written_by(&recorded_console("hello"), "main");
    assert!(app.run_until(
        |app| written_by(&console_of(app), "main") == recorded,
        TIMEOUT
    ));
    assert_eq!(app.wait_exit(ENDS_WITHIN), 0);
}

#[test]
#[ignore = "P10: needs the app module, so that the script is not ended by process.exit"]
fn quick_start_opens_its_window_and_quits_when_it_closes() {
    let app = App::launch(&fixture("quick-start"));
    let window = app
        .window("Hello World!")
        .expect("the page titled the window");
    app.shell.user_close(window.id);
    assert_eq!(app.wait_exit(ENDS_WITHIN), 0);
}

#[test]
#[ignore = "P10: the lifecycle fixture is P10's, with the app module"]
fn lifecycle_events_come_in_electrons_order() {
    replays("lifecycle");
}

#[test]
#[ignore = "P10: the single-instance fixture is P10's"]
fn a_second_launch_reaches_the_first_instance() {
    replays("single-instance");
}

#[test]
#[ignore = "P12: ipcMain and ipcRenderer"]
fn ipc_messages_travel_both_ways() {
    replays("ipc");
}

#[test]
#[ignore = "P21: contextBridge and preload scripts"]
fn a_preload_exposes_only_its_bridge_to_an_isolated_page() {
    replays("preload-isolated");
}

#[test]
#[ignore = "P21: preload scripts and nodeIntegration"]
fn a_page_with_node_integration_requires_node_modules() {
    replays("preload-node");
}

#[test]
#[ignore = "P13: parent, modal and child windows, setWindowOpenHandler"]
fn windows_have_parents_children_and_a_modal() {
    replays("multi-window");
}

#[test]
#[ignore = "P13: the BrowserWindow methods and events"]
fn window_events_come_in_the_recorded_order() {
    replays("window-state");
}

#[test]
#[ignore = "P16: protocol.handle, with P20's page loader"]
fn a_site_is_served_from_a_custom_scheme() {
    replays("protocol");
}

#[test]
#[ignore = "P15: Menu and Tray; not recorded, a tray icon being a thing to see"]
fn menus_and_a_tray_are_built() {
    let app = App::launch(&fixture("menu-tray"));
    assert_eq!(app.wait_exit(ENDS_WITHIN), 0);
}

#[test]
#[ignore = "P14: dialogs answered from presets; not recorded, a dialog waiting for a person"]
fn dialogs_answer_from_the_shells_presets() {
    let app = App::launch(&fixture("dialogs"));
    assert_eq!(app.wait_exit(ENDS_WITHIN), 0);
}

#[test]
#[ignore = "P17: child_process"]
fn children_are_spawned_executed_and_forked() {
    replays("child");
}

#[test]
#[ignore = "P11: the test packs the directory into an app.asar first"]
fn an_app_runs_from_an_asar_archive() {
    replays("asar");
}

#[test]
#[ignore = "P20: the page loader, and P30 for the layout a React page needs"]
fn a_react_app_built_by_vite_renders_and_counts_clicks() {
    let app = App::launch(&fixture("react-vite"));
    let window = app
        .window("React Vite")
        .expect("the page titled the window");
    let shows = |count: u32| {
        move |app: &App| {
            let text = app.page_text(window.id);
            text.is_some_and(|text| text.contains(&format!("count is {count}")))
        }
    };
    assert!(app.run_until(shows(0), TIMEOUT));
    app.shell.click(window.id, "#inc");
    assert!(app.run_until(shows(1), TIMEOUT));
}
