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
