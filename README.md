# Verve

A JavaScript runtime for live plugins, in Rust — on its way to a Deno-like runtime, with
Node's and the web's APIs and no Node underneath.

- **The engine** is [Boa](https://github.com/boa-dev/boa), an interpreter written in Rust,
  carried in this repository under `boa/`: 0.22.0 with module-loading hooks, parser fixes, a
  cache of compiled regular expressions, a collector that frees lazily, and an interrupt handle.
  `boa/SU.md` lists them.
- **The runtime** (`Runtime`) gives a plugin's `main.js` what it expects of Electron — a
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
  `Runtime::load_esm` and `Runtime::import_module` return namespace promises; host-driven
  ticks resume top-level await without waiting. `forget_module` clears both module caches.
  The host still requires natively with `Runtime::require` and obtains Electron's
  `window.require` from `Runtime::global_require`.
- **Node's classes extend every way**: `class X extends EventEmitter`, and the older
  `function X() { EventEmitter.call(this) }` with `util.inherits` — each class is a `Proxy` of
  its native constructor, whose `apply` trap sets up the caller's own object.
- **Isolates** (`verve::isolate`) put a runtime — and whatever the embedder serves with it —
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
- `module.registerHooks` resolve and load hooks, and `module.stripTypeScriptTypes` for the
  TypeScript a load hook hands on: Node's `strip` mode (types erased in place, every line and
  column kept) and `transform` mode (enums, namespaces and parameter properties compiled, with
  an optional source map), parsed and compiled by oxc.

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
  multipart, read back from multipart or urlencoded), constructible with no network.
- `fetch`, on the `http`/`https` client below and under the same `Permissions`: the response
  body a `ReadableStream` fed as bytes arrive (an SSE reply is read event by event) and paused
  while its reader is behind; `gzip`/`deflate`/`br` decoded; redirects followed (up to 20),
  `manual` or `error`; `AbortSignal` rejecting the response or erroring its body, and closing
  the connection; string, buffer, `Blob`, `URLSearchParams`, `FormData` and stream bodies;
  `data:` and `blob:` addresses. Network errors and refused hosts reject with
  `TypeError: fetch failed` and a `cause`, as undici does. A host that brings its own `fetch`
  puts it in the global's place.
- `Blob`/`File` (with `stream()`), `URL`, `URLSearchParams`, `URLPattern`, `atob`/`btoa`,
  `TextEncoder`/`TextDecoder`, `structuredClone` (typed arrays, `DataView`, errors, boxed
  primitives, `Blob`/`File`, `DOMException`, `transfer`), `performance`, `queueMicrotask`,
  `setImmediate`.
- `crypto`: `getRandomValues`, `randomUUID`, and `crypto.subtle` — digest; import/export/
  generate keys (raw, JWK); PBKDF2/HKDF derivation; AES-GCM/CBC/CTR encryption; HMAC sign and
  verify; AES-KW wrapping. No public-key algorithms.
- `MessageChannel`/`MessagePort` and `BroadcastChannel` within the realm, delivered as tasks on
  the host's clock; permissioned `WebSocket` (`ws`/`wss`, text/binary, subprotocols and close
  handshakes) on the TCP/TLS reactor, retaining the original failure events when denied;
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
- `Runtime::set_permissions(Permissions)` is the permission model. The network defaults to
  denial: outbound host/port rules, independent listening (`Deny`, `Loopback`, `Any`) and DNS;
  replacing a network policy closes existing resources, and workers get the network only as
  `Runtime::set_worker_permissions` grants it. The filesystem and workers default to what
  embedders always had — `fs_read`/`fs_write: FsAccess::All`, `worker: true` — and confine a
  realm as Node's `--permission` does: `FsAccess::Only(paths)` grants each path and everything
  below it, and `Permissions::none()` is Node's `--permission` with no grants. Every `fs` entry
  point (sync, callback, promises, streams, `Dir`, `FileHandle`, `watch`, `exists`, `access`,
  `realpath`, `stat`…) and every module read from disk (`require`, `import`, `import()`) is
  checked, failing with Node's `ERR_ACCESS_DENIED` (`permission: 'FileSystemRead'` or
  `'FileSystemWrite'`, `resource`); a `package.json` resolution may not read is passed over.
  Paths are checked where they really are — `..` resolved and every link followed, so neither
  escapes a grant (Node checks the path as written). A worker shares its page's filesystem and
  worker grants; without `worker`, `Worker` and `worker_threads.Worker` throw `ERR_ACCESS_DENIED`
  (`WorkerThreads`). `process.permission.has(scope, reference)` answers as Node's (`fs`,
  `fs.read`, `fs.write`, `worker`, and Verve's `net`; `child`, `addon` and `wasi` are always
  false). `Runtime::add_trusted_certificates` trusts PEM certificates besides the system's, as
  Node's `NODE_EXTRA_CA_CERTS` does. The reserved `child_process` bit does not enable execution.
