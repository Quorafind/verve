# P62 CommonJS 进代码缓存

**依赖**：无  **规模**：M  **修改**：`src/modules.rs`、`src/code_cache.rs`、`src/esm.rs`（只限把格式判定拆出来）、
新建 `src/code_cache_load.rs`（loader 两个入口，见「状态」：`style-check` 的文件长度棘轮不允许
`src/code_cache.rs` 再变长）、`src/lib.rs`（只限 `CodeCacheStats`、模块声明与 `load_module` 的格式判定）、`src/cli.rs`（只限
`start` 的格式判定）、`tests/code_cache.rs`、
`docs/startup.md`、`boa/core/engine/src/code_cache.rs`、`boa/core/engine/src/script.rs`、`boa/core/engine/src/tests/code_cache.rs`、
`boa/SU.md`

本节原是 P42 的第 2 节，因为基线测得它是最大的单项差距之一，提前到性能线做。P42 只负责在 Electron 里**接入**缓存（目录、generation、flush 时机）。

## 目标

Electron 应用的主进程产物大多是 CommonJS。现在 `require` 一个大 CJS 包不走代码缓存：

| `typescript.js` 5.9.3（9.1 MB） | 时间中位 | 内存 |
|---|---|---|
| Node 22，CJS | 118 ms | 58.5 MB |
| Verve，CJS `require` | 2,051 ms（17×） | 228 MB |
| Verve，同一源码当 ES 模块，暖缓存 | 176 ms（1.5×） | 73 MB |

目标：CJS 暖缓存下接近 ES 模块暖缓存的数字（时间 ≤ Node 的 2 倍，内存 ≤ 100 MB），并解释 CJS 无缓存时内存比 ES 模块高约 100 MB 的原因。

## 背景（先读）

- `docs/startup.md`：代码缓存的设计、格式、信任模型；「What is left」说明 CJS 为什么没缓存（经典脚本的全局声明在编译时创建，缓存后要回放）。
- `src/code_cache.rs`：pack 格式、键（构建指纹 + 全局作用域 + 源码全文）、`flush`/`flush_now`、`CodeCacheStats`。
- `src/modules.rs` 约 430–470 行：CJS 模块用 Node 的包装器拼成 `(function (exports, require, module, __filename, __dirname) {…})`
  后 `context.eval` 得到包装函数。这个脚本**不声明任何全局绑定**，所以可以像 ES 模块一样缓存。
- `src/cli.rs`：`--code-cache=<dir>` / `VERVE_CODE_CACHE`；`src/lib.rs` 的 `set_code_cache_dir_with_user_secret`、`code_cache_stats`。
- [`perf-baseline.md`](../perf-baseline.md) 第 4、5 节：测量数字与方法。

## 接口契约

- CJS 包装脚本走与 ES 模块相同的缓存路径：键 = 构建指纹 + 全局作用域 + **包装后的完整源码**；读回后经 Boa 的校验再运行。
- 前提检查：包装脚本的全局声明集合必须为空（实现里断言；不为空时不缓存并计数），理由与 `docs/startup.md` 拒绝缓存经典脚本一致。
- `CodeCacheStats` 增加 `cjs_hits`/`cjs_misses`（或按类型分组），`docs/startup.md` 更新相应段落。
- `--code-cache`/`VERVE_CODE_CACHE` 对 CJS 同样生效，`verve run` 无需新开关。
- `.json` 模块与原生模块不进缓存。经典 `<script>`（渲染器）不在本包范围。

## 步骤

1. 先测清楚：`typescript.js` 以 CJS 加载时时间与内存花在哪（解析、编译、求值；P60 的剖析配方）；与 ES 模块无缓存的 946 ms / 133 MB 对比，
   找出 CJS 多出的约 1.1 s 与 100 MB 来自哪里（例如 `context.eval` 走的路径、源码在包装时多拷贝一次、`eval` 代码的作用域分析）。能顺手修的修掉。
