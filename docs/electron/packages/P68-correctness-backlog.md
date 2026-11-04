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

三项都已完成，各一个 commit（`2b266cc3`、`cc1c263a`、`fb9a40be`），每个都先写了会失败的测试。分支 `electron/p68-correctness-backlog`，基线 `05f26157`。
两处与任务书措辞不同的地方在第 3 项（见下），其余按任务书。

### 1. `super[k] = v` 写进了原型（`2b266cc3`）

- **原因**：与任务书所述一致。`SetPropertyByValue::operation` 的「Fast Path」只看 `object.is_array()`，不看 `receiver`；`super[k] = v` 的
  `object` 是 home object 的原型、`receiver` 是 `this`，稠密数组快路径把值写进了原型数组。当前基线上确认存在：
  `{ __proto__: [0], m() { super[0] = 5 } }.m()` 之后原型的 `[0]` 变成 5、`this` 没有自有的 `'0'`。
- **修复**：把稠密数组与类型化数组两段快路径放进同一个条件——`receiver` 与 `object` 是同一个对象才走（类型化数组那段原来自己带着这个条件，
  现在两段共用一个，数组那段不再多一次 `as_object`：只在下标键时才取接收者）。
- **`SetPropertyByName` 与其他处理函数**：`SetPropertyByName`/`SetPropertyByNameWithThis`（`set_by_name`）查过，没有同样的问题——
  `super.x = v` 反复执行（内联缓存被查询）仍然每次定义在接收者上；只加了回归测试。`SetPropertyGetter/Setter*`、`SetFunctionName`
  是定义式的，不经 `[[Set]]`。`PropertyMap::set_dense_property` 的其余调用方是 `builtins/array`（`push` 等，接收者就是数组）与 JIT 的
  帮助函数（`07edaec5` 已修）。
- **测试**（`boa/core/engine/src/tests/class.rs`）：`super_store_of_an_index_defines_on_the_receiver_not_the_dense_array`（对象字面量、
  `class B extends Array`、界内与追加两种下标、严格与非严格各一遍）、`super_store_of_a_name_defines_on_the_receiver_every_time`。
  JIT 的 `stores_through_super_take_the_receiver` 照旧通过。
- **修复前的失败输出**：

  ```
  tests::class::super_store_of_an_index_defines_on_the_receiver_not_the_dense_array --- FAILED
  panicked at core\engine\src\tests\class.rs:245:9:
  Test case 1:  Object.hasOwn(o, '0') && Object.hasOwn(o, '1')
  ```
- `boa/SU.md` 已记一条。

### 2. 带 reviver 的 `JSON.parse` 丢掉孤立代理项（`fb9a40be`）

- **做法**：按任务书倾向——**值用 P64 的读取器建，`source` 取读取器记下的原文**，`JSON.parse` 不再用脚本解析器、字节码编译器和 VM。
  `Reader::read_with_nodes` 在建值的同时记一棵 `JsonNode` 树：原语节点存原文（读取器输入的一段码元，`JsString`）和读出的值，
  容器节点存成员（对象的键是 `JsString`，原来是有损的 `String`）。reviver 只有在当前值与读出的值 `SameValue` 时才得到 `source`
  （原来是再解析一遍原文来比较）。删掉了 `JsonSourceVisitor` 与旧的 `JsonNode`、`serde_json` 比较、一遍多余的校验读取。
  脚本解析器对**普通源码**字符串字面量的拒绝没有动（本包不碰它）。
- **多出来的一处**：读取器的嵌套只受内存限制，而 reviver 的 `InternalizeJSONProperty` 是原生栈递归；脚本解析器原来的 128 层上限等于替它挡着。
  现在深到 10 万层再带 reviver，测试线程会栈溢出（实测：`STATUS_STACK_OVERFLOW`）。所以 `internalize_json_property` 入口按
  `RuntimeLimits::native_stack_limit` 检查原生栈位置，超出抛 `RangeError: Maximum call stack size exceeded`（V8 同样）；
  `JsonNode` 的 `Drop` 改成堆上迭代，否则丢弃一棵深树又会递归。1000 层正常、100 万层抛 `RangeError`。
