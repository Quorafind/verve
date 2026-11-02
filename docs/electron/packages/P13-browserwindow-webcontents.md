# P13 `BrowserWindow` / `webContents`

**依赖**：P02（消息与 Hub）、P12（IPC：`webContents.send`/`ipc`）；与 P10（退出流程）、P20（页面加载）、P34（Shell 窗口）对接  **规模**：L  **修改**：`crates/verve-electron/src/api/browser_window.rs`、`api/web_contents.rs`、`api/web_frame_main.rs`、`api/web_preferences.rs`（新）、`src/window_state.rs`（新）、`hub.rs`（注册表）；向 `msg.rs` **追加**本文列出的变体

## 目标

主进程中的 `BrowserWindow`、`webContents`、`webFrameMain`（最小）与 Electron 39 一致：构造参数及默认值、方法、属性、
事件及其顺序、静态方法、`close` 可阻止的往返、`ready-to-show`、`window.open` 与 `setWindowOpenHandler`、`will-navigate`。
窗口真正的绘制在 P34/P30；本包负责**主进程侧对象、状态缓存与消息往返**，在无头模式下全部可测。

## 背景（先读）

- P02：`ToShell::{CreateWindow, Window}`、`WindowOptions`、`WindowOp`、`FromShell::Window`、`WindowEvent`、
  `ToRenderer`、`FromRenderer`、`PageEvent`、`Hub`。
- P12：`ipc-message` 分发、`webContents.ipc`。
- P10：退出状态机（"可阻止的关闭"由本包提供）。
- `crates/verve-dom/src/lib.rs` 的 `Request`：`Navigate { url, replace }`、`Open(String)`、`Title(String)`、`Alert`、`Fullscreen`。
- `src/navigator.rs`：默认 `userAgent`。
- `src/sockets.rs`、`src/events.rs`：原生类继承 `EventEmitter` 的写法（`class X extends BrowserWindow` 必须可行）。

## 对象模型

- 主进程持有注册表 `windows: BTreeMap<WindowId, WindowRecord>`、`contents: BTreeMap<ContentsId, ContentsRecord>`。
  记录里存 JS 对象的**强引用**：窗口未销毁前，即使脚本丢掉引用也不能被回收（Electron 行为）。`closed` 后移除。
- 窗口状态缓存 `WindowState { bounds, content_bounds, normal_bounds, visible, focused, minimized, maximized, fullscreen,
  title, opacity, always_on_top, resizable, … }`：getter 同步读缓存；setter **先乐观更新缓存**再发 `WindowOp`，
  Shell 回来的 `WindowEvent` 再校正（`getBounds()` 在 `setBounds()` 后立即返回新值）。
- 已销毁对象上调用方法 → `TypeError: Object has been destroyed`（Electron 原文）；`isDestroyed()` 可调用。
- `BrowserWindow.id` 与 `webContents.id` 是**两个独立计数器**，都从 1 开始，不复用。

## `new BrowserWindow(options)`

### 窗口参数（默认值以 Electron 39 为准；P02 `WindowOptions` 缺的字段在本包追加）

| 参数 | 默认 | 说明 |
|---|---|---|
| `width` / `height` | `800` / `600` | |
| `x` / `y` | 居中 | 只给一个时忽略 |
| `useContentSize` | `false` | `true` 时 width/height 指内容区 |
| `center` | `false` | |
| `minWidth` / `minHeight` | `0` | |
| `maxWidth` / `maxHeight` | 无限 | |
| `resizable` `movable` `minimizable` `maximizable` `closable` `focusable` | `true` | |
| `alwaysOnTop` | `false` | |
| `fullscreen` | `false` | |
| `fullscreenable` | `true` | |
| `simpleFullscreen` | `false` | macOS |
| `skipTaskbar` | `false` | |
| `kiosk` | `false` | |
| `title` | `'Electron'` | 页面有 `<title>` 时窗口标题跟随页面（除非 `page-title-updated` 被阻止） |
| `icon` | 无 | 路径或 `NativeImage`（P14） |
| `show` | `true` | |
| `paintWhenInitiallyHidden` | `true` | 为 `false` 且 `show: false` 时不发 `ready-to-show` |
| `frame` | `true` | |
| `parent` | `null` | `BrowserWindow` |
| `modal` | `false` | 需要 `parent` |
| `acceptFirstMouse` `disableAutoHideCursor` `enableLargerThanScreen` | `false` | |
| `autoHideMenuBar` | `false` | P15 |
| `backgroundColor` | `'#FFF'` | 支持 `#RGB` `#ARGB` `#RRGGBB` `#AARRGGBB` `rgb()` `rgba()` `hsl()` `hsla()` 与 CSS 颜色名 |
| `hasShadow` | `true` | |
| `opacity` | `1.0` | |
| `darkTheme` | `false` | |
| `transparent` | `false` | |
| `type` | 无 | 平台窗口类型字符串，记录 |
| `titleBarStyle` | `'default'` | `'hidden'` `'hiddenInset'` `'customButtonsOnHover'` |
| `titleBarOverlay` | `false` | `true` 或 `{ color, symbolColor, height }`（P34） |
| `trafficLightPosition` | 无 | macOS |
| `roundedCorners` | `true` | |
| `thickFrame` | `true` | Windows |
| `vibrancy` / `visualEffectState` | 无 | macOS，记录 |
| `backgroundMaterial` | 无 | Windows 11：`'auto' 'none' 'mica' 'acrylic' 'tabbed'`（P34 尽力） |
| `zoomToPageWidth` `tabbingIdentifier` `hiddenInMissionControl` | | macOS，记录 |
| `webPreferences` | 见下 | |

