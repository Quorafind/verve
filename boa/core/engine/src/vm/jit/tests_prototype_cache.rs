//! The inline caches for a property of a prototype: every way the prototype can change under a
//! cached read or write, in the interpreter and in compiled code. The three modes must agree,
//! and agree with Node's output for the same program.

use super::{tests::Mode, tests_typed_length::run_with_natives};

/// Each scenario has an access function of its own, so its inline cache starts empty; a hot loop
/// calls it, and one change is made in the middle of the loop. A result is shown once for each
/// run of equal results.
const SCENARIOS: &str = r##"const out = [];
const t = (name, f) => { try { out.push(name + ': ' + show(f())); } catch (e) { out.push(name + ': threw ' + e.name); } };
// A hot loop around `read`, `mutate` called once in the middle; the distinct consecutive results.
const trace = (read, mutate, n = 24, k = 12) => {
  const r = [];
  for (let i = 0; i < n; i++) {
    if (i === k) mutate();
    let v;
    try { v = show(read()); } catch (e) { v = 'threw ' + e.name; }
    if (r.length === 0 || r[r.length - 1] !== v) r.push(v);
  }
  return r;
};
const getter = (v) => ({ get() { return v; }, configurable: true });
const big = (o, n = 1100) => { for (let i = 0; i < n; i++) o['k' + i] = i; return o; };

