//! The kernel's hooks for an embedder building Electron on it, a section for each: the structured
//! clone that crosses threads, the process a realm says it is, the virtual filesystem `app.asar`
//! is read through, the permission bit a child process needs and the natives a host defines its
//! own modules with.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use boa_engine::JsValue;
use verve::clone::{CloneBytesError, Cloned};
use verve::vfs::{DirEntry, Kind, Metadata, Overlay};
use verve::{ExitBehavior, Permissions, ProcessInfo, Runtime, UncaughtBehavior};

mod common;

use common::Dir;

// ─── A realm to run things in ───────────────────────────────────────────────────────────────

/// A realm with `require` on its global object, as Electron's renderer has `window.require`.
fn realm() -> Runtime {
    let mut rt = Runtime::new();
    let require = rt.global_require();
    rt.context()
        .register_global_property(
            boa_engine::js_string!("require"),
            require,
            boa_engine::property::Attribute::all(),
        )
        .unwrap();
    rt
}

/// Runs `body` as `main.js` in `dir` (whose path it has as `__dirname`), lets what it started
/// settle, then reads `exports.result`.
fn run_in(rt: &mut Runtime, dir: &Path, body: &str) -> Vec<String> {
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    // Streams and promises settle over several turns: microtasks, then timers, again.
    for _ in 0..10 {
        rt.run_microtasks();
        rt.run_due();
    }
    common::result_of(rt, exports)
}

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

// ─── The virtual filesystem ─────────────────────────────────────────────────────────────────

/// An overlay of bytes in memory, as a packed `app.asar` is: the paths under `root` are its own
/// and nothing else is. Keys are the paths themselves, so this holds on every platform the tests
/// run on.
struct Memory {
    /// How many times `read` was asked for `data.txt`.
    reads: Arc<AtomicUsize>,
    root: PathBuf,
    files: HashMap<PathBuf, Vec<u8>>,
    dirs: Vec<PathBuf>,
}

impl Memory {
    fn new(root: &Path) -> Self {
        Self {
            reads: Arc::new(AtomicUsize::new(0)),
            root: root.to_path_buf(),
            files: HashMap::new(),
            dirs: Vec::new(),
        }
    }

    fn file(mut self, path: &str, bytes: &[u8]) -> Self {
        let path = self.root.join(path);
        let mut at = path.parent();
        while let Some(dir) = at.filter(|dir| dir.starts_with(&self.root)) {
            self.dirs.push(dir.to_path_buf());
            at = dir.parent();
        }
        self.files.insert(path, bytes.to_vec());
        self
    }

    /// The path as this overlay names it, when it is under its root at all.
    fn under(&self, path: &Path) -> Option<PathBuf> {
        path.starts_with(&self.root).then(|| path.to_path_buf())
    }

    fn answer<T>(&self, path: &Path, found: impl FnOnce(&Path) -> Option<T>) -> Option<T> {
        found(&self.under(path)?)
    }

    fn known(&self, path: &Path) -> bool {
        self.files.contains_key(path) || self.dirs.iter().any(|d| d == path)
    }
}

fn missing(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is not in the archive", path.display()),
    )
}

impl Overlay for Memory {
    fn stat(&self, path: &Path) -> Option<io::Result<Metadata>> {
        self.answer(path, |name| {
            Some(
                match (self.files.get(name), self.dirs.iter().any(|d| d == name)) {
                    (Some(bytes), _) => {
                        Ok(Metadata::new(Kind::File, bytes.len() as u64, 0., 0o100644))
                    }
                    (None, true) => Ok(Metadata::new(Kind::Dir, 0, 0., 0o040755)),
                    (None, false) => Err(missing(name)),
                },
            )
        })
    }

