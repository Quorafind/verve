//! A page's preload, `contextBridge`, `nodeIntegration`, `webFrame` and `webUtils`, headless:
//! what each `webPreferences` combination lets the page and its preload see, and what crosses
//! the bridge, held to what Electron 39.8.10 did (`tests/electron/apps/bridge-matrix`).

mod support;

use std::time::Duration;

use support::{App, TIMEOUT, console_of, fixture, recorded_console, written_by};
use verve_electron::msg::PageEvent;

const ENDS_WITHIN: Duration = Duration::from_secs(30);

/// An app with one window made with `prefs` (the text of the `webPreferences` object), showing
/// `index.html`, with `files` beside it. Waits until the page has loaded.
fn page(prefs: &str, files: &[(&str, &str)]) -> (App, u32) {
    let main = format!(
        r#"
        const {{ app, BrowserWindow, ipcMain }} = require('electron');
        const path = require('node:path');
        ipcMain.handle('echo', (event, value) => value);
        app.whenReady().then(() => {{
          const webPreferences = {prefs};
          const win = new BrowserWindow({{ show: false, webPreferences }});
          win.webContents.on('preload-error', (event, file, error) => {{
            console.log('preload-error', path.basename(file), error.name, error.message);
          }});
          win.loadFile(path.join(__dirname, 'index.html'));
        }});
        "#
    );
    let app = App::launch_script(&main, files);
    let window = app.window("page").expect("the page titled the window");
    let loaded = |app: &App| {
        app.hub
            .page_events(window.contents)
            .iter()
            .any(|e| matches!(e, PageEvent::DidFinishLoad))
    };
    assert!(app.run_until(loaded, TIMEOUT), "{:?}", app.console());
    let contents = window.contents;
    (app, contents)
}

fn html(body: &str) -> String {
    format!("<!doctype html><html><head><title>page</title></head><body>{body}</body></html>")
}

fn eval(app: &App, contents: u32, js: &str) -> String {
    app.eval_renderer(contents, js)
}

#[test]
fn the_default_preload_has_a_sandboxed_require_and_the_page_sees_only_its_bridge() {
    let preload = r#"
        const { contextBridge, ipcRenderer } = require('electron');
        const say = (...a) => console.log(...a);
        try { require('fs'); } catch (e) { say('fs', e.name, e.message); }
        say('events', typeof require('events').EventEmitter, typeof require('node:events'));
        say('globals', typeof process, typeof Buffer, typeof module, typeof __filename, process.sandboxed, process.type);
        contextBridge.exposeInMainWorld('api', {
          node: process.versions.node,
          echo: (text) => ipcRenderer.invoke('echo', text),
        });
    "#;
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let lines = app.console();
    assert!(
        lines.contains(&"fs Error module not found: fs".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"events function function".to_owned()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"globals object function object undefined true renderer".to_owned()),
        "{lines:?}"
    );
    let page = |js: &str| eval(&app, contents, js);
    assert_eq!(
        page(
            "[typeof require, typeof process, typeof Buffer, typeof module, typeof global, typeof api].join()"
        ),
        r#""undefined,undefined,undefined,undefined,undefined,object""#
    );
    assert_eq!(
        page("typeof api.echo + ' ' + typeof api.node"),
        r#""function string""#
    );
    let echoed = app.run_page(contents, "return await api.echo({ a: [1, 2] })", TIMEOUT);
    assert_eq!(echoed["value"], serde_json::json!({ "a": [1, 2] }));
}