- `Runtime::set_proxy_env(Some(ProxyEnv))` is Node 24's `NODE_USE_ENV_PROXY`: `fetch`,
  `WebSocket` and the `http`/`https` global agents (and an `Agent` with `proxyEnv`) go through
  `HTTP_PROXY`/`HTTPS_PROXY` — absolute-URI requests for `http:`, a `CONNECT` tunnel with TLS to
  the real host for `https:`, `ws:` and `wss:` — except for the hosts `NO_PROXY` lists. The
  target is never resolved locally; permissions are checked against it, and the configured proxy
  needs no rule. HTTP proxies only: `https://` and `socks5://` ones fail the request
  (`ERR_PROXY_INVALID_CONFIG`) rather than going direct. `src/network/proxy.rs` has the rules.
- `Runtime::set_waker` lets a host sleep until the network or a worker has something for the
  realm, instead of being asked (by `next_due`) to look every millisecond while a socket is
  open.
- `tls`: rustls client/server Duplex sockets on the same reactor, system trust by default,
  PEM `ca`/`cert`/`key`, `secureContext`, SNI and named server contexts, ALPN, TLS 1.2/1.3
  bounds, cipher selection, client-certificate requests, `rejectUnauthorized`, the JavaScript
  identity hook, and peer certificate metadata. Trust-store loading and handshakes stay off
  the realm thread. PEM private keys must be unencrypted; PFX, key passphrases, legacy TLS
  versions and OpenSSL engine/session options are not available. Cipher lists accept the
  rustls/IANA names and the supported suites' OpenSSL aliases, not arbitrary OpenSSL expressions.
- `http`/`https`: streaming HTTP/1.1 clients and servers over those sockets, Writable
  `ClientRequest`/`ServerResponse`, Readable `IncomingMessage`, consumed-response keepalive
  pooling, chunked bodies and trailers, `rawHeaders`/`headersDistinct`, Node's duplicate-header
  merging, upgrades, and explicit all/idle connection closure. Redirects are returned, never
  automatically followed. Idle pool limits are enforced; active `maxSockets`/`maxTotalSockets`
  request scheduling is not implemented.
- `dgram`: IPv4/IPv6 UDP sockets, explicit bind and implicit outbound bindings, connected
  peers, Buffer/string/sliced sends, message metadata, multicast and socket options. Listening
  needs its own capability; an outbound-only binding accepts replies only from contacted peers.
  Buffer-size getters expose kernel snapshots delivered on host beats; option setters are
  applied by the reactor, not synchronously by the realm.
- `http2`, `cluster`, `child_process` and `node:sqlite` load
  with their classes and constants. Process execution, cluster and IPC sockets remain denied.
  Without network capabilities, the original network refusal errors are preserved.

## Using it

```toml
[dependencies]
verve = { git = "https://github.com/Quorafind/verve.git", branch = "main" }

# The same Boa Verve is built against: the crates under `boa/` in the same repository.
[patch.crates-io]
boa_ast = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
boa_engine = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
boa_gc = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
boa_interner = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
boa_macros = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
boa_parser = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
boa_string = { git = "https://github.com/Quorafind/verve.git", branch = "main" }
```

Optimise Boa in the dev profile too (`[profile.dev.package.boa_engine] opt-level = 3`, and the
same for `boa_parser`, `boa_ast`, `boa_gc`, `boa_interner`, `boa_string`, `regress`, `wasmi`):
unoptimised, the interpreter is several times slower.

## `verve run`

The `verve` binary (and `verve::cli::main`, for an embedder shipping a binary of its own) runs
a script as a process of its own, as `node script.js` does:

```sh
verve run --allow-fs-read=./runtime --allow-fs-write=./data main.mjs --its args
verve run -e "console.log(process.argv)" a b
```

