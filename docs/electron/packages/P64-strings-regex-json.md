# P64 字符串、正则与 JSON 热路径

**依赖**：P60、P61  **规模**：L  **修改**：`boa/core/string/`、`boa/core/engine/src/builtins/{string,regexp,json}/`、`boa/core/engine/src/string/`（若有）、`boa/SU.md`、对应测试

## 目标

这几类内建是 Electron 应用（模板渲染、Markdown/语法高亮、配置与 IPC 数据、日志）最常走的路，JIT 也帮不上（`perf-baseline.md`：JIT/解释器 1.00×）。
基线（P61 修好超线性之后以 P60 的新数字为准）：

| 项 | interp / Node |
|---|---|
| `JSON.parse` + `stringify`，5.4 MB | 18× |
| 正则 `matchAll` + `replace` | 289×（含超线性，P61 后重测） |
| `split`/`join` | 209×（含超线性，P61 后重测） |
| 字符串 `+=` | 36–55× |

**参考目标**（P60 的剖析出来后由实现者提出、总控确认后写进本节，不确认不开工大改）：JSON ≤ 6×，`regex`/`tokenize`/`splitjoin`/`concat`/`template` 各 ≤ 20×。

## 背景（先读）

- P60 的 `perf-js.md`（剖析配方与示例）与 P61 的「状态」（扫描时留给本包的问题）。
- `builtins/json/mod.rs`：`JSON.parse`（约 238 行）先用 `serde_json` 把整段文本校验一遍，再在前后拼上 `(`、`);` 当成 **JavaScript 脚本**解析、编译、求值。
  一段 5 MB 的 JSON 被完整解析两次，还走一遍字节码编译。`stringify`（约 622 行）按规范对每个属性做通用的 `[[Get]]`。
- `builtins/regexp/mod.rs`：`exec` 的结构（`lastIndex` 读写、结果数组的 `index`/`input`/`groups`）；`boa/SU.md` 里已有的正则编译缓存（`COMPILED`）。
  正则引擎是 crates.io 的 `regress`，Latin-1 输入要拓宽成 UTF-16（P61 已让它每次扫描只拓宽一次）。
- `boa/core/string/`：`JsString` 的 Latin-1 / UTF-16 两种表示、`concat_array`、`slice`。
- `docs/jit.md`「Phase 2, left over」第 4 条：字符串 `+` 与原生调用的成本（JIT 侧由 P66 做，本包做内建本身）。

## 要做的事

先剖析、再定目标、再改；每一项改动都要有剖析或计时证明它值得。下面是已知的方向，不是清单：

### JSON

- `JSON.parse`：一遍解析直接构造 `JsValue`（对象、数组、字符串、数字），不再经 JS 解析器与字节码。`serde_json` 的流式接口或手写的 JSON 解析器均可，
  **不加新依赖**。有 reviver 时需要源码文本（`JSON.parse` source text access），保持现有行为。
  重复出现的键（同一形状的对象数组）复用属性键与形状。
- `JSON.stringify`：普通对象（共享形状、无访问器、无 `toJSON`、无 replacer、无 Proxy）走快路径，直接读槽位；
  字符串转义按块拷贝，不逐字符推入；结果用一个缓冲一次成串。任何条件不满足就退回现在的规范路径。

### 正则

- 未被修改的 `RegExp` 快路径（同 V8 的思路）：正则对象的形状是领域初始的、`RegExp.prototype.exec` 等未被替换时，`replace`/`split`/`matchAll`/`test`
  内部直接调用匹配器，跳过可观察的 `Get(exec)` 与 `lastIndex` 的通用属性读写。判定不满足时走规范路径。
- 结果数组的构造（`index`、`input`、`groups`、捕获组）用预建形状一次填好。
- 评估让 `regress` 直接匹配 Latin-1：若需要改 `regress`，列出改动规模与维护成本，与总控商量后再决定是否 fork；不商量不 fork。

### 字符串

- 剖析 `concat`、`template`、`splitjoin` 的热点：拓宽/收窄转换、每次拼接分配、短字符串、属性键与字符串之间的互转（`'k' + i` 作键）。
- 只做剖析证明有收益的；表示上的大改动（如引入 rope）先写方案（收益估算、内存代价、改动面），总控确认后再做。

## 验收

- P60 套件：参考目标逐项「达标/差距」；`perf-js.md` 追加本包合并后的完整一行。
- 正确性：`cargo test -p boa_engine` 的 json、regexp、string 测试通过；test262 的 `built-ins/{JSON,RegExp,String}` 与 `annexB` 中相关目录前后逐项比较，不得有新失败；
  快路径各有专门测试证明**退回条件**生效（改了 `RegExp.prototype.exec`、带访问器/`toJSON`/Proxy 的对象、reviver、带 `lastIndex` setter 的子类等）。
- `boa/SU.md` 每项改动一条。

## 不做

改变可观察语义；任何只对基准生效的特判；JIT 内的字符串操作（P66）。

## 状态

### 第一阶段（2026-11-03）：分析、参考目标与改动顺序

只有本文这一份 `docs(electron)` 提交；**没有改任何 `boa/` 代码**，等总控确认目标与顺序后再进第二阶段。

### 测量条件

- 解释器：`cargo build --release --bin verve --no-default-features --features wasm,jit`（`23,581,696` 字节），
  解释器列是该二进制的 `VERVE_JIT=0`。Node 22.21.1。
