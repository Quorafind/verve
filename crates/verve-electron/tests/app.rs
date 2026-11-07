//! `require('electron').app` and the command line, headless: the lifecycle in Electron's order,
//! the places `getPath` names, the switches, the single-instance lock.
//!
//! The expectations are what Electron 39.8.10 did on Windows with the same scripts (probes run in
//! `electron.exe` with a throwaway `--user-data-dir`); where Verve's order differs, the test says so.

#![allow(clippy::unwrap_used)]

mod support;

use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::Duration;

use support::{App, TIMEOUT};
use verve_electron::launcher::{Manifest, Start, Target};
use verve_electron::msg::ToShell;

/// An app of these tests: a headless app that logs to a file and has a profile directory of its
/// own, both in a scratch directory that goes with it.
struct Run {
    app: App,
    log: PathBuf,
}

impl Deref for Run {
    type Target = App;

    fn deref(&self) -> &App {
        &self.app
    }
}

impl Run {
    /// Starts the app in `app_dir` (its `main.js`) with these switches, plus `--log=<file>` named
    /// `log_name` in `scratch` and a `--user-data-dir` there, as every launch here has. The
    /// scratch directory is removed with the app.
    fn start(app_dir: &Path, scratch: &Path, log_name: &str, switches: &[(&str, &str)]) -> Self {
        let log = scratch.join(log_name);
        let mut start = Start::development(app_dir.to_path_buf());
        start.target = Target::App {
            app_path: app_dir.to_path_buf(),
            entry: app_dir.join("main.js"),
        };
        let user_data = scratch.join("profile").to_string_lossy().into_owned();
        let log_text = log.to_string_lossy().into_owned();
        let mut all = vec![
            ("log", log_text.as_str()),
            ("user-data-dir", user_data.as_str()),
        ];
        all.extend_from_slice(switches);
        start.switches = all
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect();
        start.argv = all.iter().map(|(n, v)| format!("--{n}={v}")).collect();
        start.manifest = Manifest::read(app_dir).unwrap_or_default();
        let app = App::launch_with(start, Some(scratch.to_path_buf()));
        Self { app, log }
    }

    /// The lifecycle fixture, running one scenario.
    fn lifecycle(scenario: &str) -> Self {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps/lifecycle");
        let scratch = App::scratch_dir(scenario);
        Self::start(&fixture, &scratch, "log.txt", &[("scenario", scenario)])
    }

    /// An app made of this one script, named `script-app` at 2.0.1.
    fn script(main_js: &str, switches: &[(&str, &str)]) -> Self {
        let dir = App::scratch_dir("script");
        std::fs::write(dir.join("main.js"), main_js).unwrap();
        let manifest = r#"{"name":"script-app","version":"2.0.1"}"#;
        std::fs::write(dir.join("package.json"), manifest).unwrap();
        Self::start(&dir, &dir.clone(), "log.txt", switches)
    }

    fn exit_code(&self) -> Option<i32> {
        self.app.exit_code(TIMEOUT)
    }

    /// What the app logged, once it has ended.
    fn events(&self) -> Vec<String> {
        self.exit_code().expect("the app ended");
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::trim_end)
            .map(str::to_owned)
            .collect()
    }
}

/// The part of the log after `marker`.
fn after<'a>(events: &'a [String], marker: &str) -> &'a [String] {
    let at = events.iter().position(|e| e == marker).expect(marker);
    &events[at + 1..]
}

#[test]
fn the_app_launches_and_becomes_ready_in_electrons_order() {
    let run = Run::lifecycle("close-listener");
    let events = run.events();
    // Electron: `will-finish-launching` and `ready` before the promise of `whenReady`; the
    // window's `web-contents-created` before its `browser-window-created`.
    assert_eq!(
        events[..7],
        [
            "entry isReady=false",
            "app will-finish-launching",
            "app ready",
            "whenReady",
            "app web-contents-created",
            "app browser-window-created",
            "--- act",
        ]
    );
}

#[test]
fn closing_the_last_window_with_a_listener_only_says_so_and_a_quit_then_ends_the_app() {
    let run = Run::lifecycle("close-listener");
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "win close",
            "win closed",
            "app window-all-closed",
            "--- quit()",
            "app before-quit",
            "app will-quit",
            "process exit 0",
            "app quit 0",
            "after quit()",
        ]
    );
    assert_eq!(run.shell.exit_code(), Some(0));
}

#[test]
fn closing_the_last_window_with_no_listener_quits_the_app() {
    let run = Run::lifecycle("close-nolistener");
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "win close",
            "win closed",
            "app before-quit",
            "app will-quit",
            "process exit 0",
            "app quit 0",
        ]
    );
    assert_eq!(run.shell.exit_code(), Some(0));
}

