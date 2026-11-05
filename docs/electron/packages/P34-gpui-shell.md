# P34 Shell：窗口管理

**依赖**：P02  **规模**：M  **修改**：`crates/verve-gpui/src/lib.rs`、`shell.rs`、`window.rs`（新）、`platform/{mod,windows,macos,linux}.rs`（新）、`app_region.rs`（新）；`crates/verve-electron/src/msg.rs`（只追加）

## 目标

GPUI 这一侧的"应用外壳"：启动 GPUI `Application`，接 Hub 的消息，为每个 `BrowserWindow` 开关、移动、缩放 GPUI 窗口，
把窗口事件报回主进程；实现无边框窗口、隐藏标题栏、`-webkit-app-region` 拖动区、`backgroundColor`、透明、父子/模态窗口、
多显示器与 DPI 变化；与主进程协调"最后一个窗口关闭"和退出。完成后，P13 的 `BrowserWindow` 的每个方法都有落点。

## 背景（先读）

- P02 的 `ToShell`、`WindowOptions`、`WindowOp`、`FromShell`、`WindowEvent`、`AppEvent`、`Display`、`ShellPort`（`send` + `wake`）、`Hub::set_shell`。
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

`ShellPort::wake` 在任意线程被调用（`Hub::set_shell` 注册的 Shell 就是唤醒的入口，没有第二个 waker）；实现为：持有 `AsyncApp`（`cx.to_async()`）不可跨线程，所以用一个
容量为 1 的 `async_channel`：`wake` 往通道里 `try_send` 一个空消息，`Full` 就是合并（取队列时本来就取空）；GPUI 前台用 `cx.spawn(async move |cx| { while rx.next().await … })`
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

`verve-electron` 不依赖 `verve-gpui`（P02 已定：没有 `gui` feature，`electron` 二进制在 `verve-gpui` 里），无头构建不编译本包任何代码；P00 的 `HeadlessShell` 实现同一个 `ShellPort`，
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

### 完成了什么

- **结构**（`crates/verve-gpui/src`）：`shell.rs` 只做分发（队列、`Item`、`Shell::process`，按窗口 id 转给 `Window`）；`window.rs` 一扇窗口
  （`open` 的 GPUI 选项映射、`Window::apply` 的每个 `WindowOp`、关闭往返）；`report.rs` 窗口事件回报；`app_region.rs` 拖动区；
  `platform/{mod,windows,taskbar,macos,other}.rs` 系统层。**与任务的差别**：没有 `linux.rs`，Linux 与 macOS 共用 `other.rs`（只有 GPUI 自己
  有 API 的部分），macOS 的 `NSWindow` 调用在 `macos.rs`；`Shell` 不再是 GPUI 实体，处理循环是 `lib.rs` 里 `cx.spawn` 的一个任务，
  `process` 是同步的、不在任何 GPUI 借用之内（见下「设计要点」）。
- **全屏与不可调整大小**：全屏是平台层自己做的，GPUI 不知道，所以子类化的窗口过程在全屏时对 `WM_NCHITTEST` 一律答 `HTCLIENT`（拖动区与上边缘缩放都不再生效），全屏时去掉 `WS_MAXIMIZEBOX`；`setResizable(false)` 时 GPUI 的上边缘缩放答案改成 `HTCLIENT`，`setResizable(true)` 只在 `maximizable` 时恢复最大化框。`focusable: false` 的窗口对 `WM_MOUSEACTIVATE` 答 `MA_NOACTIVATE`。最小化最大化的窗口只报 `Minimized`（`IsZoomed` 此时为假，差分不报 `Unmaximized`），还原时报 `Restored`、`Maximized`。`SetSkipTaskbar` 不再让有焦点的窗口失焦。
- **事件回报 = 观察 + 差分**（`report.rs`）：系统只说“变了”，不说顺序，所以窗口被**看一眼**（`Observed`：可见、焦点、最小化、最大化、全屏、外框、内容区、
  缩放），事件是与上一眼的差，顺序固定为 P00 状态机 / Electron 39.8.10 的顺序：`Shown|Hidden` → `Enter|LeaveFullScreen` → `Minimized|Restored` →
  `Resized` → `Moved`（离开最大化不发）→ `Maximized|Unmaximized` → `ScaleFactor` → `Focus|Blur`。Windows 上子类化窗口过程，`WM_SIZE/MOVE/SHOWWINDOW/
  ACTIVATE/DPICHANGED` 只往窗口 `PostMessage` 一条 `WM_APP+0x34`，处理它时才看一眼（一次最小化的各条消息因此合成一次）；`Window::apply` 在每个
  操作后直接看一眼。人拖动、系统贴靠与应用调用走同一条路，所以是同一个顺序。`Closed` 由 `Reporter::close` 保证只发一次；打不开的窗口报 `Closed`。
  页面同时收到 `Viewport`（内容区，不是外框）、`Visibility`、`Focus`、`ColorScheme`（创建时与主题变化时）。
