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
| 主进程未处理拒绝 | Electron 39.8.10 观测：不是 throw 模式。stderr 两条 `UnhandledPromiseRejectionWarning`（原因的栈、Node 的建议文字带 `rejection id`），第一条后附 ``(Use `electron --trace-warnings ...` to show where the warning was created)``；不发 `uncaughtException`，不弹错误框，进程继续（`expected/rejection-*.txt`） | 同样（`fix(kernel)`，第二阶段） |
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

没做（第二阶段已补上前三项，见下）：主进程的未处理拒绝（合同 2 第 2 行）；GPUI 检查器窗口与 REPL 的 `InspectorEval` 消息；`'devtools-focused'`；`perf` 类别的接线；
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

### 第二阶段（已完成，窗口只在代码层面验证过，见「手动检查」）

做了：
- **契约**（P02 同步）：`ToRenderer::InspectorEval { call, code }`、`FromRenderer::InspectorEvalReply { call, text, is_error }`、`ToShell::Inspector { contents, open }`，
  外加把答复从 Hub 送到窗口的 `ToShell::InspectorReply { contents, call, text, is_error }`，和窗口自己的事 `FromShell::Inspector { contents, event }`（`InspectorEvent::{Closed, Focused, Blurred}`）。
  Hub 收到 `InspectorEvalReply` 后只转给 Shell，主进程听不到（`hub/inspector.rs`）。
- **REPL 路径**（`inspector.rs::eval`）：页面在主世界（`MAIN_WORLD`，不是 preload 的隔离世界）把代码当脚本求值，结果经内核新公开的 `verve::inspect_value`（`util.inspect(value)` 的文本，字符串带引号，`document.title` 得 `'Hello'`；不是 `console.log` 用的 `util.format`，那个会把单个字符串原样给出）变成字符串；
  抛出的错误同样 inspect，`is_error` 为真；求值不写页面 console，也不触发 `'console-message'`。返回 Promise 时显示求值那一刻的状态（`Promise { 3 }`、`Promise { <pending> }`），微任务在答复生成之后才跑。
  `var` 声明留在页面里，下一行能用。无头测试 `tests/inspector.rs` 走 Hub，不开窗口。
- **Hub 给检查器的数据**：`Hub::console_since(contents, after)`（每页最近 500 条 console，带递增序号，导航时清空、序号不回头）、`Hub::set_devtools_focused`/`is_devtools_focused`；
  页面发布的 `RendererView` 在 Shell 没来得及读时被新的取代，现在取代前把旧的更新并进新的（`carry_changes`：新的没有更新就拿旧的，两者都有就合并 `Changes` 并给 `removed` 去重），所以检查器的增量更新不丢改动。
  REPL 的 `call` 由 Hub 一个计数器编号（`Hub::next_inspector_call`），同一个页面的窗口先后开关也不会重号，旧窗口的迟到答复不会落进新窗口。页面的窗口被销毁时 Hub 同时丢掉它的 console 环、焦点标志和 `devtools` 集合；每条保存的 console 文本最多 4000 个字符。
  `Hub::page_generation(contents)` 是页面当前文档的代数（每次导航，同地址重载也算），检查器靠它发现新文档：重置选中/悬停/折叠选择、清空 console 和等待中的提问。
- **模型**（`crates/verve-gpui/src/inspector/`，纯 Rust，各有单元测试）：`tree.rs`（可折叠的元素树，行来自 `Snapshot`，按 `Changes` 只重做改动节点的行；换了地址或改动过半才整棵重建；
  折叠状态由用户的选择记住，`head`/`script`/`style`/`svg`/`template` 起初折叠；一个改动的节点若没有行（纯空白文本、文档的子节点），就让它父节点的行重做，父节点是文档时整棵重建，所以文本在空白与非空白之间变、`<html>` 被替换、节点在两个父节点间移动，都与整棵重建的结果相同）、`styles.rs`（选中节点的计算样式，按名字排序）、`boxmodel.rs`（边框盒来自 `DisplayList` 里画出的 `Box`，否则取 `Hitbox`，按 Shell 已滚动的偏移放进窗口；
  margin/padding 来自计算样式，border 优先取画出的边框宽度；百分比与 `auto` 那一边没有宽度（`Option<f32>`），图里显示样式原文 `auto`/`10%`，高亮里不画那一圈，padding 有这样的边时 content 的大小未知，既不画也显示 `?`）、`console.rs`（页面行、REPL 的输入与输出，多行的值每行一行；行编辑器和最多 200 行的历史）、`model.rs`（一个窗口对页面的全部了解）。
