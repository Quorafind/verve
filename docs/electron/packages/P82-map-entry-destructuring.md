# P82 `for (const [k, v] of map)` 不再为每个条目新建数组

**依赖**：P70（已合并）  **规模**：M  **修改**：`boa/core/engine/src/bytecompiler/`（`for…of` 与数组解构的编译）、`vm/opcode/`（迭代相关指令）、
`builtins/iterable/`、`builtins/map/map_iterator.rs`，如需则 `vm/jit/`（只限新指令的编译），对应测试、`boa/SU.md`、`docs/jit.md`、`docs/electron/perf-js.md`

## 目标

P70 让迭代器步进不再分配结果对象，但 `Map` 的 `for…of` 剩下的成本在每个元素新建一个 `[key, value]` 数组，随后立即被解构、丢弃。
`for (const [k, v] of map)`、`for (const [k, v] of map.entries())`、`for (const [i, x] of arr.entries())` 是常见写法。
本包让这几种写法在可证明不可观察时直接取出键与值，不建数组。

目标：上述三种循环每个元素的耗时给出前后纳秒数（解释与 JIT 各一组）；P60 `mapset` 不变慢，若受益给出数字。

## 背景（先读）

- `P70-iterators.md`「状态」：`Stepper`、`advance_quietly`（哪些内建迭代器可以不运行 JavaScript 地步进）、最后一条遗留说明。
- 规范的数组解构（`IteratorBindingInitialization`）：对 `[k, v]` 模式，规范会对条目数组取迭代器并逐个步进，`Array.prototype[Symbol.iterator]`
  与 `%ArrayIteratorPrototype%.next` 被改写时这一过程**可观察**。
- `builtins/map/map_iterator.rs`、`builtins/array/array_iterator.rs` 的 `entries` 种类。

## 要做的事

1. **识别**：编译期识别「`for…of` 的绑定是只有两个简单元素（无默认值、无剩余、无嵌套）的数组模式」；运行期在迭代器是未被改写的 `Map`/数组条目迭代器时走快路径。
2. **可观察性守卫**（写明为什么够）：`Array.prototype[Symbol.iterator]` 与 `%ArrayIteratorPrototype%.next` 仍是内建的（P70 的 `Stepper::of` 用过类似的判断）；
   条目数组本身对用户不可见（新建后只被解构），所以不建它不可观察。守卫不成立时照旧建数组、走规范路径。
3. **JIT**：快路径在编译代码里同样生效，或明确交给辅助并写明代价。

## 测试

- 差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致，并与 Node 一致）：三种写法；`[k]`、`[k, v, w]`、`[k, ...rest]`、`[k = 1, v]`、`[[a], v]` 等不走快路径的模式结果不变；
  循环中途增删 `Map` 条目；改写 `Array.prototype[Symbol.iterator]` 或 `%ArrayIteratorPrototype%.next`（循环前与循环中途）后行为与 Node 一致；
  `Map` 子类；`break`/`return`/抛错时迭代器的关闭；循环中途触发垃圾回收。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `language/statements/for-of`、`language/destructuring`、`language/expressions/assignment/dstr`、`built-ins/Map`、`built-ins/MapIteratorPrototype`、
  `built-ins/ArrayIteratorPrototype` 前后逐项 0 回退；除 `staging` 外解释与强制编译逐项 0 差异（计时类 `Atomics.notify` 例外照旧）。
- P60 套件不变慢（同一轮，中位差 ±3%）；`perf-js.md` 追加一行。

## 不做

一般的数组解构消除；`Object.entries` 的循环；`Set` 的 `entries`。

## 状态

分支 `electron/p82-map-entry-destructuring`，基线 `docs/electron-plan` `2cb3f702`；四个代码提交：`209f68c2` 实现（含 `boa/SU.md` 条目）、`ffe78dbe` 差分测试、
`8c17aca1` 修掉第一版让普通循环变慢的问题、`af780669` 跨 realm 测试。对比用另建的 detached worktree 按同配置（`--release --features jit`）构建的二进制。**目标达到**。

**成本拆分（实现前）**：每个元素 JIT 约 800 ns、解释约 880 ns，其中条目数组的创建加上按规范解构它（取 `Array.prototype[Symbol.iterator]`、建数组迭代器、两次 `next`、`return` 查找）
约占 85 %（JIT 约 690 ns，解释约 735 ns）。

**识别规则（编译期）**：`for (const|let [a, b] of …)`，模式恰好是两个普通名字，没有默认值、空位、rest、嵌套，且不是 `for await`。`var` 头和赋值头（`[a, b] of …`）
仍走规范路径，因为它们在两个元素之间可能执行 setter。命中时 bytecompiler 发出 `IteratorEntry`。

