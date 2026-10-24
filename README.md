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
- **Modules** are CommonJS as Node has them: every built-in answers to its bare name and to
  `node:` before it; a plugin's own files resolve as Node resolves them (extensions, folders,
  `package.json`'s `main`, `node_modules`); `require.cache`, `require.resolve`, `module`, and
  `require('module')` (`createRequire`, `builtinModules`, `isBuiltin`). Node's own modules are
  made the first time they are required, the web's globals the first time they are read. The
  host requires natively with `RuntimeX::require`, and gets a `window.require` of no module's
  from `RuntimeX::global_require`.
- **Node's classes extend every way**: `class X extends EventEmitter`, and the older
  `function X() { EventEmitter.call(this) }` with `util.inherits` — each class is a `Proxy` of
  its native constructor, whose `apply` trap sets up the caller's own object.
- **Isolates** (`runtime_x::isolate`) put a runtime — and whatever the embedder serves with it —
  on a thread of its own. The embedder sends jobs and reads back events and snapshots, and never
  waits: a script busy for seconds holds only its own thread. A panic ends only its own isolate,
  and a script that will not return is stopped from outside (`Isolate::interrupt`), uncatchably.
  What runs in one is the embedder's: implement `isolate::Guest`.

## What a plugin finds

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
- `net`: permissioned TCP clients and servers (IPv4/IPv6), real `stream.Duplex` sockets,
  high-water-mark write backpressure, read suspension, keepalive, no-delay and inactivity
  timeouts. One lazy mio reactor and a fixed two-worker DNS pool deliver I/O on host beats.
  `dns.lookup` and promises use system getaddrinfo; `resolve4`, `resolve6`, `resolveTxt` and
  `resolveSrv` use the system resolver configuration. Other granted record queries fail ENOSYS.
- `RuntimeX::set_permissions(Permissions)` defaults to denial: outbound host/port rules,
  independent listening (`Deny`, `Loopback`, `Any`) and DNS. Replacing a policy closes existing
  resources; workers start denied. The reserved `child_process` bit does not enable execution.
- `tls`: rustls client/server Duplex sockets on the same reactor, system trust by default,
  PEM `ca`/`cert`/`key`, `secureContext`, SNI and named server contexts, ALPN, TLS 1.2/1.3
  bounds, cipher selection, client-certificate requests, `rejectUnauthorized`, the JavaScript
  identity hook, and peer certificate metadata. Trust-store loading and handshakes stay off
  the realm thread. Legacy TLS versions and OpenSSL engine/session options are not available.
- `http`, `https`, `http2`, `dgram`, `cluster`, `child_process` and `node:sqlite` load
  with their classes and constants. Process execution, cluster and IPC sockets remain denied.
  Without network capabilities, the original network refusal errors are preserved.

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
