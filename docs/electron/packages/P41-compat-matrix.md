# P41 兼容矩阵与真实应用

**依赖**：M1–M3 各包（持续进行，每个里程碑后更新）  **规模**：持续  **修改**：新建 `docs/electron/compat.md`、`tests/electron/compat/`（需新增）、`crates/verve-electron/examples/api_coverage.rs`（需新增）、`crates/verve-electron/src/api/registry.rs`（需新增，见契约 3）

## 目标

1. 用一份**会被脚本更新的**表格回答"现在能跑什么、差什么"：API 维度（每个 Electron 模块/类/方法的状态）和应用维度
   （README 第 6 节 L0–L4 每级的具体验收应用）。
2. 每个验收应用都能**离线、可重复**地准备和运行，无头模式在 CI 上自动跑；截图对比为可选项。
3. 用官方 `electron.d.ts` 自动统计 API 覆盖率，作为进度的客观指标。

## 背景（先读）

- README 第 6 节（兼容等级）与第 7 节（v1 不做的 API，统一抛 `ERR_NOT_SUPPORTED`）。
- P00：无头 Shell、夹具应用目录 `tests/electron/apps/`、无头验收的写法。本包的自动化完全建立在它之上。
- `tests/fixtures/network/prepare.cjs` 与 README「Checking it」：仓库里"离线准备 npm 夹具、放在仓库外、
  用环境变量指过去、测试标 `#[ignore]`"的现有做法（`VERVE_NETWORK_NPM`、`VERVE_ESM_PACKAGES`）。照抄这套约定。
- `tests/corpus.rs`：拿真实社区插件做语料验收的做法（`VERVE_CORPUS`）。
- `Cargo.toml` 已有 `oxc_parser` 等依赖：覆盖率脚本用它解析 `.d.ts`，不引入 TypeScript 编译器。

## 接口契约

### 1. 验收应用清单（初始版，实施时锁定 commit 并写回 `compat.md`）

| 等级 | 应用 | 来源 | 主要考验 |
|---|---|---|---|
| L0 | `main-only`、`child-process`、`app-paths` | P00 自带夹具 `tests/electron/apps/` | 主进程 API、Node 模块、真实 `child_process` |
| L1 | electron-quick-start | `github.com/electron/electron-quick-start`（已归档，锁最后一个 commit） | 窗口、静态 HTML/CSS、preload、`process.versions` |
| L1 | Electron 官方 fiddle 示例若干 | `github.com/electron/electron` 的 `docs/fiddles/`（锁 tag `v39.2.0`），初始候选：`ipc/pattern-1`、`ipc/pattern-2`、`ipc/pattern-3`、`features/dark-mode`、`native-ui/dialogs`、`menus/customize-menus`、`system/clipboard`、`features/notifications`（实施时以该 tag 下实际存在的目录为准，逐个登记） | 各模块的典型用法 |
| L2 | electron-vite React 模板 | `npm create @quick-start/electron`（`alex8088/quick-start`，模板 `react-ts`），锁生成器版本 | Vite 产出的 ESM 渲染器、contextBridge、IPC |
| L2 | electron-vite Vue 模板 | 同上，模板 `vue-ts` | Vue 运行时、样式 |
| L2 | electron-forge Vite 模板 | `npx create-electron-app --template=vite-typescript`，锁 forge 版本 | forge start / make 流程 |
| L2 | electron-serve 应用 | `github.com/sindresorhus/electron-serve` 的用法写成最小应用（`app://` 加载打包好的 SPA） | `protocol.handle`、自定义协议加载 |
| L3 | electron-react-boilerplate | `github.com/electron-react-boilerplate/electron-react-boilerplate` | webpack 产物、多窗口、菜单 |
| L3 | Stretchly | `github.com/hovancik/stretchly` | 托盘、通知、多窗口、定时器、i18n |
| L3 | Abricotine | `github.com/brrd/abricotine` | CodeMirror 5 Markdown 编辑器、对话框、菜单 |
| L4 | Mark Text | `github.com/marktext/marktext` | 大型 Vue 编辑器、contenteditable、复杂布局 |
| L4 | Zettlr | `github.com/Zettlr/Zettlr` | CodeMirror 6 的布局测量、大量 IPC |
| L4 | Joplin 桌面版 | `github.com/laurent22/joplin`（`packages/app-desktop`） | 超大应用；依赖原生模块的部分需 P50 |

规则：
- 每个应用准备时**扫描 `node_modules` 里的 `.node` 文件和 `binding.gyp`**，有原生依赖的在表里标"需 P50"，
  不计入 v1 的失败。
- 清单可以替换（某应用无法离线构建、许可不合适等），替换时在 `compat.md` 记录原因。
- 闭源应用（如 Obsidian）只能在维护者本机按 P40 路径 C 手工验证，不进 CI、不提交任何其文件。

### 2. 准备与运行

- `tests/electron/compat/prepare.mjs <绝对路径的暂存目录> [应用名…]`（需新增）：用 **Node + npm/git** 在仓库外准备
  （准备阶段允许用 Node，运行阶段不需要）。对每个应用：`git clone` 到锁定 commit 或运行锁定版本的生成器 →
  `npm ci --ignore-scripts`（需要构建步骤的应用，显式列出允许运行的构建命令，如 `npm run build`）→ 产出可直接
  `electron <dir>` 运行的目录 → 写 `manifest.json`（应用名、commit、构建命令、入口、是否有原生模块、准备时间）。
- 环境变量 `VERVE_ELECTRON_COMPAT=<暂存目录>`；没设时相关测试打印 "skipped" 并通过（照 `tests/rebon.rs` 的 `rebon_or_skip!` 写法）。
- 测试：`crates/verve-electron/tests/compat.rs`（需新增），每个应用一个 `#[test] #[ignore]`，用 P00 的无头 Shell 启动，
  按该应用的**脚本化场景**断言：

