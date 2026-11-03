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
