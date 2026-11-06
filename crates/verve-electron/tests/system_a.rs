//! The system modules of the main process (`nativeImage`, `clipboard`, `dialog`, `screen`,
//! `nativeTheme`, `shell`, `safeStorage`, `powerSaveBlocker`, `systemPreferences`) on a headless
//! shell, held to what Electron 39.8.10 answered on Windows (`tests/electron/system-a/expected`,
//! made by the probes in `tests/electron/system-a/observe`).
//!
//! Nothing here opens a dialog, a notification, a browser or Explorer: dialogs are answered by
//! the headless shell's presets, and the tests that touch the real clipboard or the recycle bin
//! are `#[ignore]`d and run by hand.
#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;

use serde_json::Value;
use support::{App, TIMEOUT};
use verve_electron::msg::{DialogResult, DialogSpec, Display, Rect};

/// A file of `tests/electron/system-a`.
fn system_a(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/system-a");
    root.join(name)
}

/// What Electron answered, frozen: one of the probes' outputs.
fn expected(probe: &str) -> Value {
    let text = std::fs::read_to_string(system_a(&format!("expected/{probe}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// The JSON of `js`, a JavaScript expression, in the main process of an app that has
/// `electron` as a global. A promise is waited for.
fn ask(app: &App, js: &str) -> Value {
    let program = format!(
        "globalThis.answer = undefined; Promise.resolve({js}).then(value => {{ globalThis.answer = {{ value: value === undefined ? null : value }}; }}, error => {{ globalThis.answer = {{ threw: [error.name, error.message] }}; }}); 1"
    );
    app.eval_main(&program);
    let read = |app: &App| -> Value {
        serde_json::from_str(&app.eval_main("globalThis.answer")).unwrap_or(Value::Null)
    };
    app.run_until(|app| !read(app).is_null(), TIMEOUT);
    let answer = read(app);
    match answer.get("value") {
        Some(value) => value.clone(),
        None => panic!("{js} threw {answer}"),
    }
}

fn launch() -> App {
    let app = App::launch_script(
        "globalThis.electron = require('electron'); globalThis.fs = require('fs');",
        &[],
    );
    assert!(app.run_until(
        |app| app.eval_main("typeof globalThis.electron") == "\"object\"",
        TIMEOUT
    ));
    app
}

/// `JSON.stringify` of a call that throws, as `[name, message]`.
const CATCH: &str = "(call) => { try { call(); return 'did not throw'; } catch (e) { return [e.name, e.message]; } }";

#[test]
fn a_native_image_answers_what_electron_answers_for_the_same_picture() {
    let app = launch();
    let want = expected("native-image");
    let path =
        serde_json::to_string(&system_a("fixtures/sample.png").display().to_string()).unwrap();
    let got = ask(
        &app,
        &format!(
            r"(() => {{
  const {{ nativeImage }} = electron;
  const hex = (b) => Buffer.from(b).toString('hex');
  const image = nativeImage.createFromPath({path});
  const out = {{ size: image.getSize(), empty: image.isEmpty(), aspect: image.getAspectRatio(), scaleFactors: image.getScaleFactors() }};
  out.bitmap = hex(image.toBitmap());
  out.bitmapGet = hex(image.getBitmap());
  out.dataURLPrefix = image.toDataURL().slice(0, 30);
  out.pngRoundTripBitmap = hex(nativeImage.createFromBuffer(image.toPNG()).toBitmap());
  out.jpegSize = nativeImage.createFromBuffer(image.toJPEG(90)).getSize();
  out.fromDataURL = hex(nativeImage.createFromDataURL(image.toDataURL()).toBitmap());
  const rebuilt = nativeImage.createFromBitmap(image.toBitmap(), {{ width: 4, height: 2 }});
  out.fromBitmap = {{ size: rebuilt.getSize(), bitmap: hex(rebuilt.toBitmap()) }};
  const scaled = nativeImage.createFromBitmap(image.toBitmap(), {{ width: 4, height: 2, scaleFactor: 2 }});
  out.fromBitmapScaled = {{ size: scaled.getSize(), at2: scaled.getSize(2), factors: scaled.getScaleFactors() }};
  const cropped = image.crop({{ x: 1, y: 0, width: 2, height: 2 }});
  out.crop = {{ size: cropped.getSize(), bitmap: hex(cropped.toBitmap()) }};
  out.cropPartlyOutside = image.crop({{ x: 3, y: 1, width: 5, height: 5 }}).getSize();
  out.cropAllOutside = image.crop({{ x: 10, y: 10, width: 5, height: 5 }}).getSize();
  const resized = image.resize({{ width: 8, height: 4, quality: 'best' }});
  out.resize = {{ size: resized.getSize(), len: resized.toBitmap().length }};
  out.resizeWidthOnly = image.resize({{ width: 8 }}).getSize();
  out.resizeNone = image.resize({{}}).getSize();
  const empty = nativeImage.createEmpty();
  out.emptyImage = {{ empty: empty.isEmpty(), size: empty.getSize(), png: hex(empty.toPNG()), jpeg: hex(empty.toJPEG(80)), bitmap: hex(empty.toBitmap()), dataURL: empty.toDataURL(), aspect: empty.getAspectRatio(), factors: empty.getScaleFactors() }};
  out.badBuffer = nativeImage.createFromBuffer(Buffer.from('not an image')).isEmpty();
  out.missingPath = nativeImage.createFromPath('nope.png').isEmpty();
  out.badDataURL = nativeImage.createFromDataURL('data:image/png;base64,AAAA').isEmpty();
  return out;
}})()"
        ),
    );
    for (key, value) in got.as_object().unwrap() {
        assert_eq!(value, &want[key], "{key}");
    }
}

#[test]
fn a_native_image_loads_the_files_beside_it_that_say_a_scale_factor() {
    let app = launch();
    let want = expected("native-image");
    let dir = |name: &str| {
        serde_json::to_string(&system_a(&format!("fixtures/{name}")).display().to_string()).unwrap()
    };
    let (multi, solo) = (dir("a.png"), dir("solo@2x.png"));
    let got = ask(
        &app,
        &format!(
            r"(() => {{
  const {{ nativeImage }} = electron;
  const hex = (b) => Buffer.from(b).toString('hex');
  const multi = nativeImage.createFromPath({multi});
  const solo = nativeImage.createFromPath({solo});
  const added = nativeImage.createEmpty();
  added.addRepresentation({{ scaleFactor: 2, width: 8, height: 4, buffer: fs.readFileSync({solo}) }});
  return {{
    multi: {{ size: multi.getSize(), factors: multi.getScaleFactors(), at2: multi.getSize(2), bitmap1: hex(multi.toBitmap()).slice(0, 16), bitmap2: hex(multi.toBitmap({{ scaleFactor: 2 }})).slice(0, 16), len2: multi.toBitmap({{ scaleFactor: 2 }}).length }},
    solo: {{ size: solo.getSize(), factors: solo.getScaleFactors(), len1: solo.toBitmap().length }},
    added: {{ empty: added.isEmpty(), size: added.getSize(), factors: added.getScaleFactors() }},
  }};
}})()"
        ),
    );
    for (key, value) in got.as_object().unwrap() {
        assert_eq!(value, &want[key], "{key}");
    }
}