- **`WindowOp` 全部落地（Windows）**：`Show/ShowInactive/Hide`（`ShowWindow`）、`Focus`（`SetForegroundWindow`）、`Blur`（交给下一个可见窗口，同 Chromium
  `Deactivate`，且只在本窗口是前台时）、`Minimize/Maximize/Unmaximize/Restore`、`SetFullScreen`（自己做：存样式与矩形，去 `WS_CAPTION|WS_THICKFRAME`，
  盖满显示器；GPUI 的 `toggle_fullscreen` 对隐藏窗口只记状态、且异步）、`SetBounds`（外框，夹在限制内）、`SetMin/MaxSize`（`WM_GETMINMAXINFO`，
  外框尺寸，最大 0 = 不限）、`Center`（所在显示器工作区）、`SetTitle`、`SetResizable`、`SetAlwaysOnTop`、`SetBackgroundColor`（`DomView` 底色）、
  `SetProgressBar`（`ITaskbarList3`，手写 vtable，`taskbar.rs`）、`SetIcon`（`.ico` 或 `.png`，`CreateIconFromResourceEx`）、`FlashFrame`、`SetOpacity`
  （`WS_EX_LAYERED`）、`SetSkipTaskbar`、`SetIgnoreMouseEvents`、`Close`/`CloseAnswer`/`Destroy`。
- **创建选项**：`frame`（GPUI 的 Windows 窗口**从不带标题栏**，所以 `frame: true` 时平台层加 `WS_CAPTION`；`false` 与 `titleBarStyle: hidden/hiddenInset` 同样
  无标题栏、保留可调整边框）、`width/height/x/y`（外框；居中取显示器工作区，同 P00）、`min/max_size`、`resizable`、`movable`（去掉系统菜单 `SC_MOVE`）、
  `minimizable`、`maximizable`（去 `WS_MAXIMIZEBOX`）、`closable`（灰掉 `SC_CLOSE`，且用户关闭不发 `CloseRequested`）、`focusable`（`WS_EX_NOACTIVATE`）、
  `always_on_top`、`fullscreen`、`kiosk`（全屏 + 置顶）、`show`、`transparent`（`WindowBackgroundAppearance::Transparent`，`DomView` 不画底色）、
  `background_color`（`DomView` 最底层底色，缺省白）、`title`、`icon`、`skip_taskbar`、`parent`（Windows owner 窗口）与 `modal`（按 Electron 的 `Increment/DecrementChildModals`：每个 owner 一个计数，模态子窗口**显示**时加一并 `EnableWindow(owner, FALSE)`，
  隐藏/销毁时减一，减到 0 才恢复并把前台还给 owner；`show: false` 的模态子窗口不挡父窗口。**没有**用真 Electron 39 探针核对 `parent.isEnabled()`，依据是复核给的 `native_window_views.cc`）。**窗口先隐藏地创建**，加上边框、限制、位置之后才按 `show` 显示，所以不会先以别的样子出现。
