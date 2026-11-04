# P73 内建对象在调用用户代码时仍借用着自身

**依赖**：P70（已合并）  **规模**：S  **修改**：`boa/core/engine/src/builtins/` 中查到的处理函数、对应测试、`boa/SU.md`

## 目标

P70 评审时发现：`ArrayIterator::advance`（P70 之前是 `ArrayIterator::next`，行为相同）在整个推进过程中持有迭代器的
`downcast_mut` 借用，期间调用了可能运行用户代码的 `length_of_array_like` 与 `get`。迭代对象是 Proxy 时，陷阱里再调用同一个
迭代器的 `next` 就会 panic：

```js
const a = [1, 2]; let it, hit = false;
const p = new Proxy(a, { get(t, k, r) {
  if (k === 'length' && !hit) { hit = true; it.next(); }
  return Reflect.get(t, k, r);
} });
it = Array.prototype.values.call(p);
it.next();
// Node：正常返回；Verve（05f26157 与 96a6de96 都是）：
// panicked at boa\core\engine\src\vm\opcode\get\property.rs:56:34: Object already mutably borrowed: BorrowError
```

这是上游 Boa 的写法，同一模式可能不止这一处。本包**修掉这一处，并把 `builtins/` 里同一类问题全部找出来修掉**：
持有某个对象的 `borrow()`/`borrow_mut()`/`downcast_ref()`/`downcast_mut()` 守卫，同时调用可能运行用户代码的操作
（`get`、`set`、`call`、`construct`、`to_primitive`/`to_string`/`to_number` 等转换、`length_of_array_like`、迭代器协议、Proxy 的任何内部方法）。

## 背景（先读）

- `boa/core/engine/src/builtins/array/array_iterator.rs` 的 `advance`；`builtins/iterable/mod.rs` 里 P70 的 `Stepper`
  （`advance_quietly` 的「不运行 JavaScript」判断依赖各迭代器的写法，修改后要保持成立）。
- `boa/core/gc` 的 `GcRefCell` 借用规则：守卫存活期间再借用同一对象会 panic。

## 要做的事

1. **扫描**：在 `builtins/` 里找出所有「守卫存活期间调用了可能运行用户代码的操作」的地方。方法由实现者定（阅读 + `grep`
   `downcast_mut`/`borrow_mut` 与带 `context` 的调用；能写成小脚本就写），把清单（文件、函数、触发用户代码的调用、能否构造出 panic）写进「状态」。
   只列能证明的：每一处都给出触发它的 JS（能触发 panic 的进测试），或说明为什么不可达。
2. **修复**：照规范的步骤顺序，先在借用里读出需要的状态，放开守卫，再调用用户代码，之后重新借用写回。
   写回时注意规范语义：用户代码可能已经改了状态（例如重入的 `next` 推进了迭代器、让它完成），按规范该看到的值写回，不要覆盖。
   以 `ArrayIterator` 为例，规范在读 `length` 与元素之前就已经确定了 `index`，重入调用看到的是推进后的状态；对照 Node 的输出写测试。
3. **测试**：每一处可触发的都有一个原来会 panic、修复后与 Node 输出一致的测试（Node 的输出贴进「状态」）。
   P70 的 `tests_iterators.rs` 差分测试照旧通过。

## 验收

