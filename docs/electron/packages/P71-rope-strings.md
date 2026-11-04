# P71 字符串 `+=` 线性化（rope）

**依赖**：P64（已合并）  **规模**：M  **修改**：`boa/core/string/`、`boa/core/engine/src/value/operations.rs`（`JsValue::add` 的字符串分支）、
对应测试、`boa/SU.md`

## 目标

`s += 'ab' + i` 重复 N 次是 O(n²)：每一步 `JsString::concat` → `concat_array` 新分配一个等于总长的串并复制全部已有内容。
32 万次追加 Verve 67 秒，Node 27 毫秒。本包按 [`spikes/p64-rope.md`](../spikes/p64-rope.md) 的方案加第五种字符串表示 `Rope`，
让 `+=` 变成线性。

**总控对方案「决定点」的答复（2026-11-04）**：

1. 单列为本包，单独评审。
2. 接受阈值 256 码元（两侧合计小于它时仍直接 `concat_array`），第一版**不做**叶子合并；叶子合并与 `join` 直接遍历叶子
   是方案第 4 阶段，只有测量证明有收益才做，单独 commit。
3. 目标：`concat` 探针（2 万 / 8 万 / 32 万次追加）回到线性；P60 套件 `concat` 对 Node 的倍数 ≤ 5×；套件里其余字符串类各项
   （`splitjoin`、`regex`、`json`、`tokenize`、`template`）与 `scaling.js` 的字符串探针不回退（同一轮、5 个进程，中位差在 ±3% 内）；
   方案担心的 `as_str` 多一个分支，另用 `charCodeAt` 循环的微探针前后对比。

## 背景（先读）

- [`spikes/p64-rope.md`](../spikes/p64-rope.md) 全文：表示、何时建 rope、展平策略、与 GC 的关系、解释器与 JIT、假设「平切片」的代码、
  分阶段实现、风险。它是本包的设计，**按它做**；要偏离先在「状态」写明原因。
- `P64-strings-regex-json.md`「状态」第 3 项（`concat` 慢在哪里）与参考目标表里 `concat` 一行。
- `boa/core/string/src/lib.rs`（`JsStringKind`、`as_str`、`concat_array`）、`vtable/`、`display.rs`；`boa/core/engine/src/value/operations.rs:27`。

## 要做的事

按方案第 1–3 阶段：

1. **`boa/core/string`**：`JsStringKind::Rope`、`vtable/rope.rs`、`JsString::concat_lazy`；`as_str` 第一次调用时迭代展平并缓存，
   展平后释放子串；**迭代**的 `drop`（32 万层深的链不能递归）；`len` 不展平；`display.rs` 等所有 `match JsStringKind` 的地方补齐。
   每个 `unsafe` 写 `// SAFETY:`（`boa_string` 已 `deny(undocumented_unsafe_blocks)`）。
2. **接入 `JsValue::add`** 的三处字符串拼接，用阈值常量；模板串与 `concat_array` 的多段拼接**不改**。
3. **验证**（见验收）。

先交第 1 步的单元测试与 `size_of` 实测（方案里 96 字节是按布局推算的），再接第 2 步。

## 测试

- `boa_string` 单元测试：Latin-1 与 UTF-16 混合、孤立代理项落在两侧边界、空串、深 100 万的链展平与释放（不爆栈）、
  展平后再 `clone`/`slice`、`len` 不触发展平、`ptr_eq`、展平只发生一次。若本机能跑 miri，跑 `boa_string` 的单元测试并贴结果；不能就写明。
- `Context` 级测试：`+=` 循环 20 万次；rope 作属性键（含与已有键相等）、作 `Map` 键；`===`、`<`、`localeCompare`；
  经过 `String.prototype` 的每个方法；`JSON.stringify`、正则 `exec`/`replace`；作为 `eval`/`Function` 的源码文本。
- test262：`built-ins/String`、`built-ins/RegExp`、`built-ins/JSON`、`language/expressions/addition`、
  `language/expressions/compound-assignment`、`language/expressions/template-literal` 前后逐项 0 回退。