- **关闭**：用户关闭（`WM_CLOSE`，关闭按钮、Alt+F4）→ `on_window_should_close` 总返回 false 并发 `CloseRequested`（已问过、还没答复时不再问；时间戳不被后来的点击刷新）；主进程回 `CloseAnswer`；`prevented: false`
  或 `Destroy` → `remove_window` + `Closed`。**5 秒无应答**后下一次关闭直接销毁并在 stderr 记一行。系统自己销毁窗口（GPUI 的关闭回调在 `App` 被借用时返回 true、会话结束）由 `WM_DESTROY` 钩子报 `Closed` 并让 `Shell` 忘掉它（`Item::Gone`）。**父窗口销毁时先销毁它的子窗口并各报 `Closed`**
  （系统会不声不响地带走 owned 窗口）。
- **拖动区**（`app_region.rs`）：`AppRegion { rect, drag }`（文档序）→ `drag_area`（后来的 `no-drag` 减去先前的 `drag`，后来的 `drag` 再盖回）→ 每块矩形一个
  `Hitbox` + `insert_window_control_hitbox(Drag)`（必须在 paint 阶段，prepaint 会 panic）；Windows 上 `WM_NCHITTEST` 回 `HTCAPTION`，拖动、双击最大化、Aero Snap
  是系统行为；macOS 在 `mouse_down` 里 `start_window_move` / 双击 `zoom_window`（`app_owns_titlebar_drag`）。输入是 `verve_gpui::Control::set_app_regions`。
- **显示器**：启动与 `observe_displays` 时发 `FromShell::Displays`（Windows：`EnumDisplayMonitors` + `GetDpiForMonitor`；其他：`App::displays()`，缩放写 1）。
- **生命周期**：`QuitMode::Explicit`；`Quit` 才 `cx.quit()`；`on_reopen` → `Activate`；`on_open_urls` → `OpenFile`（`file://`）/`OpenUrl`；系统要求退出
  （`on_app_quit` 时主进程还没决定退出）→ 新的 `AppEvent::QuitRequested`（走 `app.quit()` 同一条路），**立刻发出、不等**：GPUI 在调用观察者之后马上丢掉所有窗口，
  只等 `SHUTDOWN_TIMEOUT`（200 毫秒）且不再轮询这里的泵，Windows 上 `WM_ENDSESSION` 之后 `exit(0)`；所以任务书里的“等至多 3 秒”做不到，GPUI 的 `WindowsWindow::drop` 只隐藏窗口、把销毁留给一个不会再跑的任务，随后 `exit(0)`，所以这条路上**没有**窗口会报 `Closed`；
  主进程的 `before-quit` 否决与退出码在这条路上拿不到（P10 会话结束一项）。在 GPUI 关停前截 `WM_QUERYENDSESSION` 才能做到更多，**未做**；主题变化 → `AppEvent::ThemeChanged`
  + 各页 `ColorScheme`（去重）。
- **契约追加**（`docs(electron)`/`feat(electron)` 提交，已写进 P02 的备注）：`Hub::observe_shell`、`WindowEvent::FirstPaint`（第一次在可见时画了一帧）、
  `WindowEvent::NativeHandle(u64)`（`HWND`/`NSView*`，窗口建好后一次）、`ToRendererInput::Visibility(bool)`、`AppEvent::QuitRequested`（`api/app/mod.rs::platform_event` 加了一臂
  调 `quit::quit`，是 P10 的文件里唯一一处改动）。无头 Shell 不发 `FirstPaint`/`NativeHandle`/`Visibility`。

### 设计要点（给后来人）

- **GPUI 的 Windows 窗口过程会回调进 `App`**：任何同步改窗口的 Win32 调用（`SetWindowPos`、`ShowWindow`……）都会收到 `WM_SIZE` 等，回调里 `handle.update` 会在 `App`
  已被借用时 panic。GPUI 自己用 `ShowWindowAsync` 和 `executor.spawn` 避开。这里的办法是：整个 `Shell::process` 跑在 `cx.spawn` 的任务里，不在任何 `update` 之内；
  只有 `remove_window`、`open_window`、读 `HWND` 进 `update`，且不做 Win32 调用。**以后加平台调用也要守这条。**
