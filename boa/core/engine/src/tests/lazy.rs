//! Functions compiled when they are first called behave as functions compiled with the code
//! that declares them: each script here runs both ways and must log the same, and what it
//! should.

use crate::{
    Context, JsValue, Source, builtins::function::OrdinaryFunction, js_string, vm::CodeBlock,
};
use indoc::indoc;

/// Runs `source` in a fresh context, lazily or not, and answers the value of its last
/// statement, as a string, or the error it threw.
fn run(source: &str, lazy: bool) -> String {
    let mut context = Context::default();
    context.set_lazy_compile(lazy);
    let result = context.eval(Source::from_bytes(source));
    let result = result.and_then(|value| {
        context.run_jobs()?;
        Ok(value)
    });
    match result {
        Ok(value) => value.to_string(&mut context).map_or_else(
            |e| format!("toString threw {e}"),
            |s| s.to_std_string_escaped(),
        ),
        Err(error) => format!("threw {error}"),
    }
}

/// Runs `source` both ways and checks both answer `expected`.
#[track_caller]
fn same(source: &str, expected: &str) {
    let eager = run(source, false);
    let lazy = run(source, true);
    assert_eq!(
        lazy, eager,
        "lazily and eagerly compiled code differ:\n{source}"
    );
    assert_eq!(eager, expected, "{source}");
}

#[test]
fn function_is_compiled_on_first_call() {
    let mut context = Context::default();
    context.set_lazy_compile(true);
    context
        .eval(Source::from_bytes(
            "function f() { return 1 + 1 } var g = () => 2;",
        ))
        .unwrap();
    let f = context
        .global_object()
        .get(js_string!("f"), &mut context)
        .unwrap();
    let code = |value: &JsValue| {
        value
            .as_object()
            .unwrap()
            .downcast_ref::<OrdinaryFunction>()
            .unwrap()
            .code
            .clone()
    };
    let before = code(&f);
    assert!(before.is_lazy(), "declared, not called: still to compile");
    assert!(CodeBlock::resolved(&before).is_lazy());
    assert_eq!(before.bytecode.bytes.len(), 0, "a stub has no bytecode");

    context.eval(Source::from_bytes("f()")).unwrap();
    let after = code(&f);
    assert!(
        !after.is_lazy(),
        "called: the function runs its compiled code"
    );
    assert!(!after.bytecode.bytes.is_empty());
    assert!(boa_gc::Gc::ptr_eq(&CodeBlock::resolved(&before), &after));

    let mut eager = Context::default();
    eager.set_lazy_compile(false);
    eager
        .eval(Source::from_bytes("function f() { return 1 }"))
        .unwrap();
    let f = eager
        .global_object()
        .get(js_string!("f"), &mut eager)
        .unwrap();
    assert!(!code(&f).is_lazy());
}

#[test]
fn early_errors_in_uncalled_functions() {
    for source in [
        "function never(a, a) { 'use strict'; } 1",
        "function never() { 'use strict'; with ({}) {} } 1",
        "var f = () => { break; }; 1",
        "var o = { m() { super(); } }; 1",
        "function never() { return function* () { yield = 1; }; } 1",
    ] {
        let eager = run(source, false);
        assert!(eager.starts_with("threw SyntaxError"), "{source}: {eager}");
        assert_eq!(run(source, true), eager, "{source}");
    }
}

