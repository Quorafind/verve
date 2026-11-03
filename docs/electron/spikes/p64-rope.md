# P64 方案：让字符串 `+=` 线性化（rope / cons string）

日期 2026-11-03。只是方案，**没有实现**。依据是 P64 第一阶段的测量（`docs/electron/packages/P64-strings-regex-json.md`
「每一项慢在哪里」第 3 项）与对 `boa/core/string`、`boa/core/engine/src/vm` 的阅读；
标「按布局推算」的字节数是由 `#[repr(C)]` 字段相加得到的，没有跑 `size_of` 探针。

## 要解决的问题

`s += 'ab' + i` 重复 N 次：

| 追加次数 | 5,000 | 20,000 | 80,000 | 320,000 |
|---|---|---|---|---|
| Verve（解释器，ms） | 2.2 | 21.7 | 749.3 | 67,238 |
| Node（ms） | 0.4 | 0.6 | 5.0 | 26.9 |

20k→80k 是 34.5×（输入 4 倍），80k→320k 是 90×，是 O(n²)：每一步 `JsValue::add`
（`boa/core/engine/src/value/operations.rs:27`，`js_string!(&x, &y)`）经 `JsString::concat` →
`concat_array`（`boa/core/string/src/lib.rs:647`）新分配一个等于总长的 `SequenceString` 并**复制全部已有内容**。
`concat_array` 一次性分配本身已是最优，问题只在「每一步都调它」。

## 现状：与表示有关的事实

- `JsString` 是一个指针（`static_assertions::assert_eq_size!(JsString, *const ())`），指向以
  `JsStringVTable` 开头的结构；`JsStringKind` 有四种：`Latin1Sequence`、`Utf16Sequence`、`Slice`、`Static`
  （`lib.rs:103`）。
- `JsStringVTable`（`vtable/mod.rs`）：5 个函数指针（`clone`、`drop`、`as_str`、`code_points`、`refcount`）
  + `len: usize` + `kind: u8`，按布局推算 **56 字节**；`SequenceString` 再加 `refcount: Cell<usize>`，
  即**每个字符串 64 字节头**，数据紧跟其后。`SliceString`：56 + `owned: JsString` 8 + `inner: JsStr` 16
  + `refcount` 8 = **88 字节**，另外还持有被切的整个字符串。
- 引用计数是 `Cell<usize>`（非原子）；`JsString` 的 `Trace` 是 `empty_trace!`（`gc/src/trace.rs:569`）。
  所以**字符串不在 GC 里**：它们靠引用计数释放，不存在环（字符串不可变、只指向更早建成的串），
  GC 不需要知道 rope 节点。
- 读取内容的唯一入口是 `JsString::as_str(&self) -> JsStr<'_>`（`lib.rs:632`，走 vtable），`code_points`、
  `iter`、`code_unit_at`、`Hash`/`Eq`/`Ord`、`Display` 都经由它。`len` 是 vtable 里的字段，**不读内容**。
- 引擎内 `.as_str()` 调用 72 处、32 个文件（`boa/core/engine/src`）；全仓库提到 `JsStr` 或 `as_str()`
  的文件约 156 个。要依赖「串是一段连续切片」的代码，几乎都从 `as_str()` 取切片。
- JIT 不处理字符串 `+`：`Instruction::Add` 内联路径只含数值（`vm/jit/compiler.rs` 的 `arithmetic`），
  不是数值时落到 slow path，由 `jit_step`（`vm/jit/runtime.rs:117`）调用**解释器同一个 `Add` 处理函数**，
  最终也是 `JsValue::add`。因此**字符串 `+` 只有一处实现**，JIT 没有第二份要改（见下「JIT」）。
- 模板串 / 多段拼接走 `vm/opcode/concat/mod.rs:24` 的 `concat_array`（一次拼 N 段，已是一次分配）。

## 表示

新增第五种 `JsStringKind::Rope`（`vtable/rope.rs`），`#[repr(C)]`：