- 吞吐 `bash scripts/js-bench/run.sh --only <test> <exe> node`，每项 5 次取中位；负载（`\Processor(_Total)\% Processor Time`）
  测量前后各 5 次：开工 `17.4/14.9/15/42.4/28.9`（中位 17.4%），复测 `14.9/9.9/13.2/9.4/7.1`（中位 9.9%），
  收尾 `5.4/10.4/7.8/6.5/8.1`（中位 7.8%）。下表是**复测**那一轮（低负载）。
- 规模探针 `run.sh --scaling --only <probe>`，同上 5 次取中位。
- 计时实验：会话 scratchpad 里的小 JS 片段（一个进程内多次取中位）与对照 Node 跑同一片段；**没有用采样剖析器**
  （任务禁止 samply/wpr/xperf）。曾临时在 `json/mod.rs` 加 `Instant` 仪器，测前已 `git status` 确认工作树干净、
  文件已用备份还原，**未提交**。

### 当前数字（P61 之后，本包基线）

吞吐（`median (best)`，ms）：

| test | Node | Verve JIT | Verve interp | JIT / Node | interp / Node |
|---|---|---|---|---|---|
| `json` | 150.2 (141.1) | 2659.9 (2651.0) | 2608.7 (2551.7) | 18× | **17×** |
| `regex` | 1.7 (1.6) | 40.4 (40.0) | 41.4 (38.7) | 24× | **24×** |
| `splitjoin` | 2.7 (2.7) | 12.1 (11.8) | 14.2 (12.6) | 4.5× | **5.3×** |
| `concat` | 0.6 (0.6) | 21.8 (21.4) | 24.3 (21.6) | 36× | **40×** |
| `template` | 0.2 (0.2) | 1.8 (1.8) | 1.0 (0.9) | 9.0× | **5.0×** |
| `tokenize` | 2.5 (2.5) | 259.5 (229.0) | 257.1 (255.5) | 104× | **103×** |

规模探针（P61 修好超线性之后，我这一轮的解释器列）：

| probe | n | t(n) | t(4n) | ratio | verdict |
|---|---|---|---|---|---|
| `split-char-latin1` | 4000 | 0.3 (0.3) | 1.1 (1.0) | 3.7× | linear |
| `split-multi-latin1` | 4000 | 0.3 (0.3) | 1.2 (1.1) | 4.0× | linear |
| `replaceAll-string-latin1` | 4000 | 0.6 (0.6) | 4.7 (4.6) | **7.8×** | suspicious |
| `replaceAll-string-unicode` | 4000 | 0.6 (0.5) | 5.5 (4.6) | **9.2×** | **super-linear** |
| `replace-regex-latin1` | 5000 | 12.7 (11.7) | 50.2 (48.6) | 4.0× | linear |
| `replaceAll-regex-latin1` | 5000 | 12.3 (12.1) | 52.5 (51.6) | 4.3× | linear |
| `matchAll-latin1` | 4000 | 14.8 (14.5) | 55.7 (53.6) | 3.8× | linear |
| `exec-loop-latin1` | 6000 | 13.7 (13.3) | 50.0 (49.6) | 3.6× | linear |
| `repeat-latin1` | 1000000 | 11.8 (11.3) | 46.0 (45.6) | 3.9× | linear |
| `padStart-latin1` | 5000000 | 13.9 (13.4) | 53.8 (51.1) | 3.9× | linear |
| `indexOf-latin1` | 25000000 | 39.5 (39.0) | 154.4 (153.5) | 3.9× | linear |
| `lastIndexOf-latin1` | 5000000 | 152.6 (151.4) | 631.5 (615.2) | 4.1× | linear |
| `concat` | 20000 | 21.8 (21.4) | 395.3 (350.7) | **18×** | **super-linear** |
| `json-stringify` | 20000 | 68.0 (66.8) | 271.9 (270.8) | 4.0× | linear |
| `json-stringify-longstring` | 3000000 | 10.6 (10.2) | 42.0 (41.4) | 4.0× | linear |
| `json-parse` | 20000 | 187.9 (156.1) | 779.2 (734.2) | 4.1× | linear |

**与 P60 §2 的表对照，P61 的修复都成立**：`split-char` 从 16× 到 3.7×，`matchAll-latin1` 从 13× 到 3.8×，
`exec-loop-latin1` 从 17× 到 3.6×，`replace-regex-latin1` 从 17× 到 4.0×。P60 的 `regex`（280×）与
`splitjoin`（186×）也一起降到 24× 与 5.3× —— 那两个吞吐项的时间原本几乎全是超线性。**剩下两项仍是超线性**
（见下）。

### 每一项慢在哪里（计时证据）

#### 1. `tokenize` 103×，是最值得做的一项

`tokenize` 跑 102,228 个 token，解释器 257 ms，即 **每 token 2.5 µs**。

拆分实验。`exec` 的每一档**单独一个进程**、进程内 7 次取中位（同一进程里混跑会因堆状态把命中价抬高近一倍，
先量到 426.8 ms 后按此方式重测；下表是重测值）：

