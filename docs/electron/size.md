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

## P20: the page loader

`electron/p20-page-loading`: `cargo build --release -p verve-gpui --bin electron --features jit` is 33,731,072 bytes against
33,302,016 for `docs/electron-plan` `82079241` built the same way, +429,056 bytes (+1.29 %); the first measurement, before the
review's fixes, was 33,638,912 (+336,896), and after the first batch 33,566,720 (+264,704). The second batch added the write
insertion point, the attribute-driven handler/script preparation, the stylesheet ordering and blocking rules, the token-reading
module scanner and the import map's spec checks (+164,352). The fixes took the module-graph bookkeeping into one table (`Load::mods`) and out of
three parallel maps, and the `file:`/navigation checks replaced a longer ad-hoc one.

Where it is (`cargo bloat --release --crates --filter verve_dom`; `.text`): `Document::pump_loads` with the load state machine
inlined 42.7 KiB, `html::Parser::scan` and its tag/element functions (`scan` was split into several functions; the 13.9 KiB figure was for the earlier single one), `load::script::connected` 8.5 KiB; the
rest is `ElectronLoader` and the `RendererGuest`/`Hub` navigation code (small), the kernel's `ImportMap` and web resolution
(`src/esm.rs`, `src/import_map.rs`, a few KiB) and `.rdata`/`.pdata`/relocations that `cargo bloat` does not attribute. No new
crate edge: `url`, `serde_json` and `boa_gc` were in the graph. Left in: the module-graph scanner (`load/modules.rs`) and the
`@import` handling are the two pieces that are P20's own and have no cheaper form; nothing else was cheap to cut.

Third review's fixes (rebased on `docs/electron-plan` `4ffdcaa6`, base 33,653,760 bytes): the tip before them built to 34,089,472 (+435,712); `with_dom<R>`'s tail (the
`joined` collect, `connected`, `page_changes`, the `written` loop, the observers' and custom elements' hooks) was copied into each of its ~137 callers, and moving it into one non-generic
`#[inline(never)] settle` together with the other fixes (the best-effort dynamic-import prefetch, the generation checks, `order_sheets` as an in-place insertion sort instead of a
`sort_by` instantiation) brought it to 33,833,472, -256,000 bytes against the tip before and +179,712 (+0.53 %) against the base. (`cargo bloat --filter verve_dom` did not name the
functions under that filter, so the split is not itemised.)

Fourth review's fixes (the `file:` rule decided by the document, `Isolate::retire` split into its own kernel commit, the captured `Response.prototype.url` getter): 33,838,592 bytes, +184,832 (+0.55 %) against the base and +5,120 against the third round.



## P14: system integration A

`electron/p14-system-a`: `cargo build --release -p verve-gpui --bin electron --features jit` is 34,126,336 bytes against 33,839,104 for
`docs/electron-plan` `125d2d8c` (the figure the coordinator gave; the base was not rebuilt), +287,232 bytes (+0.85 %); the task allowed 1.2 MB and
the aim was 600 KB or less. The cap (35,651,584) leaves 1.53 MB.

No new crate edge: `image` is now declared once in `[workspace.dependencies]` with the feature list `verve-dom` already used (`bmp, gif, ico,
jpeg, png, webp`), so `nativeImage` adds no codec; `getrandom` was in the graph through `verve`. `rfd`, `arboard` and the `windows` crate were
not added: the clipboard, DPAPI, `ShellExecuteW`, the recycle bin, the idle time and power queries are `windows-sys` calls (feature flags
only), and the shell's dialogs are GPUI's own prompts. The per-piece split was not measured (`cargo bloat` was stopped by the coordinator
to spare the shared machine); the whole figure is the only measurement, to be itemised by the main session if wanted.

## P21: preload, contextBridge and isolated worlds; the P13/P14/P21 batch

`docs/electron-plan` `60e86fd7`, with P13's first half, P14 and P21 merged: `cargo build --release -p verve-gpui --bin electron --features jit`
is 34,431,488 bytes against 33,839,104 for `125d2d8c` before the batch, +592,384 bytes (+1.75 %). The cap (35,651,584) leaves 1.22 MB.

P13's first half measured +110,592 and P14 +287,232 on their own branches, which leaves about 194,560 bytes (+0.57 %) for P21. P21 was not
built on its own, to spare the shared machine. It adds no crate and changes no manifest: the worlds, the bridge's copy, the per-world classes
and the mirrored events are kernel and `verve-dom` code.

## P13: the second half (`webContents`)