- **不要对这些窗口调 `Window::activate_window`**：隐藏创建的 GPUI Windows 窗口把初始位置记在 `initial_placement` 里，只有 `activate` 会 `SetWindowPlacement` 回去（还会用 `SendInput`
  合成 Alt 键抢前台）；平台层自己放位置、自己显示。GPUI 的 `border_offset`（创建时量的，没有标题栏）因此对 `frame: true` 窗口是旧的，所以大小全部走外框，不用 `Window::resize`。
- `insert_window_control_hitbox` 只能在 paint 阶段调用（prepaint 里 panic）。

### 体积

`cargo build --release -p verve-gpui --bin electron --features jit` 的 `electron.exe` 为 33,385,472 字节（离 34 MB 上限还剩约 0.61 MB）；对 P10 的 33,144,832 多 240,640 字节，其中含 P10 之后合入的其他包；
拆分见 `docs/electron/size.md` 的 P34 一节（`verve_gpui` 自己的 `.text` 54 KiB，其余是 `windows-sys` 的导入与 GPUI 泛型实例化）。

### 测试

- `crates/verve-electron/tests/support/contract.rs`：把 `window-state` 夹具的步骤（show、hide、showInactive、setBounds、maximize、unmaximize、minimize、restore、blur、focus、
  setFullScreen ×2、close、关闭应答）、安静的操作、被保留的关闭、`Destroy`、限制，写成只认 `ShellPort` + `Hub::observe_shell` 的测试。`tests/shell_contract.rs` 在无头 Shell 上跑
  （默认测试）；`crates/verve-gpui/tests/shell_windows.rs`（`#[ignore = "opens real windows"]`，一个 `#[test]`，因为 GPUI 的 `Application` 一个进程只能跑一次）在真 Shell 上跑同样的步骤，
  并读回 `HWND`：标题栏样式、`WS_EX_*`、不透明度、最小尺寸、标题、`center()`、模态子窗口的 owner 与 `IsWindowEnabled`、父窗口带走子窗口、拖动区的 `WM_NCHITTEST`、显示器列表、
  `WM_CLOSE` 的往返与 5 秒放弃、`closable: false`。`app_region.rs`、`report.rs`、`lib.rs`（`file://`）有单元测试。
- 没改任何已有测试。`DomView::new` 现在带底色参数（`shell.rs` 以外没人用）。
- **验收里的 `window_lifecycle`**：P10 的 `tests/app.rs` 已覆盖“最后窗口关闭、有无 `window-all-closed` 监听”“`app.quit()` 逐个问窗口”“被拒的窗口取消退出”“`app.exit` 不问窗口”；`destroy()` 现在没有主进程
  方法（P13），只能在 `ShellPort` 层测（`contract::quiet_and_closing`：`Destroy` 只发 `Closed`，不发 `CloseRequested`）。补的是 `tests/window_lifecycle.rs`：被阻止的 `'close'` 保留窗口且不发 `'closed'`，随后放行才关；
  同一窗口连续两次用户关闭只发一次 `'closed'`。

### 手动验收清单（Windows；macOS 没有机器）

在这台机器（Windows 10 LTSC 19044，一块 1920×1080、100% 的显示器）上，我能看到、能自动化的，与留给人的：