| 片段（100,000 次，Verve 解释器 / Node，ms） | Verve | Node |
|---|---|---|
| 空循环 | 2.2 | 0.0 |
| `/[a-z]+/g` **命中**（每次先置 `lastIndex=0`） | 220.6 | 2.3 |
| `/[a-z]+/` **命中**（非 global） | 229.2 | 2.2 |
| `/[0-9]+/g` **未命中** | 66.4 | 1.4 |
| `/[0-9]+/` **未命中** | 43.2 | 1.4 |
| `.test()` 命中 / 未命中 | 226.7 / 75.2 | 1.3 / 1.3 |
| `lastIndex` 写+读 | 11.5 | 0.0 |
| 完整 tokenize（102,228 token） | 247.9 | 2.5 |

**一次命中比一次未命中贵 154–186 ms / 100k = 每次命中 1.5–1.9 µs；而结果数组那几行 JS 本身就是 1.9 µs。**
也就是说命中成本几乎**全部**是构造结果对象，匹配器扫描本身很便宜：

| 按引擎的写法构造结果数组（100,000 次） | Verve | Node |
|---|---|---|
| `new Array(2)` + `index`/`input`/`groups` + `a[0]` | **189.9** | 0.0 |
| `new Array(1)` + 三个命名属性 | 155.9 | 0.0 |
| `new Array(2)` / `new Array(0)` | 56.2 / 54.8 | 0.0 |
| `[undefined, undefined]` 字面量 | 25.6 | 0.0 |
| 数组上写 1 个 / 3 个命名属性 | 53.4 / 114.7 | 0.0 |
| 普通对象写同样 3 个属性 | 79.5 | 0.0 |

`abstract_builtin_exec`（`regexp/mod.rs:1138`）每次命中构造：`Array::array_create(n+1)`、**`indices` 数组与
`match_record` 数组（`hasIndices` 为假时完全没用上，却照样建）**、`groups`，外加 4 次以上
`create_data_property_or_throw`（`index`/`input`/`groups`/`0`）。拆到单价：两个无用的 `new Array`
= 2 × 0.56 µs ≈ **1.1 µs**，其余约 **0.8 µs** 是那些属性写，合计 1.9 µs —— 与直接量到的「等价 JS 构造
结果数组」189.9 ms 完全对上，也与命中溢价 1.5–1.9 µs 对上。三处独立测法给出同一个数。

**两项要更正我先前的判断**：

- `test` 比 `exec` 只贵 10–31 ms / 100k（0.10–0.31 µs）：同一脚本、同一定义的两轮成对测量是
  222.9 / 232.8 与 220.5 / 251.1。它走 `abstract_exec`（`regexp/mod.rs:1094`）的 `Get(exec)` + `Call`，一次 JS 调用按
  `call_js_fn` 的单价（0.135 µs）量级。所以**「跳过 `Get(exec)`」不到 10%**，不是主项——主项是结果对象。
- `lastIndex` 的通用读写 0.09–0.12 µs / 次（四次测量 9–12.1 ms / 100k），同样是小项。

对 `tokenize` 的成本账因此是闭合的：**2.43 µs/token（247.9 ms ÷ 102,228）** 对单独测到的
`exec` 命中循环 **2.21 µs / 次**（同样是 `lastIndex` 写 + `exec` + 判空 + 自增），差额 0.22 µs 是
tokenize 多读一次 `lastIndex`、以及主语在整轮扫描里一直存活。

#### 2. `regex` 24×：命中成本 + 每匹配重新扫

`regex` 测试体拆开（192,279 码元，同一进程内 7 次取中位）：

| 片段 | Verve | Node |
|---|---|---|
| 用 `+=` 建 5,000 行文本 | **9.8** | 0.2 |
| `matchAll` + `replace` 两段扫描 | **30.5** | 1.2 |
| 整个测试体（建 + 扫） | 40.4 | 1.2 |

两段扫描共 30.5 ms、一共 10,000 次命中（两段各 5,000 次）→ **每次命中 3.05 µs**。这个数比上面
`exec` 的干净单价（2.2 µs）高，因为 `(\w+)@(\w+)\.com` 有**两个捕获组**：每次命中多两个数组元素写入，
且 `groups` 为 `undefined` 时 `named_groups` 仍先 `collect` 成 `Vec`。这是同一处「结果对象构造」，只是更大。
`regex` 这项没有走 `@@split`。

`concat`/`+=` 也在这项里：**9.8 ms 中一半以上的时间是把 5,000 行拼起来**（见第 3 项）。

#### 3. `concat` 40×，且**仍是超线性**（探针 18×）

`s += 'ab' + i` 拆开（20,000 次追加，Verve / Node，ms）：

| 片段 | Verve | Node |
|---|---|---|
| `s += 'ab' + i`（基准的形状） | 44.5 | 0.6 |
| `s += i`（只转换数字，不拼内层） | 14.9 | 0.4 |
| `s += 'abcdefg'`（固定 7 码元，无转换） | 29.3 | 0.1 |
| 只做内层 `('ab' + i).length`，不累加 | 2.9 | 0.4 |
| `String(i)` 10 万次 | 14.9 | 1.5 |
| 模板串 `` `a${i}b` `` 10 万次 | 22.9 | 1.3 |

