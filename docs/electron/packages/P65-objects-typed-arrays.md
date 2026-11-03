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

（实现者完成后填写）
