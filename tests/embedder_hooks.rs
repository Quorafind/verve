//! The kernel's hooks for an embedder building Electron on it, a section for each: the structured
//! clone that crosses threads and the process a realm says it is.

use std::sync::{Arc, Mutex};

use boa_engine::JsValue;
use verve::clone::{CloneBytesError, Cloned};
use verve::{ExitBehavior, ProcessInfo, Runtime, UncaughtBehavior};

mod common;

use common::Dir;

// ─── A value between realms ───────────────────────────────────────────────────────────────

/// What a value made of `script` looks like when written out again in `other`, and again after
/// the trip through bytes.
fn round_trip(script: &str) -> (String, String) {
    let mut from = Runtime::new();
    let value = from.eval(script).unwrap();
    let cloned = Cloned::from_value(&value, from.context()).unwrap();
    let bytes = cloned.to_bytes().unwrap();
    let reread = Cloned::from_bytes(&bytes).unwrap();

    let describe = r#"
        (function (v) {
            return [
                v.text, v.number, v.big.toString(), v.date.getTime(),
                v.re.source + '/' + v.re.flags,
                v.map.get('k'), [...v.set].join(','), v.list.join(','), v.wrapped.deep,
                v.error.name + ': ' + v.error.message,
                v.cycle === v,
                v.u8.constructor.name + ':' + v.u8.join(','),
                v.i16.constructor.name + ':' + v.i16.join(','),
                v.view.constructor.name + ':' + v.view.byteLength,
                v.buffer.byteLength,
                v.blob instanceof Blob, v.blob.size, v.blob.type,
                v.file instanceof File, v.file.name, v.file.size,
                v.nested[0].b === v.nested[1].b,
            ].join('|');
        })
    "#;
    let mut to = Runtime::new();
    let describe = to.eval(describe).unwrap();
    let ask = |value: JsValue, to: &mut Runtime| {
        let text = to
            .call(&describe, &JsValue::undefined(), &[value])
            .unwrap_or_else(|e| panic!("{e}"));
        verve::print(&text)
    };
    let direct = ask(cloned.to_value(to.context()).unwrap(), &mut to);
    let written = ask(reread.to_value(to.context()).unwrap(), &mut to);
    (direct, written)
}

/// A value holding one of everything the clone carries, cycles and shared parts included.
const EVERYTHING: &str = r#"
    (() => {
        const shared = { b: 'shared' };
        const v = {
            text: 'héllo', number: 42.5, big: 9007199254740993n,
            date: new Date(1700000000000), re: /ab+c/gi,
            map: new Map([['k', 'v']]), set: new Set([1, 2]), list: [1, 2, 3],
            wrapped: { deep: 'd' }, error: new TypeError('bad'),
            u8: new Uint8Array([1, 2, 3]), i16: new Int16Array([-1, 2]),
            view: new DataView(new ArrayBuffer(4)), buffer: new ArrayBuffer(2),
            blob: new Blob(['abc'], { type: 'text/plain' }),
            file: new File(['xy'], 'n.txt', { lastModified: 5 }),
            nested: [{ b: shared }, { b: shared }],
        };
        v.cycle = v;
        return v;
    })()
"#;

const EVERYTHING_SEEN: &str = "héllo|42.5|9007199254740993|1700000000000|ab+c/gi|v|1,2|1,2,3|d|\
TypeError: bad|true|Uint8Array:1,2,3|Int16Array:-1,2|DataView:4|2|true|3|text/plain|true|n.txt|2|true";

fn assert_send<T: Send>() {}

// ─── The structured clone ───────────────────────────────────────────────────────────────────

#[test]
fn a_clone_carries_every_kind_of_value_between_realms() {
    assert_send::<Cloned>();
    let (direct, written) = round_trip(EVERYTHING);
    assert_eq!(direct, EVERYTHING_SEEN);
    assert_eq!(written, EVERYTHING_SEEN);
}

