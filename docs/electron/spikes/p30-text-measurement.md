# P30 spike: text measurement off the GPUI main thread, and GPUI's size cost

Date 2026-11-02. Windows 10 x64, rustc 1.98.1. GPUI is the Rebon fork `Quorafind/zed-c` at
`e679b519961d280cb878183709e31c28684fb5e0` (crates under
`~/.cargo/git/checkouts/zed-c-4beada37dea49152/e679b51/crates/`; paths below are relative to it).
Prototype: `crates/verve-layout-spike/` (its own Cargo workspace, so GPUI stays out of the root
`Cargo.lock`; its `Cargo.lock` is Rebon's, which pins the GPUI graph).

## Answers

| Question | Answer |
|---|---|
| Q1: can text be shaped and measured off the GPUI main thread? | **Yes, with no patch to the fork.** |
| Q2: what does GPUI cost in binary size? | **About +6.1 MB (5.8 MiB)** on top of the Electron-profile core; a bare GPUI window app is 6.55 MB. |

## Q1: evidence

### What is Send + Sync, and what needs the main thread

- `PlatformTextSystem: Send + Sync` (`gpui/src/platform.rs:1499`). On Windows it is
  `DirectWriteTextSystem` (`gpui_windows/src/direct_write.rs:241`), whose whole state is behind one
  `parking_lot::RwLock<DirectWriteState>` (line 40); `layout_line` takes the write lock (line 294).
  So every shaping call is **thread-safe but serialized** on that lock (and on glyph rasterization,
  which the painter takes on the main thread).
- `TextSystem` (`gpui/src/text_system.rs:230`) holds only `RwLock`/`Mutex`/`Arc` fields. The prototype
  asserts `TextSystem: Send + Sync` and `WindowTextSystem: Send + Sync` at compile time.
- **No `App`, `Window` or running `Application` is needed.** The public route is:
  `gpui_platform::current_platform(false)` -> `Platform::text_system()` (`platform.rs:366`) ->
  `TextSystem::new(Arc<dyn PlatformTextSystem>)` (`text_system.rs:245`, `pub`) ->
  `WindowTextSystem::new(Arc<TextSystem>)` (`text_system.rs:610`, `pub`). Inside a running app,
  `cx.text_system()` (`app.rs:2340`) returns the same `Arc<TextSystem>` and can be sent to a thread.
- P30's plan said whole-line shaping is not public on `TextSystem`. That is true for
  `TextSystem`, but `WindowTextSystem` is a plain constructible wrapper (a frame cache around the
  same `TextSystem`) and exposes `layout_line` (`text_system.rs:892`, `TextRun`s, optional
  `force_width`) and `shape_text` (`text_system.rs:754`, wrapping with `wrap_width`, returns
  `WrappedLine`s). Both are `pub`, take `&self`, and need no `Window`. The `LineLayout` they return
  has `width/ascent/descent` and per-glyph `id/position/index` (`text_system/line_layout.rs:19,45`);
  wrap points are `WrapBoundary { run_ix, glyph_ix }` (`line_layout.rs:290`), mappable to a byte
  index through `ShapedGlyph.index`.
- Public on `TextSystem` itself, also thread-safe: `resolve_font` (370), `ascent/descent` (507/513),
  `cap_height/x_height` (497/502), `advance` (433), `units_per_em` (492), `line_wrapper` (545,
  `wrap_line` over `LineFragment`s), `add_fonts` (295, for `@font-face` bytes).
- Only the paint-side calls need the main thread: `Window::paint_glyph` (`window.rs:4996`) and the
  rasterization path (`pub(crate)` `raster_bounds/rasterize_glyph`). `paint_glyph` takes a
  `FontId` and `GlyphId`, so ids shaped on the renderer thread are valid on the GPUI thread **as long
  as both use the same `Arc<TextSystem>`** (the prototype's ids are stable: 0 Segoe UI, 1 Consolas,
  2 Segoe UI Bold, 3 and 4 the CJK fallbacks). `FontId`s are per `TextSystem` instance, so do not
  build a second `TextSystem` for the layout thread in production; share the app's.

### Prototype output

`crates/verve-layout-spike/src/bin/measure.rs`, debug build, 16 px, wrap width 120 px. Run
`measure` (no App, no window) and `measure app` (inside a running GPUI app, worker given
`cx.text_system()`). Each prints the report from the main thread and from a worker; the strings are
compared byte for byte:

```
TextSystem and WindowTextSystem are Send + Sync (compile-time check passed)
===== standalone: MATCH = true =====
===== standalone: 8 threads x 100 rounds, all equal to main = true =====
===== in-app: MATCH = true =====            (and equal to the standalone output)
```

Excerpt of the report (identical on all threads):

```
metrics Segoe UI   w=400 asc=17.266 desc=-4.016 xh=8.000 cap=11.203 adv(x)=7.344 upem=2048
metrics Consolas   w=400 asc=14.719 desc=-4.016 xh=7.844 cap=10.211 adv(x)=8.797 upem=2048
metrics Segoe UI   w=700 asc=17.266 desc=-4.016 xh=8.000 cap=11.203 adv(x)=8.836 upem=2048
metrics NoSuchFont w=400 -> same FontId as Segoe UI (resolves to the system fallback)
line Segoe UI   "Hello 你好 مرحبا"  width=112.359 asc=17.266 desc=4.016 runs(font ids)=[0,3,0,0] glyphs=14
line Consolas   "Hello 你好 مرحبا"  width=128.320 ...                   runs=[1,3,1,0]
line Segoe UI   "The quick brown fox..." width=317.180  wrap@120 breaks(byte)=[10,20,35]
line Segoe UI   "日本語のテキストと English が混ざった長い行です。" width=378.930  wrap@120 breaks=[21,39,60]
line Consolas   same CJK line width=399.172 wrap@120 breaks=[21,36,57]
```

Font fallback works off-thread: in "Hello 你好 مرحبا" the CJK run lands on fallback font id 3 and the
Latin and Arabic runs stay on the requested font (Segoe UI has Arabic). Wrapping reports breaks on
spaces and between CJK characters.

### Unpatched; a few optional patches

No patch was needed, so there is no `vendor-spike/` copy. Two small fork patches are worth taking only
if the issues below bite:

1. `WindowTextSystem::finish_frame` is `pub(crate)` (`text_system.rs:884`). A `WindowTextSystem` owned
   by the layout thread never gets its frame swap, so its `LineLayoutCache` (`line_layout.rs:458`)
   only grows. Either make `finish_frame` `pub` (one word), or keep our own bounded cache in
   `TextMeasurer` and drop/recreate the `WindowTextSystem` periodically (it holds nothing else).
2. The Window cache is keyed per `WindowTextSystem`, so the layout thread's cache and the paint
   thread's do not share. Harmless: painting needs only ids and positions, not the layout.

The P30 text that asks for `TextSystem::layout_line` and `Window::with_element_opacity` patches can be
dropped for shaping; `with_element_opacity` is a paint-side concern and unrelated to this spike.

### Own-font-stack fallback (rustybuzz), compared with GPUI

`own-stack` (feature `own-stack`) shapes the same strings with `rustybuzz` straight from
`C:\Windows\Fonts\segoeui.ttf` and `consola.ttf` and compares with GPUI/DirectWrite at 16 px:

```
Segoe UI: ours asc=17.266 desc=4.016 | gpui asc=17.266 desc=4.016
  Hello 你好 مرحبا                 ours=110.359 gpui=112.359   (we have no CJK fallback; .notdef is narrower)
  The quick brown fox jumps over ours=317.180 gpui=317.180
Consolas: ours asc=11.883 desc=4.117 | gpui asc=14.719 desc=4.016
  Hello 你好 مرحبا                 ours=123.156 gpui=128.320
  The quick brown fox jumps over ours=378.266 gpui=378.266
```

- Advance widths for text in a single font agree to the third decimal (DirectWrite with the same
  unhinted design-unit advances). The `Hello` differences are only the missing fallback font.
- **Vertical metrics differ by source.** GPUI/DirectWrite takes ascent/descent from the font's
  Windows/typo metrics (Consolas 14.719 = winAscent), while hhea (what rustybuzz's `Face::ascender`
  reads) gives 11.883. `line-height: normal` in Chrome depends on which table it reads (not verified here), so
  if we need to match Chrome we must pick the source deliberately; GPUI's numbers are right for
  painting alignment.
- Painting our own shaping through GPUI is possible: `paint_glyph(origin, font_id, glyph_id, ...)`
  needs a GPUI `FontId`, which comes from `TextSystem::add_fonts` + `resolve_font` (the font must be
  registered with GPUI, which `@font-face` bytes already will be). Glyph ids are properties of the font
  file, so rustybuzz ids and DirectWrite ids agree for the same file. The cost is that we must own
  fallback, bidi and script itemization ourselves (rustybuzz shapes one run), which GPUI already does.
  I did not prototype this painting path.

## Recommendation for P30/P31

**Shape on the renderer thread through GPUI's own `TextSystem`.** `TextMeasurer` is implemented over
the app's shared `Arc<TextSystem>` plus a `WindowTextSystem` of the layout thread's own. `resolve`
-> `TextSystem::resolve_font`, `metrics` -> `ascent/descent/x_height/cap_height`, `shape` ->
`WindowTextSystem::layout_line` (per same-font run) or `shape_text` where P31 wants GPUI's breaking.
Reasons: it works today without touching the fork, results are bit-identical to what the paint thread
would draw, and CJK/Arabic fallback, bidi and emoji handling come for free.

Do not take the "layout on the GPUI thread, rects a frame late" fallback; it is not needed. Keep the
own-stack idea only as a way to get Chrome-matching vertical metrics (read hhea/OS2 tables with
`skrifa`/`ttf-parser` for the `line-height: normal` value) and for the headless `TextMeasurer`.

P30 step 1 can be reduced to: build the measurer over `cx.text_system().clone()`, move it to the
renderer thread, and add a test comparing both threads (the prototype's `report`).

## Risks

- **Lock contention.** DirectWrite state is one `RwLock`; a long layout pass holds the write lock for
  every uncached shape and can stall glyph rasterization on the GPUI thread (frame hitch). Mitigate with
  a measurer-level cache keyed by (text, font, size) so a re-layout rarely shapes, and by prewarming
  fonts (`prewarm_fonts`, `text_system.rs:395`).
- **Unbounded `WindowTextSystem` cache** (see patch 1).
- **macOS and Linux were not tested.** The Windows backend is thread-safe by `RwLock`; macOS goes
  through `gpui_macos`'s own text system (not read in this spike), and `font-kit` only affects macOS.
  The same `PlatformTextSystem: Send + Sync` bound applies, but the P30 acceptance on macOS should re-run
  `measure`.
- **`current_platform(false)` as a stand-alone `TextSystem` source creates a second platform object**
  (message window, COM). Fine for a spike and tests; production must share the app's `TextSystem`
  (`FontId`s are per instance).
- **Font ids and `add_fonts`.** Registering an `@font-face` after layout bumps a font generation and
  clears `LineLayoutCache`; already-issued `FontId`s stay valid, but cached `ShapedRun`s are stale.
- **Metrics source** differs from Chrome's `line-height: normal` (above).
- DirectWrite shaping is not byte-identical to Chrome's HarfBuzz for kerning/ligature-heavy fonts;
  expect sub-pixel differences in widths, hence wrapped lines can break differently on rare strings.

## Q2: binary size

Release profile from the root `Cargo.toml` (fat LTO, `codegen-units = 1`, `strip = "symbols"`,
`opt-level = "s"`, hot Boa crates at 3), copied into the spike's `Cargo.toml`. One build per
configuration, `CARGO_BUILD_JOBS=4`, Windows x64, fork rev above. `verve` is taken with
`default-features = false`; this base has no `wasm` feature (wasmi is unconditional), so that is the
Electron feature set here. Every binary evaluates `1 + 1` or opens one window.

| Binary | Bytes | MiB | MB |
|---|---:|---:|---:|
| (a) GPUI window app, one text line (`size-app`) | 6,553,600 | 6.25 | 6.55 |
| (c) Verve runtime alone (`size-verve-only`) | 21,087,744 | 20.11 | 21.09 |
| (b) GPUI window + Verve (`size-app --features verve`) | 27,184,128 | 25.93 | 27.18 |

- **GPUI delta over Verve = b - c = 6,096,384 B (5.81 MiB, 6.1 MB)**; a standalone GPUI app is 6.55 MB
  because it also carries its own std. This matches the README's "GPUI ~ 6 MB" estimate.
- The combined binary is 27.2 MB (25.9 MiB) **before** `verve-dom` (est. +2 MB) and the JIT (+5.9 MB).
  Against the README's budget (about 20 MB, hard cap 24 MB) this is over; it fits the "~28 MB" cap
  the README floats only without verve-dom, so the cap decision is still open (see `size.md`).
- Verve alone is 21.1 MB here, not the 17.8 MB the README quotes for the core: the spike links
  `Runtime::new()` (everything it pulls in), the README figure may have been measured with a different
  entry point or MB vs MiB; I did not chase the 3 MB difference.
- **`font-kit` on/off: no effect on Windows, so not built.** `gpui_platform`'s feature is
  `font-kit = ["gpui_macos/font-kit"]` (`gpui_platform/Cargo.toml:21`), and `gpui`'s own `font-kit`
  feature is empty (`gpui/Cargo.toml:38`). On Windows the text system is DirectWrite either way; the
  feature only switches macOS from CoreText-direct to the `font-kit` crate. It needs measuring on a Mac.
- Build time: GPUI app 1m19s warm (about 4 min cold); GPUI+Verve 4m06s; Verve alone 2m23s.

## Reproduce

```
cd crates/verve-layout-spike
set CARGO_TARGET_DIR=<target> CARGO_BUILD_JOBS=4
cargo run --bin measure                 # standalone, main vs worker vs 8 threads
cargo run --bin measure -- app          # inside a running GPUI app
cargo run --features own-stack --bin own-stack
cargo build --release --bin size-app [--features verve]
cargo build --release --features verve --bin size-verve-only
```