参数类型错误 → `TypeError`（与 Electron 一致，例如 `width` 为字符串时 Electron 静默转换？以观测冻结）。

### `webPreferences`（`web_preferences.rs`；传给 P20/P21 的 `WebPreferences` 结构）

| 字段 | 默认 |
|---|---|
| `devTools` | `true` |
| `nodeIntegration` | `false` |
| `nodeIntegrationInWorker` | `false` |
| `nodeIntegrationInSubFrames` | `false` |
| `preload` | 无（必须是绝对路径，否则 `preload-error`） |
| `sandbox` | **`true`**，除非 `nodeIntegration: true`（此时 `false`）；`app.enableSandbox()` 后强制 `true` |
| `session` / `partition` | `session.defaultSession`；`partition` 以 `persist:` 开头为持久化（P16） |
| `zoomFactor` | `1.0` |
| `javascript` | `true` |
| `webSecurity` | `true`（`false` 时同时 `allowRunningInsecureContent: true`） |
| `allowRunningInsecureContent` | `false` |
| `images` | `true` |
| `imageAnimationPolicy` | `'animate'` |
| `textAreasAreResizable` | `true` |
| `webgl` | `true`（v1 无 WebGL，仅记录） |
| `plugins` `experimentalFeatures` `scrollBounce` `offscreen` `webviewTag` `safeDialogs` `disableDialogs` `navigateOnDragDrop` `disableHtmlFullscreenWindowResize` `enablePreferredSizeMode` | `false` |
| `enableBlinkFeatures` / `disableBlinkFeatures` | `''`，记录 |
| `defaultFontFamily` | `{ standard: 'Times New Roman', serif: 'Times New Roman', sansSerif: 'Arial', monospace: 'Courier New', cursive: 'Script', fantasy: 'Impact', math: 'Latin Modern Math' }`（传给 P31 字体回退） |
| `defaultFontSize` / `defaultMonospaceFontSize` / `minimumFontSize` | `16` / `13` / `0` |
| `defaultEncoding` | `'ISO-8859-1'` |
| `backgroundThrottling` | `true`（隐藏/最小化时定时器节流到 1 Hz，rAF 停止——P20/P33） |
| `contextIsolation` | `true` |
| `additionalArguments` | `[]`（追加到渲染器 `process.argv`） |
| `safeDialogsMessage` | 无 |
| `autoplayPolicy` | `'no-user-gesture-required'` |
| `accessibleTitle` | 无 |
| `spellcheck` | `true`（v1 无拼写检查，仅记录） |
| `enableWebSQL` | `true`（v1 无 WebSQL） |
| `v8CacheOptions` | `'code'`（映射到 Verve 代码缓存开关） |
| `transparent` | `true` |

构造时顺序：分配 `WindowId`、`ContentsId` → 创建 `WebContents` 对象 → `app` 发 `web-contents-created` →
`Hub::create_renderer` 起渲染器 isolate → `ToShell::CreateWindow` → `app` 发 `browser-window-created` → 返回。
（Electron 中 `browser-window-created` 在构造函数内同步发出，监听者里 `window.webContents` 已可用。）

## `BrowserWindow` 实例方法

全部要实现（"记录"= 存进状态并让 getter 返回，平台无对应能力时不做别的）：

- 生命周期：`destroy()`、`close()`、`isDestroyed()`
- 焦点/可见：`focus()`、`blur()`、`isFocused()`、`show()`、`showInactive()`、`hide()`、`isVisible()`、`isModal()`、
  `setEnabled(bool)`/`isEnabled()`、`setFocusable(bool)`/`isFocusable()`、`focusOnWebView()`、`blurWebView()`
- 状态：`maximize()`、`unmaximize()`、`isMaximized()`、`minimize()`、`restore()`、`isMinimized()`、`isNormal()`、
  `setFullScreen(bool)`、`isFullScreen()`、`setSimpleFullScreen`/`isSimpleFullScreen`、`setKiosk`/`isKiosk`、`isTabletMode()`（`false`）、
  `isSnapped()`（Windows，`false` 起步）
- 几何：`setBounds(rect, animate?)`、`getBounds()`、`setContentBounds`/`getContentBounds`、`getNormalBounds()`、
  `setSize(w,h,animate?)`/`getSize()`、`setContentSize`/`getContentSize`、`setMinimumSize`/`getMinimumSize`、
  `setMaximumSize`/`getMaximumSize`、`setPosition(x,y,animate?)`/`getPosition()`、`center()`、`setAspectRatio(ratio, extraSize?)`、
  `moveAbove(mediaSourceId)`、`moveTop()`
