# P63 让 `electron` 二进制能带 JIT

**依赖**：P02  **规模**：S  **修改**：`crates/verve-electron/Cargo.toml`、`crates/verve-gpui/Cargo.toml`、`crates/verve-electron/tests/`、
`docs/electron/size.md`、`boa/core/engine/src/vm/jit/mod.rs` 与 `boa/core/engine/src/vm/mod.rs`（只加一个访问器及其 re-export，见下）

## 目标

基线发现 `cargo build -p verve-gpui --bin electron --features verve/jit` 构建出的 `electron.exe` **不含 JIT**：`verve-gpui` 只把 `verve`
当 dev-dependency，`verve-electron` 以 `default-features = false, features = ["wasm"]` 引入 `verve`，于是没有任何办法在 `electron` 二进制里打开 JIT。
本包只做**能开**：加 feature、确认每个 isolate 线程上 JIT 真的工作、量体积。默认已定（2026-11-02，用户：
`electron` 默认带 JIT，硬上限 34 MB，见 [`README`](../README.md)「目标」与 `P42` 第 5 节），本包不改这条决定，只提供开关。

## 背景（先读）

- 根 `Cargo.toml`：`jit = ["boa_engine/jit"]` 及其注释。
- `docs/jit.md`「Switching it on and off」：`VERVE_JIT`、`VERVE_JIT_THRESHOLD`、`Runtime::set_jit_enabled`、`jit_stats()`；
  「Property caches and elements」最后一段：对象布局检查**每个线程第一次编译时**做一次，不通过就退回慢路径——isolate 线程上要确认它通过了。
- `src/lib.rs` 约 1413 行：`Runtime::new` 读 `VERVE_JIT`/`VERVE_JIT_THRESHOLD`。
- `crates/verve-electron/src/main_guest.rs`、`renderer_guest.rs`：两种 isolate 各自 `Runtime::new()`。
- [`size.md`](../size.md) 与 README 第 1 节的体积说明：JIT 约 +5.9 MB；`electron.exe` 不带 JIT 已是 26.3 MiB。

## 要做的事

- `verve-electron`：`[features] jit = ["verve/jit"]`。`verve-gpui`：`[features] jit = ["verve-electron/jit"]`，并让 `electron` 二进制随之带上。
  依赖上方注释写用途，照仓库风格。
- 主进程与渲染器 isolate 都不额外调用 `set_jit_enabled`：带 feature 时默认开、`VERVE_JIT=0` 关，与 `verve run` 一致。
- 测试（`crates/verve-electron/tests/jit.rs`，`#[cfg(feature = "jit")]`）：在主进程 isolate 与一个渲染器 isolate 里各跑一段热循环（含属性访问），
  断言 `jit_stats().live_functions > 0`，且对象布局检查通过（属性访问走了内联路径，用 `docs/jit.md` 现有的测试手段判断）；`VERVE_JIT=0` 时为 0。

  判断布局检查的手段是 `layout::object_layout()`，但它是 `pub(crate)`：`docs/jit.md` 里现有的用法都在 `boa_engine` 自己的测试里。
  测试在别的 crate 里，看不到它（`jit_stats()` 也不报告这件事——布局没通过时属性访问退成辅助函数，编译与进入都照常）。所以
  `boa/core/engine/src/vm/jit/mod.rs` 加一个 `pub fn object_layout_checked() -> bool`，与 `jit_unavailable()` 同类（告诉宿主 JIT 退化到了哪一步），
  不改变 JIT 的行为（第一次问到时会顺带在该线程做那次布局检查并缓存，与编译时做的一样）。**这是本包唯一的 `boa/` 改动，单独 commit 并记入 `boa/SU.md`。**
- 体积：`scripts/size-report` 量带与不带 `jit` 的 `electron.exe`，写进 `size.md`。
- 用 P60 的 `bench.js`（未合并时用其附录）在 `electron.exe` 的主进程里跑一遍（`electron <dir>`，`main.js` 里 `require` 它），JIT 开与关各 5 次，
  记录到 `perf-js.md` 或本文「状态」。