2. 实现缓存，写测试。
3. 用基线的方法重测第 4、5 节的 CJS 行，写进 `docs/startup.md` 的 Results 与 `perf-js.md`。

## 验收

- `tests/code_cache.rs` 新增：CJS 第二次加载命中；源码改动后不命中；pack 损坏时安全回退为编译；包装后有全局声明（构造一个这样的输入或用测试钩子）时不缓存并计数；
  `require` 循环依赖与 `module.exports` 被替换的模块在缓存命中时行为不变。
- `cargo test --test code_cache` 与 `cargo test --test modules`（或现有的 CJS 测试文件）通过。
- release `verve` 用 `--code-cache` 第二次 `require('typescript')`：时间中位 ≤ 236 ms（Node 的 2 倍），最大工作集 ≤ 100 MB；数字写进报告。

## 不做

经典脚本缓存；懒解析；Electron 里的缓存目录与 flush 时机（P42 第 1 节）。

## 状态

完成了：

- **Boa 侧：经典脚本可写可读。** `Script::code_cache(context)` 编译脚本（含它声明的全部函数）并返回字节；
  `Script::from_code_cache(bytes, source, path, realm, context)` 读回，自己的 codeblock 立刻造，其余函数首次
  调用时造。字节格式升到 3 并多一个 kind 字节（module/script），模块的字节不会被当脚本读。
  `read_header`/`read_scopes`/`read_codes` 与模块共用；`BOA_CODE_CACHE_ROUNDTRIP=1` 让文件里解析的脚本走一遍缓存。
- **只缓存不声明全局的脚本。** `write_script` 在 `global_lexs`/`global_fns`/`global_vars` 任一非空时返回
  `None`：脚本的全局声明是解析时创建的，缓存后要回放。Node 的包装器不声明，所以 CJS 能缓存；源码里用
  `});` 提前闭合包装器再声明的，不缓存，每次编译，并计入 `cjs_skipped`。
- **Annex B 块级函数：不缓存。** 非严格脚本里顶层 `{ function f(){} }` 会在**编译时**经
  `global_declaration_instantiation_context` 建一个不可配置的全局 var 绑定，却不出现在
  `global_lexs`/`global_fns`/`global_vars` 里。命中的代码不建它，`f` 就成了 `SetName` 造的可配置属性
  （`delete f` 成功、描述符不同）。现在 `Script::code_cache` 在 `annex_b_binding` 置位时返回 `None`；
  该位由 `compile` 从 `annex_b_function_names` 非空得出，非 annex-b 构建下恒为假。
- **Verve 侧：CJS 走同一条缓存路径。** 键 = 构建指纹 + 全局作用域 + 包装后的完整源码 + kind；
  新模块 `src/code_cache_load.rs` 放 loader 的两个入口（`module`、`script`），`src/code_cache.rs` 只留
  pack 格式、键、签名、`Kind` 的键域与读写。`CodeCacheStats` 增加 `cjs_hits`/`cjs_misses`/`cjs_refused`/`cjs_stored`/`cjs_skipped`。
- **格式判定的 1.1 s 与约 100 MB 已消除，入口与 `import` 同样。** 原来 `is_esm` 为判格式把 9 MB 包装源码
  完整 parse 一遍再丢弃，`run_module` 再 parse 一次。现在 `modules::format` 一个判定：`named_format`
  （扩展名 / package.json `type`）说了就算，没说就按脚本缓存加载包装器，成功即 CJS 并直接运行它；失败才
  退回去判是否模块。`load_file`、`run` 的入口、`Runtime::load_module`
  与 `own_load`（import 用）都走它，暖缓存下 `verve run main.js` 这种无 `type` 的大 CJS 入口
  也不再整份 parse 一遍。包装器路径先 `clean` 再 `source_path_of`，错误与栈里的文件名与以前一致。
  两种解析都失败的源码照旧答 `CommonJs(None)`，到运行时才报同一个 `SyntaxError`（见「已知问题」）。