四种规模（一个进程内、每档 5 次取中位）：5,000 / 20,000 / 80,000 / 320,000 次追加 =
**2.2 / 21.7 / 749.3 / 67,238 ms**。20k→80k 是 34.5×（4 倍输入），80k→320k 是 90×。这是 **O(n²)**：
`concat_array` 每次都新分配一个长度等于总长的 `SequenceString` 并拷贝全部已有内容（`JsString::concat_array`
在 `boa/core/string/src/lib.rs:647`，本身是线性的一次性 API，问题在于 **`+=` 每一步都调它**）。
Node 的同样写法是 0.4 / 0.6 / 5.0 / 26.9 ms，超线性来自 rope/cons 串（V8 的写法），不是算法。

`concat_array` 本身不慢：`'abcdefgh' + 'ijklmnop'` 10 万次 4.9 ms（Node 0.1）。**成本是「每一步都复制整个前缀」**。

规模上界也确认过：`replaceAll` 的 `indexOf` 扫描与 `split`+`join` 两半在 4k/16k/64k 三档是线性的
（比值 ~0.9–1.1），所以 `replaceAll-string` 探针的 7.8–9.2× **不是** `replaceAll` 自身的算法问题，
而是 4000 段下 t(n) 只有 0.6 ms 时的计时噪声（P60 也把它记为 suspicious/super-linear，属同一现象）。
**本包不打算动 `replaceAll`。**

#### 4. `json` 17×：两次完整解析 + 字节码 + VM + 通用 `[[Get]]`

7.03 MB、70,000 条记录的同一份文本：

| 片段（Verve / Node，ms，5 次取中位） | Verve | Node |
|---|---|---|
| `JSON.parse(text)` | 1348.9 | 53.2 |
| `eval('(' + text + ')')` —— 同样的 JS 解析+编译+VM，但没有 serde、没有 JSON 语义 | **1220.4** | 49.4 |
| `JSON.stringify(JSON.parse(text))` | 1913.6 | 81.9 |

`JSON.parse` 比 `eval` 同一段文本**只贵 128 ms**：也就是说 serde 的重复校验约占 **10%**，
**90% 在「把 JSON 当 JS 脚本解析 + 编译字节码 + 跑 VM」这条路上**。这否掉了「只把 serde 换成流式解析」的想法——
那条路只能拿回一成。

再拆值/字节/构造（同一进程，ms）：

| 片段 | Verve | Node |
|---|---|---|
| `JSON.parse` 一个 7 MB 字符串字面量 | 98.2 | 6.0 |
| `JSON.stringify` 同一个 7 MB 字符串 | 31.7 | 15.9 |
| 建 70 万条短字符串（`'ab' + i`） | 143.1 | 45.5 |
| `JSON.parse` 那份（7.6 MB，70 万个短串） | 1063.3 | 197.1 |
| `JSON.stringify` 那一份 | 317.9 | 51.7 |
| `JSON.parse` 7.03 MB 记录 | 1451.4 | 114.6 |
| `JSON.stringify` 记录 | 527.8 | 33.4 |
| 不用 JSON 直接建同样的 70,000 条记录 | 286.5 | 8.8 |

关键对照：**不经过 JSON、直接建同样的 70,000 条记录就要 286.5 ms**（Node 8.8）。`JSON.parse` 是 1451 ms。
所以「一遍解析直接构造 `JsValue`」这条路的天花板大约是 **286 ms 级**（还要加上文本扫描），而不是 53 ms——
`parse` 要降到 Node 的 6×（≈320 ms）必须在**建对象本身**上也不输，而建对象在 Verve 上是 286 ms。
`parse` 的字节成本也够高：一个 7 MB 字符串字面量 98.2 ms（Node 6.0，16×），说明**即使只拷字节**也要 14 ms/MB。

`JSON.stringify`（7.03 MB 记录）527.8 ms。拆「读值」的下限：

| 片段 | Verve | Node |
|---|---|---|
| 逐字段读 70k 条记录（91 万次属性读） | 93.1 | 0.9 |
| 同样读、但键是运行期字符串（`r[name]`） | 35.1 | 0.9 |
| `Object.keys` 70k 个对象 | 83.4 | 2.0 |
| `Object.keys` 一个 70k 元素数组 | 16.8 | 1.7 |
| 数字转字符串 10 万次（`i * 1.5`） | 18.3 | 5.4 |
| 往数组推 100 万个码元 | 122.9 | 20.3 |
| 100 码元串 `slice(0, 50)` 7 万次 | 12.5 | 0.2 |

现在 `quote_json_string`（`json/mod.rs:891`）与 `serialize_json_object` 都是**逐码元 `Vec<u16>` push**，
`serialize_json_object` 还对每个属性构造一个 `member: Vec<u16>`，再用 `intersperse`+`flatten`+`collect`
拼第三次；`member` 那 100 万码元的量级就是 122.9 ms 级的操作。属性读取走通用 `[[Get]]`（`holder.get(key)`），
按上面 93 ms/91 万次读的单价，读值一项约 90–100 ms。**stringify 的常数里可回收的部分约 200–250 ms，上限约 2.2×**。

#### 5. `splitjoin` 5.3×、`template` 5.0× —— 已在目标内

`splitjoin` 5.3×、`template` 5.0× 都已低于任务书的 20× 参考值。`splitjoin` 的时间由 `+=`/`join` 建 20,000 段
再 `split` 两半组成；`template` 的 Node 侧只有 0.2 ms，是套件的计时下限（P60 §1 已有同样结论）。
**这两项不做改动**，只随 `concat`/`array join` 的改善顺带变化。

#### 6. `replaceAll-string` 探针：计时噪声，不是算法

