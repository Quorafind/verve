# P34 Shell：窗口管理

**依赖**：P02  **规模**：M  **修改**：`crates/verve-gpui/src/lib.rs`、`shell.rs`、`window.rs`（新）、`platform/{mod,windows,macos,linux}.rs`（新）、`app_region.rs`（新）；`crates/verve-electron/src/msg.rs`（只追加）

## 目标

GPUI 这一侧的"应用外壳"：启动 GPUI `Application`，接 Hub 的消息，为每个 `BrowserWindow` 开关、移动、缩放 GPUI 窗口，
把窗口事件报回主进程；实现无边框窗口、隐藏标题栏、`-webkit-app-region` 拖动区、`backgroundColor`、透明、父子/模态窗口、
多显示器与 DPI 变化；与主进程协调"最后一个窗口关闭"和退出。完成后，P13 的 `BrowserWindow` 的每个方法都有落点。

## 背景（先读）

- P02 的 `ToShell`、`WindowOptions`、`WindowOp`、`FromShell`、`WindowEvent`、`AppEvent`、`Display`、`ShellPort`、`Hub::set_shell_waker`。
- GPUI（Rebon 所用 fork `Quorafind/zed-c@e679b51`）：
  - `app.rs`：`Application::new().run(...)`、`App::open_window(WindowOptions, build_root)`、`App::quit`、`App::set_quit_mode(QuitMode)`
    （`Default/LastWindowClosed/Explicit`）、`App::on_reopen`（macOS 点 Dock）、`App::on_open_urls`、`App::on_window_closed`、`App::on_app_quit`、
    `App::displays()`、`App::primary_display()`、`App::foreground_executor()`、`App::to_async()`、`App::set_dock_menu`、`App::hide`。
  - `platform.rs`：`WindowOptions { window_bounds: Option<WindowBounds>, titlebar: Option<TitlebarOptions>, focus, show, kind: WindowKind,
    is_movable, app_owns_titlebar_drag, inactive_frame_interval, is_resizable, is_minimizable, display_id, window_background, app_id,
    window_min_size, window_decorations, icon /* 仅 X11 */, tabbing_identifier }`；`WindowBounds::{Windowed, Maximized, Fullscreen}(Bounds)`；
    `TitlebarOptions { title, appears_transparent, traffic_light_position }`；`WindowKind::{Normal, PopUp, AnchoredPopup, Floating, LayerShell, Dialog}`；
    `WindowBackgroundAppearance::{Opaque, Transparent, Blurred, MicaBackdrop, MicaAltBackdrop, LiquidGlass}`；`WindowDecorations::{Server, Client}`；
    `PlatformDisplay { id, uuid, bounds, visible_bounds }`。
  - `window.rs`：`Window::{start_window_move, zoom_window, minimize_window, toggle_fullscreen, activate_window, resize, set_window_title,
    set_background_appearance, set_window_edited, remove_window, is_maximized, window_bounds, bounds, scale_factor, on_window_should_close,
    observe_window_bounds（若无则用 platform 回调）, observe_window_activation, observe_window_appearance, insert_window_control_hitbox,
    show_window_menu}`、`WindowControlArea::{Drag, Close, Max, Min}`、`impl HasWindowHandle for Window`。
- 参考：Rebon 的 GPUI 窗口代码（`F:\dev\sandbox-v2\rebon\app\src` 中 `open_window` 的用法、自绘标题栏）。

## 接口契约

### 唤醒

`Hub::set_shell_waker` 注册的闭包在任意线程被调用；实现为：持有 `AsyncApp`（`cx.to_async()`）不可跨线程，所以用一个
`async_channel`/`futures::channel::mpsc` 无界通道：waker 往通道里发一个空消息；GPUI 前台用 `cx.spawn(async move |cx| { while rx.next().await … })`
在主线程上取 Hub 的 `ToShell` 队列和各渲染器的新 view，处理后对相关窗口 `notify`/`refresh`。**不轮询**。

### `WindowOptions`（P02）→ GPUI 逐字段映射

