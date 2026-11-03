# P65 对象、集合与类型化数组热路径（解释器）

**依赖**：P60、P61  **规模**：L  **修改**：`boa/core/engine/src/{object,builtins/{typed_array,array_buffer,array,map,set,object,function}}/`、
`boa/core/engine/src/vm/opcode/{get,set,new,push,define}/`、`boa/core/engine/src/vm/inline_cache/`、`boa/SU.md`、对应测试。
**不改** `vm/jit/`、`vm/call_frame/`、`vm/opcode/call/`（P66 的范围）；需要动 `boa/core/gc` 的，写方案交给 P42（它是唯一可改 GC 的包）。

## 目标

React 式 UI 与编辑器类应用的主要开销是对象分配、属性读写、方法调用、集合与类型化数组。这些在解释器下差距最大：

| 项 | interp / Node |
|---|---|
| `Float64Array` 读写，1M × 5 | 854× |
| class 方法调用 / getter / `new`，1M | 420× |
| `Map`/`Set` 增删迭代 | 87×（含超线性，P61 后重测） |
| 分配循环（30 万个小对象） | 32× |
| 带比较函数的 `sort`，1M | 15× |

本包优化**解释器**本身（JIT 关时也受益，且 JIT 的慢路径就是解释器的处理函数）。
**参考目标**（剖析后由实现者提出、总控确认）：`typed` ≤ 100×、`oo` ≤ 80×、`alloc` ≤ 15×、`mapset` ≤ 20×、`sort` ≤ 8×、`vdom` 与 `closure`（P60 新增）有明确改善。

## 背景（先读）

- P60 的 `perf-js.md`；P61 的「状态」。
- `docs/jit.md`「Property caches and elements」：解释器的内联缓存（`vm/inline_cache`，最多 4 个形状、弱引用槽位）、稠密数组元素的快路径、
  `push`/`fill` 对"普通稠密数组"的捷径——本包在解释器里补齐同类捷径时照这个写法（判定条件写清楚、不满足就走规范路径）。
- `builtins/typed_array/`：整数索引奇异对象的 `[[Get]]`/`[[Set]]`；`vm/opcode/get`、`set` 里 `GetPropertyByValue`/`SetPropertyByValue` 对数组的快路径。
- `builtins/map/ordered_map.rs`：键是 `JsValue`，哈希与相等的实现。
- `builtins/array` 的 `stable_sort_by`（`boa/SU.md`）：比较函数每次都是一次"原生调 JS"。

## 要做的事

先剖析、再定目标、再改。已知的方向：

- **类型化数组**：`GetPropertyByValue`/`SetPropertyByValue` 遇到类型化数组且索引是在界内的 `int32` 时，直接按元素类型读写缓冲区
  （检查缓冲区未分离、长度跟踪的视图按当前长度），不走通用的属性键与 `CanonicalNumericIndexString`；`length` 读取同理。
- **属性访问与方法调用**：`a.add(b)`、`o.get()` 的方法在原型上，解释器缓存对原型属性的命中率与成本；getter 调用；
  `this.x = x` 在构造函数里的形状转换（转换缓存、预测最终形状）；对象字面量 `{i, s, n: {v: i}}` 的创建路径。
- **分配**：每个对象的分配次数与大小（形状、属性存储、`GcRefCell`）；能否少一次分配或少一次引用计数往返。
- **集合**：`Map`/`Set` 键的哈希（字符串每次重新哈希？数字的规范化），迭代器的逐项开销。
- **原生调 JS**：`sort` 比较函数、`map`/`filter`/`forEach` 回调从 Rust 调进 JS 的每次固定成本（参数数组、领域切换、影子栈）。
  与 P66 的「调用成本」重叠的部分（`CallFrame` 本身）归 P66；本包只改内建这一侧的调用方式。

每项改动前后用 P60 套件计时；没收益的不合。

## 验收

- P60 套件：参考目标逐项「达标/差距」；`perf-js.md` 追加本包合并后的完整一行。
- 正确性：`cargo test -p boa_engine` 全部通过；也用 `BOA_JIT_THRESHOLD=0` 跑一遍 `boa_engine` 测试（带 `jit` feature），确认 JIT 的慢路径不受影响；
  `cargo test --features jit` 下 `vm/jit/tests.rs` 的差分测试与默认 150 个模糊测试通过；
  test262 的 `built-ins/{TypedArray,TypedArrayConstructors,ArrayBuffer,Array,Map,Set,Object}` 与 `language/expressions`、`language/statements/class`
  前后逐项比较，不得有新失败。
- 新快路径各有退回条件的测试（分离的缓冲区、越界、负数与非整数索引、可变长度视图、原型上有 setter、Proxy、被冻结的对象等）。
- `boa/SU.md` 每项改动一条。

## 不做

