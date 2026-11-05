//! The headless shell as a test sees it: what it lays out, what a person's click and window
//! resize do to a page, and how the app's console and realms are read.

mod support;

use support::{App, TIMEOUT};

const PAGE: &str = r#"
const { BrowserWindow } = require('electron');
console.log('main says hi');
const win = new BrowserWindow({ title: 'page', width: 500, height: 300 });
win.loadURL('data:text/html,<div id="a">hello world</div><p id="p"><span id="s">ab</span> cd</p><button id="b">go</button>');
"#;

/// An app whose window has shown its page: the app, the window's id and its page's.
fn shown() -> (App, u32, u32) {
    let app = App::launch_script(PAGE, &[]);
    let window = app.window("page").expect("the window was made");
    let text = |app: &App| app.page_text(window.id);
    assert!(app.run_until(
        |app| text(app).as_deref() == Some("hello worldab cdgo"),
        TIMEOUT
    ));
    (app, window.id, window.contents)
}

fn rect(app: &App, contents: u32, id: &str) -> String {
    let js = format!(
        "(() => {{ const r = document.getElementById('{id}').getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; }})()"
    );
    app.eval_renderer(contents, &js)
}

#[test]
fn blocks_stack_over_the_full_width_and_text_is_sixteen_pixels_a_line() {
    let (app, _, contents) = shown();
    // The boxes arrive after the page's first view: wait for the layout, not the page.
    assert!(app.run_until(|app| rect(app, contents, "a") == "[0,0,500,16]", TIMEOUT));
    assert_eq!(rect(&app, contents, "p"), "[0,16,500,16]");
    // Eight pixels a character: "ab" is two of them.
    assert_eq!(rect(&app, contents, "s"), "[0,16,16,16]");
    // A button sits in a line like text, as wide as its label.
    assert_eq!(rect(&app, contents, "b"), "[0,32,16,16]");
}

#[test]
fn resizing_a_window_tells_the_page_and_lays_it_out_again() {
    let (app, window, contents) = shown();
    assert!(app.run_until(|app| rect(app, contents, "a") == "[0,0,500,16]", TIMEOUT));
    app.shell.resize(window, 40., 300.);
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "innerWidth") == "40",
        TIMEOUT
    ));
    // "hello world" is ten characters, 80 px, in a 40 px line: two lines.
    assert!(app.run_until(|app| rect(app, contents, "a") == "[0,0,40,32]", TIMEOUT));
    assert_eq!(app.shell.windows()[0].bounds.width, 40.);
}

#[test]
fn a_click_reaches_the_pages_listener_and_the_console_keeps_both_processes_lines() {
    let (app, window, contents) = shown();
    app.eval_renderer(
        contents,
        "document.getElementById('b').addEventListener('click', () => console.log('clicked')); 0",
    );
    app.shell.click(window, "#b");
    assert!(app.run_until(|app| app.console().contains(&"clicked".to_owned()), TIMEOUT));
    assert_eq!(app.console(), ["main says hi", "clicked"]);
}

#[test]
fn the_main_process_answers_in_json() {
    // A window keeps the app up: with none, P02's rule ends it as soon as the script has run.
    let main = "globalThis.answer = { n: 6 * 7 }; new (require('electron').BrowserWindow)();";
    let app = App::launch_script(main, &[]);
    assert_eq!(app.eval_main("answer"), r#"{"n":42}"#);
    assert_eq!(app.eval_main("process.type"), r#""browser""#);
}

#[test]
fn the_app_exit_code_is_waited_for() {
    let app = App::launch_script("setTimeout(() => process.exit(3), 10)", &[]);
    assert_eq!(app.wait_exit(TIMEOUT), 3);
}
