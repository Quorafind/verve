# Verve JS performance (P60)

The JS throughput suite and the scaling probes of [`perf-baseline.md`](perf-baseline.md), now in
the repository and reproducible with one command each:

```sh
bash scripts/js-bench/run.sh <verve.exe> node              # the 14 throughput tests
bash scripts/js-bench/run.sh --scaling <verve.exe> node    # the 44 scaling probes

# A build a sampler can read, with the same features as the release binary measured here.
cargo build --profile profiling --bin verve --no-default-features --features wasm,jit
```

P64, P65 and P66 measure with these files and take their numbers from this one. A package
that changes a number appends its rows under "After each package" at the end, in the template's
shape; do not start another file.

## Machine, conditions, builds

- Windows 10 Enterprise LTSC 2021, Ryzen 9 7950X (32 threads), 128 GB RAM. **The machine is shared
  with the other implementations of this line.** Every figure below states the load it was taken
  at, read with `Get-Counter '\Processor(_Total)\% Processor Time'` — five samples immediately
  before the pass and five immediately after.
- Node 22.21.1 (stock Windows build). Verve: `cargo build --release --bin verve
  --no-default-features --features wasm,jit` on `docs/electron-plan` `cde79ef3`,
  `CARGO_BUILD_JOBS=6`, 23,577,600 bytes — the size `perf-baseline.md` recorded to the byte, so
  this package changed nothing in the release binary. The interpreter column is that binary with
  `VERVE_JIT=0`; there is no second build.
- One process per measurement, `--runs 5`, median and best. A throughput test warms up once and
  is then timed once with `performance.now()`, as in the baseline. A scaling probe is given no
  warm-up of its own: `fastest` times the body twice and keeps the smaller, so the first timing
  is the warm-up of the second. A run that fails or is killed stops that test's sampling and
  prints how many runs it had to stderr, so a median over three runs is never read as one over
  five; the row shows what was collected.
- The passes:
  - §1: load 17.2–35.0% before (median 22.3%), 8.4–21.5% after (median 13.4%).
  - §1 `regex`, re-measured alone after the baseline's pattern was restored: 12.8% before, 12.3%
    after.
  - `array-shift`, re-measured alone: 32–70% before the pass and 25–34% after it, the highest load
    in this file; the row is the probe's ratio, which survives that better than its times do.
  - §2: load 4.1–11.9% before (median 9.5%), 2.5–7.1% after (median 5.4%).
  - A pass taken earlier while another implementer's release build held the machine at 100% read
    **1.6–2.0× slower on every test**; it is not in this file. A busy machine inflates absolute
    times roughly uniformly, so ratios survive it better than times do — but the times above are
    from the lower-load passes and are the ones to quote.

## 1. JS throughput (`scripts/js-bench/bench.js`, ms, median (best), N=5)

The first ten rows are `perf-baseline.md`'s tests at its sizes, so they compare with its section 3
directly; the last four are shaped like application code and are new here.

| test | Node | Verve JIT | Verve interp | JIT / Node | interp / Node |
|---|---|---|---|---|---|
| `fib` (`fib(30)`) | 5.5 (5.4) | 309.7 (301.1) | 390.6 (382.2) | 56× | 71× |
| `alloc` 300k objects | 10.7 (10.2) | 298.0 (287.6) | 319.6 (304.0) | 28× | 30× |
| `concat` 20k appends | 0.9 (0.8) | 23.3 (21.7) | 32.8 (22.2) | 26× | 36× |
| `splitjoin` 20k parts | 3.3 (2.8) | 716.8 (627.1) | 614.9 (589.3) | 217× | 186× |
| `regex` 5k lines | 1.7 (1.6) | 489.7 (483.7) | 475.7 (470.2) | 288× | 280× |
| `json` 5.4 MB | 177.2 (158.1) | 2786.7 (2723.5) | 2770.4 (2743.4) | 16× | 16× |
| `sort` 1M numbers | 170.5 (168.3) | 2627.9 (2553.0) | 2586.9 (2578.7) | 15× | 15× |
| `mapset` 20k churn | 4.6 (4.5) | 409.8 (407.3) | 411.7 (409.8) | 89× | 90× |
| `oo` 1M method calls | 2.9 (2.9) | 1290.1 (1251.0) | 1319.1 (1289.4) | 445× | 455× |
| `typed` 1M × 5 | 4.3 (4.0) | 3487.4 (3370.4) | 3565.1 (3521.3) | 811× | 829× |
| `vdom` 2k-node diff × 5 | 4.4 (4.0) | 71.0 (68.6) | 72.3 (71.3) | 16× | 16× |
| `tokenize` 200 KB, sticky | 2.4 (2.4) | 5192.6 (5153.5) | 5279.2 (5054.2) | 2164× | 2200× |
| `template` 1k-row table | 0.2 (0.2) | 2.3 (1.8) | 1.5 (0.9) | 12× | 7.5× |
| `closure` 1M map/filter/reduce | 24.5 (23.7) | 660.9 (633.5) | 660.0 (653.4) | 27× | 27× |

`alloc` was measured again on its own with `--runs 11` (load 26–36%) after the 5-run pass read it
24% high: 298.0 (287.6) compiled, 319.6 (304.0) interpreted, against Node's 10.7 (10.2). That is
the row above; its own 5-run values were 358.6 and 362.3, and no code changed between the passes,
so the difference is this machine's noise at 300k allocations.

`regex` was measured again on its own because the pass above ran with the baseline's pattern
altered: it had `\d\d` where `perf-baseline.md` has `\d+`, which matches 3,389 of the 5,000 dates
instead of 5,000. The row above is the restored test, 475.7 (470.2) interpreted against Node's
1.7 (1.6), at the load stated for it above.

**Against `perf-baseline.md` section 3, all ten shared tests are within ±15%** (interpreter
medians, this run against the baseline's): `fib` +6%, `alloc` +10%, `concat` −1%, `splitjoin`
+13%, `regex` +3%, `json` +8%, `sort` +1%, `mapset` +3%, `oo` +8%, `typed` +4%. The suite
reproduces the baseline, which is what the acceptance criterion asks for.

The gap and its shape are the baseline's: 15× at the best of its ten (`sort`, with `json` next at
16×) and 30×–830× across the other eight; the four new tests land at 7.5× to 2200×. The JIT closes
almost none of it — its wins here are `fib` (1.26×) and `concat` (1.41× within noise at 0.9 ms of
Node time), nothing on builtins, and `splitjoin` is 17% *slower* compiled (716.8 against 614.9
interpreted), so the JIT is not a lever on that row.

