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

未开始。