`process.argv`, `env`, `execPath` and `exitCode` are the process's; `console`,
`process.stdout` and `process.stderr` write to its descriptors, in order; `process.stdin` reads
its input (async iteration, `data`, `readline`). It exits as Node exits: when no ref'd timer,
input being read, worker, or ref'd socket or server is left — `beforeExit` first — with
`process.exitCode` or 0; 13 for an ES module whose top-level `await` never settled; 1 for an
uncaught exception or an unhandled rejection. The loop sleeps until the next timer or a wake
from the input, the network or a worker; nothing polls.

Permissions are on unless `-A`: nothing may be read, written, connected to or started without a
flag. The entry file itself may be read (not its folder); a `-e` script needs no grant. Flags
are Deno's with Node's spellings beside them: `--allow-read[=a,b]` / `--allow-fs-read=<path>`,
`--allow-write[=a,b]` / `--allow-fs-write=<path>`, `--allow-net[=host[:port],…]`,
`--allow-worker`, `-A`; `--permission` is accepted and changes nothing. `--use-env-proxy` or
`NODE_USE_ENV_PROXY=1` routes requests through the environment's proxy, as Node 24's do. Node's
`--disable-proto=throw|delete`, `--stack-size=<KiB>` (the script thread's stack, at least
4 MiB), `--no-warnings`, `--experimental-vm-modules`, `--no-addons` and
`--max-old-space-size` (ignored: bound memory with the OS) are accepted, so a Node command line
maps one to one. `src/cli.rs` has the whole rule.

## Checking it

`cargo test` — the web's and Node's globals and modules, a `Worker` answering its page and never
holding it, IndexedDB as `idb` and Dexie use it, WebAssembly called both ways, and isolates
answering without being waited for, stopped from outside, and failing alone.

`cargo test --test corpus -- --ignored` loads real community plugins from the corpus beside the
repository (`../obsidian-corpus/plugins`, or `VERVE_CORPUS`), the host's own modules stood
in for.

`tests/network.rs` has frozen Node 22.21.1 observations for the Node 22.20 API target,
loopback TCP/TLS/HTTP/HTTPS/UDP and deterministic DNS record tests, optional live Node
cross-peer checks (`VERVE_NETWORK_NODE` selects the executable), and explicitly ignored
100 MB backpressure and 1000-live-connection acceptance cases. The latter bounds pending
handshakes to avoid OS SYN-burst limits while retaining all 1000 established connections.
Real npm fixtures
pin `ws` 8.18.3, `axios` 1.12.2, `node-fetch` 2.7.0 and `express` 4.21.2 with their transitive
tarballs. Prepare them outside the checkout with `node tests/fixtures/network/prepare.cjs
<absolute-scratch-directory>`, set `VERVE_NETWORK_NPM` to that directory, then include the
ignored network tests during integration. Preparation is offline and disables lifecycle scripts;
normal tests need neither Node nor npm.

`tests/esm.rs` freezes Node 22 package-resolution and interoperability observations. Its real-package
acceptance fixture pins nanoid 5.1.6, chalk 5.6.2, date-fns 4.1.0 and zod 4.1.12; prepare the fixture
outside the checkout and set `VERVE_ESM_PACKAGES` to that package directory. Run
`cargo test --test network --test esm --test worker -- --include-ignored --test-threads=1` with both
fixture directories set to include the live Node peers, npm packages and stress cases.

ESM relies on the module-loader and synchronous-evaluation changes recorded in `boa/SU.md`, along
with its parser correction for calls to functions named `async`; all seven Boa crates come from
`boa/` together, never a mix of patched and unpatched engine/GC crates.

Networking depends on mio, socket2, Hickory, rustls (ring), native certificate/PEM readers,
x509-parser, httparse and tungstenite, with their purposes recorded in `Cargo.toml`. They take
caret versions, as every dependency here does, so an embedder's workspace can unify them; the lock
file records the versions tested. HTTP/2 is not implemented. DNS uses two fixed workers, never a
thread per connection; dropping a realm cancels queued work without joining a system lookup that
is already in progress. Binary-size/build-time impact and Rust integration results must be
measured by the integration build.

## Integration observations (2026-10-24)

Windows 10 x64, AMD Ryzen 9 7950X (16 cores / 32 logical processors), 127.1 GiB RAM;
Rust/Cargo 1.95.0 and Node 22.21.1, measured on the networking branch before it was folded into
Verve.

