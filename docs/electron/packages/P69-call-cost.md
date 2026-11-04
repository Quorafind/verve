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

### 第 1 步（拆账）：已完成；第 2 步未开始，等总控确认目标与顺序

只交了拆账，没有改引擎。完整的表在 `docs/jit.md` 新增的「Where a call goes」一节；下面是结论、建议的目标与顺序。

**测量条件**：release `verve`（`--features jit`），Windows x86-64，Ryzen 9 7950X（TSC 4.5 GHz），`docs/electron-plan` 在 `05f26157`。
机器上有另外三个构建者，每张表只在自己那一轮内比较。负载（前 → 后）：微探针 30% → 20%；P60 套件 35% → 24%；八倍重复探针
15–45%（分 6 轮）；单计时器三轮 26% → 26%、22% → 49%、95% → 16%（最后一轮开头是别人的瞬时尖峰，那一组读数与解释器一致，没有异常迹象）。
手段：微探针 A/B（同一进程、交替、5 个进程取中位与最好）、「把某一小块多做 8 次再除以 8」的重复探针、一次只开一个的 `rdtsc` 计时器、
进程内计数器（指令数、函数环境数）。这些都是临时补丁（备份在草稿目录的 `p69/instrumentation.patch`，**未提交**）。
**探针开销**：补丁里所有计时器都关着时，编译路径的 `zero()` 84.3 ns 对干净构建 69.3 ns（+15），解释路径 102.5 对 101.3（+1）；
所以编译路径的计时器读数只当份额看。计时器本身一对读 30 周期，嵌套计时器会互相遮住（第一次全开的读数每块都是 30 周期，作废），
所以只用单个计时器，读数减 30 周期，小于约 50 周期的块只是下界。重复探针的本底 1.1 ns 没有扣。

**拆账结论**（ns；编译 / 解释；括号内为测量或推断）：

| 路径 | 总量 | 构成 |
|---|---|---|
| `zero()` JS→JS | 67 / 85（`fib` 每次 90 / 135） | 解释：7 条指令分派 ≈ 26（推断）+ `Call` 处理函数 30.8 + `Return`/`handle_return` 15.7 + 未归因 13；编译：`jit_call` 73（含被调函数）= `compiled_callee` 4.6 + `enter_ordinary_function` 15.6 + `finish` 33.4（嵌套 `execute` 27.3，其中 `Return` 一步 17.2）+ 约 20 未归因（推断）。孤立量出的簿记合计 37 / 45：7 对句柄克隆 15–20（每对 2–3）、`check_runtime_limits` 5.5–6.3、影子栈 2–3、`this` 3–4、寄存器 1–3、返回值 2–3、`__call__` 的 Pending 3、`downcast_ref` 2.2。**`CallFrame::new` 本身是 0**（`iterators`/`binding_stack` 两个空 `ThinVec` 不花钱） |
| `new E()` | 232 / 233（最好） | `function_construct` 180：`get_prototype_from_constructor` 66–70（一次通用 `Get("prototype")`）+ 对象分配 85–92 + 帧 ≈ 30；`New` 处理函数 195–200 |
| `new C0()`（空 class） | 450 / 446 | 比 `new E()` 多 219：**函数环境 push+pop 188**、类构造器的 `EnvironmentStack` 克隆 28–34（普通函数 3）、四个句柄 33–37（普通函数 12–14）。编译器对所有 class 构造器无条件给函数作用域（`bytecompiler/class.rs`） |
| `new D0()` / `D1`（派生） | 786 / 821 | 两个构造帧，各一个函数环境（环境 push 探针 408 ≈ 2 × 188 + 克隆） |
| `new F(i)` 比 `new E()` 多 | 182 / 250 | 新对象上第一个属性的形状转换（不是调用机制；`oo` 里的 `new CX(i)` 679 = 450 + 228） |
| 原生→JS（`zero.call(null)`） | 81 / 54（回调本身之外） | `function_call` 29 + `context.run()` 37.6 / 10.5 + `pop_frame`（含帧释放）13 + 压栈 1–8；**编译比解释多 27 ns 的进出编译代码**（推断：入口桩、`Return` 一步和 `finish`、`execute` 返回；状态构建 0.9、`catch_unwind` 1.3 各自很小） |

`sort` 比较函数每次约 122 / 137 ns（852 次比较，反转数组 100 个）；`forEach(zero)`、`filter(zero)`、`map(zero)` 的编译列
**比解释列慢**（117.5 对 101.5、137.2 对 111.8、237.9 对 220.6）：回调只有几条指令时，进出编译代码的 27 ns 超过编译省下的，
违反本包硬规则「JIT 列不得比解释器慢」在微观层面已经成立；P60 套件里回调约十条指令，所以套件没有显示。

