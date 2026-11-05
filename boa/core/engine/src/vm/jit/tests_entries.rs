//! Tests of `for (const [k, v] of map)` and its two siblings, which take the key and the value of a
//! built-in entry without making the `[key, value]` array when destructuring that array cannot be
//! observed. Every program runs interpreted, compiled eagerly and compiled when hot, and the
//! result must be what Node gave (frozen below, Node 22), which is also what the spec says.

use boa_parser::Source;

use super::tests::{Mode, SHOW, context, same};
use crate::{JsValue, NativeFunction, js_string};

#[track_caller]
fn matches_node(source: &str, node: &str) {
    assert_eq!(same(source), node);
}

/// The three loops, in `const`, `let`, `var` and assignment forms, over odd keys and values.
#[test]
fn the_three_loops_and_their_bindings_match_node() {
    matches_node(
        r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
const mk = () => new Map([[1, 'a'], ['k', { x: 1 }], [{ o: 1 }, [1, 2]], [NaN, undefined], [-0, 0]]);
for (let r = 0; r < 12; r++) {
  t(() => { const s = []; for (const [k, v] of mk()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of mk().entries()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [i, x] of ['a', , 'c', undefined].entries()) s.push(i, x); return s; });
  t(() => { const s = []; for (let [k, v] of mk()) { k = 1; v = 2; s.push(k + v); } return s; });
  t(() => { const s = []; for (var [k, v] of mk()) s.push(k, v); return [s, k, v]; });
  t(() => { let k, v; const s = []; for ([k, v] of mk()) s.push(k, v); return [s, k, v]; });
  t(() => { const s = []; for (const [k, v] of new Map()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of new Set([1, 2]).entries()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of new Map([[1, 2]]).keys()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of new Map([['ab', 2]]).keys()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of [[1, 2], [3, 4]]) s.push(k, v); return s; });
  t(() => { const fs = []; for (const [k, v] of new Map([[1, 2], [3, 4]])) fs.push(() => k + v); return fs.map(f => f()); });
  t(() => { const fs = []; for (const [i, x] of [5, 6].entries()) fs.push(() => i * x); return fs.map(f => f()); });
  t(() => { const o = {}; for ([o.a, o['b']] of new Map([[1, 2]])) ; return o; });
  t(() => { const s = []; for (const [k, v] of new Map([[1, 2], [3, 4]])) { const [a, b] = [v, k]; s.push(a, b); } return s; });
  t(() => { const s = []; for (const [k, v] of new Map([[1, 2], [3, 4]])) { s.push(k); continue; } return s; });
  t(() => { const s = []; outer: for (const [k] of new Map([[1, 2]])) for (const [a, b] of new Map([[3, 4], [5, 6]])) { if (a === 3) continue outer; s.push(a); } return s; });
}
out.map(show).join(' ')"####,
        r####""[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0]" "[0,\"a\",1,undefined,2,\"c\",3,undefined]" "[3,3,3,3,3]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[[1,\"a\",\"k\",{x:1},{o:1},[1,2],NaN,undefined,0,0],0,0]" "[]" "[1,1,2,2]" "TypeError" "[\"a\",\"b\"]" "[1,2,3,4]" "[3,7]" "[0,6]" "{a:1,b:2}" "[2,1,4,3]" "[1,3]" "[]""####,
    );
}

