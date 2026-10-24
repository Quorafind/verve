use crate::{JsValue, TestAction, run_test_actions};
use boa_macros::js_str;
use indoc::indoc;

#[test]
fn error_to_string() {
    run_test_actions([
        TestAction::assert_eq("(new Error('1')).toString()", js_str!("Error: 1")),
        TestAction::assert_eq("(new RangeError('2')).toString()", js_str!("RangeError: 2")),
        TestAction::assert_eq(
            "(new ReferenceError('3')).toString()",
            js_str!("ReferenceError: 3"),
        ),
        TestAction::assert_eq(
            "(new SyntaxError('4')).toString()",
            js_str!("SyntaxError: 4"),
        ),
        TestAction::assert_eq("(new TypeError('5')).toString()", js_str!("TypeError: 5")),
        TestAction::assert_eq("(new EvalError('6')).toString()", js_str!("EvalError: 6")),
        TestAction::assert_eq("(new URIError('7')).toString()", js_str!("URIError: 7")),
        // no message
        TestAction::assert_eq("(new Error()).toString()", js_str!("Error")),
        TestAction::assert_eq("(new RangeError()).toString()", js_str!("RangeError")),
        TestAction::assert_eq(
            "(new ReferenceError()).toString()",
            js_str!("ReferenceError"),
        ),
        TestAction::assert_eq("(new SyntaxError()).toString()", js_str!("SyntaxError")),
        TestAction::assert_eq("(new TypeError()).toString()", js_str!("TypeError")),
        TestAction::assert_eq("(new EvalError()).toString()", js_str!("EvalError")),
        TestAction::assert_eq("(new URIError()).toString()", js_str!("URIError")),
        // no name
        TestAction::assert_eq(
            indoc! {r#"
                let message = new Error('message');
                message.name = '';
                message.toString()
            "#},
            js_str!("message"),
        ),
    ]);
}

#[test]
fn error_names() {
    run_test_actions([
        TestAction::assert_eq("Error.name", js_str!("Error")),
        TestAction::assert_eq("EvalError.name", js_str!("EvalError")),
        TestAction::assert_eq("RangeError.name", js_str!("RangeError")),
        TestAction::assert_eq("ReferenceError.name", js_str!("ReferenceError")),
        TestAction::assert_eq("SyntaxError.name", js_str!("SyntaxError")),
        TestAction::assert_eq("URIError.name", js_str!("URIError")),
        TestAction::assert_eq("TypeError.name", js_str!("TypeError")),
        TestAction::assert_eq("AggregateError.name", js_str!("AggregateError")),
    ]);
}

#[test]
fn error_lengths() {
    run_test_actions([
        TestAction::assert_eq("Error.length", 1),
        TestAction::assert_eq("EvalError.length", 1),
        TestAction::assert_eq("RangeError.length", 1),
        TestAction::assert_eq("ReferenceError.length", 1),
        TestAction::assert_eq("SyntaxError.length", 1),
        TestAction::assert_eq("URIError.length", 1),
        TestAction::assert_eq("TypeError.length", 1),
        TestAction::assert_eq("AggregateError.length", 2),
    ]);
}

#[test]
fn error_stack_is_in_v8_format() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            function outer() { return inner(); }
            function inner() { return new TypeError('boom'); }
            const lines = outer().stack.split('\n');

            let thrown;
            function reads(o) { return o.x; }
            try { reads(null); } catch (e) { thrown = e.stack.split('\n'); }

            class MyError extends Error {
                constructor(m) { super(m); this.name = 'MyError'; }
            }
            const mine = new MyError('mine').stack.split('\n');
            class K { method() { return new Error('m').stack.split('\n'); } }
            const method = new K().method();
        "#}),
        TestAction::assert_eq("lines[0]", js_str!("TypeError: boom")),
        TestAction::assert(r"/^    at inner \(<anonymous>:2:\d+\)$/.test(lines[1])"),
        TestAction::assert(r"/^    at outer \(<anonymous>:1:\d+\)$/.test(lines[2])"),
        TestAction::assert(r"/^    at <anonymous>:3:\d+$/.test(lines[3])"),
        TestAction::assert_eq("lines.length", 4),
        TestAction::assert_eq(
            "thrown[0]",
            js_str!("TypeError: Cannot read properties of null (reading 'x')"),
        ),
        TestAction::assert(r"/^    at reads \(<anonymous>:6:\d+\)$/.test(thrown[1])"),
        TestAction::assert_eq("mine[0]", js_str!("MyError: mine")),
        TestAction::assert("mine[1].startsWith('    at MyError (')"),
        TestAction::assert("method[1].startsWith('    at method (')"),
        TestAction::assert_eq("typeof Error.prototype.stack", js_str!("undefined")),
    ]);
}

