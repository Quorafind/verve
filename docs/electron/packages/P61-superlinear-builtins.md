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

2026-11-03 完成。分支 `electron/p61-superlinear`，基于 `docs/electron-plan` 的 `c39b800a`
（开工时是 `cde79ef3`；两轮复核期间基线前进过几次——P63 的 JIT 合并、一行 JIT 代码挪位、`review.md`
加了几行——分支每次都 rebase 过去）。其中 `dd1cdb6f` 挪了 `src/lib.rs` 的 JIT 代码位置，所以最终
二进制是从 `c39b800a` 之上的本包代码重建后重新测的；`boa/core` 的行为不受这些提交影响。三个 `fix(boa)` 提交加一个 `docs(electron)`，
每个提交单独能编译、单独正确，并各自带自己的 `boa/SU.md` 条目：

| 提交 | 内容 |
|---|---|
| `d8d031b7` | `JsStr::index_of` 从 `from_index` 起查（含 `builtins/string/tests.rs` 的 `indexOf`/`split`/`replaceAll` 测试） |
| `5ccf127a` | `Map`/`Set` 删除留墓碑 + 均摊压实 + 迭代器结束释放锁 + 克隆不继承锁；`for_each_native` 跨墓碑；`OrderedSet::is_empty`；`isDisjointFrom`/`isSubsetOf`/`intersection` 遍历期间持锁 |
| `d08d62f5` | 正则 Latin-1 主体一次扫描只拓宽一次，每个出口（含 `@@search`/`@@split` 与错误路径）释放；`JsString::ptr_eq` |
| 本文 | `docs(electron)` |

探针是自己写的 N/4N 形式（放会话 scratchpad，未建 `scripts/js-bench/`，那是 P60 的目录）。

### 测量条件

**解释器** release 构建：`cargo build --release --bin verve --no-default-features` 与
`cargo build --release -p boa_tester`（在 `boa/`），Windows 10 x64，`CARGO_BUILD_JOBS=6`。
机器是共享的 Ryzen 9 7950X（32 线程）；测量全程 `\Processor(_Total)\% Processor Time` 读数
**12–18%**（开工那一轮约 12%，复核复测时 11.9–18.5%）。
每个用例 **5 次**（N/4N 交错表是 **7 次**，N 与 4N 交替跑），取**中位**；一项一个进程。

### 探针证实了三个原因

用基线二进制（基线 `cf37791b` 导出到 `E:\cargo-target\p61-baseline-src` 单独构建）跑同一批探针。
`cf37791b` 到本包起点 `cde79ef3` 之间只有文档提交，没有 `boa/core` 改动，这组基线数字仍然有效。

| 用例 | 基线 | 改后 | 倍数 |
|---|---|---|---|
| `split('\|')` 2 万段 | 204.45 ms | **1.13 ms** | 181× |
| `Map` 8 轮「填 N 删一半」（N=2 万） | 1,513.69 ms | **40.18 ms** | 38× |
| `Set` 同上 | 1,427.70 ms | **37.17 ms** | 38× |
| `matchAll`（2 万行） | 2,375.78 ms | **73.06 ms** | 33× |
| 全局 `replace`（带捕获组，2 万行） | 2,047.57 ms | **92.16 ms** | 22× |
| 全局 `exec` 循环（2 万行） | 674.45 ms | **45.51 ms** | 15× |

基线的超线性是直接的：`Map` 删一半在 4 倍输入下是 **31.1×**（1,513.69 → 47,123.15 ms），
`Set` 是 **22.3×**；这正是 `shift_remove` 每次前移的 O(n²)。`split`/`indexOf`/正则三项的超线性来自
`Windows` 只有 `next`（`skip(n)` 逐个走）与每次匹配都 `to_vec()` 拓宽整串。

### 规模验收（改后，最终二进制，N=2 万，7 次交错中位）

