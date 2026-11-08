const out = [];
const writer = () => new Function('a', 'vals', 'for (let i = 0; i < vals.length * 3; i++) a[0] = vals[i % vals.length];');
const vals = [1e300, -1e300, 3.4e38, 2 ** 63, -(2 ** 63), 2 ** 64, -(2 ** 64), 2 ** 64 + 2 ** 12, 1e19, -1e19, 1e20, 2 ** 63 - 1024,
  2 ** 53, 2 ** 53 + 2, -(2 ** 53) - 2, 2 ** 53 + 258, 2 ** 32 + 300, -(2 ** 32) - 300, 1.7976931348623157e308,
  -0, NaN, Infinity, -Infinity, 255.5, -255.5, 2 ** 31, 65535.9, -65537.9];
for (const K of [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array]) {
  const a = new K(1), row = [];
  for (const v of vals) { writer()(a, [v]); row.push(Object.is(a[0], -0) ? '-0' : a[0]); }
  out.push(K.name + ':' + row.join());
}
// DataView setters and String.fromCharCode convert through the same functions.
const view = new DataView(new ArrayBuffer(8));
const setters = [['Int8', 1], ['Uint8', 1], ['Int16', 2], ['Uint16', 2], ['Int32', 4], ['Uint32', 4]];
for (const [name, size] of setters) {
  const row = [];
  for (const v of vals) { view['set' + name](0, v); row.push(view['get' + name](0)); }
  out.push('DataView.' + name + ':' + row.join());
}
out.push('fromCharCode:' + vals.map((v) => String.fromCharCode(v).charCodeAt(0)).join());
const ints = new Int8Array(new SharedArrayBuffer(1));
out.push('Atomics.store:' + vals.map((v) => { Atomics.store(ints, 0, 0); Atomics.add(ints, 0, v); return ints[0]; }).join());
out.join('\n');