The four new tests say where application code lands:

- `tokenize` is the worst number in the suite at 2200×: the scan takes 102,228 tokens, so a token
  costs about 52 µs interpreted and 51 µs compiled against 23 ns in V8. **P64**: this is the shape
  of every hand-written tokenizer and of the scanners in PostCSS and TypeScript, which makes it
  worth more than its 200 KB suggests. A hypothesis, not a profile: that each token pays a full
  `RegExp` match entry.
- `vdom` (16×) and `closure` (27×) sit at the good end, near the builtin-dominated floor.
- `template` (7.5×) is the best ratio in the suite and shows the suite's timing floor: Node needs
  0.2 ms, so a ±0.1 ms scheduling difference moves the ratio by 50%. Treat it as "fast enough".

## 2. Scaling probes (`scripts/js-bench/scaling.js`, N=5)

Each probe is timed at its own n and at 4n, `n` chosen so that t(n) lands in 10–200 ms
interpreted. A ratio above 8 is super-linear, above 5 suspicious, 4 linear. The table is the
driver's own output, the interpreter column, so a row pastes straight back into it. The verdict
takes the worse of the two Verve engines' medians, which is why `replaceAll-regex-unicode` reads
`suspicious` on a 4.3× interpreter ratio: its compiled ratio was 5.1×. Verdicts are Verve's;
Node's column is in the driver's output but its n is sized for Verve, which leaves Node in the
microsecond range where a ratio means nothing.

The JIT changes no verdict: on every probe the two columns land in the same band. The ratios
differ by up to 2× on the super-linear rows, which is far from moving any of them below 8.
`concat` is the noisy row — its medians ranged 17–37× across the passes run here while its best
at 4n stayed 325–354 ms, so a ratio far above the rest there is one bad run of 80,000 appends,
not a JIT or an algorithmic effect.

Probes marked **super-linear** are bugs to fix, not constant factors.

| probe | n | t(n) | t(4n) | ratio | verdict |
|---|---|---|---|---|---|
| `split-char-latin1` | 4000 | 20.1 (19.6) | 325.9 (321.5) | 16× | **super-linear** |
| `split-char-unicode` | 4000 | 18.5 (18.3) | 301.7 (298.7) | 16× | **super-linear** |
| `split-multi-latin1` | 4000 | 21.4 (21.3) | 348.6 (341.9) | 16× | **super-linear** |
| `split-multi-unicode` | 4000 | 19.6 (19.3) | 324.8 (321.4) | 17× | **super-linear** |
| `indexOf-latin1` | 25000000 | 41.2 (39.5) | 162.3 (160.4) | 3.9× | linear |
| `indexOf-unicode` | 25000000 | 12.3 (12.2) | 49.2 (48.5) | 4.0× | linear |
| `includes-latin1` | 25000000 | 39.7 (39.4) | 156.5 (154.2) | 3.9× | linear |
| `includes-unicode` | 25000000 | 12.6 (12.4) | 49.4 (48.8) | 3.9× | linear |
| `lastIndexOf-latin1` | 5000000 | 174.3 (154.0) | 688.4 (641.6) | 3.9× | linear |
| `lastIndexOf-unicode` | 5000000 | 153.6 (150.0) | 600.6 (596.3) | 3.9× | linear |
| `replace-string-latin1` | 10000000 | 20.8 (20.7) | 85.9 (83.1) | 4.1× | linear |
| `replace-string-unicode` | 10000000 | 12.0 (10.5) | 47.2 (46.2) | 3.9× | linear |
| `replaceAll-string-latin1` | 4000 | 21.9 (21.3) | 356.7 (352.9) | 16× | **super-linear** |
| `replaceAll-string-unicode` | 4000 | 20.0 (19.8) | 327.3 (324.8) | 16× | **super-linear** |
| `replace-regex-latin1` | 5000 | 85.3 (82.3) | 1428.5 (1321.8) | 17× | **super-linear** |
| `replace-regex-unicode` | 5000 | 13.9 (11.8) | 69.2 (57.8) | 5.0× | linear |
| `replaceAll-regex-latin1` | 5000 | 89.3 (86.0) | 1442.9 (1436.9) | 16× | **super-linear** |
| `replaceAll-regex-unicode` | 5000 | 13.7 (12.6) | 58.7 (54.4) | 4.3× | suspicious |
| `matchAll-latin1` | 4000 | 150.7 (136.9) | 1886.0 (1858.2) | 13× | **super-linear** |
| `matchAll-unicode` | 4000 | 17.2 (15.2) | 61.1 (57.7) | 3.6× | linear |
| `exec-loop-latin1` | 6000 | 115.5 (110.3) | 1967.6 (1940.4) | 17× | **super-linear** |
| `exec-loop-unicode` | 6000 | 15.5 (15.2) | 59.5 (58.3) | 3.8× | linear |
| `repeat-latin1` | 1000000 | 12.1 (11.6) | 49.6 (48.4) | 4.1× | linear |
| `repeat-unicode` | 1000000 | 12.7 (12.6) | 53.0 (50.2) | 4.2× | linear |
| `padStart-latin1` | 5000000 | 14.5 (14.1) | 56.0 (54.7) | 3.9× | linear |
| `padStart-unicode` | 5000000 | 15.0 (14.6) | 58.6 (55.6) | 3.9× | linear |
| `concat` | 20000 | 22.2 (21.9) | 406.6 (352.2) | 18× | **super-linear** |
| `array-indexOf` | 400000 | 13.5 (13.1) | 54.6 (52.8) | 4.0× | linear |
| `array-includes` | 400000 | 13.3 (12.9) | 56.8 (54.4) | 4.3× | linear |
| `array-shift` | 50000 | 50.2 (50.1) | 212.8 (207.9) | 4.2× | linear |
| `array-unshift` | 5000 | 92.6 (88.5) | 367.8 (349.7) | 4.0× | linear |
| `array-splice` | 5000 | 87.1 (85.8) | 353.9 (344.2) | 4.1× | linear |
| `map-set` | 40000 | 14.7 (14.0) | 64.2 (56.9) | 4.4× | linear |
| `map-iterate` | 20000 | 15.3 (12.9) | 58.8 (56.4) | 3.8× | linear |
| `map-delete` | 20000 | 191.4 (188.4) | 5547.9 (5410.2) | 29× | **super-linear** |
| `set-add` | 80000 | 14.7 (13.1) | 57.8 (54.7) | 3.9× | linear |
| `set-iterate` | 40000 | 14.6 (14.5) | 58.9 (56.4) | 4.0× | linear |
| `set-delete` | 20000 | 157.8 (154.6) | 3963.1 (3886.3) | 25× | **super-linear** |
| `set-delete-while-iterating` | 20000 | 13.4 (13.2) | 52.1 (50.0) | 3.9× | linear |
| `dict-delete-read` | 4000 | 53.9 (53.5) | 1115.0 (1100.9) | 21× | **super-linear** |
| `dict-delete-write` | 4000 | 53.5 (52.0) | 1099.9 (1087.8) | 21× | **super-linear** |
| `json-stringify` | 20000 | 68.8 (68.3) | 281.7 (272.0) | 4.1× | linear |
| `json-stringify-longstring` | 3000000 | 11.6 (10.6) | 44.7 (42.7) | 3.9× | linear |
| `json-parse` | 20000 | 153.8 (146.4) | 698.5 (679.0) | 4.5× | linear |