`electron/p13-webcontents` at its tested tip (the tree `docs/electron-plan` has after the merge): `cargo build --release -p
verve-gpui --bin electron --features jit` is 34,679,808 bytes against 34,431,488 for `8e259992` (the batch above), +248,320 bytes (+0.72 %);
the brief aimed at about 250 KB. The cap (35,651,584) leaves 0.97 MB. No crate or manifest changed: the class, the history, the event
table, `window.open`'s side of the main process, the input hooks and the heartbeat are all `verve-electron` code over what was there.

## R1: the refactor gate

Two branches, measured each at its tested tip with `cargo build --release -p verve-gpui --bin electron --features jit`:

- `electron/refactor-api` (refactor A, `verve-electron`: `msg/` and `hub/` split by domain, one way to settle, report, check and
  send): 34,676,224 bytes against 34,679,808 for `35ab2dd0` (P13's second half), −3,584 bytes.
- `electron/refactor-r1-toolkit` (refactor B, the kernel and `verve-dom`: the 53 toolkit copies and `verve-dom`'s legacy helpers
  replaced by `verve::native` and `verve::encoding`): 34,617,856 bytes against 34,676,224 for `3923d2e0` (refactor A merged; B merged as `7936804e`),
  −58,368 bytes.

Together −61,952 bytes (−0.18 %). The cap (35,651,584) leaves 1.03 MB. Neither branch adds a crate or changes a manifest;
the bytes come from builders and helpers that each existed several times over and now exist once.

## P15: Menu, MenuItem, Tray and globalShortcut

`electron/p15-menu-tray` at b3b39ebf (merged as `88d46a4e` after seven small fixes that move no measurable size): `cargo build --release -p verve-gpui --bin electron --features jit` is
35,122,688 bytes against 34,617,856 for `b1a27600`, +504,832 bytes (+1.46 %). The package's budget was 300 KB, then the task's own
400 KB once the user raised the cap to 36 MB (37,748,736) on 2026-11-07. The coordinator accepted the overrun: each of the last two fix
rounds took only 8–9 KB off, and the cap leaves 2.6 MB after P15.

Where it goes (`cargo bloat --release --crates` and per function, unstripped, first round):
- **The three new crates are small.** muda is 35 KiB, and tray-icon and global-hotkey are a few KiB each. `windows-sys 0.59`, a second
  copy that every published global-hotkey requires, has no code of its own.
- **The rest is P15's own code in `verve_electron` and `verve_gpui`**, about 4,800 lines. Named functions come to about 140 KiB. The
  largest are:
  - `menu_state::read_nested`, 11.9 KiB;
  - `MenuItem::init`, 8.1 KiB;
  - `menu::sort::sort_entries`, 8.0 KiB;
  - `click_item`, `popup`, `build_from_template` and `Shortcuts::apply`, 5–7 KiB each.
  The remainder is per-type generic copies, closure glue, the role tables and the unwind tables.
- **Already cut:**
  - one `Keyed<V>` map instead of a `BTreeMap<u32, T>` per kind;
  - static role labels;
  - no `Debug` formatting on release paths;
  - merged helpers.

Candidates for P42: split `read_nested` and `MenuItem::init` so they stop inlining their helpers, and drop the headless shell's menu
model from builds that never run `--headless`.

The GPUI fork's `image` features compile nine decoders Verve never uses. exr and tiff alone are 174 KiB of `.text`, and the user's fork
patch (2026-11-07) removes them. That is not counted here, because it is in the base too.

## P30: box layout and drawing (verve-layout)

`electron/p30-boxes` at its tested tip (72a4a473; merged as `e0ec4398` after P15): `cargo build --release -p verve-gpui --bin
electron --features jit` is 35,085,312 bytes against 34,617,856 for `b1a27600`, +467,456 bytes (+1.35 %). The budget was 250 KB,
re-set to 450 KB once the cost below was known, and the 17 KB over it was accepted. `docs/electron-plan` with P15 and P30 merged is
35,588,096 bytes; the cap (37,748,736) leaves 2.16 MB.

Where it goes (`cargo bloat --release --crates`, unstripped, before the rework):
- **taffy grows from about 196 KiB to 372 KiB.** verve-layout's `TaffyTree<()>` compiles block, flex, grid and float a second time
  beside GPUI's `TaffyTree<NodeContext>`. That can't be shared without verve-layout depending on GPUI, which the architecture forbids.
- **verve-layout itself is 88.7 KiB**: the CSS readers, the tree, placement and painting.
- **verve-gpui grew by about 35 KiB**: the element, plan, rasters, wheel and screenshot.
- **png's encoder came in through tiny-skia's `png-format`.** The rework encodes with `image`'s `PngEncoder` instead, and reuses
  verve-dom's blur, which took about 22 KB off.

## P43: logs, uncaught errors and the DevTools text dump (phase 1)

`electron/p43-devtools` rebased on P30 (784fb499; merged after two small fixes to the dump and its test): `cargo build --release
-p verve-gpui --bin electron --features jit` is 35,659,264 bytes against 35,588,096 for `df018328` (P15 and P30 merged), +71,168
bytes, within the 100 KB budget. The filter table, the colours and the parent-console attach are hand-written, with no `tracing`
or `env_logger`.

## P31: inline layout and text through GPUI's text system

`electron/p31-inline-text` at its tested tip (713e900b; merged as `7d8fb819` after P43): `cargo build --release -p verve-gpui --bin
electron --features jit` is 36,048,384 bytes against 35,588,096 for `df018328` (P15 and P30 merged), +460,288 bytes (+1.29 %). The
budget was 300 KB, re-set to 350 KB after the first measure (+445,440). The 110 KB over it was accepted so that P16 and P31 could
merge, and the cut is left to P42. `docs/electron-plan` with P15, P30, P43 and P31 merged is 36,118,016 bytes, +458,752 on P43's
35,659,264; the cap (37,748,736) leaves 1,630,720 bytes.

The cost is P31's own code:
- the inline formatting context: lines, breaks, atomic inlines, and whole-block RTL;
- the text measurer on GPUI's shared text system, with its shape and resolve caches;
- `@font-face` loading and reading a face's names;
- the text items, selection and caret of the display list.

Candidates for P42: the monomorphised paint and measurer paths, and the font-table parsing.

## P16: protocol, sessions, net, cookies, webRequest, permissions, proxies and downloads

`electron/p16-protocol-session` at its tested tip (7bb3b89b; merged as `c00a983a` after P31): `cargo build --release -p verve-gpui
--bin electron --features jit` is 35,335,168 bytes against 34,617,856 for `b1a27600`, +717,312 bytes (+2.07 %). The budget was 200
KB, re-set to 500 KB after the first measure (+713,728). The round meant to cut it split `MainGuest::from_renderer` and moved the
proxy to a per-request route, but it took nothing off. The 217 KB over the budget was accepted so that P16 could merge, and the cut is
left to P42. `docs/electron-plan` with P16 merged is 36,822,016 bytes, +704,000 on P31's 36,118,016; the cap (37,748,736) leaves
926,720 bytes.

Where it goes (`cargo bloat --release`, unstripped, before that round): P16's named functions come to about 340 KiB.
- **verve-electron's API modules**: about 205 KiB for protocol, net, session, cookies, webRequest, permissions and downloads.
- **The kernel**: about 70 KiB for the `browser_net` hook and the changes to `network::fetch`.
- **The Hub**: about 38 KiB for sessions, cookies and proxies.
- **verve-dom**: about 22 KiB for cookies, permissions and downloads.
- **The rest** is generic copies, closures and data.

The largest functions were `MainGuest::from_renderer` (50.5 KiB, one match, since split by message family), `net_request::init_request`
(13.0 KiB), `web_request::run` (11.3 KiB), `net_request::incoming_message` (8.7 KiB) and `protocol_serve::start` (8.0 KiB).

Candidates for P42: net_request's closure per method, and the near-copies between the request and download paths.

## P43: the inspector window and the REPL (phase 2)

`electron/p43-inspector` at its tested tip (744b2afe; merged as `3c6b8eb8` on `124ca9af`): `cargo build --release -p verve-gpui
--bin electron --features jit` is 37,128,192 bytes against 36,822,016 for `124ca9af` (P16 merged and the cap raised), +306,176 bytes
(299 KiB), within the 300 KiB budget. The cap (41,943,040) leaves 4,814,848 bytes.

The cost is P43's own code, with no new dependency:
- the inspector model in verve-gpui: the element tree updated from `Update`'s changes, computed styles, the box model and the console;
- the window: the tree, the styles and box-model panes, the console and the REPL line editor;
- the highlight and the element picker over the page window;
- the Hub's routing of REPL calls and replies, and the per-page console ring;
- in the kernel, `inspect_value` and Node's `UnhandledPromiseRejectionWarning`/`PromiseRejectionHandledWarning` text, with V8's
  naming of a reason.

## P24: live ranges and selection, contenteditable editing, input-method composition (phase 1)

`electron/p24-editing` merged on `docs/electron-plan` after P43 phase 2: `cargo build --release -p verve-gpui --bin electron
--features jit` is 37,295,616 bytes against 37,128,192 for P43 phase 2's merge, +167,424 bytes. It was measured at 6feb23de; the
tested tip 539d4d10 adds only the doctype case of `deleteContents`. Alone on `124ca9af` it measured
+155,648. The budget is 450 KB for both phases. The cap (41,943,040) leaves 4,647,424 bytes.

The cost is verve-dom's own code, with no new dependency (`unicode-segmentation` was already in the graph):
- live ranges in the arena and their boundary rules, and `Range`/`StaticRange`/`Selection` on them;
- the contenteditable key paths, `beforeinput` with target ranges, the undo history and `execCommand`;
- composition in fields and editable regions, and the caret box for the candidate window.

## P35: pictures, background images and the media stubs (phase 1)

`electron/p35-images` merged on `docs/electron-plan` after P43 phase 2 and P24 phase 1: `cargo build --release -p verve-gpui --bin
electron --features jit` is 37,452,288 bytes against 37,295,616 for P24's merge, +156,672 bytes. It was measured at 31dfdba6; the
tested tip 3a2ff089 adds only a guard in the cascade's `revert-layer` and test waits. Alone on `124ca9af` round 1 measured +133,120.
The budget is 400 KB for both phases. The cap (41,943,040) leaves 4,490,752 bytes.

The cost is P35's own code, with no new crate (it decodes with P25's `canvas::decode`, through `image`, which was already linked):
- source choice (`srcset`, `sizes`, `<picture>`) and the `background` shorthand opened into per-layer longhands, in verve-dom;
- intrinsic sizes, `object-fit`/`object-position` and background tiles, in verve-layout;
- one decode thread per page and the published image table, in verve-electron;
- the picture cache and drawing in both painters, in verve-gpui.

