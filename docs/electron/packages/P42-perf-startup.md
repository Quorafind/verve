# P42 性能与启动

**依赖**：M1–M3（P10、P11、P20、P34 至少已合并）；P01 的体积实测  **规模**：M  **修改**：`crates/verve-electron/`（启动流程、预热池、性能追踪）、`crates/verve-gpui/`（帧时间戳）、`src/code_cache.rs` 与 `src/modules.rs`（**允许**：CommonJS 包装函数入缓存）、`boa/core/gc`（**允许，需记 `boa/SU.md`**：GC 阈值可配置）、新建 `scripts/electron-bench/` 与 `docs/electron/perf.md`

## 目标

达到并**可重复地证明** README 第 1 节的性能目标，并与真实 Electron 39.2.0 在同一台机器、同一应用上对比：

| 指标 | 目标 | 本包负责的手段 |
|---|---|---|
| 冷启动到首帧（electron-quick-start，热代码缓存） | ≤ 250 ms | 代码缓存（含 CJS）、渲染器预热、启动并行化 |
| 单窗口空闲内存 | ≤ 60 MB | GC 调参、空闲回收、快照共享 |
| `ipcRenderer.invoke` 往返 p50 | ≤ 0.2 ms | 测量与热点修复（P12 实现，本包测量和调优） |
| 2000 行列表滚动 | 60 fps | 测量（P33 实现增量渲染，本包测量和调优） |

## 背景（先读）

- `docs/startup.md`：lazy compile 与代码缓存的设计、测量方法与结果（暖缓存使 Rebon 的组合从 167 ms 降到 105 ms）。
  特别注意「What is left」：**CommonJS 脚本目前不进缓存**，而 Electron 应用的主进程产物大多是 CJS。
- `src/code_cache.rs`：缓存的格式（每个构建一个 `<dir>/<build>.pack`）、信任模型（HMAC 密钥不在缓存目录里）、
  `user_secret()`、`flush`/`flush_now` 的时机；`Runtime::set_code_cache_dir_with_secret` /
  `set_code_cache_dir_with_user_secret` / `flush_code_cache` / `code_cache_stats`（`src/lib.rs`）。
- `src/modules.rs` 约 430–470 行：CJS 模块用 Node 的包装器拼成 `(function (exports, require, …) {…})` 后
  `context.eval` 得到包装函数——这个脚本**不声明任何全局绑定**，这正是它可以像 ES 模块一样缓存的原因。
- `docs/jit.md`「Switching it on and off」与「Results (phase 2)」：JIT 的开关（feature `jit`、`VERVE_JIT`、
  `Runtime::set_jit_enabled`）、收益分布（属性/元素访问 3–12×，调用约 1.5×）、release 体积代价（phase 2 约 +0.6 MB 于 phase 1；
  Cranelift 本身的体积由 P01 测）。
- `boa/core/gc/src/lib.rs` 约 100–120、250–265 行：GC 阈值初始 1 MiB、`used_space_percentage` 70，无公开设置入口。
  每个 isolate 线程有自己的 GC 堆。
- README「Integration observations」：本仓库报告测量的格式（机器、版本、配置、取最好/中位、噪声说明）。本包的报告照这个格式写。
- P02 的线程模型与 `Hub`；P10 的启动顺序（何时知道 `userData`）；P11 的 asar 读取（asar 头已在内存里）。

## 接口契约

### 1. 代码缓存接入

- 主进程与每个渲染器的 `Runtime` 在**加载任何应用代码前**调用
  `set_code_cache_dir_with_user_secret(dir)`（用 Verve 的用户级密钥；出错则不用缓存继续运行，打印一次 `VERVE_ELECTRON_LOG=perf` 日志）。
- 目录：`<默认 userData>/Code Cache/verve/<generation>/`。
  - 默认 `userData` 由 P10 在运行 `main.js` 之前按 package.json 的 `productName`/`name` 算出；应用之后调用
    `app.setPath('userData', …)` 不改变本次运行的缓存目录（下次启动也仍用默认目录——记录这个差异）。
  - `<generation>`：应用来自 `app.asar` 时取 **asar 头（P11 已解析的 JSON 头字节）SHA-256 的前 16 个十六进制字符**；
    来自 `resources/app/` 或命令行目录时取 `dev`。缓存条目本身仍按"构建指纹 + 全局作用域 + 源码全文"做键
    （`src/code_cache.rs` 现状），generation 只用来**整体淘汰旧版本应用的缓存**：启动后空闲时删除同级的其他 generation 目录。
  - `dev` generation 设大小上限（默认 64 MiB），超过时整个清空重建。