- **测试**（`boa/core/engine/src/builtins/json/tests.rs`）：`json_parse_with_a_reviver_keeps_a_lone_surrogate`（转义与原始两种写法、孤立的高/低代理项、
  成对与颠倒、嵌套对象与数组里的值和键、`source` 对孤立代理项/`1.50`/`-0`/`true`/`null`、reviver 改写后 `source` 消失、重复键取最后一个的 `source`）、
  `json_parse_with_a_reviver_reads_nesting_and_gives_up_before_the_stack_does`。
- **修复前的失败输出**（转义写法在这条路径上同样失败，输出里先停在第一个用例）：

  ```
  Test case 1:
      JSON.parse('"\ud800"', (k, v) => v).length === 1
  Uncaught SyntaxError: invalid utf-16: lone surrogate found (unknown at :1:11)
  ```
- test262 `built-ins/JSON`：165 / 165 通过，0 失败（见下）。`boa/SU.md` 已记一条，并改了 P64 那条里「reviver 仍走脚本解析器」的句子。

### 3. `require('pkg')` 把 `ERR_ACCESS_DENIED` 报成 `MODULE_NOT_FOUND`（`cc1c263a`）

- **原因（先查出来的）**：是任务书列的第一种与第二种合在一起，不是 `modules.rs` 的统一映射。`esm_resolve::resolve` 在 `node_modules/<包>` 里调
  `manifest()` 读 `package.json`，而 `manifest()` 在 `Resolving` 期间对不可读的文件 `resolution_may_read` 为假就返回 `Ok(None)`（当作没有）。
  于是 `typescript` 这种靠 `main` 指到 `lib/typescript.js` 的包既没有 `exports` 也没有 `main`，再试 `index` 也找不到，落到
  `ERR_MODULE_NOT_FOUND`；`modules.rs::resolve` 又把这个码映射成 `MODULE_NOT_FOUND`（这一步本身没错，它只处理 `ERR_MODULE_NOT_FOUND`）。
  没有 `package.json` 的包靠 `index.js` 能找到，加载时才被拒，所以那种情形早就是 `ERR_ACCESS_DENIED`。
- **Node 22 `--permission` 的实际行为**（本机 Node 22.21.1，`--allow-fs-read=<cwd>`，`node_modules` 在 cwd 之外，实测而非推断）：
  - `require('typescript')`、`require('typescript/lib/typescript.js')`：`ERR_ACCESS_DENIED`，`permission: 'FileSystemRead'`，
    `resource` 是 `…\node_modules\typescript\package.json`；
  - **`require('nonesuch')`、`require('@s/n')`（这个包哪里都没有）也是 `ERR_ACCESS_DENIED`**，`resource` 是第一个读不了的候选
    `…\node_modules\nonesuch\package.json`：CJS 的查找不先看目录在不在，直接读候选的 `package.json`，权限检查先于存在检查；
  - `import('typescript')`：`ERR_ACCESS_DENIED`，同一个 `resource`；`import('nonesuch')`：`ERR_MODULE_NOT_FOUND`（ESM 先看目录在不在）；
  - 授权覆盖整个查找路径时，缺失的包仍是 `MODULE_NOT_FOUND` / `ERR_MODULE_NOT_FOUND`；`require('./missing.cjs')` 始终是 `MODULE_NOT_FOUND`。
- **与任务书措辞的出入**：任务书写「真正不存在时仍是 `MODULE_NOT_FOUND`」。按 Node 实测，**裸说明符的 `require` 在查找路径上遇到读不了的
  `node_modules/<名>/package.json` 时，不管包存不存在都抛 `ERR_ACCESS_DENIED`**；我按 Node 做了（任务书说以 Node 为准）。
  代价：不带 `-A` 的 `verve run` 里，`try { require('可选包') } catch (e) { if (e.code !== 'MODULE_NOT_FOUND') throw e }` 这类写法，
  在可选包缺失而查找路径越出授权目录时会抛 `ERR_ACCESS_DENIED`——和 Node `--permission` 一样。若总控想要「不存在就是 `MODULE_NOT_FOUND`」，
  只需去掉 `esm_resolve.rs` 里 `Mode::Require` 的那一次 `ensure_package_readable`（`import` 路径与存在的包不受影响）。
