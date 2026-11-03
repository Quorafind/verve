# P63 让 `electron` 二进制能带 JIT

**依赖**：P02  **规模**：S  **修改**：`crates/verve-electron/Cargo.toml`、`crates/verve-gpui/Cargo.toml`、`crates/verve-electron/tests/`、`docs/electron/size.md`

## 目标

基线发现 `cargo build -p verve-gpui --bin electron --features verve/jit` 构建出的 `electron.exe` **不含 JIT**：`verve-gpui` 只把 `verve`
当 dev-dependency，`verve-electron` 以 `default-features = false, features = ["wasm"]` 引入 `verve`，于是没有任何办法在 `electron` 二进制里打开 JIT。
本包只做**能开**：加 feature、确认每个 isolate 线程上 JIT 真的工作、量体积。**默认开不开由 P42 第 5 节按体积规则决定**，本包不改默认。

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

（实现者完成后填写）
