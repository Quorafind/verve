# P70 迭代器结果不分配对象

**依赖**：P65、P66（均已合并）  **规模**：M  **修改**：`boa/core/engine/src/builtins/iterable/`、`builtins/array/array_iterator.rs`、
`builtins/map/`、`builtins/set/`、`builtins/string/` 中的迭代器、`vm/opcode/iteration/`、`vm/call_frame/` 中只限 `iterators` 字段的类型、
`vm/jit/` 中迭代指令的编译、`docs/jit.md`、`boa/SU.md`、对应测试

## 目标

`for (const x of arr)` 每个元素约 324 ns（编译）/ 340 ns（解释），按下标循环约 26 ns。P66 测得其中约 275 ns 是
`IteratorNext` 每步分配的结果对象 `{ value, done }`；检查 `next` 方法是否被改写只是一次指针比较，不是成本。
这个对象在内建的 `next` 方法下**不可观察**：内建 `next` 用 `CreateIterResultObject` 新建它，唯一的使用者 `IteratorStep`
只读它的 `done` 与 `value`，读一个新建的普通对象的数据属性不运行用户代码。所以当记录里的 `next` 是内建的、迭代器是对应的内建类型时，
可以直接返回 `(value, done)`，不建对象。

改动在解释器与 JIT 共用的 `IteratorRecord` 上，两层都受益；展开 `[...a]`、数组解构、`Array.from(iterable)`、`new Map(entries)`、
`Promise.all` 等经 `IteratorRecord::step` 的内建也跟着变快。

**参考目标**（第 1 步之后由实现者确认）：数组 `for…of` 每元素解释 ≤ 80 ns、编译 ≤ 40 ns；`Map`/`Set` 的 `for…of` 每元素降一半以上；
P60 套件 `vdom` JIT 列 ≤ 55 ms（P66 合并时 63.7）。其余各项不变慢。

## 背景（先读）

- `docs/jit.md`「Why the baseline barely moved」中 `vdom` 与 `for-of` 的测量（`GetIterator` 3,161 周期、`IteratorNext` 1,241）。
- `P66-jit-coverage.md`「状态」第 2 步「Left out」的 `for…of` 一条（总控已决定单列为本包）。
- `builtins/iterable/mod.rs` 的 `IteratorRecord`（`iterator`、`next_method`、`done`、`last_result: IteratorResult`）与 `step`/`step_value`；
  `builtins/array/array_iterator.rs`、`map/map_iterator.rs`、`set/set_iterator.rs`、`string/string_iterator.rs` 的 `next`；
  `vm/opcode/iteration/iterator.rs`（`IteratorNext`、`IteratorDone`、`IteratorValue`、`IteratorReturn`）。
- 规范：`GetIterator`、`IteratorNext`、`IteratorComplete`、`IteratorValue`、`IteratorStep`、`IteratorClose`，以及 `%ArrayIteratorPrototype%.next`。

## 要做的事

### 1. 量清楚（短，先交）

数组、`Map`、`Set`、字符串、生成器、用户自定义迭代器六种 `for…of` 的每元素成本（编译与解释），并把一步拆成：结果对象分配、
`next` 的调用分派、`done`/`value` 的两次属性读取、迭代器自身的状态推进。写进「状态」，据此确认目标。

### 2. 结果不装箱（解释器）

- `IteratorRecord` 记下「`next_method` 是哪个内建的 `next`」（`GetIterator` 时判断一次：内建函数对象且迭代器是对应的内建类型），
  `step` 在这种记录上直接调内建的推进逻辑，得到 `(value, done)`，**不建结果对象**；`last_result` 能表示这种未装箱的结果。
- 内建 `next` 被直接调用（`it.next()`）时行为不变，照旧返回新对象；装箱与否只取决于调用者是不是 `IteratorStep`。
- 内建推进逻辑里本来可观察的读取（例如 `%ArrayIteratorPrototype%.next` 对类数组读 `length` 与元素，迭代对象可能是 Proxy 或带访问器）
  **全部保留**，只省掉结果对象。
- 改写检测：记录建好之后用户再改 `next`（`%ArrayIteratorPrototype%.next = …`）不影响已经取到的 `next_method`，这是规范行为；
  新建的迭代器在 `GetIterator` 时重新判断。
- `IteratorClose`、`return` 方法的调用时机、异常穿过 `for…of`（`try/finally`、`break`、`throw`）全部不变。

### 3. JIT

在第 2 步之上，`vm/jit/` 把数组 `for…of` 的整段（`GetIterator` 之后的 `IteratorNext`、`IteratorDone`、`IteratorValue`）在记录是内建数组迭代器、
迭代对象是普通稠密数组时编译成按下标读取；任何条件不满足都退回第 2 步的路径。P66 测得守卫只要一次指针比较，成本在第 2 步消掉的分配上，
所以本步**先测第 2 步之后 JIT 还差多少，有收益才做**，没有收益就在「状态」里写明并跳过。

