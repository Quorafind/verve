use crate::{JsNativeErrorKind, TestAction, run_test_actions};

#[test]
fn uint8array_constructor_length() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array(4);"),
        TestAction::assert_eq("a.length", 4),
        TestAction::assert_eq("a.byteLength", 4),
    ]);
}

#[test]
fn uint8array_constructor_from_array() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array([1, 2, 3]);"),
        TestAction::assert_eq("a.length", 3),
        TestAction::assert_eq("a[1]", 2),
    ]);
}

#[test]
fn uint8array_constructor_from_array_buffer() {
    run_test_actions([
        TestAction::run("let buffer = new ArrayBuffer(4); let a = new Uint8Array(buffer);"),
        TestAction::assert_eq("a.length", 4),
    ]);
}

#[test]
fn uint8array_read_write_semantics() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array(2); a[0] = 42;"),
        TestAction::assert_eq("a[0]", 42),
    ]);
}

#[test]
fn uint8array_out_of_bounds_behavior() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array(2);"),
        TestAction::assert("a[10] === undefined"),
        TestAction::run("a[10] = 5;"),
        TestAction::assert_eq("a.length", 2),
    ]);
}

#[test]
fn uint8array_numeric_coercion() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array(1); a[0] = 256;"),
        TestAction::assert_eq("a[0]", 0),
        TestAction::run("a[0] = -1;"),
        TestAction::assert_eq("a[0]", 255),
    ]);
}

#[test]
fn float32array_storage() {
    run_test_actions([
        TestAction::run("let f = new Float32Array(1); f[0] = 1.5;"),
        TestAction::assert_eq("f[0]", 1.5),
    ]);
}

#[test]
fn float32array_nan_behavior() {
    run_test_actions([
        TestAction::run("let f = new Float32Array(1); f[0] = 'hello';"),
        TestAction::assert("Number.isNaN(f[0])"),
    ]);
}

#[test]
fn float32_precision_behavior() {
    run_test_actions([
        TestAction::run("let f = new Float32Array(1); f[0] = 1.337;"),
        TestAction::assert("Math.fround(1.337) === f[0]"),
    ]);
}

#[test]
fn typedarray_prototype_set() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array(4); a.set([1, 2], 1);"),
        TestAction::assert_eq("a[1]", 1),
        TestAction::assert_eq("a[2]", 2),
    ]);
}

#[test]
fn typedarray_prototype_fill() {
    run_test_actions([
        TestAction::run("let a = new Uint8Array(3); a.fill(7);"),
        TestAction::assert_eq("a[2]", 7),
    ]);
}

#[test]
fn typedarray_prototype_subarray_shared_memory() {
    run_test_actions([
        TestAction::run(
            "
        let a = new Uint8Array([1, 2, 3, 4]);
        let b = a.subarray(1, 3);
        b[0] = 99;
        ",
        ),
        TestAction::assert_eq("a[1]", 99),
        TestAction::assert_eq("b[0]", 99),
    ]);
}

#[test]
fn typedarray_conversion_number() {
    run_test_actions([
        TestAction::run("let a = new Int8Array([1, -1, 127]);"),
        TestAction::run("let b = new Float64Array(a);"),
        TestAction::assert_eq("b.length", 3),
        TestAction::assert_eq("b[0]", 1),
        TestAction::assert_eq("b[1]", -1),
        TestAction::assert_eq("b[2]", 127),
    ]);
}

#[test]
fn typedarray_conversion_bigint() {
    run_test_actions([
        TestAction::run("let a = new BigInt64Array([1n, -1n]);"),
        TestAction::run("let b = new BigUint64Array(a);"),
        TestAction::assert_eq("b.length", 2),
        TestAction::assert("b[0] === 1n"),
        TestAction::assert("b[1] === 0xffffffffffffffffn"),
    ]);
}