- 能力开关：`setResizable/isResizable`、`setMovable/isMovable`、`setMinimizable/isMinimizable`、`setMaximizable/isMaximizable`、
  `setFullScreenable/isFullScreenable`、`setClosable/isClosable`、`setAlwaysOnTop(flag, level?, relativeLevel?)`/`isAlwaysOnTop()`、
  `setSkipTaskbar`、`setIgnoreMouseEvents(ignore, { forward })`、`setContentProtection`/`isContentProtected`、
  `setVisibleOnAllWorkspaces`/`isVisibleOnAllWorkspaces`、`setHiddenInMissionControl`
- 外观：`setTitle`/`getTitle`、`setBackgroundColor`/`getBackgroundColor`（返回 `#RRGGBB` 或 `#AARRGGBB` 大写？以观测冻结）、
  `setOpacity`/`getOpacity`、`setHasShadow`/`hasShadow`、`invalidateShadow`、`setIcon`、`setProgressBar(progress, { mode })`、
  `setOverlayIcon`（Windows）、`flashFrame(bool)`、`setShape`、`setVibrancy`、`setBackgroundMaterial`、`setTitleBarOverlay`、
  `setWindowButtonVisibility`/`setWindowButtonPosition`/`getWindowButtonPosition`（macOS）、`setAutoHideCursor`
- 菜单：`setMenu(menu|null)`、`removeMenu()`、`setAutoHideMenuBar`/`isMenuBarAutoHide`、`setMenuBarVisibility`/`isMenuBarVisible`（P15 实现菜单本体）
- 父子：`setParentWindow(win|null)`、`getParentWindow()`、`getChildWindows()`
- 内容：`loadURL(url, options?)`、`loadFile(path, { query, search, hash }?)`、`reload()`、`capturePage(rect?)`（P30/P35 提供前 reject `ERR_NOT_SUPPORTED`）
- 平台句柄：`getNativeWindowHandle()`（`Buffer`，Windows 为 HWND 的 8 字节，Shell 创建窗口后经 `WindowEvent` 追加 `NativeHandle(u64)` 回报）、
  `getMediaSourceId()`（`'window:<handle>:0'`）、`hookWindowMessage`/`isWindowMessageHooked`/`unhookWindowMessage`/`unhookAllWindowMessages`（Windows，v1 记录不触发）
- macOS 杂项：`setRepresentedFilename`/`getRepresentedFilename`、`setDocumentEdited`/`isDocumentEdited`、`setSheetOffset`、`previewFile`/`closeFilePreview`、
  `showDefinitionForSelection`、`selectPreviousTab` 等 tab 方法、`setTouchBar`：记录或 no-op
- Windows 杂项：`setThumbarButtons`（返回 `true`）、`setThumbnailClip`、`setThumbnailToolTip`、`setAppDetails`：记录

**属性**：`id`（只读）、`webContents`（只读）、`autoHideMenuBar`、`simpleFullScreen`、`fullScreen`、`focusable`、`visibleOnAllWorkspaces`、
`shadow`、`menuBarVisible`、`kiosk`、`documentEdited`、`representedFilename`、`title`、`minimizable`、`maximizable`、`fullScreenable`、
`resizable`、`closable`、`movable`、`excludedFromShownWindowsMenu`、`accessibleTitle`、`snapped`、`tabbingIdentifier`——均为对应 getter/setter 的属性形式。

**静态**：`BrowserWindow.getAllWindows()`（按创建顺序）、`getFocusedWindow()`、`fromWebContents(wc)`、`fromId(id)`、`fromBrowserView()`（返回 `null`）。

## `BrowserWindow` 事件

| 事件 | 参数 | 来源 / 时机 | 可阻止 |
|---|---|---|---|
| `page-title-updated` | `(event, title, explicitSet)` | `PageEvent::TitleUpdated`；未阻止则更新窗口标题（`WindowOp::SetTitle`） | ✓ |
| `close` | `(event)` | 见"关闭往返" | ✓ |
| `closed` | | 窗口已销毁；之后立刻从注册表移除 | |
| `query-session-end` / `session-end` | `(event)` | Windows 关机（P34 回报） | `query-session-end` ✓ |
| `unresponsive` / `responsive` | | 渲染器 isolate 心跳：一次 tick 超过 5 s 未返回 → `unresponsive`；恢复 → `responsive` | |
| `blur` / `focus` / `show` / `hide` | | `WindowEvent` | |
| `ready-to-show` | | 见下 | |
| `maximize` / `unmaximize` / `minimize` / `restore` | | `WindowEvent` | |
| `will-resize` | `(event, newBounds, { edge })` | 用户拖边框时（P34 需要在调整前询问——v1 允许只发事件、不可真正阻止，记录为已知差异） | ✓ |
| `resize` / `resized` | | `resized` 只在用户拖动结束时（Windows/macOS） | |
| `will-move` / `move` / `moved` | `(event, newBounds)` | 同上 | `will-move` ✓ |
| `enter-full-screen` / `leave-full-screen` | | | |
| `enter-html-full-screen` / `leave-html-full-screen` | | `Request::Fullscreen` | |
| `always-on-top-changed` | `(event, isAlwaysOnTop)` | | |
| `app-command` | `(event, command)` | Windows 鼠标侧键等（P34，如 `browser-backward`） | |
| `system-context-menu` | `(event, point)` | Windows 标题栏右键（无边框窗口 + 拖动区域） | ✓ |
| `swipe`、`rotate-gesture`、`sheet-begin`、`sheet-end`、`new-window-for-tab` | | macOS，v1 不发 | |