/// Patterns that are not two plain names keep the spec path, and so do iterables that are not entries.
#[test]
fn patterns_that_are_not_two_plain_names_match_node() {
    matches_node(
        r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
const mk = () => new Map([[1, 2], [3, 4]]);
for (let r = 0; r < 12; r++) {
  t(() => { const s = []; for (const [k] of mk()) s.push(k); return s; });
  t(() => { const s = []; for (const [k, v, w] of mk()) s.push(k, v, w); return s; });
  t(() => { const s = []; for (const [k, ...rest] of mk()) s.push(k, rest); return s; });
  t(() => { const s = []; for (const [k = 9, v] of new Map([[undefined, 2]])) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v = 9] of new Map([[1, undefined]])) s.push(k, v); return s; });
  t(() => { const s = []; for (const [[a], v] of new Map([[[7], 2]])) s.push(a, v); return s; });
  t(() => { const s = []; for (const [[a], v] of mk()) s.push(a, v); return s; });
  t(() => { const s = []; for (const [, v] of mk()) s.push(v); return s; });
  t(() => { const s = []; for (const [k, , w] of mk()) s.push(k, w); return s; });
  t(() => { const s = []; for (const { 0: k, 1: v, length } of mk()) s.push(k, v, length); return s; });
  t(() => { const s = []; for (const [k, { x }] of new Map([[1, { x: 5 }]])) s.push(k, x); return s; });
  t(() => { const s = []; for (const [k, [a, b]] of new Map([[1, [5, 6]]])) s.push(k, a, b); return s; });
  t(() => { const s = []; for (const [k, v] of mk()) { s.push([k, v].length); } return s; });
  t(() => { const s = []; for (const e of mk()) s.push(Array.isArray(e), e.length, Object.getPrototypeOf(e) === Array.prototype); return s; });
  t(() => { const s = []; for (const e of [7].entries()) { e.push(1); s.push(e); } return s; });
  t(() => { const s = []; for (const [k, v] of new Map([[1, 2]]).entries()) { s.push(k, v); } return s; });
  t(() => { const s = []; for (const [a, b] of 'ab') s.push(a, b); return s; });
  t(() => { const s = []; for (const [a, b] of [5]) s.push(a, b); return s; });
  t(() => { const s = []; for (const [a, b] of [null]) s.push(a, b); return s; });
  t(() => { const s = []; for (const [a, b] of [{}]) s.push(a, b); return s; });
  t(() => { const s = []; for (const [a, b] of [new Set([1, 2])]) s.push(a, b); return s; });
  t(() => { const s = []; for (const [a, a2] of [function* () { yield 1; yield 2; }()]) s.push(a, a2); return s; });
  t(() => { const s = []; for (const [a, b] of [[1]]) s.push(a, b); return s; });
  t(() => { const s = []; for (const [a, b] of [[1, 2, 3]]) s.push(a, b); return s; });
}
out.map(show).join(' ')"####,
        r####""[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]" "[1,3]" "[1,2,undefined,3,4,undefined]" "[1,[2],3,[4]]" "[9,2]" "[1,9]" "[7,2]" "TypeError" "[2,4]" "[1,undefined,3,undefined]" "[1,2,2,3,4,2]" "[1,5]" "[1,5,6]" "[2,2]" "[true,2,true,true,2,true]" "[[0,7,1]]" "[1,2]" "[\"a\",undefined,\"b\",undefined]" "TypeError" "TypeError" "TypeError" "[1,2]" "[1,2]" "[1,undefined]" "[1,2]""####,
    );
}