- **窗口**（`view.rs`、`windows.rs`，GPUI 自带元素，无新依赖）：左边元素树（`uniform_list`，点箭头折叠，点行选中，方向键移动/折叠），右上盒模型图，右下计算样式，底部 console 与 REPL 输入（Enter 发送，上下键翻历史，不碰剪贴板，所以没有粘贴）。
  `openDevTools()` → `ToShell::Inspector { open: true }` → 开窗口并带上页面最近发布的快照；`closeDevTools()`、`toggleDevTools()`、用户关窗口、页面窗口被销毁都关它。
  窗口得到焦点 → `FromShell::Inspector { Focused }` → `'devtools-focused'`，`isDevToolsFocused()` 随焦点；失去焦点只清掉标志。`devToolsWebContents` 仍为 `null`。真 Shell（`run_with`）对 Hub 说 `show_inspector_in_window()`：此后 `openDevTools()` 不再让页面把元素树文本写到终端，DevTools 打开时页面的 console 也不再转到终端（Electron 什么也不写）；无头 Shell 不说，保持第一阶段的文本转储，另外记下 `ToShell::Inspector`/`InspectorReply`
  （`open_inspectors()`、`inspector_replies()`；页面窗口销毁时也丢掉它的检查器），并能模拟用户关窗口和聚焦（`close_inspector`、`focus_inspector`）。
  窗口的开与关各写一行 `VERVE_ELECTRON_LOG=shell` 的日志（`inspector window of page N opened`/`closed`），打不开窗口写 error 级一行 `no inspector window for page N: …`；真窗口测试读它。
  Shell 给每个检查器窗口一个序号：用户关窗口时只有序号仍是当前那个才通知主进程，所以已被 `closeDevTools()` 关掉的旧窗口迟到的关闭不会把重新打开的窗口标成已关（没有改 `FromShell::Inspector` 的形状，序号只在 Shell 里）。
  窗口一建好就读一次 `is_window_active()`，不漏掉第一次激活；回复到达时先取页面新写的 console 行再显示答案，所以代码写的行在答案之前。
- **高亮与拾取**（`overlay.rs`，在 `dom_view.rs` 里作为页面元素之上的独立元素，没碰 `dom_element.rs`/`plan.rs`）：悬停或选中的节点在页面窗口上画 content/padding/border/margin（DevTools 的颜色），
  盒子每帧从 Shell 自己的 `DisplayList`（含滚轮预测的偏移）查，不问渲染器。拾取模式：页面窗口上盖一层十字光标的元素，移动时用 `DisplayList::hit_test` 找节点并高亮，点击选中并退出拾取模式。P32 合并后，输入桥若也处理页面窗口的鼠标，需要让拾取层优先。
- **主进程未处理拒绝**（内核，`fix(kernel)`）：见下面的观测——Electron 39.8.10 不是 throw 模式。`src/uncaught.rs::warn_unhandled_rejection`：无 `unhandledRejection` 监听者时，宿主上报未捕获错误的 realm（Electron 主进程）
  写两条 `process.emitWarning`：`UnhandledPromiseRejectionWarning: <栈或原因>` 和 Node 22 的那段建议文字，第一条警告后附一行 ``(Use `<argv0 去掉 .exe> --trace-warnings ...` to show where the warning was created)``
  （第一条警告是 `DeprecationWarning` 时旗标是 `--trace-deprecation`；只在这类宿主里附，`verve run` 不变）。`rejection id` 在 `hooks.rs` 记下拒绝时就编号（Node 给每个拒绝时没有处理器的 promise 编号，同一回合里随后被处理的也占一个号：`Promise.reject(e1).catch(() => {}); Promise.reject(e2)` 的第二条是 `id: 2`），号跟着 promise 走。
  已警告的拒绝之后才被处理、又没有 `rejectionHandled` 监听者时，再写一条 `PromiseRejectionHandledWarning: Promise rejection was handled asynchronously (rejection id: N)`。原因按 V8 的 `noSideEffectsToString` 写（逐个在 Electron 39.8.10 里观测）：自己带字符串 `stack` 的取 `stack`；函数写源码，BigInt 写数字（无 `n`）；`toString` 是 `Object.prototype.toString` 的写 `#<构造器名>`（`Map` 得 `#<Map>`，有 `@@toStringTag` 也是 `#<Object>`），是 `Error.prototype.toString` 的写 `Name: message`，其余写 `[object 标签]`（`Date`、`RegExp`、数组、无原型对象的 `[object Object]`）；只读数据属性，不跑 getter（构造器是 getter 时得 `[object Object]`）。Electron 的模式是 `warn-with-error-code`（不是 `warn`：`warn` 在有 `unhandledRejection` 监听者时也写警告，Electron 不写，见 `rejection-handled.txt`）：没人听的拒绝让 `process.exitCode` 变成 1，有监听者时仍是 `undefined`。同一回合里先告知晚处理的（`rejectionHandled`/警告），再处理新的拒绝。不发 `uncaughtException`，不弹错误框，进程继续。`'warning'` 事件照常发。

