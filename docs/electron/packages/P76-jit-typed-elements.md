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
## 状态

**完成，`typed` 达到冲刺目标。** 分支 `electron/p76-jit-typed-elements`，基线 `docs/electron-plan` `0cc1fa94`（P77、P78 合并后；`typed_array_element_*_shortcut` 的 `try_*` 改动已并入，
本包的机器码只读内存，不新增从辅助里取类型化数组的代码，所以不需要 `try_*`）。

**提交**：`perf(jit): Read and write typed-array elements in compiled code`（`typed_elements.rs` 布局探测、`compiler/typed.rs` 机器码、`TypedArray` 四个 `cfg(jit)` 的字段偏移常量、`boa/SU.md`）；
`test(jit): Check typed-array element accesses against the interpreter and Node`；本节与 `docs/jit.md`、`perf-js.md`、`size.md` 的 docs 提交。

**成本拆解**（`docs/jit.md`「Results (typed-array elements, P76)」有表）：JS 里一次元素读 15 ns（循环 4.0），写 17 ns；发布版 Rust 里单独量解释器的快捷函数：认出接收者 0.7、
借缓冲区取指针与长度 0.8、边界检查 0.5、按类型读出并装箱 3.5，读快捷函数合计 5.5，写 8.4。所以辅助调用的 11 与 12.5 ns 里约一半是辅助的外壳（`catch_unwind`、闭包、读写寄存器）。
**内联到哪**：五段全部内联（认出、取指针与长度、边界、读写、装箱），八种数字类型（Int8/Uint8/Uint8Clamped/Int16/Uint16/Int32/Uint32/Float32/Float64）；`BigInt64/BigUint64`（要分配）与 `Float16`（要转换）走辅助。

**守卫与为什么够**（`compiler/typed.rs` 顶部有完整说明；每次访问都从对象读，不缓存）：vtable 是类型化数组的（只有它有，故数据就是 `TypedArray`）；视图与缓冲区没被可变借用（写还要求缓冲区完全未被借用，同辅助里 `borrow_mut`）；
缓冲区是 `ArrayBuffer` 而**不是 `SharedArrayBuffer`**（守卫排除，交给辅助：它的长度是原子的）且未分离（字节指针非零）；视图是定长的（长度跟踪的视图交给辅助；`Option::None` 的载荷未规定，所以先测「是否有长度」那个字）；
种类在表里；视图在**当前**缓冲区内（`byteOffset + length*size <= 字节数`，可变长缓冲区缩小后失败）且下标 `< length`。写入只在值是数字时内联（不会 `ToNumber`，所以不会跑用户代码；带 `valueOf` 的对象走辅助，它先转换后看缓冲区，与规范同序）。
任何守卫失败都跳到现有辅助。布局（`TypedArray` 的缓冲区、种类、偏移、长度，`BufferObject` 两个字，`ArrayBuffer` 的指针与长度）在对象布局检查时用真实对象找出并核对，对不上就不内联。
写入语义：`Float64` 原样；`Float32` 用 `fdemote`（与 `as f32` 同一条指令）；整数种类取 `ToInt32` 的低位（`int32_of`，≥2^63 的浮点交给辅助）；`Uint8Clamped` 只内联 `int32`（钳到 0..255），浮点交辅助。读出与解释器一致：所有数字种类都装成 `f64`，`NaN` 取规范值。

**测试**（`tests_typed_elements.rs`，三种模式一致且与 Node 22 逐行一致）：九种数字类型与两种 BigInt、子类、范围内/越界/负数/小数/字符串/超大下标、奇怪的值、带偏移的视图、循环中途分离与分离后、可变长缓冲缩小增长（跟随长度/定长/带偏移）、
`valueOf` 里分离或缩小缓冲区、`SharedArrayBuffer`（含可增长）、不可扩展、原型上的元素与访问器、共享一个缓冲区的多个视图（`NaN` 位型、`Float32` 舍入、`Uint8Clamped` 边界、整数回绕）、同一站点遇到类型化数组/数组/代理/对象、循环中垃圾回收；
另有按种子的随机测试（种类 × 下标 × 值 × 读写，三种模式一致）。布局探测另有一个测试。生成式模糊测试不产生类型化数组，所以加了随机测试。拿掉三个守卫各试过：去掉「在缓冲区内」测试失败；去掉「不是 SharedArrayBuffer」测试进程访问违例；
去掉「定长」测试**不失败**（`None` 的载荷实际是 0，下标永远不 `< 0`），该守卫靠的是载荷未规定这一点，不是测试。

