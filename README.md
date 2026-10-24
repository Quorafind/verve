# runtime-x

A JavaScript runtime for live plugins, in Rust.

- **The engine** is [Boa](https://github.com/boa-dev/boa), an interpreter written in Rust — here
  rebon's branch of it (`boa-rebon`, `main`): 0.22.0 with parser fixes, a cache of compiled
  regular expressions, a collector that frees lazily, and an interrupt handle. Its `SU.md` lists
  them.
- **The runtime** (`RuntimeX`) gives a plugin's `main.js` what it expects of a browser and of
  Node, every one a Rust function: CommonJS `require`, timers on a clock the host drives,
  microtasks, `console`, `process`, `Intl`; `atob`/`btoa`, `TextEncoder`/`TextDecoder`,
  `URL`, `crypto`, `structuredClone`, `performance`, `AbortController`, `Blob`/`File`, `Worker`
  (a realm on a thread of its own), `indexedDB` (kept on disk as JSON), `WebAssembly` (run by
  [wasmi](https://github.com/wasmi-labs/wasmi)); Node's `Buffer`, `path`, `util`, `events`, `os`,
  `url`, `fs`.
- **Modules** are CommonJS as Node has them: every built-in answers to its bare name and to
  `node:` before it; a plugin's own files resolve as Node resolves them (extensions, folders,
  `package.json`'s `main`, `node_modules`); `require.cache`, `require.resolve`, `module`, and
  `require('module')` (`createRequire`, `builtinModules`, `isBuiltin`). Node's own modules are
  made the first time they are required.
- **`process`** is Electron's renderer's: an `EventEmitter` with `versions.node` and
  `versions.electron`, `env`, `cwd`/`chdir`, `hrtime`, `nextTick`, `memoryUsage`, `emitWarning`,
  `stdout`/`stderr` writing to the console; `exit` and `abort` are noted and end nothing.
- **Node's classes extend every way**: `class X extends EventEmitter`, and the older
  `function X() { EventEmitter.call(this) }` with `util.inherits` — each class is a `Proxy` of
  its native constructor, whose `apply` trap sets up the caller's own object.
- **Isolates** (`runtime_x::isolate`) put a runtime — and whatever the embedder serves with it —
  on a thread of its own. The embedder sends jobs and reads back events and snapshots, and never
  waits: a script busy for seconds holds only its own thread. A panic ends only its own isolate,
  and a script that will not return is stopped from outside (`Isolate::interrupt`), uncatchably.
  What runs in one is the embedder's: implement `isolate::Guest`.

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

`cargo test` — the web's and Node's globals, a `Worker` answering its page and never holding it,
IndexedDB as `idb` and Dexie use it, WebAssembly called both ways, and isolates answering without
being waited for, stopped from outside, and failing alone.