`vm/jit/` 与调用帧（P66）；GC 策略与阈值（P42）；只对基准生效的特判。

## 状态

**第一阶段（分析）完成，等总控确认目标与顺序后开始实现。** 本轮只改本文档，没有改代码。分支
`electron/p65-objects-typed-arrays`，基线 `b2a5baa3`（`docs/electron-plan` 此后已前进到 `6335a760`，P62 已合并；
实现阶段开工前 rebase 过去，`boa/core` 的行为不受这些提交影响）。

一句话结论：套件里最大的两个差距**不是**「类型化数组慢」或「对象慢」，而是两处可以直接消掉的每次访问常数：

- 类型化数组的**具名**属性（含 `length`）每读一次 385 ns，其中 **172 ns 是 `CanonicalNumericIndexString`**
  ——这是**直接测量**（符号键对照，2.1），不是推断；
- class 构造比普通函数构造贵 **277 ns**，新对象上第一次写属性又贵 **319 ns**（2.2）。

套件的所有比值还压在一个**解释器每轮循环 34 ns、每次 JS 调 JS 136 ns** 的地板上，所以初稿的 `typed` ≤ 100×、
`oo` ≤ 80×、`sort` ≤ 8× 在 P65 范围内不可达——依据见第 3 节。

### 测量条件

- release 构建：`cargo build --release --bin verve --no-default-features --features wasm,jit`，Windows 10 x64，
  `CARGO_BUILD_JOBS=6`，`target` 是 `E:\cargo-target\verve-e-p65` 的 junction（从空目录全量构建）。
  解释器列是该二进制加 `VERVE_JIT=0`；没有第二个构建。
- 机器是共享的 Ryzen 9 7950X（32 线程），另外两个实现者并行。**负载逐轮记录**
  （`\Processor(_Total)\% Processor Time`，每轮测量前 5 次、后 5 次，%）。

  | 轮 | 内容 | 测量前 | 测量后 |
  |---|---|---|---|
  | S1 | 套件 7 项 | 12.5 6.7 10.8 10.3 10.8 | 10.6 10.5 14.0 8.8 5.9 |
  | S2 | 套件 7 项（复测） | 21.4 23.1 23.4 16.2 23.1 | 19.9 27.2 23.0 21.7 18.6 |
  | P1–P6 | 全部探针的分解实验，一段连续会话（2.1–2.7 的数字都出自这里） | 39.0 38.0 41.5 37.2 37.3 | 9.1 10.6 7.2 8.3 5.7 |
  | P7 | 地板与 `sort` 复测（2.5） | 12.4 10.3 11.3 18.2 24.2 | 10.5 16.7 16.3 18.1 17.7 |
  | P8 | 70k 字面量记录（6.1） | 16.3 20.9 23.1 27.5 33.6 | 20.2 27.0 31.0 31.9 27.5 |
  | P9 | 2.1 的关键对照复测 | 42.0 31.4 34.3 28.6 36.7 | 37.3 24.2 23.9 26.2 25.5 |

  P1–P6 是一段连续的探针会话，只在它开始前与结束后各采了一次负载，那一行括住的是整段（9.1–41.5%）；
  段内每一项没有各自的读数。分解实验的每一条结论都只用**同一段会话内相邻项之间的差**，
  所以段内负载的具体落点不影响 2.1–2.7 的判读。

- 套件：`bash scripts/js-bench/run.sh --runs 5 --only <test> target/release/verve.exe node`，一项一个进程，中位与最好。
- 分解实验：自写探针（`setup` 在计时区外跑一次，`run` 计时两次取第二次，`performance.now()`），
  **每个探针 5 个进程取中位与最好**，Node 与解释器各一遍，与套件同一套统计口径。
  探针只在会话 scratchpad 里，没有进仓库（`scripts/js-bench/` 归 P60）。
- **两轮套件的差异必须写在前面**：S1 与 S2 的负载读数相近（都在 5.9–27%），但 `typed` 从 3542 到 5053 ms（+43%）、
  `sort` 从 2593 到 3384 ms（+31%）、`alloc` +2%、`vdom` +7%。所以**比值只在同一轮内比较**，跨轮的绝对值不可比。
  下文以 S1（负载最低的一轮）为主，S2 一并列出，让读者看到噪声有多大。1 秒采样的负载读数会漏掉一次持续几分钟的
  重构建，这正好解释 `perf-js.md` 里「被占满的机器上每个测试慢 1.6–2.0×」那条记录。

### 1. 基线（套件，N=5，中位 (最好)，ms）

S1（负载 6.7–14.0%）：

