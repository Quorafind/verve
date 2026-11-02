# Verve performance baseline (runtime parts only)

Measured 2026-11-03 against Node 22.21.1 and Electron 39 (`ELECTRON_RUN_AS_NODE=1` for JS-only comparisons).

**This is a baseline of the runtime, not of an app.** Verve has no layout or paint yet: its `electron.exe` opens a GPUI window and draws text. "Window ready" below is a process-start-to-window-created figure, not first paint, and it is *not* the same event in the two products (see section 2). Nothing here says how a real app would compare end to end.

## Machine, conditions, builds

- Windows 10 Enterprise LTSC 2021, Ryzen 9 7950X (32 threads), 128 GB RAM. Other processes were running: CPU load read 40% just before the main run and 5-15% after. One earlier Node/Electron pass was taken while my own `cargo` build was running (Node `-e 0` read about 200 ms against 72 ms idle); it was discarded. All figures below come from passes with no build of mine running.
- Worktree `F:\dev\sandbox-v2\verve-e-perf`, detached at `origin/electron/p02-skeleton` (6b775ce4), clean, nothing committed.
- Builds (release, repo profile, `CARGO_BUILD_JOBS=6`):
  1. `cargo build --release --bin verve --no-default-features --features wasm,jit`. This is `verve.exe`, 23,577,600 bytes. The interpreter column is the same binary with `VERVE_JIT=0`, so no second build was needed.
  2. `cargo build --release -p verve-gpui --bin electron --features verve/jit`. This is `electron.exe`, 27,602,944 bytes. **It has no JIT.** `verve-gpui` only has `verve` as a dev-dependency, so `verve/jit` does nothing here (`cargo tree` confirms it, and the binary has no cranelift or `VERVE_JIT` strings). `verve-electron` pulls verve with `wasm` only, so there is currently no way to switch the JIT on in the `electron` binary from the command line.
  3. `cargo test --release -p verve-dom --test css cascading_five_thousand_nodes_under_a_tailwind_sheet -- --ignored --nocapture`.