见第 3 项末尾：`indexOf` 扫描与 `split`+`join` 两半在 4k/16k/64k 三档都是线性。**不做改动。**

#### 7. 顺带发现的既有语义偏差（不在本包范围，记给总控）

- `JSON.parse('"\\ud800"')`：Node 返回长度 1 的字符串（孤立代理项合法），Verve 抛
  `SyntaxError: unexpected end of hex escape`。来自 `serde_json::from_str::<serde_json::Value>` 的校验步骤
  （`json/mod.rs:250`）——它把 JSON 字符串解成 Rust `String`，孤立代理项进不去。
  改写成一遍解析会**自然修好**它；但那是「改变可观察语义」的方向——是**向 Node 靠拢**。
  本包把它当收益记录，不作为改动目标，第二阶段若触及先跑 test262 `built-ins/JSON` 逐条确认。
- 深度嵌套 200 层：Node 正常返回，Verve 报 `recursion limit exceeded`（serde 的默认 128 层）。
  同上，一遍解析会自然修好。

### 参考目标（最终值，请总控确认）

任务书初稿：JSON ≤ 6×，`regex`/`tokenize`/`splitjoin`/`concat`/`template` 各 ≤ 20×。按上面的单价，我提议：

| 项 | 现在 | 提议目标 | 依据（可回收的部分） |
|---|---|---|---|
| `tokenize` | 103× | **≤ 35×**（≤ 20× 做不到，见下） | 每 token 2.43 µs ≈ 命中 ~2.2 µs（未命中基线 0.66 µs + 结果对象 1.54 µs）；去掉结果对象里的两个无用数组与逐属性写后 ~0.8 µs/token = **33×** |
| `regex` | 24× | **≤ 20×** | 命中 3 µs（两个捕获组，同一处结果对象）；9.8 ms 的文本构建随 `+=` 下降 |
| `json` | 17× | **≤ 9×**（原 ≤ 6× 达不到） | `parse` 下限 ~290 ms（= 建对象本身）+ 扫描；`stringify` 可到 ~2.2×。合计 ≈ 290 + 240 = 530 ms 对 Node 150 ms，**3.5×**；但 `parse` 的字节成本（14 ms/MB，即 98 ms/7 MB）意味着保守取 9× |
| `concat` | 40× | **≤ 20×**，且**探针回到 linear** | 现在是 O(n²)；去掉每步的整段复制即为线性。**此项依赖第 5 项（rope）是否确认** |
| `splitjoin` | 5.3× | **≤ 20×**（已达标） | 不改 |
| `template` | 5.0× | **≤ 20×**（已达标） | 不改 |

`JSON ≤ 6×` 我建议**改为 ≤ 9×**：6× 意味着 `parse` 要 ≤ 900 ms，而建同样对象本身就要 286 ms、
拷 7 MB 字节要 98 ms，`stringify` 一侧还有 90 ms 的通用属性读——把 `parse` 的字节成本压到 Node 水平需要
动 `JsString::from_slice` 的分配路径（`boa/core/string`），那超出「一遍解析」的范围。9× 是「一遍解析 +
构造快路径 + stringify 快路径」能达到的合理值。

**`tokenize` 为什么是 ≤ 35× 而不是初稿的 ≤ 20×**：Node 的同一测试是 2.5 ms / 102,228 token =
**0.0245 µs/token**（V8 把整个扫描编译成了机器码）。而 Verve 上**一次未命中的 `exec`
就要 0.664 µs**（66.4 ms / 100k）——已经是 **27×**。所以只改结果对象不可能到 20×：去掉两个
无用数组（−1.1 µs）后是 1.33 µs/token = 54×，再把属性写与 `Get(exec)` 也去掉才到
~0.8 µs = 33×。再往下就要动**未命中路径**（原生调用与 `Array` 分配），那是 P65 与 VM 的范围。
**建议把初稿的 ≤ 20× 改为 ≤ 35×。**

`concat` 的目标要分开看：**不做 rope 时**，`+=` 每一步仍复制整个前缀，缩小常数最多拿回约一半（20,000 次下
内层转换占 1/3、复制占 2/3），`concat` 从 40× 到约 30×、**探针仍是超线性**；**做 rope 时**才谈得上 ≤ 20×
与回 linear。

### 打算怎么改（按顺序，每项都要前后计时）

**顺序按「收益 ÷ 风险」排**，先做小的、退回条件清楚的：

1. **`abstract_builtin_exec` 去掉无用数组 + 结果对象预建形状**
   - `has_indices == false` 时完全不建 `indices` 数组与 `match_record`（`regexp/mod.rs:1302`、`:1308`、
     `:1311`、`:1385`）——现在无条件建两个数组、写一次属性。
   - 结果数组用预建形状一次填好（`index`/`input`/`groups`/`0..n`），不走 4 次以上
     `create_data_property_or_throw`。
   - `named_groups` 的 `Vec<(&str, Option<Range>)>`（`:1319`）只在真有命名组时才 `collect`。
   - 预计：按上面 `new Array` 与属性写的单价，每次命中去掉约 1.1 µs 的无用数组
     + 约 0.5 µs 的逐属性写，1.5–1.9 µs → 0.5 µs 级；`tokenize` 从 103× 到 **~40×**，`regex` 从 24× 到 ~15×。
   - 退回条件测试：`d` flag（`hasIndices`）、命名捕获组、`lastIndex` 为访问器的子类、
     `exec` 被替换、`Symbol.match`/`@@replace` 被替换。
   - 风险：低（只改内部分配，不改可观察顺序——须用 test262 `built-ins/RegExp` 逐条确认）。

