# P62 CommonJS 进代码缓存

**依赖**：无  **规模**：M  **修改**：`src/modules.rs`、`src/code_cache.rs`、`src/lib.rs`（只限 `CodeCacheStats`）、`tests/code_cache.rs`、`docs/startup.md`

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

（实现者完成后填写）