#[test]
fn quit_asks_the_windows_before_it_says_will_quit() {
    let run = Run::lifecycle("quit");
    // Electron emits `close` inside `quit()`, so the call has not returned when it is heard.
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "app before-quit",
            "win close",
            "after quit()",
            "win closed",
            "app will-quit",
            "process exit 0",
            "app quit 0",
        ]
    );
}

#[test]
fn quit_with_no_window_runs_through_its_events_before_it_returns() {
    let run = Run::lifecycle("quit-nowin");
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "app before-quit",
            "app will-quit",
            "process exit 0",
            "app quit 0",
            "after quit()"
        ]
    );
}

#[test]
fn a_second_quit_while_quitting_is_ignored() {
    let run = Run::lifecycle("quit-twice");
    let events = run.events();
    assert_eq!(events.iter().filter(|e| *e == "app before-quit").count(), 1);
    assert_eq!(events.last().map(String::as_str), Some("app quit 0"));
}

#[test]
fn preventing_before_quit_leaves_the_window_and_the_app_up() {
    let run = Run::lifecycle("quit-prevent-before");
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "app before-quit",
            "after quit()",
            "--- app.exit(9)",
            "win closed",
            "process exit 9",
            "app quit 9",
        ]
    );
    assert_eq!(run.shell.exit_code(), Some(9));
}

#[test]
fn a_window_that_refuses_to_close_cancels_the_quit_without_will_quit() {
    let run = Run::lifecycle("quit-prevent-close");
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "app before-quit",
            "win close",
            "after quit()",
            "--- app.exit(9)",
            "win closed",
            "process exit 9",
            "app quit 9",
        ]
    );
}

#[test]
fn preventing_will_quit_keeps_the_app_running_with_no_window() {
    let run = Run::lifecycle("quit-prevent-will");
    assert_eq!(
        after(&run.events(), "--- act"),
        [
            "app before-quit",
            "win close",
            "after quit()",
            "win closed",
            "app will-quit",
            "--- app.exit(9)",
            "process exit 9",
            "app quit 9",
        ]
    );
}

/// Electron 39.8.10: `app.exit(3)` is not silent — the windows hear `closed` (no `close`), then
/// `exit` and `quit` are heard, and the script goes on past the call.
#[test]
fn app_exit_destroys_the_windows_without_asking_and_ends_with_its_code() {
    let run = Run::lifecycle("exit");
    assert_eq!(
        after(&run.events(), "--- act"),
        ["win closed", "process exit 3", "app quit 3", "after exit()"]
    );
    assert_eq!(run.shell.exit_code(), Some(3));
}

/// `process.exit` is `app.exit` in Electron's main process (`lib/browser/init.ts`): `closed`,
/// `exit`, `quit`.
#[test]
fn process_exit_ends_the_app_with_its_code() {
    let run = Run::lifecycle("pexit");
    assert_eq!(
        after(&run.events(), "--- act"),
        ["win closed", "process exit 4", "app quit 4"]
    );
    assert_eq!(run.shell.exit_code(), Some(4));
}

/// Electron 39.8.10: `quit` ends with 0 whatever `process.exitCode` says.
#[test]
fn quit_ends_with_zero_though_exit_code_was_set() {
    let run = Run::lifecycle("code");
    let events = run.events();
    assert_eq!(events.last().map(String::as_str), Some("app quit 0"));
    assert_eq!(run.shell.exit_code(), Some(0));
}

/// An `uncaughtException` listener takes the error: no box, no message on standard error.
#[test]
fn an_uncaught_exception_listener_takes_the_error_before_the_box() {
    let run = Run::script(
        "const { app } = require('electron');
         process.on('uncaughtException', (error, origin) => {
           require('fs').writeFileSync(app.commandLine.getSwitchValue('log'), error.message + '|' + origin);
           setTimeout(() => app.exit(0), 50);
         });
         app.whenReady().then(() => setTimeout(() => { throw new Error('caught'); }, 10));",
        &[],
    );
    assert_eq!(run.exit_code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&run.log).unwrap(),
        "caught|uncaughtException"
    );
    assert!(
        run.shell
            .received()
            .iter()
            .all(|msg| !matches!(msg, ToShell::ErrorBox { .. })),
        "{:?}",
        run.shell.received()
    );
}

/// The error box the app asked for, if it did.
fn error_box(app: &App) -> Option<(String, String)> {
    app.shell.received().into_iter().find_map(|msg| match msg {
        ToShell::ErrorBox { title, content } => Some((title, content)),
        _ => None,
    })
}

