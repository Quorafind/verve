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

（实现者完成后填写）