### `ready-to-show` 的精确条件

只发一次。满足以下全部时发出：
1. 渲染器发出 `PageEvent::FirstPaintable`：主文档已过 `DOMContentLoaded`，且此前出现的所有阻塞渲染的样式表
   （`<head>` 中的 `<link rel=stylesheet>`）已加载或失败（P20 定义）；
2. Shell 已用这份（或更新的）快照完成一次布局与绘制——对隐藏窗口也要做（GPUI 窗口以不可见状态创建并布局），完成后回报
   **需要 P02 追加 `WindowEvent::FirstPaint`**；无头模式下 `HeadlessShell` 在收到首个快照后立即回报。
3. 若 `show: false` 且 `paintWhenInitiallyHidden: false` → 不发（Electron 文档明确如此），`document.visibilityState` 为 `'hidden'` 直到 `show()`。

`show: true` 的窗口同样发 `ready-to-show`。

### 关闭往返（可阻止）

`win.close()` 或用户点关闭（`WindowEvent::CloseRequested`）：

```
主进程: emit 'close'(event)
  ├─ 已 preventDefault → WindowOp::CloseAnswer{prevented:true}，结束（退出流程中则告诉 P10"被阻止"）
  └─ 否 → ToRenderer::BeforeUnload{call}                     ← 需要 P02 追加
          渲染器: 派发 beforeunload；处理器调用 preventDefault() 或设置 returnValue 为非空字符串 → 被拦
          FromRenderer::BeforeUnloadReply{call, proceed}     ← 需要 P02 追加
          ├─ proceed=false → webContents emit 'will-prevent-unload'(event)
          │     ├─ 该事件被 preventDefault → 视为 proceed=true（Electron 语义：主进程决定忽略拦截）
          │     └─ 否 → 关闭取消，CloseAnswer{prevented:true}
          └─ proceed=true → 渲染器派发 pagehide/unload（作为 BeforeUnload 应答后的下一步，由 ToRenderer::Unload 触发，← 需要 P02 追加）
                → 结束渲染器 isolate → WindowOp::Destroy → Shell 关窗
                → 窗口 emit 'closed' → webContents emit 'destroyed'
                → 若无窗口剩下：交给 P10（window-all-closed 或继续 quit 流程）
```

`win.destroy()`：不发 `close`、不跑 `beforeunload`/`unload`，直接销毁；仍发 `closed`、`destroyed`。
`webContents.close({ waitForBeforeUnload })`：`waitForBeforeUnload: true` 时走上面的 beforeunload 分支，否则直接销毁。
父窗口关闭 → 子窗口先按同样流程关闭（Electron：子窗口随父窗口关闭）。模态窗口存在时父窗口 `setEnabled(false)`。

## `webContents`

### 方法

- 加载：`loadURL(url, { httpReferrer, userAgent, extraHeaders, postData, baseURLForDataURL })` → Promise：`did-finish-load` 时 resolve；
  `did-fail-load` 时以 `Error` 拒绝，`message` 形如 `ERR_FILE_NOT_FOUND (-6) loading 'file:///x.html'`，带 `errno`、`code`、`url`；
  被新的导航打断 → `ERR_ABORTED (-3)`。`loadFile(path, { query, search, hash })` = `loadURL(pathToFileURL(path) + …)`。
- `getURL()`、`getTitle()`、`isDestroyed()`、`close(opts)`、`focus()`、`isFocused()`、`isLoading()`、`isLoadingMainFrame()`、`isWaitingForResponse()`、
  `stop()`、`reload()`、`reloadIgnoringCache()`、`isCrashed()`、`forcefullyCrashRenderer()`（`Isolate::interrupt` 后按 `reason: 'killed'` 发 `render-process-gone`）
- 历史：`navigationHistory`（`canGoBack()`、`canGoForward()`、`canGoToOffset(n)`、`goBack()`、`goForward()`、`goToIndex(i)`、`goToOffset(n)`、
  `getActiveIndex()`、`length()`、`getEntryAtIndex(i)` → `{ url, title }`、`getAllEntries()`、`clear()`、`removeEntryAtIndex(i)`）；
  旧的 `webContents.canGoBack()` 等保留并在首次调用时打印弃用警告。历史条目由主进程维护（每次 `did-navigate` 推入）。