```rust
// 每个应用一份场景，放在 tests/electron/compat/scenarios/<app>.toml（需新增）
// 示例字段：
//   wait = "ready-to-show"          # 或 did-finish-load / 某个 IPC 通道
//   timeout_ms = 10000
//   [[assert]] selector = "h1"  text_contains = "Hello World!"
//   [[assert]] eval = "window.versions.electron()"  equals = "39.2.0"
//   [[action]] click = "#open-dialog"   # 驱动输入（P00 的无头输入接口）
//   no_uncaught = true               # 主进程与渲染器都没有未捕获异常
```

  场景通过 P00 提供的无头控制接口执行（查询 DOM、执行脚本、注入输入）。若 P00 尚未提供某项能力，在本包
  PR 的"给其他包的备注"里提出，不要绕开 P00 自己造一套。
- 截图对比（可选，后做）：GUI 构建下对同一应用分别用真实 Electron 39.2.0 的 `webContents.capturePage()` 与 Verve 的同名 API
  （由 P13/P34 实现）截图，按感知差异阈值比较，结果只作报告不作门禁。

### 3. API 覆盖率

- **我们这边的名单**：`crates/verve-electron/src/api/registry.rs`（需新增）维护一张静态表，每个 Electron API 一行：
  `("BrowserWindow.prototype.setVibrancy", Status::NotSupported, "P13")`。注册原生函数的代码（各包）在注册时**同时登记**
  状态：`Done` / `Partial(note)` / `NotSupported` / 未登记即"未开始"。统一抛 `ERR_NOT_SUPPORTED` 的桩函数由
  registry 生成，保证名单与行为一致。
- 隐藏开关 `electron --verve-dump-api`（由 P10 的参数解析转交本包实现）：在主进程与渲染器两种 `require('electron')` 上
  遍历模块、类、原型、静态成员与事件名，结合 registry 输出 JSON：`[{ "name": "...", "process": "main|renderer", "status": "..." }]`。
- **官方名单**：`crates/verve-electron/examples/api_coverage.rs`（需新增）用 `oxc_parser` 解析锁定版本的官方
  `electron.d.ts`（取自 P40 打包的同一份文件），抽取：`Electron.Main`/`Electron.Renderer`/`Electron.CrossProcessExports`
  命名空间导出的模块常量、类与接口的方法/属性/静态成员、`on(event: '…')` 重载中的事件名。
- 比对输出：每个模块的覆盖率（完成/部分/不支持/未开始 计数与百分比），写入 `compat.md` 中
  `<!-- coverage:begin -->` 与 `<!-- coverage:end -->` 之间（脚本整体替换这段，不碰其余手写内容）。
- 运行：`cargo run -p verve-electron --example api_coverage -- --dts <path> --dump <dump.json> --write docs/electron/compat.md`。

### 4. `docs/electron/compat.md` 格式（需新增）

```markdown
# 兼容矩阵
更新于 <日期>，commit <短哈希>。

## 应用
| 等级 | 应用 | 锁定版本 | 状态 | 阻塞项 | 最近通过 |
|---|---|---|---|---|---|
| L1 | electron-quick-start | 1a2b3c4 | 通过 | — | 2026-11-20 |
| L3 | Abricotine | 5d6e7f8 | 部分 | P24 contenteditable；P22 calc() | — |

## API 覆盖率
<!-- coverage:begin -->
| 模块 | 完成 | 部分 | 不支持 | 未开始 | 覆盖率 |
|---|---|---|---|---|---|
| app | 41 | 3 | 2 | 6 | 79% |
<!-- coverage:end -->

## API 明细（手写补充：部分实现的差异、不支持的原因）
| API | 状态 | 包 | 测试 | 备注 |
|---|---|---|---|---|
| BrowserWindow.setVibrancy | 不支持 | P13 | — | 无平台材质效果 |
```

状态取值固定为：`完成` / `部分` / `不支持` / `未开始`（应用表另有 `通过` / `部分` / `失败` / `需 P50`）。

## 步骤

1. 写 `registry.rs` 与登记宏，补登已合并各包的 API（与各包实现者协调：此后新 API 必须登记，写进 `conventions.md` 的提议放在 PR 描述里）。
2. `--verve-dump-api` 与 `api_coverage` 示例，生成第一版覆盖率。
3. `prepare.mjs` + `manifest.json` + 场景格式 + `tests/compat.rs`，先跑通 L0、L1。
4. 每个里程碑合并后补齐下一级应用、更新 `compat.md`；失败项开 issue 或在对应包文档的「状态」里登记。
5. （可选）截图对比报告。

## 验收

- `node tests/electron/compat/prepare.mjs <dir> electron-quick-start` 离线可重复（第二次运行不联网也成功，或明确提示需要缓存）。
- `VERVE_ELECTRON_COMPAT=<dir> cargo test -p verve-electron --test compat -- --ignored electron_quick_start` 通过。
- `api_coverage` 对官方 `electron.d.ts` 抽取的 API 数与手工抽查一致（抽查 `app`、`BrowserWindow`、`ipcRenderer` 三个模块，误差为 0）；
  `compat.md` 的覆盖率段由脚本生成。
- 每个里程碑结束时 `compat.md` 已更新，L0–L当前级 全部"通过"或带明确阻塞项。

## 不做

修复兼容问题本身（交给对应包）；为验收应用打补丁绕过问题（禁止——应用必须原样运行）；闭源应用进 CI。

## 状态

（实现者完成后填写）
