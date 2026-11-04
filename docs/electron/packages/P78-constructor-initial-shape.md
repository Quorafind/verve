# P78 `new` 出来的对象直接用构造器的最终形状

**依赖**：P69、P75（均已合并）  **规模**：M  **修改**：`boa/core/engine/src/builtins/function/mod.rs`（`function_construct`）、
`object/shape/`、`object/jsobject.rs` 与 `object/property_map.rs`（只限按预测形状分配）、`vm/`（只限记录预测所需的状态）、对应测试、
`boa/SU.md`、`docs/jit.md`、`docs/electron/perf-js.md`、`docs/electron/size.md`

## 目标

P69 把 `oo` 从 1106 ms 降到 725 ms，冲刺目标 600 ms 没到。它量出的剩余大头是：`new CX(i)` 比空构造的 `new C0()` 多 183 ns，
几乎都是构造器里 `this.a = …`、`this.b = …` 一次次走形状转换（查转换表、换形状、存储扩容）。
本包让 `function_construct` 按这个构造器**上一次构造结束时的形状**预先准备新对象，使构造器里的属性写入走已缓存的快路径。

目标（P60 `oo`，JIT，中位数）：**承诺 ≤ 650 ms，冲刺 ≤ 600 ms**；`new CX(i)` 与 `new C0()` 的差给前后纳秒数。

## 背景（先读）

- `P69-call-cost.md`「状态」：微探针表（`new C0()`、`new CX(i)`、`new D0()`、`new DX(i)`）与「`this` with the initial shape」一节。
- `object/shape/`：共享形状的转换表、`root_shape`、存储与形状的对应关系；`P75` 对唯一形状与共享形状的校验论证。
- V8 的「slack tracking」思路可以参考，但**不要求**照搬；这里要的是最小的、可证明不改变可观察行为的做法。

## 要做的事

1. **方案**（由实现者定，写明为什么不改变语义）。候选：
   - 只预留存储容量：记下构造器上次构造结束时的属性数，新对象按此预分配，省去扩容。
   - 预测转换链：记下上次的最终共享形状（弱引用）；新对象仍从根形状起步，属性写入沿已有的转换走，但让 `set` 的内联缓存直接命中。
   - 直接以最终形状分配并把槽位置为「未定义」：**只有在能证明属性顺序、`Object.keys`、`in`、`hasOwnProperty`、getter/setter 与 Proxy 观察都不变时才可用**，否则不选。
   每个候选写出测得的收益，选一个；放弃的写明原因。
2. **失效**：构造器的 `prototype` 被换掉、构造器里条件分支导致形状不同、子类（`super()` 返回的对象）、`Reflect.construct` 带不同的 `newTarget`、
   构造器返回别的对象、构造过程中抛错：各种情形下预测都只能是「快一点或不快」，不能是「错」。
3. **内存**：预测状态挂在哪（函数对象、代码块、`Vm`）、多大、何时释放，写进「状态」；`size.md` 追加一行。

## 测试

- 差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致，并与 Node 输出一致），每个用例打印 `Object.keys`、`Object.getOwnPropertyNames`、
  `JSON.stringify` 与原型：属性个数不同的多次构造；条件添加属性；构造中途 `delete this.a`；构造中途抛错再构造；构造器里 `Object.defineProperty(this, …)`；
  `prototype` 被换成别的对象或带 setter 的对象（setter 拦截 `this.a = …`）；`Reflect.construct(C, [], D)`；派生类与基类交替；类字段与私有字段；循环中途触发垃圾回收。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `language/statements/class`、`language/expressions/class`、`language/expressions/new`、`built-ins/Reflect`、`built-ins/Object` 前后逐项 0 回退；
  除 `staging` 外解释与强制编译逐项 0 差异（计时类 `Atomics.notify` 例外照旧）。
- P60 套件：`oo` 达到承诺目标；`alloc`、`vdom`、`closure`、`json` 不变慢（同一轮，中位差 ±3%）。`perf-js.md` 追加一行。

## 不做

调用帧瘦身（P69「状态」说明那是重新设计）；编译代码直接调用编译代码；对象字面量的形状预测。

## 状态

**完成。** 分支 `electron/p78-constructor-initial-shape`，基线 `docs/electron-plan` `c28e026c`。