### 4. 测试

- 差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致），覆盖：六种迭代器；迭代中修改数组（追加、截短、改 `length`）；
  `Array.prototype[Symbol.iterator]` 与 `%ArrayIteratorPrototype%.next` 被改写（在 `GetIterator` 之前与之后）；迭代对象是 Proxy、带洞数组、
  类型化数组（含分离的缓冲）；`break`/`return`/`throw` 触发 `IteratorClose`；`return` 方法抛错；解构与展开；生成器的 `for…of`。
- test262：`built-ins/Array`、`built-ins/ArrayIteratorPrototype`、`built-ins/Map`、`built-ins/Set`、`built-ins/String`（迭代部分）、
  `language/statements/for-of`、`language/expressions/array`（展开）、`language/destructuring` 前后逐项 0 回退。

## 验收

- P60 套件两列与「量清楚」里六种 `for…of` 的前后对比；参考目标逐项「达标/差距」；`perf-js.md` 追加一行。
- `docs/jit.md`「Testing」的整套测试全部通过（同 P66 的验收），test262（除 `staging`）解释与强制编译逐项 0 差异。
- 体积增量写进 `size.md`。

## 不做

异步迭代器（`for await`）；生成器本身的恢复成本；调用机制（P69）。改 `vm/call_frame/` 只限 `iterators` 字段需要的类型变化，
与 P69 冲突时后合并者 rebase。

## 状态

**完成**：第 1 步（量清楚）、第 2 步（解释器）、第 3 步（JIT）、测试与验收。分支 `electron/p70-iterators`，从 `docs/electron-plan` `05f26157` 切出，
改动 3 个代码提交：`perf(iterators)`（解释器，含 `boa/SU.md`）、`test(jit)`（差分测试）、`perf(jit)`（三个快速辅助），外加文档提交。

### 第 1 步：每元素成本与拆分

`for (const x of source)`，100 万元素，ns/元素，5 个进程取中位数，同一轮内 base（`05f26157`）与改动后相邻测量（`verve` release + `jit`，
`VERVE_CODE_CACHE=0`）。

| 来源 | base 编译 | base 解释 | 之后 编译 | 之后 解释 |
|---|---|---|---|---|
| 数组 | 413.8 | 431.4 | 22.2 | 71.8 |
| `Set` | 611.9 | 546.9 | 24.0 | 46.9 |
| `Map`（entries） | 1317.1 | 1172.8 | 546.5 | 566.1 |
| 字符串 | 501.6 | 578.2 | 72.8 | 98.8 |
| 生成器 | 432.9 | 454.8 | 430.8 | 441.6 |
| 用户自定义迭代器 | 648.5 | 678.0 | 661.6 | 678.1 |

负载（`Get-Counter`，5 次采样）：数组、`Map`、生成器、用户迭代器一轮 21–26 %（后），`Set`、字符串一轮前 45–68 %、后 11–22 %（机器上有别人在编译，
数值比安静时慢约 20 %，只在轮内比较）。参照循环（同一进程内）：按下标循环 10.9 ns 编译 / 82.7 ns 解释；手写 `it.next()` 循环 246 / 290 ns。

**一步的拆分（数组，解释器，推断）**：临时在 `IteratorRecord::step` 的老路径里用 `rdtsc` 计时（负载 35–40 %，每对计时约 25 周期的开销，未提交），
一个元素约 1,900 周期：`next` 调用合计约 1,200，其中结果对象分配（`create_iter_result_object`）约 770（≈40 %）、内建推进逻辑约 155（≈8 %）、
调用分派（`native_function_call`、影子栈、`swap_realm` 等，由差得出）约 275（≈14 %）；`done` 与 `value` 两次属性读取各约 250 与 240（合计 ≈26 %）；
其余（循环自身的指令、记录出入栈）约 11 %。折算到 431 ns：分配约 170、读取约 110、分派约 60、推进约 35、其余约 50。
所以 P66 说的「约 275 ns 在 `IteratorNext`」成立（分配 + 分派 + 推进），但还有约 110 ns 在后面的两次属性读取，也一并消失。
`Map` 剩下的 ≈ 550 ns 主要是每个元素新建 `[key, value]` 数组（推断，没有单独量）。

结论：目标不是不可达，且成本在 P66 说的地方，继续。

### 第 2 步：解释器

