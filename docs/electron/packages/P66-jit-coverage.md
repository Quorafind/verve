# P66 JIT 覆盖扩展

**依赖**：P60、P63（在 `electron` 里测量）；调用帧部分与 P65 错开合并  **规模**：L  **修改**：`boa/core/engine/src/vm/jit/`、`vm/call_frame/`、`vm/opcode/call/`、
`vm/mod.rs` 中与调用和进出 JIT 相关的部分、`docs/jit.md`、`boa/SU.md`、对应测试

## 目标

基线里 JIT 几乎没有收益：`fib` 1.24×，`alloc`/`typed` 约 1.05×，其余 1.00×；`docs/jit.md` 记录的 3–12× 只出现在属性/元素访问的微循环里。
本包先**解释为什么**，再按剖析做 `docs/jit.md`「Phase 2, left over」里收益最大的几项，让 JIT 对 P60 套件里的循环型负载
（`fib`、`oo`、`typed`、`alloc`、`vdom`、`closure`）有实在的倍数。

**参考目标**（第 1 步的分析出来后由实现者提出、总控确认）：上述各项 JIT / 解释器 ≥ 2×，其中 `typed` ≥ 5×（P65 合并后的解释器为基准）。

## 背景（先读）

- `docs/jit.md` 全文，特别是「Fast helpers」「Property caches and elements」「Tier-up」（含可盈利性规则与 OSR）、「Testing」「Results (phase 2)」「Roadmap」「Risks」。
- [`perf-baseline.md`](../perf-baseline.md) 第 3 节：`VERVE_JIT_THRESHOLD=0` 也没改变 `fib`/`oo`/`typed`（298 / 1186 / 3180 ms）。
- P60 的剖析配方；P65 的「状态」（解释器侧改了什么，JIT 的慢路径随之变快）。

## 要做的事

### 1. 解释现状（先交这一步，再动手）

对 `oo`、`typed`、`alloc`、`fib` 各给出：哪些函数被编译了（`jit_stats`）、OSR 是否进入了热循环、热循环里每条指令走的是内联、快速辅助还是 `jit_step`，
以及剖析下时间的分布。写进 `docs/jit.md` 新的一节「Why the baseline barely moved」，据此定参考目标与下面各项的顺序。

### 2. 按分析结果做（候选，顺序以第 1 步为准）

- **类型化数组元素内联**：`GetPropertyByValue`/`SetPropertyByValue` 遇到类型化数组与 `int32` 索引时在机器码里读写（检查同解释器快路径：未分离、在界内、元素类型）。
- **方法调用**：`o.m()` 从原型读 `m` 时内联，需要同时检查原型的形状（`docs/jit.md` 左项第 2 条）。
- **调用本身的成本**：更瘦的 `CallFrame`（每次调用少克隆/释放几个 `Gc` 句柄）、编译代码对普通函数（无环境、无 `arguments`）的直接调用路径、
  `this` 与函数本身不进值栈（左项第 1 条）。这部分改动解释器与 JIT 共用的调用机制，解释器也受益。
- **`new` 与对象字面量**：构造调用与 `this.x = …` 的形状转换在编译代码里的路径。
- **字符串与原生调用**：字符串 `+`、`length` 与下标；调用原生函数的固定成本（左项第 4 条）。
- **可盈利性与分层阈值**：如果分析显示热函数因可盈利性规则或阈值没被编译，调整规则并给出依据。

### 3. 每项都要

- 差分测试（解释、阈值 0 编译、热后经 OSR 编译三种结果一致）覆盖新内联的每种退回条件；模糊测试跑 `BOA_JIT_FUZZ_COUNT=3000`。
- `docs/jit.md` 的 Results 追加新的一节（phase 2.5 或按实际命名），格式照现有。

## 验收

- P60 套件 JIT 列：参考目标逐项「达标/差距」；`perf-js.md` 追加本包合并后的一行；P63 的 `electron` JIT 构建里主进程跑一遍同样的数字。
- `docs/jit.md`「Testing」列出的整套测试全部通过：`boa_engine` 测试在 `BOA_JIT_THRESHOLD=0` 下、Verve 全套在四种配置下、`tests/jit.rs`、
  模糊测试 3000 个、test262（除 `staging`）解释与强制编译两种模式逐项比较 **0 差异**（已知的计时类 `Atomics.notify` 例外照旧说明）。
- 发布构建体积增量写进 `size.md`。

## 不做

第 3 阶段（类型反馈、拆箱、二级编译、后台编译）——留到性能复盘决定；常量致盲与 Code Mode 的不可信配置；展开信息注册（除非剖析因此看不到编译帧，届时单独提）。

## 状态

（实现者完成后填写）