| 项 | 状态 |
|---|---|
| `frame:false` 窗口无标题栏，拖动区返回 `HTCAPTION`、`no-drag` 按钮区与页面不返回 | **自动化通过**（`WM_MOUSEMOVE` + `WM_NCHITTEST`）。**待人**：真拖动、双击最大化、Aero Snap（需要 P30 给拖动区，夹具的 HTML 目前拖不了） |
| `backgroundColor: '#1e1e1e'` 首帧即深色 | 用 `PrintWindow` 截了 shell 开出的窗口：深灰（已看）。**待人**：肉眼看首帧无白闪 |
| `show:false` + `ready-to-show` 后显示 | `FirstPaint` 在窗口第一次可见并绘制后发一次（共用测试断言每窗口各一次）。`ready-to-show` 是 P13 的；**待人**：无白闪 |
| `transparent: true` 圆角卡片 | 只验证了选项到了 GPUI（`Transparent`）。`PrintWindow` 截 DirectComposition 窗口会丢 alpha，截图是黑的，**没有看到**；**待人** |
| `parent` + `modal` 阻止父窗口输入 | **自动化通过**（owner 窗口、`IsWindowEnabled` 假/真）。**待人**：点父窗口无反应的手感 |
| `setAlwaysOnTop`、`setBounds`、`center`、`setProgressBar(0.5)`、`flashFrame(true)` | **自动化通过**：`WS_EX_TOPMOST`、外框、居中误差 ≤ 1 px 都读回了；进度条与闪烁调用成功但**没有看到任务栏**；**待人** |
| 窗口拖到 150% DPI 的第二屏后 `devicePixelRatio === 1.5` 且字迹清晰 | **未验证**（单显示器）。`WM_DPICHANGED` 由 GPUI 处理，这里看到缩放变化后发 `ScaleFactor` + `Viewport { scale }`（差分有单元测试）；渲染器目前忽略 `scale`（P20/P30） |
| `tests/electron/apps/windows` 夹具 | 在 Windows 上跑过一遍：自己退出（约 4 秒），stderr 空。**现在的 `BrowserWindow` 构造函数只读 `width/height/show/title`**，其余选项和全部窗口方法是 P13 的，所以导览步骤都打印 “skipped”，窗口是普通的带标题栏窗口；P13 合并后需要再看一遍。`--stay` 让窗口留着手动试 |
| macOS 全部 | **未运行**。`cargo check --target x86_64-apple-darwin -p verve-gpui` 在 `cc-rs` 处失败（本机没有该目标的 C 编译器），没有绕过；`macos.rs` 在一个临时 crate 里用同样版本的 `objc2` 0.6 / `objc2-app-kit` 0.3.2 单独类型检查通过 |

### 没做 / 已知问题

- **混合 DPI（已知错误，不是“未验证”）**：屏幕坐标一律用某一个窗口或显示器自己的缩放换算。主显示器 100%、1920 像素宽，右侧副显示器 150% 时，`displays()` 把副显示器放在 x=1280 DIP，与主显示器重叠（Chromium 的 `ScreenWin` 是 1920）；`Moved` 在跨显示器时会跳；`place` 用当前显示器的 DPI 缩放。要按 `ScreenWin` 的做法，用点所在显示器的缩放换算每个点并按 DIP 重新排布显示器，还没做。
- **macOS**：`NSWindow` 的显示/隐藏、置顶、不透明度、限制、子窗口（`adopt`）、隐藏红绿灯（`hide_traffic_lights`）、`isMiniaturized` 有代码并已接线（未运行）；可见性由 Shell 应用过的 `Show/Hide` 记录（GPUI 的 `is_visible` 是遮挡判断，不用），`work_area` 取窗口所在显示器的可见区域；**窗口位置**（`setFrameOrigin` 要翻转坐标）、Dock 进度、`requestUserAttention`、`SetIcon`、`SetSkipTaskbar`、
  `SetIgnoreMouseEvents`、`SetResizable` 的运行时切换、`Blur`、最小化的 `Restore` 都没做；`application:openFiles:` 未补（GPUI 只给 URL，`file://` 当作 `OpenFile`）。
- **Linux**：只有 GPUI 原生的最小化/最大化/全屏/标题/尺寸/前置；显示隐藏、限制、置顶、不透明度、进度等是空操作。`other.rs` 里 Linux 的“最小化”恒为假（GPUI 没有这个信息），可见性由 Shell 应用过的 `Show/Hide` 记录。
  `bash scripts/linux-check.sh -p verve-gpui --all-targets` 通过（Zig 已装）。
- **Windows**：`resizable: false` 创建的无边框窗口之后 `setResizable(true)`，GPUI 的 `is_resizable` 在创建时定死，上边缘仍不能缩放（其余边可以）；焦点不保证——前台锁可能让 `Show` 后没有 `Focus`，真 Shell 的共用测试因此忽略 `Focus/Blur`（无头 Shell 严格比较）。`Maximize` 对隐藏窗口无效；`SetBounds` 的 `animate` 忽略；
  混合 DPI 的屏幕坐标换算是已知错误，见上。`titleBarOverlay`、Mica、`setShape`、缩略图按钮按计划不做。`Menu` 与托盘是 P15 的。