#[test]
fn an_error_nobody_catches_shows_a_box_and_leaves_the_app_running() {
    let run = Run::script(
        "require('electron').app.on('ready', () => setTimeout(() => { throw new Error('later'); }, 20));",
        &[],
    );
    assert!(run.run_until(|app| error_box(app).is_some(), TIMEOUT));
    let (title, content) = error_box(&run).expect("an error box");
    assert_eq!(title, "A JavaScript error occurred in the main process");
    // Electron 39.8.10's `lib/browser/init.ts` puts the error's stack after "Uncaught Exception:".
    assert!(
        content.starts_with(
            "Uncaught Exception:
Error: later
    at "
        ),
        "{content}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(run.shell.exit_code(), None);
}

/// The same file builds the text of an error that has no stack as `${name}: ${message}`, which is
/// `undefined: undefined` for a string that was thrown.
#[test]
fn a_thrown_string_shows_undefined_in_the_box_as_electrons_init_ts_does() {
    let run = Run::script(
        "require('electron').app.on('ready', () => setTimeout(() => { throw 'plain'; }, 20));",
        &[],
    );
    assert!(run.run_until(|app| error_box(app).is_some(), TIMEOUT));
    let (_, content) = error_box(&run).expect("an error box");
    assert_eq!(
        content,
        "Uncaught Exception:
undefined: undefined"
    );
}

/// Runs `script` as an app and answers what it put in `out`. The script has `app`, `out` and
/// `done()`, which writes `out` as JSON and ends the app.
fn report(script: &str, switches: &[(&str, &str)]) -> serde_json::Value {
    let prelude = "const { app } = require('electron'); const out = {}; \
        const done = () => { require('fs').writeFileSync(app.commandLine.getSwitchValue('log'), \
        JSON.stringify(out)); app.exit(0); };\n";
    let run = Run::script(&format!("{prelude}{script}"), switches);
    run.exit_code().expect("the app ended");
    let text = std::fs::read_to_string(&run.log).expect("the script reported");
    serde_json::from_str(&text).unwrap()
}

#[cfg(windows)]
const PATH_NAMES: &str = "['home','appData','userData','sessionData','temp','exe','module','desktop',\
    'documents','downloads','music','pictures','videos','recent','logs','crashDumps','assets','cache']";

/// Electron 39.8.10 on Windows: every name answers an absolute path that exists once the app is
/// ready (Chromium makes the profile's `logs` and `Crashpad`), and an unknown name — or one in the
/// wrong case — throws `Error: Failed to get '<name>' path`.
#[cfg(windows)]
#[test]
fn every_path_name_answers_an_absolute_path_that_exists() {
    let out = report(
        &format!(
            "const fs = require('fs'); const path = require('path');
             app.whenReady().then(() => {{
               out.paths = {{}};
               for (const name of {PATH_NAMES}) {{
                 const p = app.getPath(name);
                 out.paths[name] = [path.isAbsolute(p), fs.existsSync(p)];
               }}
               for (const name of ['bogus', 'appdata']) {{
                 try {{ app.getPath(name); }} catch (e) {{ out[name] = e.name + ': ' + e.message; }}
               }}
               done();
             }});"
        ),
        &[],
    );
    for (name, found) in out["paths"].as_object().unwrap() {
        assert_eq!(found, &serde_json::json!([true, true]), "{name}");
    }
    assert_eq!(out["bogus"], "Error: Failed to get 'bogus' path");
    assert_eq!(out["appdata"], "Error: Failed to get 'appdata' path");
}

#[test]
fn the_user_data_directory_is_the_one_the_command_line_gave_and_follows_set_path() {
    let given = App::scratch_dir("user-data");
    let given_text = given.to_string_lossy().into_owned();
    let out = report(
        "out.userData = app.getPath('userData'); out.session = app.getPath('sessionData');
         out.logs = app.getPath('logs');
         const moved = require('path').join(app.getPath('temp'), 'verve-moved-' + process.pid);
         app.setPath('userData', moved); out.moved = [app.getPath('userData'), moved];
         for (const bad of [['userData', 'relative'], ['nope', app.getPath('temp')]]) {
           try { app.setPath(...bad); } catch (e) { out['bad-' + bad[0]] = e.message; }
         }
         require('fs').rmSync(moved, { recursive: true, force: true });
         done();",
        &[("user-data-dir", &given_text)],
    );
    // The command line's own `--user-data-dir` is the last one: the harness's comes first.
    assert_eq!(out["userData"], given_text);
    assert_eq!(out["session"], given_text);
    assert_eq!(
        out["logs"],
        format!("{given_text}{}logs", std::path::MAIN_SEPARATOR)
    );
    assert_eq!(out["moved"][0], out["moved"][1]);
    assert_eq!(out["bad-userData"], "Path must be absolute");
    assert_eq!(out["bad-nope"], "Failed to set path");
    let _ = std::fs::remove_dir_all(given);
}

/// Electron 39.8.10: the name is `productName`, else `name`; the version is `package.json`'s;
/// `getAppPath()` is the folder; the user agent carries both with the spaces taken out.
#[test]
fn the_app_is_named_and_versioned_by_its_package_json() {
    let out = report(
        "out.name = [app.getName(), app.name]; out.version = app.getVersion();
         out.path = app.getAppPath(); out.packaged = app.isPackaged;
         out.ua = app.userAgentFallback; app.setName('Other'); out.renamed = app.getName();
         out.userData = app.getPath('userData');
         done();",
        &[],
    );
    assert_eq!(out["name"], serde_json::json!(["script-app", "script-app"]));
    assert_eq!(out["version"], "2.0.1");
    assert_eq!(out["packaged"], false);
    assert_eq!(out["renamed"], "Other");
    let agent = out["ua"].as_str().unwrap();
    assert!(agent.contains(" script-app/2.0.1 Chrome/142."), "{agent}");
    assert!(agent.ends_with(" Electron/39.2.0 Safari/537.36"), "{agent}");
    assert!(
        out["path"]
            .as_str()
            .unwrap()
            .contains("verve-electron-script-")
    );
}

#[test]
fn the_main_process_has_the_process_fields_electron_adds() {
    let out = report(
        "out.p = { type: process.type, title: process.title, modules: process.versions.modules,
            electron: process.versions.electron, defaultApp: process.defaultApp,
            mas: process.mas, windowsStore: process.windowsStore,
            resources: typeof process.resourcesPath, io: typeof process.getIOCounters,
            created: typeof process.getCreationTime(), argv: process.argv.length,
            sys: process.getSystemVersion(), fd: typeof process.setFdLimit };
         out.cpu = Object.keys(process.getCPUUsage());
         out.heap = Object.keys(process.getHeapStatistics());
         out.mem = Object.keys(process.getSystemMemoryInfo());
         process.getProcessMemoryInfo().then((m) => { out.procmem = Object.keys(m); done(); });",
        &[],
    );
    let p = &out["p"];
    assert_eq!(
        (&p["type"], &p["title"], &p["modules"]),
        (&"browser".into(), &"node".into(), &"140".into())
    );
    assert_eq!(p["electron"], "39.2.0");
    assert_eq!(p["defaultApp"], true);
    assert!(
        p["mas"].is_null() && p["windowsStore"].is_null(),
        "undefined off macOS and the store"
    );
    assert_eq!(p["resources"], "string");
    assert_eq!(p["io"], "undefined", "Electron 39 has no getIOCounters");
    assert_eq!(p["created"], "number");
    assert!(
        p["sys"].as_str().unwrap().split('.').count() >= 2,
        "{}",
        p["sys"]
    );
    assert_eq!(
        out["cpu"],
        serde_json::json!([
            "percentCPUUsage",
            "cumulativeCPUUsage",
            "idleWakeupsPerSecond"
        ])
    );
    assert_eq!(out["heap"].as_array().unwrap().len(), 9);
    assert_eq!(
        out["procmem"],
        serde_json::json!(["residentSet", "private", "shared"])
    );
    assert!(out["mem"].as_array().unwrap().len() >= 2);
}

