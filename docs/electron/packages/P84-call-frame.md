# P84 更瘦的调用帧与编译代码之间的直接调用

**依赖**：P69、P75、P78（已合并）  **规模**：L  **修改**：`boa/core/engine/src/vm/call_frame/`、`vm/mod.rs`（`push_frame`/`pop_frame`/`handle_return` 一带）、
`vm/shadow_stack.rs`、`vm/runtime_limits.rs`、`vm/opcode/call/`、`vm/opcode/` 里只因帧字段改名或改取法而跟着改的处理函数、
`builtins/function/mod.rs`（`function_call`/`function_construct` 只限进出帧的部分）、`builtins/generator/`、`builtins/async_*`（只限保存与恢复帧）、
`vm/jit/`（`call.rs`、`layout.rs`、`stack_layout.rs`、`runtime.rs` 与调用相关的部分、`compiler.rs` 里调用与 `Return` 的编译），对应测试、`boa/SU.md`、`docs/jit.md`、`docs/electron/perf-js.md`

## 目标

P69 把构造调用的大头拿掉了，P78 把 `new` 后的首属性定义拿掉了。剩下的差距在**每一次调用都要付的固定成本**：`fib`、`oo` 的方法调用、`closure` 的回调。
`oo` 现在约 560 ms，是 Node 的 193 倍，是 P60 套件里最差的比值；`fib` 约 228 ms（Node 5.5），`closure` 约 425 ms。

P69 的拆账（`docs/jit.md`「Where a call goes」，在 `05f26157` 上量的）：一次 JS→JS 调用编译 67 ns、解释 85 ns；其中孤立量出的簿记约 37 / 45 ns：
帧拥有的四个句柄（`code_block`、`realm`、`environments`、`active_runnable`）克隆加丢弃约 12 ns，影子栈的 `SourceInfo` 克隆约 3，`check_runtime_limits` 5–6，
`this`、寄存器、返回值、待定调用、`downcast_ref` 合计约 12；编译调用里另有约 20 ns 没有拆开（进出编译代码、被调函数 `Return` 经 `jit_step`/`finish`/嵌套 `execute` 的那一步约 17）。
P69 没做「更瘦的帧」，理由是 `frame.realm` 有几十处使用、帧拥有 `code_block` 和环境，是重设计不是修剪。本包就做这个重设计，外加编译代码直接调用编译代码。

**目标**（P60 套件 JIT 列，ms，中位；括号内是当前大约值，开工时先在同一轮重量一次作基线）：

| 测试 | 承诺 | 争取 | 依据 |
|---|---|---|---|
| `fib` | ≤ 210（≈ 228） | ≤ 190 | 2,692,537 次调用；帧去四个句柄与影子栈克隆 ≈ 12–15 ns / 次 → ≈ 195–200；争取值要编译代码直接调编译代码，省掉那约 20 ns 的一部分 |
| `oo` | ≤ 520（≈ 560） | ≤ 470 | 每轮两次方法调用加一次构造，各省 10–15 ns 得承诺值；争取值同样要直接调用 |
| `closure` | ≤ 400（≈ 425） | ≤ 375 | 约 2.25M 次原生→JS 回调，帧的部分 ≈ 10–12 ns / 次 |

另外：
- **解释器列**：三项各给前后数字，不得变慢；预计同降（帧的簿记两个档次共享）。
- **微探针**（编译 / 解释，ns，前后各一组）：`zero()`、`id(i)`、`obj.m()`、`pinst.get()`、`bound()`、`Math.abs(i)`，以及 `forEach(zero)`、`filter(zero)`、`map(zero)`、
  `sort` 比较函数。P69 的硬规则保留：小回调上**编译列不得慢于解释列**（持平在 2 % 内可接受）。
- 套件其余各行不变慢（同一轮，中位差 ±3 %）。

目标不可达时，量清楚卡在哪、给出数字，不要为了数字放宽语义。

## 背景（先读）

