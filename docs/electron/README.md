# Verve Electron：可直接替换的 Electron

> 本目录是 **Verve 替换 Electron** 的总体方案和分包任务书。每个 `packages/Pxx-*.md` 是一个可独立交付的工作包，
> 写给实现者（DeepSeek）看：目标、要改的文件、接口契约、步骤、验收、不做什么。
> 动手前先读完本文和 [`conventions.md`](conventions.md)。

## 1. 目标

用户把项目里的 `electron` 依赖换成 Verve 的（`npm i -D electron@npm:@verve/electron`，或直接把
`node_modules/electron/dist` 换成 Verve 的发行目录），**不改一行应用代码**，`npx electron .`、
electron-builder / electron-forge 打包出来的应用照常运行。

| 指标 | 目标（Windows x64 release，strip 后） | 说明 |
|---|---|---|
| 运行时二进制 `electron.exe` | **≈ 32 MB，含 JIT**（硬上限 **34 MB**，2026-11-02 定，见下注） | Electron 本身约 180 MB |
| JavaScript 层 | **≈ 10 MB** | 应用自己的 `app.asar` + 代码缓存；**Verve 不内置任何 JS 垫片** |
| 冷启动到首帧（electron-quick-start，热代码缓存） | ≤ 250 ms | |
| 单窗口空闲内存 | ≤ 60 MB | Electron 约 150 MB |
| `ipcRenderer.invoke` 往返 p50 | ≤ 0.2 ms | 线程间通信，不跨进程 |
| 2000 行列表滚动 | 60 fps | GPUI 绘制 + 分区增量 |

**体积实测（2026-11-02，Windows x64，strip + fat LTO）**：

| 部分 | 实测 | 来源 |
|---|---:|---|
| Verve 核心（Electron 特性组合） | 17.8 MB | P01（35.8 MB → 17.8 MB） |
| GPUI（一个窗口画一行字的增量） | +6.1 MB | P30 spike |
| verve-dom | ≈ +2 MB | 估计，待 P02 接入后实测 |
| **合计（不含 JIT）** | **≈ 26 MB** | |
| JIT（可选） | +5.9 MB | P01；数值热循环快约 8 倍 |

**决定（2026-11-02，用户）：硬上限 34 MB，JIT 默认开启**——性能优先，约 32 MB，仍约为 Electron 的 1/5。原 20/24 MB 目标作废。
继续压缩仍然是 P42 的工作（腾出余量）：`wasm` 关闭（−0.7 MB）、
依赖试 `opt-level = "z"`、归属 4.4 MiB 未归属的 `.rdata`。详见 [`size.md`](size.md)（P01 合并后可见）。

## 2. 原则（不可违反）

1. **一切都是 Rust 原生**。Electron 的每个模块、类、方法都是 Boa 上的原生函数，和 Verve 现有的 Node
   模块一样；不允许写一段 JavaScript 来"模拟" Electron 或浏览器（README 第一段的原则）。
2. **DOM 由 verve-dom 持有，布局由 verve-layout 在渲染器线程完成，GPUI 只负责绘制与文字整形**。没有 Chromium、没有 WebView。
   verve-dom 产出 `Snapshot` → `verve-layout`（taffy + 自研行内排版，文字度量走 GPUI 的 `TextSystem`）算出盒子并
   生成 `DisplayList` → GPUI 线程按序回放。布局在 JS 所在线程同步完成，所以脚本改完 DOM 立刻读
   `getBoundingClientRect`/`offsetHeight` 就是对的（CodeMirror 这类应用的硬性需求）。
   不用"每个 DOM 节点变一个 GPUI `Div`"的做法：GPUI 的样式缺 inline、fixed/sticky、z-index、多色边框、
   复杂 grid 轨道等，而且尺寸会晚一帧。
3. **一个进程，多线程**。Electron 的主进程、每个渲染进程，在这里各是一个 `verve::isolate`
   （独立线程、独立 Boa 堆、互不阻塞、panic 只结束自己）。GPUI 占用操作系统主线程。
4. **对外表现与 Electron 39 一致**：`process.versions.electron === '39.2.0'`、事件顺序、错误信息、
   默认值都以 Electron 文档和真实 Electron 的行为为准；做不到的显式抛错（`ERR_NOT_SUPPORTED`
   风格），不静默吞掉。
5. **体积是功能**。每个包新增依赖都要在 PR 里写明 release 体积增量（`P01` 提供脚本）。