/// Electron 39.8.10 on Windows, with `--foo=1 --lang=de-DE`: switches are readable by name without
/// dashes, a switch with no value reads `''`, and `getLocale()` is `''` before `ready` and the
/// language's Chromium locale (`de`) after.
#[test]
fn the_command_line_is_readable_and_appendable_and_lang_sets_the_locale() {
    let out = report(
        "const cl = app.commandLine; out.pre = app.getLocale();
         out.before = [cl.hasSwitch('foo'), cl.getSwitchValue('foo'), cl.hasSwitch('--foo'), cl.getSwitchValue('missing')];
         cl.appendSwitch('foo', 'bar'); cl.appendSwitch('flag'); cl.appendArgument('x');
         out.after = [cl.getSwitchValue('foo'), cl.hasSwitch('flag'), cl.getSwitchValue('flag')];
         cl.removeSwitch('flag'); out.removed = cl.hasSwitch('flag');
         app.whenReady().then(() => { out.locale = app.getLocale(); done(); });",
        &[("foo", "1"), ("lang", "de-DE")],
    );
    assert_eq!(out["pre"], "");
    assert_eq!(out["before"], serde_json::json!([true, "1", false, ""]));
    assert_eq!(out["after"], serde_json::json!(["bar", true, ""]));
    assert_eq!(out["removed"], false);
    assert_eq!(out["locale"], "de");
}