**做了什么，与任务书的出入**：没有改 `function_construct`，也没有在函数对象或 `Vm` 上挂预测状态。预测放在**每个 `this.x = v` 站点的 `InlineCache` 里**
（新类型 `AddCache`，`vm/inline_cache/add.rs`）：站点记下「接收者形状 → 插入后的形状 + 槽位 + 原型链的印章」，下次同形状同原型链时直接换形状、`push` 值。
构造器的最终形状就是它各个站点转换的串联，所以不需要按构造器记「上一次的最终形状」。这是任务书候选 2（预测转换链让 set 缓存命中）的落地形式。

**拆账**（插桩的 rdtsc 计时器，一个构建，400 万次给新对象赋一个属性，负载 26–49 %，当份额读；`docs/jit.md`「Results (P78)」有完整表）：
`SetPropertyByName` 处理函数里一次创建属性的赋值共 116 ns：`cached_set` 未命中 3.2、`name` 克隆 4.6、接收者检查 2.4、**原型链两层查找 19.6**、
`PropertyMap::insert` 86.1（其中形状 `lookup` 2.3、转换 12.0、存储 `push`（首次分配）34.1，其余 37.7 是描述符转换、第二次查找和旧形状的释放）。
处理函数之外的约 60 ns 是指令分派、`GetArgument` 和对象回收时释放存储（推断，未量）。换形状本身不贵，贵的是查找、构造转换键和描述符；唯一躲不掉的是存储分配。

**候选与取舍**（微探针 ns，负载约 10 %，第三轮；之前之后）：

| 候选 | 测得 | 结论 |
|---|---|---|
| 只预留存储容量（临时补丁：首次创建时 `reserve_exact(6)`，未提交） | `new CX6(i)`（六个属性）约 560 → 480，省一次 4→8 的重分配约 70 ns；`new CX3(i)` 无变化；`new CX(i)` 慢 0–25 ns（一个属性占 96 字节）；`oo` 只有一个属性，收益为 0 | 不选。要不拖慢小对象就得在函数或代码块上记构造结束时的属性个数并在每次构造返回时写一次，只为五个属性以上的对象 |
| **预测转换链，让 set 缓存命中**（选定） | `new CX(i)` 比 `new C0()` 多 **177 → 83**；`new CX3(i)` 690 → 353；`new CX6(i)` 1122 → 503；`new F(i)` 349 → 249 | 选。每个站点只重放自己已经做过并检查过的那次插入 |
| 以最终形状分配，槽位置未定义 | 未做（没法做） | 构造期间 `'a' in this`、`hasOwnProperty`、`Object.keys(this)`、`JSON.stringify(this)` 会看到还不存在的属性，顺序是上一次构造的而不是这一次的（条件属性、`delete`、`defineProperty`），`this.a = v` 会写槽位而不是撞上原型上的 setter/只读属性。证明这些不可观察需要整个构造器体，调用点没有。比选定方案能多省的上限约是命中本身（每属性约 15 ns，推断） |

**为什么不会改变可观察行为**：命中时重放的恰是 `set_new_own_property` 做过并检查过的那一次插入，条件是它检查过的一切仍然成立：
接收者就是被写的对象；普通（四个内部方法按函数比较）且可扩展（`extensible` 不在形状里，每次命中都读）；接收者形状是共享形状，不可变——没有该名字、原型是记下的那个、转换到哪个形状是确定的；
形状持有原型，所以接收者形状钉住最近的原型**对象**，该原型的形状再钉住下一个，对象的内部方法创建后不变，记录时都是普通的（链上有代理就不记）；
每个原型的形状与记录相同，**唯一形状**（内建原型、字典模式对象）还要原型的属性个数相同——唯一形状增加属性是原地的、形状身份不变，其余变化（删属性、改种类、改属性位、换原型）都会换新形状（`unique_shape.rs`）。
所以之后任何原型上多出同名 setter、只读属性、`Object.prototype` 上的 setter，命中都会失败，走原来的通用 `[[Set]]`；命中还要求存储长度等于记下的槽位、借用成功、新形状没被回收，否则走原路并重新记录。
只在未缓存的插入**成功之后**才记录：走 setter、只读、代理、不可扩展、超过转换上限变成唯一形状的情形都不会被记录。一个站点见过超过四种形状就不再记录。

**失效**（每种情形都是「快一点或不快」）：

