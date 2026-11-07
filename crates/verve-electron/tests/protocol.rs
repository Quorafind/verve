//! `protocol`: a site served from a custom scheme (`protocol.handle` with `net.fetch` of files, the
//! older `registerFileProtocol`), what `registerSchemesAsPrivileged` gives a scheme (an origin,
//! a secure context, `fetch`), an answer streamed to a page a chunk at a time and the
//! errors of the module. The expected values are frozen from Electron 39.8.10 (probes in the
//! P16 scratch folder; the source of each is named where it is not obvious).

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

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

/// An app whose script is `setup` then one hidden window at `app://bundle/index.html`, with the
/// `app` scheme registered as `privileges` says.
fn site_app(privileges: &str, setup: &str) -> App {
    let main = format!(
        "const {{ app, BrowserWindow, protocol, net }} = require('electron');
         const {{ pathToFileURL }} = require('url'); const path = require('path');
         protocol.registerSchemesAsPrivileged([{{ scheme: 'app', privileges: {privileges} }}]);
         globalThis.served = (req) => net.fetch(pathToFileURL(path.join(__dirname, 'site', new URL(req.url).pathname)).toString());
         app.whenReady().then(() => {{
           {setup}
           const win = new BrowserWindow({{ show: false }});
           win.loadURL('app://bundle/index.html');
         }});"
    );
    App::launch_script(&main, &site())
}

const FULL: &str = "{ standard: true, secure: true, supportFetchAPI: true }";

/// Waits for the page titled `Bundle`: its contents id.
fn bundle(app: &App) -> u32 {
    app.window("Bundle")
        .unwrap_or_else(|| panic!("no window; console: {:?}", app.console()))
        .contents
}

fn page(app: &App, contents: u32, js: &str) -> Value {
    serde_json::from_str(&app.eval_renderer(contents, js)).unwrap()
}

#[test]
fn a_site_is_served_from_a_custom_scheme_by_a_handler_that_fetches_files() {
    let app = site_app(FULL, "protocol.handle('app', globalThis.served);");
    let contents = bundle(&app);
    let done = |app: &App| app.eval_renderer(contents, "document.readyState") == r#""complete""#;
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    let facts = page(
        &app,
        contents,
        "[location.origin, isSecureContext, window.fromScript, getComputedStyle(document.body).color]",
    );
    // `std://bundle` and `true`: a standard, secure scheme's page (probe3, scheme `std`).
    assert_eq!(
        facts,
        json!(["app://bundle", true, "ran", "rgb(255, 0, 0)"])
    );
    let fetched = app.run_page(
        contents,
        "const r = await fetch('./data.json'); return [r.url, r.type, await r.json()];",
        TIMEOUT,
    );
    assert_eq!(
        fetched["value"],
        json!(["app://bundle/data.json", "basic", {"x": 1}])
    );
}