- **修复**：`ResolveError` 多一个 `resource: Option<PathBuf>`；`esm_resolve::ensure_package_readable(root)` 在 `node_modules/<包>` 的查找里，
  `require`（`Mode::Require`）对每个候选、`import` 对目录存在的候选，若 `package.json` 当前 realm 不能读，就以 `ERR_ACCESS_DENIED` 结束解析。
  `esm::resolve_error`（改为 `pub(crate)`，`modules.rs` 也用它）把带 `resource` 的错误交给 `permission::denied(context, "FileSystemRead", resource)`，
  字段与 `require('./hi.cjs')` 的拒绝相同。`modules.rs::resolve` 的 `ERR_MODULE_NOT_FOUND` → `MODULE_NOT_FOUND` 映射不变，不会误伤新的码。
- **`Resolving` 的约定**：只改了一处例外——裸说明符查找的那个 `package.json`。其余照旧：`scope()`（模块自己的 `type` 查找、`#imports`、自引用）
  与 CJS 的相对路径 `as_file_or_folder` 仍是「不读不可读的 `package.json`，加载时再拒绝」。`permission.rs` 里 `Resolving` 的文档注释已写明例外。
  `tests/permission.rs` 原有测试全部通过。
- **实际命令**（临时目录，`HOME` 等指向 scratchpad，release 构建）：不带 `-A` 的 `verve run app.cjs`（工作目录没有任何授权，`./hi.cjs` 本来就是 `ERR_ACCESS_DENIED`）里，
  `require('typescript')`、`require('typescript/lib/typescript.js')`、`require('nonesuch/sub')`、`require('@s/n')` 都是 `ERR_ACCESS_DENIED FileSystemRead`，
  `resource` 为第一个候选的 `node_modules/<名>/package.json`；`import('typescript')` 是 `ERR_ACCESS_DENIED`，`import('nonesuch')` 是 `ERR_MODULE_NOT_FOUND`，
  `require('./missing.cjs')` 是 `MODULE_NOT_FOUND`。带 `-A`：`typescript` 加载，缺失的是 `MODULE_NOT_FOUND`/`ERR_MODULE_NOT_FOUND`。逐项与上面的 Node 实测一致。
- **测试**（`tests/permission.rs`）：`a_package_outside_the_grant_is_refused_not_missing`（裸、`deep/lib/x`、`exports` 映射、缺失包；`require` 与 `import` 各一遍，
  不授权的 realm）、`a_package_inside_the_grant_loads_and_a_missing_one_is_missing`（`FsAccess::All`：同样的说明符都能加载，缺失的是 `MODULE_NOT_FOUND`/`ERR_MODULE_NOT_FOUND`）、
  `a_package_in_a_readable_folder_is_found_before_a_folder_that_is_not`（近处可读的 `node_modules` 先命中，上面不可读的不被探测）。
- **修复前的失败输出**（第一版测试；当时缺失的包期望 `MODULE_NOT_FOUND`，与最终按 Node 的期望只差这一项）：

  ```
  a_package_outside_the_grant_is_refused_not_missing --- FAILED
    left: ["MODULE_NOT_FOUND::", "ERR_ACCESS_DENIED:FileSystemRead:<root>/node_modules/deep/lib/x.js", "MODULE_NOT_FOUND::", "MODULE_NOT_FOUND::", "ERR_MODULE_NOT_FOUND::", "ERR_MODULE_NOT_FOUND::", "ERR_MODULE_NOT_FOUND::"]
   right: ["ERR_ACCESS_DENIED:FileSystemRead:<root>/node_modules/plain/package.json", "ERR_ACCESS_DENIED:…/deep/package.json", "ERR_ACCESS_DENIED:…/mapped/package.json", "MODULE_NOT_FOUND::", "ERR_ACCESS_DENIED:…/plain/package.json", "ERR_ACCESS_DENIED:…/mapped/package.json", "ERR_MODULE_NOT_FOUND::"]
  ```

### 验证

