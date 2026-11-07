//! `session`: what a session says about itself and hands out. The expected values are frozen from
//! Electron 39.8.10 (probes in the P16 scratch folder).

#![allow(clippy::unwrap_used)]

mod support;

use serde_json::{Value, json};

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
    // `webPreferences.session` picks the page's session, and `webContents.session` is that object.
    assert_eq!(out["windows"], json!([true, true]));
}