Every probe's t(n) is 11.6–191.4 ms interpreted, within the package's 10–200 ms window. A single
run past 20 s at either size is what the package calls super-linear, and 4n is then not attempted:
the probe answers `"superLinear": true` and the driver marks the row so rather than quoting a
ratio. None did in this pass. A probe whose t(n) lands between 1,250 ms and 20 s is a different
case — it is reported as `skipped` with no verdict, because a quadratic 4n could pass the limit and
a linear operation at an oversized n would otherwise be recorded as a bug. That did not happen
either.

**These rows are the state before P61.** P61 has since made `split`, `Map`/`Set` deletion and the
regex scans linear; its numbers and the probes that confirm them are in
[`packages/P61-superlinear-builtins.md`](packages/P61-superlinear-builtins.md) ("状态"), and its
rows belong under "After each package" here.

### What the probes found, before P61 (handed to P61 and P64)

- **`split` is quadratic**, on both encodings and for one-character and multi-character
  separators alike (16–17× for 4n). Confirms `perf-baseline.md`.
- **`Map.prototype.delete` and `Set.prototype.delete` are quadratic** (25–29× for 4n) while
  `set`, `add` and iteration are linear, so the cost is in the delete path and not in the table.
  **Deleting while iterating is linear**, so the plain delete loop takes a different path from the
  iterating one — the place to look is the tombstone or compaction step the loop alone reaches.
- **`replaceAll` with a string pattern is quadratic** (16×) although `replace` with a string
  pattern and a single occurrence is linear. `replaceAll` of a regex is quadratic on Latin-1 text
  and linear on text outside it.
- **A regex scan is super-linear on Latin-1 text and linear on text outside it**: `matchAll`
  (13×), an `exec` loop (17×), `replace` with a regex (17×) — the same pattern over
  `abcdefgh`+digits against `日本語かな漢字`. Whatever the Latin-1 path does per match, it does not
  do for the other encoding. This is the most valuable single finding for P64: it names where to
  look, not only how slow it is.
- **`delete obj[key]` leaves the object quadratic**: with 2,000 of 4,000 keys deleted, a read pass
  and a write pass are both about 21× for 4n, so this is Verve's own dictionary mode and not the
  cost of the keys.
- **String `+=` is super-linear past 20k appends** (18× for 4n interpreted) where the baseline's
  20k-append test looked fine: that test builds 20k short appends and stops, while 80k crosses
  whatever the copy starts doing. Anything building a few hundred KB of text pays it.
- **`array-shift` is linear; its 4n row used to leave the cache.** Boa's dense `shift` is
  `Vec::remove(0)`, one memmove of the rest of the array, so a call is O(n) in both engines and
  the ratio is 4 while the moved bytes stay in cache. At n=50,000 (200 KB of i32 elements, 800 KB
  at 4n) the ratio is 4.2×; at n=100,000 the 4n run is 1.6 MB, past the 1 MB L2, and the ratio
  went to 12.5× with the algorithm unchanged. Measured over n=12,500–200,000, the cliff sits
  between 50,000 and 100,000. That is a bandwidth cliff, not a scaling defect: this row is not
  handed to P65 as a bug, only as the memmove cost to reduce.
- **Linear but ~1000× off**: `array-unshift` and `array-splice` at n=5,000 take 93 and 87 ms for
  100 calls — 0.93 ms a call against 1 µs in Node. Each call is O(n) in both engines, so no ratio
  shows it; the constant is the finding.
- Linear and near Node's shape: `indexOf`, `includes`, `lastIndexOf`, `replace` with a string,
  `repeat`, `padStart`, the array scans, `Map`/`Set` insertion and iteration, `JSON.stringify` and
  `JSON.parse`.

## 3. Profiling a release Verve

The `profiling` profile is release code with symbols and line tables:

```toml
[profile.profiling]
inherits = "release"
strip = "none"
debug = "line-tables-only"
```

Build it with the same features as the binary whose numbers this file quotes, or the profile
answers for a different program:

```sh
cargo build --profile profiling --bin verve --no-default-features --features wasm,jit
target/profiling/verve.exe run scripts/js-bench/bench.js typed
```

It changes nothing in the `release` or `dist` binaries: on `cde79ef3`, the commit the numbers
above come from, this package's release build is the size of `perf-baseline.md`'s to the byte. On
MSVC the symbols live in `verve.pdb` beside the executable, not in it. Measured there:
`profiling/verve.exe` is 23,580,160 bytes against `release/verve.exe` 23,577,600, and the two
`.pdb` files are 68,198,400 and 3,969,024 bytes. The executables are nearly the same size because
what grew is the debug information; `strip = "none"` keeps its reference in the binary. Both grow
with the code, so a later commit reads differently: rebuilt on `471082de`, `profiling/verve.exe`
is 23,584,256 bytes with a 68,214,784-byte `.pdb`.

