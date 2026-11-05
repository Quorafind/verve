# P81 JIT：类型化数组元素内联的编译时间

**依赖**：P76（已合并）  **规模**：S–M  **修改**：`boa/core/engine/src/vm/jit/compiler.rs`、`vm/jit/compiler/typed.rs`、`vm/jit/typed_elements.rs`，
如需站点反馈则 `vm/jit/layout.rs` 或站点缓存（只限记录「见过类型化数组」），对应测试、`boa/SU.md`、`docs/jit.md`（「Risks」）、`docs/electron/perf-js.md`

## 目标

P76 把类型化数组元素读写放进机器码后，每个元素访问站点多出约 35 个基本块：含 40 次元素访问的函数编译慢 55–60 %
（400 个这样的函数、阈值 0：13.1–16.9 s 对 20.0–23.7 s）。P60 套件看不出来，但元素访问密集的代码（解析器、图像处理、虚拟 DOM 的 diff）会在编译上多花时间。
本包把这部分编译代价降回来，同时保住 P76 的收益。

目标：上面那个 400 函数的测量回到 P76 之前的 **+15 % 以内**；`typed` 保持 **≤ 220 ms**；元素读写微探针不回退超过 1 ns。

## 背景（先读）

- `P76-jit-typed-elements.md`「状态」：编译时间的测量方法与数字、「下一步」的两个想法；`docs/jit.md`「Risks」里对应的一条。
- `vm/jit/compiler/typed.rs`（每个站点生成的守卫与读写）、`vm/jit/compiler.rs`（元素访问怎样分派到数组路径与类型化数组路径）。

## 要做的事

1. **方案**（由实现者定，写明取舍）。候选：
   - 只在解释器（或之前的执行）见过类型化数组接收者的站点生成内联路径，其余站点照旧走辅助；需要一个廉价的站点反馈位。
   - 每个函数只生成一份类型化数组读写的代码块，各站点跳入并带回结果，而不是每个站点各一份。
   - 精简守卫与装箱生成的基本块数。
   每个候选给出编译时间与运行时间的实测，选一个或组合。
2. **不变量**：P76 的守卫一条不少（vtable、借用、`ArrayBuffer` 而非共享、未分离、定长视图、边界、写入值是数字）；守卫失败照旧交给辅助。
3. 若选站点反馈：站点先在辅助里跑、后来才见到类型化数组的情形（例如先遇到普通数组），要有重新编译或至少不出错的说明。

## 测试

- P76 的差分测试全部照旧通过（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR，与 Node 一致）；加：同一站点先见普通数组、后见类型化数组；
  先见类型化数组、后见普通数组与对象。
- 编译时间的测量脚本放在 scratchpad，命令与结果写进「状态」。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `built-ins/TypedArray`、`built-ins/TypedArrayConstructors`、`built-ins/ArrayBuffer` 解释与强制编译逐项 0 差异。
- P60 套件全部同一轮比较：`typed` ≤ 220 ms，其余中位差 ±3%；`perf-js.md` 追加一行。

## 不做

其他指令的编译时间；分层编译（基线 JIT）。

## 状态

**已完成。** 提交：`perf(jit): Inline typed-array element access only in functions that have seen one`（3fb380e1：`JitSlot` 一个标志、两个处理器的类型化数组快捷路径置位、`typed_path`、`JitOptions::typed_elements_everywhere` / `BOA_JIT_TYPED_ALL=1`、`boa/SU.md`）；
`test(jit): Check typed-array feedback with plain-first and typed-first sites`（883621e6）；本节与 `docs/jit.md`、`perf-js.md` 的 docs 提交。

**方案与取舍。** 选「见过类型化数组才编译内联路径」，反馈粒度是**函数**（`CodeBlock` 的 `JitSlot` 上一个 `Cell<bool>`），不是站点。先做了站点位图（按 pc），测出 `typed` 从 227 变成 366 ms：
`typed` 是一个箭头函数，在第一个循环里经 OSR 编译，后面两个循环的站点解释器还没跑过，没有反馈，整段落回辅助。函数粒度没有这个问题，也更简单（一个布尔，处理器里一条 `set`）。
未做「每函数一份共享类型化路径」：对见过类型化数组的函数仍付满额编译代价（下表），但这类函数少，且共享块要把装箱结果带回各站点的寄存器写回，改动和风险大；留给之后。
**反馈晚到的情形：** 编译时函数还没见过类型化数组，之后才见到：该函数的元素访问留在辅助（数组路径照常内联；类型化数组在守卫处 miss 到辅助，约 11 ns 一次而不是 4 ns），**结果不变**，不重新编译。
站点先见数组后见类型化数组、或相反，也同样正确。P76 的全部守卫一条未动（`typed.rs` 未改）。`BOA_JIT_TYPED_ALL=1` 可强制处处内联（测试的强制编译模式用它）。