## 验收

- `scaling.js` 的 `concat` 探针三档的时间与峰值内存（`verve run` 的 RSS 或进程内计数），前后对比，证明线性。
- P60 套件字符串类各项前后对比（同一轮），`perf-js.md` 追加一行。
- `cargo test -p boa_string`；`boa/` 内 `cargo test -p boa_engine`（默认与 `BOA_JIT_THRESHOLD=0`）；`cargo clippy -p boa_string -p boa_engine --all-targets --features jit -- -D warnings`；
  根 `cargo test -p verve`、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- 体积增量写进 `size.md`。

## 不做

JIT 里的字符串 `+`（方案已说明 JIT 只调同一个 `Add`，无需改）；展平后再追加的情形（不比现状差，文档注明）；
`Array.prototype.join` 与模板串的改动；字符串的其他表示（切片合并等）。

## 状态


分支 `electron/p71-rope-strings`，三个 commit：`boa_string` 的 rope 种类（含单元测试与 `size_of`）、`JsValue::add` 接入（含 `Context` 级测试）、
本节与 `perf-js.md`/`size.md`。基线是 `docs/electron-plan` `05f26157`，对比用同一台机器上另建的基线 worktree 构建的同配置二进制。

**做了什么**

- `JsStringKind::Rope`、`vtable/rope.rs`、`JsString::concat_lazy`、`ROPE_MIN_LEN = 256`。节点 `RopeString` 实测
  `size_of` = **88 字节**（方案按布局推算的 96 偏高：`RopeState` 用了 `JsString` 的非空 niche，16 字节而不是 24）。
  `as_str` 的第一次调用迭代展平（堆上的栈，从右向左填，一次分配），把 `Unflattened` 换成 `Flat` 并释放两个子串；
  `len`/`clone`/`ptr_eq` 不展平。`drop` 迭代（只把「自己是 rope」的孩子放进本地 `Vec`，别的直接释放）。
  `slice_unchecked` 对已展平的 rope 切的是它的平串，不让切片拖着节点。`display.rs` 的 `match` 补了 `Rope`。
- `JsValue::add` 的三处字符串分支改用 `concat_lazy`；模板串与其余 `concat_array` 调用点未动。JIT 无改动（字符串 `+` 本来就走同一个 `Add` 处理函数）。
- `boa/SU.md` 两条。

**与方案的偏差**

- 节点 88 而不是 96 字节（上）。
- 方案说「`as_str` 多一个分支」：实现里没有——`Flat` 判断在 rope 自己的 vtable 函数里，其他四种的 `as_str` 一个字节没动，
  所以平串的热路径没有新增开销；`charCodeAt` 微探针确认了这点（见 `perf-js.md`）。
- `concat_lazy` 的签名是 `(&JsString, &JsString)`（方案写的是取两个字符串）；短串仍走 `concat`，返回新分配的串，与此前一致。
- 展平的结果不做 `StaticJsStrings` 驻留（`concat_array` 会）。≥ 256 码元的串不会命中静态表，所以没有行为差别。

**验证（都实际跑过）**

- `cargo test -p boa_string`：44 个全过（含 11 个 rope 测试：Latin-1/UTF-16 混合、代理项落在边界、空串、深 100 万的左斜与右斜链展平、
  深 200 万节点的链 drop、展平后 `clone`/`slice`、`len` 不展平、展平只一次、`ptr_eq`/refcount、共享子串的 DAG）。
- miri：本机的 nightly 原先没有 miri，`rustup component add miri --toolchain nightly` 装上；Windows 目标上 `~/.cargo/config.toml` 的
  `runner = cargo-lowprio` 会抢走 miri 的运行器，所以用 `cargo +nightly miri test -p boa_string --lib --target x86_64-unknown-linux-gnu`
  （`CARGO_LOWPRIO_OFF=1`，深度测试在 miri 下用 2000 层）：44 个全过，62 秒。
