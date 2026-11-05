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

（实现者完成后填写）
