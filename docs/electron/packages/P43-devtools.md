# P43 调试替代

**依赖**：P13（`webContents` 与其事件）；第二阶段依赖 P30/P34  **规模**：S（第一阶段）+ M（第二阶段检查器窗口）  **修改**：`crates/verve-electron/src/log.rs`（需新增）、`crates/verve-electron/src/api/web_contents.rs` 的 DevTools 方法、P10 参数表中 `--inspect*` 等条目、第二阶段新增 `crates/verve-gpui/src/inspector.rs`；`msg.rs` 追加变体（见契约 5）

## 目标

Verve 没有 Chromium DevTools，也不实现 Chrome DevTools 协议。本包提供够用的替代，让开发者能看到日志、
错误和页面结构：

1. 主进程与渲染器的 console 输出按 Electron 的规则到达终端和 `console-message` 事件，带颜色与来源。
2. 未捕获异常、未处理的 Promise 拒绝的展示与 Electron 39 一致。
3. `--inspect` 一类参数被接受并警告，不让应用的启动脚本失败。
4. `webContents.openDevTools()`：第一阶段做文本转储；第二阶段用 GPUI 画一个简易检查器窗口（元素树、计算样式、盒子高亮、控制台 REPL）。
5. `VERVE_ELECTRON_LOG`：按类别打开 Verve 自身的诊断日志。

## 背景（先读）

- `src/lib.rs`：`ConsoleLine`、`Level`、`Runtime::take_console`、`verve::report`/`describe`/`thrown_at`（错误的文本化与位置）。
- `src/cli.rs`：`verve run` 如何把 console 写到 stdout/stderr、如何处理未捕获异常与未处理拒绝（退出码 1）。主进程在
  Electron 下的行为**不同**（见契约 2），不要直接照搬退出逻辑。
- P02 的 `FromRenderer::Console`、`ToRenderer::ExecuteJavaScript`；P13 的 `webContents` 事件派发。
- P10 的命令行参数表（本包往里加 `--inspect` 等条目，由 P10 的解析器转交）。
- verve-dom：`Snapshot`（`snapshot.dom`、`snapshot.style(node, prop)`、`is_rendered`）与 `Document::set_layout` 回报的盒子——检查器的数据来源。

## 接口契约

### 1. console 转发

| 来源 | 去向 |
|---|---|
| 主进程 `console.*` | 总是写到进程的 stdout（log/info/debug）/ stderr（warn/error），格式与 `verve run` 相同（`util.inspect` 风格） |
| 渲染器 `console.*` | ① 总是作为 `FromRenderer::Console` 交给主进程，`webContents` 发 `'console-message'`；② 仅当设了 `ELECTRON_ENABLE_LOGGING=1`、命令行 `--enable-logging`，或 `VERVE_ELECTRON_LOG` 含 `console` 时，才写到终端 stderr（与 Electron 一致：默认不写终端） |

- `'console-message'` 事件按 Electron 39 的签名：事件对象带 `level`（`'info'|'warning'|'error'|'debug'`）、`message`、
  `lineNumber`、`sourceId`、`frame`；同时保留已弃用的位置参数 `(event, level, message, line, sourceId)`，其中 `level` 为数字 0–3。
  以真实 Electron 39 的观测为准并冻结进测试。
- 终端格式：`[renderer:<ContentsId> <源文件名>:<行>] <消息>`；stderr 是 TTY 时按级别着色（warn 黄、error 红、debug 暗），
  尊重 `NO_COLOR` 与 `FORCE_COLOR`。
- Windows 子系统为 `windows` 的 exe 默认没有控制台：`--enable-logging` / `ELECTRON_ENABLE_LOGGING` 时 `AttachConsole(ATTACH_PARENT_PROCESS)`
  （与 P40 第 6 节一致，二者共用一个函数，放在本包的 `log.rs`）。

### 2. 未捕获错误

以真实 Electron 39 的行为为准，实现前先用 Electron 跑一遍下列情形并把输出冻结进测试：

| 情形 | Electron 的行为（实现前核实） | Verve |
|---|---|---|
| 主进程未捕获异常，没有 `process.on('uncaughtException')` | 弹出错误对话框 "A JavaScript error occurred in the main process" 附堆栈，进程继续 | 同样：stderr 打印 + `dialog.showErrorBox`（P14）；无头模式只打印 |
| 主进程未处理拒绝 | 按 Node 22 默认（打印警告或作为未捕获异常，以观测为准） | 与观测一致 |
| 渲染器未捕获异常 / 未处理拒绝 | 作为 `Uncaught …` 错误进入 console（因此触发 `console-message`），页面继续 | 同样 |
| 渲染器 isolate panic 或被中断 | `render-process-gone` | P02 的 `FromRenderer::Gone`；本包负责 stderr 上打印一行原因 |

### 3. 被接受并警告的参数

`--inspect[=port]`、`--inspect-brk[=port]`、`--inspect-port=`、`--inspect-publish-uid=`、`--remote-debugging-port=`、
`--remote-debugging-pipe`、`--js-flags=`、`--enable-logging[=file]`（这个真正生效）、`--log-level=`、`--v=`、`--vmodule=`。

