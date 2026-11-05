//! Tests of what a call frame reaches through its callee: the code block, script or module,
//! realm and environments the frame lends from the function. They run in the interpreter, with
//! or without the `jit` feature; `vm/jit/tests_call_frame.rs` runs the same kinds of script in
//! every mode of the JIT.

use boa_gc::force_collect;

use crate::{Context, JsValue, NativeFunction, Source, js_string};

/// A context with `collect()`, which runs the garbage collector, as a global.
pub(crate) fn context_with_collect() -> Context {
    let mut context = Context::default();
    let collect = NativeFunction::from_fn_ptr(|_, _, _| {
        force_collect();
        Ok(JsValue::undefined())
    });
    context
        .register_global_callable(js_string!("collect"), 0, collect)
        .expect("registers");
    context
}

fn run(source: &str) -> String {
    let mut context = context_with_collect();
    match context.eval(Source::from_bytes(source)) {
        Ok(value) => match value.as_string() {
            Some(string) => string.to_std_string_escaped(),
            None => value.display().to_string(),
        },
        Err(error) => format!("uncaught {error}"),
    }
}

/// The target of the proxy is anonymous (a named function expression keeps itself alive through
/// its own binding) and made in a helper, so once the proxy is revoked nothing but the call in
/// progress refers to it.
const REVOCABLE: &str = "
    let revoke;
    function make() {
        let x = 41;
        return function () { revoke(); collect(); collect(); return x + 1; };
    }
    function setup(target) { const r = Proxy.revocable(target, {}); revoke = r.revoke; return r.proxy; }
";

/// A proxy without a trap forwards the call to its target and leaves the proxy in the frame's
/// function slot; revoking it in the middle of the call must not free what the frame reads.
#[test]
fn a_revoked_proxy_does_not_free_the_running_function() {
    let call = format!("{REVOCABLE} setup(make())()");
    assert_eq!(run(&call), "42");
    for _ in 0..3 {
        assert_eq!(run(&call), "42");
    }
}

/// The same through `new`, `super()`, a native caller and a function made by `Function`.
#[test]
fn a_revoked_proxy_does_not_free_the_running_constructor() {
    let construct = format!(
        "{REVOCABLE}
         function makeCtor() {{
             let x = 41;
             return function () {{ revoke(); collect(); collect(); this.v = x + 1; }};
         }}
         new (setup(makeCtor()))().v"
    );
    assert_eq!(run(&construct), "42");

    let derived = format!(
        "{REVOCABLE}
         function makeCtor() {{
             let x = 41;
             return function () {{ revoke(); collect(); collect(); this.v = x + 1; }};
         }}
         class B extends (setup(makeCtor())) {{}}
         new B().v"
    );
    assert_eq!(run(&derived), "42");

    let callback = format!("{REVOCABLE} [0].map(setup(make())).join()");
    assert_eq!(run(&callback), "42");

    let made = "
        let revoke;
        function setup(target) { const r = Proxy.revocable(target, {}); revoke = r.revoke; return r.proxy; }
        function build() { return Function('revoke(); collect(); collect(); return 42'); }
        setup(build())()";
    assert_eq!(run(made), "42");

    let made_ctor = "
        let revoke;
        function setup(target) { const r = Proxy.revocable(target, {}); revoke = r.revoke; return r.proxy; }
        function build() { return Function('revoke(); collect(); collect(); this.v = 42'); }
        new (setup(build()))().v";
    assert_eq!(run(made_ctor), "42");
}

/// A bound function leaves itself in the function slot as well.
#[test]
fn a_bound_function_runs_its_target() {
    let source = "
        function make() { let x = 41; return function () { collect(); return x + this.d; }; }
        make().bind({ d: 1 })()";
    assert_eq!(run(source), "42");
}

/// Generators and async functions move their frame off the stack and back: `this`, closed-over
/// variables, `arguments` and `new.target` survive the suspensions, and a collection in between
/// leaves the function's code alive.
#[test]
fn moved_frames_keep_their_function() {
    let source = "
        function mk(n) { return function* (a) { const t = this; let s = n; for (let i = 0; i < 4; i++) { s += (yield [t === o, arguments.length, s]) || 0; collect(); } return s; }; }
        const o = {};
        const g = mk(100).call(o, 1, 2);
        const log = [];
        for (let i = 0; i < 6; i++) log.push(JSON.stringify(g.next(i)));
        log.join('|')";
    assert_eq!(
        run(source),
        "{\"value\":[true,2,100],\"done\":false}|{\"value\":[true,2,101],\"done\":false}|\
         {\"value\":[true,2,103],\"done\":false}|{\"value\":[true,2,106],\"done\":false}|\
         {\"value\":110,\"done\":true}|{\"done\":true}"
    );
}

/// A function of another realm runs in its own realm, whoever calls it.
#[test]
fn functions_of_another_realm_run_in_their_own() {
    let mut context = context_with_collect();
    let other = context.create_realm().expect("a second realm");
    let this_realm = context.enter_realm(other);
    context
        .eval(Source::from_bytes(
            "globalThis.marker = 'B';
             globalThis.read = function (x) { return marker + x; };
             globalThis.getterObject = { get g() { return marker; } };",
        ))
        .expect("sets up");
    let global = context.global_object();
    let read = global.get(js_string!("read"), &mut context).expect("read");
    let getter = global
        .get(js_string!("getterObject"), &mut context)
        .expect("getter");
    context.enter_realm(this_realm);
    for (name, value) in [("read", read), ("getterObject", getter)] {
        context
            .global_object()
            .set(js_string!(name), value, true, &mut context)
            .expect("imports");
    }
    let value = context
        .eval(Source::from_bytes(
            "globalThis.marker = 'A';
             [read(1), [2].map(read).join(), getterObject.g, [3].map((x) => marker + x).join()].join()",
        ))
        .expect("runs");
    assert_eq!(
        value.as_string().expect("a string").to_std_string_escaped(),
        "B1,B2,B,A3"
    );
}

/// `error.stack` through a callback of a built-in and a proxy.
#[test]
fn stacks_through_calls_are_the_same() {
    let source = "
        function c() { return new Error('boom').stack; }
        function b() { return [0].map(function inner() { return c(); })[0]; }
        const p = new Proxy(b, {});
        p()";
    let stack = run(source);
    let lines: Vec<&str> = stack.lines().collect();
    assert!(lines[0].ends_with("boom"), "{stack}");
    assert!(lines[1].contains("at c ("), "{stack}");
    assert!(lines[2].contains("at inner ("), "{stack}");
    assert!(lines[3].contains("at map (native)"), "{stack}");
    assert!(lines[4].contains("at b ("), "{stack}");
}