#[test]
fn create_from_buffer_reads_a_bitmap_only_when_it_is_given_a_size() {
    let app = launch();
    let got = ask(
        &app,
        "(() => { const { nativeImage } = electron; const sized = nativeImage.createFromBuffer(Buffer.alloc(4), { width: 1, height: 1 }); return { empty: nativeImage.createFromBuffer(Buffer.alloc(0)).isEmpty(), zero: nativeImage.createFromBuffer(Buffer.alloc(0), { width: 0, height: 0 }).isEmpty(), unsized: nativeImage.createFromBuffer(Buffer.alloc(4)).isEmpty(), sized: sized.isEmpty(), size: sized.getSize() }; })()",
    );
    assert_eq!(got["empty"], true);
    assert_eq!(got["zero"], true);
    assert_eq!(got["unsized"], true);
    assert_eq!(got["sized"], false);
    assert_eq!(got["size"], serde_json::json!({ "width": 1, "height": 1 }));
}

#[test]
fn native_image_arguments_fail_as_electrons_do() {
    let app = launch();
    let want = expected("native-image");
    let got = ask(
        &app,
        &format!(
            r"(() => {{
  const {{ nativeImage }} = electron;
  const image = nativeImage.createEmpty();
  const catches = {CATCH};
  return {{
    createFromBuffer: catches(() => nativeImage.createFromBuffer('x')),
    createFromBufferNoArg: catches(() => nativeImage.createFromBuffer()),
    createFromPath: catches(() => nativeImage.createFromPath(1)),
    createFromBitmapShort: catches(() => nativeImage.createFromBitmap(Buffer.alloc(4), {{ width: 2, height: 2 }})),
    createFromBitmapNoOpts: catches(() => nativeImage.createFromBitmap(Buffer.alloc(4))),
    resizeNoArg: catches(() => image.resize()),
    cropNoArg: catches(() => image.crop()),
    toJPEGNoArg: catches(() => image.toJPEG()),
  }};
}})()"
        ),
    );
    for (key, value) in got.as_object().unwrap() {
        let frozen = &want[format!("call_{key}")];
        assert_eq!(
            value,
            &serde_json::json!([frozen["name"], frozen["message"]]),
            "{key}"
        );
    }
}