**原因排序**（按在套件里的代价）：(1) class 构造器的函数环境，`oo` 每轮约 215 ns；(2) `new` 每次重新查原型 + 重新分配对象，共 150 ns；
(3) 原生→JS 回调的固定成本（编译 81、解释 54，对 JS→JS 的 67、85）；(4) 每次调用的簿记 37–45 ns，其中句柄 15–20；(5) 指令分派（不属于帧）。
任务书「更瘦的帧」的收益上限是 15–20 ns / 次，不是大头；`iterators`/`binding_stack` 按需分配、`loop_iteration_count` 合并**没有可测收益**，建议从候选里删掉。

**建议的目标**（JIT 列 ms，P60 套件，括号内是 P66 合并时的数字；各项之后另记 JIT/解释器比值）：

| 测试 | 参考目标 | 建议 | 依据 |
|---|---|---|---|
| `fib` | ≤ 180（241.5） | **承诺 ≤ 195，争取 180** | 2,692,537 次调用，90 ns/次；180 要 ≤ 67，即每次省 23；孤立簿记 37，句柄 15–20、限制检查 5、影子栈/`this`/寄存器/返回值 8–10，合计可得 19–26（→ 170–190 ms）；要到 180 须砍掉六成以上簿记，再加编译调用里约 20 ns 的未归因部分有一部分能省。解释器列同降约 50–70 ms（≈ 300） |
| `oo` | ≤ 600（1041.5） | **承诺 ≤ 750；≤ 600 以「`this` 按初始形状分配」为条件** | 每轮 1041 = 两次方法调用 118 × 2 + `new CX(i)` 679 + 其余。去函数环境 −215、缓存原型 −60、三次调用的帧 −45–60，得 ≈ 715–750；再省首属性定义（228，需要 `this` 按构造器形状分配，候选 3 的后半）−150 才到 ≈ 570–600。只靠本包而不做形状转换，600 不可达 |
| `closure` | ≤ 330（417.7） | **承诺 ≤ 350，争取 330** | 约 2.25M 次回调 + 初始化循环 32 ms；330 要每次回调省 39 ns；能拿到的是进出编译代码 15–25（27 的大部分）+ 帧 15–20 = 30–45，即 317–350。解释器列约 ≤ 590 |

另加验收：`forEach(zero)`、`filter(zero)`、`map(zero)`、`sort` 的编译列不慢于解释列。

**建议的第 2 步顺序**（候选，收益为本机估计，风险为实现风险）：

1. **构造调用的快路径**（候选 3 前半）：class 构造器按需建函数环境（用 `scopes.requires_function_scope()` 代替无条件置位；派生类、含直接 `eval`、
   箭头函数或字段初始化器捕获 `this`/`new.target`/`super` 的仍建），`function_construct` 缓存原型（守卫 `prototype` 槽）。收益：`new C0()` 450 → ≈ 190，
   `oo` −275 ns/轮（−26%），`new E()` −60；两个档次同时受益。风险：中高——`this` 绑定、`new.target`、`Reflect.construct` 的 `newTarget`、
   派生类未调 `super()`、字段初始化顺序。**它是单项最大的收益，所以放第一**。
2. **原生→JS 回调的入口**（候选 4）：重复调同一个函数时少做进出编译代码与 `run` 的固定工作。收益：编译列每次回调 −15–25，`closure` −35–55 ms，
   并消除小回调上编译慢于解释的问题；风险：中——`run_depth`/`host_call_depth`、`exit_early`、中断与异常穿过原生帧。
3. **更瘦的帧与普通函数的直接进入**（候选 1、2 合并，它们改的是同一批函数）：先折叠 `check_runtime_limits`（−4–6，风险低），再把能借用的句柄
   （`realm`、`active_runnable`、影子栈的 `SourceInfo`）去掉（−7–10），再让 class 帧共享 `private_stack`（类帧 −25–30），最后在编译器已知的情形下跳过
   `this`/环境/返回值的通用分支（−5–8）。收益合计 15–26 ns/次，两个档次都得，`fib` −40–70 ms。风险：中高——`CallFrame` 字段类型牵动生成器/async
   搬帧、追踪、`error.stack`（影子栈）、`vm/jit/layout.rs` 的偏移。
4. **JIT 跟进**（候选 5）：帧改完后量；编译调用里约 20 ns 的未归因部分（`catch_unwind`、进出编译代码）若能让编译代码直接调编译代码而不经 `jit_step`/`finish`
   的检查，再省一部分。先量再做。

**没量到的**：函数环境 188 ns 里分配与「是否被污染」那段遍历各占多少；类构造器 `EnvironmentStack` 克隆为何是普通函数的 9 倍（推断是非空
`private_stack`，一个会分配的 `ThinVec`）；对象分配 85–92 ns 里收集器的份额；编译调用里约 20 ns 与回调进出的 27 ns 的内部划分（只有总量与几个小块的孤立值）；
分支预测与缓存效应（不许用 samply/wpr/xperf，没有硬件计数器）；生成器与 async 函数的调用路径（本步没有拆）。编译路径的计时器读数含约 15 ns 的补丁开销，
只可当份额。
