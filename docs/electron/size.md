# Binary size

<!-- size-baseline verve 35812352 -->

`scripts/size-report.ps1` / `.sh` build one configuration and print its size against the
baseline marked above (the pre-P01 release `verve.exe`, 35,812,352 bytes, no profile at all).
Every PR that adds a dependency pastes its output.

All numbers: Windows x64, Rust 1.95, `cargo build --release --bin verve`, stripped `.exe`,
measured 2026-11-02. "Core" means the `verve` bin with the feature set named; no GPUI yet.

## Where the bytes are

`cargo bloat --release --crates` on the default features (`.text` 14.6 MiB of a 20.2 MiB file; the
rest is mostly `.rdata`, 4.4 MiB, and `.pdata`, 0.6 MiB):

| crate | .text | | crate | .text |
|---|---|---|---|---|
| boa_engine | 3.1 MiB | | oxc_* (all) | ~1.1 MiB |
| verve | 2.6 MiB | | brotli (encoder) | 316 KiB |
| std | 2.4 MiB | | hickory/tokio/futures | ~320 KiB |
| boa_parser | 1.5 MiB | | rustls | 397 KiB |
| boa_ast | 459 KiB | | regress | 251 KiB |
| wasmi + wasmparser | ~720 KiB | | x509-parser | 66 KiB |

## Feature switches

Default features are unchanged in behaviour: `npm, typescript, wasm, dns-records, x509, brotli`.
Sizes are `.text` from the table above (the whole-binary deltas are in the next sections).
Switched off, the API still exists where Node/Electron code probes for it and says
`ERR_NOT_SUPPORTED` / `ENOSYS`.

| feature | drops | .text saved | when off |
|---|---|---|---|
| `typescript` | oxc_* | ~1.1 MiB | `module.stripTypeScriptTypes` throws `ERR_NOT_SUPPORTED` |
| `wasm` | wasmi, wasmparser | ~0.7 MiB | no `WebAssembly` global |
| `dns-records` | hickory-resolver, tokio | ~0.3 MiB | `resolve4/6/Txt/Srv` fail `ENOSYS`; `lookup` stays |
| `x509` | x509-parser | ~66 KiB | `getPeerCertificate` has `raw` and fingerprints only; hostname check falls back to `subjectaltname` text |
| `brotli` | brotli encoder | ~0.3 MiB | `zlib.brotliCompress*` throws `ERR_NOT_SUPPORTED`; decoding (`fetch` `br`) stays |
| `npm` | ureq, tar | small (not in top 45) | no installer |
| `jit` (off by default) | adds Cranelift | **+5.9 MB** whole binary | see below |

`intl-full` from the brief was not made: `src/intl/` is 81 KB of source and Boa is built without
its own `intl`, so there is no big table to switch off.

## Profile comparison

Core with the Electron feature set (`--no-default-features --features wasm`), one build each.
Speed: `verve run bench.js`, a script of `fib(27)`, 200k object-key updates, a
300k-element `sort` with a comparator, a 50k-character string build and a regex replace, wall time
by `Date.now()` in the script, three runs each (interpreter, JIT off).

| configuration | size | time |
|---|---|---|
| everything opt-level 3 (`package."*"` = 3) | 20.76 MB | 858-888 ms |
| `*` = "s", verve = 3 (first draft) | 20.38 MB | 841-854 ms |
| `*` = "s", Boa crates "s" | 18.50 MB | 1037-1085 ms (+24%, rejected) |
| **`*` and verve = "s", Boa/regress/wasmi/cranelift 3 (chosen)** | **17.77 MB** | 886-905 ms (+5%) |

The dependencies at "s" instead of 3 save only 0.4 MB, but Verve's own glue at "s" saves 2.6 MB for
about 5% on a benchmark that is mostly Boa. Boa at "s" costs a quarter of the speed, so it stays 3.
The limit was 10%.

