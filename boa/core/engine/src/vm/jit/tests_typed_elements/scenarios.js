const out = [];
const t = (name, f) => { try { out.push(name + ':' + show(f())); } catch (e) { out.push(name + ':' + e.name + (globalThis.DEBUG ? e.message : '')); } };
const numeric = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
const bigs = [BigInt64Array, BigUint64Array];
const twice = (f) => { let r; for (let rep = 0; rep < 2; rep++) r = f(rep); return r; };
// Each program gets functions of its own, so that its sites start empty.
const reader = () => new Function('a', 'keys', 'reps', 'step',
  'const r = []; const n = keys.length * reps; for (let i = 0; i < n; i++) { r.push(a[keys[i % keys.length]]); if (step) step(i); } return r;');
const writer = () => new Function('a', 'keys', 'vals', 'reps', 'step',
  'const n = keys.length * reps; for (let i = 0; i < n; i++) { a[keys[i % keys.length]] = vals[i % vals.length]; if (step) step(i); }');
const at = (k, f) => (i) => { if (i === k) f(); };
const bytes = (a) => Array.from(new Uint8Array(a.buffer, a.byteOffset, a.byteLength)).join(' ');
const tame = (r) => r.map((v) => typeof v === 'number' ? (Object.is(v, -0) ? '-0' : String(v)) : show(v)).join();
const inRange = [0, 1, 2, 3, 4, 5, 6, 7];
const keysMixed = [0, 1, 7, 8, 9, 100, -1, -2147483648, 2147483647, 2147483648, 4294967295, 4294967296, 1.5, -0, NaN, Infinity, '2', '02', '-0', '1.5', 'x', undefined, null, true, 2 ** 53];
const nums = [0, -0, 1, -1, 0.5, -0.5, 1.5, 2.5, 127, 128, 255, 255.5, 256, 65535, 65536, 2 ** 31 - 1, 2 ** 31, 2 ** 32, 2 ** 32 + 1, -(2 ** 31) - 1, 1e10, Infinity, -Infinity, NaN, 3.4e30, 1e-50, 16777217, 1.1, 2 ** 62, -(2 ** 62), 2 ** 53 + 2, 4294967297.9, -4294967297.9, 0.49999999999999994];
const odd = ['5', true, false, null, undefined, { valueOf() { return 7; } }, [3], '0x10', ' 12 ', 'abc', Symbol.iterator];

