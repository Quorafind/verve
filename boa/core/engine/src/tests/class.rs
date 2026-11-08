use crate::{TestAction, run_test_actions};
use boa_macros::js_str;
use indoc::indoc;

#[test]
fn class_field_initializer_name_static() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class C {
                static a = function() {};
                static ["b"] = function() {};
                static #c = function() {};
                static c = this.#c
            }
        "#}),
        TestAction::assert_eq("C.a.name", js_str!("a")),
        TestAction::assert_eq("C.b.name", js_str!("b")),
        TestAction::assert_eq("C.c.name", js_str!("#c")),
    ]);
}

#[test]
fn class_field_initializer_name() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class C {
                a = function() {};
                ["b"] = function() {};
                #c = function() {};
                c = this.#c
            }
            let c = new C();
        "#}),
        TestAction::assert_eq("c.a.name", js_str!("a")),
        TestAction::assert_eq("c.b.name", js_str!("b")),
        TestAction::assert_eq("c.c.name", js_str!("#c")),
    ]);
}

#[test]
fn class_superclass_from_regex_error() {
    run_test_actions([TestAction::assert_native_error(
        "class A extends /=/ {}",
        crate::JsNativeErrorKind::Type,
        "superclass must be a constructor",
    )]);
}

// https://github.com/boa-dev/boa/issues/3055
#[test]
fn class_can_access_super_from_static_initializer() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class a {
                static field = "super field";
            }

            class b extends a {
                static #field = super.field;
                static get field() {
                    return this.#field;
                }
            }

            class c extends a {
                static field = super.field;
            }

        "#}),
        TestAction::assert_eq("a.field", js_str!("super field")),
        TestAction::assert_eq("b.field", js_str!("super field")),
        TestAction::assert_eq("c.field", js_str!("super field")),
    ]);
}

// https://github.com/boa-dev/boa/issues/4400
#[test]
fn class_in_constructor() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class C {
                constructor() {
                    class D {}
                    this.v = D.name.toString()
                }
            }
            let c = new C()

        "#}),
        TestAction::assert_eq("c.v", js_str!("D")),
    ]);
}

// https://github.com/boa-dev/boa/issues/4555
#[test]
fn nested_class_in_class_expression_constructor() {
    run_test_actions([TestAction::run(
        "new (class { constructor() { class D {} } })();",
    )]);
}

// https://github.com/boa-dev/boa/issues/4555
#[test]
fn nested_class_in_static_block() {
    run_test_actions([TestAction::run("(class { static { class D {} } });")]);
}

#[test]
fn property_initializer_reference_escaped_variable() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            function run() {
                const x = "D";
                class C {
                    a = x;
                    static b = x;
                    #c = x;
                    static #d = x;

                    getC() { return this.#c }
                    static getD() { return C.#d }
                }
                return C
            }
            var Z = run();
            var z = new Z();
        "#}),
        TestAction::assert_eq("z.a", js_str!("D")),
        TestAction::assert_eq("Z.b", js_str!("D")),
        TestAction::assert_eq("z.getC()", js_str!("D")),
        TestAction::assert_eq("Z.getD()", js_str!("D")),
    ]);
}

#[test]
fn private_field_initializer_reference_non_escaped_variable() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            function outer() {
                let x = 1;
                class C {
                    #p = x;
                    m() { return this.#p; }
                }
                return new C().m();
            }
        "#}),
        TestAction::assert_eq("outer()", 1),
    ]);
}

// https://github.com/boa-dev/boa/issues/4605
#[test]
fn class_boolean_literal_method_names() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class A {
                true() { return 1; }
                false() { return 2; }
                null() { return 3; }
            }
            var a = new A();
        "#}),
        TestAction::assert_eq("a.true()", 1),
        TestAction::assert_eq("a.false()", 2),
        TestAction::assert_eq("a.null()", 3),
    ]);
}

// https://github.com/boa-dev/boa/issues/4605
#[test]
fn class_boolean_literal_static_method_names() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class B {
                static true() { return 10; }
                static false() { return 20; }
            }
        "#}),
        TestAction::assert_eq("B.true()", 10),
        TestAction::assert_eq("B.false()", 20),
    ]);
}

// https://github.com/boa-dev/boa/issues/4605
#[test]
fn class_boolean_literal_getter_setter_names() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class C {
                get true() { return this._true; }
                set true(v) { this._true = v; }
                get false() { return this._false; }
                set false(v) { this._false = v; }
            }
            var c = new C();
            c.true = 42;
            c.false = 84;
        "#}),
        TestAction::assert_eq("c.true", 42),
        TestAction::assert_eq("c.false", 84),
    ]);
}