// ---------- dialog ----------

/// `Promise`-or-value `js` in an app with a hidden window, whose page counts timer ticks.
fn launch_with_window() -> App {
    let script = "const { app, BrowserWindow } = require('electron'); globalThis.electron = require('electron'); \
        app.whenReady().then(() => { globalThis.win = new BrowserWindow({ show: false }); \
        win.loadURL('data:text/html,<script>globalThis.ticks = 0; setInterval(() => { globalThis.ticks++; }, 5);</script>'); });";
    let app = App::launch_script(script, &[]);
    assert!(app.run_until(|app| !app.hub.windows().is_empty(), TIMEOUT));
    app
}

#[test]
fn a_dialog_is_answered_from_the_shells_presets_and_resolves_as_electron_does() {
    let app = launch_with_window();
    app.shell.queue_dialog_answer(DialogResult::Open {
        canceled: false,
        file_paths: vec!["C:/a.txt".into(), "C:/b.txt".into()],
    });
    app.shell.queue_dialog_answer(DialogResult::Save {
        canceled: false,
        file_path: Some("C:/out.txt".into()),
    });
    app.shell.queue_dialog_answer(DialogResult::MessageBox {
        response: 1,
        checkbox_checked: true,
    });
    let got = ask(
        &app,
        "(async () => { const { dialog } = electron; return [\
            await dialog.showOpenDialog(win, { properties: ['openFile', 'multiSelections'] }),\
            await dialog.showSaveDialog({ defaultPath: 'out.txt' }),\
            await dialog.showMessageBox(win, { message: 'Save?', buttons: ['Yes', 'No'], checkboxLabel: 'Remember' }),\
            await dialog.showOpenDialog(win),\
        ]; })()",
    );
    assert_eq!(
        got,
        serde_json::json!([
            { "canceled": false, "filePaths": ["C:/a.txt", "C:/b.txt"] },
            { "canceled": false, "filePath": "C:/out.txt" },
            { "response": 1, "checkboxChecked": true },
            { "canceled": true, "filePaths": [] },
        ])
    );
    let asked = app.shell.dialogs();
    assert_eq!(asked.len(), 4);
    let DialogSpec::MessageBox {
        buttons,
        cancel_id,
        checkbox_label,
        ..
    } = &asked[2].1
    else {
        panic!("the third dialog is a message box: {:?}", asked[2].1);
    };
    assert_eq!(
        (buttons.as_slice(), *cancel_id, checkbox_label.as_str()),
        (&["Yes".to_owned(), "No".to_owned()][..], 1, "Remember")
    );
}