#[test]
fn a_clone_of_an_error_can_be_made_without_a_realm() {
    let mut rt = Runtime::new();
    let cloned = Cloned::from_error_message("Error", "it failed");
    let value = cloned.to_value(rt.context()).unwrap();
    let error = value.as_object().expect("an object");
    let name = error
        .get(boa_engine::js_string!("name"), rt.context())
        .unwrap();
    let message = error
        .get(boa_engine::js_string!("message"), rt.context())
        .unwrap();
    assert_eq!(verve::print(&name), "Error");
    assert_eq!(verve::print(&message), "it failed");
    // Bytes that are not a clone say what is wrong with them.
    assert_eq!(
        Cloned::from_bytes(&[9, 9, 9]).unwrap_err(),
        CloneBytesError::Truncated { at: 1 }
    );
    assert_eq!(
        Cloned::from_bytes(&[200]).unwrap_err(),
        CloneBytesError::UnknownTag { tag: 200, at: 0 }
    );
    assert_eq!(
        Cloned::from_bytes(&[0, 0]).unwrap_err(),
        CloneBytesError::Trailing { count: 1 }
    );
    // Bytes from another process do not get to recurse without end: arrays inside arrays.
    let nested: Vec<u8> = (0..100_000)
        .flat_map(|_| [12, 1, 0, 0, 0])
        .chain([0])
        .collect();
    assert!(matches!(
        Cloned::from_bytes(&nested).unwrap_err(),
        CloneBytesError::TooDeep { .. }
    ));
}

#[test]
fn what_cannot_be_cloned_is_a_data_clone_error() {
    let mut rt = Runtime::new();
    let function = rt.eval("(function f() {})").unwrap();
    let denied = Cloned::from_value(&function, rt.context()).unwrap_err();
    assert!(verve::describe(rt.context(), &denied).contains("DataCloneError"));
    let symbol = rt.eval("Symbol('s')").unwrap();
    assert!(Cloned::from_value(&symbol, rt.context()).is_err());
}

// ─── The process a realm is ─────────────────────────────────────────────────────────────────

#[test]
fn process_info_shapes_the_process_object() {
    let mut rt = Runtime::new();
    let exit_seen = Arc::new(Mutex::new(Vec::new()));
    let heard_exit = exit_seen.clone();
    rt.set_process_info(ProcessInfo {
        title: "my app".into(),
        process_type: Some("browser".into()),
        argv: vec!["electron.exe".into(), "app".into()],
        argv0: "electron".into(),
        exec_path: r"C:\app\electron.exe".into(),
        exec_argv: vec!["--trace".into()],
        resources_path: Some(r"C:\app\resources".into()),
        default_app: true,
        electron_fields: true,
        sandboxed: Some(false),
        context_isolated: None,
        exit: ExitBehavior::Host(Arc::new(move |code| heard_exit.lock().unwrap().push(code))),
        on_uncaught: UncaughtBehavior::Report(Arc::new(|_| ())),
    })
    .unwrap();
    let answer = rt
        .eval(
            "[process.type, process.title, process.argv.join(','), process.execArgv.join(','), \
             process.resourcesPath, process.defaultApp, process.versions.electron, \
             'contextIsolated' in process, 'sandboxed' in process].join('|')",
        )
        .unwrap();
    assert_eq!(
        verve::print(&answer),
        r"browser|my app|electron.exe,app|--trace|C:\app\resources|true|39.2.0|false|false"
    );
    // A renderer, as Electron's is: context isolation said, sandboxing said, no app path.
    rt.set_process_info(ProcessInfo {
        process_type: Some("renderer".into()),
        context_isolated: Some(true),
        sandboxed: Some(true),
        resources_path: None,
        electron_fields: false,
        ..ProcessInfo::default()
    })
    .unwrap();
    let answer = rt
        .eval(
            "[process.type, process.sandboxed, process.contextIsolated, \
             String(process.versions.electron), 'resourcesPath' in process].join('|')",
        )
        .unwrap();
    assert_eq!(verve::print(&answer), "renderer|true|true|undefined|false");
    // Node's own process, as `verve run` makes it: no type at all.
    rt.set_process_info(ProcessInfo {
        process_type: None,
        title: "verve".into(),
        electron_fields: false,
        ..ProcessInfo::default()
    })
    .unwrap();
    let answer = rt
        .eval("[process.title, 'type' in process].join('|')")
        .unwrap();
    assert_eq!(verve::print(&answer), "verve|false");
}

