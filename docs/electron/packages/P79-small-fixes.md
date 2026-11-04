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

完成。

**第 1 项（`src/worker.rs`、`tests/worker.rs`）**：`postMessage` 本来就在发出前 `pending += 1`，所以「消息已发、worker 还没开始处理」并不是漏等的窗口。真正的窗口有两个：

1. worker 处理完消息后 `pending` 立即减到 0，但它在处理中用 `setTimeout` 安排了回复；此时 `wait` 看到 `pending == 0`、堆里没有消息，返回 `false`。P69 的测试里「编译的几毫秒」恰好盖住了它。
2. `wait` 先 `gather`（收消息）再读 `pending`：worker 恰在两步之间「先发出回复、再把 `pending` 减到 0」，页面看到 `heard` 为空而 `pending == 0`，也返回 `false`。

修法：`wait` 判断「在工作」时，除了 `pending > 0`，还算上「在 `timeout` 之内到期的 worker 定时器」（`Progress::next_timer`，worker 每次 `flush` 都更新）；并且先读「在工作」、再 `gather`、再看 `heard`。worker 总是先发送、再减 `pending`/更新 `next_timer`，所以先读到「不在工作」的 worker 已经把要发的都发完了，随后的 `gather` 一定能收到。没有新的窗口：消息从发出起由 `pending` 覆盖，处理期间由 `pending` 覆盖，处理后留下的回复要么已在通道里（`gather` 收得到），要么由定时器覆盖，定时器触发期间 `next_timer` 仍是旧值（`flush` 之后才改），而 `flush` 在改它之前已发出消息。返回语义不变（有消息到 / 超时 / 没有在做的工作）；每次循环仍 `sleep(1ms)`。三处 20 ms `sleep` 已去掉。注意语义上的小变化：worker 有一个在 `timeout` 内到期的定时器时，`wait` 会等到它（之前立即返回 `false`）；定时器晚于 `timeout` 的不等。

新测试 `a_page_waiting_at_once_hears_the_answer`：发消息后立即 `wait_for_workers`，worker 直接回复和经 30 ms 定时器回复两种；修复前后一种失败（定时器那种，必现）。

`tests/worker.rs` 连续 20 次（`for i in $(seq 20); do cargo test -p verve --test worker; done`，`LOCALAPPDATA`/`XDG_*` 指向临时目录、`VERVE_CODE_CACHE=0`）：默认 20/20；`--features jit` 20/20；`VERVE_JIT_THRESHOLD=0 --features jit` 20/20。

**第 2 项（`regexp_string_iterator.rs`）**：`next` 的函数体拆成 `step`，`next` 在 `step` 返回 `Err`（`exec`、`ToString(match[0])`、`lastIndex` 读写任一处抛错）时置 `completed`，之后的 `next()` 返回 `{ value: undefined, done: true }`。四个抛错点各一个测试（`boa/core/engine/src/builtins/regexp/tests.rs`），修复前四个全部失败（抛错后的每次 `next` 都再抛一次），修复后通过。`boa/SU.md` 已记一条。

**与 Node 22 的偏差（需总控确认）**：Node 22.21.1（V8）并不在抛错后结束迭代器：`exec` 第二次抛错后，下一次 `next()` 照常返回下一个匹配（`threw | {"value":["a"],"done":false}`），`ToString` 抛错同理；`lastIndex` 读/写抛错时（空匹配）每次 `next()` 都重新抛。这是 V8 把它实现为内建函数而不是生成器，与规范（`CreateIteratorFromClosure`，抛错即 completed）不符。任务书写的目标是 `done: true`，所以按规范与任务书做，没有照搬 V8；测试注释里写明了。

test262：`built-ins/RegExpStringIteratorPrototype`（17）、`built-ins/RegExp/prototype/Symbol.matchAll`（26）、`built-ins/String/prototype/matchAll`（25）修复前后都是全部通过，0 回退（test262 没有覆盖抛错后继续 `next` 的用例）。

没做：无。