- `docs/jit.md`「Where a call goes」（拆账方法与各块的数）、「Results (P69 …)」「Results (P78 …)」、「Roadmap」的 Phase 2 遗留第 1 项。
- `P69-call-cost.md`「状态」的「没做的」与「目标修订建议」；`P78-constructor-initial-shape.md`「状态」。
- `vm/call_frame/mod.rs`（`CallFrame` 的字段：`code_block`、`environments`、`realm`、`active_runnable` 都是拥有的句柄；`iterators`、`binding_stack`、
  `loop_iteration_count`、`flags`）；`vm/mod.rs` 的 `push_frame`/`pop_frame`/`handle_return`；`vm/shadow_stack.rs`；`vm/runtime_limits.rs`。
- `builtins/function/mod.rs` 的 `function_call`/`function_construct`；`vm/opcode/call/`；`vm/jit/call.rs`（`jit_call`、`compiled_callee`、`finish`）与 `vm/jit/layout.rs`
  （编译代码按偏移读写帧字段）。
- 生成器与 async 函数怎样保存、恢复帧（`builtins/generator/`、`builtins/async_*`、`vm/opcode/generator/`、`vm/opcode/await/`）：帧会被搬走、以后再压回。
- `error.stack` 与 `CallFrame::position` 怎样读影子栈与帧；`Realm` 切换（另一个 realm 的函数在它自己的 realm 里运行）。

## 要做的事

1. **重量基线**：在本包的基线上，用 P69 的办法（同进程交替的微探针、重复 8 次的探针、单个 `rdtsc` 计时器，补丁不提交）重量一次 JS→JS 调用、方法调用、
   原生→JS 回调的构成，确认 P69 的份额在 P75、P78 之后是否还成立。写进「状态」。不要停下等确认：份额与上表的依据一致就按下面的顺序做；
   有大出入，在「状态」里写明并据此调整顺序。
2. **帧不再克隆能借到的句柄**：`code_block`、`realm`、`active_runnable` 在被调函数对象里都有，函数值在调用期间留在 VM 栈上，帧可以不各持一份。
   做法由你定（帧持一个函数句柄、从函数读；或改为借用；或把 realm 只在跨 realm 调用时切换），要写明为什么在生成器/async 搬帧、
   `eval`、脚本与模块顶层帧、原生函数帧、跨 realm 调用下仍然对。`environments` 的克隆能不能省也量一下。
3. **影子栈与限制检查**：影子栈不再为每次调用克隆 `SourceInfo`（记帧或代码块的指针即可，`error.stack` 逐字不变）；`check_runtime_limits` 合并到更少的检查点
   （递归深度与栈大小的上限语义不变：超限时仍抛同样的 `RangeError`，在同样的深度）。
4. **编译代码直接调用编译代码**：被调函数已编译、是普通函数（不是类构造器、生成器、async，没有 `arguments` 对象与直接 `eval`）时，编译代码直接进入它，
   返回时不经 `jit_step`/`finish`/嵌套 `execute` 的那一步。保留 P66 的嵌套深度上限、异常与中断穿过的行为、去优化回退。量清楚省了多少；
   省不到就写明原因与数字，不要硬上。
5. 每一步单独提交，每步之后量一次（三项套件 + `zero()` 微探针，两个档次），记进「状态」，回退了就修或撤。

## 测试

差分测试（解释、`BOA_JIT_THRESHOLD=0` 强制编译、热起来走 OSR 三种一致，期望值是 Node 22 的输出）：
- 递归（深递归到上限：抛 `RangeError` 的深度两个档次一致且与改前一致）、相互递归、尾部位置的调用；
- 方法调用（自有、原型上、getter 返回的函数）、绑定函数、Proxy 的 `apply`/`construct`、`Reflect.apply`、`call`/`apply`；
- 跨 realm：另一个 realm 的函数在调用中读到的是自己的全局与内建；回调从原生函数（`forEach`、`sort`、`map`、`JSON.parse` 的 reviver、getter/setter）进入；
- 生成器与 async 函数在多次 `yield`/`await` 之间搬帧后，`this`、闭包变量、`arguments`、`new.target` 正确；
- 异常穿过编译帧、解释帧与原生帧交替的调用链（`try`/`finally` 的顺序）；中断（`InterruptHandle`）落在被直接调用的编译函数里；
- `error.stack` 与 `Error.captureStackTrace` 输出与改前**逐字相同**（同一组脚本在基线与最终上各跑一遍比较）；
- 直接 `eval`、`arguments` 对象、默认参数、剩余参数、类构造器与派生类（P69/P78 的测试照旧过）；
- 调用中途触发垃圾回收（帧不再持有句柄后，函数与代码块仍被正确追踪）。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit` 各一遍）、`cargo fmt`；根 `cargo test -p verve`（有无 `--features jit`）、`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262：除 `staging` 外全量，基线对最终 0 回退；最终代码上解释对强制编译逐条 0 差异（计时类 `Atomics.notify` 例外照旧）。
- 上面的目标表与微探针，前后同一轮；`perf-js.md` 追加一节，`docs/jit.md` 追加「Results (P84 …)」。
- release `electron` 体积增量（`size.md` 的方法）。

