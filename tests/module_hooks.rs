//! `module.registerHooks`: resolve and load hooks as a host uses them — an import map over a
//! vendored payload of `.js` modules with no `package.json`, source handed to the loader for a
//! file it could not read itself, hooks chained, and hooks taken out again.

use std::path::PathBuf;

use boa_engine::{builtins::promise::PromiseState, object::builtins::JsPromise};
use verve::Runtime;

/// A folder of its own for each test, with `files` written into it.
fn folder(test: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("verve-hooks-{}-{test}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (name, text) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// Runs `main` as the module `main.mjs` in `dir` until it settles; answers `globalThis.result`
/// as JSON, or the rejection.
fn run(dir: &PathBuf, main: &str) -> Result<String, String> {
    let mut runtime = Runtime::new();
    let promise: JsPromise = runtime.load_esm(&dir.join("main.mjs"), main).unwrap();
    for _ in 0..20 {
        runtime.run_microtasks();
    }
    match promise.state() {
        PromiseState::Fulfilled(_) => Ok(verve::print(
            &runtime.eval("JSON.stringify(globalThis.result)").unwrap(),
        )),
        PromiseState::Rejected(error) => Err(verve::print(
            &runtime
                .call_with(&error, "String(e && e.code) + ' ' + String(e && e.message)")
                .unwrap(),
        )),
        PromiseState::Pending => Err("pending".into()),
    }
}

trait CallWith {
    fn call_with(
        &mut self,
        value: &boa_engine::JsValue,
        body: &str,
    ) -> Result<boa_engine::JsValue, String>;
}

impl CallWith for Runtime {
    /// Runs `body` as an expression of `e`, the value given.
    fn call_with(
        &mut self,
        value: &boa_engine::JsValue,
        body: &str,
    ) -> Result<boa_engine::JsValue, String> {
        let f = self.eval(&format!("(e) => {body}"))?;
        self.call(&f, &boa_engine::JsValue::undefined(), &[value.clone()])
    }
}

#[test]
fn an_import_map_over_a_payload_of_untyped_js_modules() {
    // `.js` files under no `package.json`, in ESM syntax: Node reads them as CommonJS, so the
    // host's load hook says they are modules.
    let dir = folder(
        "map",
        &[
            ("payload/vendor/cordis/index.js", "import { tag } from 'cosmokit';\nexport const name = 'cordis/' + tag;\n"),
            ("payload/vendor/cosmokit/index.js", "export const tag = 'cosmokit';\n"),
        ],
    );
    let payload = url::Url::from_directory_path(dir.join("payload")).unwrap().to_string();
    let main = format!(
        r#"
        import {{ registerHooks }} from 'node:module';
        const payload = {payload:?};
        const map = new Map([
            ['cordis', payload + 'vendor/cordis/index.js'],
            ['cosmokit', payload + 'vendor/cosmokit/index.js'],
        ]);
        const seen = [];
        registerHooks({{
            resolve(specifier, context, next) {{
                const mapped = map.get(specifier);
                if (mapped !== undefined) {{
                    seen.push(specifier + ' from ' + context.parentURL.split('/').pop());
                    return {{ url: mapped, format: 'module', shortCircuit: true }};
                }}
                return next(specifier, context);
            }},
            load(url, context, next) {{
                if (url.startsWith(payload)) return next(url, {{ ...context, format: 'module' }});
                return next(url, context);
            }},
        }});
        const cordis = await import('cordis');
        const fs = await import('node:fs');
        globalThis.result = {{ name: cordis.name, seen, fs: typeof fs.readFileSync }};
        "#
    );
    assert_eq!(
        run(&dir, &main).unwrap(),
        r#"{"name":"cordis/cosmokit","seen":["cordis from main.mjs","cosmokit from index.js"],"fs":"function"}"#
    );
}

#[test]
fn a_load_hook_hands_the_loader_source_it_could_not_read() {
    let dir = folder(
        "source",
        &[("mod/thing.ts", "export const answer: number = 42;\n")],
    );
    let main = r#"
        import { registerHooks } from 'node:module';
        import { readFileSync } from 'node:fs';
        import { fileURLToPath } from 'node:url';
        registerHooks({
            load(url, context, next) {
                if (!url.endsWith('.ts')) return next(url, context);
                // A stand-in for stripping types: this one annotation.
                const text = readFileSync(fileURLToPath(url), 'utf8').replace(': number', '');
                return { format: 'module', source: new TextEncoder().encode(text), shortCircuit: true };
            },
        });
        const { answer } = await import('./mod/thing.ts');
        globalThis.result = answer;
    "#;
    assert_eq!(run(&dir, main).unwrap(), "42");
}

#[test]
fn hooks_chain_last_registered_first_and_deregister() {
    let dir = folder(
        "chain",
        &[("a.mjs", "export default 'a';\n"), ("b.mjs", "export default 'b';\n")],
    );
    let main = r#"
        import { registerHooks } from 'node:module';
        const order = [];
        registerHooks({
            resolve(specifier, context, next) {
                order.push('first ' + specifier);
                return next(specifier === 'x' ? './a.mjs' : specifier, context);
            },
        });
        const second = registerHooks({
            resolve(specifier, context, next) {
                order.push('second ' + specifier);
                return next(specifier === 'y' || specifier === 'z' ? 'x' : specifier, context);
            },
        });
        const viaBoth = (await import('y')).default;
        second.deregister();
        const viaFirst = (await import('x')).default;
        // `z` is new, so it is resolved now, without the hook that mapped it.
        let refused;
        try { await import('z'); } catch (e) { refused = e.code; }
        // `y` is not: this module already imported it, and gets the same module again.
        const again = (await import('y')).default;
        const resolved = import.meta.resolve('x').split('/').pop();
        globalThis.result = { viaBoth, viaFirst, refused, again, resolved, order };
    "#;
    assert_eq!(
        run(&dir, main).unwrap(),
        r#"{"viaBoth":"a","viaFirst":"a","refused":"ERR_MODULE_NOT_FOUND","again":"a","resolved":"a.mjs","order":["second y","first x","first x","first z","first x"]}"#
    );
}

#[test]
fn a_hook_answering_the_wrong_shape_is_refused_by_name() {
    let dir = folder("shape", &[]);
    let bad_url = r#"
        import { registerHooks } from 'node:module';
        registerHooks({ resolve: () => ({ url: 42 }) });
        await import('anything');
    "#;
    assert!(
        run(&dir, bad_url)
            .unwrap_err()
            .starts_with("ERR_INVALID_RETURN_PROPERTY_VALUE"),
    );
    let bad_format = r#"
        import { registerHooks } from 'node:module';
        registerHooks({ load: (url, context, next) => url.endsWith('x.mjs') ? { format: 'wasm-ish', source: '' } : next(url, context) });
        await import('./x.mjs');
    "#;
    let dir = folder("format", &[("x.mjs", "export {};\n")]);
    assert!(
        run(&dir, bad_format)
            .unwrap_err()
            .starts_with("ERR_UNKNOWN_MODULE_FORMAT"),
    );
    let not_a_function = r#"
        import { registerHooks } from 'node:module';
        try { registerHooks({ resolve: 1 }); } catch (e) { globalThis.result = e.code; }
    "#;
    assert_eq!(run(&dir, not_a_function).unwrap(), r#""ERR_INVALID_ARG_TYPE""#);
}

#[test]
fn require_keeps_commonjs_resolution_unhooked() {
    let dir = folder("require", &[("lib.cjs", "module.exports = 'file';\n")]);
    let main = r#"
        import { registerHooks, createRequire } from 'node:module';
        let calls = 0;
        registerHooks({ resolve(specifier, context, next) { calls++; return next(specifier, context); } });
        const require = createRequire(import.meta.url);
        globalThis.result = { value: require('./lib.cjs'), calls };
    "#;
    assert_eq!(run(&dir, main).unwrap(), r#"{"value":"file","calls":0}"#);
}

#[test]
fn strip_typescript_types_erases_types_in_place() {
    let dir = folder("strip", &[]);
    let main = r#"
        import { stripTypeScriptTypes } from 'node:module';
        let refused;
        try { stripTypeScriptTypes('enum E { A }'); } catch (e) { refused = e.code; }
        globalThis.result = { js: stripTypeScriptTypes('let x: number = 1'), refused };
    "#;
    assert_eq!(
        run(&dir, main).unwrap(),
        r#"{"js":"let x         = 1","refused":"ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX"}"#
    );
}