- `GetIterator`（`get_iterator_from_method`）里判断一次：`next` 是不是**当前 realm** 的数组/`Map`/`Set`/字符串迭代器的原生 `next`
  （`NativeFunction` 的函数指针比较，`as_fn_ptr`；别的 realm 的函数不走快路径，因为 `next` 会在那个 realm 里建数组），结果存进 `IteratorRecord.stepper`。
  四个内建 `next` 各拆成 `advance(&JsObject, &mut Context) -> JsResult<Option<JsValue>>`（所有对迭代对象的读取原样保留）与「包成结果对象」两半，
  直接调 `it.next()` 仍返回新对象。`last_result` 变成 `Object | Value` 两种，`IteratorResult` 指令在未装箱时按需建对象（实际只有 `for await` 与 `yield*` 用，
  它们走 `update_result`，到不了）。
- `IteratorNext` 与 `IteratorValue` 在原位步进/读取，不再把记录从帧栈弹出再压回去；新建记录不再分配那个空的 null 原型对象。
- 不保留的：步进期间 Proxy 陷阱里的 `Error.stack` 少一行 `next` 的原生帧（影子栈）；已写进 `boa/SU.md`。
- 之后：数组 `for…of` 解释 **71.8 ns（目标 ≤ 80，达标）**。

### 第 3 步：JIT（做了）

第 2 步之后数组 `for…of` 编译 60.7 ns，目标 ≤ 40，差 20 ns；剩下的是三条 `jit_step`（`catch_unwind`、帧检查、块边界）而不是工作本身，所以做了：
`IteratorNext`、`IteratorDone`、`IteratorValue` 各一个快速辅助（`helpers.rs`），失败退回 `jit_step`。`IteratorNext` 的辅助只在「步进不会跑 JavaScript 也不会失败」时回答：
`Map`/`Set`/字符串迭代器（先核对迭代器对象的类型），或普通数组上的值迭代器且 `length` 是整数、下一个元素是稠密的非洞元素；洞、Proxy、类型化数组、
`keys()`/`entries()` 与其他记录走解释器的处理函数。不改 `Instruction` 的布局，没有碰 `vm/call_frame/`。之后：数组 `for…of` 编译 **22.2 ns（目标 ≤ 40，达标）**，`Set` 24.0。
没做的：字符串迭代器还要经 `String::substring` 与上下文（72.8 ns）；`keys()`/`entries()` 不走辅助。

### 验收

| 项 | 目标 | 结果 |
|---|---|---|
| 数组 `for…of` 解释 | ≤ 80 ns | 71.8 ns，达标 |
| 数组 `for…of` 编译 | ≤ 40 ns | 22.2 ns，达标 |
| `Map`/`Set` `for…of` 降一半以上 | | `Set` 编译 −96 %、解释 −91 %；`Map` 编译 −59 %、解释 −52 %，达标 |
| `vdom` JIT 列 | ≤ 55 ms | 53.0（base 71.2），达标；解释列 60.3（base 73.4） |
| 其余不变慢 | | `mapset` 39.7 → 25.3；`fib`/`alloc`/`concat`/`oo`/`typed`/`tokenize`/`closure` 在噪声内（`alloc`、`concat`、`oo` 在两个二进制交替下各重跑两次，差在 2 % 以内），生成器与用户迭代器不变 |

P60 套件两列（ms，5 次中位数（最好），整轮 base 再 after；base 前负载 9–14 %、后 18–50 %；after 前 18–25 %、后 23–27 %）：

| 测试 | base JIT | after JIT | base 解释 | after 解释 |
|---|---|---|---|---|
| `fib` | 236.0 (232.7) | 238.9 (236.5) | 359.5 (351.4) | 381.3 (371.7) |
| `alloc` | 266.6 (259.3) | 385.6 (275.1) | 278.9 (272.9) | 333.0 (307.3) |
| `concat` | 21.8 (20.9) | 24.3 (22.4) | 53.3 (51.6) | 73.3 (47.0) |
| `splitjoin` | 12.1 (11.7) | 13.0 (12.8) | 12.2 (11.9) | 14.6 (12.1) |
| `regex` | 31.5 (30.6) | 35.6 (34.1) | 32.2 (31.4) | 32.3 (30.6) |
| `json` | 1269.1 (1235.5) | 1266.3 (1218.8) | 1256.8 (1218.1) | 1272.1 (1230.0) |
| `sort` | 2379.2 (2321.0) | 2290.5 (2250.8) | 2780.8 (2765.5) | 2740.0 (2647.2) |
| `mapset` | 39.7 (37.1) | 25.3 (21.9) | 43.7 (40.6) | 25.1 (23.3) |
| `oo` | 1126.0 (1078.7) | 1097.0 (1051.8) | 1185.8 (1156.6) | 1304.7 (1197.3) |
| `typed` | 629.1 (614.4) | 651.5 (639.9) | 1255.5 (1240.3) | 1310.2 (1263.3) |
| `vdom` | 71.2 (69.3) | 53.0 (49.1) | 73.4 (71.4) | 60.3 (54.5) |
| `tokenize` | 164.4 (159.5) | 162.2 (157.1) | 183.2 (175.1) | 178.6 (175.2) |
| `template` | 1.8 (1.8) | 1.7 (1.7) | 0.9 (0.9) | 1.0 (0.9) |
| `closure` | 456.1 (417.3) | 435.4 (429.2) | 712.1 (669.3) | 645.2 (640.3) |