- `setUserAgent(ua)`/`getUserAgent()`（默认 `app.userAgentFallback`）
- `insertCSS(css, { cssOrigin: 'author'|'user' })` → Promise<key>（渲染器里 `Document::set_stylesheet(key, css)`）、`removeInsertedCSS(key)`
- `executeJavaScript(code, userGesture?)` → Promise（`ToRenderer::ExecuteJavaScript`；结果克隆返回；脚本抛错 → 拒绝，错误对象经克隆）
- `executeJavaScriptInIsolatedWorld(worldId, scripts, userGesture?)`（P21 后实现）
- `setIgnoreMenuShortcuts(bool)`、`setAudioMuted`/`isAudioMuted`/`isCurrentlyAudible`（`false`）
- 缩放：`setZoomFactor(f)`/`getZoomFactor()`、`setZoomLevel(l)`/`getZoomLevel()`（factor = 1.2^level）、`setVisualZoomLevelLimits(min,max)` → Promise；
  `ToRenderer::SetZoomFactor` 影响 CSS 像素到窗口像素的比例（P30）
- 编辑命令：`undo` `redo` `cut` `copy` `paste` `pasteAndMatchStyle` `delete` `selectAll` `unselect` `centerSelection` `scrollToTop` `scrollToBottom`
  `adjustSelection({start,end})` `replace(text)` `replaceMisspelling(text)` `insertText(text)`（Promise）——转成渲染器的 `document.execCommand`/输入（**需要 P02 追加 `ToRenderer::EditCommand(EditCommand)`**）
- `findInPage(text, opts)` / `stopFindInPage(action)`：v1 返回递增 requestId 并发一次 `found-in-page`（`matches: 0`）——标注为未实现
- `capturePage(rect?, opts?)`：同 BrowserWindow
- `isBeingCaptured()`（`false`）、`isOffscreen()`（`false`）、`startPainting`/`stopPainting`/`isPainting`/`setFrameRate`/`getFrameRate`/`invalidate`（offscreen 专用，记录）
- 打印：`getPrintersAsync()`（`[]`）、`print()`（回调 `(false, 'Printing is not supported by Verve')`）、`printToPDF()`（reject `ERR_NOT_SUPPORTED`）
- DevTools：`openDevTools(opts)`、`closeDevTools()`、`isDevToolsOpened()`、`isDevToolsFocused()`、`toggleDevTools()`、`inspectElement(x,y)`、
  `inspectSharedWorker()`、`inspectServiceWorker()`、`setDevToolsWebContents()`、`addWorkSpace`/`removeWorkSpace`、`devToolsWebContents`（`null`）——交给 P43；
  P43 之前：记录状态、发 `devtools-opened`/`devtools-closed`
- `send` / `sendToFrame` / `postMessage` / `ipc`（P12）
- `sendInputEvent(event)`：`{ type: 'mouseDown'|'mouseUp'|'mouseMove'|'mouseEnter'|'mouseLeave'|'contextMenu'|'mouseWheel'|'keyDown'|'keyUp'|'char', x, y, button, clickCount, keyCode, modifiers, deltaX, deltaY }`
  → 经 Hub 作为 `ToRendererInput` 投递（与 Shell 来的输入同一条路径；**测试和自动化非常依赖它，必须实现**）
- `enableDeviceEmulation`/`disableDeviceEmulation`（记录）、`startDrag(item)`（P24 之前 no-op）、`savePage`（reject）、
  `beginFrameSubscription`/`endFrameSubscription`（不支持）、`downloadURL`（P16 之前抛）
- `getOSProcessId()`（`std::process::id()`）、`getProcessId()`（与 `IpcMainEvent.processId` 相同的稳定整数）、`getType()`（`'window'`，`window.open` 弹出的也是 `'window'`）、
  `takeHeapSnapshot(path)`（reject）、`getBackgroundThrottling`/`setBackgroundThrottling`、`setImageAnimationPolicy`、
  `getWebRTCIPHandlingPolicy`/`setWebRTCIPHandlingPolicy`/`getWebRTCUDPPortRange`/`setWebRTCUDPPortRange`（记录）、`getMediaSourceId(requestWebContents)`、
  `setWindowOpenHandler(handler)`

### 属性

`id`、`session`（P16；P16 前为带 `defaultSession` 外形的空对象）、`navigationHistory`、`hostWebContents`（`null`）、`devToolsWebContents`、
`debugger`（对象，`attach` 抛 not supported）、`backgroundThrottling`、`mainFrame`（`WebFrameMain`）、`opener`（`window.open` 创建时为打开者的 `WebFrameMain`）、
`focusedFrame`、`audioMuted`、`userAgent`、`zoomLevel`、`zoomFactor`、`frameRate`、`ipc`。

### 静态（`require('electron').webContents`）

`getAllWebContents()`、`getFocusedWebContents()`、`fromId(id)`、`fromFrame(frame)`、`fromDevToolsTargetId(id)`（`undefined`）。