| 用例 | N ms | 4N ms | 比值 |
|---|---|---|---|
| `split('\|')` | 1.13 | 4.68 | **4.13** |
| `indexOf` 带起点（查末尾 + 未命中） | 0.33 | 1.31 | **4.01** |
| `Map` 8 轮填 N 删一半 | 40.18 | 173.28 | **4.31** |
| `Set` 同上 | 37.17 | 160.18 | **4.31** |
| `Map` 32 轮「删+加+完整迭代」 | 353.47 | 1,475.63 | **4.17** |
| `matchAll` Latin-1 | 73.06 | 295.92 | **4.05** |
| 全局 `replace`（带捕获组） | 92.16 | 453.49 | **4.92** |
| 全局 `exec` 循环 | 45.51 | 185.08 | **4.07** |
| `matchAll` + `replace` 同一次计时（基准套件口径） | 166.84 | 758.49 | **4.55** |

复核时跑了三轮（同样 7 次交错、N 与 4N 交替，取中位）：

- 旧基线 `cde79ef3` 的二进制，负载约 12%：**3.97–4.34**（`matchAll` 3.97、`split` 4.05、
  `Map` 迭代 4.09、`indexOf` 4.28、`Map` 删半 4.30、`Set` 删半 4.34）。
- rebase 到 `4b151404`（此后又前进到 `dd1cdb6f`，只动 JIT 一行）后重建的最终二进制，负载 11.9–18.5%：**3.84–4.92**（`matchAll` 3.84、
  `split` 3.86、`indexOf` 4.09、`Map` 迭代 4.41、`Map` 删半 4.77、`Set` 删半 4.80、
  `replace` 带捕获组 4.92）。

上表是开工那两轮的绝对值；绝对耗时随负载浮动约 ±10%，比值不受影响。三轮全部 ≤ 5。

第 2 轮改动后的复测（最终二进制、`performance.now()`、每次进程内重复 20 次取均值、7 轮取中位）：

| 用例 | N ms | 4N ms | 比值 |
|---|---|---|---|
| `split('\|')`（N=2 万，负载 43.9–46.3%） | 2.05 | 10.43 | 5.10（N 仅 2 ms，噪声占比大） |
| `split('\|')`（N=5 万、重复 100 次，负载 11–14.9%） | 3.04 | 13.46 | **4.42** |
| `Map` 8 轮填 N 删一半 | 47.21 | 223.65 | **4.74** |
| `Set` 同上 | 40.23 | 192.17 | **4.78** |
| `Map` 32 轮「删+加+完整迭代」 | 451.73 | 1,899.76 | **4.21** |
| `matchAll` | 54.01 | 215.28 | **3.99** |
| 全局 `replace`（带捕获组） | 311.01 | 1,344.99 | **4.32** |

那一轮 CPU 负载是 43.9–46.3%（另两个实现者在做 release 测量），`split` 的 N 只有 2 ms，比值被
噪声推到 5.10；放大规模到 N=5 万且负载回到 11–14.9% 后是 4.42，与上一轮的 3.86–4.13 一致。

基线前进到 `c39b800a` 后，从最终代码树重建 `verve.exe`（16:15）与 `boa_tester.exe`（16:19）又测了一轮
（N=2 万，每次进程内重复 20 次取均值，7 轮取中位，负载 **11.5–14.6%**）：

| 用例 | N ms | 4N ms | 比值 |
|---|---|---|---|
| `split('\|')` | 1.17 | 5.03 | **4.30** |
| `Map` 8 轮填 N 删一半 | 40.30 | 176.49 | **4.38** |
| `Set` 同上 | 35.31 | 157.47 | **4.46** |
| `Map` 32 轮「删+加+完整迭代」 | 418.09 | 1,758.24 | **4.21** |
| `matchAll` | 50.95 | 202.70 | **3.98** |
| 全局 `replace`（带捕获组） | 274.20 | 1,246.63 | **4.55** |

这一轮全部 ≤ 5（3.98–4.55），与上两轮一致；绝对耗时比开工那轮低约 10–20%，负载也更低。

绝对耗时（5 次中位，负载约 12%）：`split` **200k 段 11.68 ms**；`Map` 删 **300k 键 29.89 ms**。
验收要求各 < 1 s，均满足（基线里这两项分别是「120 s 未完成」与「数分钟未完成」）。

### 修了什么

