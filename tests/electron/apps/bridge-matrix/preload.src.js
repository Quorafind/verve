const { contextBridge, ipcRenderer, webFrame, webUtils } = require('electron');
const say = (...args) => console.log(...args);
const byLabel = new Map(cases);

// The sandbox the preload itself runs in.
say('preload: globals', typeof require, typeof module, typeof exports, typeof process, typeof Buffer, typeof global, typeof setImmediate, typeof clearImmediate, typeof __filename, typeof __dirname);
say('preload: process', Object.keys(process).sort().join());
say('preload: process fields', process.type, process.sandboxed, process.contextIsolated, process.isMainFrame, process.platform, typeof process.pid, typeof process.env, Object.keys(process.env).length > 0, Array.isArray(process.argv), typeof process.versions.electron);
say('preload: electron', Object.keys(require('electron')).sort().join());
for (const name of ['fs', 'path', 'os', 'events', 'timers', 'url', 'util', 'buffer', 'crypto', 'child_process', 'node:events', 'node:fs', 'electron', 'electron/renderer', 'electron/common', './missing']) {
  try { require(name); say('preload: require', name, 'ok'); } catch (e) { say('preload: require', name, 'fails', e.name, e.message); }
}
say('preload: webFrame', Object.keys(webFrame).sort().join());
say('preload: webUtils', Object.keys(webUtils).sort().join());
say('preload: ipcRenderer', Object.keys(ipcRenderer).sort().join());
say('preload: readyState', document.readyState, typeof window.api, typeof window.require);
say('preload: window', window === globalThis, window === global, typeof window.fromPage);

// Each value exposed on its own, so that one that cannot cross does not take the rest down.
cases.forEach(([label, make], i) => {
  if (label === 'dom node' || label === 'event' || label === 'url') return;
  try {
    contextBridge.exposeInMainWorld('direct' + i, make());
    say('preload: expose', label, 'ok');
  } catch (e) {
    say('preload: expose', label, 'fails', e.name, e.message);
  }
});

const counter = { n: 0, list: [1] };
const original = () => 'original';
let kept;
contextBridge.exposeInMainWorld('api', {
  labels: cases.map((c) => c[0]),
  get: (label) => byLabel.get(label)(),
  take: (label, value) => {
    if (value instanceof Promise) value.catch(() => {});
    return describe(value);
  },
  takeLater: async (value) => describe(await value),
  echo: (value) => value,
  callback: (fn, ...args) => fn(...args),
  callbackSafe: (fn, arg) => {
    try { return fn(arg); } catch (e) { return 'preload caught ' + describe(e); }
  },
  keep: (fn) => { kept = fn; return 'kept'; },
  callKept: (...args) => kept(...args),
  original,
  isOriginal: (fn) => fn === original,
  sameAsKept: (fn) => fn === kept,
  mutate: (o) => { o.touched = true; if (Array.isArray(o)) o.push('added'); return describe(o); },
  thrower: () => { throw new TypeError('thrown in preload'); },
  throwValue: (v) => { throw v; },
  rejecter: () => Promise.reject(new RangeError('async fail')),
  later: () => new Promise((resolve) => setTimeout(() => resolve({ late: [1, 2] }), 10)),
  thisIs: function () { return this === undefined ? 'undefined' : typeof this; },
  counter,
  bump: () => ++counter.n,
  snapshot: () => describe(counter),
  proto: () => describe({ a: Object.getPrototypeOf([]) === Array.prototype }),
  arrayPush: () => { try { return [1].push(2); } catch (e) { return 'fails ' + e.message; } },
  args: (...all) => describe(all),
  count: (...all) => all.length,
  defaults: (a, b = 'dflt') => describe([a, b]),
  nested: { deep: { fn: (x) => x * 2 } },
  async: async (x) => x,
  ipc: (v) => ipcRenderer.invoke('echo', v),
  done: () => ipcRenderer.send('done'),
});

try {
  contextBridge.exposeInMainWorld('api', {});
} catch (e) {
  say('preload: expose again', e.name, e.message);
}
for (const key of ['location', 'document', 'Array']) {
  try { contextBridge.exposeInMainWorld(key, { a: 1 }); say('preload: expose', key, 'ok'); } catch (e) { say('preload: expose', key, 'fails', e.name, e.message); }
}
for (const [what, key, value] of [['number key', 5, {}], ['no value', 'novalue', undefined], ['string value', 'str', 'text'], ['function value', 'fnv', () => 1], ['null value', 'nullv', null]]) {
  try { contextBridge.exposeInMainWorld(key, value); say('preload: expose', what, 'ok'); } catch (e) { say('preload: expose', what, 'fails', e.name, e.message); }
}
try {
  contextBridge.exposeInIsolatedWorld(1004, 'other', { a: 1 });
  say('preload: exposeInIsolatedWorld ok');
} catch (e) { say('preload: exposeInIsolatedWorld fails', e.name, e.message); }
say('preload: executeInMainWorld', typeof contextBridge.executeInMainWorld);
try {
  say('preload: executeInMainWorld', describe(contextBridge.executeInMainWorld({ func: (a, b) => typeof window.api + a + b + typeof require, args: ['!', 2] })));
} catch (e) { say('preload: executeInMainWorld fails', e.name, e.message); }
window.addEventListener('DOMContentLoaded', () => say('preload: DOMContentLoaded', document.readyState, typeof window.api));
window.fromPreload = 'in the isolated world';