/// Electron 39.8.10: `disableHardwareAcceleration` and `enableSandbox` are for before `ready`, and
/// `getSystemLocale` is for after.
#[test]
fn calls_made_at_the_wrong_time_throw_electrons_messages() {
    let out = report(
        "const said = (f) => { try { f(); return 'ok'; } catch (e) { return e.message; } };
         out.early = [said(() => app.getSystemLocale()), said(() => app.disableHardwareAcceleration()),
                      app.isHardwareAccelerationEnabled(), app.isReady()];
         app.whenReady().then(() => {
           out.late = [said(() => app.disableHardwareAcceleration()), said(() => app.enableSandbox()),
                       said(() => app.getSystemLocale()), app.isReady()];
           done();
         });",
        &[],
    );
    assert_eq!(
        out["early"],
        serde_json::json!([
            "app.getSystemLocale() can only be called after app is ready",
            "ok",
            false,
            false
        ])
    );
    assert_eq!(
        out["late"][0],
        "app.disableHardwareAcceleration() can only be called before app is ready"
    );
    assert_eq!(
        out["late"][1],
        "app.enableSandbox() can only be called before app is ready"
    );
    assert_eq!(out["late"][2], "ok");
    assert_eq!(out["late"][3], true);
}

#[test]
fn the_figures_and_stubs_electron_answers_are_there() {
    let out = report(
        "app.whenReady().then(async () => {
           out.metrics = app.getAppMetrics().map((m) => m.type);
           out.gpu = app.getGPUFeatureStatus()['2d_canvas'];
           out.gpuInfo = Object.keys(await app.getGPUInfo('basic'));
           out.badge = [app.setBadgeCount(3), app.getBadgeCount(), app.badgeCount];
           out.jump = app.getJumpListSettings(); out.tasks = app.setUserTasks([]);
           out.login = Object.keys(app.getLoginItemSettings());
           out.dock = typeof app.dock; out.ua = typeof app.userAgentFallback;
           out.single = app.hasSingleInstanceLock();
           out.file = await app.getFileIcon('x').catch((e) => e.code);
           done();
         });",
        &[],
    );
    assert_eq!(out["metrics"], serde_json::json!(["Browser"]));
    assert_eq!(out["gpu"], "disabled_software");
    assert_eq!(
        out["gpuInfo"],
        serde_json::json!(["auxAttributes", "gpuDevice"])
    );
    assert_eq!(out["badge"], serde_json::json!([true, 3, 3]));
    assert_eq!(
        out["jump"],
        serde_json::json!({ "minItems": 10, "removedItems": [] })
    );
    assert_eq!(out["tasks"], true);
    assert_eq!(out["login"][0], "launchItems");
    assert_eq!(out["dock"], "undefined");
    assert_eq!(out["single"], false);
    assert_eq!(out["file"], "ERR_NOT_SUPPORTED");
}

/// Two instances with the same profile directory: the second is refused and told so, and the
/// first hears `second-instance` with the second's command line, working directory and data.
/// Electron 39.8.10 (Windows): the argv is the executable, the switches, then the rest; a `Date`
/// in `additionalData` arrives as its ISO string; the event object has no own keys.
#[test]
fn a_second_instance_is_refused_and_the_first_hears_of_it() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/electron/apps/single-instance");
    let shared = App::scratch_dir("single");
    let log = |role: &str| shared.join(format!("{role}.log"));
    let first = Run::start(&fixture, &shared, "first.log", &[("role", "first")]);
    let took = |_: &App| {
        std::fs::read_to_string(log("first")).is_ok_and(|text| text.contains("\"got\":true"))
    };
    assert!(first.run_until(took, TIMEOUT), "the first took the lock");
    let second = Run::start(
        &fixture,
        &shared,
        "second.log",
        &[("role", "second"), ("extra", "x")],
    );
    assert_eq!(second.exit_code(), Some(0));
    first.exit_code().expect("the first heard and left");

    let lines = |role: &str| -> Vec<serde_json::Value> {
        std::fs::read_to_string(log(role))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    let second_lines = lines("second");
    assert_eq!(
        second_lines[0],
        serde_json::json!({ "role": "second", "got": false, "has": false })
    );
    let first_lines = lines("first");
    assert_eq!(
        first_lines[0],
        serde_json::json!({ "role": "first", "got": true, "has": true })
    );
    let heard = &first_lines[1];
    assert_eq!(
        heard["additionalData"],
        serde_json::json!({
            "hello": "world", "list": [1, 2, { "nested": null }], "when": "1970-01-01T00:00:00.000Z"
        })
    );
    assert_eq!(heard["eventKeys"], serde_json::json!([]));
    assert_eq!(
        heard["workingDirectory"],
        std::env::current_dir().unwrap().to_string_lossy().as_ref()
    );
    let argv: Vec<&str> = heard["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert!(argv[1..].contains(&"--extra=x"), "{argv:?}");
    assert_eq!(first_lines[2], serde_json::json!({ "released": true }));
    let _ = std::fs::remove_dir_all(shared);
}

/// The registry key of a protocol that exists only for the test, removed whatever happens.
#[cfg(windows)]
struct Scheme(String);

#[cfg(windows)]
impl Scheme {
    /// A protocol name nothing else uses.
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        Scheme(format!("verve-test-{}-{nanos}", std::process::id()))
    }
}

