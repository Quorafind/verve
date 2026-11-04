//! The site cache for a typed array's `length` (`typed_length.rs`): every way the answer of the
//! interpreter's guard can change under a compiled site, and the read that is never cached.
//! The three modes must agree, and agree with Node's output for the same program.

use std::cell::Cell;

use super::{
    layout::FILLS,
    tests::{Mode, SHOW, context},
};
use crate::{
    Context, JsNativeError, JsValue, NativeFunction, Source, js_string,
    object::builtins::JsArrayBuffer,
};

/// A context with `detach(buffer)` and `gc()`, which the programs need and JavaScript lacks.
fn context_with_natives(mode: Mode) -> Context {
    let mut context = context(mode);
    context
        .register_global_callable(
            js_string!("detach"),
            1,
            NativeFunction::from_fn_ptr(|_, args, _| {
                let buffer = args
                    .first()
                    .and_then(JsValue::as_object)
                    .ok_or_else(|| JsNativeError::typ().with_message("not a buffer"))?;
                JsArrayBuffer::from_object(buffer.clone())?.detach(&JsValue::undefined())?;
                Ok(JsValue::undefined())
            }),
        )
        .expect("registers");
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
    context
}

pub(super) fn run_with_natives(source: &str, mode: Mode) -> String {
    let mut context = context_with_natives(mode);
    match context.eval(Source::from_bytes(&format!(
        "{SHOW}
{source}"
    ))) {
        Ok(value) => match value.as_string() {
            Some(string) => string.to_std_string_escaped(),
            None => value.display().to_string(),
        },
        Err(error) => format!("uncaught {error}"),
    }
}

/// The program, for `detach` and `gc`.
const SCENARIOS: &str = r##"const out = [];
const t = (name, f) => { lengths = makeLengths(); try { out.push(name + ':' + show(f())); } catch (e) { out.push(name + ':' + e.name); } };
const kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array,
               Uint32Array, Float32Array, Float64Array, BigInt64Array, BigUint64Array];
const TAP = Object.getPrototypeOf(Int8Array.prototype);
const original = Object.getOwnPropertyDescriptor(TAP, 'length');
const restore = () => { Object.defineProperty(TAP, 'length', original); };
// The site under test: a hot read of `length`, the values it takes recorded when they change, and
// `step` called every iteration to change the world under it. Each scenario gets a function of
// its own, so its inline cache starts empty (the interpreter's cache for a property of a
// prototype is not invalidated by deleting that property: see the package's status).
let lengths;
const makeLengths = () => new Function('a', 'n', 'step',
  'const r = []; for (let i = 0; i < n; i++) { const l = a.length; if (r.length === 0 || !Object.is(r[r.length - 1], l)) r.push(l); if (step) step(i); } return r;');
const at = (k, f) => (i) => { if (i === k) f(); };
const twice = (f) => { const r = []; for (let rep = 0; rep < 12; rep++) r.push(f(rep)); return r[11]; };

for (const K of kinds) {
  const size = K.BYTES_PER_ELEMENT;
  t('kind ' + K.name, () => twice(() => [lengths(new K(5), 100), lengths(new K(0), 100), lengths(new K(new ArrayBuffer(8 * size), size, 3), 100)]));
  t('sub ' + K.name, () => { class S extends K {} class S2 extends S {} return twice(() => [lengths(new S(4), 100), lengths(new S2(2), 100)]); });
}

t('detach', () => twice(() => kinds.map((K) => { const b = new ArrayBuffer(8 * K.BYTES_PER_ELEMENT); const a = new K(b);
  return lengths(a, 100, at(50, () => detach(b))); })));
