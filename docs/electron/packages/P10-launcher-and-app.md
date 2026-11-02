# P10 启动器与 `app` 生命周期

**依赖**：P02（骨架与消息契约）、P03（`Runtime::set_process_info`、退出/未捕获异常钩子）  **规模**：M  **修改**：`crates/verve-electron/src/launcher.rs`（新）、`src/api/app.rs`（新）、`src/api/command_line.rs`（新）、`src/single_instance.rs`（新）、`src/main_guest.rs`；`electron` bin 的参数解析

## 目标

`electron.exe` 的命令行和 Electron 39 一样：开发时 `electron .`、`electron <dir>`、`electron <file.js>`，
打包后直接双击 `MyApp.exe` 从 `resources/app.asar` 起；`ELECTRON_RUN_AS_NODE=1` 时当 Node 用。
主进程里的 `require('electron').app` 完整可用：`ready`/`whenReady`、路径、名字版本、单实例锁、
退出流程的事件顺序与 Electron 完全一致；`process.*` 上 Electron 特有的字段齐全。

## 背景（先读）

- `src/cli.rs`：`main_with(args, setup)`、`parse`、`script`、`event_loop`、`alive` —— `verve run` 怎么决定
  何时退出；`ELECTRON_RUN_AS_NODE` 直接复用它。
- `src/process.rs`：`install`（渲染器默认值：`type: 'renderer'`、`title: 'obsidian'`，需要被覆盖）、
  `Setup`、`become_process`（`verve run` 把 `process` 变成真实进程：argv/env/exit）、`exit` → `finish` →
  `std::process::exit`、`uncaught`（无监听者时打印并以 1 退出）、`unhandled_rejection`、
  常量 `ELECTRON_VERSION = "39.2.0"`、`CHROME_VERSION`、`NODE_VERSION`。
- `src/isolate.rs`：主进程跑在 `Isolate<MainGuest>` 里（P02）。
- P02 的 `msg.rs`：`AppEvent`、`FromShell`、`ToShell::Quit`。
- P11：打包形态的 `app.asar` 由 P11 的 overlay 提供；本包只负责**找到**应用目录/归档并把路径交给它。

## 接口契约

### 1. 命令行（`launcher.rs`）

```rust
pub enum Launch {
    /// 开发形态：electron <path> [args…]，path 是目录（读 package.json）或 .js/.cjs/.mjs 文件
    App { app_path: PathBuf, entry: PathBuf, args: Vec<String>, default_app: true },
    /// 打包形态：exe 旁边有 resources/app.asar 或 resources/app/
    Packaged { app_path: PathBuf /* …/resources/app.asar */, entry: PathBuf, args: Vec<String> },
    /// electron <url | file.html>：默认应用开一个窗口加载它
    OpenUrl { url: String },
    /// 没有参数且不是打包形态：默认应用欢迎页
    DefaultApp,
    /// ELECTRON_RUN_AS_NODE=1：交给 verve::cli::main_with，权限 -A（Node 语义，没有权限模型）
    RunAsNode { args: Vec<OsString> },
    Version, Abi, Help,
}
pub fn parse(args: Vec<OsString>, exe: &Path, env: &Env) -> Result<Launch, String>;
```

规则（以 Electron 的 `default_app/main.ts` 为准）：