    fn read(&self, path: &Path) -> Option<io::Result<Cow<'static, [u8]>>> {
        self.answer(path, |name| {
            if name.ends_with("data.txt") {
                self.reads.fetch_add(1, Ordering::SeqCst);
            }
            Some(match self.files.get(name) {
                Some(bytes) => Ok(Cow::Owned(bytes.clone())),
                None if self.dirs.iter().any(|d| d == name) => {
                    Err(io::Error::from(io::ErrorKind::IsADirectory))
                }
                None => Err(missing(name)),
            })
        })
    }

    fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>> {
        self.answer(path, |name| {
            if !self.dirs.iter().any(|d| d == name) {
                return Some(Err(missing(name)));
            }
            let mut entries = Vec::new();
            for file in self.files.keys() {
                if let (Some(dir), Some(rest)) = (file.parent(), file.file_name())
                    && dir == name
                {
                    entries.push(DirEntry::new(
                        rest.to_string_lossy().into_owned(),
                        Kind::File,
                    ));
                }
            }
            for dir in &self.dirs {
                if let (Some(parent), Some(rest)) = (dir.parent(), dir.file_name())
                    && parent == name
                    && !rest.is_empty()
                {
                    entries.push(DirEntry::new(
                        rest.to_string_lossy().into_owned(),
                        Kind::Dir,
                    ));
                }
            }
            Some(Ok(entries))
        })
    }

    fn realpath(&self, path: &Path) -> Option<io::Result<PathBuf>> {
        self.answer(path, |name| {
            Some(if self.known(name) {
                Ok(name.to_path_buf())
            } else {
                Err(missing(name))
            })
        })
    }

    fn write_denied(&self, path: &Path) -> Option<io::Error> {
        self.under(path)
            .map(|_| io::Error::new(io::ErrorKind::Unsupported, "EROFS: read-only file system"))
    }

    fn error_message(&self, err: &io::Error, path: &Path) -> Option<String> {
        let name = self.under(path)?;
        (err.kind() == io::ErrorKind::NotFound).then(|| {
            format!(
                "ENOENT: no such file or directory, open '{}' not found in {}",
                name.display(),
                self.root.display()
            )
        })
    }
}

/// A tree as `app.asar` has it, with `require` of its files, its packages and its `fs`.
fn packed(root: &Path) -> Memory {
    Memory::new(root)
        .file("app.asar/data.txt", b"packed bytes")
        .file(
            "app.asar/package.json",
            br#"{"name":"app","main":"./main.js"}"#,
        )
        .file(
            "app.asar/main.js",
            b"module.exports = 'main of the archive';",
        )
        .file("x.js", b"module.exports = 'x of the archive';")
        .file("x.mjs", b"export default 'esm of the archive';")
        .file(
            "node_modules/pkg/package.json",
            br#"{"name":"pkg","exports":{"./thing":"./thing.js"}}"#,
        )
        .file(
            "node_modules/pkg/thing.js",
            b"module.exports = 'thing of pkg';",
        )
}

#[test]
fn the_overlay_answers_for_what_it_holds() {
    let dir = Dir::new("overlay");
    let mut rt = realm();
    let root = dir.0.clone();
    rt.set_vfs(Some(Arc::new(packed(&root))));
    let result = run_in(
        &mut rt,
        &root,
        r#"
        const fs = require('fs');
        const path = require('path');
        const data = path.join(__dirname, 'app.asar', 'data.txt');
        const result = [];
        result.push(fs.readFileSync(data, 'utf8'));
        result.push(fs.readdirSync(path.join(__dirname, 'app.asar')).sort().join(','));
        result.push(fs.statSync(data).isFile() + ':' + fs.statSync(data).size);
        result.push(String(fs.existsSync(data)) + String(fs.existsSync(path.join(__dirname, 'nope'))));
        const fd = fs.openSync(data, 'r');
        result.push('fd:' + fs.fstatSync(fd).size);
        fs.closeSync(fd);
        fs.promises.readFile(data, 'utf8').then((text) => result.push('promise:' + text));
        result.push(require('./x'));
        result.push(require('./app.asar'));
        result.push(require('pkg/thing'));
        try { fs.readFileSync(path.join(__dirname, 'app.asar', 'missing.txt'), 'utf8'); }
        catch (e) { result.push(e.code + '|' + e.message.includes('not found in')); }
        try { fs.writeFileSync(data, 'x'); } catch (e) { result.push('write:' + e.code); }
        const stream = fs.createReadStream(data, { encoding: 'utf8' });
        let streamed = '';
        stream.on('data', (chunk) => { streamed += chunk; });
        stream.on('end', () => {
            result.push('stream:' + streamed);
            exports.result = result;
        });
        "#,
    );
    assert_eq!(
        result,
        [
            "packed bytes",
            "data.txt,main.js,package.json",
            "true:12",
            "truefalse",
            "fd:12",
            "x of the archive",
            "main of the archive",
            "thing of pkg",
            "ENOENT|true",
            "write:ENOTSUP",
            "promise:packed bytes",
            "stream:packed bytes",
        ]
    );
}