2. **未修改的 `RegExp` 快路径（`test`/`replace`/`matchAll`/`@@split` 内直接调匹配器）**
   - 判定条件：`RegExp.prototype.exec`、`lastIndex` 的 getter/setter、`@@match`/`@@replace`/`@@matchAll`/
     `@@split` 未被替换，且对象不是子类实例（`SpeciesConstructor` 为 `%RegExp%`）。
   - 满足时跳过 `abstract_exec` 的 `Get(exec)` + `Call`，以及 `lastIndex` 的通用属性读写（用槽位）。
   - 预计：**不到 10%**——两轮成对测量 `test` 比 `exec` 贵 0.10–0.31 µs（一次 JS 调用），
     `lastIndex` 读写 0.09–0.12 µs。按此 `tokenize` 从 ~40× 再降到 ~37×。**收益小、判定复杂，建议在第 1 项实测后再决定是否做。**
   - 退回条件测试：改 `RegExp.prototype.exec`、改 `lastIndex` getter/setter、子类、`Symbol.replace` 被替换。
   - 风险：中（可观察语义的判定要严；`exec` 一旦被改必须退回规范路径）。

3. **`JSON.stringify` 快路径**
   - 条件：普通对象（共享形状、无访问器、无 `toJSON`、无 replacer、无 Proxy、无 `space`）、数组同理。
   - 直接读槽位，字符串转义按块拷贝，结果用 `Vec<u8>`/`Vec<u16>` 一次成串（不再逐属性建 `member` 再拼三次）。
   - 预计：527.8 ms → 250–300 ms（2.2×）。
   - 退回条件测试：访问器、`toJSON`、replacer（函数与数组）、`space`、Proxy、循环引用、`rawJSON`。
   - 风险：中（条件判定错就有语义偏差，测试要全）。

4. **`JSON.parse` 一遍解析直接构造 `JsValue`**
   - 手写 JSON 解析器（不加新依赖），直接产出 `JsObject`/数组/`JsString`/数字；不走 JS 解析器与字节码。
   - 有 reviver 时保留现在的源码文本路径（`JsonSourceVisitor`）。
   - 预计：1348.9 → 400–500 ms（含建对象本身的 286 ms）。
   - 退回条件测试：reviver、`source` 访问、孤立代理项、深度嵌套、重复键、`__proto__`、错误消息。
   - 风险：高（错误消息会变——现在冻结的是 serde 的措辞，`json_parse_with_no_args_throws_syntax_error`
     断言 `"expected value at line 1 column 1"`，改解析器后要同时改这条测试并确认 test262 不看措辞）。

5. **`+=` 的线性化（rope / cons string）**
   - 先写方案（收益估算、内存代价、改动面），总控确认后再做——任务书要求表示上的大改动先出方案。
   - 现状：20k→80k 34.5×、80k→320k 90×，O(n²)。线性化后 `concat` 探针回到 linear、`concat` 吞吐应 ≤ 5×。
   - 影响面：`JsString` 新增一种表示（`SliceString` 之外），`concat_array` 的调用点（`vm/opcode/concat`、
     `String::concat`、`repeat`、`replace`/`replaceAll` 的累积）全部要吃，JIT 的字符串 `+` 慢路径同样
     （P66 备注里也提到这一点）。
   - 风险：高（`boa/core/string` 的核心表示，改它要跑 `cargo test -p boa_string` 全量 + test262 `String`）。
   - **若总控不确认 rope，则 `concat` 只做「缩短每次复制的常数」（`concat_array` 一次性分配已是最优，
     能做的是让 `+=` 走一个可增长的缓冲），并把目标改为「探针回 linear」以外的说法。**

**不做**：`splitjoin`、`template`（已达标）；`replaceAll-string` 探针（计时噪声）；`regress` fork
（Latin-1 直扫的收益在 P61 之后已被每扫描一次拓宽吃掉，剩下的 4× 是常数，fork 的维护成本不值）。

### 给总控的决定点

1. **`json` 的参考目标是否接受 ≤ 9×**（原 ≤ 6× 在「建对象本身 286 ms」下不可达）。
2. **第 5 项（`+=` 线性化）是否确认做 rope**，还是先只做前四项。
3. 第 4 项会改变 `JSON.parse` 的错误消息措辞（现在冻结的是 serde 的措辞），是否接受。



### 第二阶段（2026-11-03）：结果

四个 `boa/` 提交（各自单独通过 `cargo fmt --all --check`、`scripts/style-check.sh`、`boa/` 里的 `cargo fmt --all --check`）：
`08c27c26`（结果对象）、`fb918384`（`JSON.stringify`）、最后一个（`JSON.parse` 一遍解析）；rope 方案是单独的文档提交，见
`docs/electron/spikes/p64-rope.md`。每项在 `boa/SU.md` 各有一条。

#### 测量条件

`cargo build --release -p verve --features jit`，**基线是 `docs/electron-plan` 的 `6335a760`**（临时 worktree，
独立 `CARGO_TARGET_DIR`），**改后是本分支**；同一轮里先后各跑一次，每项 `run.sh --runs 5 --only <test> <exe> node`，
Node 22.21.1，`LOCALAPPDATA`/`XDG_*`/`HOME` 指向临时目录。负载（`\Processor(_Total)\% Processor Time`，每次 5 个样本）：

