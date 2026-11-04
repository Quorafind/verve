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

未开始。