**运行期守卫**（`builtins/iterable/entry_destructuring.rs` 的 `entry_destructuring_is_unobservable`，每个元素都重读）：
`%Array.prototype%[@@iterator]` 是自有数据属性且等于 `%Array.prototype.values%`；`%ArrayIteratorPrototype%.next` 是本 realm 的内置（`Stepper::of`）；
`%ArrayIteratorPrototype%`、`%IteratorPrototype%`、`%Object.prototype%` 上都没有 `return`，且这条原型链没被改过。
**为什么足够**：条目数组是新建的、没有别人持有，它的自有属性只有 `0`、`1`、`length`。规范的解构要碰到用户代码，只能经过上面这几次查找；
它们全落在未改动的内置上时，跳过数组不可观察。迭代器本身由 P70 的 `Stepper` 判定：只有内置 Map/数组迭代器、`next` 属于当前 realm 时才产生"条目"步，
否则照常建数组。所以另一个 realm 的 Map 或数组总是走规范路径，守卫读当前 realm 的内置就是对的 realm（`entries_from_another_realm_match_node` 验证，
期望值是 Node 里每个 realm 一个 `vm` context 的输出）。

**JIT**：`IteratorEntry` 内联编译，快路径调 `jit_iterator_entry` 把键、值和 `fast = true` 写进三个寄存器；其他情况交给解释器的处理函数，它建数组，
编译出的规范解构路径接着处理。

**普通循环的回退及修复**：第一版让不解构的循环慢了约 3 ns（`map.keys()` 23.2 → 26.3，数组 for-of 20.5 → 23.8，JIT）。原因有两层：每一步都变成 40 字节的 `Step`，
经 `advance` → `finish_unboxed_step` → `LastResult` 一路复制；`jit_iterator_next` 每个元素把约 100 多字节的 `IteratorRecord` 从帧的迭代器栈弹出再压回（基线就如此）。
修复：`advance` 只把键作为普通 `JsValue` 返回，条目的值写进 `&mut Option<JsValue>` 出参，单值步仍是单值宽；`jit_iterator_next` 像解释器的 `IteratorNext` 一样原地推进记录；
`IteratorValue`/`jit_iterator_value` 通过 `IteratorRecord::unboxed_value()` 读常见情形，不构造 `Step`。只做前一半时没有改善，加上原地推进后回到基线。

**数字**（每个元素 ns，20,000 项，7 个进程交替取中位，负载 14–27 %；详见 `perf-js.md` 的 P82 一节）：

| 循环 | 之前 JIT | 之后 JIT | 之前解释 | 之后解释 |
|---|---|---|---|---|
| `for (const [k, v] of map)` | 677.2 | 106.7 | 785.0 | 140.9 |
| `for (const [k, v] of map.entries())` | 685.2 | 106.7 | 801.0 | 144.4 |
| `for (const [i, x] of arr.entries())` | 745.8 | 103.7 | 830.2 | 172.5 |
| 参照 `for (const e of map)` | 214.1 | 217.9 | 310.8 | 307.8 |
| 参照 `for (const k of map.keys())` | 23.6 | 23.4 | 48.2 | 53.3 |
| 参照 `for (const x of arr)` | 20.6 | 21.1 | 80.4 | 81.9 |

**偏差**：解释模式的 `map.keys()` 中位 48.2 → 53.3（最好值 47.6 → 49.3，约 +3.5 %），没有追原因；JIT 列持平。
P60 套件（负载 18–25 %，中位 ms，JIT / 解释）：`mapset` 22.0 / 25.3 → 14.8 / 15.9；`fib` 交替 9 轮 A/B 中位 228.0 对 230.3（之前两次 +3 % 没有复现）；其余行在噪声内
（`vdom`、`splitjoin`、`regex`、`json` 在相同二进制的两次运行之间就会差 3 % 以上）。

**测试**（`vm/jit/tests_entries.rs`，每个都在解释、`BOA_JIT_THRESHOLD=0` 强制编译、热起来走 OSR 三种模式下跑，期望值是 Node 22 的输出）：
三种循环及其绑定、不是两个普通名字的模式（默认值、rest、空位、嵌套、`var`、赋值头）、循环中修改 Map/数组、循环前和循环中改写数组协议
（`Array.prototype[Symbol.iterator]`、`%ArrayIteratorPrototype%.next`、各级 `return`）、Map 子类与 break/return/throw 时的关闭、循环中 GC 时条目存活、跨 realm 的 Map 与数组。

**验证**：`boa/` 内 `cargo test -p boa_engine` 默认 1245、`--features jit` 1315、`BOA_JIT_THRESHOLD=0 --features jit` 1315 全绿；
模糊测试 `BOA_JIT_FUZZ_COUNT=3000`（1 个测试，通过）；clippy `-D warnings`（`boa_engine --all-targets`，有无 `jit`）与根 `cargo clippy --workspace --all-targets -- -D warnings` 无告警；
根 `cargo test -p verve` 有无 `--features jit` 全绿。
test262（`E:	est262`）：for-of、destructuring、assignment/dstr、Map、MapIteratorPrototype、ArrayIteratorPrototype 六个目录，基线对最终、解释与强制编译各一遍，按测试名逐条比较 0 差异；
三个失败在基线与最终、两种模式下完全一样，是旧问题：`language/destructuring/binding/keyed-destructuring-property-reference-target-evaluation-order-with-bindings.js`、
`language/expressions/assignment/dstr/obj-rest-before-comma-invalid.js`、`language/statements/for-of/dstr/obj-rest-before-comma-invalid.js`。
全部 test262（除 `staging`）在最终代码上解释对强制编译：98,195 条结果逐条相同，0 差异。
