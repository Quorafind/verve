//! Tests of what a call frame reaches through its callee (the code block, the realm and the
//! script or module are lent by the function, not counted by the frame): calls of every kind,
//! calls across realms, frames of generators and async functions moved off the stack, the garbage
//! collector running in the middle of calls, and `error.stack`.

use boa_gc::force_collect;

use crate::{JsValue, NativeFunction, Source, js_string};

use super::tests::{Mode, SHOW, context, same, same_after_jobs};

/// Every kind of call, deep and mutual recursion, and the depth at which the limit is hit.
#[test]
fn calls_of_every_kind_match_the_interpreter() {
    same(
        "function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }
         function even(n) { return n === 0 ? true : odd(n - 1); }
         function odd(n) { return n === 0 ? false : even(n - 1); }
         function tail(n, acc) { return n === 0 ? acc : tail(n - 1, acc + n); }
         function depth() { let d = 0; function r() { d++; r(); } try { r(); } catch (e) { return e.constructor.name + ':' + d; } }
         class P { constructor(x) { this.x = x; } get() { return this.x; } get g() { return () => this.x; } }
         class Q extends P { get() { return super.get() + 1; } }
         const o = { m() { return this === o; }, get h() { return function () { return this === o; }; } };
         function who(a, b) { return typeof this + a + b; }
         const bnd = who.bind('x', 1);
         const prx = new Proxy(who, { apply(t, th, args) { return 'P' + Reflect.apply(t, th, args); }, construct(t, args) { return { c: args.length }; } });
         const out = [];
         for (let i = 0; i < 40; i++) {
           out.push(fact(10), even(i), odd(i), tail(100, 0), new P(i).get(), new Q(i).get(), new P(i).g(), o.m(), o.h());
           out.push(bnd(i), prx(i, 2), new prx(1, 2).c, Reflect.apply(who, null, [1, 2]), who.call(7, 1, 2), who.apply(undefined, [3, 4]));
         }
         out.push(depth(), depth());
         out.map(show).join(' ')",
    );
}

/// Frames of generators and async functions are moved off the stack and back: `this`, closed-over
/// variables, `arguments` and `new.target` survive any number of suspensions.
#[test]
fn moved_frames_keep_their_function() {
    same_after_jobs(
        "function mk(n) { return function* (a) { const t = this; let s = n; for (let i = 0; i < 4; i++) { s += (yield [t === o, arguments.length, s, new.target === undefined]) || 0; } return s; }; }
         const o = {}, g = mk(100).call(o, 1, 2);
         const log = [];
         for (let i = 0; i < 6; i++) log.push(g.next(i));
         async function af(n) { let t = 0; for (let i = 0; i < 3; i++) { t += await Promise.resolve(n + i); } return [t, arguments.length, this === o]; }
         for (let i = 0; i < 20; i++) af.call(o, i, 0).then((v) => log.push(v));
         class A { async m() { await null; return new.target === undefined; } *it() { yield this === a; } }
         const a = new A(); a.m().then((v) => log.push(v)); log.push([...a.it()]);
         globalThis.result = log;
         0",
        "result.map(show).join(' ')",
    );
}

/// Runs `source` in `mode` in a context with `collect()`, which runs the garbage collector.
fn run_with_collect(source: &str, mode: Mode) -> String {
    let mut context = context(mode);
    let collect = NativeFunction::from_fn_ptr(|_, _, _| {
        force_collect();
        Ok(JsValue::undefined())
    });
    context
        .register_global_callable(js_string!("collect"), 0, collect)
        .expect("registers");
    let value = context
        .eval(Source::from_bytes(&format!(
            "{SHOW}
{source}"
        )))
        .expect("runs");
    match value.as_string() {
        Some(string) => string.to_std_string_escaped(),
        None => value.display().to_string(),
    }
}

/// The collector runs in the middle of calls, between a function's call and its return.
#[test]
fn collections_in_the_middle_of_calls() {
    let source = "
        function garbage(n) { let junk = []; for (let i = 0; i < n; i++) junk.push({ i, s: 'x' + i }); return junk.length; }
        function make(k) { return function inner(a) { collect(); const r = garbage(2000) + a + k; collect(); return r; }; }
        const out = [];
        for (let i = 0; i < 20; i++) {
          out.push(make(i)(1));
          out.push([1, 2].map(make(i)).join());
          out.push((function* () { collect(); yield make(i)(3); })().next().value);
        }
        out.map(show).join(' ')";
    let interpreted = run_with_collect(source, Mode::Interpreted);
    for mode in [Mode::Eager, Mode::Hot] {
        assert_eq!(interpreted, run_with_collect(source, mode), "{mode:?}");
    }
}