1. **`JsStr::index_of`**（`boa_string`, `str.rs`）：从 `from_index` 处切出子切片再查，不再
   `windows(...).skip(from_index)`。编码相同的一对用字节切片 / `u16` 切片比较，编码不同的仍逐码元比较。
   `split`、`indexOf`、`includes`、`replaceAll`、字符串模式 `replace` 随之线性。
   `last_index_of` 与 `starts_with`/`ends_with` 本来就是定长循环/比较，不需要改。
2. **`Map`/`Set` 删除**（`ordered_map.rs`, `ordered_set.rs`）：删除把条目换成墓碑（O(1)），
   墓碑数超过剩余条目数且无人持锁时整体压实一次（O(n)，均摊 O(1)）。迭代顺序、`size`、
   边迭代边删/加语义不变；`clear` 重置空位计数。`Set::delete_in_place` 不压实，供
   `Set.prototype.difference` 按下标遍历时保位。
3. **正则拓宽**（`regexp/mod.rs`）：Latin-1 → UTF-16 的拓宽结果按分配身份（`ptr_eq`）缓存在线程局部
   `WIDENED`（单项），一次全局扫描只拓宽一次；`lastIndex` 的读写次数、被改写的 `exec` 的调用都未变。
   「第二个原因」分开测过：没有独立的第三处，基线里「4 倍输入 32 倍时间」是 `matchAll` 与 `replace`
   各自平方叠在同一次计时里相加。

### 按复核意见做的修正

- **`SU.md` 补齐正则条目**，并让每条挂在自己的提交上（此前 `Map`/`Set` 条目落在了正则提交里）。
  `index_of` 条目里「the regexp widening below」现在确实指向下面那条。
- **提交历史重排**：`for_each_native`（含 `is_empty` 修复）折叠进墓碑提交，不再有「修上一个提交」的
  提交；每个提交单独能编译、各自带 SU.md 条目。
- **压缩此前实际不触发**：`MapIteratorLock`/`SetIteratorLock` 只在 `Finalize` 里 `unlock()`，而
  `next()` 走到头是把锁 `take()` 出来当普通值丢掉，`unlock()` 不会执行——凡被 `for…of`、展开、
  `Array.from`、`entries()` 完整迭代过的 `Map`/`Set` 都永远 `lock ≥ 1`，`compact()` 永不触发，
  LRU 写法（`delete` 后 `set`）会让 `full_len` 无限增长。现在两处 `next()` 在耗尽时显式 `release()`
  解锁，`Finalize` 留给中途放弃的迭代器；`difference`/`symmetricDifference` 克隆 `OrderedSet` 时
  把 `lock` 置 0。测试改成 Rust 层断言 `full_len()`：大量删除后确实压实；先完整迭代一次再大量删除
  也压实；被锁住的源克隆出来的副本能压实。
- **`WIDENED` 不再长期钉住大字符串**：`ReleaseWidened` 是 drop guard，`exec`、`@@search`、`@@split`
  的每一条出口都释放它，`?` 错误（`lastIndex` 不可写时的 `this.set(...)?`）也在内。释放点是：结束全局
  扫描的那次匹配失败、`lastIndex > length` 的兜底、既非 global 也非 sticky 的那一次匹配、整个
  `@@search` 与整个 `@@split`（含 `lim` 返回、空主体路径与抛错路径）。粘性探测失败**不**释放——
  `@@split` 用粘性分割器逐位探测。测试断言：扫描中缓存存在、扫描结束后为空，`split`（含 `limit`、
  空主体、抛错三条出口）与 `search` 之后也为空。
- **顺手改**：`Set.prototype.difference` 去掉 `delete_in_place` 后多余的 `continue`（旧注释
  「after deletion we're in the correct next position」已不成立，第 iii 步照规范 `index += 1`）；
  `boa/core/string/src/tests.rs` 里 `µµµx` 的注释原写「in UTF-16」不实（该串是 Latin-1），改成分开
  断言 Latin-1 一对与跨编码一对；补测试：`difference` 在 `has` 对连续元素返回 true、以及 `this`
  已有墓碑时保持顺序，同一正则交替扫描两个不同 Latin-1 主体，内容相同但分配不同的字符串。

