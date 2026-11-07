//! `protocol`: what a session says about itself, `protocol.handle` and the older calls with
//! `net.fetch`, and a page loaded from a handler. The expected values are frozen from Electron
//! 39.8.10 (probes in the P16 scratch folder; the source of each is named where it is not
//! obvious).

#![allow(clippy::unwrap_used)]

mod support;

use serde_json::{Value, json};
use support::{App, TIMEOUT};

const INDEX: &str = "<!doctype html><title>Bundle</title><link rel=stylesheet href=./a.css>\
<script src=./a.js></script><body>hello";

/// The files of the site every test serves.
fn site() -> Vec<(&'static str, &'static str)> {
    vec![
        ("site/index.html", INDEX),
        ("site/a.css", "body { color: red }"),
        ("site/a.js", "window.fromScript = 'ran';"),
        ("site/data.json", r#"{"x":1}"#),
    ]
}

/// Waits for the page titled `Bundle`: its contents id.
fn bundle(app: &App) -> u32 {
    app.window("Bundle")
        .unwrap_or_else(|| panic!("no window; console: {:?}", app.console()))
        .contents
}

/// A script run in the main process after the app is ready; the answer is `globalThis.out`.
fn main_says(script: &str) -> Value {
    let main = format!(
        "const {{ app, protocol, net, session }} = require('electron');
         globalThis.out = {{}};
         const note = (key, value) => {{ out[key] = value; }};
         const fail = (e) => String(e && e.message);
         app.whenReady().then(async () => {{ {script}
           note('done', true); }}).catch((e) => note('threw', String(e && e.stack)));"
    );
    support::report_of(&main, &[])
}

#[test]
fn a_handler_that_misbehaves_fails_net_fetch_with_the_net_error_electron_gives() {
    let out = main_says(
        "protocol.handle('thrower', () => { throw new Error('boom'); });
         protocol.handle('rejecter', () => Promise.reject(new Error('no')));
         protocol.handle('stringer', () => 'not a response');
         protocol.handle('errorer', () => Response.error());
         for (const scheme of ['thrower', 'rejecter', 'stringer', 'errorer']) {
           try { await net.fetch(scheme + '://h/'); note(scheme, 'resolved'); } catch (e) { note(scheme, [e.name, e.message]); }
         }
         try { await net.fetch('nosuch://h/'); } catch (e) { note('unknown', [e.name, e.message]); }
         try { await net.fetch('file:///verve-p16-surely-missing/file.txt'); } catch (e) { note('missing', [e.name, e.message]); }",
    );
    // `lib/browser/api/protocol.ts`: a throw, a rejection and a non-Response are -9
    // (ERR_UNEXPECTED), `Response.error()` is -2 (ERR_FAILED); probe1 for the rest.
    for scheme in ["thrower", "rejecter", "stringer"] {
        assert_eq!(
            out[scheme],
            json!(["Error", "net::ERR_UNEXPECTED"]),
            "{scheme}"
        );
    }
    assert_eq!(out["errorer"], json!(["Error", "net::ERR_FAILED"]));
    assert_eq!(
        out["unknown"],
        json!(["Error", "net::ERR_UNKNOWN_URL_SCHEME"])
    );
    assert_eq!(out["missing"], json!(["Error", "net::ERR_FILE_NOT_FOUND"]));
}

#[test]
fn a_session_knows_what_it_is_and_hands_out_the_same_object() {
    let out = main_says(
        "const memory = session.fromPartition('memtest'), kept = session.fromPartition('persist:per');
         note('memory', [memory.isPersistent(), memory.getStoragePath()]);
         note('kept', [kept.isPersistent(), kept.getStoragePath(), app.getPath('userData')]);
         note('same', [session.fromPartition('memtest') === memory, session.fromPartition('') === session.defaultSession]);
         note('default', [session.defaultSession.isPersistent(), session.defaultSession.getStoragePath() === app.getPath('userData')]);
         note('ua', session.defaultSession.getUserAgent().includes('Electron/'));
         session.defaultSession.setUserAgent('Probe/1');
         note('set', session.defaultSession.getUserAgent());
         try { session.fromPartition(5); } catch (e) { note('bad', fail(e)); }
         note('protocol', typeof session.defaultSession.protocol.handle);
         const { BrowserWindow } = require('electron');
         const chosen = new BrowserWindow({ show: false, webPreferences: { session: kept } });
         const plain = new BrowserWindow({ show: false });
         note('windows', [chosen.webContents.session === kept, plain.webContents.session === session.defaultSession]);",
    );
    assert_eq!(out["memory"], json!([false, null]));
    let user_data = out["kept"][2].as_str().unwrap().to_owned();
    let kept_path = std::path::Path::new(&user_data)
        .join("Partitions")
        .join("per");
    assert_eq!(
        out["kept"],
        json!([true, kept_path.to_string_lossy(), user_data])
    );
    assert_eq!(out["same"], json!([true, true]));
    assert_eq!(out["default"], json!([true, true]));
    assert_eq!(out["ua"], true);
    assert_eq!(out["set"], "Probe/1");
    assert_eq!(
        out["bad"],
        "Error processing argument at index 0, conversion failure from "
    );
    assert_eq!(out["protocol"], "function");
    // `webPreferences.session` picks the page's session, and `webContents.session` is that object.
    assert_eq!(out["windows"], json!([true, true]));
}

#[test]
fn a_file_handler_reads_files_by_passing_over_the_handlers_and_a_stream_protocol_takes_a_stream() {
    let out = main_says(
        "const { pathToFileURL } = require('url');
         let asked = 0;
         protocol.handle('file', (req) => { asked++; return net.fetch(req, { bypassCustomProtocolHandlers: true }); });
         const own = await net.fetch(pathToFileURL(__filename).toString());
         const text = await own.text();
         note('file', [own.status, own.statusText, text === require('fs').readFileSync(__filename, 'utf8'), asked]);
         protocol.registerStreamProtocol('stream', (req, callback) => {
           const readable = new (require('stream').PassThrough)();
           readable.end('abc');
           callback(readable);
         });
         note('stream', await (await net.fetch('stream://h/')).text());",
    );
    // Electron 39.8.10 (probe4): with a `file` handler that passes over the handlers, `net.fetch`
    // of a file answers 200 `OK` and its text, and the handler is asked once for each fetch.
    assert_eq!(out["file"], json!([200, "OK", true, 1]));
    assert_eq!(out["stream"], "abc");
}

#[test]
fn a_file_handler_answers_for_the_page_load_file_makes() {
    let main = "const { app, BrowserWindow, protocol, net } = require('electron');
        globalThis.seen = [];
        app.whenReady().then(() => {
          protocol.handle('file', (req) => {
            seen.push(new URL(req.url).pathname.split('/').pop());
            return net.fetch(req, { bypassCustomProtocolHandlers: true });
          });
          const win = new BrowserWindow({ show: false });
          win.loadFile('site/index.html');
        });";
    let app = App::launch_script(main, &site());
    let contents = bundle(&app);
    let done = |app: &App| app.eval_renderer(contents, "window.fromScript") == r#""ran""#;
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    let seen: Value = serde_json::from_str(&app.eval_main("globalThis.seen")).unwrap();
    for name in ["index.html", "a.js", "a.css"] {
        assert!(seen.as_array().unwrap().contains(&json!(name)), "{seen}");
    }
}

#[test]
fn net_fetch_reads_a_file_inside_app_asar_through_the_apps_filesystem() {
    let main = "const { app, protocol, net } = require('electron');
        const { pathToFileURL } = require('url'); const path = require('path');
        globalThis.out = {};
        app.whenReady().then(async () => {
          protocol.handle('packed', (req) => net.fetch(
            pathToFileURL(path.join(__dirname, 'app.asar', new URL(req.url).pathname)).toString()));
          const direct = await net.fetch(pathToFileURL(path.join(__dirname, 'app.asar', 'site/data.json')).toString());
          out.direct = [direct.status, await direct.text()];
          const handled = await net.fetch('packed://host/site/data.json');
          out.handled = [handled.status, handled.headers.get('content-type'), await handled.text()];
          try { await net.fetch(pathToFileURL(path.join(__dirname, 'app.asar', 'site/none.json')).toString()); }
          catch (e) { out.missing = e.message; }
          out.done = true;
        }).catch((e) => { out.threw = String(e && e.stack); });";
    let app = App::launch_script_with_asar(main, &[("site/data.json", r#"{"x":1}"#)]);
    assert!(
        app.run_until(
            |app| app.eval_main("globalThis.out.done === true || !!globalThis.out.threw") == "true",
            TIMEOUT
        ),
        "console: {:?}",
        app.console()
    );
    let out: Value = serde_json::from_str(&app.eval_main("globalThis.out")).unwrap();
    assert_eq!(out["direct"], json!([200, r#"{"x":1}"#]));
    assert_eq!(
        out["handled"],
        json!([200, "application/json", r#"{"x":1}"#])
    );
    assert_eq!(out["missing"], "net::ERR_FILE_NOT_FOUND");
}