/// `import()` of a file the disk does not hold: the loader asks the overlay too.
#[test]
fn an_import_reads_the_overlay() {
    let dir = Dir::new("import");
    let mut rt = realm();
    let root = dir.0.clone();
    rt.set_vfs(Some(Arc::new(packed(&root))));
    let result = run_in(
        &mut rt,
        &root,
        r#"
        const result = [];
        import('./x.mjs').then(
            (module) => { result.push('esm:' + module.default); exports.result = result; },
            (error) => { result.push('rejected:' + (error.code || error.message)); exports.result = result; },
        );
        "#,
    );
    assert_eq!(result, ["esm:esm of the archive"]);
}

#[test]
fn original_fs_and_no_asar_read_the_disk_alone() {
    let dir = Dir::new("original");
    let mut rt = realm();
    let root = dir.0.clone();
    rt.set_vfs(Some(Arc::new(packed(&root))));
    let data = root.join("app.asar").join("data.txt");
    let answer = rt
        .eval(&format!(
            r#"(() => {{
                const fs = require('fs');
                const original = require('original-fs');
                let code;
                try {{ original.readFileSync({path:?}, 'utf8'); }} catch (e) {{ code = e.code; }}
                return [fs !== original, code].join('|');
            }})()"#,
            path = data.to_string_lossy()
        ))
        .unwrap();
    assert_eq!(verve::print(&answer), "true|ENOENT");
    // `process.noAsar`: the same realm reads the disk alone from now on.
    rt.set_no_asar(true);
    let denied = rt.eval(&format!(
        "require('fs').readFileSync({:?}, 'utf8')",
        data.to_string_lossy()
    ));
    assert!(denied.is_err());
}

/// What `original-fs` reads, a stream it makes keeps reading, however late it does.
#[test]
fn an_original_fs_stream_never_reads_the_overlay() {
    let dir = Dir::new("original-stream");
    let mut rt = realm();
    let root = dir.0.clone();
    rt.set_vfs(Some(Arc::new(packed(&root))));
    let result = run_in(
        &mut rt,
        &root,
        r#"
        const original = require('original-fs');
        const fs = require('fs');
        const path = require('path');
        const data = path.join(__dirname, 'app.asar', 'data.txt');
        const result = [];
        const through = fs.createReadStream(data, { encoding: 'utf8' });
        through.on('data', (chunk) => result.push('fs:' + chunk));
        const bare = original.createReadStream(data, { encoding: 'utf8' });
        bare.on('data', (chunk) => result.push('original:' + chunk));
        bare.on('error', (error) => result.push('original:' + error.code));
        bare.on('close', () => { exports.result = result.sort(); });
        "#,
    );
    assert_eq!(result, ["fs:packed bytes", "original:ENOENT"]);
}

/// An overlay is asked about the path made absolute against the realm's folder and clean, not
/// as the caller wrote it.
#[test]
fn the_overlay_is_asked_about_absolute_clean_paths() {
    let dir = Dir::new("relative");
    let mut rt = realm();
    let root = dir.0.clone();
    rt.set_vfs(Some(Arc::new(packed(&root))));
    let result = run_in(
        &mut rt,
        &root,
        r#"
        process.chdir(__dirname);
        const fs = require('fs');
        exports.result = [
            fs.readFileSync('app.asar/data.txt', 'utf8'),
            fs.readFileSync('./app.asar/../app.asar/./data.txt', 'utf8'),
            String(fs.existsSync('app.asar/missing.txt')),
        ];
        "#,
    );
    assert_eq!(result, ["packed bytes", "packed bytes", "false"]);
}