Electron 39.8.10（Windows）观测，冻结在 `tests/electron/devtools/expected/` 和对应测试里：
- 以下对象原因、`rejection id` 编号、`PromiseRejectionHandledWarning` 的文字与 DeprecationWarning 的旗标是复核时在同一个 Electron 39.8.10 里观测到的（`tests/embedder_hooks.rs` 里各有一条测试，冻结了文字）。
- `rejection-*.txt`（`tests/electron/devtools/rejection`）：主进程 `Promise.reject(new Error('main reject'))` 无监听者时，stderr 是两条 `(node:<pid>) UnhandledPromiseRejectionWarning`（栈；`Unhandled promise rejection. This error originated … (rejection id: 1)`），
  中间一行 ``(Use `electron --trace-warnings ...` to show where the warning was created)``；`showErrorBox` 没被调用，`uncaughtException` 监听者没被触发（启动时 `listenerCount('uncaughtException')` 为 1，是 `init.ts` 装的）。
  拒绝原因是字符串时第一条是 `UnhandledPromiseRejectionWarning: plain reason`。有 `unhandledRejection` 监听者时它收到 `(reason, promise)`，stderr 无输出。**所以任务书说的「Node 22 throw 模式 + init.ts 弹框」在 Electron 39.8.10 不成立**，已按观测实现。
- `events-right.txt`（`openDevTools({ mode: 'right' })`，`show: false` 窗口里停靠，屏幕上什么也没出现）：`openDevTools()` 返回时 `isDevToolsOpened()` 仍为 `false`，之后 `'devtools-opened'` 里为 `true` 且 `devToolsWebContents` 是对象；
  `closeDevTools()` 在调用里同步发 `'devtools-closed'`，返回后 `isDevToolsOpened()` 为 `false`、`devToolsWebContents` 回到 `null`；`toggleDevTools()` 同理；停靠模式没有 `'devtools-focused'`。
- `events-detach.txt`（`mode: 'detach'`，只为看焦点，探针自己关掉窗口）：第一次打开时 `'devtools-focused'`（此时 `isDevToolsFocused()` 为 `true`）**先于** `'devtools-opened'`；约 3 秒后焦点移走，`isDevToolsFocused()` 为 `false`；第二次打开没有聚焦事件（取决于桌面前台规则）。
- 和 Electron 的已知差异（`tests/inspector.rs` 里各有注释）：`isDevToolsOpened()` 在 Verve 里调用返回就是 `true`（第一阶段语义，任务书要求保持）；`'devtools-closed'` 在调用的下一轮发，Electron 在调用里发；`devToolsWebContents` 恒为 `null`；
  `'devtools-focused'` 在窗口显示、得到焦点之后发，晚于 `'devtools-opened'`，Electron 的分离窗口里在它之前。

测试（我没有跑，只做了 `cargo check`）：
- 内核：`tests/embedder_hooks.rs`（两条警告、不发 `uncaughtException`、不通知宿主；非 Error 原因；有监听者时无警告；拒绝按拒绝时编号；晚处理的那条警告；对象原因），`src/uncaught.rs`（建议文字、两种旗标的提示文字）。
- verve-electron：`tests/inspector.rs`（REPL 全部路径、主世界与隔离世界、抛错、不写 console、Promise；DevTools 事件与状态；`devTools: false`；主进程拒绝），`hub/inspector.rs`（console 环、序号、截断、计数器、焦点），`hub/renderers.rs`（`carry_changes`）。
- verve-gpui：`inspector::{tree,styles,boxmodel,console,model,overlay}` 的单元测试；忽略的真窗口测试 `tests/inspector_window.rs`（把 `electron` 二进制当子进程跑，读它的退出码、stdout 和 `VERVE_ELECTRON_LOG=shell` 的日志；原先在测试进程里 `run_with` 的写法在测试体之后偶发 `0xc0000005`，已弃用）。

