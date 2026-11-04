# P79 两处小修：`worker::wait` 与 `matchAll` 迭代器的结束状态

**依赖**：P69、P73（均已合并）  **规模**：S  **修改**：`src/worker.rs`、`tests/worker.rs`、`boa/core/engine/src/builtins/regexp/regexp_string_iterator.rs`、
对应测试、`boa/SU.md`

## 目标

两个评审时发现、都不大的问题，一起修掉。

### 1. `Runtime::wait_for_workers` 在 worker 还没开始处理消息时立即返回

`src/worker.rs` 的 `wait` 只在「有 worker 的 `pending` 大于 0」时继续等。消息已经发出、worker 线程还没开始处理它时，`pending` 仍是 0，
`wait` 立即返回 `false`。P69 的 `tests/worker.rs` 因此在强制编译下变得确定性失败：之前是编译的几毫秒恰好给了 worker 时间。
P69 在三个等待循环里各加了 20 ms 的 `sleep` 作为权宜之计。本包修 `wait` 本身（例如发消息时就计入 `pending`，或等价的做法），然后**去掉**那三处 `sleep`。

### 2. `RegExpStringIterator` 在 `exec` 抛错后没有结束

P73「状态」记下：`matchAll` 返回的迭代器在用户的 `exec`（或 `lastIndex` 的 getter/setter、`ToString`）抛错时不置 `completed`。
规范里这个迭代器由生成器式的闭包实现，抛错即结束，之后的 `next()` 返回 `{ value: undefined, done: true }`。按 Node 22 的实际输出对照后修。

## 要做的事

1. 第 1 项：读 `src/worker.rs` 里 `pending` 的增减点与 `post_message` 路径，找出 `wait` 漏等的窗口；修复并写明为什么不再有窗口。
   `wait` 返回时机的语义（有消息到、超时、没有在做的工作）不变；不得引入忙等（每次循环仍然要睡）。
2. 第 2 项：在 `exec` 抛错、`ToString(match[0])` 抛错、`lastIndex` 读写抛错这几处都结束迭代器；与 Node 输出逐字对照。

## 测试

- 第 1 项：去掉 `sleep` 后，`tests/worker.rs` 在默认、`--features jit`、`VERVE_JIT_THRESHOLD=0 --features jit` 下各连续跑 20 次全过（写出命令与结果）；
  再加一个测试：发出消息后立即 `wait_for_workers`，必须等到回复（在修复前失败）。
- 第 2 项：每种抛错点一个测试，抛错后的 `next()` 与 Node 一致；修复前失败。

## 验收

- 根 `cargo test -p verve`（有无 `jit`）、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`）、clippy `-D warnings`、`cargo fmt`。
- test262 `built-ins/RegExpStringIteratorPrototype`、`built-ins/RegExp/prototype/Symbol.matchAll`、`built-ins/String/prototype/matchAll` 前后逐项 0 回退。

## 不做

worker 的其他调度改进；`RegExp` 的其他行为。

## 状态

未开始。
