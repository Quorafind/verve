//! User code that runs inside an internal method and writes to the object the method is
//! working on. Each case panicked with `BorrowMutError` while the method kept a borrow of it.

use crate::{TestAction, run_test_actions};
use boa_macros::js_str;
use indoc::indoc;

#[test]
fn field_initializer_may_write_to_its_class() {
    // The initializer runs while `InitializeInstanceElements` reads the class's fields.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class X { static n = 0; id = ++X.n; }
            const a = new X(), b = new X();
            [a.id, b.id, X.n].join(' ')
        "#},
        js_str!("1 2 2"),
    )]);
}

#[test]
fn class_may_be_the_receiver_of_its_own_fields() {
    // A base constructor that returns the derived class makes the class the object whose fields
    // are initialized, so the fields are defined on the object that is being read.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class A { constructor() { return Y; } }
            class Y extends A { f = 1; #p = 2; static has(o) { return #p in o; } }
            const r = new Y();
            [r === Y, Y.f, Y.has(Y)].join(' ')
        "#},
        js_str!("true 1 true"),
    )]);
}

#[test]
fn private_methods_may_be_added_to_the_class_itself() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            class A { constructor() { return Y; } }
            class Y extends A { #m() { return 7; } static call(o) { return o.#m(); } }
            new Y();
            Y.call(Y)
        "#},
        7,
    )]);
}

#[test]
fn has_instance_may_write_to_a_bound_function() {
    // `OrdinaryHasInstance` of a bound function runs the target's `@@hasInstance`.
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            function F() {}
            const bf = F.bind();
            Object.defineProperty(F, Symbol.hasInstance, { value() { bf.x = 1; return true; } });
            [({}) instanceof bf, bf.x].join(' ')
        "#},
        js_str!("true 1"),
    )]);
}

#[test]
fn bound_native_function_may_be_written_to_by_its_callback() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const bf = Array.prototype.forEach.bind([1, 2], function () { bf.x = (bf.x || 0) + 1; });
            bf();
            const bp = Promise.bind(null);
            new bp(function () { bp.y = 5; });
            [bf.x, bp.y].join(' ')
        "#},
        js_str!("2 5"),
    )]);
}

#[test]
fn bound_proxy_may_be_written_to_by_its_traps() {
    run_test_actions([TestAction::assert_eq(
        indoc! {r#"
            const p = new Proxy(function () {}, {
              apply() { bf.x = 1; return 1; },
              construct() { bf.y = 2; return {}; },
            });
            const bf = p.bind();
            const r = bf();
            new bf();
            [r, bf.x, bf.y].join(' ')
        "#},
        js_str!("1 1 2"),
    )]);
}