| test | Node | Verve JIT | Verve interp | JIT / Node | interp / Node |
|---|---|---|---|---|---|
| `typed` 1M × 5 | 4.0 (3.9) | 3779.8 (3268.0) | 3542.2 (3449.8) | 945× | **886×** |
| `oo` 1M 方法调用 | 2.8 (2.8) | 1235.0 (1213.5) | 1346.0 (1266.3) | 441× | **481×** |
| `alloc` 300k 对象 | 10.7 (10.2) | 305.2 (292.7) | 327.6 (305.6) | 29× | **31×** |
| `mapset` 20k churn | 4.9 (4.2) | 37.1 (36.2) | 38.5 (37.5) | 7.6× | **7.9×** |
| `sort` 1M | 166.4 (165.4) | 2542.1 (2534.9) | 2593.4 (2496.9) | 15× | **16×** |
| `vdom` 2k 节点 × 5 | 5.0 (3.8) | 71.6 (66.8) | 71.5 (66.5) | 14× | **14×** |
| `closure` 1M map/filter/reduce | 24.9 (19.8) | 596.9 (593.3) | 637.5 (626.2) | 24× | **26×** |

S2（负载 16.2–27.2%，同一天稍后）：

| test | Node | Verve JIT | Verve interp | interp / Node |
|---|---|---|---|---|
| `typed` | 4.2 (4.1) | 4633.2 (4362.2) | 5053.4 (4953.9) | **1203×** |
| `oo` | 2.9 (2.8) | 1299.7 (1289.0) | 1387.8 (1328.1) | 479× |
| `alloc` | 11.0 (10.1) | 318.8 (307.9) | 335.4 (321.8) | 30× |
| `mapset` | 6.3 (5.7) | 53.7 (37.7) | 49.4 (47.5) | 7.8× |
| `sort` | 177.1 (167.5) | 3263.2 (3205.3) | 3384.3 (3197.6) | 19× |
| `vdom` | 5.0 (3.9) | 78.3 (72.2) | 76.7 (73.9) | 15× |
| `closure` | 21.1 (20.5) | 612.2 (607.5) | 659.4 (640.0) | 31× |

与 [`perf-js.md`](../perf-js.md) §1 相比（用 S1 的解释器中位）：`typed` −0.6%、`oo` +2.0%、`alloc` +2.5%、
`mapset` −90.6%（P61 之后；`perf-js.md` 那一行是 P61 之前的）、`sort` +0.3%、`vdom` −1.1%、`closure` −3.4%。
除 `mapset` 外都在 ±3.5% 内，说明 S1 复现了 P60 的表。**上一版本文档记的 `vdom` 23×（93.5 ms）是 65% 负载那一轮的读数，
已作废**，正确值是 14×。

### 2. 每项慢在哪里

以下全是**解释器列**、5 个进程的中位、1M 次迭代口径（除注明者）。地板 = 同一轮里
`for (let i = 0; i < 1e6; i++) s += i`：34.3 ms（P2、P7）、36.1 ms（P1）。

#### 2.1 `typed` 886×：先修具名属性，再修元素

套件的 `typed` 拆成两半（P2，同一进程相邻项）：

| 探针 | 解释器 ms | 说明 |
|---|---:|---|
| `typed-as-is`（套件原样：6 M 次 `f.length` + 16 M 次元素访问） | 3434.5 | |
| `typed-hoisted`（`length` 提到循环外，元素访问不变） | **1126.8** | 6 M 次 `length` = **2307.7 ms，占 67%，385 ns/次** |
| `typed-noelem`（元素访问也去掉，只剩 6 个循环与算术） | **344.2** | `typed` 的地板（86×） |

成因**已直接测出**，不是推断。三组对照（P1，同一进程相邻项，1M 次）：

| 探针 | 解释器 ms | 每次 ns | Node ms |
|---|---:|---:|---:|
| `ta-len`（字符串键 `"length"`，落在 `%TypedArray%.prototype` 的访问器） | 371.4 | 371 | 0.57 |
| `ta-byteoffset`（getter 体只有一次字段读取） | 371.6 | 372 | 0.39 |
| `ta-bytelength` | 376.0 | 376 | 0.57 |
| `ta-buffer`（getter 体 clone 一个 `BufferObject`） | 394.1 | 394 | 7.45 |
| `ta-symbol-key`（**符号**键：六个内部方法里 `PropertyKey::Symbol => None`，直接跳过 `CanonicalNumericIndexString`） | **198.9** | **199** | 1.15 |
| `ab-bytelength`（同样形态的访问器，接收者不是整数索引奇异对象） | **98.7** | **99** | 2.68 |
| `ta-string-key-missing`（字符串键、原型上没有） | 373.0 | 373 | 0.39 |
| `obj-symbol-key` / `obj-string-key-missing`（普通对象上的同类读） | 131.3 / 139.7 | 131 / 140 | 0.39 / 0.39 |

读法：