for (const K of numeric) {
  t('rw ' + K.name, () => twice(() => {
    const a = new K(8);
    writer()(a, inRange, nums, 40, null);
    return [Array.from(a).map(show).join(), bytes(a)];
  }));
  t('read ' + K.name, () => twice(() => {
    const a = new K(8); for (let i = 0; i < 8; i++) a[i] = i * 37 - 100;
    return tame(reader()(a, keysMixed, 6, null));
  }));
  t('write keys ' + K.name, () => twice(() => {
    const a = new K(8);
    writer()(a, keysMixed, [11, 12, 13], 4, null);
    return [tame(Array.from(a)), Object.keys(a).join(), bytes(a)];
  }));
  t('odd values ' + K.name, () => twice(() => {
    const a = new K(4); const r = [];
    for (const v of odd) { try { writer()(a, [0, 1, 2], [v], 5, null); r.push(show(a[0]) + '/' + show(a[1])); } catch (e) { r.push(e.name); } }
    return r.join();
  }));
  t('sub ' + K.name, () => twice(() => {
    class S extends K {} class S2 extends S {}
    const r = [];
    for (const C of [S, S2]) { const a = new C(4); writer()(a, [0, 1, 2, 3, 4], [1.5, -3, 300], 6, null); r.push(tame(reader()(a, [0, 1, 2, 3, 4], 3, null))); }
    return r;
  }));
  t('offset ' + K.name, () => twice(() => {
    const size = K.BYTES_PER_ELEMENT;
    const b = new ArrayBuffer(16 * size);
    const a = new K(b, 2 * size, 5);
    writer()(a, [0, 1, 2, 3, 4, 5, -1], [9, 8, 7, 6, 5, 4], 5, null);
    return [tame(reader()(a, [0, 1, 2, 3, 4, 5, 6], 2, null)), bytes(new K(b))];
  }));
  t('detach ' + K.name, () => twice(() => {
    const b = new ArrayBuffer(8 * K.BYTES_PER_ELEMENT); const a = new K(b); a.fill(3);
    const r = [tame(reader()(a, inRange, 12, at(50, () => detach(b))))];
    const c = new ArrayBuffer(8 * K.BYTES_PER_ELEMENT); const w = new K(c);
    writer()(w, inRange, [5, 6], 12, at(50, () => detach(c)));
    r.push(w.length, w[0], w.byteLength);
    return r;
  }));
  t('detached before ' + K.name, () => twice(() => {
    const b = new ArrayBuffer(8 * K.BYTES_PER_ELEMENT); const a = new K(b); detach(b);
    writer()(a, inRange, [1], 4, null);
    return [tame(reader()(a, inRange, 4, null)), a.length];
  }));
  t('resizable ' + K.name, () => twice(() => {
    const size = K.BYTES_PER_ELEMENT;
    const b = new ArrayBuffer(8 * size, { maxByteLength: 64 * size });
    const tracking = new K(b), fixed = new K(b, 0, 4), offset = new K(b, size, 3), offsetTracking = new K(b, 2 * size);
    const r = [];
    const steps = (i) => { if (i === 20) b.resize(3 * size); if (i === 40) b.resize(20 * size); if (i === 60) b.resize(0); if (i === 80) b.resize(10 * size); };
    for (const a of [tracking, fixed, offset, offsetTracking]) {
      b.resize(8 * size);
      writer()(a, [0, 1, 2, 3, 4, 5, 9, 15], [1, 2, 3], 14, steps);
      b.resize(8 * size);
      r.push(tame(reader()(a, [0, 1, 2, 3, 4, 5, 9, 15], 14, steps)));
      r.push(a.length);
    }
    return r;
  }));
  t('grow only ' + K.name, () => twice(() => {
    const size = K.BYTES_PER_ELEMENT;
    const b = new ArrayBuffer(2 * size, { maxByteLength: 64 * size });
    const a = new K(b);
    writer()(a, [0, 1, 2, 3, 4, 5], [7], 20, (i) => { if (i % 20 === 5) b.resize(Math.min(64, 2 + i / 5) * size); });
    return [a.length, tame(Array.from(a))];
  }));
  t('valueOf detaches ' + K.name, () => twice(() => {
    const b = new ArrayBuffer(8 * K.BYTES_PER_ELEMENT); const a = new K(b);
    const evil = { valueOf() { detach(b); return 5; } };
    const vals = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, evil];
    writer()(a, [0, 1, 2], vals, 9, null);
    return [a.length, a[0], a.byteLength];
  }));
  t('valueOf shrinks ' + K.name, () => twice(() => {
    const size = K.BYTES_PER_ELEMENT;
    const b = new ArrayBuffer(8 * size, { maxByteLength: 16 * size }); const a = new K(b, 0, 8);
    const evil = { valueOf() { b.resize(2 * size); return 5; } };
    writer()(a, [0, 1, 2, 7], [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, evil, 4], 3, null);
    return [a.length, tame(reader()(a, [0, 1, 2, 7], 2, null))];
  }));
  t('shared ' + K.name, () => twice(() => {
    const b = new SharedArrayBuffer(8 * K.BYTES_PER_ELEMENT); const a = new K(b);
    writer()(a, [0, 1, 2, 7, 8], [3.5, 250, -1], 10, null);
    return [tame(reader()(a, [0, 1, 2, 7, 8], 4, null)), bytes(a)];
  }));
  t('growable shared ' + K.name, () => twice(() => {
    const size = K.BYTES_PER_ELEMENT;
    const b = new SharedArrayBuffer(2 * size, { maxByteLength: 16 * size }); const a = new K(b);
    writer()(a, [0, 1, 2, 3], [4, 5], 12, (i) => { if (i === 10) b.grow(8 * size); });
    return [a.length, tame(Array.from(a))];
  }));
  t('not extensible ' + K.name, () => twice(() => {
    const a = new K(4); Object.preventExtensions(a);
    writer()(a, [0, 1, 4, 5], [9, 8], 6, null);
    return [tame(Array.from(a)), Object.isExtensible(a)];
  }));
  t('own props ' + K.name, () => twice(() => {
    const a = new K(4); a.foo = 1; Object.defineProperty(Object.prototype, '1000', { get() { return 'proto'; }, set(v) { throw new Error('set1000'); }, configurable: true });
    try { writer()(a, [0, 1000, 1], [2, 3], 6, null); return [tame(reader()(a, [0, 1000, 1, 10], 3, null)), Object.keys(a).join()]; } finally { delete Object.prototype[1000]; }
  }));
}
for (const K of bigs) {
  t('rw ' + K.name, () => twice(() => {
    const a = new K(8);
    writer()(a, inRange, [0n, 1n, -1n, 2n ** 63n, -(2n ** 63n), 2n ** 64n - 1n, 2n ** 64n, 123456789012345678901234567890n], 20, null);
    return [Array.from(a).map(show).join(), bytes(a)];
  }));
  t('numbers throw ' + K.name, () => twice(() => {
    const a = new K(4); const r = [];
    for (const v of [1, 1.5, '5', true, undefined, null, Symbol.iterator, {}]) { try { writer()(a, [0, 1], [v], 3, null); r.push('ok'); } catch (e) { r.push(e.name); } }
    return r.join();
  }));
  t('read ' + K.name, () => twice(() => {
    const a = new K(8); for (let i = 0; i < 8; i++) a[i] = BigInt(i * 1000003) - 5n;
    return tame(reader()(a, keysMixed, 5, null));
  }));
  t('detach ' + K.name, () => twice(() => {
    const b = new ArrayBuffer(64); const a = new K(b);
    return [tame(reader()(a, inRange, 12, at(50, () => detach(b))))];
  }));
}