| P02 字段 | GPUI | 说明 / 做不到时 |
|---|---|---|
| `width/height/x/y` | `window_bounds: Some(WindowBounds::Windowed(Bounds))` | `x/y` 为 `None` 时以目标显示器 `visible_bounds` 居中计算 |
| `min_size` | `window_min_size` | |
| `max_size` | 无 | 平台层：Windows 处理 `WM_GETMINMAXINFO`（子类化窗口过程）；macOS `NSWindow.maxSize` |
| `resizable` | `is_resizable` | 运行时修改走平台层 |
| `movable` | `is_movable` | |
| `minimizable` | `is_minimizable` | |
| `maximizable` | 无 | 平台层：Windows 去掉 `WS_MAXIMIZEBOX`；macOS 禁用缩放按钮 |
| `closable` | 无 | 平台层：Windows 灰掉系统菜单 `SC_CLOSE`；macOS 去掉 `NSWindowStyleMaskClosable`；另外 `on_window_should_close` 总返回 false |
| `focusable` | `focus` 初值 | `false` 时平台层设 `WS_EX_NOACTIVATE` / macOS `canBecomeKeyWindow` 子类化（v1 只做 Windows） |
| `always_on_top` | `kind: WindowKind::Floating` | 运行时切换走平台层（`SetWindowPos(HWND_TOPMOST)` / `NSWindow.level`） |
| `fullscreen` | `WindowBounds::Fullscreen` | |
| `show` | `show`（及 `focus`） | `false` 时创建隐藏窗口；`Show/Hide` 运行时走平台层（`ShowWindow` / `orderFront`/`orderOut`） |
| `frame: false` | `titlebar: Some(TitlebarOptions { appears_transparent: true, .. })` + Linux `window_decorations: Some(Client)` | 无系统标题栏；Windows 上 GPUI 仍保留可调整大小的边框（与 Electron 一致）。macOS 隐藏交通灯：平台层 |
| `title_bar_style: Hidden` | `appears_transparent: true`，保留系统按钮 | macOS 保留交通灯；Windows 上 Electron 的 `hidden` 等同无标题栏，配合 `titleBarOverlay`（v1 不做 overlay，记录） |
| `title_bar_style: HiddenInset` | 同上 + `traffic_light_position` 内缩（macOS） | |
| `transparent` | `window_background: Transparent` | 页面 `html, body` 背景透明时才看得到；DomElement 不画默认白底 |
| `background_color` | 窗口首帧前清屏颜色 | DomElement 最底层画一个整窗 quad；没有页面背景时就是它（Electron 默认 `#FFF`） |
| `title` | `titlebar.title` | |
| `icon` | Windows：平台层 `WM_SETICON`（从 PNG/ICO 生成 HICON）；Linux X11：`WindowOptions.icon`；macOS 忽略（用 bundle 图标） | |
| `parent` | 无 | 平台层：Windows `SetWindowLongPtr(GWLP_HWNDPARENT)`（owner 窗口，始终在父上方、随父最小化）；macOS `addChildWindow:ordered:` |
| `modal` | `kind: WindowKind::Dialog`（若平台支持） | 另：Windows `EnableWindow(parent, FALSE)`，关闭时恢复；macOS sheet v1 不做（作为普通子窗口） |
| `skip_taskbar` | 无 | 平台层：Windows `WS_EX_TOOLWINDOW`；macOS 无此概念 |
| `kiosk` | `Fullscreen` + 平台层置顶 | |

### `WindowOp` 落点

| 操作 | 实现 |
|---|---|
| `Show/ShowInactive/Hide` | 平台层（GPUI 没有显示/隐藏已创建窗口的公开 API） |
| `Close` | 走与用户点关闭相同的询问流程（下节） |
| `Destroy` | `window.remove_window()`，不询问 |
| `Focus` | `window.activate_window()`；`Blur`：平台层把焦点交给下一个窗口 |
| `Minimize` | `window.minimize_window()` |
| `Maximize/Unmaximize` | `window.zoom_window()`（按当前 `is_maximized()` 判断是否需要切换） |
| `Restore` | 平台层（`ShowWindow(SW_RESTORE)` / `deminiaturize`） |
| `SetFullScreen(b)` | 状态不同时 `toggle_fullscreen()` |
| `SetBounds` | 尺寸：`window.resize(size)`；位置：平台层（`SetWindowPos` / `setFrameOrigin`）；`animate` 只在 macOS 生效 |
| `SetMinSize/SetMaxSize/SetResizable/SetAlwaysOnTop/SetOpacity/SetSkipTaskbar/SetIgnoreMouseEvents` | 平台层（`WM_GETMINMAXINFO`、样式位、`SetLayeredWindowAttributes`、`WS_EX_TRANSPARENT`；macOS 对应 NSWindow 属性） |
| `Center` | 计算后 `SetBounds` |
| `SetTitle` | `window.set_window_title()` |
| `SetBackgroundColor` | 更新 DomElement 底色并 `refresh` |
| `SetProgressBar` | Windows `ITaskbarList3::SetProgressValue/SetProgressState`；macOS Dock tile 进度 v1 不做 |
| `FlashFrame` | Windows `FlashWindowEx`；macOS `requestUserAttention` |
| `SetIcon` | 同 `icon` |