#[test]
fn a_sync_message_box_stops_the_script_but_not_the_pages_or_the_shell() {
    let app = launch_with_window();
    app.shell.hold_dialogs(true);
    app.hub.to_main(|guest| {
        let _ = guest.eval("globalThis.response = electron.dialog.showMessageBoxSync(win, { message: 'Sure?', buttons: ['OK', 'Cancel'] }); 1");
    });
    assert!(app.run_until(|app| !app.shell.dialogs().is_empty(), TIMEOUT));
    let (call, _) = app.shell.dialogs()[0].clone();
    let contents = app.hub.contents_of(app.hub.windows()[0]).unwrap();
    let ticks = |app: &App| {
        app.eval_renderer(contents, "globalThis.ticks")
            .parse::<u64>()
            .unwrap_or(0)
    };
    let before = ticks(&app);
    assert!(
        app.run_until(|app| ticks(app) > before + 3, TIMEOUT),
        "the page's timers kept running"
    );
    assert!(app.shell.answer_dialog(
        call,
        DialogResult::MessageBox {
            response: 1,
            checkbox_checked: false
        }
    ));
    assert!(app.run_until(|app| app.eval_main("globalThis.response") == "1", TIMEOUT));
}

#[test]
fn dialog_options_are_checked_as_electrons_javascript_checks_them() {
    let app = launch_with_window();
    let got = ask(
        &app,
        &format!(
            "(() => {{ const {{ dialog }} = electron; const c = {CATCH}; return {{\
                type: c(() => dialog.showMessageBoxSync({{ type: 'nope' }})),\
                buttons: c(() => dialog.showMessageBoxSync({{ buttons: 'x' }})),\
                title: c(() => dialog.showOpenDialogSync({{ title: 1 }})),\
                properties: c(() => dialog.showOpenDialogSync({{ properties: 'x' }})),\
                checkbox: c(() => dialog.showMessageBoxSync({{ checkboxChecked: true }})),\
                errorBox: c(() => dialog.showErrorBox()),\
            }}; }})()"
        ),
    );
    assert_eq!(
        got["type"],
        serde_json::json!(["TypeError", "Invalid message box type"])
    );
    assert_eq!(
        got["buttons"],
        serde_json::json!(["TypeError", "Buttons must be an array"])
    );
    assert_eq!(
        got["title"],
        serde_json::json!(["TypeError", "Title must be a string"])
    );
    assert_eq!(
        got["properties"],
        serde_json::json!(["TypeError", "Properties must be an array"])
    );
    assert_eq!(
        got["checkbox"],
        serde_json::json!([
            "Error",
            "checkboxChecked requires that checkboxLabel also be passed"
        ])
    );
    assert_eq!(
        got["errorBox"],
        expected("system")["dialogBad"]["errorBoxNoArg"]
            .as_object()
            .map(|e| serde_json::json!([e["name"], e["message"]]))
            .unwrap()
    );
}

// ---------- nativeTheme and screen ----------

fn dark_page(app: &App, contents: u32) -> bool {
    app.eval_renderer(
        contents,
        "matchMedia('(prefers-color-scheme: dark)').matches",
    ) == "true"
}

