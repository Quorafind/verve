//! Tests of the element stores and the division compiled code does without the interpreter's
//! handlers: each fallback condition is a program whose result must match the interpreter's.

use super::tests::same;

/// `a[i] = x` past the end of a plain dense array is done by a fast helper; every other array,
/// index and `length` takes the interpreter's `[[Set]]`.
#[test]
fn appending_stores_match_the_interpreter() {
    let out = same(
        "const out = [];
         const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
         const fill = (a, n, v) => { for (let i = 0; i < n; i++) a[a.length] = v === undefined ? i : v; return a; };
         t(() => fill([], 40));
         t(() => fill([], 40, 0.5));
         t(() => fill([1.5], 40, 'text'));
         t(() => { const a = []; for (let i = 0; i < 40; i++) a[i] = i * 2; return [a, a.length]; });
         t(() => { const a = new Array(50); for (let i = 0; i < 40; i++) a[i] = i; return [a.length, a.slice(0, 3), 39 in a, 45 in a]; });
         t(() => { const a = new Array(3); for (let i = 0; i < 10; i++) a[i] = i; return [a, a.length]; });
         t(() => { const a = []; for (let i = 0; i < 20; i++) a[i + (i > 9 ? 5 : 0)] = i; return [a, a.length]; });
         t(() => { const a = [1, 2, 3]; for (let i = 3; i > 0; i--) a[i] = 9; return a; });
         t(() => { const a = [1, , 3]; for (let i = 0; i < 20; i++) a[i] = i; return a; });
         t(() => { const a = Object.freeze([1, 2]); for (let i = 0; i < 20; i++) a[2 + (i % 3)] = i; return a; });
         t(() => { 'use strict'; const a = Object.freeze([1, 2]); for (let i = 0; i < 20; i++) a[2] = i; return a; });
         t(() => { const a = Object.seal([1, 2]); for (let i = 0; i < 20; i++) a[2] = i; return [a, a.length]; });
         t(() => { const a = Object.preventExtensions([1, 2]); for (let i = 0; i < 20; i++) a[2] = i; return [a, a.length]; });
         t(() => { const a = [1, 2]; Object.defineProperty(a, 'length', { writable: false }); for (let i = 0; i < 20; i++) a[2] = i; return [a, a.length]; });
         t(() => { 'use strict'; const a = [1, 2]; Object.defineProperty(a, 'length', { writable: false }); for (let i = 0; i < 20; i++) a[2] = i; return a; });
         t(() => { const log = []; Object.defineProperty(Array.prototype, '5', { set(v) { log.push('set ' + v); }, configurable: true });
                   const a = []; for (let i = 0; i < 10; i++) a[i] = i; delete Array.prototype[5]; return [a, log.join()]; });
         t(() => { Object.defineProperty(Object.prototype, '7', { get() { return 'inherited'; }, set(v) { }, configurable: true });
                   const a = []; for (let i = 0; i < 10; i++) a[i] = i; delete Object.prototype[7]; return a; });
         t(() => { Array.prototype[3] = 'proto'; const a = []; for (let i = 0; i < 6; i++) a[i] = i; delete Array.prototype[3]; return a; });
         t(() => { const a = []; Object.setPrototypeOf(a, { set 2(v) { out.push('own setter ' + v); } }); for (let i = 0; i < 5; i++) a[i] = i; return [a, a.length]; });
         t(() => { class X extends Array {} const a = new X(); for (let i = 0; i < 20; i++) a[i] = i; return [a.length, a instanceof X]; });
         t(() => { const a = []; Object.defineProperty(a, 'extra', { value: 1 }); for (let i = 0; i < 20; i++) a[i] = i; return [a, a.extra]; });
         t(() => { const a = new Proxy([], {}); for (let i = 0; i < 20; i++) a[i] = i; return [a.length]; });
         t(() => { const o = { length: 0 }; for (let i = 0; i < 20; i++) o[i] = i; return o.length; });
         t(() => { const a = []; for (let i = 0; i < 20; i++) a[i] = {}; a[20] = a; return [a.length, a[20] === a]; });
         t(() => { const a = []; a[4294967294] = 1; for (let i = 0; i < 3; i++) a[i] = i; return [a.length]; });
         t(() => { const a = []; for (let i = 0; i < 20; i++) a[i] = i; a.length = 5; for (let i = 5; i < 12; i++) a[i] = -i; return a; });
         t(() => { const a = []; for (let i = 0; i < 20; i++) { a[i] = i; if (i == 10) Object.preventExtensions(a); } return [a, a.length]; });
         out.join(' ')",
    );
    // Node 22's output for the same program.
    assert_eq!(
        out,
        r#"[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39] [0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5,0.5] [1.5,"text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text","text"] [[0,2,4,6,8,10,12,14,16,18,20,22,24,26,28,30,32,34,36,38,40,42,44,46,48,50,52,54,56,58,60,62,64,66,68,70,72,74,76,78],40] [50,[0,1,2],true,false] [[0,1,2,3,4,5,6,7,8,9],10] [[0,1,2,3,4,5,6,7,8,9,,,,,,10,11,12,13,14,15,16,17,18,19],25] [1,9,9,9] [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19] [1,2] TypeError [[1,2],2] [[1,2],2] [[1,2],2] TypeError [[0,1,2,3,4,,6,7,8,9],"set 5"] [0,1,2,3,4,5,6,,8,9] [0,1,2,3,4,5] own setter 2 TypeError [20,true] [[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19],1] [20] 0 [21,true] [4294967295] [0,1,2,3,4,-5,-6,-7,-8,-9,-10,-11] [[0,1,2,3,4,5,6,7,8,9,10],11]"#
    );
}

