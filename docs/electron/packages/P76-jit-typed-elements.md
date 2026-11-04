# P76 JIT：类型化数组元素读写内联

**依赖**：P72、P75（均已合并）  **规模**：M  **修改**：`boa/core/engine/src/vm/jit/`（新模块与编译器里元素访问的分支）、
`builtins/typed_array/` 与 `builtins/array_buffer/`（只限给机器码读布局用的只读访问器，不改语义）、对应测试、`boa/SU.md`、
`docs/jit.md`（「Property caches and elements」「Risks」）、`docs/electron/perf-js.md`、`docs/electron/size.md`

## 目标

P72 把类型化数组 `length` 从 52 ns 降到 14 ns 后，`typed` 一轮约 61 ns，其中两次元素读、一次元素写占大头：读 15.3、写 17.8 ns，
减去 3.3 ns 的循环是 11.9 与 14.5 ns，都是一次带快捷路径的辅助调用。稠密数组的 `length` 内联后只要 2.0 ns。
本包把**类型化数组的整数下标读写**放进机器码：守卫通过时直接按元素类型读写缓冲区，其余情形照旧走辅助调用。

目标（P60 套件 `typed`，JIT，中位数）：**承诺 ≤ 280 ms，冲刺 ≤ 220 ms**（合并时 368 ms）。元素读、写微探针各给前后纳秒数。

## 背景（先读）

- `P72-jit-typed-length.md`「状态」第 3 项的测量与估计，以及 `vm/jit/typed_length.rs` 的站点缓存与校验论证。
- `P75-prototype-inline-cache.md`「状态」与 `docs/jit.md`「Property caches and elements」：形状校验的边界。
- `builtins/typed_array/`（元素类型、`[[ByteOffset]]`、`[[ArrayLength]]` 与长度跟踪、`IsValidIntegerIndex`）、
  `builtins/array_buffer/`（分离、可变长、`SharedArrayBuffer`）。
- 现有元素访问辅助（`vm/jit/helpers.rs` 里类型化数组的快捷路径）：内联版本必须和它在每一种情形下给出同一结果。

## 要做的事

1. **先测量再定方案**：把一次元素读写拆成「认出接收者是类型化数组」「取缓冲区指针与长度」「边界检查」「按类型读写与装箱」几段，写进「状态」。
   据此决定内联到哪一层（例如只内联最常见的几种元素类型，其余走辅助）。
2. **守卫**（由实现者定，写明为什么够）：接收者是哪种元素类型；缓冲区没有分离；下标是整数且在当前长度内（长度跟踪与可变长缓冲要每次重算或有可证明的校验）。
   守卫失败一律走现有辅助，不得自己处理边角情形。
3. **写入**：只有值已是数字（或目标类型能直接收的值）时内联；需要 `ToNumber`/`ToBigInt` 的（会运行用户代码）一律走辅助。
   `Float32` 的舍入、`Uint8Clamped` 的钳位与取整、`-0`、NaN 的写入结果必须与解释器逐位一致。
4. **读出**：`Float64`/`Float32` 读出的值装箱方式与解释器一致（整数值的浮点是否变 int32 表示，不得影响可观察结果）；`BigInt64`/`BigUint64` 可以不内联。
5. **共享内存**：`SharedArrayBuffer` 背后的数组，读写语义（非原子）与解释器一致；如果内联在这里没有把握，守卫排除它，写明。

## 测试

- 差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致，并与 Node 输出一致）：十一种元素类型及其子类；越界读（`undefined`）与越界写（无效果）；
  负数、小数、`-0`、`NaN`、超大下标、字符串下标；循环中途分离缓冲；可变长缓冲在循环中途缩小与增长、长度跟踪视图；`byteOffset` 非零的视图；
  写入需要转换的值（带 `valueOf` 的对象，在 `valueOf` 里分离缓冲）；`Float32` 舍入、`Uint8Clamped` 钳位边界（-1、255.5、256、`NaN`）；
  同一站点交替遇到两种元素类型与普通数组；循环中途触发垃圾回收。
- 模糊测试 `BOA_JIT_FUZZ_COUNT=3000` 照旧通过；若生成器不产生类型化数组，为本包加一个小的随机测试（元素类型 × 下标 × 值）。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试、clippy `-D warnings`（有无 `jit`）、`cargo fmt`；
  根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `built-ins/TypedArray`、`built-ins/TypedArrayConstructors`、`built-ins/ArrayBuffer`、`built-ins/Atomics`（除计时类）解释与强制编译逐项 0 差异，前后 0 回退。
- P60 套件：`typed` 达到承诺目标；其余各项不变慢（同一轮，中位差 ±3%）。`perf-js.md`、`size.md` 各追加一行。

## 不做

普通数组元素的进一步内联；`DataView`；`Atomics` 的内联；解释器侧的改动（除只读访问器外）。

## 状态

未开始。