手动检查（真窗口，`cargo run -p verve-gpui --features jit --bin electron -- <有 openDevTools() 的应用>`；或跑 `tests/inspector_window.rs`）：
1. 打开：`webContents.openDevTools()` 开出标题为 `DevTools - <页面地址>` 的窗口，左边有 `<html>`、`<head>…</head>`（折叠）、`<body>`…。
2. 折叠：点 `v`/`>` 箭头折叠与展开；选中行后按左/右键折叠/展开，上/下键换行。
3. 选中：点一行，右上出现盒模型图（margin/border/padding/content 的数字和内容尺寸），右下是按名字排序的计算样式。
4. 高亮：鼠标移过树里的行，页面窗口上出现该节点的四层盒子（橙 margin、黄 border、绿 padding、蓝 content）；选中后鼠标移开仍保留选中节点的高亮；页面滚动时高亮跟着走。
5. 拾取：点工具栏的 `Pick element`，在页面窗口上移动（十字光标），节点被高亮；点击后树里选中并滚到那一行，拾取模式结束。
6. 求值：在底部输入框点一下，输入 `document.title` 回车，下一行出现带引号的 `'<标题>'`（字符串带引号）；输入 `({a: 1, b: [1, 2]})` 得到一行 `{ a: 1, b: [ 1, 2 ] }`，值很长换行时每行一行；输入 `throw new Error('x')` 得到红色的 `Error: x…`；输入 `console.log('hi')` 后 console 里出现页面的那一行；上/下键翻历史。
7. 页面变化：让页面改 DOM（如 `appendChild`），树里只出现对应的行，已折叠的节点保持折叠。
8. 关闭：关窗口（或页面里 `closeDevTools()`）后 `isDevToolsOpened()` 为 `false`，页面窗口上的高亮消失；关页面窗口时检查器跟着关；页面重载后树、选中和 console 回到新文档的状态，终端上没有元素树文本。

没做 / 已知问题：
- 关窗口的竞态，窗口序号盖不住：Shell 已经发出的 `Closed`，在主进程重新打开 DevTools 之前到达时，仍会把新窗口标成已关。要解决需要主进程给窗口一个身份（`FromShell::Inspector` 带它），这一轮没改契约。
- `verve run`（Node 默认模式）在监听者听到拒绝、之后才处理它时也会写 `PromiseRejectionHandledWarning`；`hooks.rs` 只在宿主上报未捕获错误的 realm 里写，`verve run` 没有。
- 新文档的判定：页面调用和 `Latest` 都带 `Hub::page_generation`（在取 `renderer_view()` 之前读），比窗口模型旧的丢掉，比它新的先重置再建；console 的泵与回复读 Hub 当前的代数。
- 拆除路径看过：检查器只加了一个 `detach()` 的激活订阅（属于窗口实体）、`on_window_should_close` 的闭包（只拿 `Inbox`）、覆盖层里的 `Rc` 状态，日志文件句柄在进程级 `OnceLock` 里；没有找到能活过 GPUI 应用的东西，没有猜着改。
- REPL 输入框没有 IME、选区、粘贴（不碰剪贴板）；输入框只处理按键事件，能输入 ASCII 与键盘直接产生的字符。
- 计算样式没有「来源规则」「继承」之分，也不能过滤；盒模型的 margin/padding 的百分比与 `auto` 不解析（Shell 没有包含块宽度），显示原文。
- 树里的文本节点只显示去空白后的前 60 个字符；`<template>`、Shadow DOM、`<iframe>` 的内容不展开。
- 拾取层盖在页面之上，P32 的输入桥合并后若也在页面窗口上接鼠标，需要在拾取时让位。
- 窗口标题不随页面导航更新；检查器窗口没有记住位置和大小。
- 体积预算（≤ 300 KB）没有测，请中央量；新增的是 `inspector/` 的几个文件与内核的两个小函数，无新依赖。
- 越界的改动：内核（`src/embed.rs`、`src/lib.rs` 导出 `inspect_value`；`src/uncaught.rs`、`src/process.rs`、`src/hooks.rs` 的拒绝警告），各在自己的提交里；`msg/` 只在末尾追加；`renderer_guest.rs` 只加了一个分支。
