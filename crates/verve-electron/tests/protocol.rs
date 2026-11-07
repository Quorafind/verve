//! `protocol`: what a session says about itself, `protocol.handle` with `net.fetch` of files and
//! of a file inside `app.asar`, and what a handler that misbehaves does to a `net.fetch`. The
//! expected values are frozen from Electron 39.8.10 (probes in the P16 scratch folder; the source
//! of each is named where it is not obvious).

#![allow(clippy::unwrap_used)]

mod support;

use serde_json::{Value, json};
use support::{App, TIMEOUT};

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