t('detached before', () => twice(() => { const b = new ArrayBuffer(8); const a = new Int8Array(b); detach(b); return lengths(a, 100); }));
t('resizable', () => twice(() => {
  const b = new ArrayBuffer(16, { maxByteLength: 64 });
  const tracking = new Uint8Array(b), fixed = new Uint8Array(b, 0, 8), offset = new Float64Array(b, 8);
  const steps = (i) => { if (i === 30) b.resize(4); if (i === 60) b.resize(40); if (i === 80) b.resize(64); if (i === 90) b.resize(0); };
  return [lengths(tracking, 100, steps), (b.resize(16), lengths(fixed, 100, steps)), (b.resize(16), lengths(offset, 100, steps))];
}));
t('own length', () => twice(() => {
  const before = new Float64Array(3); Object.defineProperty(before, 'length', { value: 42 });
  const during = new Float64Array(3);
  const gone = new Float64Array(3);
  const getter = new Float64Array(3);
  return [lengths(before, 100), lengths(during, 100, at(50, () => Object.defineProperty(during, 'length', { value: 42 }))),
    lengths(gone, 100, (i) => { if (i === 30) Object.defineProperty(gone, 'length', { value: 'own', configurable: true }); if (i === 70) delete gone.length; }),
    lengths(getter, 100, at(40, () => Object.defineProperty(getter, 'length', { get() { return 'own getter'; } })))];
}));
t('prototype accessor', () => {
  try {
    return twice(() => {
      const patch = (descriptor) => () => { Object.defineProperty(TAP, 'length', descriptor); };
      const results = [];
      Object.defineProperty(TAP, 'length', { get() { return 'before'; }, configurable: true });
      results.push(lengths(new Int8Array(3), 100));
      restore();
      results.push(lengths(new Int8Array(3), 100, at(50, patch({ get() { return 'patched'; }, configurable: true }))));
      restore();
      results.push(lengths(new Int8Array(3), 100, (i) => { if (i === 30) patch({ get() { return 'a'; }, configurable: true })(); if (i === 60) restore(); }));
      restore();
      results.push(lengths(new Int8Array(3), 100, at(50, patch({ value: 7, configurable: true }))));
      restore();
      let calls = 0;
      results.push(lengths(new Int8Array(3), 100, at(50, patch({ get() { calls++; return 99; }, configurable: true }))), calls);
      restore();
      results.push(lengths(new Int8Array(3), 100, at(50, () => { delete TAP.length; })));
      restore();
      results.push(lengths(new Int8Array(3), 100, at(50, patch({ get: undefined, configurable: true }))));
      restore();
      results.push(lengths(new Int8Array(3), 100));
      return results;
    });
  } finally { restore(); }
});
t('typed prototype data length', () => {
  try {
    return twice(() => { delete Int8Array.prototype.length; return [lengths(new Int8Array(3), 100, at(50, () => Object.defineProperty(Int8Array.prototype, 'length', { value: 'p1', configurable: true })))]; });
  } finally { delete Int8Array.prototype.length; }
});
t('typed prototype getter length', () => {
  try {
    return twice(() => [lengths(new Int8Array(3), 100, (i) => {
      if (i === 20) Object.defineProperty(Int8Array.prototype, 'length', { get() { return 'p1 getter'; }, configurable: true });
      if (i === 70) delete Int8Array.prototype.length;
    })]);
  } finally { delete Int8Array.prototype.length; }
});
t('inserted prototype', () => {
  try {
    return twice(() => {
      const plain = Object.create(TAP);
      const shadow = Object.create(TAP, { length: { value: 'mid', configurable: true } });
      const r = [lengths(new Int8Array(3), 100, at(50, () => Object.setPrototypeOf(Int8Array.prototype, plain)))];
      Object.setPrototypeOf(Int8Array.prototype, TAP);
      r.push(lengths(new Int8Array(3), 100, at(50, () => Object.setPrototypeOf(Int8Array.prototype, shadow))));
      Object.setPrototypeOf(Int8Array.prototype, TAP);
      r.push(lengths(new Int8Array(3), 100, (i) => { if (i === 20) Object.setPrototypeOf(Int8Array.prototype, plain); if (i === 40) Object.setPrototypeOf(Int8Array.prototype, shadow); if (i === 60) Object.setPrototypeOf(Int8Array.prototype, TAP); }));
      Object.setPrototypeOf(Int8Array.prototype, plain);
      r.push(lengths(new Int8Array(3), 100, at(50, () => { shadow.length = 1; Object.defineProperty(plain, 'length', { value: 'late', configurable: true }); })));
      delete plain.length;
      return r;
    });
  } finally { Object.setPrototypeOf(Int8Array.prototype, TAP); }
});
t('deep subclasses', () => twice(() => {
  class A extends Int8Array {} class B extends A {} class C extends B {} class D extends C {}
  return [lengths(new A(1), 100), lengths(new B(2), 100), lengths(new C(3), 100), lengths(new D(4), 100),
    lengths(new D(4), 100, at(50, () => Object.defineProperty(B.prototype, 'length', { value: 'b', configurable: true })))];
}));
t('set prototype of receiver', () => twice(() => {
  const mk = () => new Int8Array(3);
  const to = (proto) => { let a = mk(); return lengths(a, 100, at(50, () => Object.setPrototypeOf(a, proto))); };
  let a = mk();
  const back = lengths(a, 100, (i) => { if (i === 30) Object.setPrototypeOf(a, { length: 'x' }); if (i === 60) Object.setPrototypeOf(a, Int8Array.prototype); });
  return [to({ length: 'x' }), to(null), to(Array.prototype), to(Float64Array.prototype), to(Object.create(Int8Array.prototype)), back];
}));
t('same site, other values', () => twice(() => {
  const values = [new Int8Array(3), [1, 2], 'abcd', new Float64Array(5), { length: 9 }, new Uint8Array(0), 'x'.repeat(40), [], 12, new Int16Array(2)];
  const r = [];
  for (let i = 0; i < 200; i++) { try { r.push(values[(i * 7) % values.length].length); } catch (e) { r.push(e.name); } }
  return [r.slice(0, 12).join(), r.length, r.join().length];
}));
t('fake typed array', () => twice(() => {
  const r = [];
  for (let i = 0; i < 100; i++) {
    const a = i < 50 ? new Float64Array(2) : (i < 60 ? Object.create(Float64Array.prototype) : new Float64Array(2));
    try { r.push(a.length); } catch (e) { r.push(e.name); }
  }
  return [r.indexOf('TypeError'), r.lastIndexOf('TypeError'), r.length, r.filter((x) => x === 'TypeError').length];
}));
t('dictionary prototype', () => twice(() => {
  const r = [];
  const churn = () => { for (let k = 0; k < 60; k++) Float64Array.prototype['p' + k] = k; for (let k = 0; k < 60; k++) delete Float64Array.prototype['p' + k]; };
  r.push(lengths(new Float64Array(3), 100, at(50, churn)));
  r.push(lengths(new Float64Array(3), 100, at(50, () => Object.defineProperty(Float64Array.prototype, 'length', { value: 'dict', configurable: true }))));
  delete Float64Array.prototype.length;
  r.push(lengths(new Float64Array(3), 100));
  return r;
}));
t('collections', () => twice(() => {
  const r = [];
  r.push(lengths(new Float64Array(3), 100, (i) => { if (i % 10 === 0) gc(); }));
  r.push(lengths(new Float64Array(3), 100, (i) => { if (i % 10 === 0) { gc(); const o = {}; o.x = i; Object.defineProperty(TAP, 'length', original); } }));
  return r;
}));
t('length in expressions', () => twice(() => {
  const a = new Float32Array(10), b = new Uint8Array(0);
  let s = 0;
  for (let i = 0; i < a.length; i++) { s += a.length - i; if (i < b.length) s += 1000; }
  const arr = [a, b]; let c = 0;
  for (let i = 0; i < 20; i++) c += arr[i & 1].length;
  return [s, c, a.length === 10, b.length === 0, typeof a.length];
}));
out.join('\n');"##;

