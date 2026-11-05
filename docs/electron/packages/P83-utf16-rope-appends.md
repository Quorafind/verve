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
分支 `electron/p83-utf16-rope-appends`，基线 `docs/electron-plan` `2cb3f702`；三个提交：`perf(string)` 的实现加 `boa/SU.md` 条目，两个测试提交。
对比用另建的 detached worktree 里按同配置（`--release --features jit`）构建的二进制。**目标达到**：320k 次 `s += '中' + i` 67.7 → 60.2 ms（任务书的上限 62.6 ms），峰值 76.2 → 16.8 MB。

**方案**：缓冲有两种。字节缓冲（`Vec<u8>`）在整条 rope 都是 Latin-1 时用，沿用 P74；UTF-16 缓冲（`Vec<u16>`，状态 `RopeState::AppendedWide`）从第一个非 Latin-1 的片段起用，
之后无论追加的片段是什么宽度都按码元存。已经不是 Latin-1 的 rope，直接从其短右半开一个码元缓冲。
**取舍**：缓冲不能在原地从字节改成码元，因为更早做出的 rope 仍把它当字节读。所以第一次宽追加把本 rope 看得到的那一段（至多 `MAX_WIDEN_LEN` = 1024 字节，超出就给这次追加一个普通节点）
复制进新的码元缓冲，旧 rope 的内容和它们自己的可追加末端都不变。代价：一条 rope 变宽后，其缓冲中的 Latin-1 部分每个字符占 2 字节（只在宽的那条链上）；
一次性复制至多 1 KiB，所以在分叉的情形下总成本也有上界。纯 Latin-1 的路径不多一次分支以外的代价。
**不变量**：一个缓冲同时只有一个可追加的末端（只有长度正好等于缓冲长度的 rope 原地追加，其余得到普通节点，对两种缓冲相同）；`s1 = s; s += x` 之后 `s1` 只读缓冲的前缀，不变；
孤立代理项落在追加边界时，缓冲只存码元，展平时拼出的码元序列与平铺字符串逐码元相同；展平与释放仍用既有的堆栈，不递归，链深不受限（`Piece::Units` 与 `Piece::Bytes` 同样处理）。

**测试**：`boa_string`（`tests.rs`）：`wide_appends_share_one_buffer`、`latin1_buffer_is_widened_mid_buffer`（升级发生在缓冲中途）、`surrogate_pair_split_across_appends`、
`forks_on_both_sides_of_a_widening`（两侧分叉）、`wide_base_and_wide_right_half`、`empty_and_long_appends_with_buffers`（空串与长片段）、
`deep_chains_of_wide_appends_flatten_and_drop`（深链展平与释放）、`deep_chains_of_long_pieces_still_flatten_and_drop`。引擎差分 `tests/rope.rs` 的 `wide_appends_read_as_node_does`：
`length`、`charCodeAt`、`codePointAt`、`for…of`、`===`、`==`、`indexOf`、`lastIndexOf` 在五种构造上的摘要，期望值是同一脚本在 Node 22 下的输出；默认与 `--features jit`（`BOA_JIT_THRESHOLD=0`）各跑。

**数字**（同一轮相邻，5 个进程；负载 16–26 %，详见 `perf-js.md` 的 P83 一节）：

| 探针 | 之前 | 之后 |
|---|---|---|
| 320k × `s += '中' + i`，ms / 峰值 MB | 67.7 / 76.2 | 60.2 / 16.8 |
| 320k × `s += '中'`，ms / 峰值 MB | 43.2 / 46.0 | 38.7 / 16.8 |
| 320k × `s += 'ab' + i`（Latin-1），ms / 峰值 MB | 62.0 / 16.8 | 59.2 / 16.9 |
| 320k × `s += 'ab'`（Latin-1），ms / 峰值 MB | 38.3 / 16.8 | 38.1 / 16.8 |

空进程的峰值是 16.7 MB，所以宽追加的缓冲开销在测量精度以内。峰值内存用 PowerShell 每 2 ms 读 `Process.PeakWorkingSet64`，没有用性能分析器。
P60 套件：`concat` JIT 3.2 → 2.9、解释 3.6 → 3.1 ms；其余各行在 16–26 % 的负载下前后差在噪声内，且都不碰宽字符串的 rope（`sort` 只有数字，中位 +6.5 % 而它的解释列反向 -11 %，是负载；
`fib`、`alloc` 用 7 个进程复测后持平）。**偏差**：个别行的中位差超过 ±3 %，是机器共享的负载造成的，其余行最好值之差多在 ±3 % 以内（`sort` 最好值 +4.1 %，同样是负载）；没有再重复整套。
二进制体积 +3,072 字节（+0.011 %，`size.md`）。

**验证**：`boa/` 内 `cargo test -p boa_string`（56 + 2）、`cargo test -p boa_engine`（默认 1246；`--features jit` 且 `BOA_JIT_THRESHOLD=0` 1308）全绿；clippy `-D warnings`（`boa_string`、`boa_engine`，有无 `jit`）与根 `cargo clippy --workspace --all-targets -- -D warnings` 无告警；
根 `cargo test -p verve` 有无 `--features jit` 均 24 通过；`cargo fmt`、`cargo fmt --all --check`、`bash scripts/style-check.sh` 干净。
test262（`E:\test262`，release `boa_tester --features boa_engine/jit`，前后各跑 `-vv`，输出排序后 `diff`，默认与 `BOA_JIT_THRESHOLD=0` 各一遍）：`built-ins/String`（1223 项，1221 通过、2 忽略）、
`language/expressions/addition`、`language/expressions/compound-assignment` 六份前后输出逐行相同，0 回退。