- `boa/` 内 `cargo test -p boa_engine`：1206 个全过；`BOA_JIT_THRESHOLD=0 cargo test -p boa_engine --features jit`：1250 个全过。
  `tests/rope.rs` 11 个 `Context` 级测试：`+=` 20 万次；rope 作属性键（含与已有键相等）、`Map`/`Set` 键；`===`、`<`、`localeCompare`；
  `String.prototype` 的每个方法；`JSON.stringify/parse`、正则 `exec`/`replace`/`split`；作为 `eval`/`Function` 的源码；展平后再追加；
  混合宽度与跨边界代理项；非字符串操作数。
- test262（`E:\test262`，pinned commit），`built-ins/String`、`built-ins/RegExp`、`built-ins/JSON`、`language/expressions/addition`、
  `compound-assignment`、`template-literal`，前后各跑 `-vv` 把每个测试的结果行排序后 `diff`：六个子集差异 0 行；`latest.json` 逐字节相同
  （RegExp 1 个失败、String/RegExp 各有已忽略项，前后一致）。
- `cargo clippy -p boa_string -p boa_engine --all-targets --features jit -- -D warnings`、`cargo fmt`（`boa/` 内与根 `--all --check`）、
  `bash scripts/style-check.sh` 在每个 commit 前通过。**未跑**：根 `cargo test -p verve`（构建纪律：由总控统一跑；本包没有改 `src/`）。

**验收对照**

| 项 | 结果 |
|---|---|
| `concat` 探针回到线性 | 是：20k/80k/320k 次追加 11.0 / 23.4 / 75.5 ms（此前 30 / 795 / 62,744 ms）；增长 2.1×、3.2×（输入 4×）。`scaling.js` 的 `concat` 4n 比从 16× 到 4.2× |
| 峰值内存 | 320k 次约 66 MB（此前约 20 MB）：线性增长，但每次追加留一个 88 字节节点加一个小叶子，直到第一次读取 |
| `concat` 对 Node ≤ 5× | **未达**：JIT 8.1×（5.7 ms 对 0.7 ms），此前 31×；解释器 8.3×，此前 134× |
| 其余字符串类不回退 | 套件 `splitjoin`/`regex`/`json`/`tokenize`/`template` 与 44 个 `scaling.js` 探针都在该轮噪声内（负载 15–95 %，其他三个 worker 同时在跑），个别中位数差到 +8–11 %，没有一致的方向，也不在字符串路径上；**达不到「±3 %」的统计强度**，原因是机器负载，不是已发现的退步 |
| `charCodeAt` 微探针 | 平串前后无差别；rope 出来的串每次调用与平串相同，一次性的展平在数字里 |
| 体积 | 未剥离的 release `verve` +6,656 字节（27,097,600 → 27,104,256） |

**`concat` 没到 5× 的原因（测量得到）**：同一进程里，只做 `x = 'ab' + i` 就要 2.3 ms（数字转串加一次 `concat_array`，与追加无关），
对现成片段追加 3.0 ms（约 150 ns 一次：一个节点的分配与释放），Node 整个循环 0.25–0.85 ms。所以单靠 rope 到不了 5×。
叶子合并（方案第 4 阶段）能削掉追加那 3 ms 的一部分和内存放大，但削不动 `'ab' + i` 的 2.3 ms，也到不了 3.5 ms 的线，
按任务「测量证明有收益才做」的要求**没有做**。要再降，得改 `'ab' + i`（数字转串与 `concat_array` 里每次的静态串表查找），是另一个包。

**已知问题 / 没做**

- 展平后再追加仍是 O(n²)（每轮读一次就回到旧行为，只多一个节点的壳），按任务不修。
- 内存放大（上表）；叶子合并未做。
- `Array.prototype.join` 与模板串未改（任务「不做」）。
- 比较用的基线是我另建的 worktree `verve-e-p71-base`（`git worktree add --detach`，构建输出在 `E:\cargo-target\verve-e-p71-base`），总控合并后可以 `git worktree remove`。
