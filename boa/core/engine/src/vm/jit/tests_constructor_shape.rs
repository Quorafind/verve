//! The inline cache for a property that an assignment creates (`vm/inline_cache/add.rs`), seen
//! from the constructors that use it: every way the objects a constructor makes can differ from
//! one construction to the next, and every way what the cache assumed can change under it. The
//! three modes must agree, and agree with Node's output for the same program (run with
//! `--expose-gc`, `gc` being the native of the same name).

use super::{tests::Mode, tests_typed_length::run_with_natives};

/// Each scenario builds objects in a loop of its own, so its assignment sites start with empty
/// caches, and prints the distinct descriptions (own keys, own names, JSON and the prototype) of
/// what the loop made, with whatever else the scenario observes.
const SCENARIOS: &str = r####"const out = [];
const protoName = (o) => { const p = Object.getPrototypeOf(o); return p === null ? 'null' : p === Object.prototype ? 'Object.prototype' : (p.tag !== undefined ? 'tag:' + p.tag : (Object.prototype.hasOwnProperty.call(p, 'constructor') && p.constructor.name) || 'other'); };
const desc = (o) => JSON.stringify({ k: Object.keys(o), n: Object.getOwnPropertyNames(o), j: JSON.stringify(o).replace(/[0-9]+/g, '#'), p: protoName(o) });
// Every distinct description, in first-seen order.
const kinds = (n, make) => { const seen = []; for (let i = 0; i < n; i++) { let d; try { d = desc(make(i)); } catch (e) { d = 'threw ' + e.name; } if (!seen.includes(d)) seen.push(d); } return seen; };
const t = (name, f) => { try { out.push(name + ': ' + JSON.stringify(f())); } catch (e) { out.push(name + ': threw ' + e.name); } };

