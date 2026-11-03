use crate::{JsNativeErrorKind, TestAction, run_test_actions};
use indoc::indoc;

#[test]
fn ordinary_has_instance_nonobject_prototype() {
    run_test_actions([TestAction::assert_native_error(
        indoc! {r#"
            function C() {}
            C.prototype = 1
            String instanceof C
        "#},
        JsNativeErrorKind::Type,
        "function has non-object prototype in instanceof check",
    )]);
}

#[test]
fn object_properties_return_order() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
                var o = {
                    p1: 'v1',
                    p2: 'v2',
                    p3: 'v3',
                };
                o.p4 = 'v4';
                o[2] = 'iv2';
                o[0] = 'iv0';
                o[1] = 'iv1';
                delete o.p1;
                delete o.p3;
                o.p1 = 'v1';
            "#}),
        TestAction::assert(r#"arrayEquals(Object.keys(o), [ "0", "1", "2", "p2", "p4", "p1" ])"#),
        TestAction::assert(
            r#"arrayEquals(Object.values(o), [ "iv0", "iv1", "iv2", "v2", "v4", "v1" ])"#,
        ),
    ]);
}

#[test]
fn object_literal_defines_new_properties_with_default_attributes() {
    // `DefineOwnPropertyByName` inserts a missing key on an ordinary extensible object directly.
    // The attributes, the order, a repeated key and the accessors of a literal must all stay what
    // the full `[[DefineOwnProperty]]` produced.
    run_test_actions([
        TestAction::run(
            "const o = { a: 1, b: 'two' };
             const da = Object.getOwnPropertyDescriptor(o, 'a');
             const keys = Object.keys(o);",
        ),
        TestAction::assert_eq("da.value", 1),
        TestAction::assert(
            "da.writable === true && da.enumerable === true && da.configurable === true",
        ),
        TestAction::assert("keys.length === 2 && keys[0] === 'a' && keys[1] === 'b'"),
        // Freezing the literal afterwards still works, and the property is a data property.
        TestAction::run("Object.freeze(o);"),
        TestAction::assert("Object.isFrozen(o)"),
        TestAction::assert_eq("o.a", 1),
        // A repeated key runs the full path for the second definition.
        TestAction::run("const repeated = { x: 1, x: 2 };"),
        TestAction::assert_eq("repeated.x", 2),
        // Getters and setters stay accessors, and `__proto__` still sets the prototype.
        TestAction::run(
            "let seen = 0;
             const accessors = { get g() { return 7 }, set s(v) { seen = v } };
             accessors.s = 5;
             const dg = Object.getOwnPropertyDescriptor(accessors, 'g');
             const proto = { inherited: 1 };
             const withProto = { __proto__: proto, own: 2 };",
        ),
        TestAction::assert("dg.get !== undefined && dg.set === undefined"),
        TestAction::assert_eq("seen", 5),
        TestAction::assert_eq("withProto.inherited", 1),
        TestAction::assert("Object.getOwnPropertyNames(withProto).join() === 'own'"),
        // A literal built from an array-like receiver keeps its elements dense.
        TestAction::run("const arr = [1, 2, 3]; arr.extra = { k: 4 };"),
        TestAction::assert("Array.isArray(arr) && arr.length === 3 && arr.extra.k === 4"),
    ]);
}

#[test]
fn assigning_a_new_property_walks_the_prototype_chain() {
    // `this.x = x` inserts directly only when the whole prototype chain lacks the key and every
    // object on it is ordinary. A non-writable data property, a setter, a proxy, a frozen receiver
    // and a receiver other than the object written through all have to keep their behaviour.
    run_test_actions([
        // A non-writable data property on the prototype makes a sloppy-mode assignment a no-op and
        // a strict-mode one a TypeError.
        TestAction::run(
            "const lockedProto = {};
             Object.defineProperty(lockedProto, 'x', { value: 1, writable: false, configurable: true });
             const sloppy = Object.create(lockedProto);
             sloppy.x = 5;",
        ),
        TestAction::assert("sloppy.x === 1"),
        TestAction::assert("Object.getOwnPropertyNames(sloppy).length === 0"),
        TestAction::assert_native_error(
            "(function () { 'use strict'; const o = Object.create(lockedProto); o.x = 5; })()",
            JsNativeErrorKind::Type,
            "cannot set non-writable property: x",
        ),
        // A setter on the prototype runs, and nothing is created on the receiver.
        TestAction::run(
            "let setterSaw = 0;
             const withSetter = { set x(v) { setterSaw = v; } };
             const child = Object.create(withSetter);
             child.x = 9;",
        ),
        TestAction::assert_eq("setterSaw", 9),
        TestAction::assert("Object.getOwnPropertyNames(child).length === 0"),
        // A proxy anywhere on the chain keeps the assignment observable.
        TestAction::run(
            "let proxySaw = null;
             const proxied = new Proxy({}, { set(t, k, v) { proxySaw = k; return true; } });
             const throughProxy = Object.create(proxied);
             throughProxy.x = 3;",
        ),
        TestAction::assert("proxySaw === 'x'"),
        // An ordinary assignment still creates a plain writable property.
        TestAction::run(
            "const fresh = {};
             fresh.x = 1;
             const freshDesc = Object.getOwnPropertyDescriptor(fresh, 'x');",
        ),
        TestAction::assert_eq("freshDesc.value", 1),
        TestAction::assert(
            "freshDesc.writable === true && freshDesc.enumerable === true && freshDesc.configurable === true",
        ),
        // A non-extensible receiver keeps its silent failure and its TypeError.
        TestAction::run("const sealed = Object.preventExtensions({}); sealed.x = 1;"),
        TestAction::assert("sealed.x === undefined"),
        TestAction::assert_native_error(
            "(function () { 'use strict'; const o = Object.preventExtensions({}); o.x = 1; })()",
            JsNativeErrorKind::Type,
            "cannot set non-writable property: x",
        ),
        // A frozen data property on the receiver itself stays non-writable.
        TestAction::run("const frozen = Object.freeze({ x: 1 }); frozen.x = 2;"),
        TestAction::assert_eq("frozen.x", 1),
        // `Reflect.set` with a separate receiver defines on the receiver, not on the object the key
        // was looked up on.
        TestAction::run(
            "const source = {};
             const target = {};
             const ok = Reflect.set(source, 'x', 7, target);",
        ),
        TestAction::assert("ok === true"),
        TestAction::assert_eq("target.x", 7),
        TestAction::assert("source.x === undefined"),
        // An inherited `__proto__` accessor keeps intercepting an assignment of that name.
        TestAction::run(
            "const base = {};
             const derived = Object.create(base);
             derived.__proto__ = { marker: 1 };",
        ),
        TestAction::assert_eq("derived.marker", 1),
        TestAction::assert("Object.getPrototypeOf(derived) !== base"),
    ]);
}
