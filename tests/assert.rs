//! Node's `assert` and `assert/strict`: passing checks are silent, failing ones throw an
//! `AssertionError` that reads as Node's does.

use std::path::Path;

use runtime_x::RuntimeX;

fn run(rt: &mut RuntimeX, body: &str) -> Vec<String> {
    let exports = rt
        .load_module(Path::new("/plugins/assert-test/main.js"), body)
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
fn passing_checks_are_silent_and_failing_ones_read_as_nodes() {
    let mut rt = RuntimeX::new();
    let result = run(
        &mut rt,
        r#"
        const assert = require('assert');
        const caught = (f) => { try { f(); return 'passed' } catch (e) { return e.message } };
        assert(true); assert.ok(1); assert.equal(1, '1'); assert.notEqual(1, 2);
        assert.strictEqual(NaN, NaN); assert.deepEqual({ a: 1 }, { a: '1' });
        assert.deepStrictEqual({ a: [1, { b: new Map([[1, 2]]) }] }, { a: [1, { b: new Map([[1, 2]]) }] });
        assert.notDeepStrictEqual({ a: 1 }, { a: '1' });
        assert.throws(() => { throw new TypeError('bad') }, TypeError);
        assert.throws(() => { throw new Error('nope 42') }, /nope \d+/);
        assert.throws(() => { throw new Error('x') }, { message: 'x', name: 'Error' });
        assert.throws(() => { throw new Error('x') }, e => e.message === 'x');
        assert.doesNotThrow(() => 1);
        assert.match('abc', /b/); assert.doesNotMatch('abc', /z/); assert.ifError(null);
        let error;
        try { assert.strictEqual(1, 2) } catch (e) { error = e }
        exports.result = [
            error.message, error.name, error.code, String(error.actual), String(error.expected), error.operator,
            String(error instanceof assert.AssertionError), String(error instanceof Error),
            String(error.generatedMessage), String(require('util').types.isNativeError(error)),
            caught(() => assert.deepStrictEqual({ a: 1 }, { a: 2 })),
            caught(() => assert.strictEqual({ a: 1 }, { a: 1 })),
            caught(() => assert.notStrictEqual(1, 1)),
            caught(() => assert.equal(1, 2)),
            caught(() => assert.ok(0)),
            caught(() => assert.strictEqual(1, 2, 'custom message')),
            caught(() => assert.throws(() => {})),
            caught(() => assert.throws(() => { throw new Error('x') }, TypeError)),
            caught(() => assert.doesNotThrow(() => { throw new Error('oops') })),
            caught(() => assert.match('abc', /z/)),
            caught(() => assert.fail()),
            caught(() => assert.fail('boom')),
            caught(() => assert.ifError(new Error('e'))),
            caught(() => assert.strictEqual(1, 2, new RangeError('mine'))),
        ];
    "#,
    );
    assert_eq!(
        result,
        [
            "Expected values to be strictly equal:\n\n1 !== 2\n",
            "AssertionError",
            "ERR_ASSERTION",
            "1",
            "2",
            "strictEqual",
            "true",
            "true",
            "true",
            "true",
            "Expected values to be strictly deep-equal:\n\n{ a: 1 } !== { a: 2 }\n",
            "Values have same structure but are not reference-equal:\n\n{ a: 1 }\n",
            "Expected \"actual\" to be strictly unequal to: 1",
            "1 == 2",
            "The expression evaluated to a falsy value:\n\n  assert.ok(0)\n",
            "custom message",
            "Missing expected exception.",
            "The error is expected to be an instance of \"TypeError\". Received \"Error\"",
            "Got unwanted exception.\nActual message: \"oops\"",
            "The input did not match the regular expression /z/. Input:\n\n'abc'\n",
            "Failed",
            "boom",
            "ifError got unwanted exception: e",
            "mine",
        ]
    );
}

#[test]
fn strict_mode_and_promises() {
    let mut rt = RuntimeX::new();
    rt.load_module(
        Path::new("/plugins/assert-strict/main.js"),
        r#"
        const strict = require('node:assert/strict');
        const assert = require('assert');
        globalThis.heard = [String(strict === assert.strict)];
        try { strict.equal(1, '1') } catch (e) { heard.push(e.operator) }
        try { strict.deepEqual({ a: 1 }, { a: '1' }) } catch (e) { heard.push(e.operator) }
        assert.rejects(Promise.reject(new Error('r')), /r/).then(() => heard.push('rejects ok'));
        assert.rejects(async () => 1).catch(e => heard.push(e.message));
        assert.doesNotReject(Promise.resolve(1)).then(() => heard.push('doesNotReject ok'));
        assert.doesNotReject(Promise.reject(new Error('no'))).catch(e => heard.push(e.message.split('\n')[0]));
        "#,
    )
    .unwrap();
    rt.run_microtasks();
    assert_eq!(
        runtime_x::print(&rt.eval("heard.join('|')").unwrap()),
        "true|strictEqual|deepStrictEqual|rejects ok|Missing expected rejection.|doesNotReject ok|Got unwanted rejection."
    );
}