## 3. 总体架构

```
electron.exe（crates/verve-electron 的 bin）
│
├─ OS 主线程：GPUI Application ── crates/verve-gpui
│     Shell：每个 BrowserWindow 一个 GPUI Window，里面一个 DomView
│     DomView：读渲染器发布的 DisplayList → 自定义 GPUI 元素按序绘制（不做布局）
│              ← 鼠标/键盘/IME/滚轮 → Input 消息发给对应渲染器
│     菜单/托盘/全局快捷键（muda / tray-icon / global-hotkey）
│
├─ 线程 "electron-main"：isolate<MainGuest>
│     verve::Runtime + require('electron') 主进程版：
│     app, BrowserWindow, webContents, ipcMain, dialog, Menu, Tray, shell, clipboard,
│     nativeTheme, nativeImage, screen, protocol, session, net, Notification,
│     powerMonitor, safeStorage, globalShortcut, utilityProcess, systemPreferences …
│     Node 全部模块（含真实的 child_process，P17）
│
├─ 线程 "renderer-<id>"（每个 webContents 一个）：isolate<RendererGuest>
│     verve::Runtime + verve_dom::install(...) + verve-layout + require('electron') 渲染进程版：
│     DOM 变化 → 同步布局（taffy + 行内排版）→ DisplayList 放进 RendererView
│     ipcRenderer, contextBridge, webFrame, webUtils
│     页面加载管线（HTML、<script>、<link>、<img>、@font-face、ESM）
│     preload；contextIsolation=true 时 preload 在独立 Realm（P21）
│
└─ Hub（Arc，crates/verve-electron/src/hub.rs）
      所有线程间消息的路由表：WindowId ↔ ContentsId ↔ isolate 句柄 ↔ GPUI 窗口
```

线程与消息的**精确契约**在 [`P02`](packages/P02-skeleton-and-contracts.md) 里定义，所有包都按它通信。
关键规则：

- 主进程线程**永远不同步等待**渲染器或 Shell；只有渲染器可以同步等主进程（`ipcRenderer.sendSync`）。
  这条单向规则保证不会死锁。
- Shell（GPUI）线程不执行任何 JavaScript，只读快照、发消息。
- 跨线程传 JS 值一律用 Verve 的结构化克隆（`verve::clone::Cloned`，P03 开放），不传 JSON 字符串。

## 4. 代码布局

```
crates/
  verve-dom/          已有：无头 DOM + CSS 层叠（P22–P25 补齐）
  verve-asar/         新：app.asar 读取器与 `verve::vfs::Overlay` 实现（P11），只依赖 verve
  verve-electron/     新：Electron API、Hub、主进程/渲染器 Guest、无头 Shell（依赖 verve-asar；纯逻辑库，无 bin）
  verve-layout/       新：布局引擎（taffy + 行内排版 + 层叠上下文 → DisplayList），不依赖 GPUI（P30/P31）
  verve-gpui/         新：`electron` 二进制（src/bin/electron.rs）、GPUI Shell、DomView（回放 DisplayList）、TextMeasurer 的 GPUI 实现、输入桥接、菜单托盘
  verve-electron-npm/ 新：npm 包模板与平台包脚本（P40，非 Rust）
src/                  Verve 内核：只允许 P03、P11、P17、P42、P62、P67、P68 修改（P42 还可改 boa/core/gc；性能线 P61、P62、P64–P66、P68–P74 可改 boa/core 的 engine 与 string；都需记入 boa/SU.md）
docs/electron/        本方案
tests/electron/       夹具应用与无头验收（P00）
```

GPUI 版本：与 Rebon 实际使用的同一个 fork（`Quorafind/zed-c`，P30 调研时为 `e679b51`），在 workspace 里锁 rev，
由 P02 决定并写入 `Cargo.toml` 注释。渲染器线程用应用共享的 `Arc<TextSystem>` 与已公开的 `WindowTextSystem::layout_line`
整形文字，**无需补丁**（P30 spike 已验证，见 [`spikes/p30-text-measurement.md`](spikes/p30-text-measurement.md)）；
fork 只需两处一个字的可见性补丁：`Window::with_element_opacity`、`WindowTextSystem::finish_frame` 改 `pub`（P30 负责）。**verve 根 crate、verve-electron、verve-layout 都不得依赖 GPUI**；只有 `verve-gpui` 依赖。

