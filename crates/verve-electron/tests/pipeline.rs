//! P02's smallest link, end to end and headless: the app's script makes a window, loads a page
//! into it, the page's own isolate parses the page, and closing every window ends the app with
//! code 0. No display, no GPU, no GPUI.

mod support;

use std::sync::Arc;
use std::time::Duration;

use support::{App, TIMEOUT};
use verve_electron::msg::ToShell;

const MAIN: &str = r#"
const { BrowserWindow } = require('electron');

const win = new BrowserWindow({ width: 640, height: 480, title: 'hello' });
win.loadURL('data:text/html,<h1>hi</h1>');

// The window's 'closed' event reaches this script: the window it makes in answer is how the test
// knows it got there at all.
let again = false;
win.on('closed', () => {
  if (again) return;
  again = true;
  new BrowserWindow({ title: 'again' });
});
"#;

#[test]
fn a_window_draws_its_page_and_the_app_ends_when_it_closes() {
    let app = App::launch_script(MAIN, &[]);
    let hello = app.window("hello").expect("the main script made a window");
    assert!(
        matches!(
            app.shell.received().first(),
            Some(ToShell::CreateWindow { window, .. }) if *window == hello.id
        ),
        "{:?}",
        app.shell.received()
    );
    assert_eq!(hello.bounds.width, 640.0);
    assert_eq!(hello.bounds.height, 480.0);
    assert!(app.run_until(
        |app| app.page_text(hello.id).as_deref() == Some("hi"),
        TIMEOUT
    ));

    // A person clicks the close button: the window goes, the script hears of it and makes
    // another, and closing that one too leaves nothing running.
    app.shell.user_close(hello.id);
    let again = app
        .window("again")
        .expect("the script made a second window");
    assert_ne!(again.id, hello.id);
    app.shell.user_close(again.id);
    assert_eq!(app.exit_code(TIMEOUT), Some(0));
}

/// Electron 39.8.10: a main process with no window and nothing to run does not end; it waits for
/// events until `app.quit()` (P10 replaced P02's "no windows, no timers" rule with this).
#[test]
fn a_script_that_makes_no_window_does_not_end_the_app_until_it_quits() {
    let app = App::launch_script("require('electron');", &[]);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(app.exit_code(Duration::from_millis(1)), None);
    assert!(app.shell.windows().is_empty());
    let quits = App::launch_script("require('electron').app.quit();", &[]);
    assert_eq!(quits.exit_code(TIMEOUT), Some(0));
}

/// Electron 39.8.10: an error thrown as the entry loads is printed with "App threw an error during
/// load" and shown in an error box, and the process stays up; a module that cannot be found ends
/// it with 1. P02 ended the app with 1 on any throw.
#[test]
fn a_script_that_throws_stays_up_and_one_that_cannot_be_found_ends_with_one() {
    let app = App::launch_script("throw new Error('no');", &[]);
    let boxed = app.run_until(
        |app| {
            app.shell
                .received()
                .iter()
                .any(|msg| matches!(msg, ToShell::ErrorBox { content, .. } if content.contains("Error: no")))
        },
        TIMEOUT,
    );
    assert!(boxed);
    assert_eq!(app.exit_code(Duration::from_millis(1)), None);
    let missing = App::launch_script("require('./no-such-module-anywhere');", &[]);
    assert_eq!(missing.exit_code(TIMEOUT), Some(1));
}

/// `ElectronOptions::dark` is the system theme the page sees: its
/// `@media (prefers-color-scheme: dark)` matches only when the app was started dark.
#[test]
fn the_system_theme_reaches_the_pages_styles() {
    let page = |dark: bool| {
        let app = App::launch_script_in(DARK_PAGE, &[], dark);
        let window = app.window("theme").expect("the window was made");
        assert!(app.run_until(
            |app| app.page_text(window.id).as_deref() == Some("hi"),
            TIMEOUT
        ));
        let snapshot = app.shell.snapshot(window.id).expect("a view");
        let heading = snapshot
            .dom
            .query(snapshot.dom.body, "h1")
            .expect("the heading is there");
        let color = snapshot.style(heading, "color").map(str::to_owned);
        app.shell.user_close(window.id);
        (app, color)
    };

    let (dark_app, dark) = page(true);
    assert!(
        dark.as_deref().is_some_and(|c| c.contains("rgb(1, 2, 3)")),
        "a dark app's page matched prefers-color-scheme: dark, got {dark:?}"
    );
    let (light_app, light) = page(false);
    assert_ne!(
        light.as_deref(),
        dark.as_deref(),
        "a light app's page does not match it"
    );
    drop((dark_app, light_app));
}

const DARK_PAGE: &str = r#"
const { BrowserWindow } = require('electron');
const win = new BrowserWindow({ title: 'theme' });
win.loadURL('data:text/html,<style>@media (prefers-color-scheme: dark) { h1 { color: rgb(1, 2, 3) } }</style><h1>hi</h1>');
"#;

/// A window that is gone answers `TypeError: Object has been destroyed` from its methods, its
/// `'closed'` listeners included — Electron 39.8.10's native window is already destroyed by the
/// time they run — while `win.id` goes on answering.
#[test]
fn a_window_that_closed_is_destroyed_for_its_own_closed_listener() {
    let app = App::launch_script(
        r#"
        const { BrowserWindow } = require('electron');
        const win = new BrowserWindow({ title: 'gone' });
        win.on('closed', () => {
            let said;
            try {
                win.loadURL('data:text/html,x');
                said = 'no error';
            } catch (error) {
                said = error.name + ': ' + error.message + ' #' + win.id;
            }
            new BrowserWindow({ title: said });
        });
        win.close();
        "#,
        &[],
    );
    let report = app
        .window("TypeError: Object has been destroyed #1")
        .expect("the closed listener reported what it got");
    app.shell.user_close(report.id);
    assert_eq!(app.exit_code(TIMEOUT), Some(0));
}