- 渲染器与主进程共享同一目录与 pack（`code_cache.rs` 的 pack 写入是"临时文件改名"，并发写者最后一个胜出；
  需验证两个 isolate 同时 flush 不会损坏、最多丢失一次运行的新增条目，并写测试）。
- 关闭窗口销毁渲染器前、`app.quit` 退出前调用 `flush_code_cache()`。

### 2. CommonJS 入缓存（修改 `src/modules.rs`、`src/code_cache.rs`）

- 对 CJS 包装脚本走与 ES 模块相同的缓存路径：键 = 构建指纹 + 全局作用域 + **包装后的完整源码**；读回后经 Boa 的校验再运行。
- 前提检查：包装脚本的全局声明集合必须为空（实现里断言；不为空时不缓存并计数），这与 `docs/startup.md` 拒绝缓存经典脚本的理由一致。
- `CodeCacheStats` 增加 `cjs_hits`/`cjs_misses`（或按类型分组），`docs/startup.md` 更新相应段落。
- 经典 `<script>`（渲染器）不在本包范围内缓存；若测量显示它是首帧的主要成本，写出分析交给后续（需要回放全局声明实例化）。

### 3. 渲染器预热

- `Hub::prewarm_renderer()`：在主进程 isolate 开始执行 `main.js` 的**同时**，起一个空闲渲染器线程，完成与 `webPreferences`
  无关的初始化：`Runtime::new`、代码缓存目录、`verve_dom::install`（`about:blank`、UA 样式表已解析）、`electron` 渲染器模块注册。
- `Hub::create_renderer` 优先从池里取（池大小 1），然后再应用 `webPreferences`（preload、nodeIntegration、contextIsolation、
  sandbox、额外参数）。**任何依赖 `webPreferences` 的初始化都不能放进预热**；若某项必须在 install 前决定（如 sandbox 下不暴露的全局），
  就让该次创建绕过池。
- 池在第一帧呈现后、主线程空闲时补充；`VERVE_ELECTRON_PREWARM=0` 关闭。
- 测量预热前后 `BrowserWindow` 构造到 `did-finish-load` 的时间差，写进报告；收益 < 5 ms 时默认关闭。

### 4. 启动并行化

- 时间线上三件事并行：GPUI 初始化（OS 主线程）、主进程 isolate 跑 `main.js`、预热渲染器。
- `app.whenReady()` 的兑现时机：Shell 报告就绪之后（与 Electron 语义一致：ready 之后才能建窗口）。测量 GPUI 初始化
  耗时；若它是关键路径，评估"先兑现 ready、`CreateWindow` 在 Hub 排队直到 Shell 就绪"的方案，**不得改变 JS 可观察的事件顺序**。
- 字体：测量 GPUI 文本系统首次枚举系统字体的成本；若显著，改为按需加载（与 P31 协作）。

### 5. JIT 默认策略

- 由 P01 给出 `jit` feature 的 release 体积增量。规则：增量使 `electron.exe` 超过 24 MB 硬上限 → 默认构建不带 JIT，另发一个
  `-jit` 变体（P40 的平台包加一个可选变体）；在上限内 → 默认带 JIT。
- 带 JIT 时：主进程与渲染器都开，阈值用默认；`VERVE_JIT=0` 可关。用 P41 的 L2/L3 应用测量开与关的首帧时间和交互耗时，结果写报告。
- macOS 签名授权见 P40 第 10 节。

### 6. 内存

- GC 阈值可配置（改 `boa/core/gc`，记 `boa/SU.md`）：`boa_gc::set_config(threshold, used_space_percentage)` 之类的线程局部设置；
  渲染器初始阈值按测量调（目标：减少首帧前的回收次数，又不让空闲内存超标）。
- 空闲回收：窗口隐藏或最小化后、页面空闲 2 s 后，在渲染器线程上依次调用 `Document::sweep` 与 `Runtime::collect_garbage`；
  主进程空闲时同样回收。不得在动画或输入处理中途回收。