## P24: clipboard events, drag and drop, and mouse selection (phase 2)

`electron/p24-editing` merged on `docs/electron-plan` after P35 phase 1: `cargo build --release -p verve-gpui --bin electron
--features jit` is 37,627,904 bytes against 37,452,288 for P35's merge, +175,616 bytes, measured at the merge c2a87aa0. With phase 1
(+167,424) P24 has cost 343,040 bytes of its 450 KB budget for both phases. The cap (41,943,040) leaves 4,315,136 bytes.

The cost is P24's own code, with no new dependency:
- `DataTransfer`, the `copy`/`cut`/`paste` events with their default actions, transient activation and `navigator.clipboard`, in
  verve-dom;
- drag and drop inside the page and from the system, and selection by dragging the pointer, in verve-dom;
- the clipboard messages and dropped files, in verve-electron, and the shells' clipboards (in memory when the shell may not touch
  the desktop);
- HTML serialization that writes end tags for empty foreign elements, with XML serialization kept apart.

## P32: the input bridge (phase 1)

`electron/p32-input` merged on `docs/electron-plan` after P24 phase 2 (merge 67ecbefe, then the integration with P24 and two review
rounds on top): `cargo build --release -p verve-gpui --bin electron --features jit` is 37,714,432 bytes against 37,627,904 for
P24's size note, +86,528 bytes, measured at 8650a0fa. Alone on `2cd509f2` round 1 measured +74,240. The budget is 200 KB for phase 1
and 400 KB for both phases. The cap (41,943,040) leaves 4,228,608 bytes.

