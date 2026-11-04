# P77 借用重入排查：`vm/` 与 `object/`

**依赖**：P73、P75（均已合并）  **规模**：S–M  **修改**：`boa/core/engine/src/vm/`、`boa/core/engine/src/object/`、`boa/core/engine/src/environments/`
（只限修复可达的借用重入），对应测试、`boa/SU.md`

## 目标

P73 扫完了 `builtins/`：持有对象（`GcRefCell`）借用期间运行用户代码，用户代码再要可变借用同一对象，就会 `BorrowError`/`BorrowMutError` panic。
修了数组迭代器、`matchAll` 迭代器和 `Intl.Collator` 三处。它的「状态」最后写着：`vm/`、`object/` 没有看。
P75 在内联缓存里又发现一类相同性质的问题（命中时借着接收者去读原型）。本包用同样的方法把 `vm/`、`object/`、`environments/` 扫一遍，修掉可达的。

## 背景（先读）

- `P73-reentrant-borrows.md`「状态」：扫描方法、判断规则（只有会被可变借用的类型才可能出事）、清单格式、Node 对照的做法。
- `P75-prototype-inline-cache.md`「状态」：`try_borrow` 失败按未命中处理的写法。
- `object/internal_methods/`（普通对象与 Proxy 的内部方法、`get`/`set` 上的 getter/setter 调用）、`object/jsobject.rs`、`object/operations.rs`；
  `vm/opcode/` 里在借用期间调用 `call`/`construct`/`get`/`set`/转换的处理函数；`vm/jit/helpers.rs`；`environments/`（`with` 对象环境、全局对象环境）。

## 要做的事

1. **扫描**：读上述目录下所有 `borrow`/`borrow_mut`/`try_borrow*`/`downcast_*` 的守卫存活期，看其中有没有可能运行用户代码的调用（getter/setter、Proxy 陷阱、
   `valueOf`/`toString`/`Symbol.toPrimitive`、回调、终结器）。每一处写进「状态」的清单：可达（附复现）或不可达（附理由）。
2. **修复**：可达的每一处，先写在修复前 panic 的测试，再改成「读出所需的值、放开守卫、再运行用户代码」；需要回写时重新借用并重新校验。
   语义按 Node 22 的输出（与规范不一致时照 Node，写明）。
3. **JIT**：辅助函数里的借用都应当是 `try_borrow`，失败走慢路径；不是的列出并修掉。

## 测试

- 每个可达点一个复现测试（修复前 panic，修复后通过），并与 Node 输出一致；解释、`BOA_JIT_THRESHOLD=0` 两种都跑。
- 「状态」贴修复前的 panic 信息。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 中与修改相关的目录（至少 `built-ins/Proxy`、`built-ins/Object`、`language/statements/with`、`language/expressions/assignment`）前后逐项 0 回退、0 panic。
- 碰到热路径（`vm/opcode/get`、`set`、`call`）的修改，给出 P60 `oo`、`vdom`、`closure` 同一轮前后对比，中位差 ±3% 内。

## 不做

`builtins/` 的再次扫描（P73 已做）；与借用无关的正确性问题（发现了写进「状态」，不修）。

## 状态

未开始。