#[test]
fn the_theme_source_sets_every_pages_color_scheme_and_emits_updated_once_per_change() {
    let app = launch_with_window();
    ask(
        &app,
        "(() => { globalThis.heard = 0; electron.nativeTheme.on('updated', () => { globalThis.heard++; }); })()",
    );
    let contents = app.hub.contents_of(app.hub.windows()[0]).unwrap();
    let want = expected("system");
    let got = ask(
        &app,
        "({ source: electron.nativeTheme.themeSource, dark: electron.nativeTheme.shouldUseDarkColors, contrast: electron.nativeTheme.shouldUseHighContrastColors, inverted: electron.nativeTheme.shouldUseInvertedColorScheme })",
    );
    assert_eq!(got["source"], want["theme"]["source"]);
    assert_eq!(got["dark"], want["theme"]["dark"]);
    assert_eq!(got["inverted"], want["theme"]["inverted"]);
    assert!(!dark_page(&app, contents));

    ask(&app, "(electron.nativeTheme.themeSource = 'dark', 1)");
    assert!(app.run_until(|app| dark_page(app, contents), TIMEOUT));
    let got = ask(
        &app,
        "({ source: electron.nativeTheme.themeSource, dark: electron.nativeTheme.shouldUseDarkColors })",
    );
    assert_eq!(got, want["themeDark"]);
    ask(&app, "(electron.nativeTheme.themeSource = 'light', 1)");
    assert!(app.run_until(|app| !dark_page(app, contents), TIMEOUT));
    ask(&app, "(electron.nativeTheme.themeSource = 'system', 1)");
    assert_eq!(
        ask(&app, "globalThis.heard"),
        serde_json::json!(2),
        "{:?}",
        want["themeEvents"]
    );

    // The system theme changes while the source forces light: nothing changes for the pages.
    ask(&app, "(electron.nativeTheme.themeSource = 'light', 1)");
    app.shell.set_dark(true);
    assert!(!dark_page(&app, contents));
    ask(&app, "(electron.nativeTheme.themeSource = 'system', 1)");
    assert!(app.run_until(|app| dark_page(app, contents), TIMEOUT));
    assert_eq!(
        ask(
            &app,
            &format!("({CATCH})(() => {{ electron.nativeTheme.themeSource = 'blue'; }})")
        ),
        want["themeBad"]
            .as_object()
            .map(|e| serde_json::json!([e["name"], e["message"]]))
            .unwrap()
    );
}

#[test]
fn a_new_isolate_a_navigation_makes_starts_in_the_current_color_scheme() {
    let app = launch_with_window();
    let contents = app.hub.contents_of(app.hub.windows()[0]).unwrap();
    ask(&app, "(electron.nativeTheme.themeSource = 'dark', 1)");
    assert!(app.run_until(|app| dark_page(app, contents), TIMEOUT));
    ask(&app, "win.loadURL('data:text/html,<p>second</p>')");
    assert!(app.run_until(
        |app| app.eval_renderer(contents, "document.body.textContent") == "\"second\"",
        TIMEOUT
    ));
    assert!(dark_page(&app, contents));
}

fn display(id: u32, x: f32, scale: f32) -> Display {
    let bounds = Rect {
        x,
        y: 0.,
        width: 1000.,
        height: 800.,
    };
    Display {
        id,
        bounds,
        work_area: Rect {
            height: 760.,
            ..bounds
        },
        scale_factor: scale,
        primary: id == 1,
    }
}

#[test]
fn screen_answers_the_shells_displays_from_the_first_line_and_emits_their_changes() {
    let app = launch();
    let want = expected("system");
    let primary = ask(&app, "electron.screen.getPrimaryDisplay()");
    assert_eq!(
        primary["bounds"],
        serde_json::json!({ "x": 0, "y": 0, "width": 1920, "height": 1080 })
    );
    let mut keys: Vec<_> = primary.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(serde_json::json!(keys), want["display"]["keys"]);
    ask(
        &app,
        "(() => { globalThis.events = []; for (const name of ['display-added', 'display-removed', 'display-metrics-changed']) electron.screen.on(name, (event, display, metrics) => events.push([name, display.id, metrics])); })()",
    );
    app.shell
        .set_displays(vec![display(1, 0., 2.), display(2, 1000., 1.)]);
    assert!(app.run_until(|app| app.eval_main("events.length") == "2", TIMEOUT));
    assert_eq!(
        ask(&app, "events"),
        serde_json::json!([
            ["display-added", 2, null],
            [
                "display-metrics-changed",
                1,
                ["bounds", "workArea", "scaleFactor"]
            ]
        ])
    );
    assert_eq!(ask(&app, "electron.screen.getAllDisplays().length"), 2);
    assert_eq!(
        ask(
            &app,
            "electron.screen.getDisplayNearestPoint({ x: 1500, y: 10 }).id"
        ),
        2
    );
    assert_eq!(
        ask(
            &app,
            "electron.screen.getDisplayMatching({ x: 900, y: 0, width: 300, height: 100 }).id"
        ),
        2
    );
    assert_eq!(
        ask(&app, "electron.screen.dipToScreenPoint({ x: 10, y: 20 })"),
        serde_json::json!({ "x": 20, "y": 40 })
    );
    app.shell.set_displays(vec![display(2, 0., 1.)]);
    assert!(app.run_until(|app| app.eval_main("events.length") == "4", TIMEOUT));
}