#[test]
fn a_page_changing_a_built_in_does_not_reach_the_preload_that_made_its_bridge() {
    let preload = r#"
        const { contextBridge } = require('electron');
        contextBridge.exposeInMainWorld('api', {
          push: () => { try { return [1].push(2); } catch (e) { return 'broken: ' + e.message; } },
          same: () => Object.getPrototypeOf([]) === Array.prototype,
        });
    "#;
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let page = |js: &str| eval(&app, contents, js);
    page("Array.prototype.push = null; Object.prototype.polluted = 1; 0");
    assert_eq!(page("api.push()"), "2");
    assert_eq!(page("api.same()"), "true");
    assert_eq!(page("typeof [].push"), r#""object""#);
}

/// A page that changes its own built-ins after the preload has run, and then has values cross.
/// `options` are the other `webPreferences`.
fn built_ins_are_the_pages_own(options: &str) {
    let preload = r#"
        const { contextBridge, ipcRenderer } = require('electron');
        contextBridge.exposeInMainWorld('api', {
          make: () => ({ a: 1, sender: ipcRenderer }),
          list: () => [{ b: 2 }],
          map: () => new Map([[1, {}]]),
          take: (promise) => 'taken',
          probe: () => [typeof process.pageOnly, typeof ipcRenderer.pageOnly].join(),
        });
    "#;
    let index = html("<p>x</p>");
    let prefs = format!("{{ preload: path.join(__dirname, 'preload.js'), {options} }}");
    let (app, contents) = page(&prefs, &[("preload.js", preload), ("index.html", &index)]);
    let page = |js: &str| eval(&app, contents, js);
    // The page changes what the copy reads with, then a value crosses.
    page(
        "Object.prototype.propertyIsEnumerable = function () { globalThis.leak = this; return true; };          Object.keys = function (o) { globalThis.leak = o; return []; };          Array.from = function (o) { globalThis.leak = o; return []; }; 0",
    );
    assert_eq!(page("api.make().a"), "1");
    assert_eq!(page("api.list()[0].b"), "2");
    assert_eq!(page("api.map().get(1) !== undefined"), "true");
    assert_eq!(page("typeof leak"), r#""undefined""#);
    // A promise of the page's names the constructor to derive its promise with; whatever it
    // does, the engine stands and nothing of the preload's is handed to it.
    for (name, species) in [
        (
            "answer",
            "function (executor) { globalThis.F = executor.constructor; executor(() => {}, () => {}); }",
        ),
        ("thrown", "function () { throw new Error('species'); }"),
        ("silent", "function () {}"),
        (
            "twice",
            "function (executor) { executor(() => {}, () => {}); executor(() => {}, () => {}); }",
        ),
    ] {
        let script = format!(
            "{{ const p = Promise.resolve();              p.constructor = {{ [Symbol.species]: {species} }};              globalThis.{name} = api.take(p); }} 0"
        );
        page(&script);
        assert_eq!(page(name), r#""taken""#);
    }
    assert_eq!(
        page("typeof F === 'undefined' || F('return this')() === globalThis"),
        "true"
    );
    // A getter of the page's `Object.prototype` is not on the preload's chains.
    page(
        "for (const key of ['pageOnly']) {            Object.defineProperty(Object.prototype, key, { configurable: true,              get() { globalThis.seen = this; return () => {}; } }); } 0",
    );
    assert_eq!(page("api.probe()"), r#""undefined,undefined""#);
    assert_eq!(page("typeof seen"), r#""undefined""#);
}

#[test]
fn a_page_cannot_hand_its_own_built_ins_a_value_of_the_preload_as_this() {
    built_ins_are_the_pages_own("");
}

#[test]
fn an_unsandboxed_preload_is_as_safe_from_the_pages_built_ins() {
    built_ins_are_the_pages_own("sandbox: false");
}

#[test]
fn what_the_preload_posts_to_the_window_is_a_copy_for_the_page() {
    let preload = r#"
        const { contextBridge } = require('electron');
        contextBridge.exposeInMainWorld('api', {
          post: () => window.postMessage({ a: 1, inner: { b: 2 } }, '*'),
        });
    "#;
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let page = |js: &str| eval(&app, contents, js);
    page(
        "globalThis.heard = [];          addEventListener('message', (e) => heard.push(e.data.a, e.data.inner.b,            Object.getPrototypeOf(e.data) === Object.prototype,            e.data.constructor.constructor('return this')() === globalThis));          api.post(); 0",
    );
    assert_eq!(page("heard.join()"), r#""1,2,true,true""#);
}

#[test]
fn the_preload_is_in_a_world_of_its_own_over_the_same_document() {
    let preload = r#"
        window.addEventListener('DOMContentLoaded', () => {
          document.getElementById('mark').textContent = 'preload ' + typeof window.pageThing;
          window.fromPreload = 1;
        });
        document.addEventListener('click', () => console.log('preload heard a click'));
    "#;
    let index = html(
        r#"<p id="mark">no</p><button id="go">go</button><script>window.pageThing = 1; document.addEventListener('click', () => console.log('page heard a click'));</script>"#,
    );
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let page = |js: &str| eval(&app, contents, js);
    assert_eq!(
        page("document.getElementById('mark').textContent"),
        r#""preload undefined""#
    );
    assert_eq!(page("typeof window.fromPreload"), r#""undefined""#);
    page("document.getElementById('go').click(); 0");
    let heard = |app: &App| -> Vec<String> {
        app.console()
            .into_iter()
            .filter(|l| l.contains("heard"))
            .collect()
    };
    assert!(
        app.run_until(|app| heard(app).len() == 2, TIMEOUT),
        "{:?}",
        app.console()
    );
    assert_eq!(heard(&app), ["preload heard a click", "page heard a click"]);
}

#[test]
fn without_context_isolation_the_bridge_throws_and_the_preload_shares_the_pages_world() {
    let preload = r#"
        try { require('electron').contextBridge.exposeInMainWorld('api', {}); } catch (e) { console.log('bridge', e.name, e.message); }
        window.fromPreload = 'shared';
        console.log('this', this === window, process.contextIsolated);
    "#;
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ contextIsolation: false, preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let lines = app.console();
    assert!(
        lines.contains(
            &"bridge Error contextBridge API can only be used when contextIsolation is enabled"
                .to_owned()
        ),
        "{lines:?}"
    );
    assert!(lines.contains(&"this true false".to_owned()), "{lines:?}");
    assert_eq!(eval(&app, contents, "window.fromPreload"), r#""shared""#);
    assert_eq!(
        eval(&app, contents, "typeof process + typeof require"),
        r#""undefinedundefined""#
    );
}

#[test]
fn a_page_with_node_integration_and_no_isolation_has_node_and_the_preload_too() {
    let preload = "window.fromPreload = require('node:path').basename('/a/b.txt');";
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ nodeIntegration: true, contextIsolation: false, preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let page = |js: &str| eval(&app, contents, js);
    assert_eq!(
        page("require('path').join('a', 'b').replace('\\\\', '/')"),
        r#""a/b""#
    );
    assert_eq!(page("window.fromPreload"), r#""b.txt""#);
    assert_eq!(
        page(
            "[typeof module, typeof exports, typeof __filename, __filename.endsWith('index.html'), __filename.startsWith(__dirname), process.type, global === window, typeof Buffer, typeof setImmediate].join()"
        ),
        r#""object,undefined,string,true,true,renderer,true,function,function""#
    );
}

#[test]
fn node_integration_with_isolation_gives_node_to_the_preload_only() {
    let preload = r#"
        const fs = require('fs');
        console.log('node', typeof fs.existsSync, typeof process.versions.node, process.sandboxed, process.contextIsolated);
        require('electron').contextBridge.exposeInMainWorld('api', { ok: fs.existsSync(__filename), file: require('path').basename(__filename) });
    "#;
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ nodeIntegration: true, preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    assert!(
        app.console()
            .contains(&"node function string undefined true".to_owned()),
        "{:?}",
        app.console()
    );
    assert_eq!(
        eval(
            &app,
            contents,
            "typeof require + typeof process + api.ok + api.file"
        ),
        r#""undefinedundefinedtruepreload.js""#
    );
}

#[test]
fn a_preload_that_throws_is_reported_to_the_main_process_and_the_page_still_loads() {
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[
            ("preload.js", "throw new TypeError('preload exploded');"),
            ("index.html", &index),
        ],
    );
    assert!(
        app.run_until(
            |app| app
                .console()
                .contains(&"preload-error preload.js TypeError preload exploded".to_owned()),
            TIMEOUT
        ),
        "{:?}",
        app.console()
    );
    let lines = app.console();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("Unable to load preload script: ")),
        "{lines:?}"
    );
    assert_eq!(eval(&app, contents, "document.readyState"), r#""complete""#);
}

#[test]
fn a_preload_that_is_not_there_is_an_enoent_preload_error() {
    let index = html("<p>x</p>");
    let (app, _) = page(
        "{ preload: path.join(__dirname, 'nothere.js') }",
        &[("index.html", &index)],
    );
    assert!(
        app.run_until(
            |app| app.console().iter().any(|l| l.starts_with(
                "preload-error nothere.js Error ENOENT: no such file or directory, open "
            )),
            TIMEOUT
        ),
        "{:?}",
        app.console()
    );
}

#[test]
fn additional_arguments_are_in_the_preloads_argv() {
    let preload =
        "console.log('argv', JSON.stringify(process.argv.filter((a) => a.startsWith('--my'))));";
    let index = html("<p>x</p>");
    let (app, _) = page(
        "{ additionalArguments: ['--my-flag=1', '--my-other'], preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    assert!(
        app.console()
            .contains(&r#"argv ["--my-flag=1","--my-other"]"#.to_owned()),
        "{:?}",
        app.console()
    );
}

#[test]
fn web_frame_inserts_and_removes_css_and_runs_script_in_the_page() {
    let preload = r#"
        const { webFrame, contextBridge } = require('electron');
        let key;
        contextBridge.exposeInMainWorld('frame', {
          insert: (css, options) => (key = webFrame.insertCSS(css, options)),
          remove: () => webFrame.removeInsertedCSS(key),
          run: (code) => webFrame.executeJavaScript(code),
          zoom: (factor) => { webFrame.setZoomFactor(factor); return [webFrame.getZoomFactor(), webFrame.getZoomLevel().toFixed(4)].join(); },
        });
    "#;
    let index = html(r#"<p id="p">x</p>"#);
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let page = |js: &str| eval(&app, contents, js);
    let color = "getComputedStyle(document.getElementById('p')).color";
    let before = page(color);
    page("frame.insert('#p { color: rgb(1, 2, 3) }'); 0");
    assert_eq!(page(color), r#""rgb(1, 2, 3)""#);
    page("frame.remove(); 0");
    assert_eq!(page(color), before);
    page("frame.insert('#p { color: rgb(9, 9, 9) }', { cssOrigin: 'user' }); 0");
    assert_eq!(page(color), r#""rgb(9, 9, 9)""#);
    assert_eq!(
        app.run_page(contents, "return await frame.run('1 + 1')", TIMEOUT)["value"],
        2
    );
    assert_eq!(
        app.run_page(
            contents,
            "return await frame.run('window.fromScript = 5; Promise.resolve({ a: [1] })')",
            TIMEOUT
        )["value"],
        serde_json::json!({ "a": [1] })
    );
    assert_eq!(page("window.fromScript"), "5");
    let failed = app.run_page(
        contents,
        "try { await frame.run('throw new Error(\"js fail\")') } catch (e) { return e.message }",
        TIMEOUT,
    );
    assert!(
        failed["value"]
            .as_str()
            .unwrap()
            .starts_with("Script failed to execute"),
        "{failed}"
    );
    assert_eq!(page("frame.zoom(1.5)"), r#""1.5,2.2239""#);
}

#[test]
fn web_utils_answers_no_path_for_a_file_the_page_made() {
    let preload = r#"
        const { webUtils, contextBridge } = require('electron');
        contextBridge.exposeInMainWorld('utils', {
          path: (file) => webUtils.getPathForFile(file),
        });
    "#;
    let index = html("<p>x</p>");
    let (app, contents) = page(
        "{ preload: path.join(__dirname, 'preload.js') }",
        &[("preload.js", preload), ("index.html", &index)],
    );
    let page = |js: &str| eval(&app, contents, js);
    assert_eq!(page("utils.path(new File(['abc'], 'a.txt'))"), r#""""#);
    let bad = page("try { utils.path({}) } catch (e) { e.name + ': ' + e.message }");
    assert_eq!(
        bad,
        r#""Error: getPathForFile expected to receive a File object but one was not provided""#
    );
}

/// Cells of the matrix that Verve does not reproduce yet, with why. Each is a line of the
/// recording; the rest must match.
const KNOWN_DIFFERENCES: &[(&str, &str)] = &[
    // An `Event` keeps its state in Rust, so it has no own `isTrusted`.
    ("returned event ", "an Event has no own isTrusted"),
    ("received event ", "an Event has no own isTrusted"),
    // A view over part of a buffer crosses as the part: the offset into the whole is not kept.
    (
        "returned typed array view ",
        "a typed array view is copied without the buffer around it",
    ),
    (
        "received typed array view ",
        "a typed array view is copied without the buffer around it",
    ),
    (
        "direct typed array view ",
        "a typed array view is copied without the buffer around it",
    ),
];

#[test]
fn the_bridge_matrix_gives_what_electron_gave() {
    let app = App::launch(&fixture("bridge-matrix"));
    let code = app.exit_code(ENDS_WITHIN);
    let actual = written_by(&console_of(&app), "renderer");
    let recorded = written_by(&recorded_console("bridge-matrix"), "renderer");
    // The recording was made on Windows: the platform it names is only that one's.
    let elsewhere = |line: &String| !cfg!(windows) && line.contains(" win32 ");
    let skip = |line: &String| {
        elsewhere(line)
            || KNOWN_DIFFERENCES
                .iter()
                .any(|(start, _)| line.starts_with(start))
    };
    let mut wrong = Vec::new();
    for (at, expected) in recorded.iter().enumerate().filter(|(_, l)| !skip(l)) {
        let found = actual.get(at).filter(|l| *l == expected);
        if found.is_none() {
            wrong.push(format!(
                "#{at}\n  expected: {expected}\n  actual:   {:?}",
                actual.get(at)
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} cells differ:\n{}",
        wrong.len(),
        recorded.len(),
        wrong.join("\n")
    );
    assert_eq!(actual.len(), recorded.len(), "the number of lines");
    assert_eq!(code, Some(0));
}