/// What Node 22 prints for [`SCENARIOS`] (its `detach` is `structuredClone` with a transfer).
const EXPECTED: &str = r##"kind Int8Array:[[5],[0],[3]]
sub Int8Array:[[4],[2]]
kind Uint8Array:[[5],[0],[3]]
sub Uint8Array:[[4],[2]]
kind Uint8ClampedArray:[[5],[0],[3]]
sub Uint8ClampedArray:[[4],[2]]
kind Int16Array:[[5],[0],[3]]
sub Int16Array:[[4],[2]]
kind Uint16Array:[[5],[0],[3]]
sub Uint16Array:[[4],[2]]
kind Int32Array:[[5],[0],[3]]
sub Int32Array:[[4],[2]]
kind Uint32Array:[[5],[0],[3]]
sub Uint32Array:[[4],[2]]
kind Float32Array:[[5],[0],[3]]
sub Float32Array:[[4],[2]]
kind Float64Array:[[5],[0],[3]]
sub Float64Array:[[4],[2]]
kind BigInt64Array:[[5],[0],[3]]
sub BigInt64Array:[[4],[2]]
kind BigUint64Array:[[5],[0],[3]]
sub BigUint64Array:[[4],[2]]
detach:[[8,0],[8,0],[8,0],[8,0],[8,0],[8,0],[8,0],[8,0],[8,0],[8,0],[8,0]]
detached before:[0]
resizable:[[16,4,40,64,0],[8,0,8,0],[1,0,4,7,0]]
own length:[[42],[3,42],[3,"own",3],[3,"own getter"]]
prototype accessor:[["before"],[3,"patched"],[3,"a",3],[3,7],[3,99],49,[3,undefined],[3,undefined],[3]]
typed prototype data length:[[3,"p1"]]
typed prototype getter length:[[3,"p1 getter",3]]
inserted prototype:[[3],[3,"mid"],[3,"mid",3],[3,"late"]]
deep subclasses:[[1],[2],[3],[4],[4,"b"]]
set prototype of receiver:[[3,"x"],[3,undefined],[3,0],[3],[3],[3,"x",3]]
same site, other values:["3,0,9,2,,0,4,2,40,5,3,0",200,399]
fake typed array:[50,59,100,10]
dictionary prototype:[[3],[3,"dict"],[3]]
collections:[[3],[3]]
length in expressions:[55,100,true,true,"number"]"##;

#[test]
fn typed_array_length_matches_the_interpreter_and_node() {
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

/// A hot read of the `length` of a typed array fills its site once, and a collection costs one
/// miss, as it does for the other site caches.
#[test]
fn typed_array_length_sites_fill_once_and_a_collection_costs_one_miss() {
    let fills = || FILLS.with(Cell::get);
    let mut context = context_with_natives(Mode::Eager);
    let source = |collect: bool| {
        format!(
            "(function () {{
               const f = new Float64Array(4);
               let s = 0;
               for (let i = 0; i < 40; i++) {{ s += f.length; {} }}
               return s;
             }})()",
            if collect { "gc();" } else { "" }
        )
    };
    let before = fills();
    let value = context
        .eval(Source::from_bytes(&source(false)))
        .expect("runs");
    assert_eq!(value.as_number(), Some(160.0));
    let quiet = fills() - before;
    let before = fills();
    context
        .eval(Source::from_bytes(&source(true)))
        .expect("runs");
    let collecting = fills() - before;
    assert!(quiet <= 2, "a hot access fills its cache once: {quiet}");
    assert!(
        collecting >= 35,
        "each collection costs one miss: {collecting}"
    );
}