/// `/`: the quotients of `int32`s that are whole numbers are `int32`s, the others `f64`s;
/// zero divisors, `-0`, `NaN`, the infinities and `i32::MIN / -1` give what the interpreter does.
#[test]
fn division_matches_the_interpreter() {
    let out = same(
        "const vals = [0, -0, 1, -1, 2, -2, 3, 6, -6, 7, 0.5, -0.5, 2147483647, -2147483648, 2147483648,
                       4294967295, 1e308, 5e-324, NaN, Infinity, -Infinity, '6', true, null, undefined, 3n];
         const out = [];
         function div(a, b) { return a / b; }
         for (const a of vals) for (const b of vals) {
           try { out.push(show(div(a, b))); } catch (e) { out.push(e.name); }
         }
         let s = 0;
         for (let i = 1; i < 300; i++) s += div(i * 7, i) + div(i, 3) + div(-i, 4) + div(0, i) + div(0, -i);
         out.push(show(s), show(div(0, -5)), show(div(-0, 5)), show(div(1, 0)), show(div(-1, 0)), show(div(0, 0)));
         out.join(' ')",
    );
    // The last results, as Node 22 has them: `-0 / 5` (`-0`), `1 / 0`, `-1 / 0` and `0 / 0`. The
    // interpreter's `0 / -5` is `0` where Node's is `-0` (see the risks in `docs/jit.md`); compiled
    // code leaves that to it, and the three modes above agree.
    assert!(out.ends_with("-0 Infinity -Infinity NaN"), "{out}");
}

/// `super[key] = value` stores on the receiver, not on the object `super` finds, whatever the
/// object is: a store into the dense array or typed array that is the prototype would be
/// wrong. The three modes must agree (and an interpreter that stores into the array is the
/// interpreter's own fault, not compiled code's).
#[test]
fn stores_through_super_take_the_receiver() {
    same(
        "const out = [];
         const run = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
         class Base { setAt(i, v) { super[i] = v; return this; } }
         const dense = [1, 2, 3];
         Object.setPrototypeOf(Base.prototype, dense);
         const typed = new Float64Array(4);
         class Other { setAt(i, v) { super[i] = v; return this; } }
         Object.setPrototypeOf(Other.prototype, typed);
         for (let i = 0; i < 20; i++) {
           run(() => { const o = new Base().setAt(i % 3, i); return [Object.keys(o).length, dense.join()]; });
           run(() => { const o = new Other().setAt(i % 4, i + 0.5); return [Object.keys(o).length, [...typed].join()]; });
         }
         out.join(' ')",
    );
}

/// Typed arrays: every kind, out-of-range and negative indexes, values of the wrong type, a
/// `valueOf` that detaches the buffer, detached and shrinking buffers, and a `length` that is
/// shadowed, redefined on the prototypes or overridden in a subclass.
#[test]
fn typed_arrays_match_the_interpreter() {
    same(
        "const out = [];
         const run = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
         const kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array,
                        Uint32Array, Float32Array, Float64Array, BigInt64Array, BigUint64Array];
         const read = (a, i) => a[i];
         const write = (a, i, v) => { a[i] = v; return a[i]; };
         const len = (a) => a.length;
         for (const K of kinds) {
           const big = K.name.startsWith('Big');
           const a = new K(5);
           const vals = big ? [1n, -2n, 2n ** 63n, 5, 'x', null] : [1, -2.5, 300, 2 ** 33, NaN, -0, 1n, 'y', undefined];
           for (let i = -1; i < 8; i++) for (const v of vals) run(() => [write(a, i, v), len(a), read(a, i)]);
         }
         run(() => { const a = new Int8Array(4); const v = { valueOf() { return 7; } }; for (let i = 0; i < 6; i++) a[i % 4] = v; return [...a]; });
         run(() => { const buffer = new ArrayBuffer(8); const a = new Int8Array(buffer);
                     const v = { valueOf() { structuredClone(buffer, { transfer: [buffer] }); return 1; } };
                     const r = []; for (let i = 0; i < 4; i++) { try { a[i] = v; } catch (e) { r.push(e.name); } r.push(a.length, a[0]); } return r; });
         run(() => { const buffer = new ArrayBuffer(8, { maxByteLength: 16 }); const a = new Int8Array(buffer); const r = [];
                     for (let i = 0; i < 6; i++) { a[i] = i + 1; r.push(a.length, a[i]); if (i == 3) buffer.resize(2); } return r; });
         run(() => { const a = new Float64Array(3); Object.defineProperty(a, 'length', { value: 42 }); const r = []; for (let i = 0; i < 5; i++) r.push(len(a)); return r; });
         run(() => { class S extends Uint8Array { get length() { return 7; } } const a = new S(3); const r = []; for (let i = 0; i < 5; i++) r.push(len(a)); return r; });
         run(() => { const a = new Int16Array(3); const r = [len(a)];
                     for (let i = 0; i < 4; i++) r.push(len(a));
                     Object.defineProperty(Object.getPrototypeOf(Int16Array.prototype), 'length', { get() { return 'patched'; }, configurable: true });
                     for (let i = 0; i < 4; i++) r.push(len(a)); return r; });
         run(() => { const a = new Int16Array(3); return [len(a), len(new Int8Array(2)), len('abc'), len([1, 2]), len({ length: 9 })]; });
         run(() => { const a = new Int8Array(2); a.__proto__ = { length: 'own proto' }; return [len(a), len(a)]; });
         run(() => { const a = new Uint8Array([1, 2, 3]); let s = 0; for (let i = 0; i < 20; i++) s += read(a, i % 5) || 0; return s; });
         out.join(' ')",
    );
}
