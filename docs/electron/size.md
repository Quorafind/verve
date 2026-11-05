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

## P71: rope strings

`electron/p71-rope-strings`: `cargo build --release --features jit --bin verve` (not stripped, not the `size-report` build) is
27,104,256 bytes against 27,097,600 for `docs/electron-plan` `05f26157` built the same way, +6,656 bytes (+0.02 %): a fifth
string kind in `boa_string` and a changed `JsValue::add`. A rope node is 88 bytes (`size_of::<RopeString>()`, measured), 24
more than a `SequenceString` header.

## P72: typed-array `length` site cache

`electron/p72-jit-typed-length`: `cargo build --release --features jit --bin verve` (not stripped, not the `size-report` build) is
27,105,792 bytes against 27,104,256 for `docs/electron-plan` `345f1645` built the same way, +1,536 bytes (+0.006 %): a module of
about 130 lines in `vm/jit/` and a helper that takes the access's site.

## P74: string follow-ups

`electron/p74-string-followups`: `cargo build --release --features jit --bin verve` (not stripped by `size-report`) is 26,700,800 bytes
against 26,672,128 for `docs/electron-plan` `c91b2938` built the same way, +28,672 bytes (+0.11 %): a table of 256 one-character
`StaticString`s for string iteration (+22,528 with the iterator change; the table is 256 × 72 bytes), the `int32` concatenation
(+3,584) and the rope buffer (+2,560). A rope node is 96 bytes now (`size_of::<RopeString>()`), 8 more than P71's, for the third
state (`Appended`) it needs a tag for.

## P69: calls and construction

`electron/p69-call-cost`: `cargo build --release --features jit --bin verve` (not stripped, not the `size-report` build) is
26,673,664 bytes against 26,672,128 for `docs/electron-plan` `c91b2938` built the same way, +1,536 bytes (+0.006 %): a few
branches in `function_construct`, `prepare` and the class compiler, and one field in the `Vm`.

## P78: the cache for a creating assignment

`electron/p78-constructor-initial-shape`: `cargo build --release --features jit --bin verve` (not stripped, not the `size-report` build) is
26,713,600 bytes against 26,702,848 for `docs/electron-plan` `c28e026c` built the same way, +10,752 bytes (+0.04 %): `vm/inline_cache/add.rs`
(about 230 lines), a few branches in `SetPropertyByName` and its compiled helper, and 40 bytes more in each `InlineCache` (the `AddCache`).

## P76: typed-array elements in compiled code

`electron/p76-jit-typed-elements`: `cargo build --release --features jit --bin verve` (not stripped, not the `size-report` build) is
26,756,608 bytes against 26,717,696 for `docs/electron-plan` `0cc1fa94` built the same way, +38,912 bytes (+0.146 %): the typed path of
`GetPropertyByValue` and `SetPropertyByValue` in the compiler (`compiler/typed.rs`) and the layout probe (`typed_elements.rs`).

## P83: UTF-16 rope appends

`electron/p83-utf16-rope-appends`: `cargo build --release --features jit --bin verve` (not stripped, not the `size-report` build) is
26,759,680 bytes against 26,756,608 for `docs/electron-plan` `2cb3f702` built the same way, +3,072 bytes (+0.011 %): the `AppendedWide`
state and the widening path in `RopeString::append_short`. A rope node keeps its size (the new state holds the same two fields as
`Appended`).

## P10: the launcher and the `app` lifecycle

`electron/p10-split`: `cargo build --release -p verve-gpui --bin electron --features jit` is 33,144,832 bytes against 32,822,272 for
`docs/electron-plan` `4250aaa1` built the same way, +322,560 bytes (+0.98 %); 0.86 MB is left under the 34 MB cap. The first
measurement of the package, before the review's fixes, was 33,238,016 (+415,744); deduplicating the registry, command-line and
user-agent code and moving the manifest reading into one place took 93 KB of it.

Where it is (`cargo bloat --release --crates`, `.text`; the rest of the growth is `.rdata`, `.pdata` and relocations, which
`cargo bloat` does not attribute):