| 情形 | 结果 |
|---|---|
| 构造器的 `prototype` 被换掉 / 换成非对象 | 接收者形状变了（带不同原型）：未命中，走原路，再记一项 |
| 条件分支导致形状不同 | 每种形状各一项（四项以内），其余站点见到哪种形状就用哪种 |
| 构造中途 `delete this.a` / `Object.defineProperty(this, …)` / `setPrototypeOf(this, …)` / `preventExtensions(this)` | `delete`、`defineProperty`（新属性位）、`setPrototypeOf` 换形状，之后的站点未命中；不可扩展在每次命中时读到，走原路（严格模式抛 `TypeError`） |
| 子类（`super()` 返回的对象）、基类和派生类交替 | 基类构造器的站点按子类各见一种形状，超过四种就停止记录；永不出错 |
| `Reflect.construct(C, [], D)` | `D.prototype` 不同则形状不同，未命中后另记一项 |
| 构造器返回别的对象 / 抛错 | 已记录的项描述的是真实发生过的转换，留着无害；`this` 被丢弃或泄漏出去的状态与原来相同 |
| 原型（含 `Object.prototype`）事后获得 setter / 只读同名属性 | 原型印章不符，未命中，走通用 `[[Set]]`，setter 被调用 |
| 原型被回收、形状被回收 | 弱引用升级失败：未命中；站点满时先清掉已死的项 |

**内存**：预测状态在代码块里每个赋值站点的 `InlineCache.adds`（`AddCache`：`GcRefCell<Vec>` 加一个标志，40 字节；从不创建属性的站点只付这 40 字节）。
一项约 150 字节（两个弱形状、槽位、至多四个印章，印章是弱形状加一个计数），一个站点至多四项。只持弱引用，不延长任何形状或原型的寿命；随代码块一起释放。
函数对象、`Vm`、对象本身都没有新增字段。`size.md`：发布版 `verve` +10,752 字节（+0.04 %）。

**性能**（发布版 `verve --features jit`，Windows，`c28e026c` 对本分支，`--runs 7`，每项前后交替，机器共享，负载 5–21 %；JIT 列中位数（最好）毫秒）：

| 测试 | 第 2 轮前 → 后 | 第 3 轮前 → 后 | 目标 |
|---|---|---|---|
| `oo` | 721.9 (710.1) → 590.5 (574.5) | 708.6 (692.8) → 608.9 (591.0) | 承诺 ≤ 650：达到；冲刺 ≤ 600：两轮中一轮 |
| `oo`，解释器 | 811.6 → 684.4 | 789.1 → 690.4 | |
| `alloc` | 254.2 → 249.7（-1.8 %） | 245.7 → 253.4（+3.1 %） | ±3 % |
| `vdom` | 48.3 → 52.2（+8.1 %） | 52.7 → 50.4（-4.4 %） | ±3 % |
| `closure` | 437.9 → 441.8（+0.9 %） | 439.4 → 429.0（-2.4 %） | ±3 % |
| `json` | 1233.5 → 1237.4（+0.3 %） | 1214.0 → 1214.9（+0.1 %） | ±3 % |

`vdom` 第 2 轮出了 ±3 % 带（+8.1 %，最好值 +7 %），第 3 轮反向（-4.4 %）；它只用对象字面量，没有创建属性的赋值，所以不是这条路径的开销，是该项的方差（P75 已记录它方差大）。
再加两轮（`--runs 11`，负载 2–15 %）：`vdom` -3.2、-2.5 %，`alloc` +0.4、+2.7 %。负载：每轮运行前后各量五秒，全在 `bench_*.log`；微探针轮前后 11/10/10/11/11 与 10/10/14/10/8 %。
微探针（ns/次，第 3 轮，之前 → 之后）：`new C0()` 209 → 189，`new CX(i)` 386 → 273（差 177 → 83），`new CX3(i)` 690 → 353，`new CX6(i)` 1122 → 503，`new F(i)` 349 → 249，`new D0()` 550 → 498，`new DX(i)` 804 → 687，
`{}; o.x = i` 201 → 134，`{x:i}` 171 → 168（不变），`{}` 77 → 74。