/// Entries added to and removed from a map, and arrays resized, while they are iterated; getters, proxies and
/// typed arrays under `entries()`.
#[test]
fn maps_and_arrays_changing_during_the_loop_match_node() {
    matches_node(
        r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
for (let r = 0; r < 12; r++) {
  t(() => { const m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]), s = []; for (const [k, v] of m) { s.push(k + v); if (k === 1) { m.delete(2); m.set(4, 'd'); } } return s; });
  t(() => { const m = new Map([[1, 'a']]), s = []; for (const [k, v] of m) { s.push(k, v); if (k < 5) m.set(k + 1, v + k); } return s; });
  t(() => { const m = new Map([[1, 'a'], [2, 'b']]), s = []; for (const [k, v] of m) { s.push(k, v); m.clear(); } return s; });
  t(() => { const m = new Map([[1, 'a'], [2, 'b']]), s = []; for (const [k, v] of m) { s.push(k, v); m.delete(k); m.set(k, v); if (s.length > 12) break; } return s; });
  t(() => { const m = new Map([[1, 'a'], [2, 'b']]), s = []; for (const [k, v] of m) { s.push(k, v); m.set(1, 'z'); } return s; });
  t(() => { const m = new Map([[1, 'a'], [2, 'b'], [3, 'c']]), s = []; for (const [k, v] of m.entries()) { s.push(v); m.delete(3); } return s; });
  t(() => { const a = [1, 2, 3], s = []; for (const [i, x] of a.entries()) { s.push(i, x); if (a.length < 6) a.push(x * 10); } return s; });
  t(() => { const a = [1, 2, 3, 4, 5], s = []; for (const [i, x] of a.entries()) { s.push(i, x); if (i === 1) a.length = 3; } return s; });
  t(() => { const a = [1, 2, 3, 4, 5], s = []; for (const [i, x] of a.entries()) { s.push(i, x); a.shift(); } return s; });
  t(() => { const a = [1, , 3], s = []; for (const [i, x] of a.entries()) s.push(i, x); return s; });
  t(() => { const a = [1, 2, 3]; Object.defineProperty(a, 1, { get() { return 'g'; } }); const s = []; for (const [i, x] of a.entries()) s.push(i, x); return s; });
  t(() => { const a = [1, 2, 3]; Object.defineProperty(a, 1, { get() { throw new RangeError('x'); } }); const s = []; for (const [i, x] of a.entries()) s.push(i, x); return s; });
  t(() => { const p = new Proxy([1, 2], { get(o, k, r) { return Reflect.get(o, k, r); } }); const s = []; for (const [i, x] of Array.prototype.entries.call(p)) s.push(i, x); return s; });
  t(() => { const s = []; for (const [i, x] of new Uint8Array([4, 5]).entries()) s.push(i, x); return s; });
  t(() => { const s = []; for (const [i, x] of Array.prototype.entries.call({ length: 2, 0: 'a', 1: 'b' })) s.push(i, x); return s; });
  t(() => { const s = []; for (const [i, x] of Array.prototype.entries.call('xy')) s.push(i, x); return s; });
  t(() => { const it = new Map([[1, 2], [3, 4], [5, 6]]).entries(); it.next(); const s = []; for (const [k, v] of it) s.push(k, v); return [s, it.next()]; });
  t(() => { const it = [7, 8, 9].entries(); it.next(); const s = []; for (const [k, v] of it) s.push(k, v); return [s, it.next()]; });
  t(() => { const m = new Map([[1, 2]]); const it = m.entries(); const s = []; for (const [k, v] of it) { s.push(k, v); break; } for (const [k, v] of it) s.push(k, v); return s; });
}
out.map(show).join(' ')"####,
        r####""[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]" "[\"1a\",\"3c\",\"4d\"]" "[1,\"a\",2,\"a1\",3,\"a12\",4,\"a123\",5,\"a1234\"]" "[1,\"a\"]" "[1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\",2,\"b\",1,\"a\"]" "[1,\"a\",2,\"b\"]" "[\"a\",\"b\"]" "[0,1,1,2,2,3,3,10,4,20,5,30]" "[0,1,1,2,2,3]" "[0,1,1,3,2,5]" "[0,1,1,undefined,2,3]" "[0,1,1,\"g\",2,3]" "RangeError" "[0,1,1,2]" "[0,4,1,5]" "[0,\"a\",1,\"b\"]" "[0,\"x\",1,\"y\"]" "[[3,4,5,6],{value:undefined,done:true}]" "[[1,8,2,9],{value:undefined,done:true}]" "[1,2]""####,
    );
}