/// A proxy without a trap leaves itself in the frame's function slot while its target runs;
/// revoking the proxy in the middle of the call leaves the running function referred to by
/// nothing but the call, and its frame must not read what the collector frees. The target is
/// anonymous (a named function expression keeps itself alive through its own binding) and made
/// in a helper.
#[test]
fn a_revoked_proxy_does_not_free_the_running_function() {
    const SETUP: &str = "
        let revoke;
        function setup(target) { const r = Proxy.revocable(target, {}); revoke = r.revoke; return r.proxy; }
        function make() { let x = 41; return function () { revoke(); collect(); collect(); return x + 1; }; }
        function makeCtor() { let x = 41; return function () { revoke(); collect(); collect(); this.v = x + 1; }; }
        function build() { return Function('revoke(); collect(); collect(); return 42'); }
        function buildCtor() { return Function('revoke(); collect(); collect(); this.v = 42'); }
    ";
    for call in [
        "setup(make())()",
        "new (setup(makeCtor()))().v",
        "class B extends (setup(makeCtor())) {} new B().v",
        "[0].map(setup(make())).join()",
        "setup(build())()",
        "new (setup(buildCtor()))().v",
    ] {
        let source = format!("{SETUP} {call}");
        for mode in [Mode::Interpreted, Mode::Eager, Mode::Hot] {
            for _ in 0..3 {
                assert_eq!(run_with_collect(&source, mode), "42", "{mode:?}: {call}");
            }
        }
    }
}

/// A function of another realm runs in its own realm, whoever calls it: directly, as a callback
/// of a built-in, as a getter, and when it calls back.
#[test]
fn functions_of_another_realm_run_in_their_own() {
    const OTHER: &str = "
        globalThis.marker = 'B';
        globalThis.read = function (x) { return marker + ':' + (this === undefined ? 'u' : typeof this) + ':' + (Array.isArray([]) ? 'a' : '?') + x; };
        globalThis.arrayOfB = Array;
        globalThis.viaCallback = function (f) { return [1, 2].map(f).join(); };
        globalThis.getterObject = { get g() { return marker; } };
        globalThis.makeGen = function* () { yield marker; yield typeof Array; };
        globalThis.fails = function () { return undefinedVariable; };";
    let mut results = Vec::new();
    for mode in [Mode::Interpreted, Mode::Eager, Mode::Hot] {
        let mut context = context(mode);
        let other = context.create_realm().expect("a second realm");
        let this_realm = context.enter_realm(other);
        context.eval(Source::from_bytes(OTHER)).expect("sets up");
        let global = context.global_object();
        let mut exported = Vec::new();
        for name in [
            "read",
            "arrayOfB",
            "viaCallback",
            "getterObject",
            "makeGen",
            "fails",
        ] {
            exported.push((
                name,
                global
                    .get(js_string!(name), &mut context)
                    .expect("exported"),
            ));
        }
        context.enter_realm(this_realm);
        for (name, value) in exported {
            context
                .global_object()
                .set(js_string!(name), value, true, &mut context)
                .expect("imports");
        }
        let value = context
            .eval(Source::from_bytes(&format!(
                "{SHOW}
                 globalThis.marker = 'A';
                 const out = [];
                 const local = (x) => marker + x;
                 for (let i = 0; i < 30; i++) {{
                   out.push(read(i), read.call(5, i), [i].map(read).join(), viaCallback(read), viaCallback(local), getterObject.g);
                   out.push(arrayOfB === Array, [...makeGen()].join(), Reflect.apply(read, undefined, [i]));
                   try {{ fails(); }} catch (e) {{ out.push(e.name, e.constructor === ReferenceError); }}
                 }}
                 out.map(show).join(' ')"
            )))
            .expect("runs");
        results.push(value.display().to_string());
    }
    assert_eq!(results[0], results[1]);
    assert_eq!(results[0], results[2]);
    assert!(results[0].contains("B:object:a0"), "{}", results[0]);
}

/// `error.stack` and `Error.captureStackTrace` through calls of every kind.
#[test]
fn stacks_through_every_kind_of_call_match_the_interpreter() {
    let out = same(
        "function c(n) { return new Error('boom ' + n).stack; }
         function b(n) { return [n].map(function inner(x) { return c(x); })[0]; }
         function a(n) { return b(n + 1); }
         function d() { const o = {}; Error.captureStackTrace(o); return o.stack; }
         function e() { const o = {}; Error.captureStackTrace(o, e); return o.stack; }
         class K { constructor() { this.s = new Error('k').stack; } m() { return d(); } }
         function* gen() { yield new Error('gen').stack; yield d(); }
         const out = [];
         for (let i = 0; i < 20; i++) {
           out.push(a(i), d(), e(), new K().s, new K().m(), gen().next().value, [...gen()][1]);
           try { null.x; } catch (x) { out.push(x.stack); }
           out.push((function () { return new Error('bound').stack; }).bind(null)(), Reflect.apply(c, null, [1]));
         }
         out.join('|')",
    );
    assert!(out.contains("at inner"), "{out}");
    assert!(out.contains("at map (native)"), "{out}");
}