**编译时间**（`scratchpad\p81\`：`minct.sh`、`cpu.sh`、`compile-calls.js`；P76 的 `compile.js`；发布版 `verve`，取 5 次交错运行里**进程 CPU 时间**的最小值，机器负载 14–25 %）：
| 400 个含 40 次元素访问的函数，阈值 0，普通数组 | CPU ms |
|---|---|
| P76 之前（`0cc1fa94`） | 13,828 |
| P76（`2cb3f702`） | 19,406（+40 %） |
| 本包 | **13,484（−2.5 % 对 P76 之前，目标 +15 % 以内）** |

| 200 个函数 × 40 访问，调 6 次，阈值 3（反馈在编译前已有） | P76 前 | `2cb3f702` | 本包 |
|---|---|---|---|
| 接收者是 `Float64Array` | 6,781 | 10,234 | 10,188（它们需要内联路径，代价不变） |
| 接收者是普通数组 | 6,813 | 10,219 | 6,719 |

**运行时间。** `typed` JIT，交错两轮 `--runs 7`，`2cb3f702` 对本包：222.2 / 237.9 对 219.5 / 224.3 ms（≤ 220 的目标在噪声边缘：本包最优 214.7–221.2，P76 的承诺值 211 是空闲机器上的）。
全套 `--runs 5`（两个二进制各跑一遍，机器负载 23–56 %，别的工作者在编译，所以噪声大）：`2cb3f702` 对本包 中位数 ms：fib 238.7/229.7、alloc 268.2/257.5、concat 3.0/3.0、splitjoin 11.7/11.9、regex 25.8/24.9、json 1385/1287、sort 2568/2308、mapset 27.6/25.0、oo 725.9/606.6、typed 225.6/271.6（最优 223.7/215.7，一次离群）、vdom 60.1/60.6、tokenize 203.8/182.1、template 1.8/1.8；`closure` 的行在汇总输出里被截掉，未比较。
没有哪一项系统性变慢（本包不改任何不含类型化数组的编译产物之外的东西；改动的处理器多一次 `Cell::set`，仅在类型化数组快捷路径命中时）。较早一轮（负载 13–25 %）：fib 227.6/244.3 反向，同样是噪声量级。微探针：处理器与机器码路径未变，编译后的类型化路径逐字节同 P76，未另测。

**验证**（`CARGO_BUILD_JOBS=6`，`E:` 盘中途写满一次，清了自己的 `release` 与 `incremental` 后重跑全部）：`boa_engine` 默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit` 全过（含新增两个测试，`only_functions_that_saw…` 验证计数 2 与 4）；
`BOA_JIT_FUZZ_COUNT=3000` 过（1 个测试，188 s）；`boa` 里 `boa_engine` 有无 `jit` 的 clippy `-D warnings` 过（第一次有一个 `unnecessary_lazy_evaluations`，已改）；根 `cargo clippy --workspace --all-targets -D warnings` 过；
`cargo test -p verve` 有无 `jit` 过；`cargo fmt`（boa 内与 `--all --check`）、`style-check` 过。
test262 `built-ins/TypedArray`（1446，1438 过）、`TypedArrayConstructors`（738，736 过）、`ArrayBuffer`（221，192 过）：`BOA_JIT=0`、`BOA_JIT_THRESHOLD=0`、`BOA_JIT_THRESHOLD=0 BOA_JIT_TYPED_ALL=1` 三种，0 失败、0 panic、逐项 0 差异（`-vv` 输出比较）。
注意：单用 `BOA_JIT_THRESHOLD=0` 不再走内联类型化路径（没有反馈），所以第三种才是检验机器码的那一遍。

**留给之后。** 共享的每函数类型化路径（见上）；晚到反馈触发重新编译。