t('1 counts differ', () => {
  function F(n) { this.a = 1; if (n > 0) this.b = 2; if (n > 1) this.c = 3; }
  return kinds(60, (i) => new F(i % 3));
});
t('2 same count, different shapes', () => {
  function F(i) { if (i % 2) this.x = 1; else this.y = 2; this.z = 3; }
  return kinds(60, (i) => new F(i));
});
t('3 conditional first property', () => {
  class C { constructor(i) { if (i % 4 === 0) this.q = 0; this.a = i; this.b = i; } }
  return kinds(60, (i) => new C(i));
});
t('4 delete mid-construction', () => {
  function F() { this.a = 1; this.b = 2; delete this.a; this.c = 3; this.a = 4; }
  return kinds(60, () => new F());
});
t('5 delete the only property', () => {
  function F() { this.a = 1; delete this.a; this.b = 2; }
  return kinds(60, () => new F());
});
t('6 throw mid-construction, then construct again', () => {
  const leaked = [];
  function F(i) { leaked.push(this); this.a = 1; if (i % 5 === 0) throw new Error('x'); this.b = 2; }
  const r = kinds(60, (i) => new F(i));
  return [r, [...new Set(leaked.map(desc))]];
});
t('7 defineProperty inside', () => {
  function F(i) { this.a = 1; Object.defineProperty(this, 'b', { value: 2, enumerable: i % 2 === 0 }); this.c = 3; }
  return kinds(60, (i) => new F(i));
});
t('8 defineProperty accessor then assign', () => {
  const log = [];
  function F(i) { Object.defineProperty(this, 'a', { get() { return 1; }, set(v) { log.push(v); }, configurable: true, enumerable: true }); this.a = i; this.b = i; }
  const r = kinds(60, (i) => new F(i));
  return [r, log.length];
});
t('9 prototype replaced', () => {
  function F(x) { this.a = x; this.b = x; }
  const seen = [];
  for (let i = 0; i < 60; i++) {
    if (i === 30) F.prototype = { tag: 'second' };
    if (i === 45) F.prototype = 5;
    seen.push(desc(new F(i)));
  }
  return [...new Set(seen)];
});
t('10 prototype gets a setter', () => {
  const log = [];
  function F(x) { this.a = x; this.b = x; }
  const seen = [];
  for (let i = 0; i < 60; i++) {
    if (i === 30) Object.defineProperty(F.prototype, 'a', { set(v) { log.push(v); }, get() { return 'g'; }, configurable: true });
    const o = new F(i);
    seen.push(desc(o) + o.a);
  }
  return [[...new Set(seen.slice(0, 30))], [...new Set(seen.slice(30))], log.length];
});
t('11 prototype gets a read-only property', () => {
  function F(x) { this.a = x; this.b = x; }
  const seen = [];
  for (let i = 0; i < 60; i++) {
    if (i === 30) Object.defineProperty(F.prototype, 'b', { value: 'ro', writable: false, configurable: true });
    seen.push(desc(new F(i)));
  }
  return [...new Set(seen)];
});
t('12 Object.prototype gets a setter', () => {
  const log = [];
  function F(x) { this.zz = x; this.yy = x; }
  const seen = [];
  for (let i = 0; i < 60; i++) {
    if (i === 30) Object.defineProperty(Object.prototype, 'yy', { set(v) { log.push(v); }, configurable: true });
    if (i === 50) delete Object.prototype.yy;
    seen.push(desc(new F(i)));
  }
  return [[...new Set(seen)], log.length];
});
t('13 Reflect.construct with another newTarget', () => {
  function C(x) { this.a = x; this.b = x; }
  function D() {}
  D.prototype = { tag: 'D' };
  const seen = [];
  for (let i = 0; i < 60; i++) seen.push(desc(i % 3 === 0 ? Reflect.construct(C, [i], D) : new C(i)));
  return [...new Set(seen)];
});
t('14 Reflect.construct with a class as newTarget', () => {
  class A { constructor(x) { this.a = x; } }
  class B extends A { constructor(x) { super(x); this.b = x; } }
  const seen = [];
  for (let i = 0; i < 60; i++) seen.push(desc(i % 2 ? Reflect.construct(A, [i], B) : new A(i)));
  return [...new Set(seen)];
});
t('15 derived and base alternate', () => {
  class A { constructor(x) { this.a = x; this.b = x; } }
  class B extends A { constructor(x) { super(x); this.c = x; } }
  class C extends B { constructor(x) { super(x); this.d = x; } }
  const seen = [];
  for (let i = 0; i < 90; i++) seen.push(desc(i % 3 === 0 ? new A(i) : i % 3 === 1 ? new B(i) : new C(i)));
  return [...new Set(seen)];
});
t('16 derived, arrow before super', () => {
  class A { constructor(x) { this.a = x; } }
  class B extends A { constructor(x) { const f = () => x; super(f()); this.b = x; } }
  return kinds(60, (i) => new B(i));
});
t('17 constructor returns another object', () => {
  function F(i) { this.a = 1; if (i % 3 === 0) return { r: i }; this.b = 2; }
  return kinds(60, (i) => new F(i));
});
t('18 constructor returns a primitive', () => {
  function F(i) { this.a = 1; return i; }
  return kinds(60, (i) => new F(i));
});
t('19 class fields', () => {
  class C { x = 1; y = this.x + 1; constructor(i) { this.z = i; } }
  return kinds(60, (i) => new C(i));
});
t('20 private fields', () => {
  class C { #p = 1; q = 2; constructor(i) { this.z = i; } get p() { return this.#p; } }
  const r = kinds(60, (i) => new C(i));
  return [r, new C(1).p];
});
t('21 field order versus constructor', () => {
  class C { a = 1; constructor(i) { this.b = i; this.a = 5; } }
  return kinds(60, (i) => new C(i));
});
t('22 collection in the loop', () => {
  function F(x) { this.a = x; this.b = x; this.c = x; }
  const keep = [];
  const seen = new Set();
  for (let i = 0; i < 80; i++) {
    if (i % 10 === 5) gc();
    const o = new F(i);
    if (i % 7 === 0) keep.push(o);
    seen.add(desc(o));
  }
  return [[...seen], keep.length];
});
t('23 collection while constructors are dropped', () => {
  const seen = new Set();
  for (let i = 0; i < 60; i++) {
    const K = new Function('x', 'this.a = x; this.b = x;');
    seen.add(desc(new K(i)));
    if (i % 10 === 5) gc();
  }
  return [...seen];
});
t('24 properties added after construction', () => {
  function F(x) { this.a = x; }
  const seen = [];
  for (let i = 0; i < 60; i++) { const o = new F(i); if (i % 2) o.b = 1; o.c = 2; seen.push(desc(o)); }
  return [...new Set(seen)];
});
t('25 strict, non-extensible this', () => {
  function F(i) { 'use strict'; this.a = 1; if (i % 2) Object.preventExtensions(this); this.b = 2; }
  return kinds(60, (i) => new F(i));
});
t('26 sloppy, non-extensible this', () => {
  function F(i) { this.a = 1; if (i % 2) Object.preventExtensions(this); this.b = 2; }
  return kinds(60, (i) => new F(i));
});
t('27 proxy as prototype', () => {
  const log = [];
  function F(x) { this.a = x; this.b = x; }
  F.prototype = new Proxy({}, { set(t, k, v, r) { log.push(k); return Reflect.set(t, k, v, r); } });
  return [kinds(40, (i) => new F(i)), log.length];
});
t('28 intermediate prototype gets the key late', () => {
  function F(x) { this.a = x; this.b = x; }
  const mid = {};
  F.prototype = Object.create(mid);
  const seen = [];
  for (let i = 0; i < 60; i++) {
    if (i === 30) Object.defineProperty(mid, 'b', { set(v) { seen.push('mid-setter'); }, configurable: true });
    seen.push(desc(new F(i)));
  }
  return [...new Set(seen)];
});
t('29 setPrototypeOf during construction', () => {
  function F(i) { this.a = 1; if (i % 3 === 0) Object.setPrototypeOf(this, { tag: 'moved' }); this.b = 2; }
  return kinds(60, (i) => new F(i));
});
t('30 many properties', () => {
  const body = Array.from({ length: 12 }, (_, i) => 'this.p' + i + ' = x;').join(' ');
  const F = new Function('x', body);
  return kinds(40, (i) => new F(i));
});
t('31 past the transition limit', () => {
  const body = Array.from({ length: 1100 }, (_, i) => 'this.p' + i + ' = x;').join(' ');
  const F = new Function('x', body);
  return kinds(12, (i) => new F(i)).map((s) => s.length);
});
t('32 a class expression in a loop', () => {
  const seen = new Set();
  for (let i = 0; i < 40; i++) { const C = class { constructor(x) { this.a = x; } }; seen.add(desc(new C(i))); }
  return [...seen];
});
t('33 this read mid-construction', () => {
  const seen = [];
  function F(i) { this.a = 1; seen.push(Object.keys(this).join()); this.b = 2; seen.push(Object.keys(this).join() + ('b' in this)); }
  for (let i = 0; i < 40; i++) new F(i);
  return [...new Set(seen)];
});
t('34 getter-only on the prototype, sloppy', () => {
  function F(x) { this.a = x; }
  Object.defineProperty(F.prototype, 'a', { get() { return 'getter'; }, configurable: true });
  return kinds(40, (i) => new F(i));
});
t('35 getter-only on the prototype, strict', () => {
  class F { constructor(x) { this.a = x; } }
  Object.defineProperty(F.prototype, 'a', { get() { return 'getter'; }, configurable: true });
  return kinds(40, (i) => new F(i));
});
t('36 subclasses of Array and Error', () => {
  class A extends Array { constructor(x) { super(); this.a = x; } }
  class E extends Map { constructor(x) { super(); this.code = x; } }
  const seen = [];
  for (let i = 0; i < 40; i++) seen.push(desc(new A(i)), desc(new E(i)));
  return [...new Set(seen)];
});
t('37 a method adds the property', () => {
  function F(x) { this.a = x; this.init(); }
  F.prototype.init = function () { this.b = this.a; };
  return kinds(40, (i) => new F(i));
});
out.join('\n');"####;

/// What Node 22 prints for the scenarios.
const EXPECTED: &str = r####"1 counts differ: ["{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\",\"b\",\"c\"],\"n\":[\"a\",\"b\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#,\\\"c\\\":#}\",\"p\":\"F\"}"]
2 same count, different shapes: ["{\"k\":[\"y\",\"z\"],\"n\":[\"y\",\"z\"],\"j\":\"{\\\"y\\\":#,\\\"z\\\":#}\",\"p\":\"F\"}","{\"k\":[\"x\",\"z\"],\"n\":[\"x\",\"z\"],\"j\":\"{\\\"x\\\":#,\\\"z\\\":#}\",\"p\":\"F\"}"]
3 conditional first property: ["{\"k\":[\"q\",\"a\",\"b\"],\"n\":[\"q\",\"a\",\"b\"],\"j\":\"{\\\"q\\\":#,\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"C\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"C\"}"]
4 delete mid-construction: ["{\"k\":[\"b\",\"c\",\"a\"],\"n\":[\"b\",\"c\",\"a\"],\"j\":\"{\\\"b\\\":#,\\\"c\\\":#,\\\"a\\\":#}\",\"p\":\"F\"}"]
5 delete the only property: ["{\"k\":[\"b\"],\"n\":[\"b\"],\"j\":\"{\\\"b\\\":#}\",\"p\":\"F\"}"]
6 throw mid-construction, then construct again: [["threw Error","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}"],["{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}"]]
7 defineProperty inside: ["{\"k\":[\"a\",\"b\",\"c\"],\"n\":[\"a\",\"b\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#,\\\"c\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\",\"c\"],\"n\":[\"a\",\"b\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"c\\\":#}\",\"p\":\"F\"}"]
8 defineProperty accessor then assign: [["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}"],60]
9 prototype replaced: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"tag:second\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"Object.prototype\"}"]
10 prototype gets a setter: [["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}0","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}1","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}2","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}3","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}4","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}5","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}6","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}7","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}8","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}9","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}10","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}11","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}12","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}13","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}14","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}15","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}16","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}17","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}18","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}19","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}20","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}21","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}22","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}23","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}24","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}25","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}26","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}27","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}28","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}29"],["{\"k\":[\"b\"],\"n\":[\"b\"],\"j\":\"{\\\"b\\\":#}\",\"p\":\"F\"}g"],30]
11 prototype gets a read-only property: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"F\"}"]
12 Object.prototype gets a setter: [["{\"k\":[\"zz\",\"yy\"],\"n\":[\"zz\",\"yy\"],\"j\":\"{\\\"zz\\\":#,\\\"yy\\\":#}\",\"p\":\"F\"}","{\"k\":[\"zz\"],\"n\":[\"zz\"],\"j\":\"{\\\"zz\\\":#}\",\"p\":\"F\"}"],20]
13 Reflect.construct with another newTarget: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"tag:D\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"C\"}"]
14 Reflect.construct with a class as newTarget: ["{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"A\"}","{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"B\"}"]
15 derived and base alternate: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"A\"}","{\"k\":[\"a\",\"b\",\"c\"],\"n\":[\"a\",\"b\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#,\\\"c\\\":#}\",\"p\":\"B\"}","{\"k\":[\"a\",\"b\",\"c\",\"d\"],\"n\":[\"a\",\"b\",\"c\",\"d\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#,\\\"c\\\":#,\\\"d\\\":#}\",\"p\":\"C\"}"]
16 derived, arrow before super: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"B\"}"]
17 constructor returns another object: ["{\"k\":[\"r\"],\"n\":[\"r\"],\"j\":\"{\\\"r\\\":#}\",\"p\":\"Object.prototype\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}"]
18 constructor returns a primitive: ["{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"F\"}"]
19 class fields: ["{\"k\":[\"x\",\"y\",\"z\"],\"n\":[\"x\",\"y\",\"z\"],\"j\":\"{\\\"x\\\":#,\\\"y\\\":#,\\\"z\\\":#}\",\"p\":\"C\"}"]
20 private fields: [["{\"k\":[\"q\",\"z\"],\"n\":[\"q\",\"z\"],\"j\":\"{\\\"q\\\":#,\\\"z\\\":#}\",\"p\":\"C\"}"],1]
21 field order versus constructor: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"C\"}"]
22 collection in the loop: [["{\"k\":[\"a\",\"b\",\"c\"],\"n\":[\"a\",\"b\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#,\\\"c\\\":#}\",\"p\":\"F\"}"],12]
23 collection while constructors are dropped: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"anonymous\"}"]
24 properties added after construction: ["{\"k\":[\"a\",\"c\"],\"n\":[\"a\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"c\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\",\"b\",\"c\"],\"n\":[\"a\",\"b\",\"c\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#,\\\"c\\\":#}\",\"p\":\"F\"}"]
25 strict, non-extensible this: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}","threw TypeError"]
26 sloppy, non-extensible this: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}","{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"F\"}"]
27 proxy as prototype: [["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"other\"}"],80]
28 intermediate prototype gets the key late: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"other\"}","mid-setter","{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"other\"}"]
29 setPrototypeOf during construction: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"tag:moved\"}","{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}"]
30 many properties: ["{\"k\":[\"p0\",\"p1\",\"p2\",\"p3\",\"p4\",\"p5\",\"p6\",\"p7\",\"p8\",\"p9\",\"p10\",\"p11\"],\"n\":[\"p0\",\"p1\",\"p2\",\"p3\",\"p4\",\"p5\",\"p6\",\"p7\",\"p8\",\"p9\",\"p10\",\"p11\"],\"j\":\"{\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#,\\\"p#\\\":#}\",\"p\":\"anonymous\"}"]
31 past the transition limit: [25317]
32 a class expression in a loop: ["{\"k\":[\"a\"],\"n\":[\"a\"],\"j\":\"{\\\"a\\\":#}\",\"p\":\"C\"}"]
33 this read mid-construction: ["a","a,btrue"]
34 getter-only on the prototype, sloppy: ["{\"k\":[],\"n\":[],\"j\":\"{}\",\"p\":\"F\"}"]
35 getter-only on the prototype, strict: ["threw TypeError"]
36 subclasses of Array and Error: ["{\"k\":[\"a\"],\"n\":[\"length\",\"a\"],\"j\":\"[]\",\"p\":\"A\"}","{\"k\":[\"code\"],\"n\":[\"code\"],\"j\":\"{\\\"code\\\":#}\",\"p\":\"E\"}"]
37 a method adds the property: ["{\"k\":[\"a\",\"b\"],\"n\":[\"a\",\"b\"],\"j\":\"{\\\"a\\\":#,\\\"b\\\":#}\",\"p\":\"F\"}"]
"####;

#[test]
fn objects_made_by_a_constructor_are_what_the_constructor_made() {
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
