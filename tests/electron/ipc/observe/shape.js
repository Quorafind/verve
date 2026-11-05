'use strict';
// A JSON-able description of a value that says what it is, not only what it prints as: the
// constructor, the kind of view, which bytes a view still has behind it. Both processes use it,
// so what one sent and what the other received can be put side by side.

const lone = /[\ud800-\udfff]/;

function shape(value, seen = new Map()) {
  if (value === undefined) return '#undefined';
  if (value === null) return null;
  switch (typeof value) {
    case 'number':
      if (Object.is(value, -0)) return '#-0';
      return Number.isFinite(value) ? value : `#${value}`;
    case 'string':
      return lone.test(value) ? `#lone:${[...value].map((c) => c.charCodeAt(0).toString(16)).join(',')}` : value;
    case 'bigint':
      return `#${value}n`;
    case 'symbol':
      return '#symbol';
    case 'function':
      return `#function:${value.name}`;
    case 'boolean':
      return value;
  }
  if (seen.has(value)) return `#seen:${seen.get(value)}`;
  seen.set(value, seen.size);
  const proto = Object.getPrototypeOf(value);
  const ctor = proto === null ? 'null-proto' : (proto.constructor && proto.constructor.name) || '?';
  const tag = Object.prototype.toString.call(value).slice(8, -1);
  const again = (inner) => shape(inner, seen);
  if (value instanceof Error) return errorShape(value, ctor, again);
  if (Array.isArray(value)) {
    const items = [];
    for (let at = 0; at < value.length; at++) items.push(at in value ? again(value[at]) : '#hole');
    const extra = Object.keys(value).filter((key) => !/^\d+$/.test(key));
    return { ctor, length: value.length, items, extra: extra.map((key) => [key, again(value[key])]) };
  }
  if (value instanceof Date) return { ctor, time: value.getTime() };
  if (value instanceof RegExp) return { ctor, source: value.source, flags: value.flags, lastIndex: value.lastIndex };
  if (value instanceof Map) return { ctor, entries: [...value].map(([k, v]) => [again(k), again(v)]) };
  if (value instanceof Set) return { ctor, items: [...value].map(again) };
  if (value instanceof ArrayBuffer) return { ctor, bytes: Array.from(new Uint8Array(value)) };
  if (ArrayBuffer.isView(value)) {
    const bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    return {
      ctor,
      isBuffer: Buffer.isBuffer(value),
      byteOffset: value.byteOffset,
      byteLength: value.byteLength,
      bufferByteLength: value.buffer.byteLength,
      bytes: Array.from(bytes),
    };
  }
  if (typeof Blob !== 'undefined' && value instanceof Blob) {
    return { ctor, size: value.size, type: value.type, name: value.name, lastModified: value.lastModified };
  }
  if (typeof Node !== 'undefined' && value instanceof Node) return { ctor, nodeName: value.nodeName };
  if (['Number', 'String', 'Boolean', 'BigInt'].includes(tag)) return { ctor, tag, boxed: again(value.valueOf()) };
  const keys = Object.keys(value);
  const props = {};
  for (const key of keys) props[key] = again(value[key]);
  return { ctor, tag, keys, props, symbols: Object.getOwnPropertySymbols(value).length };
}

function errorShape(error, ctor, again) {
  const own = Object.getOwnPropertyNames(error);
  const props = {};
  for (const key of own) if (key !== 'stack') props[key] = again(error[key]);
  const stack = typeof error.stack === 'string' ? error.stack.split('\n')[0] : typeof error.stack;
  return { ctor, name: error.name, message: error.message, own, stackHead: stack, props, enumerable: Object.keys(error) };
}

/** What a call threw, as data. */
function thrown(error) {
  if (error === null || typeof error !== 'object') return { primitive: shape(error) };
  return { ctor: error.constructor && error.constructor.name, name: error.name, message: error.message, code: error.code };
}

/** What `fn` answers or throws, as data. */
function attempt(fn) {
  try {
    return { returned: shape(fn()) };
  } catch (error) {
    return { threw: thrown(error) };
  }
}

async function attemptAsync(fn) {
  try {
    return { resolved: shape(await fn()) };
  } catch (error) {
    return { rejected: thrown(error) };
  }
}

module.exports = { shape, thrown, attempt, attemptAsync };
