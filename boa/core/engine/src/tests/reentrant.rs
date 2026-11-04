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
