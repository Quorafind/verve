// What a value is, as the world that holds it sees it: one line, deterministic. The same file runs
// in the page and (inlined by the preload's build step: see preload.js) in the preload.
var describe = (function () {
  const known = [
    ['Object', Object], ['Array', Array], ['Function', Function], ['Error', Error],
    ['TypeError', TypeError], ['RangeError', RangeError], ['Date', Date], ['RegExp', RegExp],
    ['Map', Map], ['Set', Set], ['Promise', Promise], ['ArrayBuffer', ArrayBuffer],
    ['Uint8Array', Uint8Array], ['Float64Array', Float64Array], ['DataView', DataView],
    ['Blob', typeof Blob === 'function' ? Blob : function Blob() {}],
    ['AsyncFunction', Object.getPrototypeOf(async function () {}).constructor],
  ];
  function protoName(v) {
    const proto = Object.getPrototypeOf(v);
    if (proto === null) return 'null';
    for (const [name, ctor] of known) if (proto === ctor.prototype) return name;
    const tag = Object.prototype.toString.call(v).slice(8, -1);
    return 'other:' + tag;
  }
  function inner(v, path, seen, depth) {
    const t = typeof v;
    if (v === null) return 'null';
    if (t === 'undefined') return 'undefined';
    if (t === 'string') return JSON.stringify(v);
    if (t === 'number') return Object.is(v, -0) ? '-0' : String(v);
    if (t === 'boolean') return String(v);
    if (t === 'bigint') return v + 'n';
    if (t === 'symbol') return 'symbol ' + v.toString();
    if (seen.has(v)) return '<same as ' + seen.get(v) + '>';
    if (depth > 4) return '<deep>';
    seen.set(v, path);
    const kind = protoName(v);
    if (t === 'function') {
      const own = Object.keys(v).map((k) => k + '=' + inner(v[k], path + '.' + k, seen, depth + 1));
      return `function ${kind} name=${JSON.stringify(v.name)} length=${v.length} own=[${own.join(',')}]`;
    }
    if (v instanceof Error || kind === 'Error' || kind === 'TypeError' || kind === 'RangeError') {
      const extra = Object.keys(v).filter((k) => k !== 'message' && k !== 'stack');
      const stack = typeof v.stack === 'string' ? 'stack' : typeof v.stack;
      const parts = extra.map((k) => k + '=' + inner(v[k], path + '.' + k, seen, depth + 1));
      return `error ${kind} name=${v.name} message=${JSON.stringify(v.message)} ${stack} cause=${'cause' in v ? inner(v.cause, path + '.cause', seen, depth + 1) : 'none'} extra=[${parts.join(',')}]`;
    }
    if (Array.isArray(v)) {
      const items = [];
      for (let i = 0; i < v.length; i++) items[i] = i in v ? inner(v[i], path + '[' + i + ']', seen, depth + 1) : '<hole>';
      const extra = Object.keys(v).filter((k) => !/^\d+$/.test(k));
      return `array ${kind} len=${v.length} [${items.join(', ')}] extra=[${extra.join(',')}]`;
    }
    if (v instanceof Date) return `date ${kind} ${isNaN(v) ? 'invalid' : v.toISOString()}`;
    if (v instanceof RegExp) return `regexp ${kind} ${v}`;
    if (v instanceof Map) return `map ${kind} {${[...v].map(([a, b]) => inner(a, path + '.k', seen, depth + 1) + '=>' + inner(b, path + '.v', seen, depth + 1)).join(', ')}}`;
    if (v instanceof Set) return `set ${kind} {${[...v].map((a) => inner(a, path + '.m', seen, depth + 1)).join(', ')}}`;
    if (v instanceof ArrayBuffer) return `arraybuffer ${kind} len=${v.byteLength} [${[...new Uint8Array(v)].join(',')}]`;
    if (ArrayBuffer.isView(v)) return `view ${kind} len=${v.byteLength} off=${v.byteOffset} buf=${v.buffer.byteLength} [${[...new Uint8Array(v.buffer, v.byteOffset, v.byteLength)].join(',')}]`;
    if (typeof Blob === 'function' && v instanceof Blob) return `blob ${kind} size=${v.size} type=${v.type}`;
    if (v instanceof Promise) return `promise ${kind}`;
    const tag = Object.prototype.toString.call(v).slice(8, -1);
    const keys = Object.keys(v);
    const syms = Object.getOwnPropertySymbols(v).length;
    const names = Object.getOwnPropertyNames(v).length;
    const parts = keys.map((k) => {
      let shown;
      try { shown = inner(v[k], path + '.' + k, seen, depth + 1); } catch (e) { shown = 'throws ' + e.message; }
      return k + ': ' + shown;
    });
    return `object ${kind} tag=${tag} {${parts.join(', ')}} syms=${syms} hidden=${names - keys.length} frozen=${Object.isFrozen(v)}`;
  }
  return (v) => inner(v, '$', new Map(), 0);
})();
