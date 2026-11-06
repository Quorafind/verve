//! The fixture apps in `tests/electron/apps`, run headless and held to what real Electron did
//! (`expected.json`, recorded by `tests/electron/record.cjs`). Each package turns its tests on
//! when the API its fixture uses exists: the `#[ignore]` names who does.
//!
//! `lifecycle` and `single-instance` are not replayed here: their scenarios and log files are
//! chosen by switches (`--scenario`, `--log`, `--role`), so `tests/app.rs` runs them, with a
//! profile directory of its own each, against what Electron 39.8.10 did.

mod support;

use std::time::Duration;

use support::{App, TIMEOUT, console_of, fixture, recorded_console, written_by};

/// How long a fixture may take to end by itself.
const ENDS_WITHIN: Duration = Duration::from_secs(30);

/// Runs the fixture to its end and holds it to the recording: the same lines from each process,
/// and exit code 0. The recorded lines of each process go through `filter` first, for what the
/// system that recorded them added by itself.
fn replays_without(name: &str, filter: fn(Vec<String>) -> Vec<String>) {
    let app = App::launch(&fixture(name));
    let code = app.exit_code(ENDS_WITHIN);
    let (actual, recorded) = (console_of(&app), recorded_console(name));
    for from in ["main", "renderer"] {
        assert_eq!(
            written_by(&actual, from),
            filter(written_by(&recorded, from)),
            "{name}: {from} console"
        );
    }
    assert_eq!(code, Some(0), "{name} did not end with 0");
}

/// [`replays_without`] nothing.
fn replays(name: &str) {
    replays_without(name, |lines| lines);
}

#[test]
fn hello_shows_the_versions_in_one_window_and_ends_with_zero() {
    let app = App::launch(&fixture("hello"));
    // The window is titled by its page once it has loaded, as Electron's is.
    let window = app
        .window("Hello Verve")
        .expect("the page titled the window");
    assert_eq!((window.bounds.width, window.bounds.height), (800., 600.));

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
fn quick_start_opens_its_window_and_quits_when_it_closes() {
    let app = App::launch(&fixture("quick-start"));
    let window = app
        .window("Hello World!")
        .expect("the page titled the window");
    app.shell.user_close(window.id);
    assert_eq!(app.wait_exit(ENDS_WITHIN), 0);
}

#[test]
#[ignore = "P13/P20: webContents events, loadFile and <script src>; its conversation runs in tests/ipc.rs"]
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
#[ignore = "P13 phase 2: setWindowOpenHandler and window.open, executeJavaScript (parent, child and modal windows work: tests/browser_window.rs)"]
fn windows_have_parents_children_and_a_modal() {
    replays("multi-window");
}

/// What Windows adds to the recorded events of `window-state` by itself: a `move` when a window
/// is first shown and when it is minimized, a second `focus` when it is restored, and the `blur` of
/// a minimized window that the system had made the active one although `showInactive` showed it. The
/// headless shell does not repeat them (`headless/window.rs`; its contract test asks for the focus
/// first), so the recording is held to without them.
fn without_the_systems_own(lines: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for line in lines {
        let previous = kept.last().map(String::as_str);
        let extra = (line == "window move" && matches!(previous, Some("-- show" | "-- minimize")))
            || (line == "window focus" && previous == Some("window focus"))
            || (line == "window blur" && previous == Some("window minimize"));
        if !extra {
            kept.push(line);
        }
    }
    kept
}

#[test]
fn window_events_come_in_the_recorded_order() {
    replays_without("window-state", without_the_systems_own);
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
#[ignore = "P30: the layout a click needs (P20 loads and renders the page: tests/page_load.rs)"]
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