| 输入 | 结果 |
|---|---|
| `ELECTRON_RUN_AS_NODE` 非空且不为 `0` | `RunAsNode`，其余参数原样交给 Verve CLI（前面补 `-A`） |
| exe 所在目录有 `resources/app.asar`（macOS：`../Resources/app.asar`），否则 `resources/app/` | `Packaged`；**忽略**命令行上的路径参数，全部算 `args` |
| `-v` / `--version` | 打印 `v39.2.0`，退出 0 |
| `-a` / `--abi` | 打印 `136`（与 `process.versions.modules` 一致），退出 0 |
| `-h` / `--help` | 打印用法，退出 0 |
| `-i` / `--interactive` | 打印 "REPL is not supported by Verve"，退出 1 |
| `-r <module>` / `--require <module>` | 主进程入口执行前先 `require` 它（可多次） |
| 第一个非开关参数是目录 | 读 `<dir>/package.json`：`main` 字段（缺省 `index.js`），按 Node 规则解析（无扩展名补 `.js`、目录取 `index.js`）；没有 package.json 时报错 `Unable to find Electron app at <dir>` 退出 1 |
| 是 `.js`/`.cjs`/`.mjs` 文件 | 直接作入口，`app_path` = 文件所在目录（与 Electron 一致：`app.getAppPath()` 是该目录） |
| 是 `http(s):`/`file:` URL 或 `.html` 文件 | `OpenUrl` |
| 无参数 | `DefaultApp` |
| Chromium/Node 开关：`--inspect[=…]`、`--inspect-brk`、`--remote-debugging-port=…`、`--enable-logging`、`--no-sandbox`、`--disable-gpu`、`--disable-gpu-sandbox`、`--js-flags=…`、`--trace-warnings`、`--lang=…`、`--force-device-scale-factor=…`、`--high-dpi-support=…`、`--user-data-dir=…`、`--proxy-server=…` 等任何 `--x[=y]` | **不报错**，保留在 `process.argv` 里，并记入 `app.commandLine`（`hasSwitch`/`getSwitchValue` 能读到）。其中真正生效的：`--user-data-dir`（改 `userData`）、`--lang`（`app.getLocale`）、`--force-device-scale-factor`（交给 Shell）、`--proxy-server`（交给 P16 的 session） |
| `--` | 之后的参数一律原样进 `process.argv`，不再当开关解析 |

`process.argv`：开发形态 `[execPath, app_path_as_given, ...args]`；打包形态 `[execPath, ...args]`。
`process.execPath` = `std::env::current_exe()`；`process.argv0` = 启动时的 argv[0]。

### 2. 打包形态的查找顺序

1. `<exe_dir>/resources/app.asar`（macOS：`<Contents>/Resources/app.asar`）
2. `<exe_dir>/resources/app/`（目录）
3. 都没有 → 当开发形态处理命令行

`process.resourcesPath` = `<exe_dir>/resources`（macOS：`Contents/Resources`），两种形态都设（开发形态指向
Verve 发行目录自己的 `resources`，P40 保证它存在，里面可以没有 app）。

### 3. 主进程的 `process` 字段（通过 P03 的 `Runtime::set_process_info`）

| 字段 | 值 |
|---|---|
| `process.type` | `'browser'` |
| `process.versions.electron` / `.chrome` / `.node` / `.v8` | 沿用 `process.rs` 常量 |
| `process.resourcesPath` | 见上 |
| `process.defaultApp` | 开发形态 `true`；打包形态**不存在**（`undefined`），与 Electron 一致 |
| `process.mas` / `process.windowsStore` | `false` / Windows 上从包路径判断，先恒为 `false` |
| `process.noAsar` | 可写，默认 `false`（P11 读它） |
| `process.noDeprecation` / `throwDeprecation` / `traceDeprecation` / `traceProcessWarnings` | 可写 |
| `process.title` | 应用名（productName 或 name），不再是 `'obsidian'` |
| `process.getCreationTime()` | 启动时刻的 ms 时间戳 |
| `process.getCPUUsage()` | `{ percentCPUUsage, cumulativeCPUUsage, idleWakeupsPerSecond: 0 }` |
| `process.getHeapStatistics()` | 对象，字段为 KB：`totalHeapSize` `totalHeapSizeExecutable` `totalPhysicalSize` `totalAvailableSize` `usedHeapSize` `heapSizeLimit` `mallocedMemory` `peakMallocedMemory` `doesZapGarbage` |
| `process.getProcessMemoryInfo()` | Promise → `{ residentSet, private, shared }`（KB） |
| `process.getSystemMemoryInfo()` | `{ total, free }`（KB；Windows 加 `swapTotal`/`swapFree`） |
| `process.getSystemVersion()` | 如 `'10.0.19044'` |
| `process.getIOCounters()` | Windows/Linux，各字段数字 |
| `process.crash()` | 结束主进程 isolate（`render-process-gone` 不适用）——直接 `std::process::abort` |
| `process.hang()` | 主进程线程死循环（测试用），可实现为 `loop {}`，被 `InterruptHandle` 停 |
| `process.setFdLimit(n)` | no-op |
| `process.takeHeapSnapshot(path)` | 返回 `false` |
| 事件 `'loaded'` | 主进程入口加载前发出 |
| `process.exit(code)` | **等同 `app.exit(code)`**（见退出流程），不能直接 `std::process::exit` 跳过窗口关闭——需要 P03 提供可替换的退出钩子 |