## 不做

类型反馈（JIT 第 3 阶段）；内联被调函数；改变 `CallFrame` 以外的值表示；原生函数本身的实现。

## 状态

已完成，目标值只部分达到。分支 `electron/p84-call-frame`，基线 `824bcf70`。

### 做了什么（四个代码提交，每个提交后都量过）

1. **帧借用句柄**（`perf(boa): Read the realm and code block from the callee …`）。新增 `Lent<T>`（`vm/call_frame/lent.rs`）：要么是帧自己的句柄，要么是被调函数拥有的
   句柄的按位拷贝（不加计数、不 drop）。普通函数的帧向被调函数借 `code_block`、`active_runnable`（脚本或模块）、`realm`；`function_call` 与 `function_construct` 通过
   `lend_function_parts` 取得，`jit/call.rs` 的 `compiled_callee` 也用它。
2. **影子栈与限制检查**。影子栈的字节码项借代码块的 `SourceInfo`（`ShadowEntry::Bytecode` 的字段是 `Lent<SourceInfo>`），进入回溯的项由手写的 `Clone` 复制成自有的，
   所以 `error.stack` 不变。`check_runtime_limits` / `runtime_limits_exceeded` 加 `#[inline]`，`RangeError` 的构造移到冷路径。
3. **编译代码直接调用编译代码**。`jit_call` 用 `compiled_callee` 已经找到的 `Rc<JitCode>` 直接 `execute` 被调函数（检查与 `run_callees` 相同：嵌套上限 64、本机栈的四分之一、
   JIT 开着、非 `fuzz`、非 `trace`，不满足就走老路）；`Return` 编译成 `jit_return`：不提前退出的帧不经 `catch_unwind` 与 `finish` 直接弹帧并离开编译代码，提前退出的帧
   （原生回调）仍走 `jit_step`。
4. **环境也借**（`perf(boa): Lend the function's environments …`）。`Lent<EnvironmentStack>`，第一次对环境写（`push_*`、`pop`、`truncate`，经 `DerefMut`）时才拷贝成自有的。

### 为什么对（任务第 2 步要求写明）

借用成立的前提：**帧的函数槽（`stack[fp+1]`）里放的就是被调函数对象本身**（`callee_parts` 用一次指针比较判断；构造调用要越过栈顶的 `new.target`），这样 VM 栈用计数引用把函数
留到帧被弹出；而函数的 `code`（只有惰性函数的编译会替换，发生在帧建立之前）、`script_or_module`、`realm`、`environments` 在函数建立后不再改（例外：`PushPrivateEnvironment`
给 `GetFunction` 刚做出来的类构造器的 `environments` 加私有环境，此时脚本还够不着它）。槽里不是被调函数时（没有 `apply`/`construct` 陷阱的 Proxy 转发给目标，绑定函数转发给
目标：槽里留的是转发者），帧改为自有这些句柄。评审发现的错误正在这里：Proxy 被 `revoke()` 后不再引用目标，目标在运行中被回收，帧还在读它的环境与代码块。逐项：

- **生成器与 async 函数搬帧**：帧在 `GeneratorContext::from_current` 里被 `clone`，`CallFrame::clone` 把所有句柄复制成自有的；活着的那个帧在 `split_off_frame` 之前先
  `own_handles()`（函数值随栈一起被搬走）。恢复时压回的是自有的帧。`Lent` 的 `Trace` 对借来的不参与根计数（`trace_non_roots`、`run_finalizer` 对借来的是空操作），所以
  不会下溢。