## 5. 里程碑与依赖

```
M0 地基      P00 夹具/无头验收   P01 体积预算   P02 骨架与契约   P03 内核开放接口
               │                                 │                  │
M1 主进程    P10 启动器/app ─ P11 asar/VFS ─ P12 IPC ─ P13 BrowserWindow/webContents
             P14 系统集成A   P15 菜单托盘快捷键   P16 protocol/session/net   P17 child_process
               │
M2 渲染器    P20 页面加载管线 ─ P21 preload/contextBridge
             P22 CSS 补齐   P23 DOM 补齐   P24 编辑与输入法   P25 canvas/图片解码
               │
M3 布局/GPUI P34 Shell 窗口 ─ P30 盒模型布局与绘制 ─ P31 行内排版与文本 ─ P32 输入与表单控件
             P33 增量渲染与帧调度   P35 图片/SVG/canvas 表面
               │
M4 交付      P40 npm 包与发行布局   P41 兼容矩阵与真实应用   P42 性能与启动   P43 调试替代
后续         P50 N-API 原生模块

性能线（与批次 2 起并行，见下文「性能线」）
             P60 JS 基准与剖析 ─ P61 超线性内建   P62 CJS 代码缓存   P63 electron 可带 JIT
               │
             P64 字符串/正则/JSON   P65 对象/集合/类型化数组   P66 JIT 覆盖扩展   P67 CLI 默认开缓存
               │
             P68 评审积压修正   P69 调用与构造   P70 迭代器   P71 rope 字符串   P72 JIT 类型化数组 length
             P73 内建借用重入   P74 字符串后续 ─ RP 性能复盘
```