**测试**：`vm/jit/tests_constructor_shape.rs`——37 个场景（属性个数不同的多次构造；条件首个属性；构造中途 `delete`；中途抛错再构造并检查泄漏出去的 `this`；构造器里 `defineProperty`（数据与访问器）；
`prototype` 被换成另一个对象或非对象；原型、中间原型、`Object.prototype` 事后获得 setter 或只读属性；代理做原型；`Reflect.construct` 带不同 `newTarget`（含类）；基类、派生类、再派生类交替；
`super()` 前有箭头函数；返回对象与原始值；类字段、私有字段、字段与构造器的顺序；循环中触发 `gc()`；构造器被丢弃后回收；构造后再加属性；严格与非严格下不可扩展的 `this`；构造中 `setPrototypeOf`；
十二个属性与超过转换上限的一千一百个属性；`Array`/`Map` 的子类），每个场景打印 `Object.keys`、`Object.getOwnPropertyNames`、`JSON.stringify` 与原型名，
解释、`Eager`（`BOA_JIT_THRESHOLD=0` 等价）、`Hot`（OSR）三种一致，且与 Node 22（`--expose-gc`）逐行相同；同一份脚本在基线构建和本分支的 `verve` 上（`VERVE_JIT=0`、`BOA_JIT_THRESHOLD=0`、`=7`）输出也与 Node 相同。
`vm/inline_cache/tests.rs` 加三个：创建属性的缓存按站点记录、原型形状变化时替换而不堆积；`Object.prototype` 获得 setter 后不命中（**变异检验**：把印章里的属性计数比较去掉，这个测试和场景 12 都失败）；站点见过太多形状后停止记录。
用变异检验发现并修正了第一版测试（每次新建原型对象，永远不命中缓存，没有测到东西）。

**验证**（`CARGO_BUILD_JOBS=6`）：`boa/` 内 `cargo test -p boa_engine` 默认 1233、`--features jit` 1290、`BOA_JIT_THRESHOLD=0 --features jit` 1290 个库测试全过（集成测试同样全过）；
`BOA_JIT_FUZZ_COUNT=3000 cargo test -p boa_engine --features jit --lib random_programs_match_the_interpreter`：`1 passed`，178 s；
`boa` 里 `cargo clippy -p boa_engine --all-targets [--features jit] -- -D warnings` 有无 `jit` 都过（两个都跑了）；`cargo fmt`（`boa/` 与根 `--all --check`）、`scripts/style-check.sh` 过；
根 `cargo clippy --workspace --all-targets -- -D warnings` 过，`cargo test -p verve` 与 `--features jit` 全过。
test262（`E:\test262` `d86b2294`，release `boa_tester --features boa_engine/jit`，`CARGO_PROFILE_RELEASE_LTO=off`，`-vv` 逐条排序比对）：
`language/statements/class`（4348）、`language/expressions/class`（4043）、`language/expressions/new`（59）、`built-ins/Reflect`（153）、`built-ins/Object`（3402）基线对本分支，解释与强制编译各比一次，**逐条 0 回退（行完全相同）**；
除 `staging` 外（`built-ins` 23716、`language` 23157、`annexB` 1084、`intl402` 3357、`harness` 116）`BOA_JIT=0` 对 `BOA_JIT_THRESHOLD=0`：**只差 `Atomics/notify/notify-all-on-loc.js` 与 `bigint/notify-all-on-loc.js`**（计时类，解释失败强制通过，照旧）。

**没做 / 注意**：预留存储容量（候选 1）测了没做，收益只在五个属性以上的对象（约 70 ns）；没有按构造器记最终形状，所以本包不改 `function_construct`；
没有在 `electron` 二进制（P63）下测；性能数字是单机共享、几轮的结果，`oo` 的 600 冲刺在两轮里达到一轮（590.5 与 608.9）；
`vdom` 的第 2 轮 +8 % 见上；Linux 交叉检查没跑（没有平台分支）。
**给其他包的备注**：创建属性的赋值现在有按站点的缓存；改 `UniqueShape` 的包要保持「增加属性原地、其余变化换新形状」这条不变式，印章的正确性靠它（`shape.unique_property_count()`）。
**提交**：`887438b6` perf(boa) 缓存创建属性的赋值（含 `boa/SU.md`、`vm/inline_cache/tests.rs`）；`9560e289` test(boa) 37 个场景；`c2afcf70` docs(jit) 拆账、论证与测量（`jit.md`、`perf-js.md`、`size.md`）；本节是最后一个 docs 提交。