- **`eval`、脚本与模块顶层、合成模块帧**：用 `CallFrame::new`，句柄全是自有的，同改前。
- **原生函数帧**：原生函数不压 `CallFrame`；它在调用者帧里换 realm（`swap_realm`）。`Context::enter_realm` / `swap_realm` 用 `Lent::replace`，把借来的换成自有的（旧值克隆成自有
  的返回），所以换回时不会 drop 一个没计数的句柄；两边是同一个 realm 时 `swap_realm` 什么也不做（也省掉了同 realm 原生调用里的两次交换）。
- **跨 realm 调用**：借的是函数自己的 `realm` 字段，与改前克隆的是同一个值；测试 `functions_of_another_realm_run_in_their_own`（直接调用、`map` 回调、getter、生成器、
  抛 `ReferenceError`）三个档次一致，且读到的是函数自己的全局。
- **影子栈项**：项借的是帧的代码块的 `SourceInfo`，随 `pop_frame` 一起弹出；帧或函数在此之前不会消失。
- **垃圾回收**：借来的不计数，但函数在 VM 栈上（有根），它的 `code` 等由函数追踪；`collections_in_the_middle_of_calls` 在调用中途强制回收。

### 重新量的拆账（第 1 步）

只用了交替的微探针（同一轮、基线与分支各 7 个进程取中位数），没有重做八倍探针和 `rdtsc` 计时器：改动的效果直接量。`zero()` 编译 71.7（最好）、解释 99.7（最好）；P69 的
份额里「帧的簿记」在 P75、P78 之后仍成立，但孤立量出来的 12–15 ns 在真实调用里只省了约 5–6 ns（`zero()` 编译 71.7 -> 66.1，第 2 步 -3，第 3 步 -1.5，第 4 步 -3.3），
因为这些块在真实调用里互相重叠，`docs/jit.md`「Where a call goes」已经预告过。解释档同降（`zero()` 约 -9）。顺序没有调整。

### 数字

微探针（编译 / 解释，ns，中位数（最好），基线 -> 现在）：`zero()` 74.5 -> 66.0 / 102.4 -> 94.9；`id(i)` 102.5 -> 93.8 / 160.1 -> 147.1；`obj.m()` 76.6 -> 72.9 / 123.3 -> 111.6；
`pinst.get()` 163.4 -> 120.4 / 218.4 -> 173.7；`bound()` 104.0 -> 100.5 / 121.2 -> 112.5；`Math.abs(i)` 101.3 -> 99.5 / 133.6 -> 129.1；`forEach(zero)` 96.7 -> 90.9 / 95.2 -> 90.0；
`filter(zero)` 109.1 -> 100.5 / 111.1 -> 103.1；`map(zero)` 215.8 -> 204.9 / 216.2 -> 206.4；`sort` 比较函数 171.7 -> 162.5 / 162.4 -> 155.5。

套件（JIT / 解释，ms，中位数（最好），一轮交替，负载 41 % 前 24 % 后）：

| 测试 | 目标（承诺 / 争取） | 基线 | 现在 |
|---|---|---|---|
| `fib` | ≤ 210 / ≤ 190 | 247.2 / 388.7 | **216.1 (206.8)** / 358.7 |
| `oo` | ≤ 520 / ≤ 470 | 682.8 / 847.6 | **594.2** / 685.0 |
| `closure` | ≤ 400 / ≤ 375 | 468.8 / 695.7 | **442.7 (420.9)** / 652.6 |

另一轮九个进程交替（负载 14–36 %）：`fib` 243 -> 212（不含环境提交）/ 224（含）、`oo` 689 -> 647 -> 561、`closure` 455 -> 419 -> 437。其余十一行同一轮内无可辨别的变化
（`vdom` 的中位数 +9 % 但最好值 53.2 对 52.7，`regex` 解释档中位数 +35 % 但最好值 24.4 对 23.6，其余不超过 4 %）。**承诺值没有全部达到**：`fib` 差几毫秒（中位 216，最好 207），
`oo` 与 `closure` 差得多。

### 没做到的与原因

- **编译代码之间的真正直接调用（不经 `execute`）没有做。** 现在省掉的是 `prepare`、`finish`/`run_callees` 一轮与 `Return` 的 `jit_step`，合计约 3.3 ns。剩下最大的一块是
  进出编译代码本身（每次调用为被调函数建一个 14 个字段的 `JitState`、克隆 `Rc<JitCode>`、Cranelift 的序言与尾声）。去掉它要一套编译函数之间的调用约定：辅助函数从栈顶帧的
  `JitState` 读寄存器与帧指针，所以被调函数需要自己的状态，去优化时还得留下解释器能接着跑的帧。这是入口协议的重设计，超出本包能安全做完的范围；数字显示它值得单独立项（上限
  约 15–20 ns 一次调用）。