// ---------- shell, safeStorage, power, systemPreferences ----------

#[test]
fn shell_fails_as_electron_does_on_a_path_that_is_not_there_and_opens_nothing() {
    let app = launch();
    let want = expected("system");
    let got = ask(
        &app,
        &format!(
            "(async () => {{ const {{ shell }} = electron; const c = {CATCH}; return {{\
                missing: await shell.openPath('does-not-exist.txt'),\
                trashMissing: await shell.trashItem('does-not-exist.txt').then(() => 'trashed', (e) => e.message),\
                badUrl: await shell.openExternal('not a url').then(() => 'opened', (e) => e.message),\
                openExternalNone: c(() => shell.openExternal()),\
                openPathBad: c(() => shell.openPath(1)),\
            }}; }})()"
        ),
    );
    assert_eq!(got["missing"], want["shell"]["missing"]);
    assert_eq!(got["trashMissing"], want["shell"]["trashMissing"]);
    assert_eq!(got["badUrl"], "Failed to open: not a url");
    for key in ["openExternalNone", "openPathBad"] {
        let frozen = &want["shell"][key];
        assert_eq!(
            got[key],
            serde_json::json!([frozen["name"], frozen["message"]]),
            "{key}"
        );
    }
}

#[test]
fn safe_storage_round_trips_in_the_profile_and_says_what_electron_says_of_bad_input() {
    let app = launch();
    assert!(app.run_until(|app| ask(app, "electron.app.isReady()") == true, TIMEOUT));
    let got = ask(
        &app,
        "(() => { const { safeStorage } = electron; const cipher = safeStorage.encryptString('héllo ✓'); \
         const message = (call) => { try { call(); } catch (e) { return e.message; } }; \
         return { available: safeStorage.isEncryptionAvailable(), prefix: cipher.subarray(0, 3).toString(), length: cipher.length, \
                  plain: safeStorage.decryptString(cipher), \
                  garbage: message(() => safeStorage.decryptString(Buffer.from('plain text, not encrypted at all'))), \
                  short: message(() => safeStorage.decryptString(Buffer.from('v10abc'))), \
                  notBuffer: message(() => safeStorage.decryptString('x')) }; })()",
    );
    let want = expected("safe-storage");
    assert_eq!(got["available"], true);
    assert_eq!(got["prefix"], "v10");
    assert_eq!(got["length"], 3 + 12 + "héllo ✓".len() + 16);
    assert_eq!(got["plain"], "héllo ✓");
    assert_eq!(got["garbage"], want["decryptGarbage"]["message"]);
    assert_eq!(
        got["short"],
        "Error while decrypting the ciphertext provided to safeStorage.decryptString."
    );
    assert_eq!(got["notBuffer"], want["decryptBad"]["message"]);
}