| | .text |
|---|---|
| `verve_electron`'s P10 modules (`api::app` 54.5 KiB with the single-instance lock inlined into `request_single_instance_lock`, `launcher` 4.2, `command_line` 3.0, `process` 2.2) | ~66 KiB |
| `verve_asar` (new in the binary: the overlay P10 installs in every isolate) | 26 KiB |
| `serde_json` (59.5 KiB) and `sha2`/`digest` (37 KiB): already linked through `boa_engine` and `scrypt`; P10 adds the `package.json` and single-instance message parsing, and `verve_asar`'s header and integrity code | shared, part of the delta |
| `windows` / `windows_registry` / `windows-sys` thunks for Known Folders, the registry, the mutex and pipe, memory figures | small, not attributed |

Nothing in the list is a dependency that is not already in the Electron build: the new crate edges are `verve-asar` (a workspace
crate), `serde_json`, `sha2` and `windows-sys` (all in the graph before). The glue is already `opt-level = "s"` with fat LTO, so there
was no cheaper profile to move it to. Left in on purpose: the asar overlay (task section 2) and the registry code (protocol clients,
login items) are the two largest pieces that are P10's own.

## P34: the shell's window management

`electron/p34-gpui-shell`: `cargo build --release -p verve-gpui --bin electron --features jit` is 33,385,472 bytes at the branch's last
code commit (`270aca59`); 0.61 MB is left under the 34 MB cap. The base `docs/electron-plan` `82079241` was not built here: against P10's
33,144,832 the figure is +240,640 bytes, which includes whatever merged between P10 and the base (P84's call frame work among it), so
the base measurement is the one to subtract.

Where P34's own code is (`cargo bloat --release --crates`, `.text`): `verve_gpui` 54.3 KiB in all (`window.rs`, `report.rs`,
`app_region.rs`, `shell.rs`, `platform/windows.rs` and `taskbar.rs`; the pre-P34 shell was a few KiB of it), `gpui_windows` and `gpui`
are GPUI's own and unchanged. The rest of the growth is the `windows-sys` thunks (`user32`, `shcore`, `ole32` imports and their
`.rdata`/`.pdata`/relocation entries, about 60 imports) and the generic GPUI code that the `DomView` canvas and `observe_*`
closures instantiate, which `cargo bloat` does not attribute to the crate that asked for it.

Nothing is a new crate: `raw-window-handle` and `windows-sys` were in the graph (`raw-window-handle` through GPUI), and `objc2` is
macOS-only. The biggest single pieces, by reasoning from the code: `Window::apply` (a match arm per `WindowOp`, about 25 calls into
`platform::windows`), `open` with `gpui_options`, the subclassed window procedure, `displays()` and the hand-written `ITaskbarList3`
vtable. The drag-area subtraction and `diff` are a few hundred bytes each.

## P12: IPC

`electron/p12-ipc`: `cargo build --release -p verve-gpui --bin electron --features jit` is 33,557,504 bytes against 33,302,016 for
`docs/electron-plan` `82079241` built the same way, +255,488 bytes (+0.77 %); 0.44 MB is left under the 34 MB cap. No dependency was
added to the build (`boa_gc` was already linked through `verve`; `verve-electron` only names it for `#[derive(Trace)]`).

Where the package's own code is (`cargo bloat --release --filter verve_electron`, `.text`; `verve_electron` is 226.7 KiB in all, P10's
~66 KiB of it):

| | .text |
|---|---|
| `MainGuest::from_renderer` (the dispatch of `Ipc`/`Invoke`/`SendSync`/`Post`, with `ipc_main::receive` and the event builders inlined into it) | 16.4 KiB |
| `RendererGuest::new` (now also installs `ipcRenderer`; the `IpcRenderer` class and its method table are inlined, the rest was there) | share of 16.6 KiB |
| `ipc_main` (`IpcMainImpl` class and handle table, `sendSync` reply, `invoke` answers) | ~20 KiB |
| `ipc_renderer` (`send`, `invoke`, `sendSync`, `postMessage`, hearing) | ~14 KiB |
| `message_port_main` (`MessageChannelMain`, `MessagePortMain`, the beat that hears them) | ~12 KiB |
| `contents_ipc`, `web_frame_main` | ~7 KiB |
| `verve::messaging` remote ports and `verve::clone`'s error and "alive" checks | ~12 KiB |

The rest of the growth is `.rdata`, `.pdata` and relocations, which `cargo bloat` does not attribute. The per-module figures are
sums of the symbols `cargo bloat` lists, rounded; the dispatch in `MainGuest::from_renderer` is the biggest single piece and was not
shrunk.