### 第 2 轮复核后的补充

- **`@@split` 从不释放**：`split` 的所有出口此前都没释放，`bigFile.split(/
/)` 之后主体与其两字节副本
  一直常驻。现在在算出 `arg_str` 后建 guard，覆盖循环结束、`lim` 返回、空主体路径、最后的 `Ok` 与
  `?` 错误。
- **`@@search`** 同样在入口建 guard（它保留 `g` 调用 `exec`，匹配成功后那次不释放）。
- **仍会保留到下一次 Latin-1 `exec` 的情形**（如实写明，未改）：`/x/g.test(big)` 与
  `big.search(/x/g)` 现在都在结束时释放；剩下的是扫描中途命中、没有「扫描结束」时点的成功 `test`/`exec`
  （global 或 sticky），以及半途放弃的 `matchAll` 迭代器。
- **Set 方法遍历时的锁**：`isDisjointFrom`/`isSubsetOf`/`intersection` 按下标走 `this` 的同时调用户
  `has`；`has` 删够多时 `delete` 的压实会在遍历中途触发并跳过元素。现在都 `SetLock::new(&this)`。
  新增 Node 22.21.1 对照测试（三个方法各一组，`has` 依次删 1–4）。
- **`JsString::ptr_eq` 挪到正则提交**：只有正则用它；实现、SU.md 句子与 `boa/core/string/src/tests.rs`
  的两个测试都归正则提交。
- **提交正文用词**：墓碑提交原来写「anything iterated once stayed unlocked from compaction for good」，
  意思反了，已改为「stayed locked and never compacted after that」。

### 测试与 test262

- 新增测试：`map/tests.rs`（顺序、压实 `full_len`、`for-of` 与 `forEach` 中删除、`for_each_native`
  跨墓碑、迭代后压实、副本可压实）、`set/tests.rs`（同上，另加 `difference` 的连续命中与含墓碑顺序）、
  `string/tests.rs`（`indexOf` 起点边界、Latin-1/UTF-16 混合、2000 段 `split`、`replaceAll`）、
  `regexp/tests.rs`（Latin-1 上 `matchAll`/`replace`/`exec` 循环/正则 `split` 的 `index`、`lastIndex`、
  捕获组；同一正则交替两个 Latin-1 主体；`WIDENED` 在扫描结束后为空）、
  `boa/core/string/src/tests.rs`（`index_of` 边界与跨编码、`ptr_eq`、同内容不同分配）。
  期望值按仓库做法冻结自 Node 22.21.1 的实测输出，注释里写明。
- `cargo test -p boa_engine --lib`：**1178 passed, 0 failed**（最终代码树的完整一次）。
- `cargo test -p boa_string`：28 passed，另 2 个 doctest 通过。
- `cargo clippy -p boa_string -p boa_engine --all-targets -- -D warnings`：无警告；
  `cargo fmt --all --check`、`bash scripts/style-check.sh`：通过。
- test262（`E:\test262`，`d86b2294`，`boa/tests/tester`，`-v -v` 逐条）：改前（基线二进制）与最终二进制
  各跑 `built-ins/{String,Map,Set,RegExp}`，按「文件名 + 是否 strict」逐条比对结果：

  | 目录 | Total | Passed | Ignored | Failed | 改前/改后差异 |
  |---|---|---|---|---|---|
  | `String` | 1223 | 1221 | 2 | 0 | **0** |
  | `Map` | 204 | 204 | 0 | 0 | **0** |
  | `Set` | 383 | 383 | 0 | 0 | **0** |
  | `RegExp` | 1879 | 1865 | 13 | **1** | **0** |

  第 2 轮改完以后重建了 `verve.exe` 与 `boa_tester.exe`，用最终二进制与基线二进制各重跑一遍，
  再按「完整相对路径 + 是否 strict」逐条比对：1223 / 202 / 382 / 1878 条，**差异 0**。

  汇总：String 1221/1223 通过（2 ignored、0 failed）、Map 204/204、Set 383/383、
  RegExp 1865/1879（13 ignored、**1 failed**）。那 1 个是
  `RegExp/named-groups/non-unicode-property-names-valid.js`，改动前后同样失败：`regress` 解析非
  Unicode 模式下的数学字母命名组报 `Invalid token at named capture group identifier`，与本包的
  `split`/删除/拓宽都无关。

