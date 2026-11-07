# P43 调试替代

**依赖**：P13（`webContents` 与其事件）；第二阶段依赖 P30/P34  **规模**：S（第一阶段）+ M（第二阶段检查器窗口）  **修改**：`crates/verve-electron/src/log.rs`（需新增）、`crates/verve-electron/src/api/web_contents.rs` 的 DevTools 方法、P10 参数表中 `--inspect*` 等条目、第二阶段新增 `crates/verve-gpui/src/inspector.rs`；`msg/page.rs` 追加变体（见契约 5）

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
| 渲染器 `console.*` | ① 总是作为 `FromRenderer::Console` 交给主进程，`webContents` 发 `'console-message'`；② 仅当设了 `ELECTRON_ENABLE_LOGGING`（任何非空值）、命令行 `--enable-logging`，或 `VERVE_ELECTRON_LOG` 含 `console` 时，才写到终端 stderr（与 Electron 一致：默认不写终端） |

- Electron 39.8.10（Windows）观测 `ELECTRON_ENABLE_LOGGING`：设为 `1`、`0`、`false` 都开启日志（Chromium 写 10 行 `INFO:CONSOLE`），空值等于没设，不写（`expected/logging-env.txt`）。
- Electron 39.8.10 观测（`tests/electron/devtools/`）：事件对象自有可枚举属性恰为 `_level`（数字）、`frame`（即 `webContents.mainFrame`）、`level`、
  `lineNumber`、`sourceId`、`message`（`expected/own-keys.txt`）；监听器再收到 `(event, level, message, line, sourceId)`，`level` 为数字 debug 0、info 1、warning 2、error 3。
  任一 `console-message` 监听器声明了多于一个形参时，主进程 stderr 写一次 `(electron) 'console-message' arguments are deprecated and will be removed. Please use Event<WebContentsConsoleMessageEventParams> object instead.`。
  `message` 是 Chromium 的文本（对象为 `[object Object]`，不做 `%s` 替换）；未捕获错误是 `Uncaught Error: page boom`，未处理拒绝是 `Uncaught (in promise) Error: page reject`，
  级别都是 error，`lineNumber`/`sourceId` 指向抛出处。Verve 的 `message` 是它自己的 `util.inspect` 文本（见 P43「状态」）。
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
- 这些参数留在 `process.argv` 里：Electron 39.8.10 观测，`--inspect=127.0.0.1:0 --remote-debugging-port=0 --js-flags=… --log-level=3 --v=0 --vmodule=x=0 --inspect-port=0 --enable-logging=stderr` 全部原样在 `process.argv`（应用路径之前）。真 Electron 的 `--inspect` 还会在 stderr 写 Node 的两行 `Debugger listening on ws://…` 与 `For help, see: https://nodejs.org/en/docs/inspector`（`expected/inspect.txt`）；Verve 只写一行警告。每个开关名警告一次，`--enable-logging` 不警告。

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

为 REPL 与检查器窗口在 `msg/` **追加**（P02 同步记录）：

```rust
// Shell（检查器窗口）→ 渲染器，经 Hub::to_renderer
ToRenderer::InspectorEval { call: u64, code: String }
// 渲染器 → Hub；Hub 不交给主进程，转给检查器所在的 Shell
FromRenderer::InspectorEvalReply { call: u64, text: String, is_error: bool }  // text 为 util.inspect 的结果
// 主进程 → Shell：开/关某页的检查器窗口
ToShell::Inspector { contents: ContentsId, open: bool }
// Hub → Shell：上面那条答复，窗口按 call 找回它问的一行
ToShell::InspectorReply { contents: ContentsId, call: u64, text: String, is_error: bool }
// Shell → 主进程：窗口自己的事
FromShell::Inspector { contents: ContentsId, event: InspectorEvent }   // Closed | Focused | Blurred
```

REPL 在页面的主世界执行（`contextIsolation` 下不进入 preload 的隔离世界），结果用 Verve 的 `util.inspect` 格式化成文本后再跨线程，
不传活对象；求值本身不写页面的 console（不触发 `'console-message'`）。

- 无头 Shell 不画窗口：`ToShell::Inspector` 只被记下（`HeadlessShell::open_inspectors()`），第一阶段的文本转储照旧由页面写；
  `ToShell::InspectorReply` 也被记下（`inspector_replies()`），供测试读。无头 Shell 可以模拟用户关窗和聚焦（`close_inspector`、`focus_inspector`）。
- 检查器窗口读的数据：页面每次发布的 `RendererView.update`（Hub 在 Shell 来不及读时把 `Changes` 合并而不是丢弃）和
  `display_list`，以及 Hub 为每页留的最近 500 条 console（`Hub::console_since`）。
- 窗口开着时 `isDevToolsOpened()` 为 `true`（第一阶段的语义）；`'devtools-closed'` 在用户关窗口或 `closeDevTools()` 时发，
  `'devtools-focused'` 在窗口得到焦点时发，`isDevToolsFocused()` 随焦点；`devToolsWebContents` 仍为 `null`。

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

### 第一阶段（已完成）