### 事件

| 事件 | 参数 | 来源 | 可阻止 |
|---|---|---|---|
| `did-start-loading` / `did-stop-loading` | | `PageEvent` | |
| `did-start-navigation` | `(details: { url, isSameDocument, isMainFrame, frame, initiator })` | 导航开始 | |
| `will-navigate` | `(details: { url, isMainFrame, frame, initiator, isSameDocument: false }, url)`（第二个参数已弃用但仍传） | 见下 | ✓ |
| `will-frame-navigate` | `(details)` | 同上，在 `will-navigate` 之前 | ✓ |
| `will-redirect` | `(details)` | P20/P16 的 HTTP 重定向 | ✓ |
| `did-redirect-navigation` | `(details)` | | |
| `did-navigate` | `(event, url, httpResponseCode, httpStatusText)` | `PageEvent::DidNavigate`（file/data/自定义协议：200 / `'OK'`？以观测冻结） | |
| `did-frame-navigate` | `(event, url, httpResponseCode, httpStatusText, isMainFrame, frameProcessId, frameRoutingId)` | | |
| `did-navigate-in-page` | `(event, url, isMainFrame, frameProcessId, frameRoutingId)` | `PageEvent::DidNavigateInPage`（hash / `pushState`） | |
| `dom-ready` | `(event)` | `PageEvent::DomReady` | |
| `did-frame-finish-load` | `(event, isMainFrame, frameProcessId, frameRoutingId)` | | |
| `did-finish-load` | | `PageEvent::DidFinishLoad` | |
| `did-fail-load` / `did-fail-provisional-load` | `(event, errorCode, errorDescription, validatedURL, isMainFrame, frameProcessId, frameRoutingId)` | `PageEvent::DidFailLoad`；错误码用 Chromium 的 `net::` 值（`-6` FILE_NOT_FOUND、`-3` ABORTED、`-105` NAME_NOT_RESOLVED、`-102` CONNECTION_REFUSED、`-2` FAILED、`-10` ACCESS_DENIED、`-300` INVALID_URL） | |
| `page-title-updated` | `(event, title, explicitSet)` | 与窗口同名事件同时发，先 webContents 后窗口 | ✓ |
| `page-favicon-updated` | `(event, favicons)` | | |
| `did-create-window` | `(window, details: { url, frameName, options, disposition, referrer, postBody })` | `window.open` 成功创建窗口后 | |
| `will-prevent-unload` | `(event)` | 见关闭往返 | ✓（阻止 = 忽略页面拦截） |
| `render-process-gone` | `(event, details: { reason: 'crashed'|'killed'|'oom'|'clean-exit'|'abnormal-exit'|'launch-failed'|'integrity-failure', exitCode })` | `FromRenderer::Gone`（isolate panic → `crashed`；`forcefullyCrashRenderer` → `killed`） | |
| `unresponsive` / `responsive` | | 同窗口 | |
| `destroyed` | | | |
| `input-event` | `(event, inputEvent)` | 每个输入事件（v1 可只对键盘发） | |
| `before-input-event` | `(event, input: { type: 'keyDown'|'keyUp', key, code, isAutoRepeat, isComposing, shift, control, alt, meta, location, modifiers })` | 见下 | ✓ |
| `enter-html-full-screen` / `leave-html-full-screen` | | | |
| `zoom-changed` | `(event, 'in'|'out')` | Ctrl+滚轮（P32 回报） | |
| `blur` / `focus` | | | |
| `devtools-opened` / `devtools-closed` / `devtools-focused` / `devtools-reload-page` / `devtools-open-url` / `devtools-search-query` | | P43 | |
| `certificate-error` / `select-client-certificate` / `login` | | P16 | ✓ |
| `found-in-page` | `(event, result)` | | |
| `media-started-playing` / `media-paused` / `audio-state-changed` | | verve-dom media（可后做） | |
| `did-change-theme-color` | `(event, color|null)` | `<meta name="theme-color">` | |
| `update-target-url` | `(event, url)` | 指针悬停在链接上（P32） | |
| `cursor-changed` | `(event, type, image?, scale?, size?, hotspot?)` | `RendererView.cursor` 变化 | |
| `context-menu` | `(event, params)` | 见下 | |
| `console-message` | `(event)`，`event` 上有 `level`（`'info'|'warning'|'error'|'debug'`）、`message`、`lineNumber`、`sourceId`、`frame`；旧位置参数 `(event, level(0-3), message, line, sourceId)` 同时传入 | `FromRenderer::Console` | |
| `preload-error` | `(event, preloadPath, error)` | P21 | |
| `ipc-message` / `ipc-message-sync` | | P12 | |
| `preferred-size-changed` | `(event, preferredSize)` | 仅 `enablePreferredSizeMode` | |
| `frame-created` | `(event, { frame })` | 主框架创建时 | |
| `content-bounds-updated` | `(event, bounds)` | 页面 `window.moveTo/resizeTo` | ✓ |
| `plugin-crashed`、`will-attach-webview`、`did-attach-webview`、`paint`、`select-bluetooth-device`、`select-usb-device`、`select-hid-device`、`select-serial-port` | | v1 不发 | |