- Electron 39: `...\scratchpad\electron39\node_modules\electron\dist\electron.exe`.
- Harness: `runm.exe` (source `runm.cs`). It starts a process, polls the whole process tree every 8 ms via Toolhelp and `GetProcessMemoryInfo`, and reports wall time to exit and the highest sum of working sets over the tree ("max WS"). Wall time carries about +-10 ms of polling and process-creation noise. A process that lives under about 50 ms cannot be sampled for memory, so startup memory is taken from the idle runs. The harness also kills stragglers. No verve or electron process of mine was left running; the three `electron.exe` processes alive at the end belong to another app under `AppData\Local\electron\app-1.0.0`.
- Benchmark sources: `js\bench.js`, `js\idle.js`, `js\tsload.js`, `esm\main.mjs`, `app\main.js`, `app-verve\main.js`. Drivers: `driver.sh`, `driver2.sh`, `driver3.sh`, `driver4.sh`. Raw results: `res\`. Summary output: `res\summary.txt`.
- Statistics: best / median of N runs (N is given per table). Spread is noted where it matters.

## 1. Startup (empty script, wall time to exit, ms, N=11)

| command | best | median | worst | vs Node (median) |
|---|---|---|---|---|
| `node -e 0` | 60 | 73 | 104 | 1.0x |
| `ELECTRON_RUN_AS_NODE=1 electron -e 0` | 93 | 114 | 150 | 1.6x |
| `verve run -e 0` (JIT build) | 28 | 39 | 91 | 0.5x |
| `verve run -e 0`, `VERVE_JIT=0` | 27 | 28 | 61 | 0.4x |

Verve starts in about 28 ms, 2.6x faster than Node and about 4x faster than Electron-as-Node. The JIT build's median of 39 against 28 is run-to-run noise (best 28 vs 27). The ~28 ms floor is the figure to trust.

## 2. App startup to a window

The same minimal app on both: create a `BrowserWindow`, `loadURL('data:text/html,<h1>hi</h1>')`, write `Date.now()` to a file at the "ready" event, then quit/close. N=7. The apps differ because P02 has no `app` object:

- Electron 39 stamps at `webContents` `did-finish-load` and then calls `app.quit()`.
- Verve stamps when `loadURL()`'s promise settles. P02 settles it immediately, before the page loads. So Verve's "ready" is **window created plus load requested**, not page loaded. It flatters Verve and cannot be compared on equal terms. Wall time to exit, which includes everything, is the more honest number. Verve closes with `win.close()`, and `'closed'` calls `process.exit(0)`.

| (hold 0 ms, then quit) | ready best / median (ms) | wall to exit best / median (ms) | procs | max summed WS (MB) best / median |
|---|---|---|---|---|
| Electron 39 | 536 / 558 | 608 / 642 | 4 | 291.5 / 295.3 |
| Verve `electron.exe` (no JIT) | 248 / 254 | 387 / 402 | 1 | 64.1 / 65.9 |
| ratio Verve / Electron | 0.46x (not like-for-like) | 0.64x | | 0.22x |

Memory with the window held open 1 s before quitting (N=7):

| | max summed WS (MB) best / median | sum of per-process peaks (MB) median | procs |
|---|---|---|---|
| Electron 39 | 298.4 / 300.4 | 302.1 | 4 (main, GPU, network utility, renderer) |
| Verve `electron.exe` | 71.0 / 71.2 | 71.3 | 1 |
| ratio | 0.24x | 0.24x | |

Close-to-exit costs Verve about 70-150 ms. Verve's page is a thread in the same process, which is why it has one process; there is no separate renderer or GPU process yet. Part of the memory gap is the work Verve does not do yet (no layout tree, no paint, no compositor).

## 3. JS throughput (`js\bench.js`, ms, one process per test)

Each test runs once as a warm-up, then once timed with `performance.now()`. 5 processes per test and engine. Sizes were cut for Verve's sake (see "Findings"), so the absolute numbers are small by design. The ratios are what matter. Medians are shown; the best-to-worst spread was under 10% except where marked.

| test | Node median (best) | Verve JIT median (best) | Verve interp median (best) | JIT / Node | interp / Node | interp / JIT |
|---|---|---|---|---|---|---|
| `fib(30)` | 5.5 (5.4) | 297.9 (296.8) | 368.9 (354.5) | 54x | 67x | 1.24x |
| allocation loop (300k objects) | 9.1 (8.9) | 277.4 (270.6) | 291.4 (279.7) | 30x | 32x | 1.05x |
| string `+=`, 20k appends | 0.6 (0.5) | 21.3 (21.2) | 33.0 (21.3) | 36x | 55x | 1.0x (interp median is noise, best equal) |
| `split`/`join`, 20k parts | 2.6 (2.6) | 541.8 (541.5) | 544.4 (542.1) | 208x | 209x | 1.00x |
| regex (`matchAll` + `replace`), 5k lines | 1.6 (1.5) | 462.4 (462.0) | 461.9 (459.0) | 289x | 289x | 1.00x |
| `JSON.parse`+`stringify` x2, 5.4 MB | 141.8 (138.4) | 2568.5 (2519.1) | 2572.7 (2520.7) | 18x | 18x | 1.00x |
| `sort` 1M numbers, comparator | 168.6 (167.2) | 2606.3 (2574.7) | 2556.3 (2514.9) | 15x | 15x | 0.98x |
| `Map`/`Set` churn, 20k | 4.6 (4.4) | 405.0 (392.9) | 401.1 (394.3) | 88x | 87x | 0.99x |
| class/method-call OO loop, 1M | 2.9 (2.8) | 1208.5 (1176.7) | 1216.8 (1175.6) | 417x | 420x | 1.01x |
| `Float64Array` math, 1M x 5 | 4.0 (3.9) | 3261.9 (3188.4) | 3415.0 (3374.2) | 815x | 854x | 1.05x |

Verve is 15x to 850x behind V8 on these, with a typical gap of 30-300x. The JIT closes almost none of it on this suite: at best 1.24x on `fib` (call-heavy), roughly 1.05x on allocation and typed arrays, and nothing elsewhere. Most of these tests are dominated by builtins (sort, JSON, regex, `split`, `Map`), which the JIT does not touch. `oo` and `typed` are loops with property or element access, where `docs/jit.md` reports up to 10x on micro-loops; but here the hot loop is inside one top-level call of a function invoked only twice, and the benchmark body also does allocation and getters. `VERVE_JIT_THRESHOLD=0` did not change `fib`, `oo` or `typed` (298 / 1186 / 3180 ms).

### Findings: super-linear built-ins (the main reason the sizes above are small)

I first sized the suite like Node's, and Verve ran for minutes, so I sized down and probed scaling (interpreter; the JIT build behaves the same):

- `String.prototype.split('a')` is quadratic: 5k parts 32 ms, 10k 133 ms, 20k 555 ms, 40k 2,297 ms. A 200k-part split did not finish in 120 s (Node: about 26 ms).
- `Map.prototype.delete` and `Set.prototype.delete` are quadratic: deleting every other key of N, N=10k 42 ms, 20k 181 ms, 40k 878 ms. At 300k the test did not finish in minutes.
- Regex over generated text: 5k lines 470 ms, 20k lines 15,000 ms. Four times the input took 32 times as long. I did not isolate whether `matchAll` or `replace` is at fault.
- String `+=` and `Array.prototype.join` are fine (20k appends 21 ms; join of 40k parts 3.5 ms).

These are bugs to fix, not the engine's linear speed. They matter more than the 30-300x constant factor because real programs hit them at moderate sizes.

## 4. Memory (max summed working set, MB)

| scenario | Node | Electron-as-Node | Verve | Verve / Node |
|---|---|---|---|---|
| idle, holding a 3 s timer (N=5) | 34.3 | 38.0 | 9.6 | 0.28x |
| `require('typescript')` 9.1 MB CJS bundle (N=7, median) | 58.5 (sum of peaks 67.6) | 65.0 | 228.2 | 3.9x |
| same bundle as one ES module, no cache (N=7) | 67.6 | n/a | 133.1 | 2.0x |
| same, warm code cache | | | 73.1 | 1.1x |
| one window, 1 s hold, all processes summed (N=7) | Electron 39: 300.4 | | 71.2 | 0.24x |

Verve idles at under a third of Node's memory, and a window costs a quarter of Electron's. Loading a big bundle costs Verve 2-4x Node's memory, except in the warm code cache case.

## 5. Module loading: `typescript.js` 5.9.3 (9.1 MB, `F:\dev\sandbox-v2\NiceEval\node_modules\typescript\lib\typescript.js`)

Time to `require` (or `import`) it, measured inside the script. N=7 (warm cache N=9).

| case | best (ms) | median (ms) | vs Node (median) |
|---|---|---|---|
| Node 22, CJS `require` | 116 | 118 | 1.0x |
| Electron-as-Node, CJS `require` | 100 | 101 | 0.9x |
| Verve, CJS `require`, JIT build | 2094 | 2167 | 18x |
| Verve, CJS `require`, `VERVE_JIT=0` | 2004 | 2051 | 17x |
| Node 22, same source as ES module | 108 | 114 | 1.0x |
| Verve, ES module, no cache | 933 | 946 | 8.3x |
| Verve, ES module, **cold** cache (compiles and writes a 17.97 MB pack) | 1274 | 1291 | 11x |
| Verve, ES module, **warm** cache | 170 | 176 | **1.5x** |

- Total process wall time for the warm-cache case is 231 ms median against Node's 209 ms.
- The code cache is for ES modules only. `require()` of a CJS file never writes a pack (the directory stayed empty), so a big CommonJS bundle is the slow 2.1 s case.
- Cold costs 36% more than uncached (it compiles every function eagerly). Warm is 5.4x faster than uncached.
- `--code-cache=<dir>` requires a private directory. A fresh scratch directory is refused with "not private to this user", and the cache is silently not used (a message is printed, the run continues). I made it private with `icacls /inheritance:r /grant:r <me>:(OI)(CI)F`.
- `verve run` needs `-A` to read a file outside its working directory (`ERR_ACCESS_DENIED` otherwise).
- The ES-module version is `typescript.js` wrapped in a `createRequire` header and `export default module.exports`. It is the same code, not a different bundle.

## 6. DOM and CSS (Verve only, release)

`cascading_five_thousand_nodes_under_a_tailwind_sheet`, 5 runs of the test (the first one built the test binary). It prints its own best-of-N inside each run.

| metric | value (range over the 5 runs) |
|---|---|
| nodes / rules | 7,505 nodes, 4,004 rules |
| first `document.update()` (style a 5,000-element page) | 172-196 ms |
| `update()` after 500 elements change class | 48-51 ms |
| cascade alone (best of 5 inside the run) | 175-195 ms |
| stylesheet parse (best of 3) | 7 ms |
| selector index (best of 3) | 2 ms |

The cascade is about 25 us per node. A full restyle of a 5k-element page takes about 11 frames at 60 Hz (180 ms), and an incremental restyle of 500 changed elements takes about 50 ms (3 frames). That is not interactive-grade yet for large pages, but layout and paint are not included either way. The React 18 1000-row table render was **not run**: it needs a new test and a further release build, which would have exceeded the three-build budget.

## 7. Disk size

| | bytes | MiB |
|---|---|---|
| Verve `electron.exe` (one file, no JIT) | 27,602,944 | 26.3 |
| Verve `verve.exe` (wasm + jit, no typescript/npm/dns/x509/brotli) | 23,577,600 | 22.5 |
| Electron 39 `dist` folder, total | 341,555,344 | 325.7 |
| Electron 39 `electron.exe` alone | 210,896,896 | 201.1 |

Verve's `electron.exe` is 12.4x smaller than the whole Electron dist, and 7.6x smaller than the Electron exe alone. It also does much less.

## What this means

Verve is already ahead of Electron, and often Node, where it does less work or none:

- **Startup:** 28 ms, against Node's 73 and Electron-as-Node's 114.
- **Memory:** 9.6 MB idle, against Node's 34, and 71 MB for one window against Electron's 300 across 4 processes.
- **Size:** 26 MiB, against a 326 MiB dist.
- **Window creation to exit:** 400 ms against 640 ms, with the caveat that Verve's "ready" fires before the page loads and nothing is laid out.
- **Warm ES-module loading:** a 9 MB bundle is within 1.5x of Node.

Verve is very far behind on raw JS throughput. Typical ratios are 30x to 300x slower than V8, with outliers up to 850x (typed-array math) and 15-18x in the best builtin-bound cases (sort, JSON). The JIT, as measured on this suite, barely helps: 1.24x on recursive calls and about 1.05x elsewhere. Boa's builtins, not its bytecode loop, dominate here, and the JIT does not touch builtins. Its documented wins (3-12x) are for tight loops over property and element access, which these tests only partly contain. Large CJS bundles are the other weak spot: 2.1 s to load typescript.js (18x Node) and 228 MB of memory, since the code cache does not apply to `require`.

The most actionable items are the super-linear `split`, `Map`/`Set` delete and regex scans, getting CJS modules into the code cache, and a way to turn the JIT on in the `electron` binary.

## Caveats

- No layout or paint exist yet. The window figures measure window creation plus text drawing, and Verve's "ready" is not equivalent to Electron's `did-finish-load`. Compare wall time to exit, and treat even that cautiously.
- The `electron.exe` measured has no JIT (see Builds). App-JS throughput inside it equals the "Verve interp" column.
- The machine was shared (40% CPU before the run, 5-15% after). Medians and bests are shown; differences under about 10% are within noise. Run-to-run noise on sub-100 ms startup figures is large (see the worst column).
- Harness timing resolution is about +-10 ms. Startup memory for short runs could not be sampled, so idle runs stand in.
- Everything is one machine and one OS, release against release. Electron 39 is the stock distribution, with its GPU and utility processes counted in memory. Node was a stock Windows build.
- JS suite sizes were reduced for Verve (split, Map/Set and regex work in particular) because of the super-linear builtins. At Node-sized inputs those would run for minutes, so the ratios in section 3 understate the gap for such workloads.
- The React 1000-row render was not measured.
