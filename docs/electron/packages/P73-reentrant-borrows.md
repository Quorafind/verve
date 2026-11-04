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

未开始。