- **`import` 一个无名 `.js` 不再判定两次。** `own_load` 判出 commonjs 后，把那个 `Script` 存进 `Loader`
  按路径的槽（连同所解析源码的摘要，取出即删），随后的 `import_cjs` → `load_file` → `format` 从槽里取走
  它，不再读回一次条目、也不再解析一次包装器。暖缓存下 `import` 无名 `.js` 的 `cjs_hits` 为 1（以前 2），
  module `hits` 仍是 1（`.mjs` 本来就只加载一次）；冷缓存 `cjs_misses` 1（以前 2）。
- **测试**：`tests/code_cache.rs` 新增 14 个 CJS 用例（命中、源码改动、pack 损坏与截断回退、包装后有全局
  声明不缓存并计数、循环依赖与 `module.exports` 被替换在命中时行为不变、`.json` 与原生模块不进缓存、
  既不是 CJS 也不是模块的源码照旧报同一个 `SyntaxError` 且什么都不存、`.js` 无命名格式时按 CJS 缓存并在
  第二次命中、`.js` 是模块时退回 ESM 且 `cjs_*` 不动、同一份源码分别当 CJS 与 module 时两类条目互不误读、
  带 BOM 与 `#!` 的文件命中、`import` 无名 `.js` 只解析一次、无 `package.json` 的 `.js` 入口从判定出的
  包装器运行、同一 `.js` 两次运行之间从 CJS 变成模块时第二次按模块运行）。`tests/module_hooks.rs` 新增
  2 个：load 钩子对一个既非 CJS 也非模块的 `.js` 调 `nextLoad` 仍拿得到 `next` 的答案；同一图里先 import
  的模块在那个文件失败之前已经运行。Boa 侧 `tests/code_cache.rs` 新增
  `only_scripts_that_declare_no_global_are_written_down`：包装器可写可读，`var`/`let`/`function`/Annex B
  块级函数不写，严格模式下的块级函数照写。`cargo test --test code_cache` 23 项、`--test modules` 10 项、
  `--test module_hooks` 8 项；`cargo clippy --workspace --all-targets -- -D warnings`、
  `cargo fmt --all --check`、`scripts/style-check.sh` 均通过。
- **实测**（release `verve`，`typescript.js` 5.9.3 / 9.1 MB，`performance.now()` 包住 `require`，PowerShell
  每 5 ms 轮询 `PeakWorkingSet64`，各 5 次取中位，`Get-Counter '\Processor(_Total)\% Processor Time'`
  读 5–18%）：

  | 配置 | 时间中位 | 峰值内存 |
  |---|---|---|
  | Node 22，CJS `require` | 138.3 ms | 67.7 MB |
  | Verve，无缓存 | 1,018.4 ms | 140.7 MB |
  | Verve，`--code-cache` 暖 | **164.9 ms** | **85.7 MB** |
  | Verve 暖缓存，同一文件作 `verve run` 入口 | **219.7 ms**（整个进程） | **85.5 MB** |
  | Node 22，同一入口 | 183.2 ms（整个进程） | 67.4 MB |
  | Verve 暖缓存，`.mjs` `import` 它（无 `type`） | 268.4 ms | 178.4 MB |
  | Verve 暖缓存，同上但 `{"type":"commonjs"}` | 272.6 ms | 137.2 MB |

  验收线是 ≤ 236 ms 与 ≤ 100 MB：达标（`require` 164.9 ms = Node 的 1.19 倍，内存 85.7 MB）。
  另一组轮次在更重的负载下（37–52%）读到暖 `require` 179.2–200.1 ms、入口 253.4–392.2 ms，落在同一量级；
  上表是慢路径的对照：无缓存那行的 140.7 MB 与 1.02 s 是同一次包装器 parse 保留到编译期造成的。
  （旧基线记的 2,051 ms / 228 MB 里还有格式判定多解析的那一遍。）

