// Random reads and writes of every numeric kind: index, value and operation drawn from a seeded
// generator, one hot loop each, and the digest of what was read and of the bytes left behind.
let seed = 20240607;
const rnd = (n) => { seed = (Math.imul(seed, 1103515245) + 12345) & 0x7fffffff; return seed % n; };
const indices = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, -1, 1.5, '3', 2 ** 31, 2 ** 32, -0, NaN, 'x', 0.5, 7.0, 255, 1e9];
const values = [0, 1, -1, 2, 127, 128, 255, 256, 65535, 65536, 2 ** 31, 2 ** 32, -(2 ** 31), 0.5, 1.5, 2.5, 254.5, 255.5, -0, NaN, Infinity,
  -Infinity, 1e10, -1e10, 1.1, 16777217, 3.4e38, 1e-50, 123456.789, '7', true, null, undefined, { valueOf() { return 9; } }];
const body = new Function('a', 'ops', 'idx', 'val',
  'const r = []; for (let i = 0; i < ops.length; i++) { if (ops[i]) a[idx[i]] = val[i]; else r.push(a[idx[i]]); } return r;');
const out = [];
for (const K of [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array]) {
  for (let round = 0; round < 4; round++) {
    const n = 300, ops = [], idx = [], val = [];
    for (let i = 0; i < n; i++) { ops.push(rnd(2)); idx.push(indices[rnd(indices.length)]); val.push(values[rnd(values.length)]); }
    const a = new K(new ArrayBuffer(128), 8 * (round % 2), 4 + round);
    const reads = body(a, ops, idx, val);
    out.push(K.name + round + ':' + reads.map((v) => Object.is(v, -0) ? '-0' : String(v)).join() + '|' + Array.from(new Uint8Array(a.buffer)).join(' '));
  }
}
out.join('\n');