未捕获异常（主进程）：有 `uncaughtException` 监听者 → 交给它；**没有** → 打印到 stderr，并（有 GUI 时）
`dialog.showErrorBox('A JavaScript error occurred in the main process', stack)`，**进程不退出**（Electron 行为，
与 Node 相反）。`unhandledRejection` 无监听者 → 仅打印警告，不退出。需要 P03 让 `process::uncaught` 的
"无监听者"分支可由嵌入者接管。

### 4. `app` 模块（`api/app.rs`，EventEmitter 实例，单例）

**事件**（参数与 Electron 一致）：

| 事件 | 何时 | 可阻止 |
|---|---|---|
| `will-finish-launching` | 入口执行完后、`ready` 前（同一轮里） | |
| `ready` `(event, launchInfo)` | Shell 起来以后：需要 P02 追加 `AppEvent::Ready`（Shell 初始化完成时发）；无头模式入口执行完后的下一轮 | |
| `window-all-closed` | 最后一个窗口 `closed` 之后，**且不是 `app.quit()` 发起的关闭**。无监听者 → 自动 `app.quit()` | |
| `before-quit` `(event)` | `app.quit()` 开始时 | ✓ |
| `will-quit` `(event)` | 所有窗口都已关闭、即将退出 | ✓ |
| `quit` `(event, exitCode)` | 退出前最后一个事件 | |
| `browser-window-created` `(event, window)` | 每个 `new BrowserWindow`（含 `window.open` 创建的） | |
| `web-contents-created` `(event, webContents)` | 每个 webContents 创建 | |
| `browser-window-focus` / `browser-window-blur` `(event, window)` | 窗口焦点变化 | |
| `activate` `(event, hasVisibleWindows)` | `AppEvent::Activate`（macOS） | |
| `did-become-active` / `did-resign-active` | 应用前后台（macOS；其他平台可不发） | |
| `open-file` `(event, path)` / `open-url` `(event, url)` | `AppEvent::OpenFile/OpenUrl`（macOS） | ✓ |
| `second-instance` `(event, argv, workingDirectory, additionalData)` | 单实例：第二个实例启动 | |
| `render-process-gone` `(event, webContents, details)` | 渲染器 isolate 崩溃/被杀（P13 发出后转发） | |
| `child-process-gone` `(event, details)` | P17 的 `utilityProcess` 异常退出 | |
| `session-created` `(session)` | P16 | |
| `accessibility-support-changed`、`login`、`certificate-error`、`select-client-certificate`、`gpu-info-update`、`gpu-process-crashed` | 对象上可以监听，v1 不发出（`certificate-error` 由 P16 视情况接入） | |

**方法**：

