//! `node:vm` contexts as Node makes them: realms of their own, whose globals are the
//! contextified object's and the realm's builtins and nothing of the runtime's — what Rebon's
//! Code Mode relies on to keep a program from `process`.

use std::cell::Cell;
use std::rc::Rc;

use verve::Runtime;

/// Runs `body` as a module (with `vm` and `log` in scope), then the host's loop for a while
/// on a clock the test moves; answers `log`, one entry per line.
fn run(body: &str) -> Vec<String> {
    let time = Rc::new(Cell::new(0.));
    let read = Rc::clone(&time);
    let mut rt = Runtime::with_clock(move || read.get());
    rt.eval("globalThis.log = []").unwrap();
    let dir = std::env::temp_dir().join(format!("verve-vm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = format!(
        "const vm = require('vm');\n(async () => {{\n{body}\n}})().catch(e => log.push('threw ' + (e && e.stack || e)));"
    );
    rt.load_module(&dir.join("main.js"), &source)
        .unwrap_or_else(|e| panic!("{e}"));
    for step in 0..50 {
        time.set(f64::from(step) * 10.);
        rt.run_due();
        rt.run_microtasks();
    }
    let log = verve::print(&rt.eval("log.join('\\n')").unwrap());
    log.lines().map(str::to_owned).collect()
}

fn expect(body: &str, expected: &[&str]) {
    let log = run(body);
    assert_eq!(log, expected, "\nwhole log:\n{}", log.join("\n"));
}

#[test]
fn a_context_has_its_own_globals_and_intrinsics() {
    expect(
        r#"
        // A null-prototype object, as Code Mode's: one inheriting from this realm's
        // `Object.prototype` hands its `constructor` — this realm's `Object` — to the context,
        // as it does on Node.
        const ctx = vm.createContext(Object.create(null));
        const r = (code) => vm.runInContext(code, ctx);
        log.push(r('typeof process'), r('typeof require'), r('typeof console'),
            r('typeof setTimeout'), r('typeof queueMicrotask'), r('typeof module'));
        log.push(String(r('globalThis.process')), String(r('this === globalThis')));
        const inner = r('globalThis');
        log.push(String(inner !== globalThis), String(r('Object') !== Object),
            String(r('Function') !== Function), String(r('Promise') !== Promise));
        // The escape that works on a host object reaches only the context here.
        log.push(String(r('({}).constructor.constructor("return this")()') === inner));
        log.push(String(r('this.constructor.constructor("return typeof process")()')));
        // Its own language builtins, and the ones V8 has in every context.
        log.push(r('typeof JSON.parse + typeof Intl + typeof WebAssembly + typeof Error.captureStackTrace'));
        let caught;
        try { r('require("fs")'); } catch (e) { caught = e.name; }
        log.push(caught);
        "#,
        &[
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "true",
            "true",
            "true",
            "true",
            "true",
            "true",
            "undefined",
            "functionobjectobjectfunction",
            "ReferenceError",
        ],
    );
}

#[test]
fn the_contextified_object_is_the_global_scope_both_ways() {
    expect(
        r#"
        const sandbox = { x: 2, shadow: 'mine' };
        const ctx = vm.createContext(sandbox);
        log.push(String(vm.isContext(sandbox)), String(vm.isContext({})));
        log.push(String(vm.runInContext('x * 21', ctx)));
        vm.runInContext('var declared = 1; assigned = 2; globalThis.viaGlobal = 3; function named() {}', ctx);
        log.push(sandbox.declared, sandbox.assigned, sandbox.viaGlobal, typeof sandbox.named);
        // What the host adds afterwards is there too.
        Object.defineProperty(sandbox, 'later', { value: 'late', enumerable: true });
        log.push(vm.runInContext('later', ctx));
        sandbox.x = 10;
        log.push(String(vm.runInContext('x + declared', ctx)));
        // The object's own name hides the builtin; deleting it is seen.
        sandbox.Object = 'not the builtin';
        log.push(vm.runInContext('Object', ctx));
        delete sandbox.Object;
        log.push(vm.runInContext('typeof Object', ctx));
        log.push(String(vm.runInContext('delete assigned', ctx)), String('assigned' in sandbox));
        log.push(vm.runInContext('Object.keys(globalThis).join()', ctx));
        log.push(String(vm.runInContext('undefined = 1; typeof undefined', ctx)));
        log.push(String(vm.runInContext('"later" in globalThis && Object.getOwnPropertyDescriptor(globalThis, "later").value', ctx)));
        // Nothing of the runtime's global shows through.
        globalThis.shared = 'host';
        log.push(vm.runInContext('typeof shared', ctx));
        log.push(String(vm.runInNewContext('a + b', { a: 1, b: 2 })));
        log.push(String(vm.runInNewContext('typeof process')));
        "#,
        &[
            "true",
            "false",
            "42",
            "1",
            "2",
            "3",
            "function",
            "late",
            "11",
            "not the builtin",
            "function",
            "true",
            "false",
            "x,shadow,named,declared,viaGlobal,later",
            "undefined",
            "late",
            "undefined",
            "3",
            "undefined",
        ],
    );
}

#[test]
fn values_cross_with_their_own_realm() {
    expect(
        r#"
        const ctx = vm.createContext({});
        const list = vm.runInContext('[1, 2]', ctx);
        log.push(String(Array.isArray(list)), String(list instanceof Array),
            String(list instanceof vm.runInContext('Array', ctx)));
        const plain = vm.runInContext('({ a: 1 })', ctx);
        log.push(String(Object.getPrototypeOf(plain) === vm.runInContext('Object.prototype', ctx)));
        try { vm.runInContext('null.x', ctx); } catch (e) {
            log.push(e.name, String(e instanceof TypeError), String(e instanceof vm.runInContext('TypeError', ctx)));
        }
        try { vm.runInContext('throw new RangeError("mine")', ctx); } catch (e) {
            log.push(e.message, String(e instanceof vm.runInContext('RangeError', ctx)));
        }
        try { vm.runInContext('let = = 1', ctx); } catch (e) { log.push(e.name); }
        try { new vm.Script('(').runInContext(ctx); } catch (e) { log.push(e.name); }
        "#,
        &[
            "true", "false", "true", "true", "TypeError", "false", "true", "mine", "true",
            "SyntaxError", "SyntaxError",
        ],
    );
}

#[test]
fn code_generation_from_strings_can_be_refused() {
    expect(
        r#"
        const ctx = vm.createContext(Object.create(null), { codeGeneration: { strings: false } });
        const attempts = [
            'eval("1")',
            '(0, eval)("1")',
            'new Function("return 1")',
            'Function("return 1")',
            '(async () => {}).constructor("return 1")',
            '(function* () {}).constructor("yield 1")',
            'this.constructor.constructor("return process")()',
            '[].constructor.constructor("return this")()',
        ];
        for (const code of attempts) {
            try { vm.runInContext(code, ctx); log.push('ran ' + code); }
            catch (e) { log.push(e.name + ': ' + e.message + ' ' + String(e instanceof vm.runInContext('EvalError', ctx))); }
        }
        // `eval` of what is not a string is no code generation.
        log.push(String(vm.runInContext('eval(5)', ctx)));
        // Allowed by default, and the runtime's own realm is untouched.
        log.push(String(vm.runInNewContext('eval("6 * 7")')), String(eval('1 + 1')));
        // The host's compileFunction compiles in the context regardless.
        const f = vm.compileFunction('return typeof eval', [], { parsingContext: ctx });
        log.push(f());
        "#,
        &[
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "EvalError: Code generation from strings disallowed for this context true",
            "5",
            "42",
            "2",
            "function",
        ],
    );
}

#[test]
fn webassembly_can_be_refused() {
    expect(
        r#"
        const empty = 'new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0])';
        const refused = vm.createContext({}, { codeGeneration: { wasm: false } });
        const r = (code) => vm.runInContext(code, refused);
        log.push(String(r(`WebAssembly.validate(${empty})`)));
        try { r(`new WebAssembly.Module(${empty})`); } catch (e) { log.push(e.name + ': ' + e.message); }
        await r(`WebAssembly.compile(${empty})`).then(() => log.push('compiled'), e => log.push('compile ' + e.name));
        await r(`WebAssembly.instantiate(${empty})`).then(() => log.push('instantiated'), e => log.push('instantiate ' + e.name));
        // Strings stay allowed: the flags are separate.
        log.push(String(r('eval("1")')));
        const allowed = vm.createContext({});
        const m = vm.runInContext(`new WebAssembly.Module(${empty})`, allowed);
        log.push(String(m instanceof vm.runInContext('WebAssembly.Module', allowed)));
        "#,
        &[
            "true",
            "CompileError: Wasm code generation disallowed by embedder",
            "compile CompileError",
            "instantiate CompileError",
            "1",
            "true",
        ],
    );
}

#[test]
fn promises_and_async_functions_in_a_context_settle() {
    expect(
        r#"
        const sandbox = { hostValue: (n) => Promise.resolve(n * 2) };
        const ctx = vm.createContext(sandbox);
        const result = await vm.runInContext(`(async () => {
            const a = await Promise.resolve(1);
            const b = await hostValue(20);
            const c = await new Promise((resolve) => Promise.resolve().then(() => resolve(21)));
            return [a, b, c];
        })()`, ctx);
        log.push(JSON.stringify(result));
        const rejected = vm.runInContext('Promise.reject(new TypeError("no"))', ctx);
        await rejected.catch(e => log.push(e.message, String(e instanceof vm.runInContext('TypeError', ctx))));
        // A function the context made, called by the host later, still runs in the context.
        const later = vm.runInContext('() => typeof process + " " + (async () => 7)().constructor.name', ctx);
        log.push(later());
        // Microtasks the host hands in run.
        sandbox.queueMicrotask = queueMicrotask;
        vm.runInContext('queueMicrotask(() => { ticked = true })', ctx);
        await null;
        log.push(String(sandbox.ticked));
        "#,
        &["[1,40,21]", "no", "true", "undefined Promise", "true"],
    );
}

#[test]
fn compile_function_takes_params_and_context_extensions() {
    expect(
        r#"
        const ctx = vm.createContext({ base: 100 });
        const add = vm.compileFunction('return a + b + base', ['a', 'b'], { parsingContext: ctx });
        log.push(String(add(1, 2)));
        log.push(String(Object.getPrototypeOf(add) === vm.runInContext('Function.prototype', ctx)));
        const ext = vm.compileFunction('return a + extra + base', ['a'], {
            parsingContext: ctx, contextExtensions: [{ extra: 10 }, { base: 1000 }],
        });
        log.push(String(ext(1)));
        log.push(String(vm.compileFunction('"use strict"; return this', [])()));
        log.push(String(vm.compileFunction('return a * 2', ['a'])(21)));
        try { vm.compileFunction('}); process.exit(); ({', [], { parsingContext: ctx }); } catch (e) { log.push(e.name); }
        "#,
        &["103", "true", "1011", "undefined", "42", "SyntaxError"],
    );
}

#[test]
fn a_script_past_its_timeout_is_stopped() {
    expect(
        r#"
        const ctx = vm.createContext({});
        try { vm.runInContext('for (;;) {}', ctx, { timeout: 30 }); } catch (e) { log.push(e.code, e.message); }
        try { new vm.Script('while (true) {}').runInContext(ctx, { timeout: 20 }); } catch (e) { log.push(e.code); }
        try { vm.runInThisContext('for (;;) {}', { timeout: 20 }); } catch (e) { log.push(e.code); }
        // What runs next runs, there and here.
        log.push(String(vm.runInContext('1 + 1', ctx, { timeout: 1000 })), String(2 + 2));
        try { vm.runInContext('1', ctx, { timeout: -1 }); } catch (e) { log.push(e.code); }
        "#,
        &[
            "ERR_SCRIPT_EXECUTION_TIMEOUT",
            "Script execution timed out after 30ms",
            "ERR_SCRIPT_EXECUTION_TIMEOUT",
            "ERR_SCRIPT_EXECUTION_TIMEOUT",
            "2",
            "4",
            "ERR_OUT_OF_RANGE",
        ],
    );
}

#[test]
fn dynamic_import_in_a_context_goes_to_its_callback() {
    expect(
        r#"
        const ctx = vm.createContext({});
        await vm.runInContext('import("node:fs")', ctx)
            .then(ns => log.push('loaded ' + typeof ns.readFileSync), e => log.push(e.code));
        const script = new vm.Script('import("node:fs")', {
            importModuleDynamically() { throw new Error('dynamic import is disabled'); },
        });
        await script.runInContext(ctx).then(() => log.push('loaded'), e => log.push(e.message));
        const own = vm.createContext({}, {
            importModuleDynamically(specifier) { throw new Error('no ' + specifier); },
        });
        await vm.runInContext('(async () => import("node:process"))()', own)
            .then(() => log.push('loaded'), e => log.push(e.message));
        // The runtime's own import is untouched.
        log.push(typeof (await import('node:path')).join);
        "#,
        &[
            "ERR_VM_DYNAMIC_IMPORT_CALLBACK_MISSING",
            "dynamic import is disabled",
            "no node:process",
            "function",
        ],
    );
}

#[test]
fn a_buffer_the_allocator_refuses_is_a_range_error() {
    expect(
        r#"
        for (const make of [() => new ArrayBuffer(2 ** 40), () => new Uint8Array(2 ** 34)]) {
            try { make(); log.push('made'); } catch (e) { log.push(e.name + ': ' + e.message); }
        }
        "#,
        &[
            "RangeError: Array buffer allocation failed",
            "RangeError: Array buffer allocation failed",
        ],
    );
}