平台层 `platform/windows.rs` 用 `HasWindowHandle` 取 `HWND` 与 `windows-sys`（仓库已依赖，按需追加 `Win32_UI_WindowsAndMessaging`、
`Win32_UI_Shell` 特性）；`platform/macos.rs` 用 `objc2`/`objc2-app-kit`（GPUI 已依赖，确认版本一致，避免重复）。平台调用后，GPUI 会从系统消息里
得知尺寸/位置变化，不要自己改 GPUI 的内部状态。

### 关闭询问往返

1. `window.on_window_should_close(cx, |_, _| false)`：GPUI 询问时**总是**先拒绝，并发 `FromShell::Window { event: CloseRequested }`。
2. 主进程发 `'close'` 事件（可 `preventDefault`），以及渲染器的 `beforeunload`（P13 负责：先问渲染器，`returnValue` 非空则视为阻止，
   并发 `will-prevent-unload`），然后回 `WindowOp::CloseAnswer { prevented }`。
3. `prevented == false` → `remove_window()` → 发 `WindowEvent::Closed`；Hub 结束该窗口渲染器 isolate。
4. 超时保护：主进程 5 秒内未答复（主进程卡死）时，第二次点关闭直接关（与 Chromium "页面无响应"相似），并在 stderr 记一行。

### 窗口事件回报

| 来源 | `WindowEvent` |
|---|---|
| `observe_window_activation` | `Focus`/`Blur`（同时给该渲染器发 `ToRendererInput::Focus`） |
| 尺寸变化（`observe_window_bounds` 或在 `render` 里比较 `window.bounds()`） | `Resized`（节流到每帧一次；同时 `ToRendererInput::Viewport`）、`Moved` |
| `is_maximized()`/`is_fullscreen` 状态翻转 | `Maximized/Unmaximized`、`EnterFullScreen/LeaveFullScreen` |
| 最小化/还原（平台层监听 `WM_SIZE SIZE_MINIMIZED` / `windowDidMiniaturize`） | `Minimized/Restored` |
| `scale_factor()` 变化（拖到另一块屏幕） | `ScaleFactor`（同时 `Viewport { scale }`，渲染器更新 `devicePixelRatio` 和 `@media (resolution)`） |
| `observe_window_appearance` | `AppEvent::ThemeChanged`（同时所有渲染器 `ColorScheme`；P14 的 `nativeTheme` 由此更新） |
| 首次显示 | `Shown`；`Hide` 后 `Hidden` |

### `-webkit-app-region` 拖动区

- P30 的 `Hitbox` 项带 `app_region: Option<AppRegion>`（`Drag` / `NoDrag`），由计算样式 `-webkit-app-region`（或标准名 `app-region`）得出；
  `no-drag` 的子孙覆盖 `drag` 的祖先。
- DomElement 的 `prepaint`：对每个 `Drag` 区域减去其中 `NoDrag` 区域后的矩形，`window.insert_hitbox(..)` 然后
  `window.insert_window_control_hitbox(WindowControlArea::Drag, hitbox)`。Windows 上 GPUI 用它回答 `WM_NCHITTEST`（双击最大化、Aero Snap 都是系统行为）；
  macOS 设 `app_owns_titlebar_drag: true`，在拖动区的 `mouse_down` 里调 `window.start_window_move()`，双击调 `zoom_window()`。
- 拖动区内不向页面派发鼠标事件（与 Chromium 一致）。
- 无边框窗口的窗口按钮由页面自己画（调用 `win.minimize()` 等走 IPC）；页面若用 `WindowControlArea::Close/Max/Min` 语义（Electron 无），不处理。

### 显示器