#[test]
fn the_older_registerfileprotocol_serves_the_same_site_and_warns_of_nothing() {
    let setup = "globalThis.warned = [];
        process.on('warning', (w) => warned.push(w.name + ': ' + w.message));
        protocol.registerFileProtocol('app', (req, callback) =>
        callback(path.join(__dirname, 'site', new URL(req.url).pathname)));";
    let app = site_app(FULL, setup);
    let contents = bundle(&app);
    let done = |app: &App| app.eval_renderer(contents, "window.fromScript") == r#""ran""#;
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    let fetched = app.run_page(
        contents,
        "return await (await fetch('./data.json')).text();",
        TIMEOUT,
    );
    assert_eq!(fetched["value"], json!(r#"{"x":1}"#));
    // Electron 39.8.10 emits no deprecation warning for these calls (observed with
    // `process.on('warning')`; `protocol.ts` has none).
    assert_eq!(app.eval_main("globalThis.warned"), "[]");
}

#[test]
fn a_file_handler_does_not_let_a_page_without_node_read_files() {
    let app = site_app(
        FULL,
        "protocol.handle('app', globalThis.served);
         protocol.handle('file', (req) => net.fetch(req, { bypassCustomProtocolHandlers: true }));
         globalThis.target = require('url').pathToFileURL(path.join(__dirname, 'site', 'data.json')).toString();",
    );
    let contents = bundle(&app);
    let target: Value = serde_json::from_str(&app.eval_main("globalThis.target")).unwrap();
    let target = target.as_str().unwrap();
    // Electron 39.8.10 (probe5): with a `file` handler that passes over the handlers, an `app://`
    // page still fails to `fetch`, `XMLHttpRequest` and load a script of a `file:` address.
    let asked = format!(
        "const out = {{}};
         try {{ await fetch({target:?}); out.fetch = 'read'; }} catch (e) {{ out.fetch = e.name; }}
         out.xhr = await new Promise((done) => {{
           const q = new XMLHttpRequest(); q.open('GET', {target:?});
           q.onload = () => done('read'); q.onerror = () => done('error'); q.send();
         }});
         out.script = await new Promise((done) => {{
           const s = document.createElement('script'); s.src = {target:?};
           s.onload = () => done('load'); s.onerror = () => done('error'); document.head.appendChild(s);
         }});
         return out;"
    );
    assert_eq!(
        app.run_page(contents, &asked, TIMEOUT)["value"],
        json!({"fetch": "TypeError", "xhr": "error", "script": "error"}),
        "console: {:?}",
        app.console()
    );
}

#[test]
fn a_handler_streams_a_megabyte_and_a_reader_that_pauses_stops_its_producer() {
    let setup = "globalThis.pulls = 0;
        protocol.handle('app', (req) => {
          if (new URL(req.url).pathname !== '/big') return globalThis.served(req);
          let sent = 0;
          const body = new ReadableStream({ pull(controller) {
            pulls++;
            if (sent === 16) return controller.close();
            controller.enqueue(new Uint8Array(65536).fill(++sent));
          } });
          return new Response(body, { headers: { 'content-type': 'application/octet-stream' } });
        });";
    let app = site_app(FULL, setup);
    let contents = bundle(&app);
    // Read the first chunk and stop: the handler's stream was pulled when it was made, and once
    // for each of the two chunks the page's stream asked for (it pulls when it is made and again
    // when the one read empties it), so three times and not sixteen.
    let first = app.run_page(
        contents,
        "globalThis.reader = (await fetch('app://bundle/big')).body.getReader();
         const first = await reader.read(); return [first.value.length, first.value[0]];",
        TIMEOUT,
    );
    assert_eq!(first["value"], json!([65536, 1]));
    std::thread::sleep(Duration::from_millis(400));
    let paused: u32 = app.eval_main("globalThis.pulls").parse().unwrap();
    assert_eq!(paused, 3, "pulls while the reader waited");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(app.eval_main("globalThis.pulls"), paused.to_string());
    // Reading on gets all of it, in order.
    let rest = app.run_page(
        contents,
        "let total = 65536, last = 1;
         for (;;) { const { value, done } = await reader.read(); if (done) break; total += value.length; last = value[0]; }
         return [total, last];",
        TIMEOUT,
    );
    assert_eq!(rest["value"], json!([1_048_576, 16]));
}

#[test]
fn fetch_of_a_scheme_without_supportfetchapi_rejects_but_xhr_and_page_loads_do_not() {
    let app = site_app(
        "{ standard: true, secure: true }",
        "protocol.handle('app', globalThis.served);",
    );
    let contents = bundle(&app);
    let done = |app: &App| app.eval_renderer(contents, "window.fromScript") == r#""ran""#;
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    let tried = app.run_page(
        contents,
        "let fetched; try { await fetch('./data.json'); fetched = 'ok'; } catch (e) { fetched = String(e); }
         const xhr = await new Promise((resolve) => { const q = new XMLHttpRequest(); q.open('GET', './data.json');
           q.onload = () => resolve(q.responseText); q.onerror = () => resolve('xhr error'); q.send(); });
         return [fetched, xhr];",
        TIMEOUT,
    );
    // `TypeError: Failed to fetch` for `fetch`, the body for XHR (probe3, scheme `nofetch`).
    assert_eq!(
        tried["value"],
        json!(["TypeError: Failed to fetch", r#"{"x":1}"#])
    );
}

#[test]
fn a_scheme_that_is_not_standard_has_an_opaque_origin() {
    let app = site_app(
        "{ secure: true, supportFetchAPI: true }",
        "protocol.handle('app', globalThis.served);",
    );
    let contents = bundle(&app);
    let done = |app: &App| app.eval_renderer(contents, "document.readyState") == r#""complete""#;
    assert!(app.run_until(done, TIMEOUT), "console: {:?}", app.console());
    assert_eq!(page(&app, contents, "location.origin"), json!("null"));
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
fn the_module_throws_electrons_errors_and_answers_what_is_handled() {
    let out = main_says(
        "try { protocol.registerSchemesAsPrivileged([{ scheme: 'late' }]); } catch (e) { note('late', fail(e)); }
         protocol.handle('std', () => new Response('x'));
         note('handled', [protocol.isProtocolHandled('std'), protocol.isProtocolHandled('nothing'),
                          protocol.isProtocolHandled('https'), protocol.isProtocolHandled('file')]);
         try { protocol.handle('std', () => new Response('y')); } catch (e) { note('twice', fail(e)); }
         protocol.handle('https', () => new Response('z'));
         note('intercepted', protocol.isProtocolHandled('https'));
         protocol.unhandle('std');
         note('unhandled', protocol.isProtocolHandled('std'));
         try { protocol.unhandle('std'); } catch (e) { note('unhandle-twice', fail(e)); }
         protocol.handle('UPPER', () => new Response('up'));
         note('case', protocol.isProtocolHandled('upper'));",
    );
    // Electron 39.8.10, probe1 and `lib/browser/api/protocol.ts`.
    assert_eq!(
        out["late"],
        "protocol.registerSchemesAsPrivileged should be called before app is ready"
    );
    assert_eq!(out["handled"], json!([true, false, false, false]));
    assert_eq!(out["twice"], "Failed to register protocol: std");
    assert_eq!(out["intercepted"], true);
    assert_eq!(out["unhandled"], false);
    assert_eq!(out["unhandle-twice"], "Failed to unhandle protocol: std");
    assert_eq!(out["case"], false);
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
fn a_page_without_node_cannot_fetch_a_file() {
    let app = site_app(
        FULL,
        "protocol.handle('app', globalThis.served);
         globalThis.target = require('url').pathToFileURL(path.join(__dirname, 'site', 'data.json')).toString();",
    );
    let contents = bundle(&app);
    let target: Value = serde_json::from_str(&app.eval_main("globalThis.target")).unwrap();
    let target = target.as_str().unwrap();
    let asked =
        format!("try {{ await fetch({target:?}); return 'read'; }} catch (e) {{ return e.name; }}");
    assert_eq!(
        app.run_page(contents, &asked, TIMEOUT)["value"],
        json!("TypeError"),
        "a page may not read a file by itself; console: {:?}",
        app.console()
    );
}

/// A page without `nodeIntegration` has no Node globals, and still reaches any host: Chromium's
/// renderer does, and a page's `fetch` of an http address is the network's, not Node's. The
/// server is the main process's own, on a port the system picked.
#[test]
fn a_page_without_node_reaches_a_server_on_this_machine() {
    let app = site_app(
        FULL,
        "protocol.handle('app', globalThis.served);
         const server = require('http').createServer((request, response) => {
           response.setHeader('access-control-allow-origin', '*');
           response.end('reached');
         });
         server.listen(0, '127.0.0.1', () => { globalThis.port = server.address().port; });",
    );
    let contents = bundle(&app);
    assert!(
        app.run_until(
            |app| app.eval_main("globalThis.port") != "undefined",
            TIMEOUT
        ),
        "console: {:?}",
        app.console()
    );
    let port = app.eval_main("globalThis.port");
    let asked = format!(
        "const reply = await fetch('http://127.0.0.1:{port}/');          return [typeof process, typeof Buffer, await reply.text()];"
    );
    assert_eq!(
        app.run_page(contents, &asked, TIMEOUT)["value"],
        json!(["undefined", "undefined", "reached"]),
        "console: {:?}",
        app.console()
    );
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

#[test]
fn an_xhr_to_a_custom_scheme_works_from_a_standard_page_and_fails_from_one_without_an_origin() {
    // Electron 39.8.10 (probe3): `std` and `plain` pages read their own scheme with an
    // XMLHttpRequest; a page of `bare` (not standard, origin `null`) gets an error.
    let xhr = "return await new Promise((resolve) => { const q = new XMLHttpRequest();
        q.open('GET', './data.json'); q.onload = () => resolve(q.responseText);
        q.onerror = () => resolve('error'); q.send(); });";
    for (privileges, wanted) in [(FULL, r#"{"x":1}"#), ("{ supportFetchAPI: true }", "error")] {
        let app = site_app(privileges, "protocol.handle('app', globalThis.served);");
        let contents = bundle(&app);
        let loaded =
            |app: &App| app.eval_renderer(contents, "document.readyState") == r#""complete""#;
        assert!(
            app.run_until(loaded, TIMEOUT),
            "console: {:?}",
            app.console()
        );
        let answer = app.run_page(contents, xhr, TIMEOUT);
        assert_eq!(answer["value"], json!(wanted), "{privileges}");
    }
}