### 没完成 / 已知问题

- **`replace` 无捕获组一项卡在阈值上**：`/INFO/g` 的 `replace`，4 倍输入在不同轮次是 **4.61–5.43×**
  （46.45 → 252.18 ms 那轮 5.43×；另一轮 54.26 → 250.12 是 4.61×），20000 → 320000 是 ×4.61、×5.16。
  带捕获组的 `replace` 是 4.92×（在阈值内）。
  被测文本用 `Array.join` 拼好（不把 `+=` 的代价算进来）。
  原因是每取一个匹配，`RegExpBuiltinExec` 都从 `lastIndex` 重新扫（`regress` 没有「上次匹配结束处」的
  续扫状态），且每次匹配都要构造结果 `Array`、若干 JS 对象，`get_substitution` 每匹配再建一个 `Vec`。
  属 P64 的正则引擎与分配范围，本包不动 regress。
- **正则 `split` 仍超线性**：5k 段 114.42 ms → 20k 段 1,595.91 ms（13.9×），80k 段 80 s 级；基线同口径是
  135.69 → 2,091.65（15.4×）。`@@split` 用粘性分割器在每个位置试一次，每次都从该位置重扫。改动前后
  同一量级（本包没有让它变差），归 P64。
- **字符串 `+=`（无 rope）** 是上一轮扫到的唯一明显超线性内建（约 17×）；本包没改它，本轮也没复测，
  按当时的结论交给 P64。
- **LRU 淘汰写法每次留一个放弃的迭代器**：`map.delete(map.keys().next().value)` 里 `keys().next()` 取一次
  就丢，迭代器持锁直到 GC；两次 GC 之间不压实，墓碑在前端堆积，每次 `keys().next()` 都要越过它们，
  这段区间是二次的。不比 P61 之前差（此前 `shift_remove` 更慢），但压缩在这条写法上收益有限。
  **留给 P65**（可以让一次性 `next()` 在返回后立即还锁，或让迭代器在 Drop 前被及时回收）。
- **没写 `docs/electron/perf-js.md`**：该文件归 P60（由 `P60-js-bench.md` 新建），我开工时它还不存在，
  建它等于动 P60 的文件、两个 worktree 会撞车。三项主指标数字按本文允许的另一选项写在这里；
  **P60 合并时请把上面「规模验收」表搬进 `perf-js.md`。**
- 未跑 `cargo test --workspace`（按 `conventions.md` 归总控）；未做 release 全量体积复查。

### 给其他包的备注

- **P64**：`regress` 没有续扫状态，`replace`/`@@split` 的剩余超线性在这里；`get_substitution` 与每次匹配
  的结果数组也是常数/分配开销的大头。另：`named-groups/non-unicode-property-names-valid.js` 仍失败
  （改动前后一致）；字符串 `+=` 无 rope 也归这里。
- **P65**：`clear()` 在有活迭代器时会截断列表，迭代器会漏掉之后新加的条目（规范只把条目标空）。
  **早于本包，本包未改**，留给 P65。另：`map.delete(map.keys().next().value)` 这类 LRU 写法每次留下一个
  放弃的迭代器，持锁到 GC，两次 GC 之间不压实、墓碑堆积，`keys().next()` 变成 O(墓碑数)；见「已知问题」。另：墓碑在「删除后仍保留一半容量」时会多占内存
  （`full_len` 计入墓碑、`len` 不计）；负载因子只有一处 `empty_count * 2 > len`，`Map` 与 `Set` 各一份。
- **P66**：`index_of`/删除/拓宽都在内建里，JIT 不涉及；但 `opcode/concat` 若要 rope，得同时改 JIT 的
  字符串 `+` 慢路径（`docs/jit.md` 提到它留在编译代码里）。
- **P60**：`perf-js.md` 的搬运与三项数字；探针写法可直接照本文的 N/4N 形式。