`alloc`（JIT 385.6、解释 333.0）与 `concat` 解释列（73.3）在这一轮读得慢是负载（after 列前 18–25 %）：交替重跑两次，`alloc` 265.6/266.5 对 260.0/271.0，
`concat` 21.7/22.5 对 21.3/21.8，`oo` 1112.4/1107.5 对 1099.7/1077.8，都在噪声内（负载 9–30 %）。

### 测试与检查

- 差分测试 `vm/jit/tests_iterators.rs`（7 个程序，解释 / 强制编译 / 热后 OSR 三种一致）：六种迭代器、迭代中追加/截短/改 `length`/清空/`shift`、
  `Set`/`Map` 迭代中增删、`Array.prototype[Symbol.iterator]` 与 `%ArrayIteratorPrototype%.next` 在 `GetIterator` 前后被改写、迭代器实例自带 `next`、
  内建 `next` 挂在别的类型的对象上（`TypeError`）、Proxy、访问器、带洞数组、类型化数组（分离的缓冲区、缩小的可调整缓冲区）、`break`/`return`/`throw`/`continue`
  触发 `IteratorClose`、`return` 抛错与返回非对象、解构与展开、`Array.from`/`new Map`/`Object.fromEntries`、生成器的 `for…of` 与 `yield*`。
- `boa_engine`（`boa/` 里）：默认、`--features jit`、`BOA_JIT_THRESHOLD=0` 三种全过（1195 / 1246 个单元测试加集成测试）；`BOA_JIT_FUZZ_COUNT=3000` 过（201.7 s）。
- Verve（`-p verve`）：无 feature 446 个、有 `jit` 449 个、`VERVE_JIT_THRESHOLD=0`、`VERVE_JIT=0` 全过，0 失败（含 `tests/jit.rs`）。
- test262（`E:	est262`，release `boa_tester --features boa_engine/jit`）：`built-ins/Array`、`ArrayIteratorPrototype`、`MapIteratorPrototype`、`SetIteratorPrototype`、
  `StringIteratorPrototype`、`Map`、`Set`、`String`，`language/statements/for-of`、`language/expressions/array`、`language/destructuring`，改动前后（解释与强制编译各一遍）
  逐项比较，**0 差异**（Array 3082/3082、Map 204、Set 383、String 1221/1223、for-of 740/751、destructuring 18/19，前后一致）。
  除 `staging` 的全部目录（`built-ins`、`language`、`annexB`、`intl402`、`harness`，共 52,095 项）解释（`BOA_JIT=0`）对强制编译（`BOA_JIT_THRESHOLD=0`）：
  只有已知的计时类 `built-ins/Atomics/notify/bigint/notify-all-on-loc.js`（解释失败、强制编译通过）不同，其余 0 差异；解释通过 49,897 项。
- `cargo fmt --all --check`、`bash scripts/style-check.sh`、`boa/` 里 `cargo fmt`、`cargo clippy -p boa_engine --all-targets -- -D warnings`（有、无 `--features jit`）都过。
  没跑：`bash scripts/linux-check.sh`（没有平台分支）、根的 `cargo clippy --workspace`（只改了 `boa/` 与文档）。
- 体积：`verve.exe`（release，默认 + `jit`）27,097,600 → 27,106,816 字节，+9,216（+0.03 %），写进 `size.md`；不带 `jit` 的构建没量。
- `perf-js.md` 追加了一节，`docs/jit.md` 追加「Results (P70)」并改了「Testing」与 phase 2.5「没做」里的 `for…of` 一条。

### 已知问题 / 给其他包的备注

- 快路径只对当前 realm 的内建 `next` 生效；跨 realm 的迭代器走老路径（行为不变，只是不加速）。
- 步进期间 Proxy 陷阱里的 `Error.stack` 少一行原生 `next` 帧（`boa/SU.md` 已记）。
- `Map` 的 `for…of` 剩下的成本在每个元素新建 `[key, value]` 数组；要再降需要让 `for (const [k, v] of map)` 的解构直接读条目，不在本包范围。
- 生成器与用户迭代器没有变化（本包只处理内建迭代器）。
- 没碰 `vm/call_frame/`（`iterators` 字段的类型不变），与 P69 无冲突。