| 包 | 名称 | 依赖 | 可并行 | 规模 |
|---|---|---|---|---|
| [P00](packages/P00-fixtures-and-headless.md) | 夹具应用与无头验收 | P02 | ✓ | M |
| [P01](packages/P01-size-budget.md) | 体积预算与 release 配置 | — | ✓ | S |
| [P02](packages/P02-skeleton-and-contracts.md) | 骨架、线程与消息契约 | — | **先做** | M |
| [P03](packages/P03-kernel-hooks.md) | Verve 内核开放接口 | — | ✓ | M |
| [P10](packages/P10-launcher-and-app.md) | 启动器与 `app` 生命周期 | P02 | | M |
| [P11](packages/P11-asar-vfs.md) | asar 与虚拟文件系统 | P03 | ✓ | M |
| [P12](packages/P12-ipc.md) | IPC | P02 P03 | | M |
| [P13](packages/P13-browserwindow-webcontents.md) | `BrowserWindow` / `webContents` | P02 P12 | | L |
| [P14](packages/P14-system-a.md) | dialog/shell/clipboard/theme/screen/通知/safeStorage/nativeImage | P02 | ✓ | L |
| [P15](packages/P15-menu-tray-shortcut.md) | Menu / Tray / globalShortcut | P02 P34 | ✓ | M |
| [P16](packages/P16-protocol-session-net.md) | protocol / session / net | P20 | | M |
| [P17](packages/P17-child-process.md) | 真实 `child_process` 与 `utilityProcess` | P03 | ✓ | L |
| [P20](packages/P20-page-loading.md) | 渲染器 Realm 与页面加载管线 | P02 P03 | | L |
| [P21](packages/P21-preload-contextbridge.md) | preload / contextBridge / 隔离 | P20 P12 | | L |
| [P22](packages/P22-dom-css.md) | verve-dom CSS 补齐 | — | ✓ | L |
| [P23](packages/P23-dom-tree.md) | verve-dom DOM 补齐 | — | ✓ | L |
| [P24](packages/P24-editing-ime.md) | contenteditable、输入法、拖放 | P23 | ✓ | L |
| [P25](packages/P25-canvas-images.md) | canvas 2D 与图片解码 | — | ✓ | M |
| [P30](packages/P30-gpui-boxes.md) | verve-layout 盒模型布局 + DomView 绘制 | P02（布局部分）P34（绘制部分）P23 | | L |
| [P31](packages/P31-gpui-inline-text.md) | 行内排版与文本 | P30 | | L |
| [P32](packages/P32-gpui-input-controls.md) | 输入桥接与表单控件 | P30 | | M |
| [P33](packages/P33-gpui-incremental.md) | 增量渲染与帧调度 | P30 | | M |
| [P34](packages/P34-gpui-shell.md) | Shell：窗口管理 | P02 | | M |
| [P35](packages/P35-gpui-media.md) | 图片 / SVG / canvas 表面 | P30 P25 | ✓ | M |
| [P40](packages/P40-npm-dist.md) | npm 包与发行布局 | P10 | ✓ | M |
| [P41](packages/P41-compat-matrix.md) | 兼容矩阵与真实应用 | M1–M3 | 持续 | 持续 |
| [P42](packages/P42-perf-startup.md) | 性能与启动 | M1–M3 | | M |
| [P43](packages/P43-devtools.md) | 调试替代 | P13 | ✓ | S |
| [P50](packages/P50-napi.md) | N-API 原生模块（v2） | M4 | | XL |
| [P60](packages/P60-js-bench.md) | JS 基准与剖析方法 | — | ✓ | S |
| [P61](packages/P61-superlinear-builtins.md) | 超线性内建修复（`split`、`Map`/`Set` 删除、正则扫描） | — | ✓ | S |
| [P62](packages/P62-cjs-code-cache.md) | CommonJS 进代码缓存 | — | ✓ | M |
| [P63](packages/P63-electron-jit.md) | `electron` 二进制可带 JIT | P02 | ✓ | S |
| [P64](packages/P64-strings-regex-json.md) | 字符串、正则与 JSON 热路径 | P60 P61 | ✓ | L |
| [P65](packages/P65-objects-typed-arrays.md) | 对象、集合与类型化数组热路径（解释器） | P60 P61 | ✓ | L |
| [P66](packages/P66-jit-coverage.md) | JIT 覆盖扩展 | P60 P63 | | L |
| [P67](packages/P67-code-cache-default.md) | `verve run` 默认开代码缓存 | P62 | | M |
| [P68](packages/P68-correctness-backlog.md) | 评审积压的三处语义修正（`super[k]=v`、reviver 孤立代理项、模块解析的权限错误） | P64 P65 P67 | ✓ | S |
| [P69](packages/P69-call-cost.md) | 调用与构造的固定成本（帧、`new`、原生回调） | P65 P66 | ✓ | L |
| [P70](packages/P70-iterators.md) | 迭代器结果不分配对象（`for…of`、展开、解构） | P65 P66 | ✓ | M |
| [P71](packages/P71-rope-strings.md) | 字符串 `+=` 线性化（rope） | P64 | ✓ | M |
| [P72](packages/P72-jit-typed-length.md) | JIT：类型化数组 `length` 的站点缓存 | P65 P66 | ✓ | S |
| [P73](packages/P73-reentrant-borrows.md) | 内建对象调用用户代码时仍借用着自身（重入 panic） | P70 | ✓ | S |
| [P74](packages/P74-string-followups.md) | 字符串后续：数字转字符串、rope 叶子合并、字符串迭代 | P70 P71 | ✓ | S–M |

### 重构关卡

每个里程碑结束时做一次只重构、不加功能的关卡；关卡通过后才派发下一里程碑。要求见 [`conventions.md`](conventions.md)「精炼」「重构节奏」。

| 关卡 | 时机 | 重点 |
|---|---|---|
| R1 | M0 + M1 合并后 | `verve-electron` 的 API 定义方式统一（方法表/宏）、Hub 与两个 Guest 的消息分发去重、P03 内核钩子的命名与文档；公开 `verve::encoding`（base64、data: URL 解析、percent-decode），删除 `esm.rs`、`network/fetch.rs`、`verve-dom/src/canvas/mod.rs` 里各自手写的副本；把根 crate 各模块与 verve-dom `bind/mod.rs` 的工具箱副本迁到 `verve::native`，`ci/style/toolkit-copies.txt` 清空 |
| R2 | M2 合并后 | 页面加载管线、preload/world、verve-dom 新增部分的模块边界；拆分 `ci/style/large-files.txt` 里的 verve-dom 文件；`verve-dom` 接入 `[lints] workspace = true` |
| R3 | M3 合并后 | verve-layout 的样式解析与 verve-dom 的 CSS 值解析合并为一处；DisplayList 与 GPUI 回放的边界；输入翻译表 |
| R4 | M4 前 | 全仓库：死代码、未用的 feature 与公开 API、依赖与体积复查；拆分根 crate 的超长文件，`verve` 接入 `[lints] workspace = true` |

此外每合并约 3 个功能 PR，插一个同区域的小重构 PR（见 `DISPATCH.md`）。

