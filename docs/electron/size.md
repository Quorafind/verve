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