#[test]
fn process_exit_asks_the_host_and_stops_the_script() {
    let mut rt = Runtime::new();
    let codes = Arc::new(Mutex::new(Vec::new()));
    let heard = codes.clone();
    rt.set_process_info(ProcessInfo {
        exit: ExitBehavior::Host(Arc::new(move |code| heard.lock().unwrap().push(code))),
        ..ProcessInfo::default()
    })
    .unwrap();
    // The `exit` event is heard first, then the host, then the script ends uncatchably — at its
    // next call, which is all a host's `app.exit` can promise.
    let outcome = rt.eval(
        "globalThis.exited = false; \
         process.on('exit', () => { globalThis.exited = true }); \
         process.exit(3); \
         (function later() { globalThis.after = true })();",
    );
    assert!(outcome.is_err(), "the script goes on after process.exit");
    assert_eq!(*codes.lock().unwrap(), [3]);
    assert_eq!(verve::print(&rt.eval("globalThis.exited").unwrap()), "true");
    assert_eq!(
        verve::print(&rt.eval("globalThis.after").unwrap()),
        "undefined"
    );
}

#[test]
fn the_default_process_is_the_plugin_hosts_unchanged() {
    let mut rt = Runtime::new();
    let answer = rt
        .eval(
            "[process.type, process.title, 'sandboxed' in process, 'resourcesPath' in process, \
             'noAsar' in process, Object.keys(process).filter((k) => !k.startsWith('_')).join(',')].join('|')",
        )
        .unwrap();
    let text = verve::print(&answer);
    assert!(
        text.starts_with("renderer|obsidian|false|false|false|"),
        "{text}"
    );
    // The keys keep the order they were always made in.
    let keys = text.rsplit('|').next().unwrap();
    let at = |name: &str| keys.split(',').position(|k| k == name).expect(name);
    assert!(at("title") < at("version"));
    assert!(at("execPath") < at("type"));
    assert!(at("type") < at("exitCode"));
    assert!(at("noDeprecation") < at("argv"));
    assert!(at("env") < at("versions"));
}

#[test]
fn a_host_exit_honours_the_exit_code_a_script_set() {
    let mut rt = Runtime::new();
    let codes = Arc::new(Mutex::new(Vec::new()));
    let heard = codes.clone();
    rt.set_process_info(ProcessInfo {
        exit: ExitBehavior::Host(Arc::new(move |code| heard.lock().unwrap().push(code))),
        ..ProcessInfo::default()
    })
    .unwrap();
    let _ = rt.eval("process.exitCode = 2; process.exit();");
    assert_eq!(*codes.lock().unwrap(), [2]);
}

#[test]
fn a_noted_exit_sets_the_code_and_goes_on() {
    let mut rt = Runtime::new();
    rt.set_process_info(ProcessInfo {
        exit: ExitBehavior::Note,
        ..ProcessInfo::default()
    })
    .unwrap();
    let answer = rt
        .eval(
            "[(() => { process.on('exit', (c) => { globalThis.heard = c }); \
              process.exit(7); return 'went on' })(), process.exitCode, globalThis.heard].join('|')",
        )
        .unwrap();
    assert_eq!(verve::print(&answer), "went on|7|7");
    // The warning a plugin's exit is noted with, on the console the host takes.
    assert!(
        rt.take_console()
            .iter()
            .any(|line| line.text.contains("process.exit(7) ignored")),
        "no warning"
    );
    // The realm still runs: nothing was interrupted.
    assert_eq!(verve::print(&rt.eval("1 + 1").unwrap()), "2");
}

#[test]
fn an_uncaught_error_goes_to_the_host_that_asked() {
    let mut rt = Runtime::new();
    let lines = Arc::new(Mutex::new(Vec::new()));
    let heard = lines.clone();
    rt.set_process_info(ProcessInfo {
        on_uncaught: UncaughtBehavior::Report(Arc::new(move |line| {
            heard.lock().unwrap().push(line.to_owned())
        })),
        ..ProcessInfo::default()
    })
    .unwrap();
    rt.eval("queueMicrotask(() => { throw new Error('boom') })")
        .unwrap();
    let lines = lines.lock().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("boom"), "{lines:?}");
    // And the realm goes on.
    assert_eq!(verve::print(&rt.eval("1 + 1").unwrap()), "2");
}

#[test]
fn verve_run_still_ends_the_process_itself() {
    let dir = Dir::new("exit");
    let script = dir.0.join("exit.js");
    std::fs::write(
        &script,
        "process.on('exit', () => console.log('bye')); process.exit(3);",
    )
    .unwrap();
    let ran = std::process::Command::new(env!("CARGO_BIN_EXE_verve"))
        .arg(&script)
        .current_dir(&dir.0)
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), "bye");
}

// ─── The rest of the hooks ─────────────────────────────────────────────────────────────────
