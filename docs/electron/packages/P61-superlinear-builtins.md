# P61 超线性内建修复

**依赖**：无（P60 合并后用它的规模探针验收；未合并时用本文的探针）  **规模**：S  **修改**：`boa/core/string/src/`、`boa/core/engine/src/builtins/{map,set,regexp,string}/`、`boa/SU.md`、上述 crate 的测试

## 目标

`perf-baseline.md` 测到三个内建的耗时随输入**平方级**增长，中等规模就跑几秒到几分钟。它们比 30–300× 的常数差距更要紧，
因为真实程序在普通规模下就会撞上。本包把这三个修成线性（或 n log n），再用 P60 的规模探针扫一遍其余内建，修掉同类问题。

| 操作 | 基线（解释器） | Node |
|---|---|---|
| `'…'.split('a')` | 5k 段 32 ms，10k 133，20k 555，40k 2,297；200k 段 120 s 未完成 | 200k 段约 26 ms |
| `Map`/`Set` 的 `delete`（删一半键） | N=10k 42 ms，20k 181，40k 878；300k 数分钟未完成 | — |
| 生成文本上的 `matchAll` + `replace` | 5k 行 470 ms，20k 行 15,000 ms（4 倍输入 32 倍时间） | 5k 行 1.6 ms |

## 背景（先读）

- `boa/SU.md` 开头：本仓库的 Boa 是 `su/0.22` 分支，每处改动都要在 SU.md 记一条（写法照现有条目：哪个 crate、哪个文件、改了什么、为什么）。
- 下面三处是**已定位的原因**（总控阅读代码所得，实现者先用测试证实再改）：
  1. **`split`**：`builtins/string/mod.rs` 的 `split`（约 1948 行）循环调用 `JsStr::index_of(sep, i)`。
     `boa/core/string/src/str.rs` 约 241 行的 `index_of` 是 `self.windows(len).skip(from_index).position(…)`，而
     `boa/core/string/src/iter.rs` 的 `Windows` 只实现了 `next`，没有 `nth`，所以 `skip(from_index)` 每次从头走 `from_index` 步。
     每次查找 O(i)，整个 split O(n²)。`indexOf`、`includes`、`replaceAll`、字符串模式的 `replace` 等凡是带起点调用 `index_of` 的都受影响。
  2. **`Map`/`Set` 的 `delete`**：`builtins/map/ordered_map.rs` 的 `remove`（约 136 行）与 `builtins/set/ordered_set.rs` 的 `remove`（约 92 行），
     没有迭代器持锁（`lock == 0`）时用 `IndexMap::shift_remove`，它把后面所有元素前移一位，O(n)。持锁时已经走"留空位 + `swap_remove`"的路径。
  3. **正则扫描**：`builtins/regexp/mod.rs` 的 `exec`（约 1190 行）遇到 Latin-1 字符串时 `input.to_vec()`，**每次匹配**把整串拓宽成 UTF-16
     （注释写着 regress 不支持 Latin-1）。`matchAll`、全局 `replace`、`split` 每取一个匹配都调一次，O(串长 × 匹配数)。
     基线里 4 倍输入花了 32 倍时间，比平方还陡，说明可能还有第二个原因（例如 `replace` 拼结果的方式），要先分开测 `matchAll` 和 `replace`。
- `boa/core/engine/src/builtins/map/mod.rs` 与 `set/mod.rs` 里 `MapLock`/`SetLock` 的用法：迭代期间删除的语义（被删的项不再被访问、
  新加的项会被访问）由"空位 + 解锁时清理"保证，修改不得破坏它。
- `docs/jit.md`「Testing」最后一条：test262 怎么跑、怎么逐项比较前后结果。

## 要做的事

### 1. 字符串查找

- `index_of` 从 `from_index` 处开始查，不再从头走：要么给 `Windows` 实现 `nth`（以及 `advance_by` 能用时），要么直接在
  `from_index..` 的切片上查。Latin-1 对 Latin-1 的常见情形用字节切片查找（标准库或已有依赖即可，**不加新 crate**）。
- 查一遍 `boa/core/string` 与 `builtins/string` 里其他"从某处开始"的查找（`last_index_of`、`includes`、`starts_with` 带位置等），同样处理。

### 2. `Map`/`Set` 删除

- 不持锁时也走"留空位"路径：删除是把条目换成空位（O(1)），空位数超过条目总数一半时**在不持锁的时刻**整体压实一次（O(n)，均摊 O(1)）。
- 迭代顺序、`size`、边迭代边删/加的语义与现在一致；`clear` 重置空位计数。`Map` 与 `Set` 用同一个思路，能共用的辅助就共用。

### 3. 正则

- 先分开测 `matchAll`、全局 `replace`、全局 `exec` 循环、正则 `split`，确认每一项的超线性来自哪里。
- 拓宽：同一个字符串在一次 `replace`/`split`/`matchAll` 迭代中**只拓宽一次**。做法由实现者定（例如在这些内建的循环里拓宽一次后复用，
  或一个按字符串身份缓存最近一次拓宽结果的线程局部单项缓存）；不得让用户可观察的行为（`lastIndex` 读写次数、对被改写的 `exec` 的调用）发生变化。
- 发现的第二个原因一并修掉，写进 SU.md。

### 4. 扫一遍

- 用 P60 的 `scaling.js`（未合并时自己写同样的 N/4N 探针）跑一遍所列内建，Latin-1 与 UTF-16 字符串各一遍。
- 比值 > 8 的：原因明确且改动局部的，本包修掉；需要大改的，写进 `docs/electron/perf-js.md`（或本文「状态」）交给 P64/P65。

## 验收

- 新增测试（放 `boa/core/engine` 对应内建的 `tests.rs`）：边迭代边删的 `Map`/`Set`、删除后迭代顺序、大量删除后压实、
  `split`/`indexOf` 带起点的边界、Latin-1 与 UTF-16 输入上的 `matchAll`/`replace` 结果与 Node 一致（照仓库冻结 Node 观测值的做法）。
- 规模：三项各自 4 倍输入的耗时比 ≤ 5；`split` 200k 段与 `Map` 删 300k 键在解释器下各 < 1 s。
- `cargo test -p boa_engine` 中 map、set、string、regexp 相关测试通过；test262 的 `built-ins/{String,Map,Set,RegExp}` 前后**逐项**比较，不得有新失败。
- `perf-js.md` 追加本包合并后的 `splitjoin`、`mapset`、`regex` 三项数字。

## 不做

正则引擎本身（regress）的提速与 Latin-1 原生支持（P64）；与超线性无关的常数优化（P64/P65）；改变任何可观察语义。

## 状态

（实现者完成后填写）