- `boa/`：`cargo test -p boa_engine`（默认；`--features jit` 的默认与 `BOA_JIT_THRESHOLD=0`）全部通过：lib 1243 个（`jit` 特性）/ 1199 个，另有 doctest 与集成测试，0 失败；
  `cargo clippy -p boa_engine --all-targets --features jit -- -D warnings` 通过；`cargo fmt --all --check` 通过。
- 根：`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test -p verve`（含 `tests/permission.rs` 13 个）、`cargo fmt --all --check`、
  `bash scripts/style-check.sh` 全部通过（`VERVE_CODE_CACHE=0`）。
- test262（`E:\test262`，`d86b2294`，`boa_tester` release，`-O`）改后：

  | 范围 | 通过 | 忽略 | 失败 |
  |---|---|---|---|
  | `built-ins/JSON` | 165 | 0 | 0 |
  | `language/expressions/super` | 94 | 0 | 0 |
  | `language/statements/class` | 4353 | 14 | 0 |

  三个范围改后都是 0 失败（0 panic），所以相对任何基线都不可能有回退；没有另外编译基线的 `boa_tester` 来逐项比较（`built-ins/JSON` 的基线 165/165 见 P64「状态」）。
  忽略数由 `test262_config.toml` 决定，本包没有改它。
- P60 套件（`bash scripts/js-bench/run.sh --runs 5 <exe> node`，release、`--features jit`，旧 = `05f26157` 的源码、新 = 本分支，同一台机器同一会话，
  顺序旧 A → 新 A → 新 B → 旧 B 抵消漂移）。负载（`Get-Counter`，每次 5 个采样）：开始前 41–70%，旧 A 后 14–19%，新 A 后 20–36%，新 B 后 14–24%，旧 B 后 53–64%。
  JIT 列中位（ms，旧 A / 新 A / 新 B / 旧 B）：`fib` 258.6 / 243.7 / 243.2 / 241.1，`alloc` 274.3 / 262.6 / 256.4 / 281.0，`json` 1426.5 / 1250.6 / 1299.4 / 1264.7，
  `sort` 2413.9 / 2323.7 / 2410.6 / 2611.2，`oo` 1121.4 / 1074.5 / 1174.7 / 1223.5，`typed` 634.5 / 616.5 / 633.5 / 659.8，`closure` 430.2 / 442.2 / 447.5 / 461.2，
  其余行在噪声内。**没有一行新比旧慢超出同一二进制两轮之间的差别**（两个二进制各自的两轮互差 5–10%，`json` 新的两轮都不比旧的任何一轮慢）。
  `SetPropertyByValue` 的改动只是把「接收者是对象本身」的判断提到数组/类型化数组两段共用，且仅在下标键时才取接收者。原始输出在会话 scratchpad（`p68/bench.txt`）。

### 没做 / 注意

- 没有编译基线 `boa_tester` 做逐项 test262 对比（理由见上）。
- `JSON.parse` 带 reviver 的**错误信息**现在与不带 reviver 完全相同（V8 措辞），P64 记的「两条路径措辞不同」不再成立。
- 带 reviver 的 `JSON.parse` 在深嵌套下现在抛 `RangeError` 而不是原来 128 层处的 `SyntaxError`（后者与 Node 不一致）。

### 给其他包的备注

- **给任何改模块解析的包**：`esm_resolve::resolve` 对 `require` 与 `import` 的存在性检查顺序不同（CJS 先查权限再看目录，ESM 先看目录），与 Node 对应；`ResolveError.resource` 是新增字段，
  转成 JS 错误一律走 `esm::resolve_error`，不要再直接 `coded_error(&error.message, error.code)`，否则 `ERR_ACCESS_DENIED` 会丢掉 `permission`/`resource`。
- **给以后动 `SetPropertyByValue` 快路径的包**：凡是「直接写 `object`」的捷径都要先确认 `receiver` 就是 `object`；新增的测试在 `boa/core/engine/src/tests/class.rs`。
- **给读 `context.source` 的代码**：`JsonNode::Primitive` 现在带读出的值，`source` 的条件是 `SameValue`，不再是文本重新解析后相等。
