//! Node's `util`: `inspect` and `format` writing values as Node writes them, deep strict
//! equality, `promisify` and `callbackify`, `deprecate`, the type checks — and the console,
//! which formats its arguments as Node's does.

use std::path::Path;

use runtime_x::RuntimeX;

/// Runs `body` as a CommonJS module; it sets `exports.result` to an array of strings.
fn run(rt: &mut RuntimeX, body: &str) -> Vec<String> {
    let exports = rt
        .load_module(Path::new("/plugins/util-test/main.js"), body)
        .unwrap_or_else(|e| panic!("{e}"));
    let json = rt.eval("JSON.stringify").unwrap();
    let text = rt
        .call(&json, &boa_engine::JsValue::undefined(), &[exports])
        .unwrap_or_else(|e| panic!("{e}"));
    let parsed: serde_json::Value = serde_json::from_str(&runtime_x::print(&text)).unwrap();
    parsed["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .collect()
}

#[test]
fn inspect_writes_values_as_node_does() {
    let mut rt = RuntimeX::new();
    let result = run(
        &mut rt,
        r#"
        const { inspect } = require('util');
        class Foo { constructor() { this.x = 1 } }
        class Bar extends Foo {}
        const circular = { name: 'c' }; circular.self = circular;
        exports.result = [
            inspect('it\'s'), inspect(-0), inspect(10n), inspect(Symbol('s')),
            inspect([1, 2, 3]), inspect({ a: 1, b: 'x' }), inspect({ a: { b: { c: { d: 1 } } } }),
            inspect(new Foo()), inspect(Bar), inspect(new Map([['a', 1]])), inspect(new Set([1, 2])),
            inspect(new Date(0)), inspect(/ab+c/gi), inspect(function named() {}), inspect(async function af() {}),
            inspect(new Number(3)), inspect(circular), inspect(Object.create(null)), inspect([1, , 3]),
            inspect(new Uint8Array([1, 2])), inspect(Promise.resolve(4)), inspect({ get g() { return 1 } }),
            inspect({ 'a-b': 2, [Symbol('k')]: 1 }), inspect({ a: [{ b: 1 }] }, { depth: 0 }),
            inspect({ [inspect.custom]: (depth, options) => 'custom ' + options.depth }),
            inspect(Array.from({ length: 30 }, (_, n) => n)),
            inspect({ a: 1, b: 2 }, { compact: false }),
            inspect(new Error('boom')).split('\n')[0],
            inspect('x'.repeat(5), { maxStringLength: 2 }),
            inspect(new Map([[{ k: 1 }, [1, 2]]])),
        ];
    "#,
    );
    let array30 = "[\n   0,  1,  2,  3,  4,  5,  6,  7,  8,\n   9, 10, 11, 12, 13, 14, 15, 16, 17,\n  18, 19, 20, 21, 22, 23, 24, 25, 26,\n  27, 28, 29\n]";
    assert_eq!(
        result,
        [
            "\"it's\"",
            "-0",
            "10n",
            "Symbol(s)",
            "[ 1, 2, 3 ]",
            "{ a: 1, b: 'x' }",
            "{ a: { b: { c: [Object] } } }",
            "Foo { x: 1 }",
            "[class Bar extends Foo]",
            "Map(1) { 'a' => 1 }",
            "Set(2) { 1, 2 }",
            "1970-01-01T00:00:00.000Z",
            "/ab+c/gi",
            "[Function: named]",
            "[AsyncFunction: af]",
            "[Number: 3]",
            "<ref *1> { name: 'c', self: [Circular *1] }",
            "[Object: null prototype] {}",
            "[ 1, <1 empty item>, 3 ]",
            "Uint8Array(2) [ 1, 2 ]",
            "Promise { 4 }",
            "{ g: [Getter] }",
            "{ 'a-b': 2, [Symbol(k)]: 1 }",
            "{ a: [Array] }",
            "custom 2",
            array30,
            "{\n  a: 1,\n  b: 2\n}",
            "Error: boom",
            "'xx'... 3 more characters",
            "Map(1) { { k: 1 } => [ 1, 2 ] }",
        ]
    );
}

#[test]
fn format_and_the_console_fill_in_specifiers() {
    let mut rt = RuntimeX::new();
    let result = run(
        &mut rt,
        r#"
        const util = require('util');
        console.log('%s has %d items', 'list', 3, { more: [1, 2] });
        console.group('outer');
        console.log('a\nb');
        console.groupEnd();
        exports.result = [
            util.format('%s is %d and %i, %f %j %O %%', 'x', 42, 3.7, '1.5', { a: 1 }, { b: 2 }),
            util.format('%s', { a: 1 }), util.format(1, 'a', { b: 1 }), util.format('%s %s', 'only'),
            util.format('%d', 10n), util.format('%c%s', 'color: red', 'styled'),
            util.formatWithOptions({ compact: false }, '%O', { a: 1 }),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "x is 42 and 3, 1.5 {\"a\":1} { b: 2 } %",
            "{ a: 1 }",
            "1 a { b: 1 }",
            "only %s",
            "10n",
            "styled",
            "{\n  a: 1\n}"
        ]
    );
    let console: Vec<String> = rt.take_console().into_iter().map(|l| l.text).collect();
    assert_eq!(
        console,
        ["list has 3 items { more: [ 1, 2 ] }", "outer", "  a\n  b"]
    );
}

#[test]
fn deep_strict_equality_and_the_type_checks() {
    let mut rt = RuntimeX::new();
    let result = run(
        &mut rt,
        r#"
        const util = require('util');
        const types = require('node:util/types');
        const eq = util.isDeepStrictEqual;
        const a = { x: [1, { y: new Map([[1, new Set([2])]]) }], d: new Date(5) };
        const b = { x: [1, { y: new Map([[1, new Set([2])]]) }], d: new Date(5) };
        const c1 = { n: 1 }; c1.me = c1;
        const c2 = { n: 1 }; c2.me = c2;
        exports.result = [
            eq(a, b), eq([1], ['1']), eq(NaN, NaN), eq(0, -0), eq({}, Object.create(null)), eq(c1, c2),
            eq({ a: undefined }, {}), eq(new Set([{ a: 1 }]), new Set([{ a: 1 }])), eq(/a/g, /a/i),
            types.isPromise(Promise.resolve()), types.isAsyncFunction(async () => {}),
            types.isGeneratorFunction(function* () {}), types.isUint8Array(Buffer.from('a')),
            types.isBoxedPrimitive(new Number(1)), types.isNativeError(new TypeError()), types.isMap(new Map()),
            types.isProxy(new Proxy({}, {})), types.isArrayBufferView(new DataView(new ArrayBuffer(1))),
            util.isArray([]), util.isPrimitive(null), require('sys') === util, util.types === types,
        ].map(String);
    "#,
    );
    assert_eq!(
        result,
        [
            "true", "false", "true", "false", "false", "true", "false", "true", "false", "true",
            "true", "true", "true", "true", "true", "true", "true", "true", "true", "true", "true",
            "true",
        ]
    );
}

#[test]
fn promisify_callbackify_deprecate_and_the_rest() {
    let mut rt = RuntimeX::new();
    rt.load_module(
        Path::new("/plugins/util-async/main.js"),
        r#"
        const util = require('util');
        globalThis.heard = [];
        util.promisify((a, cb) => cb(null, a * 2))(21).then(v => heard.push('promisify ' + v));
        function custom() {}
        custom[util.promisify.custom] = () => Promise.resolve('c');
        util.promisify(custom)().then(v => heard.push('custom ' + v));
        util.promisify((cb) => cb(new Error('no')))().catch(e => heard.push('rejected ' + e.message));
        util.callbackify(async x => x + 1)(1, (e, v) => heard.push('callbackify ' + e + ' ' + v));
        util.callbackify(async () => { throw null })(e => heard.push('falsy ' + e.code));
        const old = util.deprecate(() => 'r', 'old thing', 'DEP0999');
        old(); old();
        function A() {} function B() {}
        util.inherits(A, B);
        heard.push('inherits ' + (new A() instanceof B) + ' ' + (A.super_ === B));
        heard.push('mime ' + new util.MIMEType('text/html; charset="utf-8"'));
        heard.push('args ' + JSON.stringify(util.parseArgs({
            args: ['-f', '--bar=3', 'pos'], allowPositionals: true,
            options: { foo: { type: 'boolean', short: 'f' }, bar: { type: 'string' } },
        })));
        heard.push('debuglog ' + util.debuglog('nothing').enabled);
        "#,
    )
    .unwrap();
    rt.run_microtasks();
    let heard = runtime_x::print(&rt.eval("heard.join('|')").unwrap());
    assert_eq!(
        heard,
        "inherits true true|mime text/html;charset=utf-8|args {\"values\":{\"foo\":true,\"bar\":\"3\"},\"positionals\":[\"pos\"]}|debuglog false|promisify 42|custom c|rejected no|callbackify null 2|falsy ERR_FALSY_VALUE_REJECTION"
    );
    let warnings: Vec<String> = rt
        .take_console()
        .into_iter()
        .map(|l| l.text)
        .filter(|t| t.contains("DEP0999"))
        .collect();
    assert_eq!(warnings.len(), 1, "deprecate warns once");
}