| 方法 | 说明 |
|---|---|
| `quit()` | 见"退出流程" |
| `exit(exitCode = 0)` | **不发任何事件**（无 `before-quit`/`will-quit`/`quit`/`close`），直接销毁所有窗口，以 exitCode 结束进程 |
| `relaunch({ args, execPath })` | 记下，`exit`/`quit` 后用 `std::process::Command` 以相同（或给定）参数重新启动自己 |
| `isReady()` / `whenReady()` | `whenReady` 返回 Promise，ready 后 resolve；ready 之后调用立即 resolve |
| `focus({ steal })` / `hide()` / `show()` / `isHidden()` | focus 发 `ToShell` 让最近窗口获焦；hide/show 仅 macOS，其他平台 no-op |
| `getAppPath()` | 开发形态：应用目录；打包：`…/resources/app.asar`（或 `app` 目录） |
| `getPath(name)` / `setPath(name, path)` | 见下表；未知名字抛 `Error: Failed to get '<name>' path`；`setPath` 在 ready 前调用对 `userData` 等生效，路径不存在时创建 |
| `getVersion()` | package.json `version`；默认应用时为 Electron 版本 |
| `getName()` / `setName(name)` / 属性 `name` | package.json `productName` → `name` → `'Electron'` |
| `getLocale()` | `--lang` > 系统 UI 语言，Chromium 形式如 `'zh-CN'`、`'en-US'` |
| `getSystemLocale()` / `getPreferredSystemLanguages()` / `getLocaleCountryCode()` | 系统区域；列表；两字母国家码 |
| `addRecentDocument(path)` / `clearRecentDocuments()` | Windows/macOS 可 no-op（记录在状态里） |
| `setAsDefaultProtocolClient(protocol, path?, args?)` / `removeAsDefaultProtocolClient` / `isDefaultProtocolClient` / `getApplicationNameForProtocol` / `getApplicationInfoForProtocol` | Windows：写/读 `HKCU\Software\Classes\<protocol>`（`windows-sys` 注册表 API）；其他平台先返回 `false` |
| `setUserTasks(tasks)` / `getJumpListSettings()` / `setJumpList(categories)` | 返回 `true` / `{ minItems: 10, removedItems: [] }` / `'ok'`，不真正写跳转列表 |
| `requestSingleInstanceLock(additionalData?)` / `hasSingleInstanceLock()` / `releaseSingleInstanceLock()` | 见"单实例" |
| `setAppUserModelId(id)` | Windows：`SetCurrentProcessExplicitAppUserModelID` |
| `setActivationPolicy`、`dock`（macOS 专属；其他平台 `app.dock` 为 `undefined`） | |
| `getAppMetrics()` | `[{ pid, type: 'Browser', creationTime, cpu: {...}, memory: {...} }]`，每个渲染器再加一项 `type: 'Tab'`（pid 同进程） |
| `getGPUFeatureStatus()` | 各字段 `'disabled_software'` 的对象 |
| `getGPUInfo(type)` | Promise → 最小对象 `{ auxAttributes: {}, gpuDevice: [] }` |
| `setBadgeCount(n)` / `getBadgeCount()` / 属性 `badgeCount` | 记录；Windows 返回 `false` |
| `isUnityRunning()` | `false` |
| `getLoginItemSettings(opts)` / `setLoginItemSettings(settings)` | Windows：`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`；其他平台返回 `{ openAtLogin: false, … }` |
| `isAccessibilitySupportEnabled()` / `setAccessibilitySupportEnabled` | `false` / no-op |
| `setAboutPanelOptions` / `showAboutPanel` | 记录 / 用 P14 的 `dialog.showMessageBox` 展示 |
| `isEmojiPanelSupported()` / `showEmojiPanel()` | Windows `true`，发 Win+. ；其他 `false` |
| `disableHardwareAcceleration()` / `disableDomainBlockingFor3DAPIs()` / `isHardwareAccelerationEnabled()` | ready 前调用有效（ready 后抛错与 Electron 同）；记录；`isHardwareAccelerationEnabled` 返回记录值 |
| `enableSandbox()` | ready 前调用：之后所有 webContents 默认 `sandbox: true` |
| `isInApplicationsFolder()` / `moveToApplicationsFolder()` | macOS；其他平台 `false` |
| `getFileIcon(path, { size })` | Promise<NativeImage>，P14 提供 nativeImage 后实现，此前 reject `ERR_NOT_SUPPORTED` |
| `setProxy(config)` / `resolveProxy(url)` | 转给 `session.defaultSession`（P16） |
| `setClientCertRequestPasswordHandler`、`importCertificate` | 抛 not supported |
| 属性 `userAgentFallback` | 默认 `navigator.userAgent` 的值（`src/navigator.rs`），可写，新 webContents 用它 |
| 属性 `isPackaged` | 打包形态 `true` |
| 属性 `commandLine` | 见下 |
| 属性 `applicationMenu` | P15 |
| 属性 `runningUnderARM64Translation` | `false` |

**`getPath` 名字与位置**（`<name>` = `app.getName()`）：

| 名字 | Windows | macOS | Linux |
|---|---|---|---|
| `home` | `%USERPROFILE%` | `~` | `~` |
| `appData` | `%APPDATA%`（Roaming） | `~/Library/Application Support` | `$XDG_CONFIG_HOME` 或 `~/.config` |
| `userData` | `appData/<name>`（`--user-data-dir` 优先） | 同 | 同 |
| `sessionData` | 同 `userData` | 同 | 同 |
| `temp` | `%TEMP%` | `$TMPDIR` | `/tmp` 或 `$TMPDIR` |
| `exe` | 当前 exe | `Contents/MacOS/<exe>` | 当前 exe |
| `module` | 同 `exe` | | |
| `desktop` `documents` `downloads` `music` `pictures` `videos` | Known Folder（`SHGetKnownFolderPath`） | `~/Desktop` 等 | `xdg-user-dirs`（`~/.config/user-dirs.dirs`），缺省 `~/Desktop` 等 |
| `recent` | `%APPDATA%\Microsoft\Windows\Recent` | 抛错 | 抛错 |
| `logs` | `userData/logs` | `~/Library/Logs/<name>` | `userData/logs` |
| `crashDumps` | `userData/Crashpad` | 同 | 同 |