做了：
- `log.rs`：`VERVE_ELECTRON_LOG` 的过滤表（`Filter`，`OnceLock` 里的进程级 `Log`，关闭的类别只一次原子读，无新依赖）、`diag!` 宏、
  颜色（Node 22 `lib/internal/tty.js` 的顺序：`FORCE_COLOR` 为空/`1`/`true`/`2`/`3` 开、其它值关；没有它时有 `NO_COLOR`（含空值）则关；否则看是否 TTY）、`VERVE_ELECTRON_LOG_FILE`、`AttachConsole(ATTACH_PARENT_PROCESS)`
  （`attach_parent_console`，Windows 才有实现；任何东西要写终端时——日志开关、`VERVE_ELECTRON_LOG` 的任一类别、开关警告——由 `launcher::boot` 调用，P40 切到 GUI 子系统后沿用）。
  测试用 `Log::to_memory` 读终端输出，`Hub::set_log` 换上。
- `'console-message'`（`api/console_message.rs`）：事件对象 + 已弃用的位置参数 + 一次性弃用警告，全按 Electron 39.8.10 的观测
  （`tests/electron/devtools/expected/`）。渲染器 console 写终端的条件：`ELECTRON_ENABLE_LOGGING`（任何非空值）、`--enable-logging`、
  `VERVE_ELECTRON_LOG` 含 `console`，或该页 DevTools 开着；格式 `[renderer:<id> <文件名>:<行>] <消息>`，按级别着色。
- 未捕获错误：渲染器的 `Uncaught …`/`Uncaught (in promise) …` 进 console（因此 `console-message`），带抛出处的行号与文件（取自错误 `stack` 的第一个脚本帧，
  具名函数的 `at f (file:l:c)` 与匿名函数、顶层的 `at file:l:c` 两种写法都认），不再把栈拼进文本；抛出的字符串不带引号。
  主进程（没有 `Profile`、`on_uncaught` 为 `Report` 的 realm）：`lib.rs` 的 `report()` 在 `uncaughtException` 监听器之后，把栈写到 stderr（`process::print_uncaught`），
  并把 Electron `lib/browser/init.ts` 的文本 `Uncaught Exception:\n` + `e.stack`（无栈时 `${name}: ${message}`）交给 shell 的错误框；`verve run` 的路径（有 `Profile`）不变。
  GUI shell 在 `VERVE_NO_SYSTEM_DIALOGS` 下不弹框，并像 headless shell 一样在 stderr 写一行 `[app] error box …`（此前只有无窗口的系统框遵守它）。
  渲染器崩溃/被中断的 stderr 一行原因是 P13 已有的（`main_guest.rs` 的 `Gone` 分支）。
- 参数：`--inspect`、`--inspect-brk`、`--inspect-port`、`--inspect-publish-uid`、`--remote-debugging-port`、`--remote-debugging-pipe`、`--js-flags`、
  `--log-level`、`--v`、`--vmodule` 每个名字启动时在 stderr 一行警告（`launcher::unsupported_switch_warnings`），仍在 `process.argv`/`app.commandLine`。
- `VERVE_ELECTRON_LOG` 接线（只加调用，`hub/diagnostics.rs`）：`ipc`（每条 IPC 一行）、`lifecycle`（页面事件与窗口事件）、`load`（`Load`/`Reload`）、
  `shell`（双向消息）、`input`、`protocol`（请求与回答）、`asar`（打包应用）、`console`。`perf` 类别能解析，但没有现成的诊断点可接。
- `openDevTools()` 等（`api/devtools.rs`、`inspector.rs`、`ToRenderer::InspectTree`）：见下。`webPreferences.devTools: false` 时与 Electron 一样什么也不做（观测）。
  `inspectElement(x, y)` 只打印该点元素的一行，不改变开关状态。`'devtools-opened'`/`'devtools-closed'` 在调用返回后的下一轮发。`devTools: false` 的窗口上 `inspectElement` 也什么都不做。
- `console-message` 的弃用警告现在走新加的 `define::deprecate_once`（一次、`process.noDeprecation` 时不写）；`contents_history` 的旧导航方法警告也改走它（原来是 `eprintln!`，现在写主进程 console）。
- 内核 `Cloned::array_len`：IPC 诊断行的参数个数。

没做：主进程的未处理拒绝（合同 2 第 2 行）：`src/hooks.rs` 里它仍只是一行 console，没有错误框、也不发 `uncaughtException`；Node 22 的 throw 模式加上 `init.ts` 装的监听器本会弹框。第二阶段（GPUI 检查器窗口、REPL 的 `InspectorEval` 消息，等 P30 合并）；`'devtools-focused'`（DevTools 从不聚焦）；`perf` 类别的接线；
P40 的 fuse `EnableNodeCliInspectArguments` 关闭时的行为。

和 Electron 的已知差异：
- `'console-message'` 的 `message` 是 Verve 的 `util.inspect` 文本（`console.log('plain', 1, {a: 1})` 得 `plain 1 { a: 1 }`、`%s` 会替换），Electron 是 Chromium 的
  文本（`plain 1 [object Object]`、不替换）。内核的 console 只把格式化后的文本交给 sink。