```
RopeString {
    vtable: JsStringVTable,        // 56  len = left.len + right.len, kind = Rope
    refcount: Cell<usize>,         //  8
    state: UnsafeCell<RopeState>,  // 16+  Unflattened { left: JsString, right: JsString } | Flat(JsString)
    latin1: bool,                  //  8  （含填充）两侧都是 Latin-1 时为真，展平时据此选宽度
}
```

按布局推算约 **96 字节**一个节点（比 `SequenceString` 头多 32 字节），两个子串由节点持有一份引用。
`len` 在构造时由两侧相加（`checked_add`，超 `usize`/引擎上限仍按 `alloc_overflow()` 处理），所以
`s.length`、JIT 的 `GetLengthProperty`（`vm/jit/helpers.rs:313`）与 `string.len()` 一律**不展平**。

- **展平后的缓存**：`as_str(&self)` 只拿到 `&self`，所以展平结果必须放在节点内的 `UnsafeCell` 里，
  第一次调用时写入、之后只读。展平完成后把 `Unflattened` 换成 `Flat(JsString)`，**子串引用随之释放**
  （内存回落到一个平串加一个 96 字节的壳）。切片借用的生命周期仍是 `&self`，成立的理由是：`Flat` 一旦
  写入就不再改动，只要 `&self` 活着，它指向的平串就不会被释放。
- 单线程假设与现有一致（`Cell` 引用计数、`JsString: !Sync`）；`UnsafeCell` 不引入新的线程约束。
- `SliceString`（`slice`/`substring`）建立在 `owned.as_str()` 之上：对 rope 取切片会先展平，
  切片持有的是**展平后的平串**（`owned` 取 `Flat` 里的那个），不是 rope 节点。

## 何时建 rope，何时仍直接拷贝

只有 `JsValue::add` 的「两侧都是字符串」与「一侧转字符串」两条路径、以及 `+=` 对应的 `Add` 处理函数，
改成调用新的 `JsString::concat_lazy(x, y)`。其余 `concat_array` 的调用点（模板串、`repeat`、`replace`
的累积等）**保持一次性拷贝**——它们本来就是线性的，且调用方往往立刻读内容。

`concat_lazy(x, y)` 的决策：

1. 任一侧为空串：返回另一侧（零成本，现在也应如此）。
2. `x.len() + y.len() < ROPE_MIN`（建议 **256 个码元**）：直接 `concat_array`。理由：一个 rope 节点 96 字节
   加一次分配（约 20–50 ns），而拷贝 256 个码元只是几十 ns；`'a' + i`、`'ab' + i` 这类短串继续是
   一次 `SequenceString`（64 + n 字节），**不会更差**。阈值在实现时用 `concat` 探针与 `template`/
   `splitjoin` 确认，写进实现任务的验收。
3. 否则：新建 `RopeString { left: x, right: y }`。

对 `s += 'ab' + i` 的内层 `'ab' + i`：总长短，仍直接拷贝；外层 `s += ...`：左侧一长，建 rope。
最坏情形是「右侧极短、追加很多」：每追加 7 个码元多 96 字节，**内存放大约 14×**（Latin-1）直到展平
（320k 次约 31 MB 节点，对照今天的 O(n²) 拷贝可以接受，Node 的 `ConsString` 在指针压缩下是 20 字节）。
缓解：**叶子合并**——若右侧短（< 32 码元）且左侧是**未展平 rope 的右子串又是个短平串**，把新片段并进那个
右叶（重新分配一小段），使节点数与片段数不同比。这是第二阶段的优化，第一版不做。

## 展平策略（必须先展平的操作）

规则：**任何拿 `JsStr` 的地方都展平**，因为 `as_str()` 内部就是展平。展平发生在这些操作上：

- 索引与 `charCodeAt` / `at` / `codePointAt` / `s[i]`（`GetPropertyByValue` 的字符串分支）；
- 所有 `String.prototype` 方法（`indexOf`、`slice`、`split`、`replace`、`match`、`trim`……）与 `RegExp` 匹配；
- 作为属性键（`PropertyKey` 的字符串要展平并 intern/哈希）；`Hash`、`Eq`、`Ord`；
- 比较 `===`：先比 `len`，长度相同再展平；
- `Display`/`to_std_string_*`、`console.log`、文件与网络写出（FFI 边界）、`JSON.stringify`；
- `JsString::concat_lazy` 的**输入**不展平（这是线性化的要点）：rope 可以做 rope 的子节点。