#[test]
fn super_call_as_the_base_of_a_chain() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class B {
                constructor(v) { this.v = v; this.k = "key"; }
                tag(strings) { return strings[0] + this.v; }
                get self() { return () => this; }
            }
            class C extends B {
                constructor(v) { return super(v).self(); }
            }
            class D extends B {
                constructor(v) { super(v)[this.k] = v * 2; }
            }
            class E extends B {
                constructor(v) { E.seen = super(v).tag`t:`; E.opt = this?.v; }
            }
            class F extends B {
                constructor(v) { F.then = super(v)?.v; }
            }
            const c = new C(1);
            const d = new D(2);
            new E(3);
            new F(4);
        "#}),
        TestAction::assert_eq("c.v", 1),
        TestAction::assert("c instanceof C"),
        TestAction::assert_eq("d.key", 4),
        TestAction::assert_eq("E.seen", js_str!("t:3")),
        TestAction::assert_eq("F.then", 4),
    ]);
}

#[test]
fn super_store_of_an_index_defines_on_the_receiver_not_the_dense_array() {
    // `super[k] = v` has the prototype as `object` and `this` as receiver, so `OrdinarySet` defines
    // the element on `this`; a dense array that is the prototype must be left alone, for an index
    // inside the array and for one that would append, in sloppy and strict code alike.
    for directive in ["", "'use strict';"] {
        run_test_actions([
            TestAction::run(format!(
                "{directive}
                 const proto = [0];
                 const o = {{ __proto__: proto, m(i) {{ super[i] = 5; }} }};
                 o.m(0); o.m(1);"
            )),
            TestAction::assert("Object.hasOwn(o, '0') && Object.hasOwn(o, '1')"),
            TestAction::assert("proto[0] === 0 && proto.length === 1"),
            TestAction::run(format!(
                "{directive}
                 class B extends Array {{ f(i) {{ super[i] = 1; }} }}
                 const b = new B();
                 b.f(0); b.f(3);"
            )),
            TestAction::assert("b[0] === 1 && b[3] === 1 && b.length === 4"),
            TestAction::assert(
                "!Object.hasOwn(Array.prototype, '0') && Array.prototype.length === 0",
            ),
        ]);
    }
}

#[test]
fn super_store_of_a_name_defines_on_the_receiver_every_time() {
    // The inline cache of `super.name = value` is keyed on the prototype's shape; a hit must not
    // store into the prototype's slot instead of the receiver's.
    run_test_actions([
        TestAction::run(
            "const proto = { x: 1 };
             const make = () => ({ __proto__: proto, m(v) { super.x = v; } });
             const seen = [];
             for (let i = 0; i < 4; i++) { const o = make(); o.m(10 + i); seen.push(Object.hasOwn(o, 'x') && o.x); }",
        ),
        TestAction::assert("proto.x === 1"),
        TestAction::assert("seen.join() === '10,11,12,13'"),
    ]);
}

/// A base class constructor gets a function environment only when something needs it (an arrow
/// function, `eval`, `super.x` or `new.target` in the body, a binding a closure captures); the
/// answers must be the same either way.
#[test]
fn base_constructor_function_environment_is_built_only_when_needed() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            class Plain { constructor(x) { this.x = x; } }
            class Empty {}
            class Arrow { constructor() { this.f = () => this; this.g = () => new.target; } }
            class Eval { constructor() { this.ok = eval("this") === this; } }
            class Super { constructor() { this.ok = super.toString === Object.prototype.toString; } }
            class Captured { constructor(a) { this.get = () => a; } }
            class Returns { constructor() { return { z: 1 }; } }
            class Prim { constructor() { return 5; } }
            class Derived extends Plain { constructor() { super(4); this.nt = new.target === Derived; } }
            class DefaultDerived extends Plain {}
            const plain = new Plain(3);
            const arrow = new Arrow();
            const proto = Reflect.construct(Plain, [1], Derived);
            function F() {}
            F.prototype = 3;
        "#}),
        TestAction::assert_eq("plain.x", 3),
        TestAction::assert("Object.getPrototypeOf(plain) === Plain.prototype"),
        TestAction::assert("new Empty() instanceof Empty"),
        TestAction::assert("arrow.f() === arrow && arrow.g() === Arrow"),
        TestAction::assert("new Eval().ok"),
        TestAction::assert("new Super().ok"),
        TestAction::assert_eq("new Captured(7).get()", 7),
        TestAction::assert_eq("new Returns().z", 1),
        TestAction::assert("new Prim() instanceof Prim"),
        TestAction::assert("new Derived().nt"),
        TestAction::assert_eq("new DefaultDerived(8).x", 8),
        TestAction::assert("Object.getPrototypeOf(proto) === Derived.prototype && proto.x === 1"),
        TestAction::assert("Object.getPrototypeOf(new F()) === Object.prototype"),
    ]);
}

/// `function_construct` reads `prototype` from the constructor's shape; it must see a changed
/// value, and a proxy as `Get` does.
#[test]
fn construct_follows_changes_to_prototype() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            function F() {}
            const first = new F();
            const other = { tag: 1 };
            F.prototype = other;
            const second = new F();
            const viaProxy = Reflect.construct(F, [], new Proxy(F, {
                get(target, key) { return key === "prototype" ? Array.prototype : target[key]; },
            }));
        "#}),
        TestAction::assert("Object.getPrototypeOf(first) === F.prototype === false"),
        TestAction::assert("Object.getPrototypeOf(second) === other"),
        TestAction::assert("Object.getPrototypeOf(viaProxy) === Array.prototype"),
    ]);
}
