'use strict';
// Values to send each way. Each is sent alone, so one that cannot be cloned does not hide the rest.

class Point {
  constructor() {
    this.x = 1;
  }
  norm() {
    return this.x;
  }
}

class MyError extends Error {
  constructor(message) {
    super(message);
    this.name = 'MyError';
    this.code = 'E_MINE';
  }
}

function samples() {
  const shared = { n: 1 };
  const circular = { name: 'loop' };
  circular.self = circular;
  const extra = [1, 2];
  extra.label = 'x';
  const regexp = /a+b/gi;
  regexp.lastIndex = 3;
  const withProps = new RangeError('ranged');
  withProps.code = 'E_RANGE';
  withProps.detail = { a: 1 };
  const noStack = new Error('nostack');
  delete noStack.stack;
  const nullProto = Object.create(null);
  nullProto.k = 'v';
  return {
    undefined: undefined,
    null: null,
    true: true,
    negativeZero: -0,
    nan: NaN,
    infinity: Infinity,
    string: 'héllo',
    loneSurrogate: '\ud800',
    bigint: 12n,
    date: new Date(86400000),
    invalidDate: new Date(NaN),
    regexp,
    error: new Error('plain'),
    typeError: new TypeError('typed'),
    errorWithProps: withProps,
    errorWithCause: new Error('outer', { cause: new Error('inner') }),
    customError: new MyError('mine'),
    errorWithoutStack: noStack,
    arrayBuffer: new Uint8Array([9, 8, 7]).buffer,
    uint8: new Uint8Array([1, 2, 3]),
    float32Offset: new Float32Array([1.5, 2.5, 3.5, 4.5]).subarray(1, 3),
    dataView: new DataView(new Uint8Array([1, 2, 3, 4]).buffer, 1, 2),
    uint8Window: new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]).subarray(2, 5),
    bigInt64: new BigInt64Array([5n]),
    buffer: Buffer.from('abc'),
    bufferAlloc: Buffer.alloc(3, 7),
    map: new Map([['a', 1], [{ k: 1 }, new Set([1])]]),
    set: new Set([1, 'two', [3]]),
    nested: { a: { b: [1, { c: 2 }] } },
    circular,
    sharedTwice: [shared, shared],
    sparse: [1, , 3],
    arrayWithProps: extra,
    getter: { get computed() { return 5; }, plain: 1 },
    nullProto,
    symbolKey: { [Symbol('s')]: 1, visible: 2 },
    instance: new Point(),
    undefinedProperty: { a: undefined, b: 1 },
    boxedNumber: new Number(4),
    boxedString: new String('s'),
    url: new URL('https://example.com/a?b=1'),
    // None of the rest can be cloned.
    function: () => 1,
    symbol: Symbol('x'),
    promise: Promise.resolve(1),
    weakMap: new WeakMap(),
    objectWithFunction: { f() {}, a: 1 },
    proxy: new Proxy({ a: 1 }, {}),
    generator: (function* () {})(),
  };
}

module.exports = { samples };