**一次正常加载的事件顺序**（以 Electron 观测冻结）：
`did-start-loading` → `did-start-navigation` → `did-navigate` → `page-title-updated`（若有 `<title>`）→ `dom-ready` →
`did-frame-finish-load` → `did-finish-load` → `did-stop-loading`；`ready-to-show` 在 `dom-ready` 之后、通常在 `did-finish-load` 前后，取决于首帧。

### `will-navigate`

- 只对**渲染器发起**的跨文档导航发出：`location.href = …`、`location.assign/replace`、点链接（无 `target`/`target=_self`）、表单提交——
  即 verve-dom 的 `Request::Navigate { url, replace }` / `Request::Submit`（P20 把 Submit 转成导航请求）。
- **不**对 `loadURL`/`reload`/`goBack` 等主进程发起的导航发出；不对同文档导航（hash、`pushState`）发出。
- 顺序：`will-frame-navigate` → `will-navigate`；任一被阻止 → 不导航，页面留在原处（无事件）；否则 `ToRenderer::Load` 继续。

### `setWindowOpenHandler` 与 `window.open`

渲染器 `window.open(url, frameName, features)` 或 `<a target="_blank">` 点击 → `FromRenderer::Dom(Request::Open(...))`。
现有 `Request::Open(String)` 只带 URL，**需要 P23/P20 把它扩展为 `Open { url, frame_name, features, disposition, referrer, post_body }`**
（`disposition`：`'default'`、`'foreground-tab'`（`target=_blank` 链接）、`'background-tab'`（中键/Ctrl+点击）、`'new-window'`（有 features 的 `window.open`）、`'other'`）。

主进程：
1. 若注册了 handler：调用 `handler({ url, frameName, features, disposition, referrer: { url, policy }, postBody })`，返回值：
   - `{ action: 'deny' }` → 不创建，`window.open` 返回 `null`；
   - `{ action: 'allow', overrideBrowserWindowOptions?, outlivesOpener?: false, createWindow?: (options) => WebContents }` → 创建；
   - 返回其他值 → 主进程 `console.error('The window open handler response must be an object with an \'action\' property of \'allow\' or \'deny\'.')`，按 `deny`。
2. 没有 handler → 允许。
3. 创建：选项 = 解析 `features`（`width`、`height`、`left`→`x`、`top`→`y`、`resizable`、`show` 等，`yes/no/1/0`）
   合并 `overrideBrowserWindowOptions`；`webPreferences` 继承打开者的（`nodeIntegration`、`contextIsolation`、`sandbox`、`preload`、`session`），
   Electron 的规则是**子窗口不能比父窗口拥有更多权限**；`parent` 不自动设置。新窗口 `webContents.opener` = 打开者的主框架。
4. 发打开者 webContents 的 `did-create-window(window, details)`、`app` 的 `browser-window-created`。
5. 打开者关闭时，若 `outlivesOpener` 为 `false`，子窗口一并关闭。

渲染器里 `window.open` 的返回值：跨 isolate 无法给出真实 `Window`，返回一个代理对象（Electron 在 sandbox/跨源时也只给代理），
支持 `closed`（getter）、`close()`、`focus()`、`blur()`、`postMessage(message, targetOrigin)`（经主进程转发为对方页面的 `message` 事件，
`event.source` 为指回打开者的同类代理）、`location`（只写 `href`，触发对方导航）。该代理是原生对象（P20 实现渲染器侧，本包定义主进程转发：
**需要 P02 追加 `ToRenderer::WindowMessage { from: ContentsId, data: Cloned, origin }` 与 `FromRenderer::WindowProxyOp { target, op }`**）。

### `context-menu`

渲染器的 `contextmenu` DOM 事件未被 `preventDefault` 时，渲染器发 **`FromRenderer::ContextMenu(ContextMenuParams)`（需要 P02 追加）**，主进程发 `context-menu`，`params`：
`x`、`y`、`frame`、`linkURL`、`linkText`、`pageURL`、`frameURL`、`srcURL`、`mediaType`（`'none'|'image'|'audio'|'video'|'canvas'|'file'|'plugin'`）、
`hasImageContents`、`isEditable`、`selectionText`、`titleText`、`altText`、`suggestedFilename`、`selectionRect`、`selectionStartOffset`、
`referrerPolicy`、`misspelledWord`（`''`）、`dictionarySuggestions`（`[]`）、`frameCharset`、`formControlType`、`spellcheckEnabled`、
`menuSourceType`（`'mouse'|'keyboard'|…`）、`mediaFlags`（各 `false`）、`editFlags`（`canUndo`、`canRedo`、`canCut`、`canCopy`、`canPaste`、`canDelete`、`canSelectAll`、`canEditRichly`）。
Electron **不**显示默认右键菜单；没有监听者时什么也不发生。

