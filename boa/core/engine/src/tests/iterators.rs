use indoc::indoc;

use crate::{TestAction, run_test_actions};

#[test]
fn iterator_close_in_continue_before_jobs() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
            var actual = [];

            var iter = {
                [Symbol.iterator]() {
                    return this;
                },
                next() {
                    actual.push("call next");
                    return {
                        done: false,
                    };
                },
                get return() {
                    actual.push("get return");
                    return function () {
                        actual.push("return call");
                        return {
                            done: true
                        }
                    }
                }
            };

            Promise.resolve(0)
                .then(() => actual.push("tick 1"))
                .then(() => actual.push("tick 2"));

            void async function f() {
                actual.push("async fn start");
                let count = 0;
                loop: while (count === 0) {
                    count++;
                    for (_ of iter) {
                        continue loop;
                    }
                }
                actual.push("async fn end");
            }();
        "#}),
        TestAction::inspect_context(|ctx| ctx.run_jobs().unwrap()),
        TestAction::assert(indoc! {r#"
            arrayEquals(
                actual,
                [
                    "async fn start",
                    "call next",
                    "get return",
                    "return call",
                    "async fn end",
                    "tick 1",
                    "tick 2",
                ]
            )
            "#}),
    ]);
}

#[test]
fn async_iterator_close_in_continue_is_awaited() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
            var actual = [];

            var asyncIter = {
                [Symbol.asyncIterator]() {
                    return this;
                },
                next() {
                    actual.push("async call next");
                    return {
                        done: false,
                    };
                },
                get return() {
                    actual.push("get async return");
                    return function () {
                        actual.push("async return call");
                        return {
                            done: true
                        };
                    }
                }
            };

            Promise.resolve(0)
                .then(() => actual.push("tick 1"))
                .then(() => actual.push("tick 2"))
                .then(() => actual.push("tick 3"));

            void async function f() {
                actual.push("async fn start");
                let count = 0;
                loop: while (count === 0) {
                    count++;
                    for await (__ of asyncIter) {
                        continue loop;
                    }
                }
                actual.push("async fn end");
            }();
        "#}),
        TestAction::inspect_context(|ctx| ctx.run_jobs().unwrap()),
        TestAction::assert(indoc! {r#"
            arrayEquals(
                actual,
                [
                    "async fn start",
                    "async call next",
                    "tick 1",
                    "get async return",
                    "async return call",
                    "tick 2",
                    "async fn end",
                    "tick 3"
                ]
            )
        "#}),
    ]);
}

#[test]
fn mixed_iterators_close_in_continue() {
    run_test_actions([
        TestAction::run_harness(),
        TestAction::run(indoc! {r#"
            var actual = [];

            var iter = {
                [Symbol.iterator]() {
                    return this;
                },
                next() {
                    actual.push("call next");
                    return {
                        done: false,
                    };
                },
                get return() {
                    actual.push("get return");
                    return function () {
                        actual.push("return call");
                        return {
                            done: true
                        }
                    }
                }
            };

            var asyncIter = {
                [Symbol.asyncIterator]() {
                    return this;
                },
                next() {
                    actual.push("async call next");
                    return {
                        done: false,
                    };
                },
                get return() {
                    actual.push("get async return");
                    return function () {
                        actual.push("async return call");
                        return {
                            done: true
                        };
                    }
                }
            };

            Promise.resolve(0)
                .then(() => actual.push("tick 1"))
                .then(() => actual.push("tick 2"))
                .then(() => actual.push("tick 3"));

            void async function f() {
                actual.push("async fn start");
                let count = 0;
                loop: while (count === 0) {
                    count++;
                    for (_ of iter) {
                        for await (__ of asyncIter) {
                            continue loop;
                        }
                    }
                }
                actual.push("async fn end");
            }();
        "#}),
        TestAction::inspect_context(|ctx| ctx.run_jobs().unwrap()),
        TestAction::assert(indoc! {r#"
            arrayEquals(
                actual,
                [
                    "async fn start",
                    "call next",
                    "async call next",
                    "tick 1",
                    "get async return",
                    "async return call",
                    "tick 2",
                    "get return",
                    "return call",
                    "async fn end",
                    "tick 3",
                ]
            )
        "#}),
    ]);
}

#[test]
fn string_iteration_yields_each_code_point() {
    run_test_actions([
        TestAction::assert(indoc! {r#"
            const text = 'a\u00e9\u00ff\u0100\u20ac\ud83d\ude00\ud83dz\ude00 \u0000';
            const seen = [...text];
            seen.map((c) => c.codePointAt(0)).join() === '97,233,255,256,8364,128512,55357,122,56832,32,0'
                && seen.map((c) => c.length).join() === '1,1,1,1,1,2,1,1,1,1,1'
        "#}),
        TestAction::assert(indoc! {r#"
            let all = '';
            for (let code = 0; code < 256; code++) {
                const ch = String.fromCharCode(code);
                const [first] = ch + ch;
                if (first !== ch || first.length !== 1 || first.charCodeAt(0) !== code) throw code;
                all += first;
            }
            const wide = '\u20ac' + all;
            [...wide].length === 257 && [...wide].slice(1).join('') === all
        "#}),
    ]);
}