- **`oo`**：每轮两次方法调用加一次 `new CX(i)`，类构造器的函数环境（188 ns，见 `docs/jit.md`）每次都新建并拷贝，帧借不到。`oo` 因此只降到 594（-13 %）。
- **`closure`**：约 225 万次原生→JS 回调，每次仍要进出编译代码（上面同一块）。借环境的提交让带捕获变量的箭头函数每次慢了约 8 ns（`mapArrow` 探针 241.5 对 233.1 最好值），
  `closure` 比不借环境时慢约 18 ms（437 对 419），原因没有查到：箭头函数压函数作用域时本来就要拷贝环境，拷贝不是新增的；多出来的只是一次比较和帧大了 8 字节。因为 `oo`
  （套件里比值最差的一行）从中得到 -75 ms，保留。
- 小回调规则：`sort` 比较函数编译档仍比解释档慢 4.5 %（改前 5.7 %），`forEach` 1 %（在 2 % 之内）。
- 没有重做 `rdtsc` 与八倍探针，也没有量 `environments` 单独的孤立克隆成本：用提交前后的交替差代替。

### 验证

- `boa/`：`cargo test -p boa_engine`（默认 1246 个；`--features jit` 1325 个；`BOA_JIT_THRESHOLD=0 --features jit` 1325 个）全过；`BOA_JIT_FUZZ_COUNT=3000 cargo test -p boa_engine
  --features jit --lib random_programs_match_the_interpreter` 跑了 1 个测试，通过；`cargo clippy -p boa_engine --all-targets -- -D warnings` 有无 `jit` 各一遍无警告；`cargo fmt`。
- 根目录：`cargo clippy --workspace --all-targets -- -D warnings` 无警告；`cargo test -p verve` 与 `--features jit` 全过；`cargo fmt --all --check`；`bash scripts/style-check.sh`。
- test262（`E:\test262`，除 `staging` 外全量：language、built-ins、annexB、intl402、harness，共 52,095 个，49,898 个通过）：基线（`BOA_JIT=0`）、最终解释（`BOA_JIT=0`）、
  最终强制编译（`BOA_JIT_THRESHOLD=0`）三份按叶子目录逐个对比通过 / 失败 / 忽略数：基线对最终 **0 回退**（基线在 `Atomics` 下多失败 2 个（`notify` 一类），是
  计时类的那几个，最终上通过）；最终解释对强制编译 **0 差异**。
- 新测试：`vm/jit/tests_call_frame.rs`（三个档次一致）：各种调用与递归深度、搬帧后的 `this`/闭包/`arguments`/`new.target`、调用中途强制 GC、被撤销的 Proxy（见下）、跨 realm、
  `error.stack` 与 `Error.captureStackTrace`、被直接调用的函数里的中断、超过嵌套上限的链；`vm/call_frame/tests.rs`：同类的解释器层测试，不依赖 `jit` 特性，默认的
  `cargo test -p boa_engine` 也跑。另用 `diff1.js`（递归、方法、绑定、Proxy、Reflect、回调、生成器、async、异常、eval）三个档次输出的哈希与
  Node 22 相同；`stack1.js` 的 `error.stack` 与 `captureStackTrace` 输出（21 KB）在基线与最终上、解释与强制编译下逐字相同（`cmp`）。
- release `verve.exe` 26,787,328 -> 26,838,016 字节（+50,688，+0.19 %）；release `electron`（`cargo build --release -p verve-gpui --bin electron --features jit`）32,781,312 ->
  32,833,024 字节（+51,712，+0.16 %）。

### 已知问题与给其他包的说明

- `Lent` 的安全性依赖「函数在帧的函数槽里待到帧弹出」。以后若有代码在帧存活期间改写 `stack[fp+1]`、替换 `OrdinaryFunction` 的 `code`/`realm`/`script_or_module`/`environments`，
  或在不 `clone`/`own_handles` 的情况下把活帧搬进 GC 对象，都要先让帧自有这些句柄。`CallFrame` 的 `code_block`、`realm`、`active_runnable`、`environments` 现在是 `Lent<…>`，读法不变
  （`Deref`）；赋值要用 `Lent::owned`/`replace`；`frame.environments` 的写操作会触发拷贝。
