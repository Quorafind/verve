//! A window's life as the main process sees it, on the headless shell: a `'close'` listener
//! that prevents keeps the window, a person's close asks the same listeners as `win.close()`
//! does, and a close that is not prevented ends the window once. The quit flow itself
//! (`window-all-closed` with and without a listener, `app.quit()` asking every window) is
//! `app.rs`'s; the shell's side of a window is `shell_contract.rs`'s, for both shells.

mod support;

use std::time::Duration;

use support::{App, TIMEOUT};

const SCRIPT: &str = r#"
const { app, BrowserWindow } = require('electron');
let keep = true;
const win = new BrowserWindow({ title: 'kept', show: false });
win.on('close', (e) => { console.log('close, keep=' + keep); if (keep) e.preventDefault(); });
win.on('closed', () => console.log('closed'));
globalThis.allow = () => { keep = false; };
app.on('window-all-closed', () => console.log('window-all-closed'));
"#;

fn log(app: &App) -> Vec<String> {
    app.console()
}

#[test]
fn a_prevented_close_keeps_the_window_and_the_next_one_closes_it() {
    let app = App::launch_script(SCRIPT, &[]);
    assert!(app.run_until(|app| app.window("kept").is_some(), TIMEOUT));
    let window = app.window("kept").expect("the window");
    app.shell.user_close(window.id);
    assert!(app.run_until(
        |app| log(app).iter().any(|line| line == "close, keep=true"),
        TIMEOUT
    ));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !app.window("kept").expect("still listed").closed,
        "{:?}",
        log(&app)
    );
    assert!(!log(&app).iter().any(|line| line == "closed"));

    app.eval_main("allow()");
    app.shell.user_close(window.id);
    assert!(app.run_until(|app| app.window("kept").is_some_and(|w| w.closed), TIMEOUT));
    let lines = log(&app);
    let order: Vec<_> = lines
        .iter()
        .filter(|line| !line.starts_with("main says"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        order,
        [
            "close, keep=true",
            "close, keep=false",
            "closed",
            "window-all-closed"
        ]
    );
}

#[test]
fn closing_a_window_twice_in_a_row_says_closed_once() {
    let app = App::launch_script(SCRIPT, &[]);
    assert!(app.run_until(|app| app.window("kept").is_some(), TIMEOUT));
    let window = app.window("kept").expect("the window");
    app.eval_main("allow()");
    app.shell.user_close(window.id);
    app.shell.user_close(window.id);
    assert!(app.run_until(|app| app.window("kept").is_some_and(|w| w.closed), TIMEOUT));
    std::thread::sleep(Duration::from_millis(200));
    let closed = log(&app).iter().filter(|line| *line == "closed").count();
    assert_eq!(closed, 1);
}