`userData` 在 `ready` 前可用 `setPath` 改；`ready` 时若目录不存在则创建（Electron 的 Chromium 在启动时建立 profile 目录）。
精确时机以真实 Electron 观测为准，冻结在测试里。

**`app.commandLine`**：`appendSwitch(switch, value?)`、`appendArgument(value)`、`hasSwitch(switch)`、
`getSwitchValue(switch)`（无值返回 `''`）、`removeSwitch(switch)`。内容来自启动参数 + 运行时追加；
`appendSwitch` 对 Chromium 专属开关只记录。`lang`、`proxy-server`、`force-device-scale-factor`、
`remote-debugging-port`（P43）在 ready 前追加时生效。

### 5. 退出流程（必须与 Electron 完全一致）

**A. 用户关掉最后一个窗口**：窗口 `close` → `closed`（P13）→ 若无窗口剩下：
`app` 发 `window-all-closed`；**无监听者**则执行 B（`app.quit()`）；有监听者则什么也不做（应用自己决定，macOS 惯例不退出）。

**B. `app.quit()`**：
1. 发 `before-quit`；被 `preventDefault()` → 结束，什么也不做。
2. 依次对每个窗口执行"可阻止的关闭"（P13：发 `close`、页面 `beforeunload`）。任何一个被阻止 → 退出取消，
   已关闭的窗口不恢复；**不**发 `window-all-closed`。
3. 全部关闭后发 `will-quit`；被阻止 → 结束（进程继续运行，没有窗口）。
4. 发 `quit` `(event, exitCode)`，然后 `ToShell::Quit { exit_code }`，Shell 结束 GPUI，进程以该码退出。
5. `before-quit` 在一次退出尝试中只发一次；再次调用 `quit()` 重新从 1 开始。

**C. `app.exit(code)` / 主进程 `process.exit(code)`**：不发任何事件，销毁所有窗口和渲染器 isolate，
刷新 stdout/stderr（`process.rs` 的 `write_fd` 已经是无缓冲写），以 `code` 退出。

**D. 系统关机 / 会话结束（Windows `WM_QUERYENDSESSION`）**：窗口发 `query-session-end`/`session-end`（P13/P34），
然后按 B 处理但不可阻止。

**E. 主进程没有窗口、没有 ref 的定时器/套接字/子进程、入口已结束**：Electron **不会**自动退出（`app` 一直在
等事件），必须显式 `app.quit()`。注意这与 `verve run` 的 `alive` 判断相反——`MainGuest` 不能沿用 cli 的退出条件。
唯一例外：`ready` 之前入口就抛出未捕获异常且无监听者 → 打印后退出 1（Electron 会弹框后保持；先按 Electron 冻结观测）。

### 6. 单实例锁（`single_instance.rs`）

- 锁的范围：同一个 `userData` 目录（Electron 语义）。键 = `sha256(userData 的规范路径)` 的前 16 个十六进制字符。
- Windows：`CreateMutexW("Local\\verve-electron-<key>")`，`GetLastError() == ERROR_ALREADY_EXISTS` → 已有实例。
  主实例在独立线程上开命名管道服务 `\\.\pipe\verve-electron-<key>`；第二实例连上去写一条 JSON
  `{ "argv": [...], "cwd": "...", "additionalData": <Cloned 的 JSON 形态> }`，等主实例回一个字节 `1`（确认收到）后返回 `false`。
- macOS/Linux：在 `userData` 里 `SingletonLock` 文件上 `flock(LOCK_EX|LOCK_NB)`，`SingletonSocket` Unix 域套接字收发同样的 JSON。
- 主实例收到后通过 Hub 投递 `AppEvent::SecondInstance`（P02 已有）→ 主进程线程发 `second-instance`。
  `additionalData` 用结构化克隆传（P03 的 `Cloned`；跨进程走 JSON，仅支持 JSON 可表示的值，与 Electron 的限制一致）。
- `requestSingleInstanceLock()` 是同步的；第二实例里返回 `false` 后应用通常调用 `app.quit()`。
- `releaseSingleInstanceLock()` 关闭管道/套接字并释放锁；进程退出时自动释放。

### 7. 默认应用

`DefaultApp`：开一个 800×600 窗口，加载内嵌的一页 HTML（Rust 字符串常量；它是**页面内容**，不是 JS 垫片，
允许），显示 Verve/Electron/Node 版本和用法。`OpenUrl`：开窗口 `loadURL(url)`。两者 `process.defaultApp = true`，
窗口全关后退出。