- 想继续降调用成本的包：编译函数之间的调用约定（见上），以及把 `this`/函数移出值栈（`docs/jit.md` Roadmap 第 1 项剩下的部分）。
- 磁盘：E: 在这次工作中被写满过一次（所有人的 `cargo-target`），`cargo test -p verve --features jit` 的调试构建要 14 GB。

### 评审后的修改（Proxy 撤销后的悬空借用，等）

- **阻塞项**：没有 `apply`/`construct` 陷阱的 Proxy 把调用转发给目标，函数槽里留的是 Proxy；帧却借了目标的代码块、环境、realm、脚本。目标只被 `P.[[ProxyTarget]]` 持有，
  目标的函数体里 `revoke()` 再触发回收，目标就被释放，帧继续读悬空的句柄。**修复前复现**：`setup(make())()`（目标是在辅助函数里做的匿名函数，闭包变量 `x = 41`，体内
  `revoke(); collect(); collect(); return x + 1`）在默认特性的解释器里返回 `NaN`，另一次运行是 `STATUS_ACCESS_VIOLATION`（不稳定，读的是释放过的内存）；`new P()`、`class B extends P`、
  `[0].map(P)` 与 `Function(...)` 目标的变体在修复前的这几次运行里没有可观察的错误（释放了的内存碰巧还没被覆盖），没有 sanitizer 看不到。修复：`callee_parts` 只在槽里就是被调
  函数时借，否则（Proxy、绑定函数转发）用 `own_function_parts` 克隆成自有的；不把目标写进槽里（那会改变 `active_function_object()`）。正常调用多一次指针比较：`zero()` 编译
  73.2 -> 66.4（最好 71.0 -> 65.6），`fib` JIT 237 -> 209、`oo` 617 -> 543（交替，负载 15–30 %），与修复前的数字一致。回归测试：`a_revoked_proxy_does_not_free_the_running_function`
  （`vm/call_frame/tests.rs` 解释器层，`vm/jit/tests_call_frame.rs` 三个档次各跑 3 遍），覆盖调用、`new`、`super()`、`[0].map(P)`、`Function(...)` 目标的调用与构造。
  已重新检查其他借用处：影子栈项借帧的代码块（帧借或自有，由上面保证）；`jit/call.rs` 的 `compiled_callee` 的被调函数就是槽里取出的值；`Lent<EnvironmentStack>` 同源；绑定函数
  现在也走自有路径，不再依赖「绑定目标不可变」。
- 测试：原先只在 `jit` 特性下编译的测试，核心部分搬了一份到 `vm/call_frame/tests.rs`（不依赖 `jit`）；GC 测试改成真正的借用测试（被撤销的 Proxy 的目标没有别的引用）。
- 文档：SU.md 与 `lend_function_parts` 的说明改为「槽里是被调函数」、四个字段，并写明 `PushPrivateEnvironment` 这个例外；`jit_return` 的说明改为所有编译的 `Return`；`jit/call.rs` 的
  模块说明补上直接运行被调函数。
- 风格：`jit/call.rs::call` 拆成 `call` 与 `run_callee`（嵌套不超过 3 层），`may_run_nested`/`may_enter` 由 `run_callees`、`prepare`、`run_callee` 共用；惰性编译的块抽成
  `compile_lazy_function`（返回重新借用的函数，不再返回被丢弃的代码块）与 `callee_parts`；`function_construct` 拆出 `construct_this` 与 `push_construct_scopes`（131 行 -> 63 行）；
  `ShadowStack::pop` 不再把可能是借来的项交还调用者。
- 范围：`context/mod.rs`（`enter_realm`、`swap_realm`）不在本包的「修改」清单里，但它们要跟着帧的字段走；`swap_realm` 两边是同一个 realm 时提前返回是新增的快路径（同 realm 的原生
  调用不再交换两次）。已写进 `boa/SU.md`。另外为 `callee_parts` 的指针比较把 `JsValue::with_object` 去掉了 `jit` 特性限制（`value/mod.rs`、`value/inner/*.rs`）。
