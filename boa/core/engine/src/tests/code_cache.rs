//! Modules written down by `Module::code_cache` and read back by `Module::from_code_cache` run
//! as the modules they were parsed from, and bytes that are not such a module are refused
//! without a panic.

use std::{path::Path, rc::Rc};

use crate::{Context, JsValue, Module, Source, js_string, module::MapModuleLoader};
use indoc::indoc;

const DEP: &str = indoc! {r#"
    export const base = 40;
    export function add(a, b = 2) { return a + b; }
    export let counter = 0;
    export function bump() { counter++; }
    export default class Point {
        #x;
        constructor(x) { this.#x = x; }
        get x() { return this.#x; }
        static of(x) { return new Point(x); }
    }
"#};

const MAIN: &str = indoc! {r#"
    import Point, { base, add, counter, bump } from 'dep';
    import * as ns from 'dep';

    const log = [];
    function* gen() { yield 1; yield* [2, 3]; }
    async function later(x) { await null; return x * 2; }
    const tag = (strings, ...values) => strings.raw.join('|') + values.join(',');
    label: for (const i of gen()) { if (i === 2) continue label; log.push(i); }
    try { null.prop; } catch (e) { log.push(e.constructor.name); }
    const { a, ...rest } = { a: 1, b: 2, c: 3 };
    log.push(a, Object.keys(rest).join(''));
    log.push(add(base), add(1, 1), Point.of(7).x, ns.base, 10n ** 20n);
    bump(); bump();
    log.push(counter);
    log.push(tag`x${1}y${2}`, /a(b+)c/.exec('xabbbc')[1], `${base}`.padStart(4, '0'));
    switch (log.length) { case 0: log.push('empty'); break; default: log.push('full'); }
    log.push((() => { try { return eval('base + 2'); } finally { log.push('finally'); } })());
    log.push(add.toString(), typeof Point, Point.name, add.length);
    let stack;
    function thrower() { throw new Error('here'); }
    try { thrower(); } catch (e) { stack = e.stack.split('\n').slice(0, 2).join(' / '); }
    log.push(stack);
    export const result = log.join(';');
    export const pending = later(21);
"#};

/// A context whose modules are `dep` (from `DEP`) and whatever else is inserted.
fn context(lazy: bool) -> (Context, Rc<MapModuleLoader>) {
    let loader = Rc::new(MapModuleLoader::default());
    let mut context = Context::builder()
        .module_loader(loader.clone())
        .build()
        .unwrap();
    context.set_lazy_compile(lazy);
    (context, loader)
}

fn parse(source: &str, name: &str, context: &mut Context) -> Module {
    Module::parse(
        Source::from_bytes(source).with_path(Path::new(name)),
        None,
        context,
    )
    .unwrap()
}

/// Evaluates `module` and answers its `result` export, and what its `pending` export settled
/// to.
fn evaluate(module: &Module, context: &mut Context) -> String {
    let promise = module.load_link_evaluate(context);
    context.run_jobs().unwrap();
    if let Some(error) = promise.state().as_rejected() {
        return format!("rejected: {}", error.display());
    }
    let namespace = module.namespace(context);
    let result = namespace.get(js_string!("result"), context).unwrap();
    let result = result.to_string(context).unwrap().to_std_string_escaped();
    let pending = namespace.get(js_string!("pending"), context).unwrap();
    let settled = pending
        .as_promise()
        .and_then(|promise| promise.state().as_fulfilled().cloned())
        .unwrap_or_else(JsValue::undefined);
    format!("{result} => {}", settled.display())
}

/// Runs `MAIN` (importing `DEP`) parsed, and again from the code each wrote down.
fn parsed_and_cached(lazy: bool) -> (String, String) {
    let (mut context, loader) = context(lazy);
    let dep = parse(DEP, "dep.mjs", &mut context);
    let dep_bytes = dep
        .code_cache(&mut context)
        .unwrap()
        .expect("dep can be written down");
    loader.insert("dep", dep);
    let main = parse(MAIN, "main.mjs", &mut context);
    let main_bytes = main
        .code_cache(&mut context)
        .unwrap()
        .expect("main can be written down");
    let parsed = evaluate(&main, &mut context);

    let (mut context, loader) = self::context(lazy);
    let dep = Module::from_code_cache(
        &dep_bytes,
        DEP,
        Some(Path::new("dep.mjs")),
        None,
        &mut context,
    )
    .unwrap();
    loader.insert("dep", dep);
    let main = Module::from_code_cache(
        &main_bytes,
        MAIN,
        Some(Path::new("main.mjs")),
        None,
        &mut context,
    )
    .unwrap();
    let cached = evaluate(&main, &mut context);
    (parsed, cached)
}

#[test]
fn cached_modules_run_as_parsed_ones() {
    for lazy in [false, true] {
        let (parsed, cached) = parsed_and_cached(lazy);
        assert!(!parsed.starts_with("rejected"), "{parsed}");
        assert_eq!(cached, parsed, "lazy: {lazy}");
        assert!(
            parsed.contains("at thrower (main.mjs:"),
            "stack traces name the module's path: {parsed}"
        );
    }
}

#[test]
fn code_is_refused_for_another_source() {
    let (mut context, _) = context(false);
    let module = parse("export const x = 1;", "a.mjs", &mut context);
    let bytes = module.code_cache(&mut context).unwrap().unwrap();
    let (mut context, _) = self::context(false);
    let error = Module::from_code_cache(&bytes, "export const x = 2;", None, None, &mut context)
        .unwrap_err();
    assert!(error.to_string().contains("another source text"), "{error}");
    assert!(
        Module::from_code_cache(&bytes, "export const x = 1;", None, None, &mut context).is_ok()
    );
}

#[test]
fn code_is_refused_against_another_global_scope() {
    // Compiled where a script declared a global `let`, the module finds it by its index in the
    // global scope; a realm without it must compile the module again.
    let source = "export const seen = typeof shared === 'undefined' ? 'none' : shared;";
    let (mut context, _) = context(false);
    context
        .eval(Source::from_bytes("let shared = 'declared';"))
        .unwrap();
    let module = parse(source, "g.mjs", &mut context);
    let bytes = module.code_cache(&mut context).unwrap().unwrap();

    let (mut context, _) = self::context(false);
    let error = Module::from_code_cache(&bytes, source, None, None, &mut context).unwrap_err();
    assert!(error.to_string().contains("global scope"), "{error}");

    let (mut context, _) = self::context(false);
    context
        .eval(Source::from_bytes("let shared = 'again';"))
        .unwrap();
    let module = Module::from_code_cache(&bytes, source, None, None, &mut context).unwrap();
    let promise = module.load_link_evaluate(&mut context);
    context.run_jobs().unwrap();
    assert!(promise.state().as_fulfilled().is_some());
    let seen = module
        .namespace(&mut context)
        .get(js_string!("seen"), &mut context)
        .unwrap();
    assert_eq!(seen, js_string!("again").into());
}

#[test]
fn only_unlinked_source_modules_are_written_down() {
    let (mut context, _) = context(false);
    let module = parse("export const x = 1;", "a.mjs", &mut context);
    let promise = module.load_link_evaluate(&mut context);
    context.run_jobs().unwrap();
    assert!(promise.state().as_fulfilled().is_some());
    assert!(module.code_cache(&mut context).unwrap().is_none());
}

/// Every byte changed, and every truncation, of a small module's code is either refused or
/// read back: never a panic. (What a changed byte reads back as is not checked; bytes that
/// are well formed are trusted, and an embedder authenticates them.)
#[test]
fn damaged_code_never_panics() {
    let source = indoc! {r#"
        export function f(a, b) { return [a, b, `t${a}`, 1n, /x/g, { a }]; }
        export class C { m() { return super.toString(); } }
        export const v = f(1, 2);
    "#};
    let (mut context, _) = context(false);
    let module = parse(source, "d.mjs", &mut context);
    let bytes = module.code_cache(&mut context).unwrap().unwrap();
    let (mut context, _) = self::context(false);
    assert!(Module::from_code_cache(&bytes, source, None, None, &mut context).is_ok());
    let mut refused = 0;
    for index in 0..bytes.len() {
        for flip in [0x01u8, 0x80, 0xff] {
            let mut damaged = bytes.clone();
            damaged[index] ^= flip;
            if Module::from_code_cache(&damaged, source, None, None, &mut context).is_err() {
                refused += 1;
            }
        }
    }
    for len in 0..bytes.len() {
        assert!(
            Module::from_code_cache(&bytes[..len], source, None, None, &mut context).is_err(),
            "a truncation to {len} bytes is refused"
        );
    }
    assert!(
        refused > bytes.len(),
        "most damage is refused: {refused} of {}",
        bytes.len() * 3
    );
}

#[test]
fn modules_with_direct_eval_and_lazy_functions_round_trip() {
    let source = indoc! {r#"
        function outer(doEval) {
            if (doEval) eval('var x = "local"');
            return function inner() { return typeof x === 'undefined' ? 'none' : x; };
        }
        const deep = () => () => () => 'deep';
        export const result = [outer(true)(), outer(false)(), deep()()()].join(',');
        export const pending = Promise.resolve(0);
    "#};
    for lazy in [false, true] {
        let (mut context, _) = context(lazy);
        let module = parse(source, "e.mjs", &mut context);
        let bytes = module.code_cache(&mut context).unwrap().unwrap();
        let parsed = evaluate(&module, &mut context);
        let (mut context, _) = self::context(lazy);
        let module = Module::from_code_cache(&bytes, source, None, None, &mut context).unwrap();
        assert_eq!(evaluate(&module, &mut context), parsed);
        // Modules are strict: the `eval` declares `x` in a scope of its own.
        assert!(parsed.starts_with("none,none,deep"), "{parsed}");
    }
}

#[test]
fn cached_functions_are_read_back_when_first_called() {
    use crate::{builtins::function::OrdinaryFunction, vm::CodeBlock};
    let source = "export function f(a) { return a * 2; } export const g = () => f(21);";
    let (mut context, _) = context(false);
    let module = parse(source, "f.mjs", &mut context);
    let bytes = module.code_cache(&mut context).unwrap().unwrap();
    let (mut context, _) = self::context(false);
    let module = Module::from_code_cache(&bytes, source, None, None, &mut context).unwrap();
    let promise = module.load_link_evaluate(&mut context);
    context.run_jobs().unwrap();
    assert!(promise.state().as_fulfilled().is_some());
    let namespace = module.namespace(&mut context);
    let code = |name: &str, context: &mut Context| {
        let value = namespace.get(js_string!(name), context).unwrap();
        value
            .as_object()
            .unwrap()
            .downcast_ref::<OrdinaryFunction>()
            .unwrap()
            .code
            .clone()
    };
    let f = code("f", &mut context);
    assert!(f.is_lazy(), "read back, not called: still a stub");
    assert!(CodeBlock::resolved(&f).is_lazy());
    let g = namespace.get(js_string!("g"), &mut context).unwrap();
    let result = g
        .as_callable()
        .unwrap()
        .call(&JsValue::undefined(), &[], &mut context)
        .unwrap();
    assert_eq!(result, JsValue::new(42));
    // Called through `g`: `f`'s code block was made then, and its function object runs it next.
    assert!(!CodeBlock::resolved(&f).is_lazy());
    // Written down again, a module read back is the same module.
    let again = module.code_cache(&mut context).unwrap();
    assert!(again.is_none(), "a linked module is not written down");
}
