//! CommonJS as Node has it — built-ins by either name, a plugin's own files, `require.cache`,
//! `require.resolve`, `require('module')` — and `process` as Electron's renderer has it.

use std::path::{Path, PathBuf};

use runtime_x::RuntimeX;

/// A folder of its own for one test, with `files` written in it.
fn folder(test: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("runtime-x-modules-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (name, text) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// Runs `body` as `main.js` in `dir`; it sets `exports.result` to an array.
fn run_in(rt: &mut RuntimeX, dir: &Path, body: &str) -> Vec<String> {
    let exports = rt
        .load_module(&dir.join("main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text)).unwrap();
    parsed["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn built_ins_answer_to_both_names_and_to_a_trailing_slash() {
    let dir = folder("builtins", &[]);
    let mut rt = RuntimeX::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        exports.result = [
            require('node:path') === require('path'),
            require('node:process') === process,
            require('process') === globalThis.process,
            require('node:events') === require('events'),
            require('original-fs') === require('fs'),
            require('module').isBuiltin('node:fs'),
            require('module').isBuiltin('left-pad'),
            require.resolve('node:fs'),
            (() => { try { require('left-pad'); return 'found' } catch (e) { return e.code } })(),
            (() => { try { require(5); return 'found' } catch (e) { return e.code } })(),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "false",
            "node:fs",
            "MODULE_NOT_FOUND",
            "ERR_INVALID_ARG_TYPE"
        ]
    );
}

#[test]
fn a_plugins_own_files_folders_packages_and_json() {
    let dir = folder(
        "files",
        &[
            (
                "lib/a.js",
                "exports.name = 'a'; exports.dir = __dirname; exports.file = __filename;",
            ),
            ("lib/data.json", "{ \"n\": 3 }"),
            ("lib/pkg/package.json", "{ \"main\": \"./dist/entry\" }"),
            ("lib/pkg/dist/entry.js", "module.exports = 'pkg main';"),
            ("lib/idx/index.js", "module.exports = 'index';"),
            (
                "node_modules/dep/package.json",
                "{ \"exports\": { \".\": { \"require\": \"./cjs.js\" } } }",
            ),
            (
                "node_modules/dep/cjs.js",
                "module.exports = 'dep ' + require('../../lib/a').name;",
            ),
            (
                "lib/cycle-a.js",
                "exports.early = 1; const b = require('./cycle-b'); exports.b = b.saw;",
            ),
            (
                "lib/cycle-b.js",
                "exports.saw = require('./cycle-a').early;",
            ),
            (
                "lib/count.js",
                "globalThis.runs = (globalThis.runs || 0) + 1; module.exports = runs;",
            ),
        ],
    );
    let mut rt = RuntimeX::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const path = require('path');
        const a = require('./lib/a');
        const first = require('./lib/count.js');
        const again = require('./lib/count');
        const key = require.resolve('./lib/count');
        delete require.cache[key];
        const fresh = require('./lib/count');
        exports.result = [
            a.name,
            path.basename(a.dir),
            path.basename(a.file),
            require('./lib/data.json').n,
            require('./lib/data').n,
            require('./lib/pkg'),
            require('./lib/idx'),
            require('dep'),
            require('./lib/cycle-a').b,
            first, again, fresh,
            require.cache[require.resolve('./lib/a')].exports === a,
            module.children.length > 0,
            typeof module.paths[0],
            require('module').createRequire(path.join(__dirname, 'lib', 'x.js'))('./a') === a,
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "a", "lib", "a.js", "3", "3", "pkg main", "index", "dep a", "1", "1", "1", "2", "true",
            "true", "string", "true"
        ]
    );
}

#[test]
fn process_looks_like_electrons_renderer() {
    let dir = folder("process", &[]);
    let mut rt = RuntimeX::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const EventEmitter = require('events');
        const heard = [];
        process.on('warning', w => heard.push(w.name + ':' + w.message));
        process.emitWarning('careful', 'DeprecationWarning');
        process.stdout.write('to the console\n');
        const t = process.hrtime();
        const d = process.hrtime(t);
        exports.result = [
            process.version.startsWith('v22.'),
            typeof process.versions.node,
            typeof process.versions.electron,
            process.type,
            typeof process.browser,
            process instanceof EventEmitter,
            typeof process.hrtime.bigint(),
            d.length,
            typeof process.memoryUsage().heapUsed,
            typeof process.uptime(),
            typeof process.cwd(),
            typeof process.env,
            typeof process.pid,
            ['x64', 'arm64', 'ia32'].includes(process.arch),
            heard.join(','),
            process.stdout.isTTY,
            (() => { try { process.binding('fs'); return 'bound' } catch (e) { return 'no binding' } })(),
            (() => { process.exit(3); return process.exitCode })(),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "true",
            "string",
            "string",
            "renderer",
            "undefined",
            "true",
            "bigint",
            "2",
            "number",
            "number",
            "string",
            "object",
            "number",
            "true",
            "DeprecationWarning:careful",
            "false",
            "no binding",
            "3",
        ]
    );
    let console = rt.take_console();
    assert!(console.iter().any(|l| l.text == "to the console"));
    assert!(
        console
            .iter()
            .any(|l| l.text.contains("DeprecationWarning: careful"))
    );
}

#[test]
fn node_classes_extend_three_ways() {
    let dir = folder("classes", &[]);
    let mut rt = RuntimeX::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const EventEmitter = require('events');
        const util = require('util');
        // ES5, as old npm code writes it.
        function Old() { EventEmitter.call(this); this.mine = true; }
        util.inherits(Old, EventEmitter);
        // A class.
        class New extends EventEmitter { constructor() { super(); this.mine = true; } }
        // A mixin: called on an object that does not inherit it.
        const mixed = { own: 1 };
        EventEmitter.call(mixed);
        const results = [];
        for (const e of [new Old(), new New(), new EventEmitter()]) {
            let got = 0;
            e.on('x', v => got += v);
            e.emit('x', 2);
            results.push(String(got) + (e instanceof EventEmitter) + (e.mine === true));
        }
        exports.result = results.concat([typeof mixed._events, Old.super_ === EventEmitter]);
    "#,
    );
    assert_eq!(
        result,
        ["2truetrue", "2truetrue", "2truefalse", "object", "true"]
    );
}

/// A global made on first use, read again where the first read was — a constructor called in a
/// loop — is still a constructor: Boa's cached lookup of a global otherwise loses `new.target`
/// once the global changes from an accessor to a value.
#[test]
fn a_lazy_global_constructs_again_and_again() {
    let dir = folder("lazy", &[]);
    let mut rt = RuntimeX::new();
    let result = run_in(
        &mut rt,
        &dir,
        r#"
        const made = [];
        for (const i of [1, 2, 3]) made.push(new TextDecoderStream() instanceof TextDecoderStream);
        const before = globalThis.ReadableStream;
        globalThis.ReadableStream = 'replaced';
        exports.result = made.concat([typeof before, ReadableStream]);
    "#,
    );
    assert_eq!(result, ["true", "true", "true", "function", "replaced"]);
}

/// The host requires from native code, and hands scripts a `window.require` of no module's —
/// built-ins by either name, its own (`electron`) among them, files by absolute path.
#[test]
fn the_host_requires_natively_and_gives_scripts_a_window_require() {
    let dir = folder(
        "native-require",
        &[("lib/x.js", "module.exports = 'x file';")],
    );
    let mut rt = RuntimeX::new();
    let electron = rt.eval("({ remote: 'stub' })").unwrap();
    rt.define_builtin("electron", electron);
    let fs = rt.require("node:original-fs").unwrap();
    let again = rt.require("fs").unwrap();
    assert!(fs == again);
    assert!(
        rt.require("nope")
            .unwrap_err()
            .contains("Cannot find module 'nope'")
    );
    let require = rt.global_require();
    rt.context()
        .register_global_property(
            boa_engine::js_string!("windowRequire"),
            require,
            boa_engine::property::Attribute::all(),
        )
        .unwrap();
    let file = dir.join("lib").join("x.js");
    let result = run_in(
        &mut rt,
        &dir,
        &format!(
            r#"
            exports.result = [
                windowRequire('electron').remote,
                windowRequire('node:fs') === require('fs'),
                windowRequire({file:?}),
                typeof windowRequire.resolve,
                typeof require('node:sqlite').DatabaseSync,
                (() => {{ try {{ new (require('node:sqlite').DatabaseSync)(':memory:'); return 'made' }} catch (e) {{ return e.code }} }})(),
                (() => {{ try {{ require('sqlite'); return 'found' }} catch (e) {{ return e.code }} }})(),
            ];
        "#,
            file = file.to_string_lossy()
        ),
    );
    assert_eq!(
        result,
        [
            "stub",
            "true",
            "x file",
            "function",
            "function",
            "ERR_FEATURE_UNAVAILABLE_ON_PLATFORM",
            "MODULE_NOT_FOUND"
        ]
    );
}