| 测试 | 基线前 | 基线后 | 改后前 | 改后后 |
|---|---|---|---|---|
| tokenize | 10–20% | 9–14% | 10–14% | 19–61% |
| regex | 13–31% | 13–18% | 17–29% | 19–22% |
| json | 15–25% | 9–17% | 11–34% | 11–16% |
| splitjoin | 10–16% | 13–23% | 9–20% | 17–27% |
| template | 13–18% | 13–19% | 15–25% | 14–32% |
| concat | 17–51% | 19–22% | 13–17% | 12–23% |

（`tokenize` 改后运行结束时有一次外部峰值 61%，跑的当中多数样本在 20% 上下。）

#### 每项前后与目标（中位（最好），ms；括号外是 Verve 解释器 `VERVE_JIT=0`，与参考目标同口径）

| 测试 | Node | 基线 JIT / 解释器 | 改后 JIT / 解释器 | 基线 → 改后（解释器 / Node） | 目标 | 结论 |
|---|---|---|---|---|---|---|
| `tokenize` | 2.5 | 229.2 (219.1) / 244.8 (237.4) | 162.7 (154.5) / 174.6 (172.2) | 98× → **70×** | ≤ 35× | **差距**（见下） |
| `regex` | 1.8 | 42.9 (40.2) / 41.8 (40.4) | 32.9 (31.2) / 32.0 (31.4) | 25× → **18×** | ≤ 20× | 达标 |
| `json` | 152.5 | 2682.9 (2641.0) / 2710.2 (2629.6) | 1303.9 (1263.3) / 1238.4 (1225.1) | 16× → **8.1×**（JIT 8.6×） | ≤ 9× | 达标 |
| `splitjoin` | 2.8 | 12.5 (11.7) / 13.6 (12.1) | 12.9 (11.8) / 12.7 (12.4) | 4.5× → 4.5× | 不改 | 不变 |
| `template` | 0.2 | 1.8 (1.7) / 0.9 (0.9) | 1.8 (1.7) / 0.9 (0.9) | 4.5× → 4.5× | 不改 | 不变 |
| `concat` | 0.6 | 22.7 (22.5) / 66.6 (41.6) | 22.0 (21.8) / 78.1 (48.8) | 95× → 130×（JIT 32× → 37×） | 不含 rope | 无变化，见下 |

- **`tokenize` 没到 ≤ 35×**：102,228 个 token，每 token 2.39 → 1.71 µs（解释器）。结果对象的两个无用数组与逐属性写
  去掉后，剩下的是 `exec` 每次调用本身：一次**未命中**的 `exec` 在第一阶段就量到 0.66 µs（27× Node），
  这部分在 `abstract_builtin_exec` 之外（原生调用、`lastIndex` 通用读写、`Array` 分配、主语拓宽）。
  要到 35× 需要另做第 2 项（见下）加上 P65 对对象/数组分配的改动；本包范围内没有再收的东西。
- **`concat` 在两个二进制上都是双峰的**（解释器列的中位与最好差 1.4–1.6×，基线上同样），所以 66.6 对 78.1 是这个双峰的噪声，
  不是回退；本包没动 `+=`，`+=` 的 O(n²) 等 rope（`spikes/p64-rope.md`）。`concat` 的 JIT 列 22.7 → 22.0 在噪声内。
- 最后一次修改（`JSON.parse` 把整数读成 `i32`）之后在最终二进制上复测：`json` 156.1 → JIT 1246.9 / 解释器 1320.8（8.0× / 8.5×，负载 13–18%），
  与上表在噪声内一致（`tokenize`、`regex` 那一轮负载升到 34–40%，读数偏高，未采用；`built-ins/JSON` 的 test262 结果仍与基线逐字节相同）。
- 第一阶段的数字（`tokenize` 103×、`regex` 24×、`json` 17×）是另一轮低负载下的；本节基线是本次同轮重测，
  所以与第一阶段表里的小数不完全一致。

#### 第 2 项没做：未修改 `RegExp` 的快路径

条件是 `Get(exec)` + `lastIndex` 的通用读写占 `tokenize` 的 10% 以上。第 1 项做完后实测：
`exec` 142 ms，`test` 156 ms（100k 次），差 **13.4 ms，占 9.4%**，低于 10%，**按约定不做**。
`test` 经 `abstract_exec` 的 `Get(exec)` + `Call` 比 `exec` 贵的就是这 9.4%；要拿回它得绕开用户可见的 `exec` 查找，
需要证明 `RegExp.prototype.exec` 与 `lastIndex` 未被改动，复杂度与回报不成比例。

#### 观察得到的变化

- **`JSON.parse` 的错误消息改成 V8 的措辞**（原来冻结的是 `serde_json` 的）：`Unexpected token '}', "[1,]" is not valid JSON`、
  `Expected ',' or ']' after array element in JSON at position 4 (line 1 column 5)`、`Unexpected end of JSON input`；
  文本只是 `undefined`、`NaN`、`Infinity` 时整段被引用（`"undefined" is not valid JSON`）。
  `json_parse_with_no_args_throws_syntax_error` 已按此更新（`"undefined" is not valid JSON`）。带 reviver 的 `JSON.parse` 仍用脚本解析器建值（`context.source` 需要原文），但先用同一个读取器校验文本，所以**两条路径的错误措辞相同**（测试 `json_parse_with_a_reviver_throws_the_same_messages`）。
  仍走旧路径（`context.source` 需要原文），措辞因此**仍是旧路径的**（`serde_json` 与 `boa_parser`）：两条路径的错误措辞不同，这是已知的不一致。