/// `Array.prototype[Symbol.iterator]`, `%ArrayIteratorPrototype%.next` and `return` anywhere on the
/// iterator chain rewritten before the loop starts.
#[test]
fn a_rewritten_array_protocol_before_the_loop_matches_node() {
    matches_node(
        r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
const AIP = Object.getPrototypeOf([][Symbol.iterator]());
const IP = Object.getPrototypeOf(AIP);
const realNext = Object.getOwnPropertyDescriptor(AIP, 'next');
const realIter = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
const restore = () => {
  Object.defineProperty(AIP, 'next', realNext);
  Object.defineProperty(Array.prototype, Symbol.iterator, realIter);
  delete AIP.return; delete IP.return; delete Object.prototype.return; delete Array.prototype.next;
  Object.setPrototypeOf(AIP, IP);
  Object.setPrototypeOf(IP, Object.prototype);
};
const guard = (f) => () => { try { return f(); } finally { restore(); } };
const mk = () => new Map([[1, 2], [3, 4]]);
const run = () => { const s = []; for (const [k, v] of mk()) s.push(k, v); return s; };
const runA = () => { const s = []; for (const [k, v] of ['a', 'b'].entries()) s.push(k, v); return s; };
for (let r = 0; r < 12; r++) {
  t(guard(() => { Array.prototype[Symbol.iterator] = function* () { yield 'g1'; yield 'g2'; }; return [run(), runA()]; }));
  t(guard(() => { const log = []; Array.prototype[Symbol.iterator] = function () { log.push('iter'); return realIter.value.call(this); }; return [run(), runA(), log]; }));
  t(guard(() => { Object.defineProperty(Array.prototype, Symbol.iterator, { get() { return realIter.value; }, configurable: true }); return [run(), runA()]; }));
  t(guard(() => { Object.defineProperty(Array.prototype, Symbol.iterator, { get() { throw new RangeError('get'); }, configurable: true }); return [run(), runA()]; }));
  t(guard(() => { delete Array.prototype[Symbol.iterator]; return run(); }));
  t(guard(() => { Array.prototype[Symbol.iterator] = undefined; return runA(); }));
  t(guard(() => { const log = []; const n = realNext.value; AIP.next = function () { log.push('next'); return n.call(this); }; return [run(), runA(), log]; }));
  t(guard(() => { AIP.next = function () { return { done: true }; }; return [run(), runA()]; }));
  t(guard(() => { AIP.next = function () { const c = (this.c = (this.c || 0) + 1); return { done: c > 2, value: 'n' + c }; }; return [run(), runA()]; }));
  t(guard(() => { delete AIP.next; return run(); }));
  t(guard(() => { const log = []; AIP.return = function () { log.push('ret'); return {}; }; return [run(), runA(), log]; }));
  t(guard(() => { const log = []; IP.return = function () { log.push('iret'); return {}; }; return [run(), runA(), log]; }));
  t(guard(() => { const log = []; Object.prototype.return = function () { log.push('oret'); return {}; }; return [run(), runA(), log]; }));
  t(guard(() => { Object.defineProperty(AIP, 'return', { get() { throw new RangeError('rget'); }, configurable: true }); return run(); }));
  t(guard(() => { const log = []; Object.setPrototypeOf(AIP, { return() { log.push('protoret'); return {}; }, __proto__: IP }); return [run(), log]; }));
  t(guard(() => { Object.setPrototypeOf(AIP, new Proxy(IP, { get(o, k, r) { return Reflect.get(o, k, r); } })); return run(); }));
  t(guard(() => { const log = []; Object.setPrototypeOf(IP, new Proxy(Object.prototype, { get(o, k, r) { log.push(String(k)); return Reflect.get(o, k, r); } })); return [run(), log]; }));
  t(guard(() => { Array.prototype.next = 5; return run(); }));
  t(guard(() => { Object.defineProperty(Array.prototype, 0, { get() { throw new Error('x'); }, configurable: true }); try { return run(); } finally { delete Array.prototype[0]; } }));
  t(guard(() => { Array.prototype.length = 0; return run(); }));
  t(() => run());
}
out.map(show).join(' ')"####,
        r####""TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iter\",\"iter\",\"iter\",\"iter\",\"iter\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"]]" "RangeError" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\",\"next\"]]" "[[],[]]" "TypeError" "TypeError" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"iret\",\"iret\",\"iret\",\"iret\"]]" "[[1,2,3,4],[0,\"a\",1,\"b\"],[\"oret\",\"oret\",\"oret\",\"oret\"]]" "RangeError" "[[1,2,3,4],[\"protoret\",\"protoret\"]]" "[1,2,3,4]" "[[1,2,3,4],[\"return\",\"return\"]]" "[1,2,3,4]" "TypeError" "[1,2,3,4]" "[1,2,3,4]""####,
    );
}

