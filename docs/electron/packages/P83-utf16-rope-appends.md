# P83 rope 的 UTF-16 追加也走缓冲

**依赖**：P74（已合并）  **规模**：S  **修改**：`boa/core/string/src/`（`vtable/rope.rs`、`lib.rs`）、对应测试、`boa/SU.md`、`docs/electron/perf-js.md`、`docs/electron/size.md`

## 目标

P74 给 rope 加了共享追加缓冲（`append_short`，`MAX_BUFFERED_LEN` 64），但只覆盖 Latin-1：当左侧或右侧含非 Latin-1 字符时，仍是一次追加一个节点，
而且节点比 P71 大了 8 字节。P74 的数字：320k 次 `s += '中' + i` 从 71.2 MB 到 76.3 MB、62.6 ms 到 68.1 ms（约慢 9 %）。
本包让 UTF-16 的短追加也走缓冲，使中文、表情等文本的拼接与 Latin-1 同样省内存。

目标：上面的测量回到 **≤ 62.6 ms 且内存明显下降**（给出数字）；Latin-1 的 P74 数字不回退。

## 背景（先读）

- `P71-rope-strings.md`、`P74-string-followups.md`「状态」：`RopeState::{Unflattened, Appended, Flat}`、共享的 `Rc<RefCell<Vec<u8>>>` 追加缓冲、
  谁可以向缓冲追加（只有缓冲末尾的拥有者）、展平规则。
- `boa_string` 里 Latin-1 与 UTF-16 两种存储，以及混合时的展平。

## 要做的事

1. 方案（由实现者定）：例如缓冲按 `u16` 存，或 Latin-1 缓冲在遇到第一个非 Latin-1 字符时整体升级为 UTF-16。写明内存与正确性的取舍。
2. 不变量：一个缓冲同时只有一个可追加的末端（P74 的规则）；把旧字符串留在原处的 `s1 = s; s += x` 之后，`s1` 不变；孤立代理项落在追加边界时展平结果正确。
3. 测量：上面的 320k 次追加（时间与峰值内存）、P60 `concat`，Latin-1 情形前后对比。

## 测试

- `boa_string` 单元测试：Latin-1 → UTF-16 的升级发生在缓冲中途；代理项对被拆在两次追加的边界；分叉（`s1 = s; s += 'a'; s1 += '中'`）后两边都正确；
  空串；深链的展平与释放（不爆栈）。
- 引擎差分测试（解释与 `BOA_JIT_THRESHOLD=0`，与 Node 一致）：`length`、`charCodeAt`、`codePointAt`、`for…of`、`===`、`indexOf` 在追加后的字符串上。

## 验收

- `boa/` 内 `cargo test -p boa_string`、`cargo test -p boa_engine`（默认、`--features jit`）、clippy `-D warnings`（有无 `jit`）、`cargo fmt`；
  根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `built-ins/String`、`language/expressions/addition`、`language/expressions/compound-assignment` 前后逐项 0 回退。
- P60 套件不变慢（同一轮，中位差 ±3%）；`perf-js.md`、`size.md` 各追加一行。

## 不做

rope 叶子合并的其他策略；`String.prototype.concat`、模板字符串的专门路径。

## 状态

未开始。