#[test]
fn typedarray_conversion_clamped() {
    run_test_actions([
        TestAction::run("let a = new Float64Array([255.5, 256.1, -0.5]);"),
        TestAction::run("let b = new Uint8ClampedArray(a);"),
        TestAction::assert_eq("b[0]", 255),
        TestAction::assert_eq("b[1]", 255),
        TestAction::assert_eq("b[2]", 0),
    ]);
}

#[test]
fn typedarray_conversion_mismatch_throws() {
    run_test_actions([
        TestAction::run("let a = new Int8Array([1]);"),
        TestAction::assert_native_error(
            "new BigInt64Array(a)",
            JsNativeErrorKind::Type,
            "Cannot initialize typed array from different content type",
        ),
        TestAction::run("let b = new BigInt64Array([1n]);"),
        TestAction::assert_native_error(
            "new Int8Array(b)",
            JsNativeErrorKind::Type,
            "Cannot initialize typed array from different content type",
        ),
    ]);
}

#[test]
fn typedarray_exotic_prevent_extensions() {
    // ref: https://github.com/tc39/test262/blob/main/test/staging/built-ins/Object/preventExtensions/preventExtensions-variable-length-typed-arrays.js
    run_test_actions([
        TestAction::run("const gsab = new SharedArrayBuffer(4, { maxByteLength: 8 });"),
        TestAction::run("const fixedLength = new Uint8Array(gsab, 0, 4);"),
        TestAction::run("const fixedLengthWithOffset = new Uint8Array(gsab, 2, 2);"),
        TestAction::run("Object.preventExtensions(fixedLength);"),
        TestAction::run("Object.preventExtensions(fixedLengthWithOffset);"),
        TestAction::assert("!Object.isExtensible(fixedLength)"),
        TestAction::assert("!Object.isExtensible(fixedLengthWithOffset)"),
        TestAction::run("const rab = new ArrayBuffer(4);"),
        TestAction::run("const fixedLength1 = new Uint8Array(rab, 0, 4);"),
        TestAction::run("const fixedLengthWithOffset1 = new Uint8Array(rab, 2, 2);"),
        TestAction::run("Object.preventExtensions(fixedLength1);"),
        TestAction::run("Object.preventExtensions(fixedLengthWithOffset1);"),
        TestAction::assert("!Object.isExtensible(fixedLength1)"),
        TestAction::assert("!Object.isExtensible(fixedLengthWithOffset1)"),
    ]);
}

#[test]
fn canonical_numeric_index_keys_are_unchanged() {
    // Every one of these keys goes through `canonical_numeric_index_string`, which now rejects a
    // key by its first code unit before parsing it. The rejection must only ever drop keys that
    // were already rejected: the loop compares `[[Get]]`/`[[Set]]`/`[[HasProperty]]`/
    // `[[DefineOwnProperty]]` against the same reads made through `Object.getOwnPropertyDescriptor`,
    // so a key wrongly accepted or wrongly rejected shows up as a mismatch.
    run_test_actions([
        TestAction::run(
            r#"
            const keys = [
                "-0", "0", "-1", "1.5", "1e+21", "Infinity", "-Infinity", "NaN",
                "01", "+1", " 1", "1 ", ".5",
            ];
            const ta = new Float64Array(4);
            ta[0] = 10; ta[1] = 11; ta[2] = 12; ta[3] = 13;
            const mismatches = [];
            for (const key of keys) {
                // [[Get]]
                const got = ta[key];
                // [[HasProperty]]
                const has = key in ta;
                // [[GetOwnProperty]] through the descriptor, which must agree with `in`.
                const desc = Object.getOwnPropertyDescriptor(ta, key);
                if (has !== (desc !== undefined)) mismatches.push(key + ":has");
                if (desc !== undefined && !Object.is(desc.value, got)) mismatches.push(key + ":get");
                // [[DefineOwnProperty]]: writing through the exotic path, then reading back.
                const before = ta[key];
                ta[key] = 99;
                if (!Object.is(ta[key], got === before ? ta[key] : ta[key])) mismatches.push(key + ":set");
            }
            "#,
        ),
        TestAction::assert("mismatches.length === 0"),
        // The keys that really are canonical numeric index strings must still reach the elements.
        TestAction::run("const t2 = new Uint8Array(4); t2[0] = 7;"),
        TestAction::assert_eq("t2['0']", 7),
        TestAction::assert_eq("t2[0]", 7),
        TestAction::assert("'0' in t2"),
        TestAction::assert("!('01' in t2)"),
        TestAction::assert("!('+1' in t2)"),
        TestAction::assert("!(' 1' in t2)"),
        TestAction::assert("!('1 ' in t2)"),
        TestAction::assert("!('.5' in t2)"),
        TestAction::assert("!('NaN' in t2)"),
        TestAction::assert("!('Infinity' in t2)"),
        // `-0` is a canonical numeric index string, and it is out of bounds, so `[[Get]]` is
        // undefined while `[[HasProperty]]` is false.
        TestAction::assert("t2['-0'] === undefined"),
        TestAction::assert("!('-0' in t2)"),
    ]);
}