/// The same protocol rewritten between two elements, or by the getter of the element being read.
#[test]
fn a_rewritten_array_protocol_inside_the_loop_matches_node() {
    matches_node(
        r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
const AIP = Object.getPrototypeOf([][Symbol.iterator]());
const IP = Object.getPrototypeOf(AIP);
const realNext = Object.getOwnPropertyDescriptor(AIP, 'next');
const realIter = Object.getOwnPropertyDescriptor(Array.prototype, Symbol.iterator);
const restore = () => {
  Object.defineProperty(AIP, 'next', realNext);
  Object.defineProperty(Array.prototype, Symbol.iterator, realIter);
  delete AIP.return; delete IP.return; delete Object.prototype.return;
  Object.setPrototypeOf(AIP, IP);
};
const guard = (f) => () => { try { return f(); } finally { restore(); } };
const big = () => new Map([[1, 2], [3, 4], [5, 6], [7, 8]]);
for (let r = 0; r < 12; r++) {
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) Array.prototype[Symbol.iterator] = function* () { yield 'g1'; yield 'g2'; }; } return s; }));
  t(guard(() => { const s = [], log = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) Array.prototype[Symbol.iterator] = function () { log.push('iter'); return realIter.value.call(this); }; } return [s, log]; }));
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) AIP.next = function () { return { done: true }; }; } return s; }));
  t(guard(() => { const s = [], log = []; const n = realNext.value; for (const [k, v] of big()) { s.push(k, v); if (k === 3) AIP.next = function () { log.push('n'); return n.call(this); }; } return [s, log]; }));
  t(guard(() => { const s = [], log = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) AIP.return = function () { log.push('ret'); return {}; }; } return [s, log]; }));
  t(guard(() => { const s = [], log = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) Object.prototype.return = function () { log.push('oret'); return {}; }; } return [s, log]; }));
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) Object.defineProperty(Array.prototype, Symbol.iterator, { get() { throw new RangeError('g'); }, configurable: true }); } return s; }));
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) delete Array.prototype[Symbol.iterator]; } return s; }));
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) Object.setPrototypeOf(AIP, null); } return s; }));
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) { Array.prototype[Symbol.iterator] = function* () { yield 'g'; }; } if (k === 5) restore(); } return s; }));
  t(guard(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 1) { Array.prototype[Symbol.iterator] = function* () { yield 'g'; }; restore(); } } return s; }));
  // array entries: the getter of an element patches the protocol while the step is made.
  t(guard(() => { const a = [1, 2, 3]; Object.defineProperty(a, 1, { get() { Array.prototype[Symbol.iterator] = function* () { yield 'g1'; yield 'g2'; }; return 'got'; }, configurable: true }); const s = []; for (const [i, x] of a.entries()) s.push(i, x); return s; }));
  t(guard(() => { const a = [1, 2, 3]; Object.defineProperty(a, 1, { get() { AIP.next = function () { return { done: true }; }; return 'got'; }, configurable: true }); const s = []; for (const [i, x] of a.entries()) s.push(i, x); return s; }));
  t(guard(() => { const a = [1, 2, 3]; Object.defineProperty(a, 2, { get() { AIP.return = function () { throw new RangeError('ret'); }; return 'got'; }, configurable: true }); const s = []; for (const [i, x] of a.entries()) s.push(i, x); return s; }));
  t(guard(() => { const a = ['a', 'b', 'c'], s = []; for (const [i, x] of a.entries()) { s.push(i, x); if (i === 0) AIP.next = function () { return { done: true }; }; } return s; }));
  t(guard(() => { const a = ['a', 'b', 'c'], s = [], log = []; for (const [i, x] of a.entries()) { s.push(i, x); if (i === 0) Array.prototype[Symbol.iterator] = function () { log.push('iter'); return realIter.value.call(this); }; } return [s, log]; }));
  // the map iterator's own next and the iterator it was handed
  t(guard(() => { const MIP = Object.getPrototypeOf(new Map()[Symbol.iterator]()); const real = MIP.next; try { const s = []; for (const [k, v] of big()) { s.push(k, v); if (k === 3) MIP.next = function () { return { done: true }; }; } return s; } finally { MIP.next = real; } }));
  t(guard(() => { const MIP = Object.getPrototypeOf(new Map()[Symbol.iterator]()); const real = MIP.next; try { MIP.next = function () { const r = real.call(this); if (!r.done) r.value = [r.value[1], r.value[0]]; return r; }; const s = []; for (const [k, v] of big()) s.push(k, v); return s; } finally { MIP.next = real; } }));
  t(guard(() => { const m = big(); const it = m[Symbol.iterator](); it.next = function () { return { done: true }; }; const s = []; for (const [k, v] of { [Symbol.iterator]: () => it }) s.push(k, v); return s; }));
  t(guard(() => { const orig = Map.prototype[Symbol.iterator]; const s = []; try { Map.prototype[Symbol.iterator] = function () { return [[9, 8]][Symbol.iterator](); }; for (const [k, v] of big()) s.push(k, v); } finally { Map.prototype[Symbol.iterator] = orig; } return s; }));
  // a setter that runs between the two elements of an assignment pattern rewrites the protocol
  t(guard(() => { const log = []; Object.defineProperty(globalThis, 'ga', { set(v) { log.push('set' + v); AIP.next = function () { return { done: true }; }; }, configurable: true }); let gb = 'unset'; try { for ([ga, gb] of big()) log.push(gb); } finally { delete globalThis.ga; } return [log, gb]; }));
  t(guard(() => { const log = []; Object.defineProperty(globalThis, 'ga', { set(v) { log.push('set' + v); Array.prototype[Symbol.iterator] = function* () { yield 'g1'; yield 'g2'; }; }, configurable: true }); let gb = 'unset'; try { for ([ga, gb] of big()) log.push(gb); } finally { delete globalThis.ga; } return [log, gb]; }));
  t(guard(() => { const log = []; const o = { set a(v) { log.push('set' + v); AIP.return = function () { log.push('ret'); return {}; }; } }; let b = 'unset'; for ([o.a, b] of big()) { log.push(b); } return [log, b]; }));
}
out.map(show).join(' ')"####,
        r####""[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]" "[1,2,3,4,\"g1\",\"g2\",\"g1\",\"g2\"]" "[[1,2,3,4,5,6,7,8],[\"iter\",\"iter\"]]" "[1,2,3,4,undefined,undefined,undefined,undefined]" "[[1,2,3,4,5,6,7,8],[\"n\",\"n\",\"n\",\"n\"]]" "[[1,2,3,4,5,6,7,8],[\"ret\",\"ret\"]]" "[[1,2,3,4,5,6,7,8],[\"oret\",\"oret\"]]" "RangeError" "TypeError" "[1,2,3,4,5,6,7,8]" "[1,2,3,4,\"g\",undefined,\"g\",undefined]" "[1,2,3,4,5,6,7,8]" "[0,1,\"g1\",\"g2\",\"g1\",\"g2\"]" "[0,1,undefined,undefined,undefined,undefined]" "RangeError" "[0,\"a\",undefined,undefined,undefined,undefined]" "[[0,\"a\",1,\"b\",2,\"c\"],[\"iter\",\"iter\"]]" "[1,2,3,4,5,6,7,8]" "[2,1,4,3,6,5,8,7]" "[]" "[9,8]" "[[\"set1\",2,\"setundefined\",undefined,\"setundefined\",undefined,\"setundefined\",undefined],undefined]" "[[\"set1\",2,\"setg1\",\"g2\",\"setg1\",\"g2\",\"setg1\",\"g2\"],\"g2\"]" "[[\"set1\",\"ret\",2,\"set3\",\"ret\",4,\"set5\",\"ret\",6,\"set7\",\"ret\",8],8]""####,
    );
}