## 验收

- `cargo build --release -p verve-gpui --bin electron --features jit` 产物含 Cranelift（`size-report` 或字符串检查），不带 `--features jit` 时与现在逐字节同大小。
- `cargo test -p verve-electron --features jit --test jit` 通过；`cargo test -p verve-electron`（不带 feature）不受影响。
- `size.md` 有两种构建的体积；主进程跑 `bench.js` 的开/关数字。

## 不做

JIT 默认策略、`-jit` 发行变体（P42、P40）；JIT 本身的改进（P66）。

## 状态

2026-11-03，Windows 10 x64，Rust 1.95，`CARGO_BUILD_JOBS=6`，Ryzen 9 7950X（32 线程，与其他实现者共用）。

### 实际跑过的检查

| 命令 | 结果 |
|---|---|
| `cargo build --release -p verve-gpui --bin electron` | 27,602,944 字节；`cranelift` 0 处、`VERVE_JIT` 0 处 |
| `cargo build --release -p verve-gpui --bin electron --features jit` | 32,971,264 字节；`cranelift` 81 处、`regalloc2` 21 处 |
| `cargo test -p verve-electron --features jit --test jit` | 3 passed |
| `cargo test -p verve-electron`（不带 feature） | 6 个套件全 ok（8 + 7 + 5 + 12 passed，另两个空） |
| `cargo clippy -p verve-electron -p verve-gpui --all-targets --features jit -- -D warnings` | 无警告 |
| `cargo fmt --all --check` | 通过（逐 commit 也通过） |
| `bash scripts/style-check.sh` | `style: ok`（逐 commit 也通过） |
| `cargo test -p boa_engine --features jit --lib`（`boa/` 改动不能弄坏引擎） | 1188 passed |

CI 的 clippy 不带 `jit`，`tests/jit.rs` 与 `object_layout_checked()` 在 CI 里不被 lint——见下面给 P01/P42 的备注。

### 完成了什么

- **feature**：`verve-electron` 加 `jit = ["verve/jit"]`，`verve-gpui` 加 `jit = ["verve-electron/jit"]`，两处依赖上方都有用途注释。
  `cargo build --release -p verve-gpui --bin electron --features jit` 因此带上 Cranelift；不带 feature 时与改动前同一棵树构建的文件同长（27,602,944 字节），
  逐字节只差链接器写的时间戳与 CodeView GUID（12 字节）。默认值未改。
- **两个 isolate 都不额外调 `set_jit_enabled`**：`MainGuest::new` 与 `RendererGuest::new` 各 `Runtime::new()`，`VERVE_JIT` 由 `Runtime::new` 读，与 `verve run` 一致。
  代码没有改动。
- **测试**：`crates/verve-electron/tests/jit.rs`，三个用例，`#[cfg(feature = "jit")]`：
  - 主进程形状的 isolate（`Runtime::new()`，Hub 的 isolate 选项）与页面形状的 isolate（再 `verve_dom::install`）各跑一段热循环（每轮读写一次属性）；
  - 断言 `jit_enabled()`、`compiled > 0`、`entries > 0`、`live_functions > 0`，以及 `boa_engine::vm::object_layout_checked()` 为真——
    即对象布局在该 isolate 线程上检查通过（该线程第一次编译或第一次问到时就做，结果缓存），属性访问走的是内联路径；
  - 热循环的结果回到测试线程再与 Node 22.21.1 的同一段代码比对（`319660`），这样失败时看到的是值本身，不是「isolate 没能测完」；
  - `VERVE_JIT=0`：在子进程里重跑自己（变量只在 `Runtime::new` 时读一次），断言两个 isolate 的 `compiled`/`entries`/`live_functions` 全为 0，
    且子进程的输出里有 `test result: ok. 1 passed`——过滤器匹配不到任何测试时 libtest 也以 0 退出，不查这一条就会假通过。
  - 两个 guest 都不把 runtime 借出去，所以测试在「同形状的 isolate 线程」上建 runtime，而不是驱动 `MainGuest`/`RendererGuest` 本身；`electron.exe` 端到端的能力由下面的基准数字覆盖。