- **孤立代理项**（转义的与原始的）现在按码元保留，之前 `String::from_utf16` 会拒绝；与 Node 一致。
- **嵌套深度**：脚本解析器在 128 层放弃，新的读取器用堆上的 `Vec` 记容器，深度只受内存限制；与 Node 一致。
- 新增测试：`json_parse_reads_a_lone_surrogate_as_node_does`、`json_parse_reads_nesting_deeper_than_the_script_parser_allows`、
  `json_parse_reports_errors_the_way_node_does`、`json_parse_answers_the_numbers_and_keys_node_does`、
  `json_stringify_builds_the_same_text_the_spec_path_did`、`match_result_carries_groups_and_indices_as_node_does`。
- **已知问题（留给之后的包，本包不修）**：带 reviver 时，文本里的**原始**孤立代理项仍会失败：`to_std_string()` 要求合法 UTF-16，抛 `SyntaxError`。已测：`JSON.parse('"\ud800"', (k, v) => v)`（转义写法，文本是 ASCII）也仍失败，原因是脚本解析器建字符串字面量时拒绝孤立代理项（`invalid utf-16: lone surrogate found`），不是 `to_std_string()`；无 reviver 的 `JSON.parse` 两种写法都正常。

#### test262（`E:\test262`，`d86b2294`，`boa_tester`，基线 `6335a760` 对改后）

| 范围 | 通过 | 忽略 | 失败 |
|---|---|---|---|
| `built-ins/JSON` | 165 / 165 | 0 | 0 / 0 |
| `built-ins/RegExp` | 1865 / 1865 | 13 / 13 | 1 / 1 |
| `built-ins/String/prototype/match` | 49 / 49 | 2 / 2 | 0 / 0 |
| `.../matchAll` | 25 / 25 | 0 | 0 / 0 |
| `.../replace` | 55 / 55 | 0 | 0 / 0 |
| `.../replaceAll` | 45 / 45 | 0 | 0 / 0 |
| `.../split` | 120 / 120 | 0 | 0 / 0 |
| `.../search` | 43 / 43 | 0 | 0 / 0 |
| `annexB/built-ins/RegExp` | 35 / 35 | 27 / 27 | 0 / 0 |
| `annexB/built-ins/String` | 105 / 105 | 0 | 6 / 6 |

（每格「基线 / 改后」。十个范围的 `results.json` 逐字节相同：**没有新增通过或失败**。`RegExp` 的 1 个与 `annexB/String` 的 6 个失败是基线就有的。）

#### 其他检查

`cargo clippy -p boa_engine -p boa_string --all-targets -- -D warnings`（在 `boa/` 里）、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo clippy -p verve --features jit --all-targets -- -D warnings` 全部通过。`boa_engine --features jit` 的 clippy 有 5 条
**基线就有**的告警，全在没改过的 `vm/jit/{mod,compiler,tests}.rs`（未使用的导入、`format!` 追加、`is_multiple_of`），不属于本包。
接手时对 `JSON.parse` 提交做了两处 fixup：`end_of_input(&self)` 触发 `clippy::unused_self`；整数文本（如 `123`）被读成 `f64` 而不是 `i32`，使 `tests/module.rs` 的 `test_json_module_from_str`、`test_json_module_dynamic_import` 失败（`to_json` 输出 `123.0`），现在能放进 `i32` 的整数按整数读（`-0` 与带小数点/指数的仍是浮点），并加了测试 `json_parse_reads_a_whole_number_as_an_integer_value`。

#### 给其他包的备注

- **给 P65（对象与分配）**：不经过 JSON，**直接用 JS 建 70,000 条记录就要 286.5 ms，Node 8.8 ms**。`json` 的 8.1× 里
  这一块是下限（解析本身现在是 ~450–530 ms 全程，其中建对象占一半以上）。`tokenize` 的剩余 1.7 µs/token 同样是
  对象/数组分配与一次未命中的 `exec` 调用（0.66 µs），P65 改好后 `json`、`tokenize` 都会顺带下降。
- **给 P66（JIT 字符串路径）**：JIT 对字符串 `+` 没有内联路径，慢路径经 `jit_step` 调解释器同一个 `Add` 处理函数
  （最终 `JsValue::add`），所以字符串 `+` 只有一份实现；`vm/jit/` 里读字符串的只有 `GetLengthProperty` 读 `len`。
  给字符串 `+` 加内联路径应调用 Rust 帮助函数（rope 落地后是 `concat_lazy`），不要在机器码里自己拷贝；
  不要与 rope 同时改 `JsValue::add`。`template` 的 JIT 列（1.8 ms）比解释器列（0.9 ms）慢一倍，是 P66 值得看的现象。
- **rope 方案**：`docs/electron/spikes/p64-rope.md`，待总控确认后决定在本包还是单列一个包；约 4–5 个工作日，
  `boa/core/string` 的核心表示改动，风险点是递归释放/展平的栈安全与 `UnsafeCell` 展平缓存。