/// `Map` subclasses with their own iteration, and `break`, `return`, `throw` and `continue` closing the
/// iterator.
#[test]
fn map_subclasses_and_closing_match_node() {
    matches_node(
        r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name + ':' + (e.message === 'boom' ? e.message : '')); } };
class M extends Map {}
class N extends Map { *[Symbol.iterator]() { yield [100, 200]; yield* super[Symbol.iterator](); } }
class E extends Map { entries() { return [[7, 8]][Symbol.iterator](); } }
class S extends Map { get size() { return 99; } }
const MIP = Object.getPrototypeOf(new Map().entries());
const retLog = [];
const withReturn = (f) => () => { MIP.return = function () { retLog.push('ret'); return {}; }; try { return f(); } finally { delete MIP.return; } };
const big = (C = Map) => new C([[1, 2], [3, 4], [5, 6]]);
for (let r = 0; r < 12; r++) {
  t(() => { const s = []; for (const [k, v] of big(M)) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of big(N)) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of big(E)) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of big(E).entries()) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of big(S)) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of Map.prototype.entries.call(big(M))) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of Object.assign(big(), { [Symbol.iterator]: Map.prototype.entries })) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of Object.assign(big(), { next() { return { done: true }; }, [Symbol.iterator]() { return this; } })) s.push(k, v); return s; });
  t(() => { const s = []; for (const [k, v] of Object.assign(big().entries(), { next() { return { done: true }; } })) s.push(k, v); return s; });
  // closing
  retLog.length = 0;
  t(withReturn(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); break; } return [s, retLog.slice()]; }));
  t(withReturn(() => { const s = []; for (const [k, v] of big()) { s.push(k, v); } return [s, retLog.slice()]; }));
  t(withReturn(() => { const f = () => { for (const [k, v] of big()) return k + v; }; return [f(), retLog.slice()]; }));
  t(withReturn(() => { const s = []; try { for (const [k, v] of big()) { s.push(k); throw new Error('boom'); } } catch (e) { s.push(e.message); } return [s, retLog.slice()]; }));
  t(withReturn(() => { const s = []; a: for (const x of [1, 2]) for (const [k, v] of big()) { s.push(k); continue a; } return [s, retLog.slice()]; }));
  t(withReturn(() => { const s = []; a: for (const x of [1, 2]) for (const [k, v] of big()) { s.push(k); break a; } return [s, retLog.slice()]; }));
  t(withReturn(() => { const s = []; for (const [k, v] of big()) { s.push(k); continue; } return [s, retLog.slice()]; }));
  t(() => { MIP.return = function () { retLog.push('rthrow'); throw new RangeError('r'); }; try { for (const [k, v] of big()) break; } finally { delete MIP.return; } });
  t(() => { MIP.return = function () { throw new RangeError('r'); }; try { for (const [k, v] of big()) throw new TypeError('boom'); } finally { delete MIP.return; } });
  t(() => { MIP.return = function () { return 5; }; try { for (const [k, v] of big()) break; } finally { delete MIP.return; } });
  t(() => { const s = []; for (const [k, v] of big()) { try { s.push(k); if (k === 3) throw 1; } catch (e) { s.push('c'); } finally { s.push('f'); } } return s; });
  t(() => { const s = []; const f = function* () { for (const [k, v] of big()) { const x = yield k; s.push(x, v); } }; const g = f(); g.next(); g.next('a'); g.next('b'); g.return('z'); return [s, g.next()]; });
  t(() => { const s = []; const f = function* () { for (const [k, v] of big()) yield [k, v]; }; for (const [a, b] of f()) s.push(a, b); return s; });
}
out.map(show).join(' ')"####,
        r####""[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[100,200,1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[7,8]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[1,2,3,4,5,6]" "[]" "[]" "[[1,2],[\"ret\"]]" "[[1,2,3,4,5,6],[\"ret\"]]" "[3,[\"ret\",\"ret\"]]" "[[1,\"boom\"],[\"ret\",\"ret\",\"ret\"]]" "[[1,1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "[[1,3,5],[\"ret\",\"ret\",\"ret\",\"ret\",\"ret\",\"ret\"]]" "RangeError:" "TypeError:boom" "TypeError:" "[1,\"f\",3,\"c\",\"f\",5,\"f\"]" "[[\"a\",2,\"b\",4],{value:undefined,done:true}]" "[1,2,3,4,5,6]""####,
    );
}