不需要展平：`length`、`typeof`、真值判断（`len == 0`）、`concat_lazy` 的子节点引用、`clone`/`drop`。

**展平算法**（栈安全是硬要求，`s += x` 320k 次会得到深 320k 的左斜链）：

1. 分配目标缓冲，长度 `len`，宽度由 `latin1` 标志定（两侧都 Latin-1 才是 Latin-1，否则 UTF-16）。
2. **从右向左**填：显式 `Vec<&JsString>` 作栈（堆分配，不用 Rust 递归），弹出节点，若是未展平的 rope 就先压左再压右（保证右先写、位置从尾往前减），
   若是叶子（平串、`Slice`、`Static` 或**已展平**的 rope）就整段 `copy_nonoverlapping`（Latin-1 → UTF-16 逐字节拓宽）。
   已展平的子 rope 直接取其 `Flat`，不再下探。
3. 把结果写入根节点的 `Flat`，释放子串引用。成本 O(总长 + 节点数)。

左斜链的栈深度为 1（每次弹一个节点，先压右子叶再压左子 rope），一般形状的栈上界是树高，
最坏是节点数，仍在堆上。

**展平后再追加会重新二次化**：`s += x; s.charCodeAt(0)` 每轮都展平，等同今天的 O(n²)（V8 同样如此），
**不会比今天更差**（今天也是每步复制），只多一个 96 字节的壳。这点要在文档与测试里写明，不在本方案内「修」。

## 与 GC、释放的关系

- 字符串不在 GC 里（见上），rope 节点只是又一种引用计数对象：**`Trace` 不用改**。
- **递归释放是真风险**：320k 深的左斜链，朴素的 `drop` 逐层递归会爆 Rust 栈（Boa 的 `drop` 经 vtable 的
  `fn(NonNull<JsStringVTable>)`）。`rope_drop` 必须**迭代**：引用计数降到 0 时把子串移进一个本地 `Vec`，
  循环「弹一个、递减它的计数、若为 0 且是 rope 就把它的子串也压进去」，其余直接释放。要有测试：
  建 100 万节点的链再丢掉，不得栈溢出。
- 展平后释放子串引用，也就是说一个被读过的长串不会长期占着 N 个 96 字节节点。
- `refcount` 函数仍返回 `Some(n)`；`JsString::ptr_eq`（同一分配）语义不变。

## 解释器与 JIT

- **解释器**：`vm/opcode/binary_ops` 的 `Add` 处理函数只调 `JsValue::add`，改动集中在
  `value/operations.rs:27`、`:36`、`:37` 那三处 `js_string!(&x, &y)`（改成 `JsString::concat_lazy`），
  `+=` 的 `Add` 操作码不变。
- **JIT**：字符串 `+` 没有内联路径，slow path 经 `jit_step::<ADD>` 调同一个处理函数，**不需要 JIT 的改动**，
  自动得到 rope。已核对 `vm/jit/` 中字符串的全部用法：只有 `helpers.rs:313` 的 `GetLengthProperty` 读 `string.len()`，
  而 `len` 是 vtable 字段、不展平，所以 JIT 里**没有**读字符串内容的代码要改。
- P66（JIT 字符串路径）的关系：P66 若要给字符串 `+` 加内联快路径，应当调用 `concat_lazy` 的 Rust 帮助函数，
  不要在机器码里自己拷贝；这个方案落地前后 P66 都能做，但**两者不要同时改 `JsValue::add`**，让 rope 先合。

## 假设「平切片」的代码会怎样

因为展平藏在 `as_str()` 里，**调用方的源码不用改**。会出问题的只有三类：

1. **缓存了 `JsStr` 或 `as_ptr()` 后又期望 `JsString` 内容就是它**：切片借用生命周期是 `&self`，在 rope 上
   同样成立（展平结果在节点里不再变），没有新问题。
