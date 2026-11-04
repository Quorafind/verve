# P74 字符串的三个后续：数字转字符串、rope 内存、字符串迭代

**依赖**：P70、P71（均已合并）  **规模**：S–M  **修改**：`boa/core/string/`、`boa/core/engine/src/value/`（数值到字符串）、
`builtins/number/`（只限转字符串）、`builtins/string/string_iterator.rs`、对应测试、`boa/SU.md`

## 目标

P71 让 `+=` 变成线性，但 P60 套件的 `concat` 仍是 Node 的 8.1×（目标 5×）；P70 让字符串的 `for…of` 从约 500 ns 降到 73 ns，
剩下的在 `String::substring`。三件小事，各自独立 commit，都先测再改：

1. **数字转字符串**（`concat` 的大头）：P71 测得同一进程里 `x = 'ab' + i` 单独就占 5.7 ms 中的 2.3 ms；`'ab' + i` 先把 `i`（`int32`）
   经 `JsValue::to_string` → `JsString::from(i32)` 转成字符串，再拼接。
   - 先拆账：`i32` → `JsString` 一次多少纳秒、分配占多少；`concat_array` 每次调用里查静态字符串表（P71 报告提到）占多少。
   - 候选：`i32` 转换走栈上缓冲一次分配（不经 `String`/`format!`）；小整数（例如 0–255）用静态表；`concat_array` 在结果不可能命中静态表时
     （长度超过表里最长的串）跳过查表。只做测出有收益的。
   - **目标**：`concat` ≤ Node 的 5×；`'ab' + i` 循环的微探针前后对比。
2. **rope 的叶子合并**（P71 方案第 4 阶段）：32 万次追加的内存峰值约 66 MB（P71 之前约 20 MB），因为每次追加都留下一个 88 字节节点直到第一次读取。
   - 右侧是短串（例如 < 64 码元）且左侧是**未被共享的** rope（引用计数为 1）时，把短串并入右侧叶子（可以是一个预留容量的可增长叶子），不新建节点。
   - **目标**：32 万次追加的内存峰值 ≤ 30 MB；`concat` 时间不变慢；P71 的全部 rope 测试照旧通过，另加「共享的 rope 不被原地修改」的测试
     （`a = s + 'x'; b = s + 'y'` 之后 `a`、`b`、`s` 各自正确）。
   - 若测量表明合并让 `concat` 变慢或改动面过大，停下来在「状态」写明数字，不做。
3. **字符串迭代**：`StringIterator::advance` 每步经 `String::substring` 新建一个字符串。
   - 单个码元（非代理项对）的字符用静态表或已有的单字符字符串缓存（Boa 若已有就复用，没有就加，限 Latin-1 范围）；代理项对照旧建两码元的串。
   - **目标**：字符串 `for…of` 每元素（编译）≤ 35 ns（P70 后 72.8）。

## 背景（先读）

- `P71-rope-strings.md`「状态」（「Why `concat` misses 5x」、偏离方案的说明）；[`spikes/p64-rope.md`](../spikes/p64-rope.md) 第 4 阶段与「风险」。
- `P70-iterators.md`「状态」；`boa/core/string/src/vtable/rope.rs`、`lib.rs` 的 `concat_array`、`concat_lazy`；
  `boa/core/engine/src/value/mod.rs` 的 `to_string`；`builtins/string/string_iterator.rs`。

## 验收

- 每项的前后数字（同一轮、5 个进程、中位与最好，记负载）；`perf-js.md` 追加一行；体积增量写进 `size.md`。
- `cargo test -p boa_string`（rope 的深链、共享、孤立代理项测试）；`boa/` 内 `cargo test -p boa_engine`（默认与 `BOA_JIT_THRESHOLD=0 --features jit`）、
  clippy `-D warnings`；根 `cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262：`built-ins/String`、`built-ins/Number`（`toString`）、`language/expressions/addition`、`language/expressions/template-literal`、
  `built-ins/StringIteratorPrototype` 前后逐项 0 回退。
- P60 套件字符串类各项不变慢。

## 不做

浮点数转字符串（`ryu_js`）本身；`Array.prototype.join`；展平后再追加的 O(n²)。

## 状态

未开始。