- 185 default tests and the separate real-plugin corpus test passed. The concentrated
  acceptance run passed all 29 network, 26 ESM and 2 worker tests, including ignored tests.
- Boa: 320 parser tests passed (one ignored), all 13 module tests and 35 function tests passed.

The two ignored stress tests were also run as separate test processes, observing Windows process
threads and peak working set at a requested 5 ms sampling interval:

| Acceptance | Observed peak threads | Peak working set |
| --- | ---: | ---: |
| 100 MiB loopback upload | 10 | 98.6 MiB |
| 1000 simultaneously established connections | 10 | 73.2 MiB |

The upload additionally asserts that backpressure occurs, all bytes arrive, the JavaScript writable
queue never exceeds 64 KiB, and it drains completely. The connection test retains all 1000 live
connections before closing them; neither test creates one I/O thread per connection. These are dev
build process observations, not release-build size or latency claims.

### Installed compilation targets

`cargo check --lib --target ...` passed for all eleven of these native targets; macOS/Linux checks
used cargo-zigbuild 0.23.0 with Zig 0.15.2, and Android checks used NDK 28.2.13676358 (API 21):

- `aarch64-apple-darwin`, `x86_64-apple-darwin`.
- `aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`, `x86_64-linux-android`.
- `aarch64-unknown-linux-musl`, `i686-unknown-linux-musl`, `x86_64-unknown-linux-musl`,
  `x86_64-unknown-linux-gnu`.
- `x86_64-pc-windows-msvc`.

Two installed targets did not pass. `aarch64-apple-ios` first exceeded cargo-zigbuild's supported
target set; retrying with LLVM clang reached ring's C build but required `xcrun` and an Apple SDK,
which this Windows machine does not have. iOS therefore remains unverified, not a passing check.
`wasm32-unknown-unknown` fails in getrandom without a JavaScript entropy backend; the baseline
checkout without networking fails there too. This is not a claim of browser/Wasm support. Other targets were
not installed and were not checked. Cross-target checks do not establish executable linking or
runtime behavior on those platforms.

### Same-input build and binary observations

The baseline was the runtime before networking (`46c3072`); the integrated variant includes
network and ESM. Both used
exactly the same 11-line program: create a runtime on a 16 MiB-stack thread, evaluate `1 + 1`, and
print the answer. Both printed `2`. Both used the same locally patched Boa checkout, dev profile
(Boa crates, regress and wasmi at opt-level 3), Cargo target directory and shared dependency cache.
Each variant was built once and then immediately rebuilt without changes, with `-j 4`.

| Variant | First measured build | Unchanged rebuild | Executable bytes | Executable size |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 106.687 s | 1.067 s | 33,100,288 | 31.57 MiB |
| Integrated | 97.621 s | 1.429 s | 41,966,592 | 40.02 MiB |

The dev executable grew by 8,866,304 bytes (8.46 MiB, 26.8%). These are single, shared-cache build
observations, not clean-build timings, release sizes, or evidence of a compilation speedup. They
compare the networking integration, not the separate cost of Boa's module/parser changes.

Run the runtime on a thread with room for its stack, as workers and isolates do (16 MB or
more): Boa's interpreter recurses deeply, and a dev build overflows Windows' 1 MB main thread
on ordinary bundles.

## Boa in this repository

`boa/` is Boa's own tree and workspace, brought in with `git subtree` with its history, so Boa's
crates keep their names and an embedder patches them as above. The root workspace excludes it.
To take a newer Boa from upstream:

```sh
git subtree pull --prefix=boa https://github.com/boa-dev/boa.git main
```

Changes to Boa itself are commits under `boa/` like any other; `git subtree split --prefix=boa`
gives them back as a Boa branch to offer upstream.

## The JIT

With the `jit` feature (`verve = { …, features = ["jit"] }`), Boa compiles hot functions to
native code with Cranelift: numeric loops run several times faster, everything else as before.
It is on once built in; `VERVE_JIT=0` or `Runtime::set_jit_enabled(false)` switches it off, and
`VERVE_JIT_THRESHOLD=0` compiles everything first (for testing). How it works, what it covers,
its safety for untrusted code and the plan beyond it: [`docs/jit.md`](docs/jit.md).