- 启动时与 `App::displays()` 变化时（平台层监听 `WM_DISPLAYCHANGE` / `NSApplicationDidChangeScreenParametersNotification`，或每次窗口
  `ScaleFactor` 变化时重查）构造 `Vec<Display>` 发 `FromShell::Displays`：`id` = `DisplayId` 数值，`bounds` = `bounds()`，`work_area` = `visible_bounds()`，
  `primary` = 与 `primary_display()` 同 id。`scale_factor`：GPUI 的 `PlatformDisplay` 不提供，平台层取（`GetDpiForMonitor` / `NSScreen.backingScaleFactor`）。
- `screen.getCursorScreenPoint()`（P14）需要同步答复：Shell 不可阻塞，所以 P14 在主进程线程直接调用平台 API（`GetCursorPos`），不经 Shell。

### 应用生命周期

- `App::set_quit_mode(QuitMode::Explicit)`：**由主进程决定何时退出**（Electron 语义：最后一个窗口关闭时发 `window-all-closed`，有监听者则不退出；
  macOS 应用通常保持运行）。Shell 收到 `ToShell::Quit { exit_code }` 才 `cx.quit()`，并让 `electron` 进程以该码退出。
- `App::on_reopen` → `AppEvent::Activate { has_visible_windows }`；`App::on_open_urls` → `OpenUrl`（协议注册由 P10/P14 处理）；
  macOS "打开文件"（Dock 拖入、Finder 双击关联文件）→ `OpenFile`（若 GPUI 未暴露，在平台层注册 `application:openFiles:`，或在 fork 中补；记录）。
- `on_app_quit`：系统要求退出（注销、Cmd+Q 菜单由 P15 发 `app.quit()`）时先转发给主进程走 `before-quit/will-quit`，异步等待其 `Quit` 消息，最多 3 秒。

### 无头模式

`verve-gpui` 是 `verve-electron` 的可选依赖（feature `gui`），无头构建不编译本包任何代码；P00 的 `HeadlessShell` 实现同一个 `ShellPort`，
并模拟窗口事件（创建后立即 `Shown`/`Focus`、按 `SetBounds` 回 `Resized`），保证主进程逻辑在两种模式下一致。

## 步骤

1. 唤醒通道与 `ToShell` 处理循环；`Quit`。
2. `CreateWindow` 映射表的 GPUI 原生部分；窗口事件回报；关闭往返。
3. 平台层 Windows（全部 `WindowOp`），然后 macOS（至少 show/hide/bounds/always-on-top/parent/opacity/交通灯），Linux 只做 GPUI 原生部分。
4. app-region 拖动区。
5. 显示器列表与 DPI 变化。
6. 生命周期：QuitMode、reopen、open-urls/open-file、系统退出。
7. 手动验收清单逐项截图/录屏附 PR。

## 验收

- `cargo test -p verve-electron --test window_lifecycle`（无头）：`close` 被阻止时窗口不关；`destroy()` 不发 `close`；关闭最后窗口且无
  `window-all-closed` 监听 → 进程退出码 0；有监听 → 不退出。
- 夹具 `tests/electron/apps/windows/`（手动，Windows 与 macOS 各一遍，录屏附 PR）：
  `frame:false` 窗口用 `-webkit-app-region: drag` 的标题栏可拖动、双击最大化、Aero Snap 可用、按钮区（`no-drag`）可点击；
  `show:false` + `ready-to-show` 后显示无白屏闪烁；`backgroundColor: '#1e1e1e'` 首帧即为深色；`transparent: true` 圆角卡片窗口；
  `parent` + `modal` 子窗口阻止父窗口输入；`setAlwaysOnTop`、`setBounds`、`center`、`setProgressBar(0.5)`、`flashFrame(true)` 生效；
  窗口拖到 150% DPI 的第二屏后 `window.devicePixelRatio === 1.5` 且字迹清晰。

## 不做

`titleBarOverlay`（Windows 11 自绘窗口按钮覆盖层）、`vibrancy`/`backgroundMaterial` 之外的材质（`MicaBackdrop` 可选做：映射 `backgroundMaterial: 'mica'`）、
macOS sheet、原生窗口标签页（`tabbingIdentifier` 可直接映射，但不保证）、`setShape`、`setThumbarButtons`、`setOverlayIcon`、触控栏。

## 状态

（实现者完成后填写）