The cost is P32's own code. `url` became a direct dependency of verve-gpui, but it was already linked through verve-dom and
verve-electron:
- the keymap from GPUI keystrokes to the page's `key`/`code`, the key route (`before-input-event`, the page, then the menu for an
  unhandled key), the typed-character queue and dead keys, in verve-gpui;
- the pointer, wheel and cursor bridge, the hit test against the display list's hitboxes, drag areas, and the input-method bridge,
  in verve-gpui;
- pointer fields, the focus ring's input origin, ending a composition on a focus change, and what a key's default action did, in
  verve-dom;
- the page events for the cursor, the link under the pointer and the zoom wheel, and the menu lookup shared by both shells, in
  verve-electron.

## P35: inline SVG, canvas surfaces and animated pictures (phase 2)

`electron/p35-images` merged on `docs/electron-plan` after P32 phase 1: `cargo build --release -p verve-gpui --bin electron --features
jit` is 38,151,168 bytes against 37,714,432 for P32's size note, +436,736 bytes, measured at the branch's tip before the last change
(a test only). With phase 1 (+156,672) P35 has cost 593,408 bytes against its 400 KB budget for both phases, over by about 190 KB.
The cap (41,943,040) leaves 3,791,872 bytes.

Where it goes (cargo-bloat, `.text`, the branch's first round against its base; the later rounds added about 28 KB of P35's own
code):
- resvg's renderer, +110 KiB: GPUI links usvg, but this binary never reached resvg's drawing code before;
- resvg's own decoders for pictures inside an SVG (its `raster-images` feature: zune-jpeg, png, image-webp), +144 KiB. The feature
  is on because the GPUI fork's workspace `Cargo.toml` enables it, and features unify across the build. Turning it off there would
  give these back, at the cost of SVGs not drawing pictures embedded in them;
- P35's own code (the SVG writer, frames, surfaces, budgets) in verve-layout, verve-gpui, verve-electron and verve-dom, about 70 KiB;
- the rest is in `std` and rounding.