1. **`ta-len` − `ta-symbol-key` = 172 ms / 1M = 172 ns/次**。两者走同一个 `typed_array_exotic_*` 内部方法、
   同一个访问器、同一条原型链，唯一差别是键的类型，而 `PropertyKey::Symbol(_) => None` 正是绕开了
   `canonical_numeric_index_string`。**这 172 ns 就是那个函数的成本**：`ToNumber(键)` 加一次 `JsString::from(n)`
   分配、再比较。`ta-string-key-missing`（373.0）比 `obj-string-key-missing`（139.7）贵的 233 ns 是同一件事的
   第二个证据，只是那里还叠了原型查找。
2. 四个 getter 的体重差得远（一次字段读取 vs 一次 `BufferObject` clone），代价却都在 371–394 ns：
   **代价不在 getter 体里**。
3. `ta-len` − `ab-bytelength` = 273 ns：同样形态的访问器，仅因接收者是整数索引奇异对象就贵 273 ns。

**P9 在 24–42% 负载下复测了这一行**（`floor` 37.1、`ta-len` 372.3、`ta-symbol-key` 200.4、`ta-byteoffset` 386.9、
`ab-bytelength` 98.8）：`ta-len` − `ta-symbol-key` = **171.9 ns**，与 P1 的 172 ns 一致。**这个差值不受负载影响**，
所以 2.1 的三条读法都是实测结论，实现第 1、2 项时不必再为它做假设检验。

元素访问每次 49 ns（`1126.8 − 344.2 = 782.6` 摊到 16 M）。单次对照（P2）：`elem-i32-index` **78.6 ns**、
`elem-array-i32`（稠密普通数组，已有快路径）**62.1 ns**、`elem-out-of-range` 134.3 ns。
即类型化数组的元素读比普通稠密数组贵 16.5 ns，而后者自己也在高于地板 27.8 ns 的位置。

**打算怎么改**：

1. `canonical_numeric_index_string` 前面加一个只看前几个字节的预判：能回环的键只有规范数字串
   （首字符 `0`–`9`，或 `-`、`.`、`I`、`N` 开头的 `-0`、`Infinity`、`NaN` 一类）——规范里带空白、`+` 号或任何其他
   字符的都不满足 `ToString(ToNumber(s)) == s`。其余键直接判否，不解析、不分配。六个内部方法共用这一处
   （`builtins/typed_array/object.rs`）。
2. `GetLengthProperty` 对类型化数组加一条与数组同样的快路径：接收者是类型化数组、它**自己没有** `length`、
   且 `%TypedArray%.prototype.length` 仍是内建访问器时，直接读 `array_length`；其余情形退回今天的路径
   （`vm/opcode/get/property.rs`，在任务书允许的范围内）。
3. `GetPropertyByValue`/`SetPropertyByValue` 遇到类型化数组 + `PropertyKey::Index` 且在界内时按元素类型直接读写
   缓冲区；缓冲区已分离、视图越界、索引 ≥ `array_length` 一律退回规范路径。可变长度视图（`is_auto_length`）
   每次现取缓冲区长度，`byte_offset` 每次现算。

**预计收益**（按上表推算到套件的 6 M / 16 M）：修 1 去掉 6 M × 172 ns = 1034 ms；修 2 去掉 `length` 剩下的
6 M × 213 ns = 1278 ms，两项合起来让 `length` 归零，`typed` = `typed-hoisted` = 1127 ms；修 3 让元素从 78.6 ns
降到普通稠密数组的 62.1 ns，省 16.5 × 16 M = 264 ms → **862 ms（216×）**。若元素路径能一并短路到约 50 ns，
省 458 ms → **669 ms（167×）**。

**目标 ≤ 250×（推算 167–216×）。**

#### 2.2 `oo` 481×：`new` 与第一次写属性

| 探针（P2、P3，1M 次） | 解释器 ms | Node ms |
|---|---:|---:|
| `oo-as-is`（套件形态：1 次 `new` + 2 次方法调用 + 1 读 + 1 写） | 1229.8 | 2.14 |
| `oo-new-only`（`class A{constructor(x){this.x=x}}`，`new A(i)`） | 884.0 | 1.83 |
| `oo-new-nofield`（同一 class，构造函数空） | 563.5 | 1.48 |
| `oo-calls-only`（两次方法调用，不构造） | **555.1** | 0.39 |
| `obj-method`（原型方法：取 + 调 + 读 `this.x`） | 220.8 | 0.40 |
| `obj-get-only`（只取不调） | 110.5 | 0.29 |
| `obj-getter` / `obj-getter-nostore`（原型上的 getter） | 194.8 / 172.6 | 0.40 / 0.39 |
| `obj-read` / `obj-write`（已存在的形状，读 / 写） | 61.9 / 48.6 | 0.39 / 0.40 |

构造本身（P3，同一进程相邻项）：