- 行号是脚本文本内的行：内联 `<script>` 紧跟标签时与 Chromium 的文档行号一致，脚本前有换行时不同；拒绝原因不是 `Error` 时（`Promise.reject('x')`）没有位置（行号 0，`sourceId` 空），Electron 给拒绝处的行。
- 终端格式是文档约定的 `[renderer:<id> <文件>:<行>] …`，不是 Chromium 的 `[pid:时间:INFO:CONSOLE:行] "…", source: … (行)`（Electron 把所有级别都写成 INFO）；已冻结在 `expected/console-logging.txt`。
- `--inspect` 在 Electron 里还写 Node 的两行 `Debugger listening on …`（`expected/inspect.txt`），Verve 只写一行警告。
- `--enable-logging=file`、`ELECTRON_ENABLE_LOGGING=file` 在 Electron 里写文件，Verve 仍写 stderr（P10 的开关表里同样记了）。
- `inspectElement` 不会像 Electron 那样打开 DevTools，只打印一行。
- 没有活着观测 `openDevTools()` 本身（会在用户桌面上弹出 DevTools 窗口）：事件名与开关语义来自文档与 `devTools: false` 的观测；没有探测主进程未捕获异常（会弹错误框），
  改读 electron.exe 里的 `init.ts`（`expected/main-uncaught.txt`）；`--inspect-brk` 会一直等调试器，未探测。

文本转储的格式：第一行 `[devtools:<id>] element tree of <url>`，之后每个元素一行：两个空格的缩进（每层）、标签、`#id`、`.class`（每个）、盒子 `x,y,宽,高`
（取整到 0.1 px，页面没有给它画盒子时是 `-`）、`display` 的计算值。盒子来自 `Document::rect`（新加在 verve-dom 的 `rect.rs`），它经 `laid()` 读 P30 的布局提供者（先按需重新布局），所以 DOM 改动后几何是新的。

越界的改动（任务书「修改」之外）：内核的提交（`src/uncaught.rs`、`src/process.rs`、`src/lib.rs`、`src/hooks.rs`、`src/embed.rs`、`src/clone.rs`）、verve-dom 的 `rect.rs`、verve-gpui 的 `dialog.rs` 与新测试文件 `crates/verve-gpui/tests/electron_debug.rs`、`crates/verve-gpui/tests/support/mod.rs`，各在自己的提交里，不与 verve-electron 的改动混在一起：
- 内核：console sink 的第三个参数现在真有值（调用 console 的脚本的位置、未捕获错误的抛出处，在新的 `src/uncaught.rs` 里），
  未捕获错误的 console 行在有 sink 时不再附 `(栈)`；`src/process.rs`/`src/lib.rs`：主进程错误框的文本与 stderr 的栈（上面）；`tests/embedder_hooks.rs` 加了测试。
- `crates/verve-dom/src/rect.rs`：`Document::rect`（读 P30 的布局）。`crates/verve-gpui/src/dialog.rs`：`VERVE_NO_SYSTEM_DIALOGS` 下不弹错误框、改写一行。
- `crates/verve-electron/Cargo.toml`：`windows-sys` 加 `Win32_System_Console`（不动 `Cargo.lock`）。

给其他包：
- P40：`verve_electron::log::attach_parent_console()` 已在 `launcher::boot` 里调用，GUI 子系统的 exe 不需要再写一份。
- P42：IPC 路径上多了每条消息一次 `Hub::log()` 的读取和一次原子读（`hub/diagnostics.rs`），未设 `VERVE_ELECTRON_LOG` 时不格式化任何东西；请用 IPC 基准确认无可测差异。
- P30：已在变基时完成（P43 现在建在 df018328 上）。
  - `Document::rect` 改为 `self.laid().rects.get(&node).copied()`；P30 没有现成的公开读取（`laid()` 是 `pub(crate)`），所以没有加第二份。`inspectElement` 的 `hit_test` 本来就走 `laid()`。
  - `tests/devtools.rs` 的 `open_dev_tools_dumps_…` 现在断言 verve-layout 配 headless 度量器给出的盒子（窗口 800 x 600，视口取窗口外框的尺寸）：`html 0,0,800,32`（内容 16 + body 上下各 8 的外边距）、
    `body 8,8,784,16`、`div#main`/`p` 同为 `8,8,784,16`（测试页给 `p` 写了 `margin:0`，以免 1em 的外边距穿过 div 与 body 折叠而依赖折叠细节）、`head`/`script` 是 `-`（`display: none`）；
    (1,1) 在 body 的外边距里，命中 `html`。数字的由来写在测试的注释里。
  - `tests/preload.rs` 的 `KNOWN_DIFFERENCES` 删去了「未处理拒绝的行带栈」一条：内核现在在有 sink 时不再附栈，渲染器的行与录制的 `Uncaught (in promise) Error: nope` 相同。未跑测试确认，请中央跑 `bridge_matrix`。
  - 第二阶段的 `InspectorEval` 在这里没有加。