- 「状态」里的清单与每一处的处理；新测试修复前 panic、修复后通过。
- `boa/` 内 `cargo test -p boa_engine`（默认与 `BOA_JIT_THRESHOLD=0 --features jit`）、clippy `-D warnings`（有无 `jit`）、`cargo fmt`；
  根 `cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262：涉及的内建目录前后逐项 0 回退。
- P60 套件 `vdom`、`mapset`、`sort` 不变慢（同一轮比较；P70 的迭代器快路径若因修复多了开销，给出数字）。

## 不做

`builtins/` 以外（`vm/`、`object/`）的同类问题：发现了列进「状态」，不修。上游 Boa 的其他已知偏差。

## 状态

**完成**。分支 `electron/p73-reentrant-borrows`，从 `docs/electron-plan` `c91b2938` 切出，三个修复提交（各带 `boa/SU.md` 条目与测试）：
`fix(iterators)`（数组迭代器）、`fix(regexp)`（`matchAll` 迭代器）、`fix(intl)`（`Intl.Collator` 的 `compare`），外加本文档提交。只改了 `builtins/` 与 `boa/SU.md`。

### 扫描清单

方法：读 `builtins/` 下所有 `downcast_mut`/`downcast_ref`/`borrow_mut`/`borrow`/`try_borrow`（含 `let` 绑定与 `if let` 的存活期）与 `as_buffer`/`as_buffer_mut`，对每一处看守卫存活期间有没有 `get`/`set`/`call`/转换/`length_of_array_like`/Proxy 内部方法。
判断规则：持有共享借用只会在**别处要求同一对象的可变借用**时 panic，所以只有「可变借用的类型」才可能出事；`temporal/` 的对象从不可变借用，整个目录不可达。

可达（已修，测试在修复前 panic、修复后通过）：

| 文件 | 函数 | 守卫存活期间的用户代码 | 测试 |
|---|---|---|---|
| `array/array_iterator.rs` | `ArrayIterator::advance` | `length_of_array_like`、`get`（Proxy 陷阱、getter） | `array_iterator_may_be_stepped_again_from_a_length_trap`、`..._from_an_element_trap`、`..._from_a_length_getter`、`array_iterator_may_be_drained_from_a_length_trap`、`array_iterator_may_be_stepped_past_the_end_from_an_element_trap` |
| `regexp/regexp_string_iterator.rs` | `RegExpStringIterator::next` | `RegExpExec`（用户的 `exec`）、`Get(match, "0")` 上的 `toString`、`lastIndex` 的 get/set | `regexp_string_iterator_may_be_stepped_again_from_exec`、`..._steps_in_order_when_exec_steps_it`、`..._completed_by_a_nested_step` |
| `intl/collator/mod.rs` | `compare` 返回的函数 | 持有 `downcast_ref::<Collator>()` 时对两个参数 `ToString`；陷阱里读 `compare` 取 `downcast_mut`，`BorrowMutError` | `compare_may_read_compare_while_converting_its_arguments`（`intl_bundled` 下，修复前 panic 已验证） |

不可达（或不会 panic）：

- `object/for_in_iterator.rs` `ForInIterator::next`：持守卫调用 `own_property_keys`/`get_own_property`（Proxy 陷阱）。迭代器对象只在 VM 栈上，JS 拿不到，陷阱里无法重入。
- `string/string_iterator.rs`、`map/map_iterator.rs`、`set/set_iterator.rs`：守卫内只有自身状态、`String::substring` 与建数组，不运行用户代码（P70 `advance_quietly` 的「不运行 JavaScript」判断因此仍成立；`advance_dense` 没动）。
- `map/mod.rs`、`set/mod.rs`（`forEach`、`for_each_native`、`getOrInsert*`、集合运算）、`weak_map`、`weak_set`：每次读都在块内取出，回调前放开；`MapLock` 不是借用。
- `generator/mod.rs`、`async_generator/mod.rs`、`iterable/iterator_helper`、`iterable/wrap_for_valid_iterator.rs`：调用用户代码前 `drop` 守卫或把协程取走（重入得到「already executing」的 `TypeError`）。
- `array/mod.rs`（`set_length`、`plain_dense_array`/`push_dense`/`fill_dense`、`shift` 快路径、`array_exotic_define_own_property`）：用户代码前已 `drop`；`plain_dense_array` 收 `&Context`，不可能运行 JS。
- `typed_array/*`、`array_buffer/*`、`dataview`、`atomics`：`as_buffer`/`as_buffer_mut` 与 `ta.borrow()` 都限定在不调用转换的块里（转换在前、重新校验在后），`slice`/`set` 在循环调用 `get`/`set` 前 `drop`。
- `promise`、`finalization_registry`（`cleanup` 先换出回调再调用）、`proxy`（`revoke`）、`regexp/mod.rs`（`abstract_builtin_exec` 先克隆数据）、`error/*`、`function/*`、`json`、`date`、`intl/{date_time_format,number_format,segmenter}`、`builder.rs`：守卫内无用户代码。
- 手头没有 `Declined` 之外的 JIT 路径改动；`vm/`、`object/` 没有看。

### 重入语义（对照 Node 22.21.1）

Node 的数组迭代器：先读 `index`，再读 `length`，**之后**存 `index + 1`，再读元素。所以 `length` 陷阱里重入的 `next` 看到旧的 `index`，外层随后用自己读到的 `index` 覆盖；元素陷阱里重入的 `next` 看到的是推进后的 `index`。
重入的 `next` 在 `length` 陷阱里把迭代器耗尽时，外层仍照常返回自己的元素，并把迭代器恢复为未完成（Node 的第二个 `next` 返回 `[1,2]`）。任务书里「规范在读 length 之前就确定了 index、重入看到推进后的状态」与 Node 的输出不符（规范里生成器重入应当抛 `TypeError`），照 Node 的输出实现并写进测试。

每个复现用例 Node 与 Verve（release + `jit`，解释与 `VERVE_JIT_THRESHOLD=0` 各一遍）逐字相同：

| 用例 | Node 22.21.1 与 Verve 输出 |
|---|---|
| 任务书的例子（`length` 陷阱里 `it.next()`） | 正常返回，不 panic（修复前：`BorrowError`） |
| `ai1` `length` 陷阱里重入 | `{"value":1,"done":false} {"value":1,"done":false} {"value":2,"done":false} {"done":true}` |
| `ai2` 元素 `'0'` 陷阱里重入 | `{"value":20,"done":false} {"value":10,"done":false} {"value":30,"done":false} {"done":true}` |
| `ai3` `entries`，`length` 陷阱里耗尽 | `{"value":[0,1],"done":false} {"value":[1,2],"done":false} {"value":[2,3],"done":false} {"done":true} {"done":true} {"value":[0,1],"done":false} {"value":[1,2],"done":false}` |
| `ai4` 最后一个元素的陷阱里连续两次 | `{"value":1,"done":false} {"done":true} {"done":true} {"value":2,"done":false} {"done":true}` |
| `ai5` `length` getter 里重入 | `{"value":"a","done":false} {"value":"a","done":false} {"value":"b","done":false} {"done":true}` |
| `ai6`/`ai7` 数组迭代器上的迭代器 | `{"done":true} {"done":true} {"value":1,"done":false} undefined`；`{"done":true} {"value":[0,1],"done":false}`（修复前不 panic，作回归） |
| `rs1` `exec` 里 `it.next()` | `{"value":["a"],"done":false} \| {"value":["a"],"done":false}`（修复前 panic） |
| `rs2` | `inner 1 false \| outer 0 \| 2` |
| `rs3` | `false true true` |
| `co1` `Intl.Collator` | `-1 function`（修复前 `BorrowMutError`） |

脚本在 scratchpad `p73/`（`ai1.js`…`co1.js`）。

### 验收

- 新测试 11 个（数组迭代器 7、`matchAll` 3、Collator 1）；基线（`c91b2938`）上 8 个 panic（`ai6`/`ai7` 不 panic），Collator 在 `intl_bundled` 下修复前 panic、修复后通过。
- `boa/`：`cargo test -p boa_engine` 默认 1221 + 其他套件全过；`BOA_JIT_THRESHOLD=0 --features jit` 1272 全过；`--features intl_bundled -- collator` 通过。
  `cargo clippy -p boa_engine --all-targets -- -D warnings` 有、无 `--features jit` 都过；`cargo fmt`、根 `cargo fmt --all --check`、`bash scripts/style-check.sh` 每个提交都过。
- test262（`E:\test262`，release `boa_tester`，基线 `c91b2938` 对改动后，逐项 `compare`）：`built-ins/Array` 3082、`ArrayIteratorPrototype` 27、`RegExp` 1865（1 个既有失败）、`RegExpStringIteratorPrototype` 17、`Iterator` 420、`Proxy` 311、`language/statements/for-of` 740、`language/expressions/array` 52、`language/destructuring` 18：**全部 0 差异、0 panic**。`intl402` 没跑（tester 没有 `intl` 特性）。
- P60（release + `jit`，每格 7 个进程取中位数（最好），ms，基线与改动后在同一轮相邻测量；括号后为采样的整机负载 %）：

| 测试 | 基线 JIT | 之后 JIT | 基线解释 | 之后解释 | 负载 |
|---|---|---|---|---|---|
| `vdom` 第 1 轮 | 63.9 (52.0) | 52.3 (49.3) | 56.9 (54.9) | 57.5 (53.6) | 13–23 |
| `vdom` 第 2 轮 | 74.9 (59.7) | 57.1 (53.9) | 73.8 (68.6) | 56.4 (52.9) | 31–36 |
| `mapset` 第 1 轮 | 23.9 (21.8) | 23.6 (22.8) | 27.1 (23.3) | 25.9 (24.7) | 13–17 |
| `mapset` 第 2 轮 | 25.2 (22.8) | 30.4 (22.9) | 26.8 (23.4) | 31.0 (26.5) | 21–28 |
| `sort` 第 1 轮 | 2510 (2279) | 2438 (2356) | 2870 (2792) | 2867 (2780) | 12–33 |
| `sort` 第 2 轮 | 2706 (2541) | 2347 (2297) | 2821 (2703) | 2761 (2677) | 19–52 |

  没有变慢：只有 `mapset` 第 2 轮「之后」的中位数高（最好值 22.9 与基线 22.8 相同，第 1 轮相反），是噪声。P70 的数组迭代器快路径（`advance_dense`、`advance_quietly`）没动，解释器路径多了一次 `Gc` 克隆与两次借用，不可测。

### 没做 / 已知问题

- 没修 `builtins/` 之外的同类问题（没扫 `vm/`、`object/`）。
- `RegExpStringIterator` 在 `exec` 抛错时不置 `completed`（生成器规范里会结束），是上游旧行为，没动。
- `boa/target` 在工作树里是真实目录（`boa/` 不走 `target` 联接），已被 gitignore，没有提交。