#[test]
fn error_capture_stack_trace() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            const plain = {};
            function captures() { Error.captureStackTrace(plain); }
            captures();

            function MyError(message) {
                Error.captureStackTrace(this, MyError);
                this.message = message;
            }
            MyError.prototype = Object.create(Error.prototype);
            MyError.prototype.name = 'MyError';
            function makes() { return new MyError('late message'); }
            const mine = makes();

            const absent = {};
            Error.captureStackTrace(absent, function notOnTheStack() {});

            const replaced = {};
            Error.captureStackTrace(replaced);
            replaced.stack = 'replaced';
            const desc = Object.getOwnPropertyDescriptor(plain, 'stack');
        "#}),
        TestAction::assert(r"plain.stack.startsWith('Error\n    at captures (')"),
        TestAction::assert(r"mine.stack.startsWith('MyError: late message\n    at makes (')"),
        TestAction::assert("!mine.stack.includes('at MyError')"),
        TestAction::assert_eq("absent.stack", js_str!("Error")),
        TestAction::assert_eq("replaced.stack", js_str!("replaced")),
        TestAction::assert("!desc.enumerable && desc.configurable"),
        TestAction::assert_eq("Error.captureStackTrace.length", 2),
        TestAction::assert_native_error(
            "Error.captureStackTrace(1)",
            crate::JsNativeErrorKind::Type,
            "Error.captureStackTrace: invalid argument",
        ),
    ]);
}

#[test]
fn error_stack_trace_limit() {
    run_test_actions([
        TestAction::assert_eq("Error.stackTraceLimit", 10),
        TestAction::assert(indoc! {r#"
            const d = Object.getOwnPropertyDescriptor(Error, 'stackTraceLimit');
            d.writable && d.enumerable && d.configurable
        "#}),
        TestAction::run(indoc! {r#"
            function deep(n) { return n === 0 ? new Error('deep') : deep(n - 1); }
            const ten = deep(30).stack.split('\n').length;
            Error.stackTraceLimit = 2;
            const two = deep(30).stack.split('\n').length;
            Error.stackTraceLimit = 0;
            const none = deep(30).stack;
            Error.stackTraceLimit = 'not a number';
            const notNumber = deep(30).stack;
            Error.stackTraceLimit = 10;
        "#}),
        TestAction::assert_eq("ten", 11),
        TestAction::assert_eq("two", 3),
        TestAction::assert_eq("none", js_str!("Error: deep")),
        TestAction::assert_eq("notNumber", js_str!("Error: deep")),
    ]);
}

#[test]
fn error_prepare_stack_trace() {
    run_test_actions([
        TestAction::run(indoc! {r#"
            // As `depd` (express, body-parser) gets its caller.
            function getStack() {
                var limit = Error.stackTraceLimit;
                var obj = {};
                var prep = Error.prepareStackTrace;
                Error.prepareStackTrace = function (o, stack) { return stack; };
                Error.stackTraceLimit = Math.max(10, limit);
                Error.captureStackTrace(obj);
                var stack = obj.stack.slice(1);
                Error.prepareStackTrace = prep;
                Error.stackTraceLimit = limit;
                return stack;
            }
            function depd() { return getStack(); }
            function caller() { return depd(); }
            const sites = caller();

            Error.prepareStackTrace = (error, sites) => `${error.message}:${sites.length}`;
            const custom = new Error('custom').stack;
            Error.prepareStackTrace = (error, sites) => 'outer ' + error.stack;
            const nested = new Error('nested').stack;
            delete Error.prepareStackTrace;
            const plain = new Error('plain').stack;
        "#}),
        TestAction::assert("Array.isArray(sites)"),
        TestAction::assert_eq("sites[0].getFunctionName()", js_str!("depd")),
        TestAction::assert_eq("sites[1].getFunctionName()", js_str!("caller")),
        TestAction::assert_eq("sites[2].getFunctionName()", JsValue::null()),
        TestAction::assert_eq("sites[0].getLineNumber()", 14),
        TestAction::assert_eq("typeof sites[0].getColumnNumber()", js_str!("number")),
        TestAction::assert_eq("sites[0].getFileName()", JsValue::undefined()),
        TestAction::assert("!sites[0].isNative() && !sites[0].isEval() && sites[0].isToplevel()"),
        TestAction::assert("!sites[0].isConstructor() && sites[0].getThis() === undefined"),
        TestAction::assert("String(sites[0]).startsWith('depd (<anonymous>:14:')"),
        TestAction::assert("custom.startsWith('custom:')"),
        TestAction::assert(r"nested.startsWith('outer Error: nested\n    at ')"),
        TestAction::assert(r"plain.startsWith('Error: plain\n    at ')"),
    ]);
}