- **真 Shell 与无头 Shell 的差别**：真 Shell 在窗口建好后发 `NativeHandle`（无头不发）；`FirstPaint` 两边都发，是“窗口可见时的第一帧”（真 Shell 显示时强制画一帧；无头在可见窗口的第一份快照到达时），隐藏窗口的 `ready-to-show` 要靠渲染器的第一份快照，由 P13 决定；创建出的可见窗口随后系统给的 `Focus` 会作为事件报（P00 说创建不发事件）；
  销毁父窗口时无头 Shell 不带走子窗口，真 Shell 带走并各发 `Closed`（Electron 关父窗口时子窗口也关；`'close'` 是否也对子窗口发，未观察）。
- Electron 的观测：没有新的探针。共用测试按 `window-state` 的记录（Electron 39.8.10，Windows）；其中 `minimize` 之后记录里有 `blur`，是因为那个 `showInactive` 的窗口当时是活动窗口，
  而 P00 的状态机原先只在窗口有焦点时发 `Blur`（无头 Shell 与记录不一致，错在状态机），所以无头状态机改成：窗口可见时 `Minimize` 一律 `Minimized, Blur`（与记录一致）。共用测试对两种 Shell 用同一份精确列表；真 Shell 的 `Focus`/`Blur` 只断言排在每步最后、不比较有无。
- **`DomView`** 仍只画文本节点；拖动区的 `Hitbox` 层、底色、`on_draw` 钩子在它的 `render` 里，P30 换绘制时要保留。

### 给其他包的备注

- **P30**：对每个计算样式 `-webkit-app-region` 为 `drag`/`no-drag`（或 `app-region`）的元素，布局后调 `Control::set_app_regions(window, Vec<AppRegion>)`：`rect` 是**窗口内容区坐标、DIP**，**文档序**（后者覆盖前者，所以 `no-drag` 的后代要排在 `drag` 祖先之后），
  布局变化时重发整张表。macOS 上拖动区内的鼠标按下不应再派发给页面：用 `verve_gpui::drag_area(&regions)` 判断；Windows 上系统吞掉了。`DomView::render` 的 `bg(background)` 要留在最底层，`app_region::layer` 放在最上层，`on_draw` 钩子保留（`FirstPaint` 靠它）。
- **P13**：每个 `BrowserWindow` 方法发 `ToShell::Window` 即可，落点都在 `window.rs::apply`；`ready-to-show` 可用 `WindowEvent::FirstPaint`，但 `show: false` 的窗口真 Shell 要到第一次显示后才会画，所以 `ready-to-show` 仍要 P13 自己按页面首帧决定；
  `getNativeWindowHandle()` 取 `NativeHandle`（小端 8 字节）；构造选项要全部解析进 `WindowOptions`（现在只解析 4 个）；`parent`/`modal` 要把 `WindowOptions.parent` 填成父的 `WindowId`；
  **`CloseRequested` 之后必须回 `CloseAnswer`**，否则要 5 秒后用户再点一次才会关。
- **P14**：`NativeHandle` 给对话框做父窗口；显示器列表在 `FromShell::Displays`（启动、变化时），`screen` 的同步查询仍按本包契约由 P14 直接调平台 API；对话框、`ErrorBox` 加进 `shell.rs` 的 `message` 分发即可，窗口管理在 `window.rs`/`platform/`，不要写进 `shell.rs`。
- **P15**：菜单/托盘/全局快捷键是 `ToShell::Menu`；`NativeHandle` 是 `HWND`/`NSView*`；系统要求退出走 `AppEvent::QuitRequested`。
- **P32**：输入事件走 GPUI；本包没有碰；`WS_EX_NOACTIVATE` 的窗口（`focusable: false`）在 Windows 上点击不会拿到键盘焦点。
- **P41**：`SetIcon` 接受 `.ico` 与 `.png`；任务栏进度/闪烁是 Windows 的，macOS Dock 的版本空着。