| 用例 | 解释器 ms / 1M | Node ms | 与 `new-zero-arg-fn` 的差 |
|---|---:|---:|---:|
| `new-zero-arg-fn` `function C(){}` | 246.1 | 1.44 | — |
| `new-one-field-fn` `function C(x){this.x=x}` | 564.9 | 1.82 | **+319 ns** |
| `new-two-field-fn`（两个字段） | 775.0 | 2.22 | +529 ns（第二个 +210） |
| `new-class-empty` `class C{}` | 523.4 | 1.45 | **+277 ns** |
| `new-class-ctor-empty` `class C{constructor(){}}` | 514.8 | 1.44 | +269 ns |
| `new-class-field` `class C{x=1}` | 949.0 | 1.77 | +703 ns（`define_field`） |

两条清楚的差：

- **class 构造比同等的普通函数构造贵 277 ns**（523.4 对 246.1）。
- **新对象上写第一个属性 319 ns**（564.9 对 246.1），而往已存在的形状写只要 14 ns（`obj-write` 48.6 − 地板 34.3）
  ——**差 22 倍**。

对象字面量（P4）：`lit-empty` 120.5（造一个空对象 86 ns）、`lit-one` 240.2（一次 `DefineOwnPropertyByName` **120 ns**）、
`lit-then-set` 313.9（一次 `SetPropertyByName` 插入 **193 ns**）、`lit-3nested` 684.9（套件的内层形态）。

**打算怎么改**：

1. **新属性的 define/set 捷径**（收益最大，两处共用）：对象可扩展、普通对象、目标键不存在、属性是默认数据属性
   （可写可枚举可配置）时，直接按新属性插入形状。`[[DefineOwnProperty]]` **不查原型链**（只有 `[[Set]]` 查），
   所以 define 那一侧条件更少；set 那一侧要额外确认原型链上没有同名 setter。
   照 `docs/jit.md`「Property caches and elements」里 `push`/`fill` 的写法：判定条件写清楚、不满足走规范路径。
   落在 `vm/opcode/define/own_property.rs`、`vm/opcode/set/property.rs` 与 `object/` 的对应处。
2. **class 构造**：`function_construct` 里 class 专属的几段（`code.is_class_constructor()`、
   `initialize_instance_elements` 的两次 `downcast_ref::<OrdinaryFunction>` 与 `get_private_methods()`/`get_fields()`
   遍历、`code.is_derived_constructor()`）。实现时先加临时计数器把这 277 ns 拆开，再把「无私有方法、无字段、
   非派生」这条常见路径压成一次判断。

**预计收益**：首个属性 319 → 约 20 ns，省约 300 ms；class 省 277 ms → `oo` ≈ 769 ms。
**目标 ≤ 300×（推算 275×）。**

#### 2.3 `alloc` 31×：每个对象的固定开销

`alloc` 每次迭代 1092 ns（327.6 ms / 300 k）。按上面实测的单价拆：2 个对象 × 86 = 172 ns，
3 次 define × 120 = 360 ns，其余（`'k' + i`、`push`、`length` 读）约 560 ns。
**define 的 360 ns 是本包能拿到的一块**，其余主要是 P64 的字符串拼接。

**改法**：同 2.2 第 1 条，同一处捷径。
**预计收益**：define 120 → 约 50 ns（省 3 × 70 = 210 ns），对象创建 86 → 约 60（省 2 × 26 = 52 ns），
每次省 262 ns → 300 k × 262 = 78.6 ms → **249 ms（23×）**。

**目标 ≤ 25×（推算 23–24×）。** 初稿的 ≤15×（164 ms，每次 547 ns）需要把字符串拼接也拿下，那归 P64。

#### 2.4 `mapset` 7.9×：已达标

| 探针（P4、P6） | Verve ms | Node ms | 备注 |
|---|---:|---:|---|
| `map-str-set-new`（20 万次插入新字符串键） | 85.6 | 31.8 | 1.5× |
| `map-num-set`（1M 次数字键） | 297.3 | 88.1 | 3.4× |
| `map-int-get`（1M 次 `get(i % 1000)`） | 139.0 | 3.9 | **139 ns/次** |
| `map-str-get`（1M 次字符串键） | 212.2 | 6.6 | **212 ns/次** |
| `map-iter-values-10r`（2 万项 × 10 轮） | 72.1 | 0.42 | 迭代便宜 |
| `set-iter-10r` | 65.7 | 0.40 | 同上 |
| `map-iter-pairs-10r`（`for (const [k,v] of m)`） | **360.2** | 0.97 | **比 `values()` 贵 5.0×** |

插入与迭代都不贵，贵的是 `get`（139–212 ns）与 `for (const [k,v] of …)` 的解构（见 2.7）。
`get` 落在 `builtins/map/mod.rs` 的 `get_this`（参数与 `this` 的校验 + `borrow`）+ `OrderedMap::get` →
`IndexMap` 查找（键是 `JsValue`，`Equivalent<MapKey> for JsValue` 用 `PartialEq` = `same_value_zero`）。
`get` 的 139/212 ns 分几段、各占多少，实现第 7 项时用临时计数器确认。