/// Electron's defaults for a window made with no options, and `show: false` creating it invisible.
#[test]
fn a_window_takes_electrons_defaults() {
    let app = App::launch_script(
        r#"
        const { BrowserWindow } = require('electron');
        new BrowserWindow();
        new BrowserWindow({ title: 'hidden', show: false });
        "#,
        &[],
    );
    let plain = app.window("Electron").expect("the default title");
    assert_eq!(plain.bounds.width, 800.0);
    assert_eq!(plain.bounds.height, 600.0);
    assert!(plain.visible, "a window defaults to shown");

    let hidden = app.window("hidden").expect("the second window");
    assert!(!hidden.visible, "show: false keeps it hidden");
    assert!(!hidden.focused);

    app.shell.user_close(plain.id);
    app.shell.user_close(hidden.id);
    assert_eq!(app.exit_code(TIMEOUT), Some(0));
}

/// `process.type` is Electron's: `browser` in the main process, whose `process.exit` is the app's.
#[test]
fn the_main_process_is_the_browser_process_and_its_exit_is_the_apps() {
    let app = App::launch_script(
        r#"
        const { BrowserWindow } = require('electron');
        new BrowserWindow({ title: process.type });
        setTimeout(() => process.exit(3), 20);
        "#,
        &[],
    );
    assert!(app.window("browser").is_some(), "process.type is 'browser'");
    assert_eq!(app.exit_code(TIMEOUT), Some(3));
}

#[test]
fn process_exit_while_the_script_loads_is_not_a_failure_to_load() {
    let app = App::launch_script("process.exit(7); throw new Error('not reached');", &[]);
    assert_eq!(app.exit_code(TIMEOUT), Some(7));
}

/// Electron 39.8.10 reads a constructor's options leniently: a size that is not an integer, or
/// not a number, is the default, and the rest of the options are still read.
#[test]
fn options_of_the_wrong_type_are_left_at_their_defaults() {
    let app = App::launch_script(
        r#"
        const { BrowserWindow } = require('electron');
        new BrowserWindow({ title: 'abc', width: 'abc', height: 300.7 });
        new BrowserWindow({ title: 'size', width: 300, height: 200, show: 0 });
        new BrowserWindow(5);
        "#,
        &[],
    );
    let lenient = app.window("abc").expect("the first window");
    assert_eq!((lenient.bounds.width, lenient.bounds.height), (800., 600.));
    let sized = app.window("size").expect("the second window");
    assert_eq!((sized.bounds.width, sized.bounds.height), (300., 200.));
    assert!(!sized.visible, "show: 0 is false, by truthiness");
    assert!(
        app.window("Electron").is_some(),
        "options of 5 make a default window"
    );
}

/// Electron keeps a window until it is closed: a script that let go of its last reference still
/// hears `'closed'` on it.
#[test]
fn a_window_nothing_refers_to_still_closes_with_its_listener() {
    let app = App::launch_script(
        r#"
        const { BrowserWindow } = require('electron');
        (() => {
            const win = new BrowserWindow({ title: 'unreferenced' });
            win.on('closed', () => new BrowserWindow({ title: 'heard' }));
        })();
        // Enough garbage to make the collector run before the window is closed.
        let junk = [];
        for (let round = 0; round < 20; round++) {
            for (let i = 0; i < 20000; i++) junk.push({ i, text: 'x'.repeat(8) });
            junk = [];
        }
        "#,
        &[],
    );
    let window = app
        .window("unreferenced")
        .expect("the script made a window");
    app.shell.user_close(window.id);
    let heard = app
        .window("heard")
        .expect("'closed' ran on the unreferenced window");
    app.shell.user_close(heard.id);
    assert_eq!(app.exit_code(TIMEOUT), Some(0));
}

/// Ending the app ends every thread the Hub started: nothing holds the Hub once it has said so,
/// and a script that never returns is interrupted rather than waited for.
#[test]
fn shutdown_ends_the_main_process_and_every_page() {
    let app = App::launch_script(
        r#"
        const { BrowserWindow } = require('electron');
        new BrowserWindow({ title: 'one' });
        new BrowserWindow({ title: 'two' });
        setTimeout(() => process.exit(4), 30);
        "#,
        &[],
    );
    assert_eq!(app.exit_code(TIMEOUT), Some(4));
    // The test's own handle is the only one left once the threads have ended.
    let ended = app.run_until(|app| Arc::strong_count(&app.hub) == 1, TIMEOUT);
    assert!(
        ended,
        "{} handles on the Hub are still held by a thread",
        Arc::strong_count(&app.hub) - 1
    );
}

#[test]
fn a_main_script_that_never_returns_is_interrupted_by_shutdown() {
    let app = App::launch_script("for (;;) {}", &[]);
    std::thread::sleep(Duration::from_millis(100));
    app.hub.shutdown(9);
    assert_eq!(app.exit_code(TIMEOUT), Some(9));
    let ended = app.run_until(|app| Arc::strong_count(&app.hub) == 1, TIMEOUT);
    assert!(ended, "the main thread left its loop and ended");
}