#[test]
fn to_string_name_length_and_prototype() {
    same(
        indoc! {r#"
            function f(a, b = 1, ...c) { return a; }
            const g = async function* gen(x) { yield x; };
            const o = { get x() { return 1; }, m(p, q) {}, async am() {} };
            const d = Object.getOwnPropertyDescriptor(o, 'x');
            [
              f.toString(), f.name, f.length, typeof f.prototype,
              g.toString(), g.name, g.length,
              Object.getPrototypeOf(g.prototype) === Object.getPrototypeOf(async function*(){}).prototype,
              d.get.name, o.m.length, 'prototype' in o.m, 'prototype' in o.am,
              (() => {}).hasOwnProperty('prototype'),
              f(5), f.toString()
            ].join('|')
        "#},
        "function f(a, b = 1, ...c) { return a; }|f|1|object|async function* gen(x) { yield x; }|gen|1|true|get x|2|false|false|false|5|function f(a, b = 1, ...c) { return a; }",
    );
}

#[test]
fn closures_and_scopes() {
    same(
        indoc! {r#"
            const log = [];
            function counter() {
              let n = 0;
              return { inc: () => ++n, get: function () { return n; } };
            }
            const c1 = counter(), c2 = counter();
            c1.inc(); c1.inc(); c2.inc();
            log.push(c1.get(), c2.get());
            function later() { return lateVar + lateLet; }
            var lateVar = 1;
            let lateLet = 2;
            log.push(later());
            function tdz() { return early; }
            try { tdz(); } catch (e) { log.push(e.name); }
            let early = 3;
            log.push(tdz());
            const fns = [];
            for (let i = 0; i < 3; i++) fns.push(() => i);
            log.push(fns[2](), fns[0](), fns[1]());
            function outer(a) { function inner(b) { return a + b + (typeof self) } return inner; }
            log.push(outer(1)(2), outer(10)(20));
            const named = function fact(k) { return k <= 1 ? 1 : k * fact(k - 1); };
            log.push(named(5));
            log.join(',')
        "#},
        "2,1,3,ReferenceError,3,2,0,1,3undefined,30undefined,120",
    );
}

#[test]
fn default_parameters_and_arguments() {
    same(
        indoc! {r#"
            const log = [];
            function p(a, b = () => a) { return [a, b()].join(':'); }
            log.push(p(1));
            function q(a, b = a * 2, c = (() => a + b)()) { return c; }
            log.push(q(3));
            function mapped(a) { arguments[0] = 9; return a; }
            function unmapped(a) { 'use strict'; arguments[0] = 9; return a; }
            function withDefault(a = 0) { arguments[0] = 9; return a; }
            log.push(mapped(1), unmapped(1), withDefault(1));
            function args() { return (() => arguments.length)(); }
            log.push(args(1, 2, 3));
            function rest(x, ...ys) { return ys.length + arguments.length; }
            log.push(rest(1, 2, 3));
            function destructure({ a, b: [c, d = 4] = [] } = {}) { return a + c + d; }
            log.push(destructure({ a: 1, b: [2] }));
            log.join(',')
        "#},
        "1:1,9,9,1,1,3,5,7",
    );
}

#[test]
fn classes() {
    same(
        indoc! {r#"
            const log = [];
            class A {
              #secret = 41;
              field = () => this.#secret + 1;
              static count = 0;
              static { A.count = [1, 2].map(x => x * 2).length; }
              constructor(v) { this.v = v; }
              get value() { return this.v; }
              method() { return this.field(); }
              static make() { return new this(7); }
              #priv() { return 'p'; }
              callPriv() { return this.#priv(); }
            }
            class B extends A {
              constructor() { super(5); this.w = super.method(); }
              method() { return super.method() * 2; }
            }
            const b = new B();
            log.push(b.v, b.w, b.method(), A.make().value, A.count, b.callPriv());
            try { A(); } catch (e) { log.push(e.name); }
            const obj = { __proto__: { hi() { return 'proto'; } }, hi() { return super.hi() + '!'; } };
            log.push(obj.hi());
            log.join(',')
        "#},
        "5,42,84,7,2,p,TypeError,proto!",
    );
}

#[test]
fn generators_and_async() {
    same(
        indoc! {r#"
            const log = [];
            function* gen(n) { for (let i = 0; i < n; i++) yield i * 2; return 'done'; }
            log.push([...gen(3)].join(''));
            const it = gen(1); it.next();
            log.push(it.next().value);
            async function af(x) { const y = await x; return y + 1; }
            async function* ag() { yield 1; yield await Promise.resolve(2); }
            (async () => {
              log.push(await af(Promise.resolve(1)));
              for await (const v of ag()) log.push(v);
              try { await (async () => { throw new Error('boom'); })(); } catch (e) { log.push(e.message); }
            })();
            globalThis.result = log;
            'ok'
        "#},
        "ok",
    );
    for lazy in [false, true] {
        let mut context = Context::default();
        context.set_lazy_compile(lazy);
        context
            .eval(Source::from_bytes(indoc! {r#"
                const log = [];
                async function af(x) { const y = await x; return y + 1; }
                async function* ag() { yield 1; yield await Promise.resolve(2); }
                (async () => {
                  log.push(await af(Promise.resolve(1)));
                  for await (const v of ag()) log.push(v);
                })();
                globalThis.result = log;
            "#}))
            .unwrap();
        context.run_jobs().unwrap();
        let result = context
            .eval(Source::from_bytes("result.join(',')"))
            .unwrap()
            .to_string(&mut context)
            .unwrap();
        assert_eq!(result.to_std_string_escaped(), "2,1,2", "lazy: {lazy}");
    }
}

#[test]
fn direct_eval() {
    // An `eval` that declares a `var` in a function: functions nested in it must not see that
    // binding in calls that did not run the `eval`.
    same(
        indoc! {r#"
            var x = 'global';
            function outer(doEval) {
              if (doEval) eval('var x = "local"');
              return function inner() { return x; };
            }
            const a = outer(true);
            const r1 = a();
            const b = outer(false);
            [r1, b(), outer(false)(), outer(true)()].join(',')
        "#},
        "local,global,global,local",
    );
    same(
        indoc! {r#"
            var y = 'global';
            function viaArrow(doEval) {
              const run = () => { if (doEval) eval('var y = "arrow"'); return () => y; };
              return run()();
            }
            [viaArrow(true), viaArrow(false), viaArrow(true)].join(',')
        "#},
        "arrow,global,arrow",
    );
    same(
        indoc! {r#"
            var z = 'global';
            if (true) eval('var z2 = 1');
            function top() { return z + z2; }
            eval('function made() { return z + 1; }');
            function f() { eval('function g() { return typeof q; } var q = 1;'); return g(); }
            [top(), made(), f(), (0, eval)('typeof z2')].join(',')
        "#},
        "global1,global1,number,number",
    );
    same(
        indoc! {r#"
            function evalInParams(a = eval('var p = 1'), b = () => typeof p) { return b(); }
            function noEval(b = () => typeof p) { return b(); }
            [evalInParams(), noEval()].join(',')
        "#},
        "number,undefined",
    );
}

#[test]
fn with_statement() {
    same(
        indoc! {r#"
            const o = { v: 'object' };
            var v = 'global';
            let f;
            with (o) { f = function () { return v; }; }
            const r1 = f();
            delete o.v;
            [r1, f()].join(',')
        "#},
        "object,global",
    );
}

#[cfg(feature = "annex-b")]
#[test]
fn annex_b_block_functions() {
    same(
        indoc! {r#"
            const log = [];
            function host() {
              log.push(typeof hoisted);
              if (true) { function hoisted() { return 'h'; } }
              return hoisted();
            }
            log.push(host());
            log.join(',')
        "#},
        "undefined,h",
    );
}

#[test]
fn stack_traces_and_positions() {
    same(
        indoc! {r#"
            function thrower() {
              const x = 1;
              throw new Error('here');
            }
            function caller() { return thrower(); }
            let stack;
            try { caller(); } catch (e) { stack = e.stack; }
            function Custom() { const err = {}; Error.captureStackTrace(err, Custom); return err.stack; }
            function viaCustom() { return Custom(); }
            [stack, viaCustom()].join('\n---\n')
        "#},
        "Error: here\n    at thrower (<anonymous>:3:9)\n    at caller (<anonymous>:5:35)\n    at <anonymous>:7:13\n---\nError\n    at viaCustom (<anonymous>:9:37)\n    at <anonymous>:10:18",
    );
}

#[test]
fn templates_regexps_and_function_constructor() {
    same(
        indoc! {r#"
            const sites = [];
            function tag(strings) { sites.push(strings); return strings.raw[0]; }
            function use() { return tag`a\n`; }
            use(); use();
            function re() { return /ab+c/g; }
            const r1 = re(), r2 = re();
            const F = new Function('a', 'b', 'return (() => a + b)()');
            [sites[0] === sites[1], r1 !== r2, r1.source, F(1, 2), F.toString().length > 0].join(',')
        "#},
        "true,true,ab+c,3,true",
    );
}

#[test]
fn closures_made_before_the_first_call() {
    same(
        indoc! {r#"
            function make(k) { return function (x) { return x * k; }; }
            const fs = [make(1), make(2), make(3)];
            const r = [fs[2](1), fs[0](1), fs[1](1)];
            const late = make(4);
            r.push(late(1), fs[2](2));
            r.join(',')
        "#},
        "3,1,2,4,6",
    );
}

#[test]
fn recursion_and_mutual_recursion() {
    same(
        indoc! {r#"
            function even(n) { return n === 0 ? true : odd(n - 1); }
            function odd(n) { return n === 0 ? false : even(n - 1); }
            function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }
            [even(10), odd(7), fib(15)].join(',')
        "#},
        "true,true,610",
    );
}

#[test]
fn strict_mode_and_this() {
    same(
        indoc! {r#"
            function sloppy() { return typeof this; }
            function strict() { 'use strict'; return typeof this; }
            const arrow = () => typeof this;
            const o = { m() { return [typeof this, (() => this === o)()]; } };
            function ctor() { if (!new.target) return 'no new'; this.made = true; }
            [sloppy(), strict(), arrow(), o.m().join('/'), ctor(), new ctor().made].join(',')
        "#},
        "object,undefined,object,object/true,no new,true",
    );
}

#[test]
fn first_call_from_another_context() {
    let source = "(function x(a) { return a + 1; })";
    // Lazily: the function's syntax tree belongs to its context, so a first call from another
    // one throws instead of compiling it there.
    let mut owner = Context::default();
    owner.set_lazy_compile(true);
    let function = owner.eval(Source::from_bytes(source)).unwrap();
    let mut other = Context::default();
    let callable = function.as_callable().unwrap();
    let error = callable
        .call(&JsValue::undefined(), &[JsValue::new(1)], &mut other)
        .unwrap_err();
    assert!(error.to_string().contains("another context"), "{error}");
    // Called once at home, it is compiled, and then other contexts can call it too.
    let result = callable
        .call(&JsValue::undefined(), &[JsValue::new(1)], &mut owner)
        .unwrap();
    assert_eq!(result, JsValue::new(2));
    let result = callable
        .call(&JsValue::undefined(), &[JsValue::new(2)], &mut other)
        .unwrap();
    assert_eq!(result, JsValue::new(3));
}

#[cfg(feature = "jit")]
#[test]
fn jit_compiles_lazily_compiled_functions() {
    let source = indoc! {r#"
        function sum(n) { let s = 0; for (let i = 0; i < n; i++) s = (s + i * 3) | 0; return s; }
        function outer(n) { const inner = (k) => sum(k) + 1; return inner(n); }
        outer(1000) + sum(10)
    "#};
    let mut results = Vec::new();
    for lazy in [false, true] {
        let mut context = Context::default();
        context.set_lazy_compile(lazy);
        context.jit_options_mut().threshold = 0;
        let value = context.eval(Source::from_bytes(source)).unwrap();
        results.push(
            value
                .to_string(&mut context)
                .unwrap()
                .to_std_string_escaped(),
        );
        if crate::vm::jit_unavailable().is_none() {
            assert!(context.jit_stats().compiled > 0, "lazy: {lazy}");
        }
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0], "1498636");
}

/// Runs `first` and `scripts` in one context, lazily or not, each as a script of its own, and
/// answers what each script answered or threw.
fn scripts(lazy: bool, scripts: &[&str]) -> Vec<String> {
    let mut context = Context::default();
    context.set_lazy_compile(lazy);
    scripts
        .iter()
        .map(|source| match context.eval(Source::from_bytes(*source)) {
            Ok(value) => value.to_string(&mut context).map_or_else(
                |e| format!("toString threw {e}"),
                |s| s.to_std_string_escaped(),
            ),
            Err(error) => format!("threw {error}"),
        })
        .collect()
}

#[test]
fn a_parsed_script_declares_nothing_until_it_runs() {
    for lazy in [false, true] {
        let mut context = Context::default();
        context.set_lazy_compile(lazy);
        context
            .eval(Source::from_bytes(
                "globalThis.setting = 'global'; function readSetting() { return setting; }",
            ))
            .unwrap();
        // Parsed, never run: its `let` is not a global binding of the realm.
        let parsed = crate::Script::parse(
            Source::from_bytes("let setting = 'lexical';"),
            None,
            &mut context,
        )
        .unwrap();
        let read = context.eval(Source::from_bytes("readSetting()")).unwrap();
        assert_eq!(read, js_string!("global").into(), "lazy: {lazy}");
        // Run, it is.
        parsed.evaluate(&mut context).unwrap();
        let read = context.eval(Source::from_bytes("setting")).unwrap();
        assert_eq!(read, js_string!("lexical").into(), "lazy: {lazy}");
    }
}

#[test]
fn a_script_that_fails_to_declare_declares_nothing() {
    for lazy in [false, true] {
        let results = scripts(
            lazy,
            &[
                "function usesUndefined(v) { return v === undefined; } 'ok'",
                "let undefined = 0;",
                "usesUndefined(1)",
                "typeof undefined",
            ],
        );
        assert!(results[1].starts_with("threw SyntaxError"), "{results:?}");
        assert_eq!(results[2..], ["false", "undefined"], "lazy: {lazy}");
    }
}

#[test]
fn global_lexicals_declared_twice() {
    for lazy in [false, true] {
        let mut context = Context::default();
        context.set_lazy_compile(lazy);
        let first =
            crate::Script::parse(Source::from_bytes("let dup = 1; dup"), None, &mut context)
                .unwrap();
        let second =
            crate::Script::parse(Source::from_bytes("let dup = 2; dup"), None, &mut context)
                .unwrap();
        assert_eq!(first.evaluate(&mut context).unwrap(), JsValue::new(1));
        let error = second.evaluate(&mut context).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("'dup' has already been declared"),
            "{error}"
        );
        assert_eq!(
            context.eval(Source::from_bytes("dup")).unwrap(),
            JsValue::new(1)
        );
        // A script that cannot even parse declares nothing either.
        assert!(
            context
                .eval(Source::from_bytes("let z = 1; let dup = 3;"))
                .is_err()
        );
        assert!(
            context
                .eval(Source::from_bytes("typeof z"))
                .unwrap()
                .as_string()
                .is_some_and(|s| s == js_string!("undefined"))
        );
    }
}

/// A function compiled when first called finds a global `let` a script declared after the
/// function was: as the specification has it. One compiled with its script looks the name up
/// on the global object, as eager Boa always has.
#[test]
fn a_later_global_let_is_found_by_a_lazily_compiled_function() {
    let source = [
        "function readX() { return typeof x === 'undefined' ? 'undef' : x; } 'ok'",
        "let x = 5;",
        "readX()",
    ];
    assert_eq!(scripts(true, &source)[2], "5");
    assert_eq!(scripts(false, &source)[2], "undef");
}