没完成：

- `docs/electron/perf-js.md` 没有写：它是 P60 的交付物，本包没有创建。数字在 `docs/startup.md` 与本节。
- `perf-baseline.md` 里的旧 CJS 数字没有改动（那是 P60 的基线文件）。

超出原范围（理由写在同一批 `docs(electron)` commit 里）：

- 任务书「修改」栏原来没有 `src/code_cache_load.rs`。`scripts/style-check.sh` 的文件长度棘轮不许
  `src/code_cache.rs`（1666 行）再变长，而新增的 loader 入口与统计让它涨到 1761 行；把它拆成新模块后
  `src/code_cache.rs` 是 1660 行，`src/code_cache_load.rs` 110 行。
- 本包还要改 `boa/core/engine`（经典脚本可写可读）与 `src/esm.rs`（把格式判定收成一处），所以在同一处
  也改了 `docs/electron/README.md` 第 100 行的允许清单，给 P62 加上 `boa/core` 的 engine。

已知问题：

- 暖缓存路径仍要整份读回 17 MB 的 entry 并逐条校验，暖 `require` 的 179–200 ms 里这是主要部分；校验与
  lazy 化的 scopes 是「Faster reading back」那条留下的空间。
- 包装器源码在 Rust 侧仍会 `format!` 出约 9 MB 字符串并转成 UTF-16 交给 Boa；命中时这次分配还在，
  只是不再解析。要再省内存得让 Boa 接受不含包装前缀的源码切片。
- `import` 一个无名 `.js` 比同一文件在 `{"type":"commonjs"}` 下多约 41 MB 峰值：判定那一步的调用方同时持有
  源码文本与它解析出的包装器，槽只共享后者。要抹平得让 `own_load` 不再克隆源码。
- 无缓存路径的 141 MB 峰值没有降：它来自同时存活的包装器 parse 与编译结果，属解析器/作用域分析的内存
  问题，不在本包范围。
- 一个既不是 CJS 也不是模块的文件（入口、`require` 或 `import` 都一样）：`modules::format` 答
  `CommonJs(None)`，由 `run_module_with` 在运行时再解析一次并抛出同一个 `SyntaxError`，所以同图里先
  前的模块照样先运行，load 钩子对 `nextLoad` 的调用也不会提前失败（只在出错路径上多一次解析）。
- 槽里的包装器在模块图链接中途失败时不会被取走，会留到 runtime 结束；它按路径 + 源码摘要校验，所以不会
  让改过的文件跑旧代码，只是多占一份编译结果。

给其他包的备注：

- P63（electron JIT）：脚本读回后 codeblock 的 `lazy` 槽为空、嵌套函数是 `LazyCode::cached`，JIT 的门槛
  计数照旧只看运行次数，无需特判；但 JIT 化的函数在第二次运行时才可能编译成机器码，暖缓存下函数首次
  调用走的是「从字节造 codeblock」而不是「编译 AST」。
- P42（Electron 接入缓存）：CJS 与 ESM 共用同一个 pack 与同一把 secret，`--code-cache` 与
  `VERVE_CODE_CACHE` 对两者都生效，不需要新开关；日志里用 `cjs_*` 字段区分。
- P61/P64：`CodeCacheStats` 加了 5 个字段，按结构体字面量构造的地方要补齐（本仓库内只有 `Default` 与
  增量赋值，已通过编译）。
- 将来的 `<script>` 缓存（渲染器）：`Script::from_code_cache` 的头与键里都没有 context 的严格模式
  （`Script::parse` 在 `context.is_strict()` 时 `parser.set_strict()`），读回不检查。Verve 不设严格模式，
  所以本包不受影响；谁要用严格 context 缓存脚本，得先把这一位加进头和键。

