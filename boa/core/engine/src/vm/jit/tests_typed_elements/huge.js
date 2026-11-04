const out = [];
const writer = () => new Function('a', 'vals', 'for (let i = 0; i < vals.length * 3; i++) a[0] = vals[i % vals.length];');
const vals = [1e300, -1e300, 3.4e38, 2 ** 63, -(2 ** 63), 2 ** 64 + 2 ** 12, 1e19, -1e19, 1e20, 2 ** 63 - 1024, 2 ** 53, 1.7976931348623157e308];
for (const K of [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array]) {
  const a = new K(1), row = [];
  for (const v of vals) { writer()(a, [v]); row.push(a[0]); }
  out.push(K.name + ':' + row.join());
}
out.join('\n');