#[test]
#[ignore = "DPAPI: only the Windows user who made the samples can decrypt them"]
fn safe_storage_opens_what_electron_39_encrypted_with_the_same_users_dpapi() {
    // Remade with `node tests/electron/system-a/observe/run.cjs <electron> observe/safe-storage.cjs <scratch>`:
    // `<scratch>/samples.json` and `<scratch>/profile/Local State` go to tests/electron/system-a/safe-storage.
    let text = std::fs::read_to_string(system_a("safe-storage/samples.json")).unwrap();
    let samples: Value = serde_json::from_str(&text).unwrap();
    let profile = App::scratch_dir("safe-storage-profile");
    std::fs::copy(
        system_a("safe-storage/Local State"),
        profile.join("Local State"),
    )
    .unwrap();
    let app = launch();
    assert!(app.run_until(|app| ask(app, "electron.app.isReady()") == true, TIMEOUT));
    let dir = serde_json::to_string(&profile.display().to_string()).unwrap();
    ask(&app, &format!("electron.app.setPath('userData', {dir})"));
    for sample in samples.as_array().unwrap() {
        let (text, cipher) = (
            sample["text"].as_str().unwrap(),
            sample["cipher"].as_str().unwrap(),
        );
        let got = ask(
            &app,
            &format!("electron.safeStorage.decryptString(Buffer.from('{cipher}', 'hex'))"),
        );
        assert_eq!(got, text);
    }
    let _ = std::fs::remove_dir_all(profile);
}

#[test]
fn power_blockers_start_and_stop_and_the_monitor_and_preferences_answer_electrons_shapes() {
    let app = launch();
    let want = expected("system");
    let got = ask(
        &app,
        &format!(
            "(async () => {{ const {{ powerSaveBlocker: b, powerMonitor: m, systemPreferences: p }} = electron; const c = {CATCH};\
              const id = b.start('prevent-app-suspension'); const started = b.isStarted(id); b.stop(id);\
              const second = b.start('prevent-display-sleep') - id; b.stop(id + 1);\
              return {{ idType: typeof id, started, afterStop: b.isStarted(id), unknownStop: b.stop(9999), second,\
                badType: c(() => b.start('nope')), idle: m.getSystemIdleState(1e9), idleTime: typeof m.getSystemIdleTime(),\
                thermal: m.getCurrentThermalState(), battery: typeof m.isOnBatteryPower(), badThreshold: c(() => m.getSystemIdleState()),\
                accent: /^[0-9A-F]{{8}}$/.test(p.getAccentColor()), window: /^#[0-9A-F]{{8}}$/.test(p.getColor('window')),\
                badColor: c(() => p.getColor('nope')), animation: Object.keys(p.getAnimationSettings()), media: p.getMediaAccessStatus('microphone'),\
                badMedia: c(() => p.getMediaAccessStatus('nope')), mac: typeof p.getUserDefault }}; }})()"
        ),
    );
    let pair = |e: &Value| serde_json::json!([e["name"], e["message"]]);
    assert_eq!(got["idType"], "number");
    assert_eq!(
        (
            &got["started"],
            &got["afterStop"],
            &got["unknownStop"],
            &got["second"]
        ),
        (&true.into(), &false.into(), &false.into(), &1.into())
    );
    assert_eq!(got["badType"], pair(&want["blocker"]["badType"]));
    assert_eq!(got["idle"], "active");
    assert_eq!(got["thermal"], want["power"]["thermal"]);
    assert_eq!(got["badThreshold"], pair(&want["power"]["badThreshold"]));
    assert_eq!(
        (&got["accent"], &got["window"]),
        (&true.into(), &true.into())
    );
    assert_eq!(got["badColor"], pair(&want["prefs"]["badColor"]));
    assert_eq!(
        got["animation"],
        serde_json::json!([
            "shouldRenderRichAnimation",
            "scrollAnimationsEnabledBySystem",
            "prefersReducedMotion"
        ])
    );
    assert_eq!(got["media"], want["prefs"]["media"]);
    assert_eq!(got["badMedia"], pair(&want["prefs"]["badMedia"]));
    assert_eq!(got["mac"], "undefined");
}