#[test]
fn process_no_asar_puts_the_overlay_aside_from_javascript() {
    let dir = Dir::new("no-asar");
    let mut rt = realm();
    let root = dir.0.clone();
    rt.set_vfs(Some(Arc::new(packed(&root))));
    let result = run_in(
        &mut rt,
        &root,
        r#"
        const fs = require('fs');
        const data = require('path').join(__dirname, 'app.asar', 'data.txt');
        const result = [String(process.noAsar), fs.readFileSync(data, 'utf8')];
        process.noAsar = true;
        try { fs.readFileSync(data, 'utf8'); } catch (e) { result.push(e.code); }
        process.noAsar = false;
        result.push(fs.readFileSync(data, 'utf8'));
        exports.result = result;
        "#,
    );
    assert_eq!(result, ["false", "packed bytes", "ENOENT", "packed bytes"]);
    // With the overlay taken away there is nothing to put aside: the accessor goes too.
    rt.set_vfs(None);
    let answer = rt.eval("'noAsar' in process").unwrap();
    assert_eq!(verve::print(&answer), "false");
}

/// A file stream asks an overlay for its file once, not once for each chunk.
#[test]
fn an_overlay_file_is_read_once_for_all_the_chunks_of_a_stream() {
    let dir = Dir::new("chunks");
    let mut rt = realm();
    let root = dir.0.clone();
    let memory = packed(&root);
    let reads = memory.reads.clone();
    rt.set_vfs(Some(Arc::new(memory)));
    let result = run_in(
        &mut rt,
        &root,
        r#"
        const fs = require('fs');
        const data = require('path').join(__dirname, 'app.asar', 'data.txt');
        const chunks = [];
        fs.createReadStream(data, { highWaterMark: 4, encoding: 'utf8' })
            .on('data', (chunk) => chunks.push(chunk))
            .on('end', () => { exports.result = [chunks.length, chunks.join('')]; });
        "#,
    );
    assert_eq!(result, ["3", "packed bytes"]);
    assert_eq!(reads.load(Ordering::SeqCst), 1);
}

#[test]
fn original_fs_shares_fs_classes() {
    let mut rt = realm();
    let answer = rt
        .eval(
            "(() => { const fs = require('fs'); const original = require('original-fs');              return [original.Stats === fs.Stats, original.statSync('.') instanceof fs.Stats,              fs.statSync('.') instanceof original.Stats, original.Dirent === fs.Dirent].join('|'); })()",
        )
        .unwrap();
    assert_eq!(verve::print(&answer), "true|true|true|true");
}

// ─── The rest of the hooks ─────────────────────────────────────────────────────────────────

#[test]
fn the_child_permission_bit_is_reported() {
    let mut rt = Runtime::new();
    assert_eq!(
        verve::print(&rt.eval("process.permission.has('child')").unwrap()),
        "false"
    );
    rt.set_permissions(Permissions {
        child_process: true,
        ..Permissions::none()
    });
    assert_eq!(
        verve::print(&rt.eval("process.permission.has('child')").unwrap()),
        "true"
    );
    // `-A` grants no child processes: running a program is not one of the grants.
    assert!(!Permissions::all().child_process);
}

#[test]
fn an_embedder_can_build_an_event_emitter_class() {
    let mut rt = realm();
    let context = rt.context();
    let events = verve::require(context, "events").unwrap();
    let emitter = verve::native::get(&events, "EventEmitter", context).unwrap();
    let emitter = emitter.as_object().expect("a class");
    let (class, _prototype) = verve::native::class(
        context,
        verve::native::ClassSpec {
            name: "Window",
            parent: Some(&emitter),
            init: |this, _, context| {
                let events = verve::require(context, "events")?;
                let emitter = verve::native::get(&events, "EventEmitter", context)?;
                verve::call(context, &emitter, &this.clone().into(), &[])?;
                verve::native::hidden(this, boa_engine::js_string!("id"), 1, context)
            },
            ..verve::native::ClassSpec::default()
        },
    )
    .unwrap();
    context
        .register_global_property(boa_engine::js_string!("Window"), class, Default::default())
        .unwrap();
    rt.eval("var w = new Window(); var heard = []; w.on('ping', (a, b) => heard.push(a + b));")
        .unwrap();
    let window = rt.eval("w").unwrap();
    verve::events::emit_event(
        rt.context(),
        &window,
        "ping",
        &[JsValue::from(1), JsValue::from(2)],
    )
    .unwrap();
    let answer = rt
        .eval("[w instanceof require('events').EventEmitter, heard.join(','), w.id].join('|')")
        .unwrap();
    assert_eq!(verve::print(&answer), "true|3|1");
}