/// A collection inside the loop: the key and the value of an entry that was deleted from the map are
/// held by nothing but the loop, in the registers or, on the spec path, in the entry array.
#[test]
fn a_collection_inside_the_loop_keeps_the_entry_alive() {
    let source = r####"const out = [];
const t = (f) => { try { out.push(show(f())); } catch (e) { out.push(e.name); } };
for (let r = 0; r < 6; r++) {
  // the key and the value are held only by the loop once the entry is deleted from the map
  t(() => {
    const m = new Map(); for (let i = 0; i < 12; i++) m.set({ id: i }, [i, { deep: i }]);
    const s = [];
    for (const [k, v] of m) {
      m.delete(k);
      gc();
      const junk = []; for (let j = 0; j < 50; j++) junk.push({ j, a: [j] });
      s.push(k.id + ':' + v[1].deep);
    }
    return s;
  });
  t(() => {
    const a = []; for (let i = 0; i < 12; i++) a.push({ id: i });
    const s = [];
    for (const [i, o] of a.entries()) {
      a[i] = null;
      gc();
      s.push(o.id);
    }
    return s;
  });
  // the fallback path holds the entry array across a collection too
  t(() => {
    const m = new Map([[[{ a: 1 }], { b: 2 }], [[{ a: 3 }], { b: 4 }]]);
    const s = [];
    for (const [[x], v] of m) { gc(); s.push(x, v); }
    return s;
  });
  t(() => {
    const m = new Map([[{ a: 1 }, { b: 2 }], [{ a: 3 }, { b: 4 }]]);
    const s = [];
    for (const [k, v] of m) { m.clear(); gc(); s.push(k.a, v.b); }
    return s;
  });
  t(() => {
    const m = new Map([[{ a: 1 }, { b: 2 }], [{ a: 3 }, { b: 4 }]]);
    const s = [];
    const it = m.entries();
    for (const [k, v] of it) { m.clear(); gc(); s.push(k.a, v.b); }
    return [s, it.next()];
  });
}
out.map(show).join(' ')"####;
    let node = r####""[\"0:0\",\"1:1\",\"2:2\",\"3:3\",\"4:4\",\"5:5\",\"6:6\",\"7:7\",\"8:8\",\"9:9\",\"10:10\",\"11:11\"]" "[0,1,2,3,4,5,6,7,8,9,10,11]" "[{a:1},{b:2},{a:3},{b:4}]" "[1,2]" "[[1,2],{value:undefined,done:true}]" "[\"0:0\",\"1:1\",\"2:2\",\"3:3\",\"4:4\",\"5:5\",\"6:6\",\"7:7\",\"8:8\",\"9:9\",\"10:10\",\"11:11\"]" "[0,1,2,3,4,5,6,7,8,9,10,11]" "[{a:1},{b:2},{a:3},{b:4}]" "[1,2]" "[[1,2],{value:undefined,done:true}]" "[\"0:0\",\"1:1\",\"2:2\",\"3:3\",\"4:4\",\"5:5\",\"6:6\",\"7:7\",\"8:8\",\"9:9\",\"10:10\",\"11:11\"]" "[0,1,2,3,4,5,6,7,8,9,10,11]" "[{a:1},{b:2},{a:3},{b:4}]" "[1,2]" "[[1,2],{value:undefined,done:true}]" "[\"0:0\",\"1:1\",\"2:2\",\"3:3\",\"4:4\",\"5:5\",\"6:6\",\"7:7\",\"8:8\",\"9:9\",\"10:10\",\"11:11\"]" "[0,1,2,3,4,5,6,7,8,9,10,11]" "[{a:1},{b:2},{a:3},{b:4}]" "[1,2]" "[[1,2],{value:undefined,done:true}]" "[\"0:0\",\"1:1\",\"2:2\",\"3:3\",\"4:4\",\"5:5\",\"6:6\",\"7:7\",\"8:8\",\"9:9\",\"10:10\",\"11:11\"]" "[0,1,2,3,4,5,6,7,8,9,10,11]" "[{a:1},{b:2},{a:3},{b:4}]" "[1,2]" "[[1,2],{value:undefined,done:true}]" "[\"0:0\",\"1:1\",\"2:2\",\"3:3\",\"4:4\",\"5:5\",\"6:6\",\"7:7\",\"8:8\",\"9:9\",\"10:10\",\"11:11\"]" "[0,1,2,3,4,5,6,7,8,9,10,11]" "[{a:1},{b:2},{a:3},{b:4}]" "[1,2]" "[[1,2],{value:undefined,done:true}]""####;
    for mode in [Mode::Interpreted, Mode::Eager, Mode::Hot] {
        let mut context = context(mode);
        context
            .register_global_callable(
                js_string!("gc"),
                0,
                NativeFunction::from_fn_ptr(|_, _, _| {
                    boa_gc::force_collect();
                    Ok(JsValue::undefined())
                }),
            )
            .expect("registers");
        let result = match context.eval(Source::from_bytes(&format!("{SHOW}\n{source}"))) {
            Ok(value) => value.as_string().map_or_else(
                || value.display().to_string(),
                |s| s.to_std_string_escaped(),
            ),
            Err(error) => format!("uncaught {error}"),
        };
        assert_eq!(result, node, "{mode:?}");
    }
}
