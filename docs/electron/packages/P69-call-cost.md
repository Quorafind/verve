# P69 调用与构造的固定成本

**依赖**：P65、P66（均已合并）  **规模**：L  **修改**：`boa/core/engine/src/vm/call_frame/`、`vm/opcode/call/`、`vm/opcode/new/`、
`vm/mod.rs` 中压帧、弹帧、`handle_return` 与进入解释循环的部分、`builtins/function/`（`function_call`、`function_construct`、
`enter_ordinary_function`）、`object/` 中原生代码调用 JS 函数的入口、`vm/jit/call.rs`（跟着新帧走）、`docs/jit.md`、`boa/SU.md`、对应测试

## 目标

P66 把 JIT 能做的内联都做了，剩下的差距在解释器与 JIT **共用的调用机制**上：一次 JS→JS 调用约 75 ns，其中 `Call` 处理函数约 35、
`Return` 约 20，是建帧和拆帧的成本；`new F(i)` 编译后 419.8 ns、解释 415.3 ns，成本在 `function_construct` 与它建的帧；
原生代码回调 JS（`map`/`filter`/`reduce`、`sort` 的比较函数）每次约 111–185 ns。这三条路径决定了 `fib`、`oo`、`closure`
以及一切带调用的应用代码的速度。

**参考目标**（第 1 步之后由实现者提出、总控确认；单位 ms，P60 套件，JIT 列，P66 合并时的数字在括号里）：
`fib` ≤ 180（241.5）、`oo` ≤ 600（1041.5）、`closure` ≤ 330（417.7）。解释器列同时变快，各项另记 JIT/解释器比值。
硬规则：套件里任何一项的 JIT 列都不得比解释器慢，解释器列不得比 P66 合并时慢。

## 背景（先读）

- `docs/jit.md`「Why the baseline barely moved」与「Results (phase 2.5)」：调用成本的测量与微探针表。
- `P66-jit-coverage.md`「状态」两步的全部内容，特别是「Left out」里 `CallFrame` 与 `new` 两条的数字。
- `P65-objects-typed-arrays.md`「状态」里解释器每次 JS→JS 调用约 136 ns、首属性定义约 319 ns 的分析，以及 `oo` 中
  `new Class()` 144 ms 对 `Object.create` 30 ms 的对比。
- `vm/call_frame/mod.rs` 的 `CallFrame`（现在每帧带 `code_block`、`active_runnable`、`environments`、`realm` 四个 `Gc`/引用计数句柄，
  以及 `iterators`、`binding_stack` 两个 `ThinVec`）；`builtins/function/mod.rs` 的 `function_call`、`enter_ordinary_function`、
  `function_construct`；`vm/jit/call.rs`（P66 的 `jit_call`，直接调 `enter_ordinary_function`）。

## 要做的事

### 1. 拆账（先交这一步，再动手）

用 P66 的办法（进程内计数与 `rdtsc` 计时、微探针 A/B 交替、5 个进程取中位与最好；不用 samply/wpr/xperf）给出：

- 一次 `zero()` 调用（编译与解释）里 `Call`、`enter_ordinary_function`、`push_frame`、`this` 绑定、环境建立、`Return`、`handle_return`、
  `pop_frame` 各占多少；每帧克隆和释放了哪些句柄、各多少纳秒。
- `new F(i)`、`new class {}`、`new class extends Base {}` 同样拆开：`function_construct` 里对象分配、原型查找（`get_prototype_from_constructor`）、
  帧、`this` 初始化、`CheckReturn` 各占多少。
- 原生→JS：`[].map(f)` 与 `sort` 比较函数每次回调的路径（`JsObject::call` → 进入解释循环）各段成本。

写进 `docs/jit.md` 新的一节「Where a call goes」（英文，照该文件文风），在「状态」里提出目标与第 2 步的顺序。

### 2. 按拆账做（候选，顺序以第 1 步为准）

- **更瘦的帧**：不再每次调用克隆能从 `code_block` 或调用者帧得到的句柄（`realm`、`active_runnable` 与调用者相同时共享）；
  `iterators`、`binding_stack` 改为按需分配；`loop_iteration_count` 等字段合并。目标是 `push_frame`/`pop_frame` 不碰引用计数的常见情形。
- **普通函数的直接进入**：无 `arguments` 对象、无参数表达式、无独立函数环境的函数（编译器已知道），跳过环境建立与 `this` 的通用分支。
- **构造调用的快路径**：`function_construct` 对普通构造器（原型是普通对象、`prototype` 属性未被改成访问器）缓存原型查找，
  `this` 直接按构造器的初始形状分配；派生类构造器的 `super()` 走同一条路。
- **原生→JS 回调**：原生代码重复调用同一个 JS 函数时（`map`、`forEach`、`sort`、`reduce`），避免每次都重新进入解释循环的固定开销
  （`run_depth`、`host_call_depth` 记账、帧外的状态保存）；只改调用入口，不改各内建的算法。
- **JIT 跟进**：`vm/jit/call.rs` 随新帧调整；若帧变化让编译代码能直接调用编译代码（不经 `jit_step`），在本包里做并给出收益。

### 3. 每项都要

- 差分测试（解释、`BOA_JIT_THRESHOLD=0` 编译、热后经 OSR 编译三种结果一致），覆盖：异常穿过调用链、递归上限（`RangeError`）、
  中断、生成器与 async 函数、箭头函数的词法 `this`、`arguments` 对象（映射与非映射）、默认参数与参数解构、`new.target`、
  派生类构造器未调 `super()`、构造器返回对象与原始值、`Reflect.construct` 带不同 `newTarget`、绑定函数与 Proxy 作为被调用者。
- 错误栈与 `Error.prototype.stack` 的帧信息不变（现有测试覆盖到的逐字不变）。
- `docs/jit.md` 的 Results 追加一节，格式照现有。

## 验收

- P60 套件 JIT 与解释器两列：参考目标逐项「达标/差距」；`perf-js.md` 追加本包合并后的一行。
- `docs/jit.md`「Testing」列出的整套测试全部通过：`boa_engine` 测试（默认与 `BOA_JIT_THRESHOLD=0`）、Verve 全套四种配置、`tests/jit.rs`、
  模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、test262（除 `staging`）解释与强制编译两种模式逐项比较 **0 差异**（计时类 `Atomics.notify` 例外照旧说明）。
- 发布构建体积增量写进 `size.md`。

## 不做

迭代器协议与 `for…of`（P70）；类型化数组 `length`（P72）；字符串（P71）；JIT 第 3 阶段（类型反馈、内联被调用者）；
各内建自身的算法。改 `vm/jit/` 只限调用相关的文件，与 P70、P72 冲突时后合并者 rebase。

## 状态

未开始。