t('float32 rounding', () => twice(() => {
  const a = new Float32Array(1), u = new Uint32Array(a.buffer);
  const vals = [16777217, 1.1, 3.4028235677973366e38, 3.4028235677973362e38, 1e39, -1e39, 1e-46, 1.4e-45, 7e-46, 1.17549435e-38, 0.1, NaN, -0, 5e-324, 2 ** 24 + 3, 0.30000000000000004];
  const r = [];
  for (const v of vals) { writer()(a, [0], [v], 20, null); r.push(show(a[0]) + '/' + u[0]); }
  return r.join(' ');
}));
t('clamped boundaries', () => twice(() => {
  const a = new Uint8ClampedArray(1); const r = [];
  const vals = [-1, 0, 0.5, 1.5, 2.5, 3.5, 254.5, 254.4, 255, 255.5, 256, 300, -300, NaN, Infinity, -Infinity, -0, -0.5, 0.49999999999999994, 2147483647, 2147483648, -2147483648, 1e300, 127.5, 128.5];
  for (const v of vals) { writer()(a, [0], [v], 20, null); r.push(a[0]); }
  return r.join();
}));
t('integer wraps', () => twice(() => {
  const r = [];
  for (const K of [Int8Array, Uint8Array, Int16Array, Uint16Array, Int32Array, Uint32Array]) {
    const a = new K(1);
    const vals = [127, 128, 255, 256, 32767, 32768, 65535, 65536, 2147483647, 2147483648, 4294967295, 4294967296, -1, -128, -129, -32769, -2147483648, -2147483649, 1e10, -1e10, 2 ** 53, 2 ** 62, 123456789.987, -123456789.987, 255.99, -0.99, NaN, Infinity];
    const row = [];
    for (const v of vals) { writer()(a, [0], [v], 20, null); row.push(a[0]); }
    r.push(row.join());
  }
  return r;
}));
t('float64 specials', () => twice(() => {
  const a = new Float64Array(4), b = new BigUint64Array(a.buffer);
  const r = [];
  for (const v of [NaN, -0, 0, Infinity, -Infinity, 5e-324, 1.7976931348623157e308, 0.1, 2 ** 53 + 2]) { writer()(a, [0, 3], [v], 6, null); r.push(show(a[0]) + '/' + b[0]); }
  return r.join(' ');
}));
t('aliasing views', () => twice(() => {
  const b = new ArrayBuffer(16); const f = new Float32Array(b), u8 = new Uint8Array(b), i16 = new Int16Array(b, 2, 3), d = new Float64Array(b, 8, 1);
  writer()(f, [0, 1, 2, 3], [1.5, -2.25, 3, 1e30], 8, null);
  writer()(i16, [0, 1, 2, 3], [-3, 70000, 5], 8, null);
  writer()(d, [0], [Math.PI], 4, null);
  return [bytes(f), tame(reader()(u8, [0, 5, 9, 15, 16], 2, null)), tame(reader()(f, [0, 1, 2, 3], 2, null)), tame(reader()(i16, [0, 1, 2, 3], 2, null))];
}));
t('same site, other receivers', () => twice(() => {
  const vals = [new Float64Array(3).fill(1.5), new Int8Array([1, -2, 3]), [10, 20, 30], { 0: 'a', 1: 'b', 2: 'c' }, 'xyz', new Uint8ClampedArray([300, 2, 3]), new BigInt64Array([5n, 6n, 7n]),
    new Float32Array([0.1, 0.2, 0.3]), Object.create(new Int32Array([4, 5, 6])), new Proxy(new Uint16Array([7, 8, 9]), {}), new Proxy([1, 2, 3], { get: (t, k) => 'p' + String(k) }), [1.5, 2.5, 3.5], [, 'hole', 3], null, 12, undefined];
  const get = new Function('o', 'i', 'return o[i];');
  const set = new Function('o', 'i', 'v', 'o[i] = v;');
  const r = [];
  for (let n = 0; n < 160; n++) {
    const o = vals[n % vals.length], i = n % 3;
    try { r.push(show(get(o, i))); } catch (e) { r.push(e.name); }
    try { set(o, i, n % 5 === 0 ? 2.5 : n); } catch (e) { r.push('set' + e.name); }
  }
  return [r.length, r.join().length, r.slice(0, 32).join(' '), r.slice(-16).join(' ')];
}));
t('inherited elements', () => twice(() => {
  const base = new Int16Array([1, 2, 3]); const o = Object.create(base);
  const get = new Function('o', 'i', 'return o[i];'), set = new Function('o', 'i', 'v', 'o[i] = v;');
  const r = [];
  for (let n = 0; n < 40; n++) { r.push(get(o, n % 4)); set(o, n % 4, n); }
  return [r.join(), Object.keys(o).join(), Array.from(base).join()];
}));
t('prototype accessors', () => twice(() => {
  Object.defineProperty(Object.prototype, '1005', { get() { return 'obj5'; }, configurable: true });
  Object.defineProperty(Object.getPrototypeOf(Int8Array.prototype), '1006', { get() { return 'ta6'; }, configurable: true });
  try { return tame(reader()(new Int8Array(3), [0, 1, 1005, 1006, 7], 4, null)); }
  finally { delete Object.prototype[1005]; delete Object.getPrototypeOf(Int8Array.prototype)[1006]; }
}));
t('collections', () => twice(() => {
  const a = new Float64Array(64); const r = [];
  writer()(a, [0, 1, 2, 3], [1.5, 2.5], 25, (i) => { if (i % 7 === 0) { gc(); const junk = []; for (let k = 0; k < 20; k++) junk.push({ k }); } });
  r.push(tame(reader()(a, [0, 1, 2, 3], 25, (i) => { if (i % 5 === 0) gc(); })));
  return r;
}));
t('hot loops', () => twice(() => {
  const f = new Float64Array(2000), u = new Uint8Array(2000), c = new Uint8ClampedArray(2000), i32 = new Int32Array(2000), f32 = new Float32Array(2000);
  let s = 0;
  for (let i = 0; i < f.length; i++) { f[i] = i * 0.5; u[i] = i; c[i] = i - 100; i32[i] = i * 100000; f32[i] = i / 3; }
  for (let r = 0; r < 3; r++) for (let i = 0; i < f.length; i++) { f[i] = f[i] * 1.0001 + 1; s += f[i] + u[i] + c[i] + i32[i] + f32[i]; }
  return [s, f[1999], u[1999], c[1999], i32[1999], f32[1999]];
}));
t('index types in loop', () => twice(() => {
  const a = new Float64Array([1, 2, 3, 4]);
  const get = new Function('a', 'ks', 'let s = []; for (let i = 0; i < ks.length; i++) s.push(a[ks[i]]); return s;');
  return tame(get(a, [0, 1, 2.0, 3, 1.5, '1', -1, 4, 1e0, 2 ** 31, Math.floor(2.7), 3 - 1, 0.1 * 10]));
}));
out.join('\n');