### `before-input-event`

需要在按键到达页面前同步询问主进程，但 Shell 不能等 JS（P02 规则）。做法：
- 主进程只要有 `before-input-event` 监听者，就通过 Hub 设置该 webContents 的 `intercept_keys = true`（监听者归零时清除）。
- `intercept_keys` 为真时，Shell 把键盘事件发给**主进程**（追加 `FromShell::KeyForContents { contents, key }`），主进程发事件；
  未阻止 → 主进程把它转发给渲染器（`Hub::input`）。否则 Shell 直接发渲染器（热路径）。
- 菜单加速键（P15）在此之后处理，`setIgnoreMenuShortcuts(true)` 时跳过。

### `WebFrameMain`（最小）

`require('electron').webFrameMain.fromId(processId, routingId)`、`fromFrameToken`；实例：`url`、`origin`、`name`、`top`（自身）、`parent`（`null`）、
`frames`/`framesInSubtree`（`[]`/`[自身]`）、`frameTreeNodeId`、`processId`、`routingId`、`frameToken`、`visibilityState`、`detached`、`isDestroyed()`、
`executeJavaScript(code, userGesture)`、`reload()`、`send`、`postMessage`、`ipc`、`collectJavaScriptCallStack()`（reject）。子框架等 P23 的 `<iframe>` 之后再扩展。

## 步骤

1. `web_preferences.rs`、`WindowOptions` 解析（含颜色解析）与默认值；单元测试对照上表。
2. 注册表、`WindowState` 缓存、销毁检查、ID 计数器。
3. `BrowserWindow` 类（可被 `class extends` 与 `util.inherits` 继承）、全部方法与属性、静态方法；`WindowOp` 映射。
4. `FromShell::Window` → 事件；`page-title-updated` 双发。
5. 关闭往返 + 与 P10 退出状态机对接 + 父子/模态。
6. `WebContents` 类：加载 Promise、导航历史、事件映射（`PageEvent` → 事件）、`executeJavaScript`、`insertCSS`、编辑命令、`sendInputEvent`。
7. `will-navigate`、`setWindowOpenHandler`、`window.open` 代理的主进程侧、`context-menu`、`before-input-event`。
8. 心跳：Hub 记录每个渲染器 isolate 最后一次 tick 开始/结束时间，主进程每秒检查一次。
9. 追加的消息变体全部写回 P02 文档的契约代码块（单独的 docs commit）。

## 验收

`cargo test -p verve-electron --test browser_window --test web_contents`（无头，P00 的 `HeadlessShell`）：
- 默认值：`new BrowserWindow()` 后 `getSize()` 为 `[800, 600]`、`getTitle()` 为 `'Electron'`、`webContents.getUserAgent()` 含 `Electron/39.2.0`。
- `getAllWindows`/`fromId`/`fromWebContents`/`getFocusedWindow`；ID 递增不复用；销毁后方法抛 `Object has been destroyed`。
- `class MyWin extends BrowserWindow` 与 `util.inherits` 都可构造并收到事件。
- 加载顺序：data URL、`loadFile` 的事件顺序与冻结观测一致；`loadURL` 不存在的文件 → Promise 以 `ERR_FILE_NOT_FOUND (-6)` 拒绝并发 `did-fail-load`。
- `ready-to-show`：`show:false` 时发且只发一次；`paintWhenInitiallyHidden:false` 时不发。
- 关闭：`close` 阻止；`beforeunload` 拦截 + `will-prevent-unload` 两种分支；`destroy()` 不发 `close`；子窗口随父关闭。
- `will-navigate`：点击链接触发、可阻止；`loadURL` 不触发；hash 变化发 `did-navigate-in-page`。
- `window.open`：无 handler 时创建窗口并发 `did-create-window`；handler 返回 deny 时渲染器得到 `null`；`overrideBrowserWindowOptions` 生效；
  features 字符串解析；代理 `postMessage` 往返；打开者关闭时子窗口随之关闭。
- `executeJavaScript` 返回值/Promise/抛错；`insertCSS` 后 `getComputedStyle` 变化，`removeInsertedCSS` 恢复。
- `sendInputEvent({type:'mouseDown',x,y})` + `mouseUp` 让页面 `click` 监听者触发（需 P30 未就绪时以 `set_layout` 手工给框）。
- `context-menu` 参数里 `linkURL`、`isEditable`、`selectionText` 正确。
- `before-input-event` 阻止后页面收不到 `keydown`。
- 渲染器 isolate panic → `render-process-gone`，`reason: 'crashed'`；窗口仍在，可 `reload()` 恢复。

## 不做

窗口的真实创建与绘制（P34/P30）、菜单（P15）、`session`（P16）、页面加载管线本身（P20）、preload（P21）、`BrowserView`/`WebContentsView`/`BaseWindow.contentView`（v2）、
`<webview>`、离屏渲染、打印、真实 DevTools（P43）、`findInPage` 的真实搜索。

## 状态

（实现者完成后填写）