### Prerequisites

- **samply**: `cargo install samply --locked` (0.13.1 here). It is a sampler, not a viewer.
- Or **Windows Performance Toolkit**, which ships `wpr.exe` and `xperf.exe` with the Windows SDK
  and is on `PATH` on a machine that has the SDK's performance tools installed.
- An **elevated terminal**: starting an ETW trace needs it. Both routes below assume one.

### samply

[samply](https://github.com/mstange/samply) on Windows samples through ETW, which it drives with
`xperf`. The `--save-only` flag is what keeps it from starting a web server and opening a browser:

```sh
# One test, saved to a file, with no browser window: --save-only, and no `samply load` after it.
samply record --save-only -o typed.json.gz -- target/profiling/verve.exe run scripts/js-bench/bench.js typed
samply record --save-only -o regex.json.gz -- target/profiling/verve.exe run scripts/js-bench/bench.js regex

# Real function names rather than addresses, with a sidecar so the .pdb is not needed later:
samply record --save-only --unstable-presymbolicate \
  -o typed.json.gz -- target/profiling/verve.exe run scripts/js-bench/bench.js typed
```

`samply record` without `--save-only` starts a local web server and opens a browser; with it, the
file is written and nothing else happens. `--unstable-presymbolicate` writes a
`typed.json.syms.json` beside the profile holding the symbols resolved from the `.pdb`, so the
profile can be read on another machine without it.

Reading either file:

1. Open <https://profiler.firefox.com> and drop the `.json.gz` in (the sidecar sits beside it).
   That is a local viewer in the browser; no profile leaves the machine. `samply load
   typed.json.gz` does the same thing in the browser it opens, which is why this recipe stops at
   the file.
2. **Self time** — the "Bottom Up" view, or the call tree inverted: what the CPU was running,
   ranked per function. That ranking is what the two tables below hold.
3. **Top-down** — "Call Tree" from the root. Expect the root to be the `verve-main` thread that
   `verve` spawns to run the script (`src/cli.rs`, `run`), not `main`: the JS never runs on the
   main thread. Read the whole path under it, not only the leaf — the interesting cost is often
   which native entry a builtin was reached through.
4. The JavaScript runs on that one thread; samples on the other threads are the runtime's own and
   are not part of the test.

### Windows Performance Recorder / xperf, as the alternative

Also elevated — a non-elevated `xperf -on` fails with `NT Kernel Logger: 拒绝访问 (0x5)`:

```sh
wpr.exe -start CPU -filemode
target/profiling/verve.exe run scripts/js-bench/bench.js typed
wpr.exe -stop typed.etl

xperf -i typed.etl -o typed.csv -symbols -a dumper   # a SampledProfile row per sample, then its frames
xperf -i typed.etl -a stack -symbols > stacks.txt    # the same samples, aggregated for reading
```

`xperf -a dumper` is the machine-readable one and what the two tables can be read from: it writes
a `SampledProfile` row per sample followed by one `Stack` row per frame of that sample, so counting
the first frame after each `SampledProfile` gives self time and counting whole runs of `Stack` rows
gives the top-down path. `-a stack` is an aggregate over the same data — a report meant to be read,
not the folded one-line-per-stack format a flame-graph plotter takes. The `.pdb` sits next to the
executable, so symbols resolve without a symbol server.

### Examples: the ten functions with the most self time

**待总控实测 (left for the coordinator to measure).** The `typed` and `regex` profiles have to be
taken on a machine that can start an ETW trace, in an elevated terminal, with the recipe above.
Profiling was not run for this package, so nothing is filled in: the two tables (share of samples,
function) go here once that pass is made. No figure was carried over from an attempt that did not
finish.

## After each package

A later package that changes a number adds its rows here, one table per package, in the shape the
files print. Copy the header and fill the cells; keep the same column names and the same
`median (best)` form, so the rows stay comparable with §1 and §2.

Throughput (from `bash scripts/js-bench/run.sh <verve.exe> node`, which prints this header):

| package | test | Node | Verve JIT | Verve interp | JIT / Node | interp / Node |
|---|---|---|---|---|---|---|
| P64 | `tokenize` | 2.5 (2.5) | 162.7 (154.5) | 174.6 (172.2) | 65× | 70× |
| P64 | `regex` | 1.8 (1.8) | 32.9 (31.2) | 32.0 (31.4) | 18× | 18× |
| P64 | `json` | 152.5 (147.1) | 1303.9 (1263.3) | 1238.4 (1225.1) | 8.6× | 8.1× |
| P64 | `splitjoin` | 2.8 (2.7) | 12.9 (11.8) | 12.7 (12.4) | 4.6× | 4.5× |
| P64 | `template` | 0.2 (0.2) | 1.8 (1.7) | 0.9 (0.9) | 9.0× | 4.5× |
| P64 | `concat` | 0.6 (0.6) | 22.0 (21.8) | 78.1 (48.8) | 37× | 130× (bimodal; 66.6 (41.6) on the base binary) |
| P65 | `alloc` | 9.3 (9.1) | 253.2 (248.7) | 276.3 (264.6) | 27× | 30× |
| P65 | `mapset` | 5.0 (4.4) | 35.9 (35.0) | 36.1 (33.4) | 7.2× | 7.2× |
| P65 | `typed` | 4.0 (4.0) | 1046.6 (1026.5) | 1239.0 (1213.3) | 262× | 310× |
| P65 | `oo` | 2.9 (2.8) | 1099.8 (1090.5) | 1132.6 (1095.3) | 379× | 391× |
| P65 | `sort` | 173.4 (164.4) | 2891.3 (2710.0) | 3123.6 (2804.8) | 17× | 18× |
| P65 | `vdom` | 4.1 (3.7) | 67.0 (66.2) | 72.1 (70.0) | 16× | 18× |
| P65 | `closure` | 19.7 (19.5) | 593.1 (591.0) | 634.8 (621.5) | 30× | 32× |

P64 rows: this branch against `6335a760`, `cargo build --release -p verve --features jit`, same round, load 9-34% before and after each pass (one 61% peak at the end of the `tokenize` pass); details in `packages/P64-strings-regex-json.md`.

Scaling (from `bash scripts/js-bench/run.sh --scaling <verve.exe>`, the first table it prints):

| package | probe | n | t(n) | t(4n) | ratio | verdict |
|---|---|---|---|---|---|---|

### P65: objects, collections, typed arrays

Built and measured on `electron/p65-objects-typed-arrays` with
`cargo build --release -p verve --features jit`, `CARGO_BUILD_JOBS=6`; the interpreter column is the
same binary with `VERVE_JIT=0`, so there is one build. `--runs 5`, median (best), one process per
measurement.

The seven rows above are the final pass: five `Get-Counter` samples before it read 4.9, 2.2, 4.2,
3.2 and 1.2 % (median 3.2 %), five after it 10.3, 14.4, 14.6, 11.1 and 11.6 % (median 11.6 %).

Against the targets the pass meets `mapset` (7.2× against ≤8×) only. `typed` misses on the compiled
column by 12 (262× against ≤250×) and on the interpreter column by 60 (310×); `alloc` is 27× and 30×
against ≤25×; `vdom` (16×, 18×), `sort` (17×, 18×), `closure` (30×, 32×) and `oo` (379×, 391×) are
further out. The gaps in `oo`, `sort` and `closure` are in the call frame and the call opcode, which
are P66's, and phase 1 measured those floors. The `typed` interpreter column is the one the shortcuts
help least: they remove work from an opcode handler the interpreter reaches either way, while the
compiled column re-enters the handler only from the slow path.

Two low-load passes were taken on the same code, and the earlier one (load 10.8 % before, 4.4 %
after) read `alloc` 28×, `mapset` 7.5×, `typed` 251×, `oo` 376×, `sort` 15×, `vdom` 19×, `closure` 31×
on the compiled column — the same ranking within noise, except `alloc`, `typed` and `vdom`, whose
distance from their targets is 1–5× and therefore inside what the two passes differ by. `alloc` is
the test whose absolute ratio moves most with the machine; the per-change table in
`packages/P65-objects-typed-arrays.md` §8, which alternates the two binaries inside one round, is
the figure to trust for it.

### P66: JIT coverage (phase 2.5)

Built and measured on `electron/p66-jit-coverage` with `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`; the before column is `docs/electron-plan` `a5409ef8` built the same way. Both
binaries and both modes were interleaved in one round, `--runs 5`, median (best), one process per
measurement, `VERVE_CODE_CACHE=0`. Load before the round 1.3, 4.9, 4.5, 3.3 and 5.5 %, after it 2.6,
3.2, 3.7, 6.3 and 5.9 %.

| test | before, JIT | after, JIT | after, interp | JIT / interp | target (JIT) |
|---|---|---|---|---|---|
| `fib` | 273.6 (271.6) | 241.5 (225.4) | 355.7 (350.6) | 1.47× | ≤ 180 |
| `oo` | 1043.2 (1023.0) | 1041.5 (996.3) | 1104.2 (1067.9) | 1.06× | ≤ 600 |
| `typed` | 1020.6 (991.2) | 608.3 (604.5) | 1262.5 (1259.3) | 2.08× | ≤ 450 |
| `alloc` | 251.6 (249.0) | 260.7 (258.2) | 277.4 (267.1) | 1.06× | ≤ 220 |
| `vdom` | 63.8 (63.0) | 63.7 (63.1) | 68.5 (67.0) | 1.08× | ≤ 55 |
| `closure` | 581.4 (576.8) | 417.7 (413.5) | 627.6 (618.9) | 1.50× | ≤ 430 |

`closure` meets its target; the other five do not. What is left is `Call`/`Return` and the frame
(`fib`, `oo`), the `length` of a typed array (`typed`), and object construction and iteration, which
are the interpreter's (`alloc`, `vdom`); `docs/jit.md` "Results (phase 2.5)" has the causes and the
micro-probes.

### P71: `+=` on strings is linear (ropes)

`electron/p71-rope-strings` against `docs/electron-plan` `05f26157`, both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder. The machine was shared with three other workers, so
the load is given and only same-round numbers are compared. Round 1 (suite): load 23.0, 22.6, 20.7, 22.6, 21.6 % before,
23.4, 23.5, 20.7, 18.4, 15.7 % after. Round 2 (`concat` probe, `charCodeAt`): 18.1, 13.6, 13.9, 16.4, 13.6 % before,
57.4, 81.4, 66.9, 63.9, 60.1 % after (other workers started building during it).

`s += 'ab' + i`, interpreter and JIT alike (the JIT calls the same `Add`), process wall time of the loop, median (best) of 5
runs, 2 runs for the 320k baseline (one run is a minute); peak working set sampled every 5 ms from outside, so only its
order of magnitude is exact:

| appends | before, ms | after, ms | before, peak MB | after, peak MB |
|---|---|---|---|---|
| 20,000 | 30.4 (28.2) | 11.0 (10.3) | 15 | 12–15 |
| 80,000 | 795.3 (785.2) | 23.4 (23.1) | 15 | 14–26 |
| 320,000 | 62,744 (58,893) | 75.5 (74.3) | 17–20 | 66.5 |

Before grows 26× then 79× for 4× the input (quadratic); after grows 2.1× then 3.2× (linear). Memory is the price:
every append that is not merged keeps a 88-byte node (`size_of::<RopeString>()`) and its operand alive until the first read,
so 320k appends of a 6-code-unit piece peak at about 66 MB where the copying version, which freed each intermediate, stayed
near 20 MB. Leaf merging (the spike's phase 4) is the cure and was not done; see the package's status.

The suite's string tests, one round (`--runs 5`, median (best) ms, Node in the same round), before / after:

| test | Node | JIT before | JIT after | interp before | interp after |
|---|---|---|---|---|---|
| `concat` | 0.7 (0.6) / 0.7 (0.6) | 21.5 (21.1) | 5.7 (5.3) | 93.9 (53.0) | 5.8 (5.4) |
| `splitjoin` | 3.5 (2.7) / 3.3 (2.7) | 12.3 (11.1) | 12.1 (12.0) | 12.3 (11.9) | 13.4 (12.7) |
| `regex` | 2.4 (2.2) / 2.0 (1.9) | 38.3 (33.0) | 36.6 (26.7) | 35.0 (33.9) | 28.9 (27.2) |
| `json` | 177.5 (157.2) / 168.1 (151.5) | 1306.7 (1264.3) | 1453.5 (1253.9) | 1347.1 (1307.5) | 1286.4 (1257.7) |
| `tokenize` | 2.5 (2.4) / 2.5 (2.4) | 167.8 (162.1) | 171.7 (166.1) | 175.4 (170.8) | 191.2 (180.1) |
| `template` | 0.2 (0.2) / 0.2 (0.2) | 1.8 (1.8) | 2.2 (1.7) | 0.9 (0.9) | 0.9 (0.9) |

`concat` goes from 31× Node (JIT) to 8.1×; the target is 5×. What stops it is not the append: `x = 'ab' + i` alone
(number to string, one `concat_array`, no append) takes 2.3 ms of the 5.7, and the append of ready-made pieces 3.0 ms, in
the same process. Node does the whole loop in 0.25 to 0.85 ms. The other five are inside the round's noise: the medians
differ by up to 11 % (`json` JIT, whose best times are 1264 and 1254; `template` JIT 1.8 to 2.2 ms is 22 % of a sub-3-ms test whose best times are 1.8 and 1.7) and neither direction is consistent. Those tests do not
read a rope in their timed region and a flat string's `as_str` is untouched, so no regression is expected (inferred).

All 44 `scaling.js` probes, base then after, 3 runs, load 21.9, 22.4, 23.5, 27.2, 18.2 % before and 14.9, 17.8, 72.7, 93.5,
95.2 % after: `concat` goes from 16× (super-linear) to 4.2×; every other probe's t(4n) is within -13 % to +8 % of the base,
the +8 % being `set-delete-while-iterating`, which does not touch strings, so the spread is the noise of that round.

`charCodeAt` over a 16,000-code-unit string, 400 passes (6.4 M calls), five alternating pairs, ms: flat string 637 to 722
before, 655 to 688 after; the same loop over a string that came out of a rope 678 to 759 before (it was flat), 735 to 874
after. The rope's one-time flatten is in that number; the per-call cost is the flat string's.

### P70: iterators without result objects

Built and measured on `electron/p70-iterators` with `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`; the before column is `docs/electron-plan` `05f26157` built the same way. Both
binaries, both modes, `--runs 5`, median (best), one process per measurement, `VERVE_CODE_CACHE=0`.
Load before the base column 9–14 %, after it 18–50 %, before the after column 18–25 %, after it 23–27 %
(other builds on the machine); `alloc`, `concat` and `oo` were rerun alternating the two binaries
twice (load 9–30 %) and read within 2 % of each other.

| test | before, JIT | after, JIT | before, interp | after, interp |
|---|---|---|---|---|
| `vdom` | 71.2 (69.3) | 53.0 (49.1) | 73.4 (71.4) | 60.3 (54.5) |
| `mapset` | 39.7 (37.1) | 25.3 (21.9) | 43.7 (40.6) | 25.1 (23.3) |
| `fib` | 236.0 (232.7) | 238.9 (236.5) | 359.5 (351.4) | 381.3 (371.7) |
| `oo` | 1126.0 (1078.7) | 1097.0 (1051.8) | 1185.8 (1156.6) | 1304.7 (1197.3) |
| `typed` | 629.1 (614.4) | 651.5 (639.9) | 1255.5 (1240.3) | 1310.2 (1263.3) |
| `tokenize` | 164.4 (159.5) | 162.2 (157.1) | 183.2 (175.1) | 178.6 (175.2) |
| `closure` | 456.1 (417.3) | 435.4 (429.2) | 712.1 (669.3) | 645.2 (640.3) |

The other seven tests read within load noise. `for (const x of source)` per element, 1M elements: array
413.8 → 22.2 ns compiled and 431.4 → 71.8 interpreted; `Set` 611.9 → 24.0 and 546.9 → 46.9; string
501.6 → 72.8 and 578.2 → 98.8; `Map` (entries) 1317.1 → 546.5 and 1172.8 → 566.1; generators and
user-written iterators unchanged. `vdom` meets its 55 ms target (53.0). `docs/jit.md` "Results (P70)" and
`packages/P70-iterators.md` have the split and the loads.

### P72: a site cache for typed-array `length`

`electron/p72-jit-typed-length` against `docs/electron-plan` `345f1645`, both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder, the machine shared with three other workers. Suite round (`typed`,
`--runs 5`, before then after, load 13.4, 14.7, 19.6, 15.4, 15.3 % before and 16.5, 12.2, 11.4, 10.4, 14.4 % after): JIT 643.9 (618.5) to
368.2 (365.9) ms against a target of 450; interpreter 1304.1 (1291.0) to 1308.3 (1291.5), unchanged. Probe round (`s += f.length` on a
`Float64Array`, five alternating pairs, load 13.4 to 17.8 % before and 28 to 62 % after, the rise after the last process): 55.7 (54.5) to
17.0 (16.5) ns an iteration, 3.3 of it the loop, against a target of 15; element read 15.2 and write 17.6 ns, and a dense array's `length`
5.5 ns, unchanged. Details and what is left in `docs/jit.md` "Results (typed-array `length`, P72)".

### P74: string follow-ups (`'ab' + i`, rope buffers, string iteration)

`electron/p74-string-followups` against `docs/electron-plan` `c91b2938`, both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder, machine shared with three other workers (load given per
round; only same-round, adjacent numbers are compared). Probes are `scratchpad` scripts that time one loop in one process; median of
5 processes, JIT unless said.

| probe | before | after | load |
|---|---|---|---|
| `x = 'ab' + i`, ns per iteration | 117.0 | 72.0 | 17–37 % |
| `s += 'ab' + i` (`concat` probe), 20,000 appends, ns per append | 273.4 | 146.9 | 14–24 % |
| `s += 'ab123'`, 20,000 appends, ns per append | 112.8 | 79.1 | 14–24 % |
| `s += 'ab'`, 320,000 appends, ns per append | 128.1 | 86.6 | 14–24 % |
| 320,000 × `s += 'ab' + i`, peak working set, MB | 66.5 | 16.3 | 16–22 % |
| `for (const c of text)`, Latin-1, ns per element | 74.0 | 21.3 | 17–37 % |
| the same, with non-Latin-1 characters | 71.9 | 27.6 | 17–37 % |
| the same, interpreted (`VERVE_JIT=0`) | 109.0 | 38.1 | 46–57 % |

Suite `concat` (`run.sh --runs 5`, median (best) ms, Node 0.6 in the same round): JIT 5.6 (5.4) → 2.9 (2.8), 9.3× → 4.8× of Node;
interpreter 5.6 (5.5) → 3.1 (3.0), 9.3× → 5.2×. `splitjoin`, `regex`, `json`, `tokenize` and `template` read within noise (load 13–17 %
before, 55–68 % after, so the table in `packages/P74-string-followups.md` is the same pair, adjacent); the 44 `scaling.js` probes
too, `concat` 20k/80k 7.6 → 3.0 and 27.9 → 12.2 ms. Peak memory is 16.3 MB against an empty process's 16.2: the rope's last piece is a
shared byte buffer instead of a node per append. Appending UTF-16 pieces is not buffered (71.2 → 76.3 MB for 320k `s += '中' + i`).

### P69: the cost of calls and construction

Built and measured on `electron/p69-call-cost` with `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`; the before column is `docs/electron-plan` `c91b2938` built the same way. Both
binaries, both modes, `--runs 5`, median (best), one process per measurement, `VERVE_CODE_CACHE=0`,
interleaved per test. Load 15 % before the suite, 8 % after.

| test | before, JIT | after, JIT | before, interp | after, interp |
|---|---|---|---|---|
| `fib` | 243.5 (240.0) | 236.5 (234.8) | 369.9 (364.6) | 363.2 (362.1) |
| `oo` | 1106.4 (1067.5) | 724.8 (718.0) | 1205.7 (1180.1) | 850.1 (777.8) |
| `closure` | 450.5 (439.3) | 436.9 (432.3) | 667.7 (642.2) | 649.6 (634.2) |

`oo` meets its commit target (≤ 750); `fib` (≤ 195) and `closure` (≤ 350) do not, and the remaining
gap needs the `CallFrame` redesign `docs/jit.md` "Results (P69)" prices. The other eleven tests were
not rerun. `new C0()` (an empty class) 424.1 → 206.5 ns compiled and 453.9 → 214.5 interpreted,
`new E()` 216.2 → 169.7 and 243.7 → 173.7; `forEach`/`filter`/`map` with a callback of a few
instructions are no longer slower compiled than interpreted (93.3 against 91.8, 112.0 against 109.8,
202.7 against 201.0).

### P75: the prototype inline cache checks the prototype's shape

`electron/p75-prototype-inline-cache` against `docs/electron-plan` `2e902b19`, both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder, the machine shared with three other workers. Suite round (`--runs 7`,
before then after for each test, load 8.6–14.8 % before and 4–11 % after; median (best)):

| test | before, JIT | after, JIT | before, interp | after, interp |
|---|---|---|---|---|
| `oo` | 1079.4 (1056.5) | 1101.2 (1062.0) | 1144.9 (1088.9) | 1167.4 (1105.6) |
| `vdom` | 51.4 (49.0) | 53.2 (50.0) | 57.1 (52.6) | 53.9 (52.9) |
| `alloc` | 264.6 (257.4) | 267.1 (262.9) | 277.6 (266.6) | 284.8 (277.3) |
| `closure` | 435.3 (428.1) | 428.4 (421.6) | 646.8 (642.1) | 655.4 (637.2) |
| `tokenize` | 161.6 (157.6) | 164.2 (154.1) | 173.5 (167.7) | 168.0 (165.4) |

Medians move +2.0, +3.5, +1.0, -1.6, +1.6 % compiled and +2.0, -5.0, +2.6, +1.3, -3.2 % interpreted; `vdom` is the noisy one (its best
runs: +2.0 and +0.6 %). An earlier round, before the hit path was trimmed (one borrow, no shape upgrades, no prototype clone), had `alloc`
at +5.7 % compiled; two rounds at load 45–60 % were not used. Probe (a data property on the class prototype read in a loop, `-A`, five
runs, best, minus the loop): 17.4 to 19.0 ns compiled, interpreted unchanged within noise; `a.push` on an array in a loop of 300k:
86 to 88 ns an iteration. Details in `docs/jit.md` "Property caches and elements" and `packages/P75-prototype-inline-cache.md`.

### P78: a cache for the property an assignment creates (constructors)

`electron/p78-constructor-initial-shape` against `docs/electron-plan` `c28e026c`, both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder, the machine shared with three other workers. Suite rounds
(`--runs 7`, before then after for each test, median (best); load before/after each run 5–20 %), JIT then interpreter for `oo`:

| test | before, JIT | after, JIT | before, interp | after, interp |
|---|---|---|---|---|
| `oo` (round 2) | 721.9 (710.1) | 590.5 (574.5) | 811.6 (797.3) | 684.4 (683.7) |
| `oo` (round 3) | 708.6 (692.8) | 608.9 (591.0) | 789.1 (776.5) | 690.4 (676.1) |
| `alloc` (rounds 2, 3) | 254.2 → 245.7 | 249.7 → 253.4 | 264.1 → 261.5 | 259.2 → 254.6 |
| `vdom` (rounds 2, 3) | 48.3 → 52.7 | 52.2 → 50.4 | 52.2 → 52.3 | 53.2 → 50.9 |
| `closure` (rounds 2, 3) | 437.9 → 439.4 | 441.8 → 429.0 | 674.0 → 664.8 | 644.6 → 627.1 |
| `json` (rounds 2, 3) | 1233.5 → 1214.0 | 1237.4 → 1214.9 | 1226.6 → 1221.2 | 1223.4 → 1225.2 |

(Cells with an arrow are round 2 then round 3.) `oo` is 18 and 14 % faster compiled, 16 and 12 % interpreted: the commitment (650)
is met, the stretch (600) in one round of two. `vdom` left the ±3 % band in round 2 (+8.1 % compiled, -4.4 % in round 3); it has no
creating assignment, and two more rounds of eleven runs gave -3.2 and -2.5 %. `alloc` +0.4 and +2.7 % in those. The probe, ns per
iteration (load about 10 %, round 3): `new CX(i)` less `new C0()` 177 to 83, `new CX3(i)` 690 to 353, `new CX6(i)` 1122 to 503,
`new F(i)` 349 to 249. Details in `docs/jit.md` "Results (P78)" and `packages/P78-constructor-initial-shape.md`.

### P76: typed-array elements are read and written in compiled code

`electron/p76-jit-typed-elements` against `docs/electron-plan` `0cc1fa94` (P77 and P78 merged), both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder. Suite round (`--runs 5`, before then after for each test, machine load 1–7 % throughout
the round; median (best); compiled / interpreted):

| test | before, JIT | after, JIT | before, interp | after, interp |
|---|---|---|---|---|
| `typed` | 356.1 (353.1) | 211.0 (210.6) | 1206.0 (1187.0) | 1220.6 (1216.3) |
| `fib` | 222.7 (218.3) | 220.3 (219.5) | 350.2 (349.5) | 352.9 (348.3) |
| `alloc` | 243.3 (235.2) | 244.3 (235.3) | 244.5 (240.3) | 261.6 (247.2) |
| `concat` | 2.9 (2.9) | 3.0 (2.9) | 3.1 (3.0) | 3.3 (3.1) |
| `splitjoin` | 10.5 (10.3) | 11.3 (10.5) | 11.1 (10.4) | 10.1 (10.1) |
| `regex` | 23.3 (22.6) | 22.4 (22.1) | 23.6 (22.2) | 22.9 (22.2) |
| `json` | 1184.4 (1168.7) | 1180.9 (1161.4) | 1171.6 (1165.5) | 1171.9 (1167.9) |
| `sort` | 2231.2 (2225.9) | 2238.8 (2211.4) | 2574.6 (2543.1) | 2563.7 (2545.5) |
| `mapset` | 19.9 (19.0) | 21.1 (20.2) | 21.9 (21.1) | 21.1 (20.3) |
| `oo` | 563.2 (557.4) | 560.9 (557.3) | 669.4 (668.3) | 666.1 (651.4) |
| `vdom` | 48.9 (46.6) | 47.8 (45.5) | 49.9 (48.0) | 49.2 (48.5) |
| `tokenize` | 157.2 (151.7) | 152.3 (151.9) | 167.8 (165.2) | 168.3 (165.0) |
| `template` | 1.7 (1.7) | 1.6 (1.6) | 0.9 (0.9) | 0.9 (0.9) |
| `closure` | 422.5 (417.6) | 428.4 (423.0) | 622.5 (620.0) | 631.3 (618.5) |

`typed` goes from 356 to 211 ms, under both the commit target (280) and the stretch (220). The others are within ±3 % of their medians except
`splitjoin` (+7.6 %), `mapset` (+6 %) and `concat` (+3 %), tests of 1 to 20 ms that move that much between identical runs (an earlier round with nine
runs and the first version of the change had `splitjoin` 10.5 / 11.2 before against 11.1 / 10.5 after and `mapset` 20.9 / 21.8 against 21.1 / 21.6).
Probe (`-A`, the array in a local, best of 7, the loop itself 4.0 ns), ns per iteration:

| body | before | after |
|---|---|---|
| `s += a[i & 1023]`, `Float64Array` / `Int32Array` / `Uint8Array` / `Float32Array` | 14.5 / 15.0 / 15.0 / 15.0 | 8.5 / 9.0 / 9.0 / 9.5 |
| `a[i & 1023] = x`, `Float64Array` (number) / `Int32Array` / `Uint8Array` / `Float32Array` / `Uint8ClampedArray` | 16.5 / 15.0 / 20.5 / 16.5 / 17.0 | 6.5 / 6.5 / 6.0 / 6.5 / 6.0 |
| `u8[i & 1023] = 200.5` (a float into an integer kind) | 20.0 | 7.0 |
| a dense array, read / write | 7.5 / 6.0 | 7.5 / 5.5 |

Details and the cost in compile time in `docs/jit.md` "Results (typed-array elements, P76)" and `packages/P76-jit-typed-elements.md`.

### P83: UTF-16 appends go through the rope buffer

`electron/p83-utf16-rope-appends` against `docs/electron-plan` `2cb3f702`, both `cargo build --release --features jit --bin verve`,
`CARGO_BUILD_JOBS=6`, `VERVE_CODE_CACHE=0`, a throwaway profile folder. Machine shared with three other workers; the load (CPU %, five
1 s samples) was 16–24 % around the probe rounds and 16–26 % around the suite. Base and after are run adjacent, median of 5 processes,
JIT. Peak memory is the process's `PeakWorkingSet64`, polled every 2 ms from PowerShell while the script runs (an empty process is
16.7 MB).

| probe | before | after |
|---|---|---|
| 320,000 × `s += '中' + i`, loop ms | 67.7 (65.5) | 60.2 (57.4) |
| the same, peak working set, MB | 76.2 | 16.8 |
| 320,000 × `s += '中'`, loop ms | 43.2 (42.4) | 38.7 (38.3) |
| the same, peak working set, MB | 46.0 | 16.8 |
| 320,000 × `s += 'ab' + i` (Latin-1), loop ms | 62.0 (58.2) | 59.2 (57.8) |
| the same, peak working set, MB | 16.8 | 16.9 |
| 320,000 × `s += 'ab'` (Latin-1), loop ms | 38.3 (37.4) | 38.1 (36.9) |
| the same, peak working set, MB | 16.8 | 16.8 |

Suite (`run.sh --runs 5`, median (best) ms, JIT / interpreter), base then after: `concat` 3.2 (3.0) / 3.6 (3.1) then 2.9 (2.9) / 3.1 (3.1).
The other rows differ by up to a few percent in either direction with load 16–26 %, and none of them touches a rope of wide text. `fib`
and `alloc` re-run with 7 processes: `fib` JIT 230.6 (229.4) then 226.8 (225.3), `alloc` JIT 312.8 (266.9) then 287.5 (266.2). `sort` (numbers only)
read +6.5 % in the median of its re-run (2353.8 then 2507.8, best 2312.1 then 2407.5) with its interpreter column the other way round
(3076.5 then 2723.7): load, not the change.