#[test]
#[ignore = "uses the real clipboard"]
fn the_clipboard_round_trips_text_html_rtf_images_and_buffers_and_lists_its_formats() {
    let app = launch();
    let path =
        serde_json::to_string(&system_a("fixtures/sample.png").display().to_string()).unwrap();
    let got = ask(
        &app,
        &format!(
            "(() => {{ const {{ clipboard: c, nativeImage }} = electron; const saved = c.readText(); const formats = {{}};\
              const hex = (b) => Buffer.from(b).toString('hex');\
              c.writeText('plain'); formats.text = c.availableFormats(); const text = c.readText();\
              c.writeHTML('<b>héllo</b>'); formats.html = c.availableFormats(); const html = c.readHTML();\
              c.writeRTF('{{\\\\rtf1 hello}}'); formats.rtf = c.availableFormats(); const rtf = c.readRTF();\
              const image = nativeImage.createFromPath({path}); c.writeImage(image); formats.image = c.availableFormats();\
              const back = hex(c.readImage().toBitmap()) === hex(image.toBitmap());\
              c.write({{ text: 'both', html: '<i>both</i>' }}); formats.both = c.availableFormats();\
              c.writeBuffer('verve-test-format', Buffer.from('abc')); const buffer = Buffer.from(c.readBuffer('verve-test-format')).toString(); const has = c.has('verve-test-format');\
              c.writeBookmark('Title', 'https://example.com/'); const bookmark = c.readBookmark();\
              c.clear(); const empty = [c.readText(), c.readHTML(), c.readRTF(), c.readImage().isEmpty()];\
              c.writeText(saved); return {{ text, html, rtf, back, buffer, has, bookmark, empty, formats }}; }})()"
        ),
    );
    let want = &expected("system")["formats"];
    assert_eq!(got["text"], "plain");
    assert_eq!(got["html"], "<b>héllo</b>");
    assert_eq!(got["rtf"], "{\\rtf1 hello}");
    assert_eq!(
        (&got["back"], &got["buffer"], &got["has"]),
        (&true.into(), &"abc".into(), &true.into())
    );
    assert_eq!(
        got["bookmark"],
        serde_json::json!({ "title": "", "url": "https://example.com/" })
    );
    assert_eq!(got["empty"], serde_json::json!(["", "", "", true]));
    for key in ["text", "html", "rtf", "image", "both"] {
        assert_eq!(
            got["formats"][key], want[key],
            "availableFormats after {key}"
        );
    }
}

#[test]
#[ignore = "moves a file it made to the recycle bin"]
fn trash_item_moves_a_file_to_the_recycle_bin() {
    let app = launch();
    let dir = App::scratch_dir("trash");
    let file = dir.join("to-trash.txt");
    std::fs::write(&file, "bye").unwrap();
    let path = serde_json::to_string(&file.display().to_string()).unwrap();
    assert_eq!(
        ask(
            &app,
            &format!("electron.shell.trashItem({path}).then(() => 'trashed')")
        ),
        "trashed"
    );
    assert!(!file.exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// Removes a scratch directory when dropped, whether or not the test got that far.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn trash_item_refuses_a_relative_path_before_it_asks_the_system() {
    // A relative path would be deleted for good, so it is refused whatever is there; the file is
    // one this test makes in a scratch directory the app works in.
    let app = launch();
    let dir = Scratch(App::scratch_dir("trash-relative"));
    let file = dir.0.join("stay.txt");
    std::fs::write(&file, "stay").unwrap();
    let folder = serde_json::to_string(&dir.0.display().to_string()).unwrap();
    let got = ask(
        &app,
        &format!(
            "(process.chdir({folder}), electron.shell.trashItem('stay.txt').then(() => 'trashed', (e) => e.message))"
        ),
    );
    assert_eq!(got, "Failed to parse path");
    assert!(file.exists(), "the file was deleted");
}

#[test]
fn a_notification_says_it_failed_until_the_system_toast_is_bound() {
    let app = launch();
    let got = ask(
        &app,
        "(() => { const { Notification } = electron; const heard = []; const n = new Notification({ title: 'x' }); \
          n.on('failed', (event, reason) => heard.push(reason)); n.show(); n.close(); \
          return { supported: Notification.isSupported(), heard }; })()",
    );
    assert_eq!(got["supported"], false);
    assert_eq!(got["heard"].as_array().map(Vec::len), Some(1));
}
