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

未开始。