Other profile settings: `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`; `panic` stays
`unwind` (isolates `catch_unwind`). `[profile.dist]` inherits release.

## JIT

| | size | `examples`-style loop, 20k calls of a 1000-iteration function |
|---|---|---|
| Electron set | 17.77 MB (20.38 MB at the first draft) | ~790 ms |
| + `jit` | 26.31 MB at the first draft | ~97 ms |

The JIT is 5.9 MB for 8x on hot numeric loops. The user decided on 2026-11-02 that `electron` ships with it,
under a 34 MB cap (README, "目标").

### `electron.exe` (`P63`, measured 2026-11-03)

`cargo build --release -p verve-gpui --bin electron`, Windows x64, stripped, Rust 1.95,
`CARGO_BUILD_JOBS=6`. `scripts/size-report.sh` builds only the root `verve` package and has no
`-p`, so the two binaries were built directly and measured with `wc -c`.

| build | bytes | MiB |
|---|---|---|
| `electron.exe`, no `jit` | 27,602,944 | 26.32 |
| `electron.exe`, `--features jit` | 32,971,264 | 31.44 |
| difference | +5,368,320 | **+5.12** |

The `jit` feature adds 5,368,320 bytes here — 5.12 MiB, which is 5.37 MB in the other unit —
against the 5.9 MB it added to the core table above. In the same unit the two are close (5.37 MB
against 5.9 MB) but not equal, and why they differ was not measured. Cranelift is in the JIT
build only: 81 `cranelift` and 21 `regalloc2` occurrences in the image, and none in the other.

The environment variables are built with the feature too, so they say which build is which:
`VERVE_JIT_THRESHOLD` and `BOA_JIT_THRESHOLD` once each, and `VERVE_JIT` and `BOA_JIT` once each
as a whole literal (a search finds each of those twice, the second hit being the prefix inside the
`_THRESHOLD` name). The build without the feature has none of the four: the only reader,
`jit_from_env`, is `#[cfg(feature = "jit")]`.

The build without the feature is the binary it was before this package: 27,602,944 bytes, the
same length as the build taken before the change, and the two images differ in 12 bytes only —
the PE `TimeDateStamp` at 128 (and the copy of it in the debug directory at 23,319,824), and the
first 8 bytes of the `RSDS` CodeView GUID at 23,319,852. That is the linker's own stamping, not a
code change: building the same unchanged tree twice in a row gives a pair of images that differ in
exactly those 12 bytes and nowhere else. Everything else is byte for byte identical, so the
feature costs nothing when it is off.

### Iterators without result objects (`P70`, measured 2026-11-04)

`verve.exe` built by `cargo build --release --features jit --bin verve` (default features plus
`jit`), the tree before the package against the tree after it: 27,097,600 bytes to 27,106,816, which
is +9,216 bytes (+9.0 KiB, +0.03 %). The build without the `jit` feature was not measured; its share
is the interpreter's half (the stepper and the in-place handlers), the three fast helpers and their
compiler cases being `jit` only.

## Summary

| step | verve.exe |
|---|---|
| baseline (no profile, default features, no GPUI) | 35.81 MB |
| first-draft profile (opt 3), default features | 24.02 MB |
| first-draft profile (opt 3), Electron set | 20.38 MB |
| **final profile, Electron set (`wasm` only)** | **17.77 MB** |
| final profile, default features | ~21.2 MB (from cargo-bloat's build, not the stripped binary) |

Recommended Electron combination for the core: `--no-default-features --features wasm`
(`jit` per P42). 17.77 MB is above the 14 MB core target of the brief; against the plan's budget
(core <= 10 MB, GPUI 6, rest 4) the core is 7.8 MB over, so the budget table needs revisiting
once GPUI is measured. Candidates still open: `wasm` off (-0.7 MB), the 4.4 MiB `.rdata`
(not attributed; panic locations and string tables), `std` at 2.4 MiB, `opt-level = "z"`,
and loading rarely used Node modules on demand.
