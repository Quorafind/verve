# P68 评审积压的三处语义修正

**依赖**：P64、P65、P67（均已合并）  **规模**：S  **修改**：`boa/core/engine/src/vm/opcode/set/`、`boa/core/engine/src/builtins/json/`、
`src/modules.rs`、`src/esm_resolve.rs`、`src/esm.rs`（只限模块解析的错误路径）、`src/permission.rs`（只限解析期的读权限判断）、对应测试、`boa/SU.md`

## 目标

P64、P65、P67 的评审各留下一处与 Node 不一致的行为，本包把三处都修掉。三项互不依赖，**各自独立 commit**，每项先写一个失败的测试再改。
本包只修语义，不做性能；三项都不得让 P60 套件变慢（合并前跑一遍 `run.sh --runs 5`，与 `docs/electron-plan` 同一轮比较）。

## 背景（先读）

- `P65-objects-typed-arrays.md`「状态」末尾「评审后的两处修正」：类型化数组的写入快捷路径只在接收者就是对象本身时生效。
- `P66-jit-coverage.md`「状态」第 2 步的「Receiver bug」：JIT 的内联写入已按同样的规则修好（`07edaec5`），**解释器上游的稠密数组快路径还没修**。
- `P64-strings-regex-json.md`「状态」第 7 项与第二阶段末尾的「已知问题」：带 reviver 的 `JSON.parse` 对孤立代理项仍抛错。
- `P67-code-cache-default.md`「状态」「给其他包的备注」第 1 条：裸说明符的 `require` 把权限拒绝报成「找不到模块」。
- `src/permission.rs` 的 `Resolving` 与 `resolution_may_read`（解析期读 `package.json` 的规则）。

## 要做的事

### 1. `super[k] = v` 写进了原型（解释器）

`vm/opcode/set/property.rs` 的 `SetPropertyByValue::operation`：「Fast Path」里 `object.is_array()` 的那一段直接对 `object` 调
`set_dense_property`，**没有看 `receiver`**。`super[k] = v` 的 `object` 是 home object 的原型，`receiver` 是 `this`，按规范
（`OrdinarySet` → 在接收者上定义）值应该落在 `this` 上。复现（实现者先确认在当前 `docs/electron-plan` 上确实错）：

```js
const proto = [0];
const o = { __proto__: proto, m() { super[0] = 5; } };
o.m();
[Object.hasOwn(o, '0'), proto[0]]   // 规范与 Node：[true, 0]
```

- 修法与类型化数组那段一致：只有 `receiver` 与 `object` 是同一个对象时才走稠密快路径。
- 测试：上面的对象字面量、`class B extends Array { f() { super[0] = 1 } }`、在界内与追加两种下标；严格模式与非严格模式；
  JIT 的 `stores_through_super_take_the_receiver` 照旧通过。
- `SetPropertyByName` 与其他按值写入的处理函数顺手查一遍有没有同样的问题，有就一并修、写进「状态」。

### 2. 带 reviver 的 `JSON.parse` 丢掉孤立代理项

P64 让无 reviver 的 `JSON.parse` 改用一遍读取器，孤立代理项（转义的 `"\ud800"` 与原始的）都按码元保留，与 Node 一致。
带 reviver 时仍用脚本解析器建值（`context.source` 需要原文），两种写法都抛 `SyntaxError`（原因见 P64「已知问题」：
脚本解析器建字符串字面量时拒绝孤立代理项）。

- 目标：`JSON.parse('"\\ud800"', (k, v) => v)` 与原始孤立代理项的写法都返回长度 1 的字符串；reviver 的 `context.source`
  照旧是原文。
- 做法由实现者定，倾向：**值用 P64 的读取器建**，`source` 由读取器记下的原文区间取，这样两条路径只剩一个解析器。
  若改脚本解析器更小，先说明它是否影响普通源码里的字符串字面量（规范要求那里拒绝的情形不能放开）。
- 测试：两种孤立代理项写法、高低代理项成对与颠倒、`source` 访问、嵌套对象与数组里的孤立代理项。
- test262 `built-ins/JSON` 前后逐项 **0 回退**。

### 3. `require('pkg')` 把 `ERR_ACCESS_DENIED` 报成 `MODULE_NOT_FOUND`

不带 `-A` 的 `verve run` 里，`require('typescript')`（`node_modules` 在工作目录之外）报 `Cannot find module 'typescript'`
（`code: 'MODULE_NOT_FOUND'`）；同一目录下的 `require('./hi.cjs')` 却正确地报 `ERR_ACCESS_DENIED` + `permission: 'FileSystemRead'`。

- 先找出原因并写进「状态」：是 `Resolving` 期间不可读的 `package.json` 被当作不存在、ESM 解析器（`esm_resolve.rs`）的探测把拒绝当作没找到，
  还是 `modules.rs` 的 `resolve` 把解析器的错误统一映射成了 `not_found`。
- 目标：解析找到的候选（包目录、`package.json`、入口文件）若存在但当前 realm 不能读，抛 `ERR_ACCESS_DENIED`，字段与
  `require('./hi.cjs')` 的拒绝相同（`permission`、`resource`）；真正不存在时仍是 `MODULE_NOT_FOUND`。
  以 Node 22 `--permission` 的行为为准，拿不准时在「状态」写出依据。
- ESM 的 `import 'pkg'` 走同一个解析器，一并检查；它的不存在错误码是 `ERR_MODULE_NOT_FOUND`，拒绝同样应是 `ERR_ACCESS_DENIED`。
- `Resolving` 的现有约定（解析期间不读不可读的 `package.json`，加载时再拒绝）若因此要改，说明改了什么；`tests/permission.rs` 不得回退。
- 测试：裸说明符、带子路径的说明符（`pkg/lib/x`）、`exports` 映射的包、真正不存在的包，各在授权与不授权两种 realm 下。

## 验收

- 三个 commit 各带测试，测试在修复前失败、修复后通过（「状态」里贴修复前的失败输出）。
- `boa/` 内：`cargo test -p boa_engine`（默认与 `BOA_JIT_THRESHOLD=0`）、`cargo clippy -p boa_engine --all-targets --features jit -- -D warnings`。
- 根：`cargo test -p verve`（含 `tests/permission.rs`）、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check`、
  `bash scripts/style-check.sh`。
- test262：`built-ins/JSON`、`language/expressions/super`、`language/statements/class` 前后逐项 0 回退。
- P60 套件不变慢（同一轮比较）。

## 不做

上游 Boa 的其他已知偏差；模块解析的性能；JSON 的其他行为（P64 已覆盖）。

## 状态

未开始。