### 其他包登记的启动器需求（总控注）

- **Electron Fuses**（P40）：二进制里必须带官方 fuse 哨兵串与线格式（`dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX` + 版本 + 各 fuse 字节），
  否则 electron-builder/forge 配了 `electronFuses` 会打包失败。`main` 一开始读取 fuse 字节并遵守：`RunAsNode`
  （关则忽略 `ELECTRON_RUN_AS_NODE`）、`EnableNodeCliInspectArguments`、`OnlyLoadAppFromAsar`、`EnableEmbeddedAsarIntegrityValidation`
  （P11 实现校验）、`EnableNodeOptionsEnvironmentVariable`、`GrantFileProtocolExtraPrivileges`。哨兵串与字节布局以 `@electron/fuses` 源码为准。
- **保留的隐藏开关**：`--verve-dump-api`（P41：打印所有已注册的 Electron API 名后退出）、`--verve-skip-asar-integrity`（P40）、
  `--headless`（P00）；`--inspect`、`--inspect-brk`、`--remote-debugging-port` 等接受并打印一次警告（P43）。
- **ABI 号**：`process.versions.modules` / `--abi` 现为 `136`，须对照 Electron 39 的 `NODE_MODULE_VERSION` 核实后修正（P50 依赖它）。
- **`ELECTRON_RUN_AS_NODE`**：P17 在 `src/cli.rs` 新增 `main_node`，启动器检测到该变量（且 fuse 允许）时直接转入 Node 模式。

## 步骤

1. `launcher.rs`：参数解析与打包查找，表格中每一行一个单元测试。
2. `ELECTRON_RUN_AS_NODE`：bin 里在创建 GPUI 之前分流到 `verve::cli::main_with`。
3. `MainGuest` 用 P03 的 `set_process_info` 设好 `process.*`；接 P03 的退出钩子和未捕获异常钩子。
4. `api/app.rs`：事件、方法、路径（Windows Known Folder 用 `windows-sys` 的 `Win32_UI_Shell`；不引入 `dirs` 以外的新库，
   `dirs` 也尽量不用——几行代码能写完）。
5. 退出状态机：`QuitState { Running, BeforeQuit, ClosingWindows { remaining }, WillQuit, Exiting(code) }`，放在 `MainGuest`，
   由 P13 的窗口关闭结果驱动。P13 未合并前用 P02 的最小 BrowserWindow 测。
6. 单实例：Windows 先做，Unix 跟上（CI 在 Linux 跑 Unix 部分）。
7. `relaunch`：退出路径的最后一步 spawn。

## 验收

`cargo test -p verve-electron --test app`（无头，使用 P00 夹具；P00 未就绪时直接用 P02 的 `HeadlessShell`）：

- 参数表格全部用例；`electron --version` 输出 `v39.2.0`。
- `tests/electron/apps/lifecycle/`：主进程记录事件到 stdout，断言顺序：
  - 关最后一个窗口、有 `window-all-closed` 监听者 → 只有 `window-all-closed`，进程不退出；随后 `app.quit()` →
    `before-quit, will-quit, quit`，退出码 0。
  - 无监听者 → `window-all-closed, before-quit, will-quit, quit`。
  - `before-quit` 里 `preventDefault` → 无后续事件、窗口仍在。
  - 某窗口 `close` 被阻止 → 无 `will-quit`。
  - `app.exit(3)` → 无事件，退出码 3；主进程 `process.exit(4)` → 同，退出码 4。
- `app.getPath` 每个名字在 Windows 上返回存在的绝对路径；`--user-data-dir=X` 后 `userData === X`。
- 单实例：起两个 `electron.exe`（同 userData），第二个 `requestSingleInstanceLock()` 返回 `false` 并退出；
  第一个收到 `second-instance`，argv 与 cwd 正确，`additionalData` 往返相等。
- `ELECTRON_RUN_AS_NODE=1 electron -e "console.log(process.versions.node)"` 输出 `22.20.0`。
- 主进程未捕获异常且无监听者：stderr 有堆栈，进程仍在运行（无头模式下记录一次"错误框"调用）。

## 不做

菜单（P15）、BrowserWindow 本身（P13）、asar 读取（P11）、真实的 `autoUpdater`、`crashReporter`（P02 留空对象，
`crashReporter.start` no-op，`getLastCrashReport` 返回 `null`）、macOS Dock/About 面板的原生实现。

## 状态

（实现者完成后填写）