- **`boa/` 的一个访问器**（单独 commit，记入 `boa/SU.md`）：`vm::object_layout_checked()`。它只读 JIT 的状态、
  不改变行为，但第一次问到时会顺带在该线程做那次布局检查（缓存结果，与编译时做完全一样）——所以严格说不是"纯只读"。
  理由见上面「要做的事」。
- **体积**（写进 [`size.md`](../size.md)「JIT」）：不带 `jit` 27,602,944 字节 / 26.32 MiB；带 `jit` 32,971,264 字节 / 31.44 MiB；**+5,368,320 字节（+5.12 MiB）**。
  Cranelift 只在带 feature 的产物里（`cranelift` 81 处、`regalloc2` 21 处；另一个 0 处）。
  `scripts/size-report.sh` 只构建根 `verve` 包且没有 `-p`，量不到 `electron`，两个二进制是直接 `cargo build` + `wc -c` 量的——**P01 可以给它加一个 `--package`**。
- **基准**：P60 的 `bench.js` 尚未合并（`scripts/js-bench/` 不存在），用附录那份，原样不动。跑法是 `electron.exe <dir>`，`main.js` 里
  `process.argv[2] = BENCH_TEST; require(<bench.js>)`——`bench.js` 从 `process.argv[2]` 取测试名，直接 `require` 会跑全部 10 项。
  每项每次一个新进程；开与关**交替**跑，各 5 次；取中位与最好。Node 22.21.1 同机同次。

  | test | JIT 开 最好/中位 | `VERVE_JIT=0` 最好/中位 | Node 22 最好/中位 | 关/开（最好） |
  |---|---|---|---|---|
  | `fib` | 306.3 / 316.9 | 374.0 / 379.3 | 5.7 / 5.7 | **1.22x** |
  | `alloc` | 364.3 / 402.0 | 344.5 / 367.4 | 11.1 / 11.6 | 0.95x |
  | `concat` | 25.1 / 79.0 | 25.0 / 26.6 | 0.6 / 0.8 | 1.00x（离散大，见下） |
  | `splitjoin` | 580.3 / 582.0 | 584.1 / 592.8 | 2.9 / 3.0 | 1.01x |
  | `regex` | 789.7 / 922.9 | 664.5 / 846.6 | 1.8 / 2.0 | 0.84x |
  | `json` | 3127.0 / 3184.8 | 3130.6 / 3199.3 | 155.1 / 160.7 | 1.00x |
  | `sort` | 2585.4 / 2629.4 | 2583.2 / 2600.7 | 174.0 / 174.6 | 1.00x |
  | `mapset` | 426.4 / 431.9 | 432.5 / 435.8 | 4.9 / 6.0 | 1.01x |
  | `oo` | 1461.5 / 1542.3 | 1481.6 / 1675.9 | 2.9 / 2.9 | 1.01x |
  | `typed` | 5825.9 / 5932.6 | 5761.2 / 6135.9 | 4.0 / 4.1 | 0.99x |

  测量时的 CPU 负载：开始前 `\Processor(_Total)\% Processor Time` 55%，测完 36–39%（其他实现者在并行构建）。
  这份负载下 Node 仍与 `perf-baseline.md` 吻合（`fib` 中位 5.7 对 5.5，`sort` 中位 174.0 对 168.6，均在 +4% 内），
  但 Verve 的绝对值系统性偏高（本表 JIT 中位对基线第 3 节 JIT 列中位：`sort` +0.9%、`fib` +6%、`mapset` +7%、
  `splitjoin` +7%、`json` +24%、`oo` +28%、`alloc` +45%、`typed` +82%、`regex` +100%、`concat` +271%）。
  基线是 5–15% 负载下用 `verve.exe` 量的，这里是 36–55% 负载下用 `electron.exe` 量的。所以**这张表只能读关/开的比值，
  不要与基线逐格比较**；比值本身两侧同机同负载同时测，可用。
  `concat` 与 `regex` 的离散度超过 30%（`concat` 开 24.4–97.5 ms，`regex` 开 790–1174 ms）：交替重测 7 次后，
  `concat` 开/关互有胜负（54.1/25.5、25.3/53.3、24.7/25.4、97.5/56.9、30.2/25.5、24.4/24.3、24.9/25.2），
  `regex` 7 对里 6 对是开更慢（+2% 到 +19%），一对开快 32%——这一项整体确实倾向开更慢，但单次不可信。