### 性能线

[`perf-baseline.md`](perf-baseline.md)（2026-11-03）测得：启动、内存、体积都远好于 Electron，但**纯 JS 吞吐比 V8 慢 15–850 倍**（典型 30–300 倍），
JIT 在这组测试上几乎没有收益，三个内建是平方级的，大 CJS 包不走代码缓存（`require('typescript')` 是 Node 的 17 倍时间、3.9 倍内存）。
对 React 式 UI 与编辑器类应用，这是本方案最大的风险。所以性能不等到 M4，单开一条线与功能包并行：

1. **立即可做（与批次 2 并行）**：P60 把基准搬进仓库并给出剖析方法；P61 修三个超线性内建并扫一遍同类问题；
   P62 让 CJS 进代码缓存（原 P42 第 2 节）；P63 让 `electron` 二进制能带 JIT（默认开不开仍由 P42 第 5 节定）。
2. **剖析驱动（P60、P61 合并后，与批次 3 并行）**：P64 字符串/正则/JSON，P65 对象/集合/类型化数组（解释器），P66 JIT 覆盖；P67 让 `verve run` 默认开代码缓存（P62 合并后即可派发，含首次运行成本、旧缓存淘汰与并发写）。
   这三个包的目标先写成「参考目标」，第一次剖析后由实现者提出、总控确认，才开始大改。
   **第二轮（P64–P67 合并后，2026-11-04）**：P68 修评审积压的三处语义偏差；P69 调用与构造的固定成本，P70 迭代器结果不装箱，P71 rope，P72 JIT 类型化数组 `length`——都来自 P64–P66 的「状态」里留下的差距。
3. **RP 性能复盘**：P41 能跑起 L2 应用（Vite/React）后、P64–P66 与 P69–P72 合并后，用真实应用（React 1000 行表格渲染、L3 编辑器的输入延迟、
   主进程大 CJS 包启动）测 Verve 与 Electron 39，按剖析决定下一步：继续调内建与解释器，做 JIT 第 3 阶段（类型反馈），
   还是把差距作为已知限制写进兼容矩阵。

预期：这条线能把差距缩小几倍到几十倍，**达不到 V8 的水平**；它的价值是先把"慢到不能用"的地方（超线性、无缓存、JIT 不生效）清掉，
再让复盘基于真实应用而不是微基准。性能线的包可以改 `boa/core` 的 engine 与 string（单独 commit，记 `boa/SU.md`）；
做测量的包允许 release 构建，见 `conventions.md`「构建纪律」。

规模：S ≈ 1–2 天，M ≈ 3–5 天，L ≈ 1–2 周，XL ≈ 1 月以上（以一个实现者计）。

**建议的派发顺序**：第一批并行 `P02`、`P01`、`P03`、`P22`、`P23`、`P25`；
P02 合并后并行 `P00`、`P10`、`P11`、`P12`、`P14`、`P17`、`P20`、`P34`；
之后 `P13`、`P21`、`P30` → `P31`/`P32`/`P33`/`P35`、`P15`、`P16`；最后 M4。

## 6. 兼容等级（P41 维护，用来衡量进度）

| 等级 | 能跑的东西 |
|---|---|
| L0 | `electron -e`/纯主进程脚本：`app.whenReady`、Node 模块、`child_process` |
| L1 | electron-quick-start：窗口、HTML+CSS 静态页、preload、`process.versions` 显示 |
| L2 | IPC 示例、多窗口、对话框、菜单、托盘、`app://` 协议加载的 Vite/React 应用 |
| L3 | 带 contextIsolation 的中型应用（如待办/Markdown 编辑器），electron-builder 打包产物替换后可用 |
| L4 | 大型真实应用（编辑器类、CodeMirror 6 / Monaco 级别的布局测量依赖） |

## 7. v1 明确不做

N-API 原生模块（`.node`，放到 P50）；WebGL/WebGPU；WebRTC；打印与 `printToPDF`；
`desktopCapturer`；Chrome 扩展；`<webview>` 标签与 `BrowserView`/`WebContentsView`（v2）；
拼写检查；Chrome DevTools 协议（P43 只做替代品）；Widevine/DRM；自动更新的真实实现（`autoUpdater` 只发 `error`）。
遇到这些 API：对象存在、调用抛 `Error('… is not supported by Verve')`，并在 P41 的矩阵登记。