#[cfg(windows)]
impl Drop for Scheme {
    fn drop(&mut self) {
        let key = format!(r"HKCU\Software\Classes\{}", self.0);
        let _ = std::process::Command::new("reg.exe")
            .args(["delete", &key, "/f"])
            .output();
    }
}

/// `setAsDefaultProtocolClient` writes `HKCU\Software\Classes\<protocol>`; only a throwaway
/// protocol is used here.
#[cfg(windows)]
#[test]
fn a_protocol_client_is_registered_found_and_removed() {
    let scheme = Scheme::new();
    let out = report(
        &format!(
            "const p = '{}';
             out.before = app.isDefaultProtocolClient(p);
             out.set = app.setAsDefaultProtocolClient(p);
             out.after = [app.isDefaultProtocolClient(p), app.isDefaultProtocolClient(p, process.execPath, ['--x'])];
             out.name = app.getApplicationNameForProtocol(p + '://x');
             app.getApplicationInfoForProtocol(p + '://x').then((info) => {{
               out.info = Object.keys(info);
               out.removed = app.removeAsDefaultProtocolClient(p);
               out.gone = app.isDefaultProtocolClient(p);
               done();
             }});",
            scheme.0
        ),
        &[],
    );
    assert_eq!(out["before"], false);
    assert_eq!(out["set"], true);
    assert_eq!(out["after"], serde_json::json!([true, false]));
    assert!(!out["name"].as_str().unwrap().is_empty());
    assert_eq!(out["info"], serde_json::json!(["name", "path"]));
    assert_eq!(
        (&out["removed"], &out["gone"]),
        (&true.into(), &false.into())
    );
}

/// `relaunch` starts the given program as the app ends; the test's program makes a folder and
/// exits, so nothing is left running.
#[test]
fn relaunch_starts_the_given_program_as_the_app_ends() {
    let marker = App::scratch_dir("relaunch").join("started");
    let (program, args) = if cfg!(windows) {
        (
            "cmd.exe",
            vec![
                "/c".to_string(),
                "md".to_string(),
                marker.display().to_string(),
            ],
        )
    } else {
        ("mkdir", vec![marker.display().to_string()])
    };
    let args_json = serde_json::to_string(&args).unwrap();
    let run = Run::script(
        &format!(
            "const {{ app }} = require('electron');
             app.whenReady().then(() => {{
               app.relaunch({{ execPath: {program:?}, args: {args_json} }});
               app.quit();
             }});"
        ),
        &[],
    );
    assert_eq!(run.exit_code(), Some(0));
    assert!(
        run.run_until(|_| marker.exists(), TIMEOUT),
        "the relaunched program ran"
    );
    let _ = std::fs::remove_dir_all(marker.parent().unwrap());
}

/// A script's start: `app`, and `log(...)` that appends a line to the file the test reads.
const LOG: &str = "const { app, BrowserWindow } = require('electron'); const fs = require('fs');
    const log = (...a) => fs.appendFileSync(app.commandLine.getSwitchValue('log'), a.join(' ') + '\\n');\n";

/// Runs a script that logs, and answers its log once the app has ended.
fn logged(script: &str) -> (Run, Vec<String>) {
    let run = Run::script(&format!("{LOG}{script}"), &[]);
    let events = run.events();
    (run, events)
}

/// Electron's `lib/browser/init.ts` makes `process.exit` `app.exit`: a script that leaves the app
/// at its first line is no error, whatever follows it, and `ready` never comes.
#[test]
fn process_exit_in_the_entry_ends_the_app_without_an_error() {
    let (run, events) = logged(
        "process.on('uncaughtException', (e) => log('uncaught', e));
         if (!app.requestSingleInstanceLock({}) || true) process.exit(0);
         app.whenReady().then(() => log('window made'));",
    );
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(run.shell.exit_code(), Some(0));
    assert!(error_box(&run).is_none(), "{:?}", error_box(&run));
}

/// `app.on('window-all-closed', () => process.exit(0))` is the usual way to end an app: no
/// `Uncaught` message, no error box, no call of an `uncaughtException` listener.
#[test]
fn process_exit_in_a_listener_is_not_an_uncaught_error() {
    let (run, events) = logged(
        "process.on('uncaughtException', (e) => log('uncaught', e));
         app.on('window-all-closed', () => process.exit(0));
         app.whenReady().then(() => { new BrowserWindow({ show: false }).close(); });",
    );
    assert!(
        events.iter().all(|e| !e.starts_with("uncaught")),
        "{events:?}"
    );
    assert_eq!(run.shell.exit_code(), Some(0));
    assert!(error_box(&run).is_none());
    let console = run.console().join("\n");
    assert!(!console.contains("Uncaught"), "{console}");
}

