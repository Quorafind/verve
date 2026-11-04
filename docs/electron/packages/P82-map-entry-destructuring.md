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

未开始。
