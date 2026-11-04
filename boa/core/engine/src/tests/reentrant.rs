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

/// What compiled code reads an object through must answer "not here" for an object that is
/// borrowed, so that the slow path decides, instead of panicking.
mod declines_a_borrowed_object {
    use boa_parser::Source;

    use crate::{
        Context, JsObject, JsValue,
        builtins::typed_array::object::{
            typed_array_element_get_shortcut, typed_array_element_set_shortcut,
            typed_array_length_guard, typed_array_length_read,
        },
    };

    fn object(context: &mut Context, source: &str) -> JsObject {
        context
            .eval(Source::from_bytes(source))
            .expect("runs")
            .as_object()
            .expect("an object")
    }

    #[test]
    fn typed_array_shortcuts() {
        let context = &mut Context::default();
        let array = object(context, "new Uint8Array([7, 8])");
        {
            let _writing = array.borrow_mut();
            assert!(typed_array_element_get_shortcut(&array, 0).is_none());
            assert!(
                typed_array_element_set_shortcut(&array, 0, &JsValue::from(1), context).is_none()
            );
            assert!(typed_array_length_read(&array).is_none());
            assert!(!typed_array_length_guard(&array));
        }
        assert_eq!(
            typed_array_element_get_shortcut(&array, 1),
            Some(JsValue::from(8))
        );
        assert!(typed_array_length_guard(&array));
        assert_eq!(typed_array_length_read(&array), Some(JsValue::from(2)));
    }

    #[test]
    fn typed_array_length_guard_with_a_borrowed_prototype() {
        let context = &mut Context::default();
        let array = object(context, "new Uint8Array(2)");
        let prototype = array.prototype().expect("has a prototype");
        let _writing = prototype.borrow_mut();
        assert!(!typed_array_length_guard(&array));
    }

    #[cfg(feature = "jit")]
    #[test]
    fn array_iterator_step() {
        use crate::builtins::array::array_iterator::ArrayIterator;

        let context = &mut Context::default();
        let iterator = object(context, "[1, 2][Symbol.iterator]()");
        {
            let _writing = iterator.borrow_mut();
            assert!(ArrayIterator::advance_dense(&iterator).is_err());
        }
        assert_eq!(
            ArrayIterator::advance_dense(&iterator).ok().flatten(),
            Some(JsValue::from(1))
        );
    }
}