**数字**（`docs/electron/perf-js.md` 有全表；基线 `0cc1fa94` 与本分支各一个发布版，同一轮，`--runs 5`，机器负载 1–7 %）：`typed` JIT **356.1 (353.1) → 211.0 (210.6) ms**（承诺 280，冲刺 220，均达成），解释器列不变。
探针：读 14.5 → 8.5 ns，写 16.5 → 6.5 ns（循环 4.0）。其余 13 项中位数在 ±3 % 内，除 `splitjoin`（+7.6 %）、`mapset`（+6 %）、`concat`（+3 %）：都是 1–20 ms 的测试，相同二进制两次之间也这么动
（较早一轮九次取中位数：`splitjoin` 10.5/11.2 对 11.1/10.5，`mapset` 20.9/21.8 对 21.1/21.6）。体积 +38,912 字节（+0.146 %，`size.md`）。

**代价：编译时间。** 含 40 次元素访问的函数编译慢 55–60 %（400 个这样的函数，阈值 0：13.1–16.9 s 对 20.0–23.7 s）：每个元素访问站点多出约 35 个基本块。把守卫折成三个分支、与数组路径共用寄存器写回只省了约 5 %。
套件里热函数的元素访问不多，没看出来，但元素访问密集的函数会感觉到。下一步：每个函数一份类型化数组路径（各站点跳入），或只在解释器见过类型化数组的站点编译（`docs/jit.md` Risks）。

**验证**（在 `0cc1fa94` 之上重放后的树，`CARGO_BUILD_JOBS=6`）：`boa_engine` 默认 1245 个、`--features jit` 1307 个、`BOA_JIT_THRESHOLD=0 --features jit` 1307 个，加集成测试全过；`BOA_JIT_FUZZ_COUNT=3000` 过（跑了 1 个测试，175 s）；
`boa` 里 `boa_engine` 有无 `jit` 的 `clippy --all-targets -D warnings` 过，`cargo fmt --all --check`（boa 与根）过，`style-check` 过，根 `cargo clippy --workspace --all-targets -D warnings` 过；`cargo test -p verve`（450）过；
`--features jit` 下 `tcp_socket_options_and_inactivity_timeout_are_chainable` 在整套并发时两次出 `EIO`（os error 10022，与 P66、P72 记的是同一个），单独跑有无 `jit`、`VERVE_JIT=0` 都过，其余 452 个过。
test262 `built-ins/TypedArray`、`TypedArrayConstructors`、`ArrayBuffer`、`Atomics`（`-vv`，`BOA_JIT=0` 对 `BOA_JIT_THRESHOLD=0`，含严格模式各一项）解释与强制编译**逐项 0 差异**，全部 0 失败；基线构建与本分支构建逐项比较也是 **0 差异**（8 组）。
（test262 的前后比较是在基线 `c28e026c` 与提交 1、2 上做的，之后只变了基线里的 P77/P78，没有重跑。）

**解释器老 bug（与本包无关，没修）**：`to_int8`、`to_uint8`、`to_int16`、`to_uint16` 先 `as i64`（≥2^63 时饱和）再取余，`new Int8Array(1)[0] = 3.4e38` 得 `-1`，规范与 Node 得 `0`。辅助与编译码与解释器一致（编译码把 ≥2^63 的浮点交给辅助），
测试里这类值只在三种模式间比较（`huge.js`）。`-NaN` 的符号位也与 Node 不同（Boa 的值只存规范 `NaN`），测试里没放。

**给其他包的备注**：`ObjectLayout` 多了 `typed: Option<TypedLayout>`；`array_and_index` 多一个「不是数组」的目标块；`tests_typed_length.rs` 的 `context_with_natives` 改成 `pub(super)`。
未做：长度跟踪的视图、`SharedArrayBuffer`、`BigInt` 与 `Float16`（都走辅助，见上）。