### 没完成什么 / 已知问题

- **JIT 在这套基准上依然几乎没有收益，`fib` 1.22x 是唯一的明确赢面**，与 `perf-baseline.md` 的结论一致（那里 `fib` 是 1.24x）。
  `alloc`、`regex` 的「开更慢」在噪声范围内但不能说没有：这两项的代码路径里编译本身要花时间，而热点又几乎全在内建里。
  这属于 P66 的范围，本包不动。
- **布局检查的验证是「在 isolate 线程上建同形状的 runtime」**，不是在 `MainGuest`/`RendererGuest` 实例里。后者没有公开入口能把
  runtime 借出来跑一段脚本（`executeJavaScript` 是 P13/P20 的），要为本包开一个只供测试用的接口，代价大于收益。
  两个 guest 的构造路径与本测试一致（都是 `Runtime::new()`，页面再 `verve_dom::install`），所以结论适用；
  但**如果 P20 以后在渲染器里给 `Runtime::new()` 之外的东西设了 JIT 选项，这个测试不会发现**。
- `object_layout_checked()` 是 `boa/` 的公开 API；如果 P66 重构 `layout`，它要跟着改。
- 渲染器一侧只量了 JS 吞吐的量级（测试里的热循环），没有量真实页面的首帧延迟——那是 P42 的。

### 给其他包的备注

- **P40/P42**：按硬上限 34 MB（`--max-bytes 35651584`，README「目标」）：带 `jit` 的 `electron.exe` 是 32,971,264 字节
  （31.44 MiB），**余量 2,680,320 字节（2.56 MiB / 2.68 MB）**；不带 `jit` 是 27,602,944 字节（26.32 MiB），余量 8,048,640 字节。
  这是**还没接 GPUI 绘制（P30 起的布局与 DomView）、也还没接 verve-layout/verve-dom 的增量**之前的数字——那些都在余量里出，
  所以后续包每加一块都要盯住这 2.56 MiB，而不是只看到「还没超」。`[profile.dist]` 继承 release，两种都能用 `--features jit` 打。
- **P01**：`scripts/size-report.sh/.ps1` 只构建根 `verve` 包，没有 `-p/--package`，因此量不了 `electron`、`verve-dom` 等 workspace 成员。建议加一个可选参数。
- **P01/P42（CI 的 lint 盲区）**：CI 的 `cargo clippy --workspace --all-targets` 不带 `jit`，所以 `tests/jit.rs`（整体
  `#![cfg(feature = "jit")]`，不带 feature 时是空文件）和 `boa/` 的 `object_layout_checked()` 在 CI 里不会被 lint；
  它们只在本地带 `--features jit` 的 clippy 里检查过。要让 CI 覆盖，得给 clippy 任务加一个带 `--features jit` 的变体。
- **P60**：`run.sh` 驱动 Verve 二进制时如果要支持 `electron.exe`，需要一个「把测试名塞进 `process.argv[2]`」的入口；
  本包用的是 `main.js` 里 `process.argv[2] = …; require(bench.js)`。另外 `bench.js` 一次 `require` 会跑全部 10 项，
  驱动要按项拆开就只能靠 `process.argv[2]`，这依赖 `bench.js` 继续从那里取。
- **P66**：这次测量确认了两件事——`fib`（调用密集）有 1.2x，`oo`/`typed`（属性与元素密集、但热循环在一次顶层调用里）**没有**收益，
  与 `docs/jit.md`「Results (phase 2)」里 6.7x/9.0x 的微基准不冲突：那两组微基准的函数是被反复调用的。P66 应该先看 OSR 在这类「一个函数调用里跑完整个循环」的形状上有没有进入。