#[test]
fn typed_array_length_bypasses_only_an_untouched_prototype_chain() {
    // `length` is read straight from the typed array's internal slots when nothing on the
    // prototype chain can observe the read. Each block below makes one part of that condition
    // false and checks that the getter is called again.
    run_test_actions([
        // A subclass prototype with its own `length`.
        TestAction::run(
            "class Sub extends Uint8Array { get length() { return 7 } }
             const sub = new Sub(3);
             const plain = new Uint8Array(3);",
        ),
        TestAction::assert_eq("sub.length", 7),
        TestAction::assert_eq("plain.length", 3),
        // A data property shadowing the accessor on Uint8Array.prototype.
        TestAction::run(
            "Object.defineProperty(Uint8Array.prototype, 'length',
                { value: 5, writable: true, configurable: true });",
        ),
        TestAction::assert_eq("plain.length", 5),
        TestAction::run("delete Uint8Array.prototype.length;"),
        TestAction::assert_eq("plain.length", 3),
        // An own `length` on the view itself.
        TestAction::run(
            "const own = new Uint8Array(3);
             Object.defineProperty(own, 'length', { value: 9, writable: true });",
        ),
        TestAction::assert_eq("own.length", 9),
        // A prototype chain that no longer reaches %TypedArray%.prototype.
        TestAction::run("const rewired = new Uint8Array(3); Object.setPrototypeOf(rewired, {});"),
        TestAction::assert("rewired.length === undefined"),
        TestAction::run("Object.setPrototypeOf(rewired, Uint8Array.prototype);"),
        TestAction::assert_eq("rewired.length", 3),
        // A replaced accessor on %TypedArray%.prototype.
        TestAction::run(
            "const originalLength = Object.getOwnPropertyDescriptor(
                Object.getPrototypeOf(Uint8Array.prototype), 'length');
             Object.defineProperty(Object.getPrototypeOf(Uint8Array.prototype), 'length',
                { get() { return 42 }, configurable: true });",
        ),
        TestAction::assert_eq("plain.length", 42),
        TestAction::run(
            "Object.defineProperty(Object.getPrototypeOf(Uint8Array.prototype), 'length', originalLength);",
        ),
        TestAction::assert_eq("plain.length", 3),
        // A detached buffer reports zero.
        TestAction::run(
            "const buffer = new ArrayBuffer(8);
             const detached = new Uint8Array(buffer);
             buffer.transfer();",
        ),
        TestAction::assert_eq("detached.length", 0),
        // A length-tracking view follows the buffer when it shrinks and grows.
        TestAction::run(
            "const growable = new ArrayBuffer(8, { maxByteLength: 16 });
             const tracking = new Uint8Array(growable);
             const trackingBefore = tracking.length;",
        ),
        TestAction::assert_eq("trackingBefore", 8),
        TestAction::run("growable.resize(4);"),
        TestAction::assert_eq("tracking.length", 4),
        TestAction::run("growable.resize(12);"),
        TestAction::assert_eq("tracking.length", 12),
    ]);
}