t('1 data to accessor', () => {
  function A() {} A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { delete A.prototype.x; Object.defineProperty(A.prototype, 'x', getter('g')); });
});
t('2 delete shrinks storage', () => {
  function B() {} B.prototype.p = 1; B.prototype.q = 2; B.prototype.x = 'd';
  const b = new B(); const g = (o) => o.x;
  return trace(() => g(b), () => { delete B.prototype.p; delete B.prototype.q; delete B.prototype.x; });
});
t('3 delete earlier property', () => {
  function C() {} C.prototype.p = 1; C.prototype.x = 'd'; C.prototype.q = 3;
  const c = new C(); const g = (o) => o.x;
  return trace(() => g(c), () => { delete C.prototype.p; C.prototype.p = 9; });
});
t('4 accessor to data', () => {
  function A() {} Object.defineProperty(A.prototype, 'x', getter('g'));
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { Object.defineProperty(A.prototype, 'x', { value: 'd', writable: true, configurable: true }); });
});
t('5 value change keeps hitting', () => {
  function A() {} A.prototype.x = 1;
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { A.prototype.x = 2; });
});
t('6 getter replaced', () => {
  function A() {} Object.defineProperty(A.prototype, 'x', getter('g1'));
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { Object.defineProperty(A.prototype, 'x', getter('g2')); });
});
t('7 shadow a farther property', () => {
  function A() {} function B() {} B.prototype.x = 'far'; A.prototype = Object.create(B.prototype);
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { A.prototype.x = 'near'; });
});
t('8 shadow then unshadow', () => {
  function A() {} function B() {} B.prototype.x = 'far'; A.prototype = Object.create(B.prototype);
  A.prototype.x = 'near';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { delete A.prototype.x; });
});
t('9 own property shadows', () => {
  function A() {} A.prototype.x = 'p';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { a.x = 'own'; });
});
t('10 setPrototypeOf receiver', () => {
  function A() {} A.prototype.x = 'a'; const other = { x: 'o' };
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { Object.setPrototypeOf(a, other); });
});
t('11 setPrototypeOf the prototype', () => {
  function A() {} A.prototype.x = 'a'; const other = { x: 'o' };
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { Object.setPrototypeOf(A.prototype, other); delete A.prototype.x; });
});
t('12 farther prototype changes', () => {
  function A() {} function B() {} B.prototype.x = 'far'; A.prototype = Object.create(B.prototype);
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { delete B.prototype.x; Object.defineProperty(B.prototype, 'x', getter('g')); });
});
t('13 prototype becomes a dictionary', () => {
  function A() {} A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { big(A.prototype); delete A.prototype.x; Object.defineProperty(A.prototype, 'x', getter('g')); });
});
t('14 dictionary prototype, delete', () => {
  function A() {} big(A.prototype); A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { delete A.prototype.k3; delete A.prototype.x; Object.defineProperty(A.prototype, 'x', getter('g')); });
});
t('15 dictionary prototype, data to accessor', () => {
  function A() {} big(A.prototype); A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { Object.defineProperty(A.prototype, 'x', getter('g')); });
});
t('16 dictionary prototype, shrinking', () => {
  function A() {} big(A.prototype); A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => { for (let i = 0; i < 1100; i++) delete A.prototype['k' + i]; delete A.prototype.x; });
});
t('17 dictionary receiver, own shadow', () => {
  const p = { x: 'p' }; const a = big(Object.create(p));
  const f = (o) => o.x;
  return trace(() => f(a), () => { a.x = 'own'; });
});
t('18 dictionary receiver, delete from prototype', () => {
  const p = { x: 'p' }; const a = big(Object.create(p));
  const f = (o) => o.x;
  return trace(() => f(a), () => { delete p.x; });
});
t('19 setPrototypeOf dictionary receiver', () => {
  const p = { x: 'p' }; const a = big(Object.create(p));
  const f = (o) => o.x;
  return trace(() => f(a), () => { Object.setPrototypeOf(a, { x: 'q' }); });
});
t('20 Array.prototype property', () => {
  const arr = [1]; const f = (o) => o.zz;
  Array.prototype.zz = 'a';
  try { return trace(() => f(arr), () => { delete Array.prototype.zz; Object.defineProperty(Array.prototype, 'zz', getter('g')); }); }
  finally { delete Array.prototype.zz; }
});
t('21 Array.prototype method replaced', () => {
  const arr = [1, 2]; const f = (o) => o.at(0);
  const original = Array.prototype.at;
  try { return trace(() => f(arr), () => { Array.prototype.at = function () { return 'mine'; }; }); }
  finally { Array.prototype.at = original; }
});
t('22 Array.prototype method deleted', () => {
  const arr = [1, 2]; const f = (o) => o.findLast;
  const original = Array.prototype.findLast;
  try { return trace(() => typeof f(arr), () => { delete Array.prototype.findLast; }); }
  finally { Array.prototype.findLast = original; }
});
t('23 Map.prototype accessor', () => {
  const m = new Map([[1, 2]]); const f = (o) => o.size;
  const original = Object.getOwnPropertyDescriptor(Map.prototype, 'size');
  try { return trace(() => f(m), () => { Object.defineProperty(Map.prototype, 'size', { get() { return 'fake'; }, configurable: true }); }); }
  finally { Object.defineProperty(Map.prototype, 'size', original); }
});
t('24 Map.prototype data to accessor', () => {
  const m = new Map(); const f = (o) => o.forEach;
  const original = Object.getOwnPropertyDescriptor(Map.prototype, 'forEach');
  try { return trace(() => typeof f(m), () => { delete Map.prototype.forEach; Object.defineProperty(Map.prototype, 'forEach', getter(7)); }); }
  finally { Object.defineProperty(Map.prototype, 'forEach', original); }
});
t('25 Object.prototype property seen through Array.prototype', () => {
  const f = (o) => o.isPrototypeOf;
  const r = trace(() => typeof f(Array.prototype), () => { Array.prototype.isPrototypeOf = 5; }, 24, 12);
  delete Array.prototype.isPrototypeOf;
  return r;
});
t('26 Object.prototype property deleted', () => {
  Object.prototype.qq = 'q'; const o = {}; const f = (x) => x.qq;
  try { return trace(() => f(o), () => { delete Object.prototype.qq; }); }
  finally { delete Object.prototype.qq; }
});
t('27 Object.prototype accessor', () => {
  Object.prototype.qq = 'q'; const o = {}; const f = (x) => x.qq;
  try { return trace(() => f(o), () => { delete Object.prototype.qq; Object.defineProperty(Object.prototype, 'qq', getter('g')); }); }
  finally { delete Object.prototype.qq; }
});
t('28 global name through Object.prototype', () => {
  Object.prototype.gg = 'o';
  const f = () => gg;
  try { return trace(f, () => { globalThis.gg = 'g'; }); }
  finally { delete Object.prototype.gg; delete globalThis.gg; }
});
t('29 setter loses its setter', () => {
  function A() {} const log = [];
  Object.defineProperty(A.prototype, 'x', { get() { return 'g'; }, set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return log.length; }, () => { Object.defineProperty(A.prototype, 'x', { get() { return 'g'; }, set: undefined }); });
  return [log.length, a.hasOwnProperty('x')];
});
t('30 setter removed', () => {
  function A() {} const log = [];
  Object.defineProperty(A.prototype, 'x', { set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return log.length; }, () => { delete A.prototype.x; });
  return [log.length, Object.keys(a).join()];
});
t('31 setter replaced', () => {
  function A() {} const log = [];
  Object.defineProperty(A.prototype, 'x', { set(v) { log.push('one' + v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return 0; }, () => { Object.defineProperty(A.prototype, 'x', { set(v) { log.push('two' + v); }, configurable: true }); });
  return [log.length, log[0], log[log.length - 1]];
});
t('32 setter becomes data', () => {
  function A() {} const log = [];
  Object.defineProperty(A.prototype, 'x', { set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return 0; }, () => { delete A.prototype.x; A.prototype.x = 'data'; });
  return [log.length, Object.keys(a).join(), A.prototype.x];
});
t('33 setter becomes read-only data', () => {
  function A() {} const log = [];
  Object.defineProperty(A.prototype, 'x', { set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return 0; }, () => { Object.defineProperty(A.prototype, 'x', { value: 1, writable: false }); });
  return [log.length, Object.keys(a).join()];
});
t('34 dictionary prototype setter removed', () => {
  function A() {} const log = []; big(A.prototype);
  Object.defineProperty(A.prototype, 'x', { set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return 0; }, () => { delete A.prototype.x; });
  return [log.length, Object.keys(a).join()];
});
t('35 dictionary prototype loses its setter', () => {
  function A() {} const log = []; big(A.prototype);
  Object.defineProperty(A.prototype, 'x', { get() { return 1; }, set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { o.x = v; };
  let i = 0;
  trace(() => { w(a, i++); return 0; }, () => { Object.defineProperty(A.prototype, 'x', { set: undefined }); });
  return [log.length, Object.keys(a).join()];
});
t('36 Array.prototype setter removed', () => {
  const log = []; const arr = [];
  Object.defineProperty(Array.prototype, 'ss', { set(v) { log.push(v); }, configurable: true });
  const w = (o, v) => { o.ss = v; };
  let i = 0;
  try { trace(() => { w(arr, i++); return 0; }, () => { delete Array.prototype.ss; }); return [log.length, Object.keys(arr).join()]; }
  finally { delete Array.prototype.ss; }
});
t('37 Map.prototype setter replaced', () => {
  const log = []; const m = new Map();
  Object.defineProperty(Map.prototype, 'ss', { set(v) { log.push(v); }, configurable: true });
  const w = (o, v) => { o.ss = v; };
  let i = 0;
  try { trace(() => { w(m, i++); return 0; }, () => { Object.defineProperty(Map.prototype, 'ss', { set(v) { log.push('x' + v); }, configurable: true }); }); return [log.length, log[log.length - 1]]; }
  finally { delete Map.prototype.ss; }
});
t('38 getter that rewrites its own prototype', () => {
  function A() {} let a;
  Object.defineProperty(A.prototype, 'x', { get() { delete A.prototype.x; A.prototype.p = 1; A.prototype.x = 'back'; return 'g'; }, configurable: true });
  a = new A(); const f = (o) => o.x;
  return trace(() => f(a), () => {});
});
t('39 garbage collection in the loop', () => {
  function A() {} A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => { gc(); return f(a); }, () => { delete A.prototype.x; Object.defineProperty(A.prototype, 'x', getter('g')); });
});
t('40 garbage collection, prototype grows', () => {
  function A() {} A.prototype.x = 'd';
  const a = new A(); const f = (o) => o.x;
  return trace(() => { gc(); return f(a); }, () => { A.prototype.y = 1; });
});
t('41 instances of fresh classes', () => {
  const f = (o) => o.x; const r = [];
  for (let i = 0; i < 30; i++) {
    class K { } K.prototype.x = i; if (i % 3 === 0) K.prototype.p = 0;
    if (i % 5 === 0) { delete K.prototype.p; }
    r.push(f(new K));
  }
  return r.join();
});
t('42 polymorphic receivers, one prototype changes', () => {
  function A() {} function B() {} A.prototype.x = 'a'; B.prototype.x = 'b'; B.prototype.y = 0;
  const a = new A(), b = new B(); const f = (o) => o.x; const r = [];
  for (let i = 0; i < 24; i++) {
    if (i === 12) { delete B.prototype.y; delete B.prototype.x; Object.defineProperty(B.prototype, 'x', getter('gb')); }
    r.push(show(f(a)) + show(f(b)));
  }
  return [...new Set(r)];
});
t('43 own property turns read-only on a dictionary object', () => {
  const o = big({}); o.x = 1; const w = (p, v) => { p.x = v; };
  let i = 0;
  return trace(() => { w(o, i++); return o.x; }, () => { Object.defineProperty(o, 'x', { writable: false }); });
});
t('44 prototype property made read-only', () => {
  function A() {} A.prototype.x = 1; const a = new A(); const w = (o, v) => { o.x = v; };
  trace(() => { w(a, 2); return 0; }, () => { Object.defineProperty(A.prototype, 'x', { writable: false }); delete a.x; });
  return [a.x, Object.keys(a).join()];
});
t('45 strict write, setter lost', () => {
  function A() {} const log = [];
  Object.defineProperty(A.prototype, 'x', { get() { return 'g'; }, set(v) { log.push(v); }, configurable: true });
  const a = new A(); const w = (o, v) => { 'use strict'; o.x = v; };
  let i = 0;
  return trace(() => { w(a, i++); return log.length; }, () => { Object.defineProperty(A.prototype, 'x', { set: undefined }); });
});
out.join('\n');"##;

/// What Node 22 prints for the scenarios.
const EXPECTED: &str = r##"1 data to accessor: ["\"d\"","\"g\""]
2 delete shrinks storage: ["\"d\"","undefined"]
3 delete earlier property: ["\"d\""]
4 accessor to data: ["\"g\"","\"d\""]
5 value change keeps hitting: ["1","2"]
6 getter replaced: ["\"g1\"","\"g2\""]
7 shadow a farther property: ["\"far\"","\"near\""]
8 shadow then unshadow: ["\"near\"","\"far\""]
9 own property shadows: ["\"p\"","\"own\""]
10 setPrototypeOf receiver: ["\"a\"","\"o\""]
11 setPrototypeOf the prototype: ["\"a\"","\"o\""]
12 farther prototype changes: ["\"far\"","\"g\""]
13 prototype becomes a dictionary: ["\"d\"","\"g\""]
14 dictionary prototype, delete: ["\"d\"","\"g\""]
15 dictionary prototype, data to accessor: ["\"d\"","\"g\""]
16 dictionary prototype, shrinking: ["\"d\"","undefined"]
17 dictionary receiver, own shadow: ["\"p\"","\"own\""]
18 dictionary receiver, delete from prototype: ["\"p\"","undefined"]
19 setPrototypeOf dictionary receiver: ["\"p\"","\"q\""]
20 Array.prototype property: ["\"a\"","\"g\""]
21 Array.prototype method replaced: ["1","\"mine\""]
22 Array.prototype method deleted: ["\"function\"","\"undefined\""]
23 Map.prototype accessor: ["1","\"fake\""]
24 Map.prototype data to accessor: ["\"function\"","\"number\""]
25 Object.prototype property seen through Array.prototype: ["\"function\"","\"number\""]
26 Object.prototype property deleted: ["\"q\"","undefined"]
27 Object.prototype accessor: ["\"q\"","\"g\""]
28 global name through Object.prototype: ["\"o\"","\"g\""]
29 setter loses its setter: [12,false]
30 setter removed: [12,"x"]
31 setter replaced: [24,"one0","two23"]
32 setter becomes data: [12,"x","data"]
33 setter becomes read-only data: [12,""]
34 dictionary prototype setter removed: [12,"x"]
35 dictionary prototype loses its setter: [12,""]
36 Array.prototype setter removed: [12,"ss"]
37 Map.prototype setter replaced: [24,"x23"]
38 getter that rewrites its own prototype: ["\"g\"","\"back\""]
39 garbage collection in the loop: ["\"d\"","\"g\""]
40 garbage collection, prototype grows: ["\"d\""]
41 instances of fresh classes: "0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29"
42 polymorphic receivers, one prototype changes: ["\"a\"\"b\"","\"a\"\"gb\""]
43 own property turns read-only on a dictionary object: ["0","1","2","3","4","5","6","7","8","9","10","11"]
44 prototype property made read-only: [1,""]
45 strict write, setter lost: ["1","2","3","4","5","6","7","8","9","10","11","12","threw TypeError"]
"##;

#[test]
fn a_changed_prototype_is_noticed_by_the_cache() {
    let interpreted = run_with_natives(SCENARIOS, Mode::Interpreted);
    for mode in [Mode::Eager, Mode::Hot] {
        assert_eq!(
            run_with_natives(SCENARIOS, mode),
            interpreted,
            "{mode:?} differs from the interpreter"
        );
    }
    assert_eq!(interpreted.lines().count(), EXPECTED.lines().count());
    for (line, expected) in interpreted.lines().zip(EXPECTED.lines()) {
        assert_eq!(line, expected);
    }
}
