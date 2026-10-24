//! Native ESM regression tests, including the Latex Suite blob import/revoke sequence.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use boa_engine::{JsValue, builtins::promise::PromiseState, object::builtins::JsPromise};
use verve::Runtime;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/esm")
        .join(name)
}

fn loaded(runtime: &mut Runtime, source: &str) -> JsPromise {
    runtime.load_esm(&fixture("probe.mjs"), source).unwrap()
}

fn fulfilled(promise: &JsPromise) {
    assert!(
        matches!(promise.state(), PromiseState::Fulfilled(_)),
        "{:?}",
        promise.state()
    );
}

fn result(source: &str) -> String {
    let mut runtime = Runtime::new();
    let promise = loaded(&mut runtime, source);
    fulfilled(&promise);
    verve::print(&runtime.eval("JSON.stringify(globalThis.result)").unwrap())
}

#[test]
fn node_22_reference_packages_interop_and_error_codes() {
    let mut runtime = Runtime::new();
    let promise = runtime
        .load_esm(
            &fixture("node-reference.mjs"),
            include_str!("fixtures/esm/node-reference.mjs"),
        )
        .unwrap();
    fulfilled(&promise);
    let lines = runtime.take_console();
    let actual: serde_json::Value = serde_json::from_str(&lines.last().unwrap().text).unwrap();
    // Recorded with Node 22.21.1; the target contract is Node 22.20/Electron 39.
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/esm/node22.json")).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(
        runtime.timer_count(),
        0,
        "require(esm) must reject TLA before running any module"
    );
}

#[test]
fn native_import_api_fulfills_namespaces_and_rejects_resolution_errors() {
    let mut runtime = Runtime::new();
    let promise = runtime
        .import_module("./value.js", Some(&fixture("probe.mjs")))
        .unwrap();
    fulfilled(&promise);
    let promise = runtime
        .import_module("https://example.invalid/module.mjs", None)
        .unwrap();
    assert!(matches!(promise.state(), PromiseState::Rejected(_)));
}

#[test]
fn imports_patterns_external_packages_builtins_and_nested_null_conditions() {
    let mut runtime = Runtime::new();
    let promise = runtime
        .load_esm(
            &fixture("node-imports.mjs"),
            include_str!("fixtures/esm/node-imports.mjs"),
        )
        .unwrap();
    fulfilled(&promise);
    assert_eq!(
        runtime.take_console().last().unwrap().text,
        "[\"function\",42,\"feature\",\"import\",\"commonjs\",[\"ERR_PACKAGE_IMPORT_NOT_DEFINED\",\"ERR_PACKAGE_IMPORT_NOT_DEFINED\",\"MODULE_NOT_FOUND\",\"ERR_PACKAGE_PATH_NOT_EXPORTED\",\"MODULE_NOT_FOUND\"]]"
    );
}

#[test]
fn require_rejects_cjs_esm_back_edges_with_the_node_cycle_code() {
    let mut runtime = Runtime::new();
    runtime
        .load_module(
            &fixture("cycle-probe.cjs"),
            r#"
        try { require('./require-cycle.cjs'); globalThis.result = 'accepted'; }
        catch (error) { globalThis.result = error.code; }
    "#,
        )
        .unwrap();
    assert_eq!(
        verve::print(&runtime.eval("result").unwrap()),
        "ERR_REQUIRE_CYCLE_MODULE"
    );
}

#[test]
fn native_builtins_share_the_cjs_default_and_sync_only_existing_named_exports() {
    assert_eq!(
        result(
            r#"
        import fs, {readFileSync} from 'node:fs';
        import {createRequire, syncBuiltinESMExports} from 'node:module';
        const require = createRequire(import.meta.url);
        const before = readFileSync;
        fs.readFileSync = () => 'patched';
        fs.extra = 42;
        syncBuiltinESMExports();
        const namespace = await import('fs');
        globalThis.result = [fs === require('fs'), before !== readFileSync, readFileSync(), namespace.extra === undefined];
    "#
        ),
        "[true,true,\"patched\",true]"
    );
}

#[test]
fn cjs_named_exports_are_static_snapshots_and_default_is_the_whole_value() {
    assert_eq!(
        result(
            r#"
        import value, {name, __esModule, change} from './reexport.cjs';
        change();
        globalThis.result = [name, value.name, value.default, __esModule];
    "#
        ),
        "[\"commonjs\",\"changed\",\"not-the-namespace-default\",true]"
    );
}