/// A listener of `will-quit` or `before-quit` that throws is reported and has prevented nothing.
#[test]
fn a_quit_listener_that_throws_does_not_stop_the_quit() {
    for event in ["before-quit", "will-quit"] {
        let (run, _) = logged(&format!(
            "app.on('{event}', () => {{ throw new Error('listener'); }});
             app.whenReady().then(() => app.quit());"
        ));
        assert_eq!(run.shell.exit_code(), Some(0), "{event}");
        assert!(
            error_box(&run).is_some_and(|(_, text)| text.contains("listener")),
            "{event}"
        );
    }
}

/// Electron: a `quit()` from inside `will-quit` is a no-op, so the events are heard once.
#[test]
fn quit_from_a_will_quit_listener_is_a_no_op() {
    let (_, events) = logged(
        "app.on('before-quit', () => log('before-quit'));
         app.on('will-quit', () => { log('will-quit'); app.quit(); log('nested returned'); });
         app.whenReady().then(() => app.quit());",
    );
    assert_eq!(events, ["before-quit", "will-quit", "nested returned"]);
}

/// `app.exit(n)` inside `before-quit` ends the app with `n` and no `will-quit` follows; a
/// `preventDefault` after it does not bring the app back.
#[test]
fn exit_inside_before_quit_wins_over_the_quit_it_interrupted() {
    let (run, events) = logged(
        "app.on('before-quit', (e) => { log('before-quit'); app.exit(5); e.preventDefault(); });
         app.on('will-quit', () => log('will-quit'));
         app.on('quit', (e, code) => log('quit', code));
         app.whenReady().then(() => app.quit());",
    );
    assert_eq!(events, ["before-quit", "quit 5"]);
    assert_eq!(run.shell.exit_code(), Some(5));
}

/// Electron 39.8.10 with two windows, the first refusing to close: both are asked, the second
/// closes, the quit is off, and nothing says `will-quit`.
#[test]
fn a_refusing_window_does_not_stop_the_others_being_asked() {
    let (run, events) = logged(
        "app.on('will-quit', () => log('will-quit'));
         app.whenReady().then(() => {
           const make = (name, prevent) => {
             const w = new BrowserWindow({ show: false });
             w.on('close', (e) => { log(name, 'close'); if (prevent) e.preventDefault(); });
             w.on('closed', () => log(name, 'closed'));
           };
           make('A', true); make('B', false);
           setTimeout(() => { app.quit(); log('after quit'); setTimeout(() => app.exit(0), 300); }, 100);
         });",
    );
    assert_eq!(
        events,
        ["A close", "B close", "after quit", "B closed", "A closed"]
    );
    assert_eq!(run.shell.exit_code(), Some(0));
}

/// A new instance may start as the old one ends, so the lock is let go before `relaunch` starts it
/// (and the moment the app has said `quit`).
#[test]
fn the_single_instance_lock_is_released_as_the_app_ends() {
    let profile = App::scratch_dir("lock");
    let shared = profile.to_string_lossy().into_owned();
    let script = format!(
        "{LOG}log('got', app.requestSingleInstanceLock()); setTimeout(() => app.quit(), 200);"
    );
    let first = Run::script(&script, &[("user-data-dir", &shared)]);
    assert_eq!(first.exit_code(), Some(0));
    let again = Run::script(&script, &[("user-data-dir", &shared)]);
    assert_eq!(again.events(), ["got true"]);
    assert_eq!(first.events(), ["got true"]);
}

/// Electron 39.8.10: `setPath` does not make the directory, and a name it does not know fails with
/// `Failed to set path`.
#[test]
fn set_path_makes_no_directory_and_refuses_an_unknown_name() {
    let out = report(
        "const dir = require('path').join(app.getPath('temp'), 'verve-set-path-' + process.pid);
         app.setPath('exe', dir); out.exists = require('fs').existsSync(dir);
         try { app.setPath('bogus', dir); } catch (e) { out.bogus = e.message; }
         try { app.setPath('temp', 'relative'); } catch (e) { out.relative = e.message; }
         done();",
        &[],
    );
    assert_eq!(out["exists"], false);
    assert_eq!(out["bogus"], "Failed to set path");
    assert_eq!(out["relative"], "Path must be absolute");
}

/// Electron 39.8.10: an entry that throws still has its `uncaughtException` listener called (with
/// no origin), and `ready` still comes.
#[test]
fn an_error_thrown_as_the_entry_loads_reaches_the_uncaught_exception_listener() {
    let (run, events) = logged(
        "process.on('uncaughtException', (e, origin) => log('listener', e.message, origin));
         app.on('ready', () => { log('ready'); setTimeout(() => app.exit(0), 100); });
         throw new Error('entry-boom');",
    );
    // Electron hands the listener no origin here; Verve names it, as Node does.
    assert!(events[0].starts_with("listener entry-boom"), "{events:?}");
    assert!(events.contains(&"ready".to_string()), "{events:?}");
    assert!(error_box(&run).is_none());
}

