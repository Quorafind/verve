# runtime-x

A JavaScript runtime for live plugins, in Rust.

- **The engine** is [Boa](https://github.com/boa-dev/boa), an interpreter written in Rust — here
  rebon's branch of it (`boa-rebon`, `main`): 0.22.0 with parser fixes, a cache of compiled
  regular expressions, a collector that frees lazily, and an interrupt handle. Its `SU.md` lists
  them.
- **The runtime** (`RuntimeX`) gives a plugin's `main.js` what it expects of Electron — a
  browser and Node in one — every one a Rust function: CommonJS `require`, timers on a clock the
  host drives, microtasks, `console`, `process`. Nothing here is JavaScript written to stand in
  for Node or the web.
- **Modules** are Node 22's CommonJS and native ECMAScript modules. `.mjs`, `.cjs`, package
  `type` and ambiguous `.js` syntax select the format. Both use package `exports`, `imports`
  (`#` names), self references, patterns and ordered conditions; ESM uses exact file URLs,
  never extension or directory-index guessing. `require(esm)` handles synchronous graphs,
  rejects top-level await before execution, and honors the `module.exports` export.
  Builtins are native synthetic modules sharing their CommonJS default export, with
  `syncBuiltinESMExports`; CJS named exports are discovered statically in Rust.
  `RuntimeX::load_esm` and `RuntimeX::import_module` return namespace promises; host-driven
  ticks resume top-level await without waiting. `forget_module` clears both module caches.
  The host still requires natively with `RuntimeX::require` and obtains Electron's
  `window.require` from `RuntimeX::global_require`.
- **Node's classes extend every way**: `class X extends EventEmitter`, and the older
  `function X() { EventEmitter.call(this) }` with `util.inherits` — each class is a `Proxy` of
  its native constructor, whose `apply` trap sets up the caller's own object.
- **Isolates** (`runtime_x::isolate`) put a runtime — and whatever the embedder serves with it —
  on a thread of its own. The embedder sends jobs and reads back events and snapshots, and never
  waits: a script busy for seconds holds only its own thread. A panic ends only its own isolate,
  and a script that will not return is stopped from outside (`Isolate::interrupt`), uncatchably.
  What runs in one is the embedder's: implement `isolate::Guest`.

## What a plugin finds

**Modules**
- Static and dynamic ESM imports, including imports from CommonJS wrappers and global scripts;
  Node package maps and conditions, builtins and statically discovered CJS named exports.
- `import.meta.url`, `filename`, `dirname` and synchronous `resolve`; JSON imports with
  `with { type: 'json' }`; `data:` and `blob:` modules. A blob import captures its bytes when
  `import()` is called, so immediate `URL.revokeObjectURL` does not cancel it.
- Module workers (`new Worker(url, { type: 'module' })`), with static imports and top-level
  await. Unsupported import schemes reject with Node error codes, never an unsettled promise.

**The web's globals**
- `EventTarget`, `Event`, `CustomEvent`, `MessageEvent`, `ErrorEvent`, `CloseEvent`,
  `ProgressEvent`; `DOMException`; `AbortController`/`AbortSignal` (an `EventTarget`, with
  `timeout` and `any`); `reportError`.
- `ReadableStream` (default and byte streams, BYOB reader, `tee`, `pipeTo`/`pipeThrough` with
  abort signals, `for await`, `ReadableStream.from`), `WritableStream`, `TransformStream`,
  `CountQueuingStrategy`/`ByteLengthQueuingStrategy`, `TextEncoderStream`/`TextDecoderStream`,
  `CompressionStream`/`DecompressionStream` (gzip, deflate, deflate-raw).
- `Headers`, `Request`, `Response` (body as bytes or a stream; `text`/`json`/`arrayBuffer`/
  `bytes`/`blob`/`formData`; `Response.json`/`error`/`redirect`) and `FormData` (sent as
  multipart, read back from multipart or urlencoded), constructible with no network — `fetch`
  itself is the host's.
- `Blob`/`File` (with `stream()`), `URL`, `URLSearchParams`, `URLPattern`, `atob`/`btoa`,
  `TextEncoder`/`TextDecoder`, `structuredClone` (typed arrays, `DataView`, errors, boxed
  primitives, `Blob`/`File`, `DOMException`, `transfer`), `performance`, `queueMicrotask`,
  `setImmediate`.
- `crypto`: `getRandomValues`, `randomUUID`, and `crypto.subtle` — digest; import/export/
  generate keys (raw, JWK); PBKDF2/HKDF derivation; AES-GCM/CBC/CTR encryption; HMAC sign and
  verify; AES-KW wrapping. No public-key algorithms.
- `MessageChannel`/`MessagePort` and `BroadcastChannel` within the realm, delivered as tasks on
  the host's clock; `WebSocket`, whose connection fails as an unreachable server's does;
  `Worker` (a realm on a thread of its own); `indexedDB` (kept on disk as JSON); `WebAssembly`
  (run by [wasmi](https://github.com/wasmi-labs/wasmi)); `navigator`, as Electron's Chromium has
  it.
- `Intl` in English: `Segmenter`, `DateTimeFormat` (the system zone, UTC, offsets, major zones
  with their daylight rules), `NumberFormat`, `Collator`, `PluralRules`, `RelativeTimeFormat`,
  `ListFormat`, `DisplayNames`, `Locale`; `toLocaleString` and `localeCompare` go through them.

**Node's modules**
- `process` (the global, and `require('process')`) is Electron's renderer's: an `EventEmitter`
  with `versions.node` and `versions.electron`, `env`, `cwd`/`chdir`, `hrtime`, `nextTick`,
  `memoryUsage`, `emitWarning`, `stdout`/`stderr` writing to the console; `exit` and `abort` are
  noted and end nothing.
- `events`: listeners in `_events` as Node keeps them, `newListener`/`removeListener`, the leak
  warning, `errorMonitor`, and the statics `once` (a promise) and `on` (an async iterator).
- `stream`: `Readable`, `Writable`, `Duplex`, `Transform`, `PassThrough` and the legacy
  `Stream`, with Node's buffering, backpressure and event order, state in `_readableState`/
  `_writableState`, `Readable.from`, async iteration, `pipeline`, `finished`, `addAbortSignal`;
  `stream/promises`, `stream/web`; `string_decoder`.
- `buffer`: `Buffer` complete — numbers at an offset (`readUInt32BE`, `writeDoubleLE`,
  `readBigUInt64LE`…), `copy`, `fill`, `indexOf`, `swap16`, every encoding as Node reads it,
  Node's error codes — with `kMaxLength`, `constants`, `SlowBuffer`, `isUtf8`, `transcode`.
- `util` writing values as Node does: `inspect` (Node's layout, depth, circular references,
  custom inspect), `format`, `isDeepStrictEqual`, `promisify`/`callbackify`, `deprecate`,
  `debuglog`, `types.*`, `MIMEType`, `parseArgs`; `util/types`. The console formats its
  arguments the same way. `assert` and `assert/strict`, failing with Node's `AssertionError`.
- `crypto`: hashes, HMAC, PBKDF2, scrypt, HKDF, AES-GCM/CBC/CTR/ECB ciphers, `randomBytes`/
  `randomInt`/`randomUUID`, `timingSafeEqual`, secret `KeyObject`s, `webcrypto`.
- `zlib`: deflate, raw deflate, gzip, unzip and brotli — synchronous, with callbacks, and as
  `Transform` streams; `crc32`, `constants`.
- `fs` (sync, callback and promise forms, `Stats`/`Dirent`/`Dir`, descriptors, `FileHandle`,
  `createReadStream`/`createWriteStream`, errors with Node's codes; `original-fs` is the same),
  `path` (exact `posix` and `win32`; `require('path')` writes `/` on Windows, as vault paths
  are), `os`, `url` (WHATWG, and the legacy `parse`/`format`/`resolve`), `querystring`,
  `punycode`, `constants`, `timers` and `timers/promises`, `async_hooks` (`AsyncLocalStorage`),
  `diagnostics_channel`, `perf_hooks`, `vm`, `v8`, `worker_threads`, `readline`, `tty`,
  `console`, `module`.
- `net`, `tls`, `dns`, `http`, `https`, `http2`, `dgram`, `cluster`, `child_process` and
  `node:sqlite` load with their classes and constants, and refuse to connect, listen, start
  programs or open databases, as Node does when the system says no: plugins use `requestUrl`.

## Using it

```toml
[dependencies]
runtime-x = { git = "http://nas.taila4c97f.ts.net:3000/boninall/runtime-x.git", branch = "main" }

# The same Boa runtime-x is built against.
[patch.crates-io]
boa_ast = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
boa_engine = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
boa_gc = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
boa_interner = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
boa_macros = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
boa_parser = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
boa_string = { git = "http://nas.taila4c97f.ts.net:3000/boninall/boa-rebon.git", branch = "main" }
```

Optimise Boa in the dev profile too (`[profile.dev.package.boa_engine] opt-level = 3`, and the
same for `boa_parser`, `boa_ast`, `boa_gc`, `boa_interner`, `boa_string`, `regress`, `wasmi`):
unoptimised, the interpreter is several times slower.

## Checking it

`cargo test` — the web's and Node's globals and modules, a `Worker` answering its page and never
holding it, IndexedDB as `idb` and Dexie use it, WebAssembly called both ways, and isolates
answering without being waited for, stopped from outside, and failing alone.

`cargo test --test corpus -- --ignored` loads real community plugins from the corpus beside the
repository (`../obsidian-corpus/plugins`, or `RUNTIME_X_CORPUS`), the host's own modules stood
in for.

Run the runtime on a thread with room for its stack, as workers and isolates do (16 MB or
more): Boa's interpreter recurses deeply, and a dev build overflows Windows' 1 MB main thread
on ordinary bundles.