#[test]
fn cjs_does_not_infer_dynamic_export_names() {
    assert_eq!(
        result(
            r#"
        const namespace = await import('./dynamic.cjs');
        globalThis.result = [namespace.computed === undefined, namespace.default.computed, Object.prototype.toString.call(namespace), Object.isExtensible(namespace)];
    "#
        ),
        "[true,42,\"[object Module]\",false]"
    );
}

#[test]
fn cjs_lexer_matches_nodes_conservative_objects_and_safe_getter_patterns() {
    assert_eq!(
        result(
            r#"
        const literal = await import('./object.cjs');
        const shorthand = await import('./object-shorthand.cjs');
        const descriptors = await import('./descriptors.cjs');
        globalThis.result = [Object.keys(literal), Object.keys(shorthand), Object.keys(descriptors), descriptors.safe];
    "#
        ),
        "[[\"default\"],[\"default\",\"five\"],[\"default\",\"safe\"],3]"
    );
}

#[test]
fn esm_cycles_keep_live_bindings() {
    assert_eq!(
        result(
            r#"
        import {readB} from './cycle-a.mjs';
        globalThis.result = readB();
    "#
        ),
        "1"
    );
}

#[test]
fn require_esm_does_not_drain_unrelated_microtasks_and_keeps_namespace_identity() {
    let mut runtime = Runtime::new();
    runtime.load_module(&fixture("sync.cjs"), r#"
        let ran = false;
        Promise.resolve().then(() => {ran = true});
        const first = require('./value.js');
        const before = first.count;
        first.increment();
        globalThis.result = [ran, before, first.count, first.__esModule, first === require('./value.js')];
    "#).unwrap();
    assert_eq!(
        verve::print(&runtime.eval("JSON.stringify(result)").unwrap()),
        "[false,1,2,true,true]"
    );
}

#[test]
fn package_conditions_are_ordered_and_custom_conditions_are_opt_in() {
    assert_eq!(
        result(
            r#"
        import {value} from 'verve-esm-fixture/ordered';
        import {value as fallback} from 'verve-esm-fixture/array';
        globalThis.result = [value, fallback];
    "#
        ),
        "[42,42]"
    );
    let mut runtime = Runtime::new();
    runtime.set_module_conditions(vec!["plugin".into()]);
    let promise = loaded(
        &mut runtime,
        "import value from 'verve-esm-fixture/conditions'; globalThis.result = value",
    );
    fulfilled(&promise);
    assert_eq!(verve::print(&runtime.eval("result").unwrap()), "custom");
}

#[test]
fn top_level_await_returns_to_the_host_and_resumes_on_its_clock() {
    let now = Rc::new(Cell::new(0.0));
    let clock = now.clone();
    let mut runtime = Runtime::with_clock(move || clock.get());
    let promise = loaded(
        &mut runtime,
        "import {data} from './tla-graph.mjs'; globalThis.result = data;",
    );
    assert!(matches!(promise.state(), PromiseState::Pending));
    assert_eq!(runtime.timer_count(), 1);
    now.set(10.0);
    assert_eq!(runtime.run_due(), 1);
    fulfilled(&promise);
    assert_eq!(verve::print(&runtime.eval("result").unwrap()), "42");
}

#[test]
fn cjs_wrappers_and_global_scripts_have_dynamic_import() {
    let mut runtime = Runtime::new();
    runtime
        .load_module(
            &fixture("dynamic-import.cjs"),
            "import('./value.js').then(m => globalThis.answer = m.value);",
        )
        .unwrap();
    assert_eq!(verve::print(&runtime.eval("answer").unwrap()), "42");
    let value = runtime.eval("import('data:text/javascript,export default 43').then(m => globalThis.answer = m.default)").unwrap();
    fulfilled(&JsPromise::from_object(value.as_object().unwrap()).unwrap());
    assert_eq!(verve::print(&runtime.eval("answer").unwrap()), "43");
}

#[test]
fn import_meta_has_file_url_native_paths_and_synchronous_resolve() {
    assert_eq!(
        result(
            r#"
        import path from 'node:path';
        const nativePath = process.platform === 'win32' ? path.win32 : path.posix;
        globalThis.result = [import.meta.url.startsWith('file:///'), nativePath.basename(import.meta.filename),
            nativePath.basename(import.meta.dirname), import.meta.resolve('./not-there.js').endsWith('/not-there.js'),
            import.meta.resolve('fs'), import.meta.resolve('#value').endsWith('/value.js')];
    "#
        ),
        "[true,\"probe.mjs\",\"esm\",true,\"node:fs\",true]"
    );
}

#[test]
fn json_attributes_are_validated_even_after_the_module_is_cached() {
    assert_eq!(
        result(
            r#"
        import value from './data.json' with {type: 'json'};
        const codes = [];
        for (const options of [undefined, {with:{type:'css'}}, {with:{unknown:'x'}}]) {
            try { await import('./data.json', options); } catch (error) { codes.push(error.code); }
        }
        try { await import('./value.js', {with:{type:'json'}}); } catch (error) { codes.push(error.code); }
        globalThis.result = [value.answer, codes];
    "#
        ),
        "[42,[\"ERR_IMPORT_ATTRIBUTE_MISSING\",\"ERR_IMPORT_ATTRIBUTE_UNSUPPORTED\",\"ERR_IMPORT_ATTRIBUTE_UNSUPPORTED\",\"ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE\"]]"
    );
}

#[test]
fn data_urls_support_javascript_base64_json_and_no_filesystem_relative_resolution() {
    assert_eq!(
        result(
            r#"
        const a = await import('data:text/javascript,export default 42');
        const b = await import('data:text/javascript;base64,ZXhwb3J0IGRlZmF1bHQgNDM=');
        const json = await import('data:application/json,%7B%22n%22%3A44%7D', {with:{type:'json'}});
        let code;
        try { await import('data:text/javascript,import "./value.js"'); } catch (e) {code = e.code;}
        globalThis.result = [a.default, b.default, json.default.n, code];
    "#
        ),
        "[42,43,44,\"ERR_UNSUPPORTED_RESOLVE_REQUEST\"]"
    );
}

#[test]
fn url_queries_and_fragments_are_distinct_cache_keys() {
    assert_eq!(
        result(
            r#"
        const a = await import('./counter.mjs?a');
        const again = await import('./counter.mjs?a');
        const b = await import('./counter.mjs?b');
        const c = await import('./counter.mjs?b#c');
        globalThis.result = [a === again, a.run, b.run, c.run];
    "#
        ),
        "[true,1,2,3]"
    );
}

#[test]
fn forget_module_clears_both_cjs_and_esm_views() {
    let mut runtime = Runtime::new();
    let first = runtime
        .require(fixture("counter.mjs").to_str().unwrap())
        .unwrap();
    runtime.forget_module(&fixture("counter.mjs"));
    let second = runtime
        .require(fixture("counter.mjs").to_str().unwrap())
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(verve::print(&runtime.eval("executions").unwrap()), "2");
    runtime
        .load_module(&fixture("cached.cjs"), "module.exports = 1")
        .unwrap();
    runtime.forget_module(&fixture("cached.cjs"));
    assert_eq!(
        verve::print(
            &runtime
                .load_module(&fixture("cached.cjs"), "module.exports = 2")
                .unwrap()
        ),
        "2"
    );
}

#[test]
fn extension_package_type_and_ambiguous_syntax_detection_match_node() {
    assert_eq!(
        result(
            r#"
        import common from './commonjs/value.js';
        import {answer} from './ambiguous/value.js';
        import {value} from './value.js';
        globalThis.result = [common, answer, value];
    "#
        ),
        "[42,42,42]"
    );
    let mut runtime = Runtime::new();
    let exports = runtime
        .load_module(&fixture("ambiguous/inline.js"), "export const answer = 42")
        .unwrap();
    assert_eq!(
        verve::print(
            &exports
                .as_object()
                .unwrap()
                .get(boa_engine::js_string!("answer"), runtime.context())
                .unwrap()
        ),
        "42"
    );
}

#[test]
fn failures_settle_and_esm_error_stacks_use_file_urls() {
    assert_eq!(
        result(
            r#"
        const results = [];
        for (const name of ['./syntax-error.mjs', './throws.mjs', 'node:not-a-builtin', './value%2f.js', './unknown.txt']) {
            try { await import(name); results.push('accepted'); }
            catch (e) { results.push(e.code || (e.message === 'esm-stack' ? e.stack.includes('file:///') : e.name === 'SyntaxError')); }
        }
        globalThis.result = results;
    "#
        ),
        "[true,true,\"ERR_UNKNOWN_BUILTIN_MODULE\",\"ERR_INVALID_MODULE_SPECIFIER\",\"ERR_UNKNOWN_FILE_EXTENSION\"]"
    );
}

#[test]
fn module_worker_runs_static_import_and_top_level_await() {
    let mut runtime = Runtime::new();
    runtime.eval(r#"
        globalThis.workerAnswer = 'pending';
        const url = URL.createObjectURL(new Blob([
            'import value from "data:text/javascript,export default 42"; await Promise.resolve(); postMessage(value);'
        ], {type:'text/javascript'}));
        globalThis.worker = new Worker(url, {type:'module'});
        worker.onmessage = event => { workerAnswer = event.data; worker.terminate(); };
        URL.revokeObjectURL(url);
    "#).unwrap();
    for _ in 0..100 {
        runtime.wait_for_workers(Duration::from_millis(20));
        runtime.run_workers();
        if verve::print(&runtime.eval("workerAnswer").unwrap()) != "pending" {
            break;
        }
    }
    assert_eq!(
        verve::print(&runtime.eval("workerAnswer").unwrap()),
        "42"
    );
}

#[test]
#[ignore = "Run npm ci in tests/fixtures/esm-packages, or set VERVE_ESM_PACKAGES; no install scripts are needed"]
fn real_pinned_nanoid5_chalk5_date_fns4_and_zod4() {
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            verve::set_thread_stack(32 * 1024 * 1024);
            let root = std::env::var_os("VERVE_ESM_PACKAGES")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/esm-packages")
                });
            let mut runtime = Runtime::new();
            let promise = runtime
                .load_esm(
                    &root.join("accept.mjs"),
                    include_str!("fixtures/esm-packages/accept.mjs"),
                )
                .unwrap();
            fulfilled(&promise);
            let lines = runtime.take_console();
            assert_eq!(
                lines.last().unwrap().text,
                "[true,true,\"2024-02-02\",true,false]"
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn dynamic_import_must_reject_instead_of_remaining_pending() {
    let mut runtime = Runtime::new();
    let value = runtime
        .eval("import('https://example.invalid/module.mjs')")
        .unwrap();
    let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
    match promise.state() {
        PromiseState::Rejected(reason) => {
            let code = reason
                .as_object()
                .unwrap()
                .get(boa_engine::js_string!("code"), runtime.context())
                .unwrap();
            assert_eq!(verve::print(&code), "ERR_UNSUPPORTED_ESM_URL_SCHEME");
        }
        state => panic!("import must reject with a Node error code, got {state:?}"),
    }
}

#[test]
fn latex_suite_blob_import_captures_bytes_before_immediate_revoke() {
    let mut runtime = Runtime::new();
    runtime.eval(r#"
        globalThis.answer = 'pending';
        const url = URL.createObjectURL(new Blob(['export default 42'], {type: 'text/javascript'}));
        globalThis.loading = import(url).then(m => answer = m.default, e => answer = e.code || e.message);
        URL.revokeObjectURL(url);
    "#).unwrap();
    assert_eq!(verve::print(&runtime.eval("answer").unwrap()), "42");
    let promise = runtime.eval("loading").unwrap();
    assert!(matches!(
        JsPromise::from_object(promise.as_object().unwrap())
            .unwrap()
            .state(),
        PromiseState::Fulfilled(_)
    ));
}

#[test]
fn invalid_or_already_revoked_blob_imports_reject_instead_of_hanging() {
    let mut runtime = Runtime::new();
    runtime
        .eval(
            r#"
        const invalid = URL.createObjectURL(new Blob(['export const = 1']));
        const gone = URL.createObjectURL(new Blob(['export default 42']));
        globalThis.invalidLoading = import(invalid);
        URL.revokeObjectURL(invalid);
        URL.revokeObjectURL(gone);
        globalThis.goneLoading = import(gone);
    "#,
        )
        .unwrap();
    for (name, expected) in [
        ("invalidLoading", "SyntaxError"),
        ("goneLoading", "ERR_MODULE_NOT_FOUND"),
    ] {
        let value = runtime.eval(name).unwrap();
        let promise = JsPromise::from_object(value.as_object().unwrap()).unwrap();
        let PromiseState::Rejected(reason) = promise.state() else {
            panic!("{name} must reject, got {:?}", promise.state());
        };
        let object = reason.as_object().unwrap();
        let key = if expected == "SyntaxError" {
            "name"
        } else {
            "code"
        };
        assert_eq!(
            verve::print(
                &object
                    .get(boa_engine::JsString::from(key), runtime.context())
                    .unwrap()
            ),
            expected
        );
    }
}

#[test]
fn boa_minimal_module_lifecycle() {
    // The engine-level reproduction intentionally uses the same module APIs as the runtime.
    let mut runtime = Runtime::new();
    let module = boa_engine::Module::parse(
        boa_engine::Source::from_bytes("export const value = 42"),
        None,
        runtime.context(),
    )
    .unwrap();
    let promise = module.load_link_evaluate(runtime.context());
    runtime.run_microtasks();
    assert!(
        matches!(promise.state(), PromiseState::Fulfilled(_)),
        "{:?}",
        promise.state()
    );
    assert_eq!(
        module
            .get_value(boa_engine::js_string!("value"), runtime.context())
            .unwrap(),
        JsValue::from(42)
    );
}