**目标 ≤ 8×（已达标；初稿 ≤ 20×）。** 本项真正影响的是 `vdom` 与真实应用（`Map` 做 key→node 索引）。

#### 2.5 `sort` 16×：比较次数多 19%

带计数器的比较函数（P5、P7，同一件工作）：

| 规模 | Verve 解释器 ms | Verve 比较次数 | Node ms | Node 比较次数 |
|---|---:|---:|---:|---:|
| 1M | 2528.5–2638.6（`sort-only`） | **20,459,184** | 167.4–172.5 | 17,204,934 |

**Verve 的合并排序比 V8 多比较 19%**（`stable_sort_by`，`RUN = 16` 的插入块 + 归并）。
排序本身 = `sort-only` − `sort-fill-only` = 2528.5 − 260.2 = 2268 ms，摊到 20.46 M 得 **111 ns/次比较**；
Node 是 161.3 ms / 17.2 M 得 9.4 ns。而内建调进 JS 的地板：`call-from-builtin`（`forEach` 回调，1M 次）
141.5–142.1 ms、`call-js`（JS 调 JS）136.2–145.1 ms。**111 ns 已低于「一次 JS 调 JS」的 136 ns**，
说明比较函数本身的调用成本已被摊销，剩下的就是**比较次数**与 `sort_indexed_properties` 的读回写。

**改法**：`stable_sort_by` 的块大小与归并（向 V8 的 17.2 M 靠）；`sort_indexed_properties` 与回写在
「普通稠密数组」上用元素直通（照 `push`/`fill` 的判定写法）。
**预计收益**：比较降到 17.2 M ⇒ 排序段 1907 ms，加填充 260 ms = 2167 ms（**13.0×**）；
读回写再省约 150 ms ⇒ 2017 ms（**12.1×**）。

**目标 ≤ 14×（推算 12.1–13.0×，留一点余量给 S1/S2 那种 31% 的轮间漂移）。**
初稿 ≤ 8×（1331 ms）需要每次比较降到约 62 ns，那是 P66 的调用成本。

#### 2.6 `vdom` 14×：已经不错，本包只拿到一小块

`vdom` 71.5 ms。`vdom-lite`（只造 2 k × 5 个 vnode，5 个属性、`props: {}`、`children: []`）**15.6 ms（22%）**，Node 1.4 ms。
其余 56 ms 在 diff：`Map` 的 `set`/`get`（每轮 2000 + 2000，5 轮，按 2.4 约 3.5 ms）、`for…of` 子节点、
键拼接（`path + '/' + child.key`、`'s' + s + '-' + i`）——那是 P64 的字符串拼接（`concat` 一行在本套件里 36×）。

**本包能拿到的**：对象字面量/define 捷径（vdom-lite 里约 1 万次 define 约 0.7 ms）与 `Map` 的约 3.5 ms。
**预计收益**：71.5 → 约 67 ms。**目标 ≤ 14×（保持；推算 13–14×）。** 初稿只要求「明确改善」；
本轮把这个数从被污染的 23× 修正为 14×，实现阶段以 14× 为基线。

#### 2.7 `closure` 26×：回调已在调用成本上

`closure` 637.5 ms。三段（P6，1M 次）：`map` 232.3、`filter` 146.9、`reduce` 120.8（合 500），
其余 137.5 ms 是建 1M 元素的输入数组。
内建调 JS 的地板 `call-from-builtin` 141.5 ms 与 `call-js` 136.2 ms 几乎一样：**每次回调约 142 ns 就是 P66 的成本**。
本包只拿得到结果数组的写入——`map` 的 1M 次追加，按 2.2 的捷径省约 70 ms。

**预计收益**：637.5 → 约 568 ms。**目标 ≤ 24×（推算 23×）。** 余下归 P66。

顺带量到一件影响应用代码、在套件里只露出一角的事：**数组解构极贵**（P5，1M 次）。
`destr-literal`（`const [a, b] = p`）**1337.8 ms** 对 `index-only`（`p[0] + p[1]`）**100.0 ms**；
`getiter-only`（`p[Symbol.iterator]()` + 两次 `next()`）874.6 ms、`iter-while` 843.0 ms。
`for (const [k,v] of m)`（360.2）对 `m.values()`（72.1）差的 5.0× 就是它。
解构的 opcode 在 `vm/opcode/iteration/`（本包范围），**接收者是数组时可按索引直读**，不必走迭代器协议。
它几乎不动上面五个数（`mapset` 里 2 万次解构约 5.8 ms），但那是 React 式代码（`entries()`、`Object.entries`
之后逐个解构）的日常写法，所以列为第 9 项；收益不足就不合。