/// A throwaway key, as `Scheme` makes, that holds an icon of the kind other software leaves.
#[cfg(windows)]
fn reg(args: &[&str]) -> bool {
    std::process::Command::new("reg.exe")
        .args(args)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Electron (`browser_win.cc`) removes `<protocol>\shell`, then the two values it set, and the key
/// itself only if it is empty then: a foreign `DefaultIcon` stays with its key. An empty protocol
/// is refused with `false`.
#[cfg(windows)]
#[test]
fn removing_a_protocol_client_leaves_what_is_not_the_apps() {
    let scheme = Scheme::new();
    let key = format!(r"HKCU\Software\Classes\{}", scheme.0);
    let out = report(
        &format!(
            "const p = '{}';
             out.empty = [app.setAsDefaultProtocolClient(''), app.isDefaultProtocolClient(''), app.removeAsDefaultProtocolClient('')];
             out.set = app.setAsDefaultProtocolClient(p);
             done();",
            scheme.0
        ),
        &[],
    );
    assert_eq!(out["empty"], serde_json::json!([false, false, false]));
    assert_eq!(out["set"], true);
    assert!(reg(&[
        "add",
        &format!(r"{key}\DefaultIcon"),
        "/ve",
        "/d",
        "icon.ico",
        "/f"
    ]));
    let out = report(
        &format!(
            "const p = '{}';
             out.removed = app.removeAsDefaultProtocolClient(p);
             out.stillThere = app.isDefaultProtocolClient(p);
             done();",
            scheme.0
        ),
        &[],
    );
    assert_eq!(
        (&out["removed"], &out["stillThere"]),
        (&true.into(), &false.into())
    );
    assert!(
        reg(&["query", &format!(r"{key}\DefaultIcon")]),
        "the icon stays"
    );
    assert!(
        !reg(&["query", &format!(r"{key}\shell")]),
        "the open command goes"
    );
    assert!(
        !reg(&["query", &key, "/v", "URL Protocol"]),
        "so does the value that made it a protocol"
    );
    assert!(reg(&["query", &key]), "the key stays for the icon");
}

/// Nothing else in the key: removing the client leaves no stub of it behind.
#[cfg(windows)]
#[test]
fn removing_a_protocol_client_that_is_alone_in_its_key_removes_the_key() {
    let scheme = Scheme::new();
    let key = format!(r"HKCU\Software\Classes\{}", scheme.0);
    let out = report(
        &format!(
            "const p = '{}';
             out.set = app.setAsDefaultProtocolClient(p);
             out.removed = app.removeAsDefaultProtocolClient(p);
             done();",
            scheme.0
        ),
        &[],
    );
    assert_eq!((&out["set"], &out["removed"]), (&true.into(), &true.into()));
    assert!(!reg(&["query", &key]), "the key is gone");
}

/// Electron 39.8.10: `app.quit()` in the entry runs its events at once, the script goes on, and
/// `will-finish-launching` and `ready` still come before the process ends.
#[test]
fn quit_in_the_entry_still_says_ready_before_the_app_ends() {
    let (run, events) = logged(
        "for (const n of ['will-finish-launching', 'ready', 'before-quit', 'will-quit', 'quit'])
           app.on(n, () => log('app', n));
         app.whenReady().then(() => log('whenReady'));
         process.on('exit', (c) => log('process exit', c));
         log('entry'); app.quit(); log('after quit');",
    );
    assert_eq!(
        events,
        [
            "entry",
            "app before-quit",
            "app will-quit",
            "process exit 0",
            "app quit",
            "after quit",
            "app will-finish-launching",
            "app ready",
            "whenReady",
        ]
    );
    assert_eq!(run.shell.exit_code(), Some(0));
}

/// Electron 39.8.10: before `ready`, `app.exit()` ends the process on the spot — no event, and
/// not another line of the script.
#[test]
fn exit_in_the_entry_ends_the_process_with_no_event_and_no_more_script() {
    let (run, events) = logged(
        "app.on('quit', () => log('quit')); process.on('exit', () => log('exit'));
         log('entry'); app.exit(2); log('after exit');",
    );
    assert_eq!(events, ["entry"]);
    assert_eq!(run.shell.exit_code(), Some(2));
}

/// An unhandled rejection in the main process prints a warning on the console and goes no
/// further: no error box, and the app lives on.
#[test]
fn an_unhandled_rejection_is_printed_and_nothing_more() {
    let run = Run::script(
        "require('electron').app.whenReady().then(() => Promise.reject(new Error('unhandled')));",
        &[],
    );
    let said = |app: &App| app.console().iter().any(|line| line.contains("unhandled"));
    assert!(run.run_until(said, TIMEOUT), "{:?}", run.console());
    std::thread::sleep(Duration::from_millis(200));
    assert!(error_box(&run).is_none());
    assert_eq!(run.shell.exit_code(), None);
}