2. **绕开 `as_str()` 直接读内存的 unsafe 代码**：`SequenceString::data`、`JsString::from_ptr`、
   `display.rs` 的 `JsStringDebugInfo`（按 `JsStringKind` 的 `match`，`display.rs:94` 要加 `Rope` 分支）、
   `tests.rs`。这些都在 `boa/core/string` 内，约 4 处 `match kind`/`kind ==`，由编译器的穷尽检查指出。
3. **以「串很便宜、读内容也很便宜」为前提的算法**：`as_str()` 现在可能一次分配加 O(n) 拷贝（第一次）。
   热点是循环里对同一个 rope 反复调 `as_str()` 的代码——第一次之后是缓存命中（读一次 `UnsafeCell`
   的判别），不会重复展平，但**多出一个分支**；这是 `charCodeAt` 之类热路径要用基准确认的。

## 分阶段实现

1. **`boa/core/string`**：加 `JsStringKind::Rope`、`vtable/rope.rs`、`JsString::concat_lazy`；`as_str` 的
   展平（迭代）与迭代 `drop`；`display.rs` 的 `match`；单元测试：Latin-1/UTF-16 混合、孤立代理项边界、
   空串、深 100 万的链展平与释放、展平后再 `clone`/`slice`、`len` 不展平、`ptr_eq`。
   （约 1.5–2 天。）
2. **`JsValue::add` 接入**（`value/operations.rs` 三处），阈值常量；`Context` 级测试：`+=` 循环 20 万次、
   rope 做属性键、rope 比较、rope 经 `String.prototype` 全部方法、`JSON.stringify`、正则。（约 1 天。）
3. **验证**：`cargo test -p boa_string`、`cargo test -p boa_engine`（含 `BOA_JIT_THRESHOLD=0`）、test262
   `built-ins/String`、`built-ins/RegExp`、`built-ins/JSON`、`language/expressions/addition`、
   `language/expressions/compound-assignment` 前后逐项；`concat` 探针（20k/80k/320k）应回到 linear，
   `concat` 吞吐从 40× 掉到约 ≤ 5×（P64 第一阶段的估计）。（约 1 天。）
4. **可选**：叶子合并（右侧短时并入右叶，削掉 14× 的内存放大）、对 `Array.prototype.join`/`concat_array`
   的输入是 rope 时直接遍历叶子而不先展平。（约 1 天，要有收益证据才做。）

合计约 **4–5 个工作日**（单人），第 1–3 阶段是最小可交付。

## 风险

| 风险 | 程度 | 缓解 |
|---|---|---|
| 递归释放/展平爆栈 | 高（320k 深是本需求的常态） | 迭代 `drop` 与展平，加深度测试 |
| `UnsafeCell` 展平的 aliasing/生命周期（unsafe） | 高 | 只在 `Unflattened → Flat` 一次转换，之后只读；`boa_string` 已有 `deny(undocumented_unsafe_blocks)`，每处写 Safety；跑 miri 的 `boa_string` 单元测试（若 CI 有） |
| 内存放大（短右侧、很多次追加） | 中 | 阈值 + 第二阶段叶子合并 |
| 热路径多一个分支（`as_str` 上的 `Flat` 判断） | 中 | `charCodeAt`、`split`、`tokenize` 基准前后对比，没有收益不合 |
| 展平后又追加仍 O(n²) | 低（不比现状差） | 文档注明；不修 |
| 与 P65/P66 并行改 `JsValue::add`/opcode 冲突 | 低 | 本方案只动 `operations.rs` 三行与 `boa/core/string`，让后合并者 rebase |

## 决定点（给总控）

1. 是否做，以及放在 P64 之内还是单列一个包。方案与 P64 已完成的三项**互不依赖**，可以单列；
   它改的是 `boa/core/string` 的核心表示，建议单独一个包、单独评审。
2. 阈值（256 码元）与「第一版不做叶子合并」是否接受。
3. 目标：`concat` 探针回 linear，`concat` 吞吐 ≤ 5×；其余字符串基准不得回退。