### 3. 参考目标（本包提出的最终值，请总控确认）

三个不达标的，原因同一个：解释器每轮循环 34 ns、每次 JS 调 JS 136 ns 的地板，而 `vm/opcode/call/` 与
`CallFrame` 归 P66。

- `typed`：地板（`typed-noelem`，6 个循环 + 算术）就是 **344 ms = 86×**。初稿 ≤100× 是 400 ms，意味着 16 M 次
  元素访问总共只能花 56 ms = 3.5 ns/次，而现有最好的稠密数组路径自己就高于地板 28 ns（62.1 对 34.3）。不可达。
- `oo`：每次迭代 1 次 `new` + 2 次方法调用。两次调用不构造的地板就在 `oo-calls-only` 里：**555 ms = 198×**。
  初稿 ≤80× 是 224 ms，连这个地板都不到。不可达。
- `sort`：比较降到 V8 的 17.2 M 后，17.2 M × 111 ns = 1907 ms，加填充 260 ms = **2167 ms = 13.0×** 就是地板。
  初稿 ≤8× 是 1331 ms。不可达。

| 项 | 现状（S1） | 任务书初稿 | **本包提出的目标** | 推算 | 依据 |
|---|---|---|---|---|---|
| `typed` | 886× (3542.2 ms) | ≤ 100× | **≤ 250×** | 167–216× | 2.1；地板 86× |
| `oo` | 481× (1346.0 ms) | ≤ 80× | **≤ 300×** | 275× | 2.2；两次调用地板 198× |
| `alloc` | 31× (327.6 ms) | ≤ 15× | **≤ 25×** | 23–24× | 2.3 |
| `mapset` | 7.9× (38.5 ms) | ≤ 20× | **≤ 8×（已达标）** | — | 2.4 |
| `sort` | 16× (2593.4 ms) | ≤ 8× | **≤ 14×** | 12.1–13.0× | 2.5；地板 13.0× |
| `vdom` | 14× (71.5 ms) | 「明确改善」 | **≤ 14×（保持）** | 13–14× | 2.6 |
| `closure` | 26× (637.5 ms) | 「明确改善」 | **≤ 24×** | 23× | 2.7 |

JIT 列会跟着动的是改在内建里的（2.1 第 1 条、2.2/2.3 的 define 捷径、2.4 的 `Map`）——它们在 JIT 的慢路径里
是同一份代码；只在解释器 opcode 里的（2.1 第 2、3 条、2.2 的构造、2.5、解构）只动解释器列。每项都用两列分别记。

### 4. 改动顺序（每项前后用套件计时，没收益的不合）

1. **类型化数组具名属性**：`canonical_numeric_index_string` 的前置过滤（`builtins/typed_array/object.rs`）。
   收益最大（约 1034 ms）、风险最低。
2. **`GetLengthProperty` 对类型化数组的快路径**（`vm/opcode/get/property.rs`）。与第 1 项合起来让 `length` 归零。
3. **类型化数组元素直读**（`vm/opcode/get/property.rs`、`set/property.rs`）。
4. **新属性的 define 捷径**（`vm/opcode/define/own_property.rs`、`object/`）——`alloc`、`vdom`、对象字面量。
5. **新属性的 set 捷径**（`vm/opcode/set/property.rs`）——构造函数里的 `this.x = x`。
6. **class 构造**（`builtins/function/`、`vm/opcode/new/`），先加临时计数器把 277 ns 拆开再改。
7. **`Map.get`/`has`**（`builtins/map/`）——`vdom`、应用代码；一并处理第 5 节的两条。
8. **`sort` 的读入/回写直通与比较次数**（`builtins/array/`）。
9. **数组解构 / `for…of` 数组快路径**（`vm/opcode/iteration/`）——套件里只动 `mapset` 一点，
   若无净收益就只留分析、不合代码。
10. **对象创建本身**（`Template::create` 的分配次数）——只在 4、5 之后还有余量时做。
    `boa/core/gc` 不动（归 P42）；若第 10 项需要动 GC，写成方案交给 P42。

每项的退回条件测试（写进实现）：缓冲区已分离、视图越界、`is_auto_length` 视图在缓冲区增长后长度变化、
负数与 `-0` 与非整数与 `1e21` 索引、原型上有 setter、Proxy 包住的类型化数组、被 `preventExtensions` 的对象、
`Object.defineProperty` 改成访问器之后再读、`ta.length` 被定义成自己的数据属性、用户改写
`%TypedArray%.prototype.length`、符号键、稀疏/字典模式的普通对象、`__proto__` 键。

### 5. P61 留给本包的两条（都在 `builtins/map`，属本包范围）

- **`clear()` 在有活迭代器时会截断列表**（规范只把条目标空，迭代器应当看不到之后新加的条目）。
  这是行为缺陷、不是性能项；按 P61 的交付说明由本包修，单列一个 commit 与一条 `boa/SU.md`。