- 快照共享：Shell 只持有 `Snapshot` 的 `Arc`，不做深拷贝（审查 P30/P33 的代码，有深拷贝就提 issue 给对应包）。
- 跨 isolate 共享只读数据：UA 样式表解析结果、字体数据等放进进程级 `OnceLock<Arc<…>>`。Boa 的对象和 interner 不能共享，不做尝试。
- 目标分解（实测后修订）：进程基线 + GPUI ≤ 25 MB，主进程 isolate ≤ 10 MB，每个渲染器（quick-start 页面）≤ 15 MB。

### 7. 测量方法（`scripts/electron-bench/`，需新增；报告写 `docs/electron/perf.md`，需新增）

- **追踪**：`VERVE_ELECTRON_TRACE=1` 时，各线程把关键时间点（相对进程启动的微秒数）写进一个无锁环形缓冲，退出时输出 JSON：
  `process_start`、`main_script_start`、`app_ready`、`create_window`、`renderer_ready`、`load_start`、`dom_content_loaded`、
  `load`、`first_snapshot`、`first_present`（Shell 首次呈现含渲染器内容的帧）。不开时零开销（一个 relaxed 原子读）。
- **跨运行时可比的指标**：基准应用（`scripts/electron-bench/apps/`，需新增：quick-start 副本、IPC 压测、2000 行列表）
  在渲染器首个 `requestAnimationFrame` 回调里经 IPC 把 `Date.now()` 报给主进程打印；驱动程序记录子进程创建时刻。
  同一份应用代码分别用 Verve 和**真实 Electron 39.2.0** 运行，这个"首帧"定义对两者相同。
- **驱动程序**：`crates/verve-electron/examples/bench.rs`（需新增，Rust，不依赖 Node）——启动 N 次（默认 15），配置交错运行
  （Verve 冷缓存 / Verve 暖缓存 / Verve 暖缓存 + JIT 关 / Electron），记录首帧时间与**整个进程树**的峰值工作集
  （Windows：`GetProcessMemoryInfo` 的 `PeakWorkingSetSize`，Electron 要把所有子进程加总；按 README 的做法以 5 ms 间隔采样）。
- **IPC**：基准应用做 10,000 次 `invoke`（空参数、1 KB 对象、64 KB 字节数组三档），报告 p50/p95/p99。
- **滚动**：GUI 构建下 Shell 注入匀速滚轮事件 5 s，统计呈现帧数、丢帧数、帧时间 p95。
- **报告格式**（照 README「Integration observations」）：日期；OS、CPU（核/线程）、内存；Rust 版本、Verve commit、Electron 版本；
  release 配置与 feature；每项"最好 / 中位"与峰值工作集中位；机器是否被其他任务占用、多少差异以内算噪声。

## 步骤

1. 追踪点与 `bench.rs`；先测出现状基线（Verve 与 Electron），写 `perf.md` 第一版，标出关键路径。
2. 代码缓存接入 + generation 淘汰；CJS 入缓存（含测试：改动源码、损坏 pack、两个 isolate 并发 flush）。
3. 渲染器预热与启动并行化，逐项测量收益，无收益的默认关闭。
4. GC 可配置、空闲回收、内存分解测量。
5. 按 P01 的结果定 JIT 默认策略。
6. 达标后再测一轮，`perf.md` 记最终数字；未达标项写明差距与原因。

## 验收

- `cargo test -p verve-electron` 与 `cargo test --test code_cache` 通过（新增的 CJS 缓存与并发 flush 测试在内）。
- 第二次启动 electron-quick-start 时 `code_cache_stats()` 显示主进程 CJS 与渲染器 ESM 均命中（日志可见）。
- `perf.md` 有一份按上述格式的完整报告，包含与 Electron 39.2.0 的对比；README 第 1 节的四项目标逐项给出"达标/差距"。
- 关闭追踪时，`bench.rs` 测得的首帧时间与打开追踪时差异在噪声内。

## 不做

懒解析（parser 重写，`docs/startup.md` 已说明）；经典脚本缓存（只做分析）；V8 快照式的堆快照；
为了跑分而改变语义的任何优化。

## 状态

（实现者完成后填写）
