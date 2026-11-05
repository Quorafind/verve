//! A webContents loading a page the way a tab does, headless: the stages the main process hears
//! of (as the `PageEvent`s P13 turns into `webContents` events), the fixture apps' pages
//! rendering, and a page that is not there.

mod support;

use support::{App, TIMEOUT, fixture};
use verve_electron::msg::PageEvent;

/// What the main process heard of page `contents`, as short names.
fn heard(app: &App, contents: u32) -> Vec<String> {
    let name = |event: PageEvent| match event {
        PageEvent::DidStartLoading => "did-start-loading".to_owned(),
        PageEvent::DidNavigate { url } => format!("did-navigate {}", file_name(&url)),
        PageEvent::DomReady => "dom-ready".to_owned(),
        PageEvent::DidFinishLoad => "did-finish-load".to_owned(),
        PageEvent::DidFailLoad { code, .. } => format!("did-fail-load {code}"),
        PageEvent::TitleUpdated(title) => format!("title {title}"),
        PageEvent::FirstPaintable => "first-paintable".to_owned(),
        other => format!("{other:?}"),
    };
    app.hub
        .page_events(contents)
        .into_iter()
        .map(name)
        .collect()
}

fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

#[test]
fn electron_quick_start_loads_its_page_and_tells_each_stage_in_order() {
    let app = App::launch(&fixture("quick-start"));
    let window = app
        .window("Hello World!")
        .expect("the page titled the window");
    assert!(app.run_until(
        |app| heard(app, window.contents).contains(&"did-finish-load".to_owned()),
        TIMEOUT
    ));
    assert_eq!(
        heard(&app, window.contents),
        [
            "did-start-loading",
            "did-navigate index.html",
            "first-paintable",
            "title Hello World!",
            "dom-ready",
            "did-finish-load"
        ]
    );
    // The sheet and the script are in: the page's own script ran without a complaint.
    let page = |js: &str| app.eval_renderer(window.contents, js);
    assert_eq!(page("document.readyState"), r#""complete""#);
    assert_eq!(
        page("document.querySelector('h1').textContent"),
        r#""Hello World!""#
    );
    let errors = app.console().into_iter().filter(|l| l.contains("Error"));
    assert_eq!(errors.collect::<Vec<_>>(), Vec::<String>::new());
}

#[test]
fn a_vite_react_app_renders_into_its_root_after_load_file() {
    let app = App::launch(&fixture("react-vite"));
    let window = app
        .window("React Vite")
        .expect("the page titled the window");
    let root = "document.querySelector('#root').children.length";
    assert!(
        app.run_until(
            |app| app.eval_renderer(window.contents, root) != "0",
            TIMEOUT
        ),
        "#root stayed empty; console: {:?}",
        app.console()
    );
    assert!(
        app.run_until(
            |app| app
                .page_text(window.id)
                .is_some_and(|t| t.contains("count is 0")),
            TIMEOUT
        ),
        "the counter was not drawn; console: {:?}",
        app.console()
    );
}

/// An app with one window titled `t` that does `body` once it has it, as `win`.
fn app_with(body: &str, files: &[(&str, &str)]) -> App {
    let main = format!(
        "const {{ app, BrowserWindow }} = require('electron');
         app.whenReady().then(() => {{
           const win = new BrowserWindow({{ show: false, title: 't' }});
           {body}
         }});"
    );
    App::launch_script(&main, files)
}

#[test]
fn a_file_that_is_not_there_fails_the_load_with_minus_six() {
    let app = app_with("win.loadFile('nowhere.html');", &[]);
    let window = app.window("t").expect("a window");
    assert!(app.run_until(
        |app| heard(app, window.contents).contains(&"did-fail-load -6".to_owned()),
        TIMEOUT
    ));
    assert_eq!(
        heard(&app, window.contents),
        ["did-start-loading", "did-fail-load -6"]
    );
}

#[test]
fn a_page_without_node_integration_resolves_modules_the_way_a_browser_does() {
    let page = "<script type=module>
        for (const name of ['fs', 'node:fs', 'x']) {
          await import(name).catch((e) => (window.__log ||= []).push(name + ': ' + e.message));
        }
        window.__done = true;</script>";
    let app = app_with("win.loadFile('m.html');", &[("m.html", page)]);
    let window = app.window("t").expect("a window");
    let done = |app: &App| app.eval_renderer(window.contents, "window.__done === true") == "true";
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    assert_eq!(
        app.eval_renderer(window.contents, "JSON.stringify(window.__log)"),
        r#""[\"fs: Failed to resolve module specifier 'fs'\",\"node:fs: Failed to fetch dynamically imported module: node:fs\",\"x: Failed to resolve module specifier 'x'\"]""#
    );
}

#[test]
fn a_pages_require_resolves_against_the_folder_of_the_page() {
    let main = "const { app, BrowserWindow } = require('electron');
         app.whenReady().then(() => {
           const win = new BrowserWindow({ show: false, title: 't',
             webPreferences: { nodeIntegration: true, contextIsolation: false } });
           win.loadFile('sub/m.html');
         });";
    let page = "<title>t</title><script>window.__v = require('./helper.js').value; window.__done = true;</script>";
    let app = App::launch_script(
        main,
        &[
            ("sub/m.html", page),
            ("sub/helper.js", "exports.value = 'from-sub';"),
        ],
    );
    let window = app.window("t").expect("a window");
    let done = |app: &App| app.eval_renderer(window.contents, "window.__done === true") == "true";
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    assert_eq!(
        app.eval_renderer(window.contents, "window.__v"),
        r#""from-sub""#
    );
}
