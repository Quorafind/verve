# P72 JIT：类型化数组 `length` 的站点缓存

**依赖**：P65、P66（均已合并）  **规模**：S  **修改**：`boa/core/engine/src/builtins/typed_array/object.rs`（只拆分
`typed_array_length_shortcut`，单独一个移动 commit）、`vm/jit/`、`docs/jit.md`、`boa/SU.md`、对应测试

## 目标

P66 之后 `typed` 的 JIT 列是 608 ms，目标 450。剩下的大头是 `f64.length`：一轮 140 ns 里占 65 ns。它每次都走 P65 的
`typed_array_length_shortcut`，沿原型链确认没有被遮蔽或改写的 `length` 访问器，然后才读内部槽。原型链检查的结果在一个
编译站点上几乎从不变化，可以缓存；读内部槽（长度、是否分离、可变长缓冲的当前长度）每次都要做。

**目标**：`f64.length` 编译后 ≤ 15 ns；P60 套件 `typed` JIT 列 ≤ 450 ms。其余各项不变慢。

## 背景（先读）

- `P66-jit-coverage.md`「状态」第 2 步「Left out」中「Typed-array `length` site cache」一条，以及微探针数字（`docs/jit.md`「Results (phase 2.5)」）。
- `builtins/typed_array/object.rs` 的 `typed_array_length_shortcut`：先查对象自身没有 `length`，再沿原型链走普通对象，找到访问器并确认是内建 getter。
- `docs/jit.md`「Property caches and elements」：现有属性站点缓存怎样按形状校验、怎样失效。

## 要做的事

1. **移动 commit（不改行为）**：把 `typed_array_length_shortcut` 拆成两半：**守卫**（「这个对象的 `length` 就是内建 getter」）与**读取**（由内部槽算出长度，
   含分离与可变长缓冲）。解释器照旧两半连着调，P65 的测试原样通过。
2. **站点缓存**：`GetLengthProperty` 遇到类型化数组时，按 `docs/jit.md` 现有的属性缓存方式缓存守卫的结论，用能证明结论仍成立的形状
   （接收者的形状、链上各原型的形状或等价的失效机制）校验；命中时只做读取。由实现者设计校验条件，并在 `docs/jit.md` 写明它为什么够。
3. 若测量显示类型化数组元素读写（P66 后读 23.3、写 26.2 ns）在同一循环里也值得同样的处理，写出数字，交总控决定，**本包不做**。

## 测试

差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致），覆盖：九种元素类型；分离的缓冲（循环中途分离）；可变长缓冲在循环中途缩小与增长；
实例上定义自有 `length`；`%TypedArray%.prototype` 的 `length` 被改成别的访问器或数据属性（在编译之前与之后）；中间原型被插入带 `length` 的对象；
`Object.setPrototypeOf` 改接收者的原型；同一站点交替出现类型化数组、普通数组与字符串。

## 验收

- `f64.length` 微探针与 P60 套件 `typed` 的前后对比（同一轮，5 个进程），`perf-js.md` 追加一行。
- `docs/jit.md`「Testing」的整套测试全部通过（同 P66 的验收），test262 `built-ins/TypedArray`、`built-ins/TypedArrayConstructors`
  解释与强制编译逐项 0 差异。
- 体积增量写进 `size.md`。

## 不做

元素读写的进一步内联（见第 3 项）；解释器侧的 `length` 缓存；其他内建访问器。

## 状态

未开始。