- **`map.delete(map.keys().next().value)` 每次留一个放弃的迭代器**，持锁到 GC，两次 GC 之间不压实、墓碑堆积，
  `keys().next()` 变成 O(墓碑数)。第 7 项做 `Map` 时一并试：让一次性 `next()` 返回后立即还锁。

### 6. 给其他包的备注

#### 6.1 P64 移交的 70k 条记录（属本包的对象与分配范围）

P64 实测：不经 JSON、直接用字面量建 70,000 条记录，Verve 286.5 ms 对 Node 8.8 ms。本轮复现（P8，5 次中位）：

| 探针 | 解释器 ms | Node ms | 比值 |
|---|---:|---:|---:|
| `records-70k`（`{id, name, tags:['a','b','c'], nested:{x, y, z:[1,2,3]}}` × 70 000） | **234.6（最好 211.6）** | 14.2（11.3） | **16.6×** |
| `records-70k-noarrays`（去掉两个数组字面量与 2 个属性） | 128.0 | 9.5 | 13.4× |
| `records-70k-flat`（只 `{id, name}`，无外层数组、无 push） | 43.8 | 2.2 | 20.2× |

本轮的绝对值比 P64 低（234.6 对 286.5 ms；Node 这边相反，14.2 对 8.8 ms），所以**比值**（16.6×）比 P64 那组的
32.6× 低——两组的负载不同，按本文件的规矩只在同一组内比较。每条记录 3351 ns（Node 202 ns）。每条记录的账：

- 4 次对象/数组创建 × 86 ns = 344 ns，7 次 define × 120 ns = 840 ns，合计 **1184 ns = 82.9 ms（占 234.6 ms 的 35%）**。
  形状是外层对象 4 个属性、`tags` 数组 3 个元素、内层对象 3 个属性、`z` 数组 3 个元素，所以创建 4 次、define 7 次。
- 剩下的约 150 ms 主要是 2 次字符串拼接（`'item' + i`、`'yy' + i`）与 `push`、内层数组；
  `records-70k`（234.6）减 `records-70k-noarrays`（128.0）差的 106.6 ms 正是两个数组字面量加 `tags`/`z` 两个属性。

**P65 的预计收益**：define 从 120 ns 降到约 50、创建从 86 降到约 60，每条记录省 7 × 70 + 4 × 26 = **594 ns**，
70 000 条省 **41.6 ms**：**234.6 → 约 193 ms，比值 16.6× → 约 13.6×**。
**这个数的天花板在 P64**：剩下的约 150 ms 里两次字符串拼接是大头（`concat` 一行在本套件里 36×），
P64 拿下拼接后同一条记录还能再降一大截。本包不会为了这一个数做特判。

#### 6.2 给 P66

`closure`（回调约 142 ns/次）、`sort`（比较 111 ns/次）、`oo`（两次方法调用 555 ms/1M）三个数的剩余部分都落在
调用成本上——`CallFrame` 与 `vm/opcode/call/` 归 P66。本包只改内建这一侧的调用方式（`map`/`filter`/`forEach`/
`sort` 的结果写入），不动调用帧。

#### 6.3 给 P64

`vdom` 的 56 ms 里主要是字符串拼接与 `for…of` 字符串键；`typed` 与 `alloc` 修完剩下的也都有字符串成分；
6.1 的 70k 记录的天花板也在拼接。本包不碰 `boa/core/string` 与 `builtins/{string,regexp,json}`。

#### 6.4 给 P42

第 10 项（对象创建本身的分配次数）若需要动 `boa/core/gc`，写方案交给 P42，本包不动 GC。

### 7. 没做 / 待确认

- **没改任何代码**。本轮只改本文档；验收项（`perf-js.md` 追加本包那一行、`boa/SU.md` 每项一条、test262 前后
  逐项比较、`cargo test`）留到实现阶段。test262 在 `E:\test262`（`boa/test262_config.toml` 固定 d86b229）。
- **2.2 的 277 ns（class 构造）还没拆到具体行**。它是同一进程相邻项的差值测量，可靠；但「哪几段各占多少」
  要在实现第 6 项时加临时计数器才能定（计数器不提交、不留在代码里）。
- **`Map.get` 的 139/212 ns 的三段拆分**同理，实现第 7 项时确认。
- **套件两轮差异 31–43%**（S1 与 S2）。实现阶段每项前后都在同一轮里测，并在「状态」里记该轮测量前后的负载读数；
  某一轮明显偏离就重测、不引用。
- 探针脚本在会话 scratchpad，未进仓库。若希望它们进 `scripts/js-bench/`，需要 P60 同意（那个目录归 P60）。
- 未跑 `cargo test --workspace`、未跑 test262、未做 release 体积复查——按 `conventions.md` 归总控/实现阶段。