- 每个不支持的参数在启动时 stderr 打印**一行**：`Verve: --inspect is not supported; see VERVE_ELECTRON_LOG and webContents.openDevTools()`。
- P40 的 fuse `EnableNodeCliInspectArguments` 关闭时，`--inspect*` 按 Electron 行为处理（以观测为准）。
- 这些参数不进入 `process.argv` 的应用部分的规则与 Electron 相同（Chromium 开关会被 Electron 吞掉；以观测为准）。

### 4. `VERVE_ELECTRON_LOG`

- 语法类似 `RUST_LOG`：逗号分隔的 `类别[=级别]`，级别 `error|warn|info|debug|trace`，默认 `info`；`all` 表示全部类别。
  例：`VERVE_ELECTRON_LOG=ipc,load=debug`。
- 类别：`console`（渲染器 console 写终端）、`ipc`、`lifecycle`（窗口与页面事件）、`load`（URL、协议、资源加载）、`shell`（GPUI 窗口操作）、
  `input`、`perf`（与 P42 的追踪共用时间源）、`asar`、`protocol`。
- 输出到 stderr；`VERVE_ELECTRON_LOG_FILE=<path>` 时追加写文件。
- 实现：`log.rs` 里一个极小的宏与全局过滤表（解析一次，`OnceLock`），**不引入** `tracing`/`env_logger` 等依赖（体积）；
  关闭的类别只是一次原子读。

### 5. `openDevTools` 与检查器

方法与事件：`webContents.openDevTools([options])`、`closeDevTools()`、`toggleDevTools()`、`isDevToolsOpened()`、
`isDevToolsFocused()`、`devToolsWebContents`（`null`）、`inspectElement(x, y)`、事件 `'devtools-opened'`、`'devtools-closed'`、
`'devtools-focused'`；菜单角色 `toggleDevTools`（P15）调用同一实现。

**第一阶段（v1 必做）**：`openDevTools()` → 发 `'devtools-opened'`，把该页面的元素树转储到 stderr（每行：缩进、标签、
`#id`、`.class`、布局盒子 `x,y,w,h`、`display`），并为该 `webContents` 打开渲染器 console 的终端转发；
`isDevToolsOpened()` 为 `true` 直到 `closeDevTools()`。`options.mode` 等参数接受并忽略。

**第二阶段**：`openDevTools()` 打开一个 GPUI 窗口（`crates/verve-gpui/src/inspector.rs`）：
- 左：元素树（来自该渲染器最新 `Snapshot`，可折叠，随 `Changes` 增量刷新）；
- 右：选中节点的计算样式（`snapshot.style`）与盒模型图（来自 Shell 已有的布局结果）；
- 页面窗口上高亮悬停/选中的节点（Shell 自己画覆盖层，不经过渲染器）；元素拾取模式（点页面选节点）；
- 底部：console 列表 + REPL 输入框。

为 REPL 在 `msg.rs` **追加**：

```rust
// 主进程/Shell → 渲染器
ToRenderer::InspectorEval { call: u64, code: String }
// 渲染器 → Hub（转给检查器所在的 Shell）
FromRenderer::InspectorEvalReply { call: u64, text: String, is_error: bool }  // text 为 util.inspect 的结果
// 主进程 → Shell
ToShell::Inspector { contents: ContentsId, open: bool }
```

REPL 在页面的主世界执行（`contextIsolation` 下不进入 preload 的隔离世界），结果用 Verve 的 `util.inspect` 格式化成文本后再跨线程，
不传活对象。

## 步骤

1. `log.rs`：过滤表、宏、着色、`AttachConsole`；接入各包已有的诊断点（只加调用，不改它们的逻辑）。
2. console 转发与 `'console-message'`；先用真实 Electron 冻结事件签名与终端输出规则。
3. 未捕获错误的四种情形，冻结 Electron 的观测后实现。
4. 参数表条目与警告。
5. 第一阶段 `openDevTools` 文本转储。
6. （第二阶段）检查器窗口与 REPL 消息。

## 验收

- 无头测试：渲染器 `console.warn('x')` 使主进程收到 `'console-message'`（事件对象与位置参数都对）；未设日志开关时终端无输出，
  设 `ELECTRON_ENABLE_LOGGING=1` 后有带来源的输出。
- 主进程抛未捕获异常：进程不退出，stderr 有堆栈，GUI 构建下出现错误对话框。
- `electron --inspect=9229 .` 正常启动，stderr 恰好一行警告。
- `VERVE_ELECTRON_LOG=ipc` 时每次 IPC 有一行日志；不设时 IPC 基准（P42）无可测差异。
- 第一阶段：`openDevTools()` 输出元素树，`isDevToolsOpened()` 正确。第二阶段：检查器窗口能浏览树、看样式、高亮、执行 `document.title` 并显示结果。

## 不做

Chrome DevTools 协议、断点调试、性能面板、网络面板、源码映射；在 REPL 里传递活对象。

## 状态

（实现者完成后填写）
