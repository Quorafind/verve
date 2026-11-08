// The values the matrix sends across, made by whichever world sends them.
var cases = (function () {
  class Point { constructor() { this.x = 1; this.y = 2; } norm() { return 1; } }
  function named(a, b) { return a + b; }
  named.extra = 5;
  const shared = { n: 1 };
  const cycle = { name: 'loop' };
  cycle.self = cycle;
  const deep = {};
  let at = deep;
  for (let i = 0; i < 40; i++) at = at.next = {};
  const deepest = {};
  let low = deepest;
  for (let i = 0; i < 1100; i++) low = low.next = {};
  let gets = 0;
  const withGetter = { plain: 1, get computed() { gets++; return 'got'; } };
  const hidden = { shown: 1 };
  Object.defineProperty(hidden, 'secret', { value: 2, enumerable: false });
  const sym = Symbol('s');
  const keyed = { a: 1, [sym]: 2 };
  const bytes = new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]);
  const sparse = [1, , 3];
  const tagged = [1, 2];
  tagged.extra = 'x';
  const nullProto = Object.create(null);
  nullProto.k = 'v';
  const frozen = Object.freeze({ f: 1 });
  const err = new Error('boom');
  err.code = 'E_BOOM';
  class Custom extends Error { constructor(m) { super(m); this.name = 'Custom'; } }
  return [
    ['string', () => 'héllo'],
    ['numbers', () => [0, -0, NaN, Infinity, -Infinity, 1.5, 2 ** 53]],
    ['boolean', () => true],
    ['null', () => null],
    ['undefined', () => undefined],
    ['bigint', () => 12345678901234567890n],
    ['object', () => ({ a: 1, b: 'two', c: [3, { d: 4 }], e: null, f: undefined })],
    ['array', () => [1, 'a', [2], { z: 1 }]],
    ['sparse array', () => sparse],
    ['array with property', () => tagged],
    ['getter', () => withGetter],
    ['getter calls', () => gets],
    ['non-enumerable', () => hidden],
    ['symbol key', () => keyed],
    ['null prototype', () => nullProto],
    ['class instance', () => new Point()],
    ['frozen', () => frozen],
    ['shared reference', () => ({ a: shared, b: shared })],
    ['cycle', () => cycle],
    ['deep 40', () => deep],
    ['deep 1100', () => deepest],
    ['function', () => named],
    ['arrow', () => (x) => x],
    ['async function', () => async function later() { return 1; }],
    ['class', () => Point],
    ['bound', () => named.bind(null, 1)],
    ['object with function', () => ({ f: () => 'called', n: 1 })],
    ['resolved promise', () => Promise.resolve({ ok: [1, 2] })],
    ['rejected promise', () => { const p = Promise.reject(new Error('nope')); p.catch(() => {}); return p; }],
    ['thenable', () => ({ then(r) { r(1); } })],
    ['error', () => err],
    ['custom error', () => new Custom('mine')],
    ['type error', () => new TypeError('bad type')],
    ['error with cause', () => new Error('outer', { cause: new Error('inner') })],
    ['date', () => new Date(Date.UTC(2020, 0, 2, 3, 4, 5))],
    ['regexp', () => /a+b/gi],
    ['map', () => new Map([[1, { a: 1 }], ['k', 2]])],
    ['set', () => new Set([1, 'two'])],
    ['arraybuffer', () => new Uint8Array([9, 8, 7]).buffer],
    ['typed array', () => bytes],
    ['typed array view', () => new Uint8Array(bytes.buffer, 2, 3)],
    ['float64 array', () => new Float64Array([1.5, 2.5])],
    ['data view', () => new DataView(new ArrayBuffer(4))],
    ['blob', () => new Blob(['abc'], { type: 'text/plain' })],
    ['symbol', () => Symbol('top')],
    ['symbol value', () => ({ s: Symbol('inner') })],
    ['weakmap', () => new WeakMap()],
    ['proxy', () => new Proxy({ p: 1 }, {})],
    ['number object', () => new Number(7)],
    ['string object', () => new String('boxed')],
    ['dom node', () => document.body],
    ['event', () => new Event('x')],
    ['url', () => new URL('http://example.com/a?b=1')],
  ];
})();
