# P02 骨架、线程与消息契约

**依赖**：无（第一个做）  **规模**：M  **修改**：新建 `crates/verve-electron`、`crates/verve-gpui`；根 `Cargo.toml` 的 workspace members

## 目标

建立两个新 crate 和 `electron` 可执行文件的空架子，定下**所有包共用的线程模型与消息类型**，
跑通一条最小链路：`electron.exe main.js` → 主进程 isolate 执行 `main.js` → 它调用
`new BrowserWindow().loadURL('data:text/html,<h1>hi</h1>')` → 渲染器 isolate 解析出文档 → Shell
（GPUI）打开窗口并把 `<h1>` 的文字画出来（只需最朴素的绘制，P30 再做真）。

## 背景（先读）

- `src/isolate.rs`：`Guest` trait、`Isolate`、`Outbox`、`InterruptHandle`。所有 JS 线程都用它。
- `crates/verve-dom/src/lib.rs`：`install`、`Document`、`Snapshot`、`Update`、`Request`、输入方法、`set_layout`、`set_layout_provider`（P30）、`set_range_provider`、`Layout.fragments`、`RangeProvider`、`Boundary`（P31）。
- `crates/verve-dom/tests/isolate.rs`：文档放在 isolate 里、另一线程读快照的现成例子。
- `src/cli.rs`：`verve run` 的事件循环（什么时候退出、怎样睡到下一个到期点）。
- 参考（只读）：`F:\dev\sandbox-v2\su-editor\crates\su-shell\src\editor\obsidian\view.rs`——su-editor 把计算样式映射成
  GPUI `Div` 的做法。**本项目不沿用这种做法**（布局在渲染器线程，见 P30），只参考它的样式值解析（颜色、长度）。

## crate 划分

```
crates/verve-electron   (lib；没有 bin，不依赖 GPUI)
  src/lib.rs            pub mod 声明、ElectronOptions
  src/ids.rs            WindowId / ContentsId / 计数器
  src/msg/              本文定义的全部消息类型（唯一来源）；按领域分文件：shell、window、dialog、page、input、preferences，`msg::X` 的路径不变
  src/hub/              Hub：mod.rs（结构、启动、关闭）、renderers.rs（页面的 isolate 与代数检查）、windows.rs、shell.rs（含主题）、records.rs；ShellPort
  src/main_guest.rs     MainGuest: isolate::Guest（主进程）
  src/renderer_guest.rs RendererGuest: isolate::Guest（渲染器）
  src/api/mod.rs        require('electron') 的两种形态：main / renderer
  src/api/define.rs     Electron 在 `verve::native` 之上加的那一点：对象 ↔ id、已销毁检查、参数/选项校验（类本身用 `native::class`）
  src/api/*.rs          各模块（后续包填充），本包只放 BrowserWindow 的最小版
  src/headless.rs       HeadlessShell（P00 扩充）
  依赖：verve、verve-dom、boa_engine、boa_gc（std 的 mpsc 够用，不加别的 channel 库）
  **不依赖 GPUI**（这样无头测试可以不编 GPUI）

crates/verve-gpui       (lib + bin "electron")
  src/lib.rs            run(hub, opts) -> i32：启动 GPUI Application，主循环，答案退出码
  src/shell.rs          处理 ToShell 消息：开关窗口等
  src/dom_view.rs       DomView：Snapshot → GPUI 元素（本包只画文本节点）
  src/bin/electron.rs   入口：解析参数 → verve_gpui::run(hub, opts)
  依赖：gpui、gpui_platform、async-channel（把别的线程的唤醒送进 GPUI 前台）、
        verve-electron（只用 msg/hub 类型）、verve-dom（只用 Snapshot 类型）
```

**已定**：`electron` 二进制放在 `verve-gpui`（`crates/verve-gpui/src/bin/electron.rs`），
`verve-electron` 保持纯逻辑库——没有 bin，也不提供 `run()`：启动是 `Hub::new` + `Hub::start_main`，
主循环在 `verve_gpui::run`，退出码由 `Hub::exit_code()` 给出。无头验收（P00）自己装 `HeadlessShell`
并驱动同一个 `Hub`。

## 线程模型

| 线程 | 跑什么 | 栈 |
|---|---|---|
| OS 主线程 | GPUI `Application`；Shell；DomView；muda/tray（P15） | 系统默认 |
| `electron-main` | `Isolate<MainGuest>` | 16 MiB 起（`verve::set_thread_stack` 同款） |
| `renderer-<ContentsId>` | `Isolate<RendererGuest>`，每个 webContents 一个 | 16 MiB 起 |
| Verve 已有的网络 reactor、DNS 池、Worker | 不变 | |

**同步规则**：
1. 主进程线程不得阻塞等待任何其他线程的回复。
2. Shell 线程不得阻塞等待 JS 线程。需要 JS 答复的（如 `close` 事件可否阻止），走"异步询问 + 回复消息"。
3. 渲染器线程可以阻塞等主进程（仅 `ipcRenderer.sendSync`），用一次性通道，带超时（默认无超时，
   与 Electron 一致；但主进程 isolate 退出时必须唤醒并抛错）。

**规则 1 的例外清单**（只有这些 Electron 本身就是同步的 API，允许主进程线程阻塞等 Shell；Shell 自己绝不阻塞，
且 Shell 处理这些请求时不得反过来等主进程）：

| API | 包 | 说明 |
|---|---|---|
| `dialog.show*Sync` | P14 | 对话框在 OS 主线程弹出，主进程线程在 `ToShell::Dialog` 带去的一次性通道（`DialogReply::Sync`）上等结果；Shell 在 GPUI 的 `cx.spawn` 里等对话框的 future，不阻塞消息循环；父窗口已销毁或 Shell 在退出时发 `DialogResult::Closed`，主进程当作取消；阻塞期间到达的 `FromShell`/`FromRenderer` 照常排队，对话框返回后处理 |
| `globalShortcut.register` / `registerAll` | P15 | 主进程在 `ToShell::Menu(MenuOp::ShortcutRegister { reply })` 带去的一次性通道上等 Shell 注册结果（Shell 只做一次系统调用就回答，不等主进程），500 ms 超时后按失败处理并警告；`isRegistered` 由主进程自己的记录回答，不经过 Shell（Electron 39.8.10 观察：`isRegistered` 在 `ready` 之前也不抛错，返回 `false`） |
| `clipboard.*`（若走 Shell） | P14 | 不走 Shell：主进程线程直接调用系统剪贴板（Windows 用 `windows-sys`），所以不在此例外内 |

新增例外必须先改这张表。

## 消息类型（`crates/verve-electron/src/msg/`）

这是契约，字段名和语义按此实现；需要增加变体时**只能追加**，并更新本文。

```rust
pub type WindowId = u32;    // BrowserWindow.id，从 1 开始，与 Electron 一致
pub type ContentsId = u32;  // webContents.id，从 1 开始

/// 跨线程传递的 JS 值：Verve 的结构化克隆（P03 把 worker.rs 的 Cloned 公开为 verve::clone::Cloned）。
pub use verve::clone::Cloned;

// ---------- 主进程 → Shell ----------
pub enum ToShell {
    CreateWindow { window: WindowId, contents: ContentsId, options: WindowOptions },
    Window { window: WindowId, op: WindowOp },
    /// 应用菜单、上下文菜单、托盘、全局快捷键：P15 定义 MenuSpec 等，先占位。
    Menu(MenuOp),
    /// 渲染器有新快照了（Hub 收到 renderer 的 view 后转发，或 Shell 直接订阅 isolate view）。
    Quit { exit_code: i32 },
    /// `dialog.showErrorBox`：主进程未捕获的错误弹的框（P10 加；P14 画，之前 Shell 只记录）。
    ErrorBox { title: String, content: String },
    /// P14：对话框。`call` 对应 `FromShell::DialogResult`；`reply` 为 `Sync` 时答复写进它的通道。
    Dialog { call: u64, parent: Option<WindowId>, spec: DialogSpec, reply: DialogReply },
    /// P14：`nativeTheme.themeSource`，让 Shell 切换窗口的亮暗标题栏。
    SetThemeSource(ThemeSource),
    // —— P43 追加（第二阶段）——
    /// `openDevTools()`（`open: true`）/ `closeDevTools()`：Shell 为 `contents` 开或关检查器窗口。已开着再开、
    /// 已关再关都不做事；无头 Shell 只记下它（文本转储仍由页面写）
    Inspector { contents: ContentsId, open: bool },
    /// Hub 把渲染器的 `InspectorEvalReply` 转给检查器：窗口按 `call` 找回它问的那一行；检查器已关则丢弃
    InspectorReply { contents: ContentsId, call: u64, text: String, is_error: bool },
    // —— P24 追加（第二阶段）——
    /// 页面复制了：剪贴板归 Shell，由它改持这些内容；`custom` 是其余格式（`image/png` 等，按 MIME 类型）
    WriteClipboard { text: Option<String>, html: Option<String>, custom: Vec<(String, Vec<u8>)> },
    /// `contents` 的页面读剪贴板：Shell 回 `ToRendererInput::ClipboardContents { call, data }`
    ReadClipboard { contents: ContentsId, call: u64 },
}

/// P14：`DialogSpec::{Open, Save, MessageBox}`、`DialogReply::{Async, Sync(SyncSender<DialogResult>)}`、
/// `DialogResult::{Open{canceled, file_paths}, Save{canceled, file_path}, MessageBox{response, checkbox_checked}, Closed}`、
/// `ThemeSource::{System, Light, Dark}`。字段以 `crates/verve-electron/src/msg/` 为准，与 P14 文档第 2 节的草稿不同处：
/// 路径一律 `String`（`default_path`；`DialogResult` 里的 `file_paths: Vec<String>`、`file_path: Option<String>`），不是 `Option<PathBuf>`；
/// `MessageBox.default_id: i32`（`-1` 为没指定）；没有 `bookmarks` 和 `normalize_access_keys`（按钮文字在 `api/dialog.rs` 里已按平台处理）；
/// 没有 `DialogSpec::ErrorBox`（用 `ToShell::ErrorBox`）；没有 `ToShell::Notification` 和 `FromShell::Theme`/`ThemeState`。

pub struct WindowOptions {
    pub width: f32, pub height: f32,            // 默认 800 x 600
    pub x: Option<f32>, pub y: Option<f32>,     // None = 居中
    pub min_size: Option<(f32, f32)>, pub max_size: Option<(f32, f32)>,
    pub resizable: bool, pub movable: bool, pub minimizable: bool, pub maximizable: bool,
    pub closable: bool, pub focusable: bool, pub always_on_top: bool, pub fullscreen: bool,
    pub show: bool,                              // false → 等 show()/ready-to-show
    pub frame: bool,                             // false → 无边框，-webkit-app-region 拖动（P34）
    pub title_bar_style: TitleBarStyle,          // Default | Hidden | HiddenInset
    pub transparent: bool,
    pub background_color: Option<[u8; 4]>,       // '#RRGGBB' / '#AARRGGBB' 解析后
    pub title: String,
    pub icon: Option<std::path::PathBuf>,
    pub parent: Option<WindowId>, pub modal: bool,
    pub skip_taskbar: bool, pub kiosk: bool,
}

pub enum WindowOp {
    Show, ShowInactive, Hide, Close /* 可被 close 事件阻止 */, Destroy, Focus, Blur,
    Minimize, Maximize, Unmaximize, Restore, SetFullScreen(bool),
    SetBounds { x: Option<f32>, y: Option<f32>, width: Option<f32>, height: Option<f32>, animate: bool },
    SetMinSize(f32, f32), SetMaxSize(f32, f32), Center, SetTitle(String),
    SetResizable(bool), SetAlwaysOnTop(bool), SetBackgroundColor([u8; 4]),
    SetProgressBar(f64), SetIcon(std::path::PathBuf), FlashFrame(bool),
    SetOpacity(f64), SetSkipTaskbar(bool), SetIgnoreMouseEvents(bool),
    /// close 事件 JS 侧的答复：true = 已阻止
    CloseAnswer { prevented: bool },
    // —— P13 追加 ——
    SetClosable(bool), SetMovable(bool), SetMinimizable(bool), SetMaximizable(bool), SetFocusable(bool),
    /// false = 窗口不收输入（模态子窗口显示时的父窗口）
    SetEnabled(bool),
    SetParent(Option<WindowId>),
    /// 置于其他窗口之上，不取焦点
    MoveTop,
}

// ---------- Shell → 主进程 ----------
pub enum FromShell {
    Window { window: WindowId, event: WindowEvent },
    App(AppEvent),
    MenuClicked { menu_item: u32, window: Option<WindowId> },     // P15
    Displays(Vec<Display>),                                       // screen 模块缓存（P14）
    DialogResult { call: u64, result: DialogResult },             // P14：异步对话框的答复
    /// P13（第二段）：`before-input-event`——页面的 `intercept_keys` 为真（主进程有监听者）时，Shell 把键盘事件发到这里而不是直接发渲染器；主进程发事件，未阻止就用 `Hub::input` 转交
    KeyForContents { contents: ContentsId, down: bool, key: verve_dom::Key },
    /// P43（第二阶段）：检查器窗口的事 —— 用户关了它（`Closed`：主进程发 `'devtools-closed'`）、它得到或失去焦点
    /// （`'devtools-focused'`、`isDevToolsFocused()`）
    Inspector { contents: ContentsId, event: InspectorEvent },
}

/// P43（第二阶段）：检查器窗口自己发生的事。
pub enum InspectorEvent { Closed, Focused, Blurred }

pub enum WindowEvent {
    Shown, Hidden, Focus, Blur,
    /// 用户点了关闭：主进程发 'close'，再回 WindowOp::CloseAnswer
    CloseRequested,
    Closed,
    Resized { width: f32, height: f32 }, Moved { x: f32, y: f32 },
    Maximized, Unmaximized, Minimized, Restored,
    EnterFullScreen, LeaveFullScreen,
    ScaleFactor(f32),
}

pub enum AppEvent {
    /// Shell 已就绪、入口已跑完：`app` 的 `ready`（P10 加；Hub 在 `start` 时若已有 Shell 就发）。
    Ready,
    /// macOS 点 Dock 图标；Windows 无。
    Activate { has_visible_windows: bool },
    OpenFile(std::path::PathBuf), OpenUrl(String),
    ThemeChanged { dark: bool },
    /// 第二个实例启动（P10 的单实例锁）
    /// `data` 是第二个实例的 `additionalData` 的 JSON 文本（它跨了进程，P10 加）。
    SecondInstance { argv: Vec<String>, cwd: std::path::PathBuf, data: String },
}

pub struct Display { pub id: u32, pub bounds: Rect, pub work_area: Rect, pub scale_factor: f32, pub primary: bool }
pub struct Rect { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }

// ---------- 主进程 → 渲染器 ----------
pub enum ToRenderer {
    Load { url: String, options: LoadOptions },      // loadURL / loadFile / reload
    Ipc { channel: String, args: Vec<Cloned>, sender_id: ContentsId /* 0 = 主进程 */ },
    InvokeReply { call: u64, result: Result<Cloned, Cloned /* Error 克隆 */> },
    SendSyncReply { call: u64, value: Cloned },      // 也可直接走 oneshot，见 P12
    /// `world`: 0 = the page's own world, else `executeJavaScriptInIsolatedWorld`'s number (P13 第二段加）
    ExecuteJavaScript { call: u64, code: String, user_gesture: bool, world: i32 },
    SetZoomFactor(f64),
    // —— P13 追加（关闭往返）——
    /// 窗口要关了：在页面 window 上派发 `beforeunload`，答 `FromRenderer::BeforeUnloadReply`
    BeforeUnload { call: u64 },
    /// 关闭继续：派发 `pagehide`、`unload`，答 `FromRenderer::UnloadReply`
    Unload { call: u64 },
    // —— P13 追加（第二段）——
    /// `webContents.insertCSS`：键由主进程取好（`insertCSS` 的 Promise 以它 resolve），`user` 是 `cssOrigin: 'user'`
    InsertCss { key: String, css: String, user: bool },
    /// `webContents.removeInsertedCSS(key)`
    RemoveCss { key: String },
    /// `webContents` 的编辑命令（`undo`、`copy`、`selectAll`、`insertText`…；菜单 role 的 `EditCommand` 也是它，P15 复用）
    EditCommand(EditCommand),
    /// 页面自己控制台的一行错误（`window.open` 被拒时 Chromium 写在打开者的控制台里）
    ConsoleError { message: String },
    // —— P43 追加（第一阶段）——
    /// `webContents.openDevTools()` / `inspectElement(x, y)`：渲染器把元素树（`at` 为 `None`）或 `at` 处元素的一行
    /// 写到终端（经 `Hub` 的日志输出），不回消息
    InspectTree { at: Option<(f32, f32)> },
    // —— P43 追加（第二阶段）——
    /// 检查器的 REPL：页面在**主世界**（`contextIsolation` 下不是 preload 的隔离世界）把 `code` 当脚本求值，
    /// 结果用 Verve 的 `util.inspect` 格式化成文本，答 `FromRenderer::InspectorEvalReply`；求值不写页面的 console，
    /// 不触发 `'console-message'`。`call` 由 Hub 的一个计数器编号（`Hub::next_inspector_call`），检查器窗口先后开关也不会重号
    InspectorEval { call: u64, code: String },
}

// ---------- 渲染器 → 主进程 ----------
pub enum FromRenderer {
    Ipc { channel: String, args: Vec<Cloned> },
    Invoke { call: u64, channel: String, args: Vec<Cloned> },
    SendSync { channel: String, args: Vec<Cloned>, reply: SyncReply },
    /// ipcRenderer.sendTo / sendToHost
    SendTo { target: ContentsId, channel: String, args: Vec<Cloned> },
    ExecuteJavaScriptReply { call: u64, result: Result<Cloned, Cloned> },
    Lifecycle(PageEvent),
    /// verve-dom 的 Request（Navigate/Open/Title/Alert/CopyText/Fullscreen…）
    Dom(verve_dom::Request),
    /// 页面自己重载（`location.reload()`）：主进程当作一次 `reload`，由 P13 接成 webContents 的事件
    Reload { url: String },
    Console { level: verve::Level, message: String, line: u32, source: String },
    Gone { reason: GoneReason },                     // 'render-process-gone'
    // —— P13 追加（关闭往返）——
    /// `proceed = false`：处理器调了 `preventDefault()` 或把 `returnValue` 设成了非空字符串
    BeforeUnloadReply { call: u64, proceed: bool },
    /// `pagehide`、`unload` 已派发
    UnloadReply { call: u64 },
    /// P21：preload 抛错（或读不到）：主进程在 webContents 上发 `preload-error(event, path, error)`
    PreloadError { path: String, name: String, message: String },
    /// P21：`webFrame.setZoomFactor`：页面自己改了缩放系数（主进程不记；`getZoomFactor`/`zoom-changed` 是 P13 的，要用自己留）
    ZoomChanged(f64),
    // —— P13 追加（第二段）——
    /// 页面的 `contextmenu` 事件未被 `preventDefault`（右键松开）：主进程发 `webContents` 的 `context-menu`
    ContextMenu(ContextMenuParams),
    // —— P43 追加（第二阶段）——
    /// `ToRenderer::InspectorEval { call }` 的答复：`text` 是求值结果的 `util.inspect` 文本，或抛出的错误的 inspect 文本
    /// （`is_error` 为 `true`）；从不带活对象。Hub 收到后**不交给主进程**，转给检查器所在的 Shell
    /// （`ToShell::InspectorReply`）；没有开着的检查器时丢弃
    InspectorEvalReply { call: u64, text: String, is_error: bool },
}

pub enum PageEvent {
    DidStartLoading, DomReady, DidFinishLoad,
    DidFailLoad { code: i32, description: String, url: String },
    DidNavigate { url: String }, DidNavigateInPage { url: String },
    TitleUpdated(String), FaviconUpdated(Vec<String>),
    /// 首次有可绘制内容（ready-to-show 的依据之一，P13 决定精确条件）
    FirstPaintable,
}

// ---------- Shell ↔ 渲染器（绕过主进程，热路径） ----------
pub enum ToRendererInput {
    Viewport { width: f32, height: f32, scale: f32 },
    ColorScheme { dark: bool },
    /// 已废弃：布局改在渲染器线程由 verve-layout 完成（见 P30），Shell 不再回报布局。保留变体以免破坏编号。
    Layout(Vec<(verve_dom::NodeId, verve_dom::Rect)>),
    ScrollOffsets(Vec<(verve_dom::NodeId, [f32; 2])>),
    Frame { now: f64 },
    Pointer { kind: PointerKind, pointer: verve_dom::Pointer, target: Option<verve_dom::NodeId> },
    Wheel { pointer: verve_dom::Pointer, dx: f32, dy: f32, target: Option<verve_dom::NodeId> },
    Key { down: bool, key: verve_dom::Key },
    Text(String),                                        // 输入法提交 / 粘贴
    Composition(CompositionEvent),                       // P24
    Focus(bool),                                         // 窗口获得/失去焦点
    ClipboardContents { call: u64, data: verve_dom::ClipboardData }, // Shell 对 ToShell::ReadClipboard 的回答（P24）
    Drag(DragInput),                                     // 从系统拖入的文件/文本（P24；P34 从 GPUI 转换）
}

/// 渲染器 isolate 的 View（`Guest::View`）：Shell 读这个来画。
pub struct RendererView {
    pub applied: u64,
    pub update: Option<verve_dom::Update>,     // None = 文档没变（调试/检查器用；绘制不读它）
    /// 绘制用：verve-layout 生成的按层叠顺序排好的绘制项（P30）。None = 与上次相同。
    pub display_list: Option<Arc<verve_layout::DisplayList>>,
    pub images: Arc<ImageTable>,               // 解码好的位图（P35）
    pub wants_frame: bool,
    pub cursor: Option<String>,                // 指针下元素的 CSS cursor
    pub ime_rect: Option<verve_dom::Rect>,     // 输入法候选框位置（P24/P32）
    pub drop_effect: Option<verve_dom::DropEffect>, // 从系统拖入时，此刻放下会怎样（Shell 画光标；P24）
}
```

P13 第二段另加两个类型（`msg/page.rs`）：`EditCommand`（`Undo` `Redo` `Cut` `Copy` `Paste` `PasteAndMatchStyle` `Delete` `SelectAll` `Unselect` `ScrollToTop` `ScrollToBottom` `InsertText(String)`）与 `ContextMenuParams`（`x` `y` `link_url` `link_text` `src_url` `media_type` `is_editable` `selection_text` `title_text` `alt_text`；其余参数由主进程按页面地址与这些字段补全）。

`MenuOp`、`CompositionEvent`、`GoneReason`、`LoadOptions`、`PointerKind`、`TitleBarStyle` 在本包给出最小定义，
后续包（P15/P24/P13/P20/P32/P34）在其上**追加**。

本包落地时确认的一处删减：

- `RendererView` 的 `display_list`（`verve_layout::DisplayList`，P30）与 `images`（`ImageTable`，P35）在 P02 时
  两个 crate 与类型都还不存在，本包不定义这两个字段，由 P30/P35 追加（与下表「已登记的追加」一致）。
  P02 的 `DomView` 按文档顺序画 `update.snapshot` 里的文本节点。

`Cloned` 就是 `verve::clone::Cloned`（`pub use`），它是 `Clone + Debug`，所以 `ToShell: Clone`（P00 要求）成立。

已登记的追加（各包实现时加入，本表是汇总）：

| 变体 | 包 |
|---|---|
| `FromShell::MenuClicked` 增加字段 `triggered_by_accelerator: bool` 与 `modifiers: Modifiers`（占位变体，允许 P15 改字段；`modifiers` 是点击事件 `shiftKey`/`ctrlKey`/`altKey`/`metaKey` 的来源） | P15 |
| `ToRenderer::EditCommand(EditCommand)`（菜单 role：copy/paste/undo/selectAll…）——P13 已经追加，P15 直接复用，不再改 | P13（P15 复用） |
| `MenuOp` 的全部变体（`Upsert`、`Drop`、`SetApplicationMenu`、`SetWindowMenu`、`SetMenuBarVisibility`、`Popup`、`ClosePopup`、`Tray*`、`Shortcut*`）与 `msg/menu.rs` 的 `MenuSpec`/`MenuItemSpec`/`TrayOp`/`TrayEvent`；`FromShell::{MenuWillShow, MenuWillClose, Tray, Shortcut, TrayBounds}`（详见 P15「接口契约」，实现与那里一致） | P15 |
| `ToRenderer::SetProxy(…)` | P16 |
| `ToRenderer::InspectorEval { call, code }`、`FromRenderer::InspectorEvalReply { call, text, is_error }`（答复的是文本，不是 `result`）、`ToShell::Inspector { contents, open }`、`ToShell::InspectorReply { contents, call, text, is_error }`（Hub 把答复转给 Shell）、`FromShell::Inspector { contents, event }` 与 `InspectorEvent::{Closed, Focused, Blurred}`（第二阶段） | P43 |
| Shell 提供 `native_handle(WindowId) -> Option<RawWindowHandle>`（对话框父窗口、muda 挂菜单） | P34（P14/P15 使用） |
| `AppEvent::Ready`（Shell 启动完成，`app` 的 `ready` 依据）— P10 已加，由 Hub 在 `start` 时发 | P10 |
| `WindowEvent::FirstPaint`（`ready-to-show` 的精确依据）、`WindowEvent::NativeHandle(u64)` | P13 / P34 |
| `ToRenderer::{BeforeUnload, Unload}`（各带 `call`）、`FromRenderer::{BeforeUnloadReply { call, proceed }, UnloadReply { call }}`；`WindowOp::{SetClosable, SetMovable, SetMinimizable, SetMaximizable, SetFocusable, SetEnabled, SetParent, MoveTop}` | P13（第一阶段） |
| `ToRenderer::WindowMessage`、`FromRenderer::WindowProxyOp`（`window.open` 返回的代理）——**P13 第二段没做**：渲染器侧的代理是 P20 的，没有它主进程这一侧无从验证 | P13 |
| `ToRenderer::{InsertCss, RemoveCss, EditCommand, ConsoleError}`、`ExecuteJavaScript.world`、`FromRenderer::ContextMenu`、`FromShell::KeyForContents`，及 `EditCommand`、`ContextMenuParams` | P13（第二段） |
| `FromShell::Page { contents, event: PageEvent }`（Shell 看到而渲染器看不到的：光标、指针下的链接）、`PageEvent::{CursorChanged(String), UpdateTargetUrl(String), ZoomChanged { zoom_in }}`、`PointerKind::Leave`、`verve_dom::Pointer.buttons`、`FromRenderer::KeyUnhandled(Key)`（页面没处理的 key down，主进程据此查菜单加速键）；`RendererView.cursor` 不再使用（光标由 Shell 从 `Hitbox.cursor` 取得） | P32 |
| `ToRenderer::InspectTree { at }`（`openDevTools` 的文本转储与 `inspectElement`；第二阶段的 `InspectorEval` 不在其中） | P43 |
| `FromShell::KeyForContents`（`before-input-event`，Shell 不等 JS：`Hub::intercepts_keys(contents)` 为真时先投递给主进程，主进程决定后再转渲染器；上一行也列了它） | P13 / P32 |
| `ToRendererInput::Visibility(bool)`（`document.visibilityState`） | P13 / P34 |
| ~~`FromRenderer::Font`、`ToRendererInput::FontsChanged`~~（第二阶段作废：渲染器线程直接 `add_fonts`，见 P31 的 `@font-face` 通道；只剩 P30 契约的 `PlacedGlyph.font`：画这个字形的字体，用了回退时与请求的不同） | P31 |
| `ToRendererInput::Choose { select, value }`（`<select>` 下拉弹层选中） | P32 |
| `RendererView.display_list`（`Option<Arc<verve_layout::DisplayList>>`，随 `verve-layout` 接入渲染器时加）；`DisplayList.rasters`、`DisplayList.root`（页面的滚动容器）与进程内唯一的 `revision`、`Hitbox` 的形状、滚动帧 `PushScroll`/`PopScroll`（滚动只改 `Scroller` 的 offset）见 P30 的「契约调整」 | P30 |
| `RendererView.images`（`Arc<verve_layout::ImageTable>`，键是 `ImageKey`，值是 straight-alpha BGRA 的 `ImageFrames`）、`PaintItem::Image.frame`、`Fill::Picture`、`Snapshot.image_sources`/`base_url`（P35，细节见 P35 的「接口契约」）；`DisplayList.vectors`、`DisplayList::same_but_frames`、`Document::commit_canvases`（pub）、`ImageFrames.loops`、`Layout.svg_boxes`、canvas 的键（P35 第二阶段，见 P35 的「第二阶段的契约」）；`DisplayList.chunks`（P33） | P35 / P33 |
| `FromRenderer::{Protocol, ProtocolPull, ProtocolCancel}`、`ToRenderer::{ProtocolHead, ProtocolChunk, ProtocolEnd}`（页面 `fetch` 自定义 scheme，逐块拉取；见 P16 契约修订） | P16 |
| `FromRenderer::{ProtocolRequest, ZoomChanged, PreloadError}`、`ToRenderer::ProtocolResponse` | P20 / P21 |
| `ToRendererInput::{Drag(DragInput), ClipboardContents { call, data }}`、`DragInput::{Enter { data, pointer }, Over { pointer }, Leave, Drop { pointer }}`、`ToShell::{WriteClipboard { text, html, custom }, ReadClipboard { contents, call }}`、`RendererView.drop_effect`（`ExternalDrag`、`ClipboardData`、`DropEffect` 在 verve-dom）；Ctrl/Cmd+V 是普通按键（粘贴是它 keydown 的默认动作，经 `ReadClipboard`/`ClipboardContents`），没有 `Paste` 消息 | P24 |
| `CompositionEvent` 具体化为 `Update { text, selection } \| Commit \| Cancel` | P24 |
| `ToRenderer::Post { channel, message, ports }`、`FromRenderer::Post { channel, message, ports }`（`ipcRenderer.postMessage` / `webContents.postMessage`：带端口的消息；端口是 `verve::messaging::PortEnd`） | P12 |
| `ToShell::Dialog { call, parent, spec, reply }`、`ToShell::SetThemeSource(ThemeSource)`、`FromShell::DialogResult { call, result }`，及 `DialogSpec`/`DialogReply`/`DialogResult`/`ThemeSource`（`msg/dialog.rs`，`ThemeSource` 在 `msg/shell.rs`，字段以它为准；`DialogReply::Sync` 带 `SyncSender<DialogResult>`）。`FromShell::Displays` 由 Hub 存下（`Hub::displays()`），主进程按需读取，所以启动时先于主进程发出的不会丢 | P14 |
| 内核：`verve::isolate::Isolate::poker`（唤醒 isolate 线程）、`ClassSpec::new_only`、`verve::events::{emitter_class, emitter_init}` 公开 | P02 |

verve-dom 侧的配套改动（由 P20/P23/P24 在 verve-dom 内完成）：`Request::Open(String)` → `Open { url, name, features }`；
`CopyText` → `WriteClipboard { text, html, custom }`；新增 `ReadClipboard { call }`；`ScrollRequest::Reveal` 增加 `block`/`inline`/`smooth`；
`SnapshotData` 增加 `pseudo`（P22）、`canvases`/`images`（P25）、`fonts`（P20）、`selection`/`caret`（P24），以及平铺树遍历
`Snapshot::flat_children`/`flat_parent`（P23）——**P30 的布局必须遍历平铺树**，否则 shadow DOM 内容画不出来。

滚动归渲染器所有：渲染器自己执行 `verve_dom::Request::Scroll`（不再转给 Shell）；Shell 对滚轮做即时视觉滚动，
随后以渲染器回报的偏移为准（P30/P33）。

修正：
- `FromRenderer::SendTo`：Electron 28 起已移除 `ipcRenderer.sendTo`，此变体保留给 `sendToHost`/内部用途，`ipcRenderer` 不再产生它。
- 文中 `oneshot::Sender` 是 `SyncReply`（`msg/page.rs`）：包着容量 1 的 `std::sync::mpsc::SyncSender<Cloned>`，
  只有 `send(self, value)`——消耗自己、用 `try_send`，所以"只答一次、主进程永不阻塞"由类型保证；
  不答就丢掉它，等着的页面会因为发送端被丢而被放开：主进程已在退出则抛 `Main process exited`，否则得到 `{ error: 'reply was never sent' }`（Electron 事件被回收时的答复，P12 观测）。
- `ToRenderer::Stop` 已删：P02 的加载是同步的，没有可停的东西；`webContents.stop()` 由 P13/P20 追加。
- `WebPreferences`（`msg/preferences.rs`）P02 给出能解析的子集（`devTools`/`nodeIntegration*`/`preload`/`sandbox`/`partition`/
  `zoomFactor`/`javascript`/`webSecurity`/`allowRunningInsecureContent`/`images`，默认值同 Electron），P13 补全（第一阶段：`api/web_preferences.rs` 读 P13 文档表里的全部字段，除 `additionalArguments`，那是 P21 的）；
  P21 加 `additional_arguments: Vec<String>`（`additionalArguments`，追加到渲染器 `process.argv`）；
  `Hub::create_renderer(contents, prefs)` 把它存在页面的槽里（`Hub::preferences_of`），P20 在导航时用同一份重建 isolate。

## Hub（`hub/`）

```rust
pub struct Hub { /* Arc 内部 */ }
impl Hub {
    pub fn new(opts: ElectronOptions) -> Arc<Hub>;
    pub fn start_main(&self, entry: PathBuf);                          // 起 electron-main isolate
    pub fn create_renderer(&self, contents: ContentsId, prefs: WebPreferences) -> RendererHandle;
    pub fn to_shell(&self, msg: ToShell);                              // 主进程用
    pub fn from_shell(&self, msg: FromShell);                          // Shell 用
    pub fn to_renderer(&self, id: ContentsId, msg: ToRenderer);
    pub fn input(&self, id: ContentsId, msg: ToRendererInput);         // Shell 用，热路径
    pub fn renderer_view(&self, id: ContentsId) -> Option<RendererView>; // Shell 每帧取（取走；下次没有新的就是 None）
    pub fn has_renderer(&self, id: ContentsId) -> bool;                // 页面有可问的 isolate（没建、已退役或已崩溃的没有）；关闭往返问之前看
    pub fn set_displays(&self, displays: Vec<Display>);                // 外壳报的显示器；`FromShell::Displays` 到时 Hub 自己记
    pub fn primary_display(&self) -> Display;                          // 新窗口落在哪块屏；外壳没报过就是 `Display::default()`（1920×1080，1x）
    pub fn renderer_busy_for(&self, id: ContentsId) -> Option<Duration>; // 页面正在跑的这一段脚本跑了多久（心跳读它；P13）
    pub fn kill_renderer(self: &Arc<Self>, id: ContentsId);           // `forcefullyCrashRenderer`：丢掉 isolate，主进程收到 `Gone { Crashed }`，下次加载重建（P13）
    pub fn set_intercept_keys(&self, id: ContentsId, on: bool);        // 主进程有 `before-input-event` 监听者时为真（P13）
    pub fn intercepts_keys(&self, id: ContentsId) -> bool;            // Shell 据此把键发给 `FromShell::KeyForContents` 而不是直接 `input`（P13 / P32）
    pub fn set_shell(&self, shell: Arc<dyn ShellPort>);                // ToShell 的收件人；它也就是唤醒的入口
    pub fn shutdown(&self, exit_code: i32);                            // 先到的退出码为准；停掉所有 isolate，再发 Quit
    pub fn exit_code(&self) -> Option<i32>;                            // 进程该用的退出码
    pub fn options(&self) -> &ElectronOptions;
}
```

- 主进程 Guest 收到 `FromShell`/`FromRenderer` 后在自己线程上分发成 JS 事件。
- 窗口与 contents 的对应（谁是谁的、还活着没有）只有 `Hub` 记：`add_window(window, contents)`、
  `contents_of(window)`、`window_of(contents)`、`windows()`（给退出规则数窗口）。`WindowId`/`ContentsId`
  在同一个 `Hub` 里各自从 1 开始；`BrowserWindow` 对象 ↔ `WindowId` 的映射在 API 层（`define.rs` 的实例数据）。
- `set_shell` 是外壳唯一的登记口：每条 `ToShell` 走 `ShellPort::send`，随后的通知走 `ShellPort::wake`——
  同一个对象负责两件事，外壳怎么把唤醒送进自己的线程是它自己的事（GPUI 侧是往一条通道里发一个空消息，
  由 OS 线程上的任务取走）。没有第二个 waker：两条唤醒路径会让人不知道哪条是正路。
- 渲染器的 `FromRenderer::Lifecycle`/`Dom`/`Console` 先到 Hub，由 Hub 转给主进程（主进程上的
  `webContents` 对象发事件）。`Dom(Request::Title)` 要让 Shell 更新窗口标题，但这一步在 `MainGuest` 里做
  （它转成 `ToShell::Window{SetTitle}`）而不是 Hub 拷贝：P13 的 `'page-title-updated'` 可以被 `preventDefault`，
  标题得由主进程的 JS 决定后才发给 Shell。Guest 仍然不直接认识 Shell——它只通过 Hub 说话。
- **线程的生与死**：`start_main` 先造 isolate，再把"设 waker"和"跑入口脚本"依次作为它的前两个 job 发出，
  所以脚本运行时别的线程发来的消息排在后面而不是丢失，脚本死循环也有 interrupt 句柄可用。
  `shutdown` 打断主进程正在跑的脚本、丢掉主进程与所有页面的 `Isolate` 句柄（线程随之结束，也就放开了它们手里的
  `Arc<Hub>`），再发 `ToShell::Quit`。isolate 死了（panic）：主进程 → 打印并 `shutdown(1)`；页面 →
  主进程收到一次 `FromRenderer::Gone { Crashed }`。
- **唤醒**：两种 Guest 都把 `Isolate::poker()` 交给 `Runtime::set_waker`，网络/worker 的完成会立即叫醒 isolate，
  不必等 500 ms 的空闲上限（内核改动：`verve::isolate::Isolate::poker`）。
- 无头模式：`Hub` 的 Shell 端由 P00 的 `HeadlessShell` 实现（同一套消息），用来在测试里断言。
  所以把"Shell 端"抽象成 trait：

```rust
pub trait ShellPort: Send + Sync {
    fn send(&self, msg: ToShell);
    fn wake(&self);
}
```

## 主进程 Guest / 渲染器 Guest

- `MainGuest::new(hub, entry)`：建 `verve::Runtime`（只建，不跑脚本；`MainGuest::run(entry)` 是 Hub 发的第一个
  job）；`process.type = 'browser'`；`define_builtin_lazy("electron", api::main_module)`；`tick` 里依次：处理入站消息队列 → `run_due` →
  `run_workers` → 微任务。`next_due` 合并 Runtime 的定时器和入站消息。
- `RendererGuest::new(hub, contents, prefs)`：建 Runtime + `verve_dom::install(Options{ frames: FrameClock::Host, .. })`；
  `process.type = 'renderer'`；`define_builtin_lazy("electron", api::renderer_module)`；`publish` 返回
  `RendererView`（只在 `Document::update()` 有变化或 `ran` 时）。
- 退出条件（与 Electron 一致）：`app.quit()` 或最后一个窗口关闭且无 `window-all-closed` 监听者时
  退出（P10 细化）。本包只需：主进程脚本结束且没有窗口、没有定时器 → 退出。这条规则**不看**网络、子进程和
  未决的 promise（它们让 `next_due` 之外的工作悬着），P10 用 `app` 的生命周期取代它。

## API 定义辅助（精炼要求）

Electron 有几十个类、几百个方法，绝大多数是"校验参数 → 发一条消息 / 读一个缓存值"。本包必须先给出一套声明式辅助
（`crates/verve-electron/src/api/define.rs`），后续包都用它，不手写样板。它**建立在 `verve::native`（P03 公开）之上**，
只加 Electron 特有的部分，不另起一套通用工具（见 `style.md` §1）。三个新 crate 的 `Cargo.toml` 都写 `[lints] workspace = true`。
形态由实现者定，至少做到：

- 一张表声明一个类：类名、是否继承 `EventEmitter`、构造函数、方法（名字、参数个数、实现函数）、getter/setter、静态方法；
  继承关系（`class X extends BrowserWindow`、`util.inherits`）由辅助统一处理。
- 参数提取与校验一行一个（`args.string(0, "url")?`、`args.object_opt(1)?`、`args.f64_or(2, 800.)`），错误自动是 Electron/Node 的格式。
- "原生对象 ↔ Rust 句柄（WindowId/ContentsId）"的映射与已销毁检查（`Object has been destroyed`）由辅助统一处理。

用本包的最小 `BrowserWindow` 证明这套辅助好用；R1 关卡会检查后续包是否都用了它。

## 本包要交付的最小 `BrowserWindow`

只做：`new BrowserWindow({width,height,show,title})`、`loadURL(dataUrl)`、`webContents.id`、
`'closed'` 事件。完整版是 P13。

进程形态（P03 的 `Runtime::set_process_info`）：主进程 `process.type === 'browser'`，`exit` 是 `ExitBehavior::Host`
（`process.exit(code)` 就是 `Hub::shutdown(code)`，先到的退出码为准），控制台走标准输出/错误；渲染器
`process.type === 'renderer'`，控制台行变成 `FromRenderer::Console`。`require('electron')` 用
`Runtime::define_builtin_lazy` 注册，第一次 `require` 时才造。

本包在这条链路上暂时做不到、后续包接手的事：

- **页面加载**：P02 只解 `data:` URL（走 `verve::encoding::data_url`，base64 也行）；其它地址
  （`file:`、http(s)）按一次失败的加载处理，`DidFailLoad { code: -3, description: "ERR_ABORTED" }`。
  真正的加载管线（`<script>`、`<link>`、`file://`、网络）是 P20。渲染器的
  `document.URL` 在 P02 一直是 `about:blank`（P20 改地址与历史）。

## 步骤

1. 建两个 crate，加入 workspace；锁定 GPUI rev（与 `F:\dev\sandbox-v2\rebon\app\Cargo.toml` 当前所用同一个，
   写在注释里）。
2. 写 `msg/`、`ids.rs`、`hub/`、`ShellPort`。
3. `MainGuest`、`RendererGuest`，带最小 `electron` 模块。
4. `verve-gpui`：GPUI app + 每个 `CreateWindow` 开一个窗口 + DomView 把所有文本节点按文档顺序
   竖排画出来（占位；P30 建 verve-layout 后换成回放 DisplayList）。
5. 无头 `ShellPort` 的最小实现放在 `verve-electron/src/headless.rs`（P00 会扩充）。
6. 测试：无头模式下 `main.js` 创建窗口、加载 data URL，断言 Hub 收到 `CreateWindow`、渲染器文档
   `body.textContent == 'hi'`、关闭窗口后进程退出码 0。

## 验收

- `cargo test -p verve-electron` 通过（无头，不需要 GPUI）。
- `cargo run -p verve-gpui --bin electron -- tests/electron/apps/hello/main.js` 弹出窗口显示 "hi"，
  关掉窗口后进程退出。
- `cargo build -p verve-electron`（不带 GPUI）不拉入任何 gpui crate（`cargo tree -p verve-electron | grep gpui` 为空）。

## 不做

完整的 BrowserWindow（P13）、页面加载管线（P20）、真实布局绘制（P30）。

## 状态

### 完成了什么

- **两个 crate 进了 workspace，都 `[lints] workspace = true`**：`crates/verve-electron`（纯库，`cargo tree -p verve-electron`
  里没有 gpui）与 `crates/verve-gpui`（GPUI Shell + `electron` 二进制）。GPUI 是 workspace 依赖，rev 与 Rebon 的
  `[patch."https://github.com/zed-industries/zed"]` 同一个：`Quorafind/zed-c@e679b519961d280cb878183709e31c28684fb5e0`，
  `gpui` 带 `version = "=0.2.2"`，`gpui_platform` 带 `font-kit`（同 Rebon）。`verve-electron` 用 `size.md` 定的特性集
  （`default-features = false, features = ["wasm"]`）。
- **契约**：`ids.rs`、`msg/`（`Cloned` 即 `verve::clone::Cloned`；`SyncReply`；`WebPreferences` 子集）、`hub/`
  （`Hub`、`ShellPort`、`RendererHandle`）。窗口与页面的对应只有 Hub 记。
- **两个 Guest**：`MainGuest`：`ProcessInfo { process_type: "browser", exit: ExitBehavior::Host(→ Hub::shutdown) }`，
  控制台走标准输出/错误；入口脚本是 Hub 发给它的第一个 job（isolate 与句柄先就位）。`RendererGuest::new(hub, contents,
  prefs)`：`process_type: "renderer"`，`verve_dom::install(FrameClock::Host)`，控制台行变成 `FromRenderer::Console`，`data:` URL
  走 `verve::encoding::data_url`。两者的 `Runtime::set_waker` 接的是 `Isolate::poker()`；共用的节拍在 `beat.rs`。
- **线程的生死**：`Hub::shutdown` 打断并丢掉所有 isolate（主进程、每个页面），线程随之结束并放开 `Arc<Hub>`；
  isolate 的 `Health` 在唤醒时读：主进程死 → `shutdown(1)`，页面死 → 一次 `Gone { Crashed }`。
  `BrowserWindow` 对象由 `MainState` 强持有，直到 `'closed'` 之后的 `forget`（Electron 同样钉住到关闭）。
- **`api/define.rs`** 建在 `verve::native` 上，没有工具箱副本：类用 `native::class`/`ClassSpec`，参数错误用
  `native::invalid_arg_type`/`invalid_arg_value`。只加：`Kind` + `bind`/`id_of`/`live`（按类的 id 映射，存在 realm 一侧的表里，
  脚本看不到、复制不了）、`Args`（gin 方言 / `Args::node` Node 方言）、`Fields`（构造选项：`Fields::new` 宽松，`Fields::strict`
  Node 属性形态；`f64`/`f64_opt`/`int`/`f32`/`bool`/`string`/`function`/`object`/`one_of`）、`emit`、`emitter`。
- **内核提交**（`src/`）：`verve::events::{emitter_class, emitter_init}` 公开（`sockets.rs` 的副本删了）；
  `ClassSpec::new_only: Option<NewOnly>`；`Isolate::poker()`。
- **最小 `BrowserWindow`**（原生类，`new` 才能调用）、`HeadlessShell`（弱引用 Hub）、GPUI Shell 与 `electron` 二进制
  （目录取 `main.js`/`index.js`，文件按路径跑；GPUI 用 `QuitMode::Explicit`，退出码用 `std::process::exit(code)` 整个交出）。
- **CI**：`.github/workflows/style.yml` 的 Linux 任务装了 GPUI Linux 后端需要的系统库（同 Zed 的 CI）。

### Electron 39.8.10 的观测（`npm i electron@39` 的探针，冻结在 `tests/api.rs`、`tests/define.rs`、`tests/pipeline.rs`）

- `BrowserWindow(...)`、`BrowserWindow.call({}, …)`、`util.inherits` 子类里的 `BrowserWindow.call(this)` 都抛
  **`Error: Requires constructor call`**（是 `Error`，不是 V8 的 `TypeError: Class constructor … without 'new'`）。
  `class X extends BrowserWindow` 可以。原型链 `BrowserWindow > BaseWindow > EventEmitter`；`BrowserWindow.length === 0`。
- 构造选项**宽松**：`new BrowserWindow(5 | 'x' | null)` 是默认窗口；`width: 'abc' | '300' | 300.7`、`height: true`、`title: 5 | {} | null`
  都当没写（Integer 才算数）；`show: 'yes'`、`resizable: 0` 按真值（`false` 的是 `0`）；`width: -5` 被夹到最小宽度；
  getter 里抛的错原样传出。`webPreferences: 5` 当没写。
- gin 方法：`loadURL(5)` → `TypeError: Error processing argument at index 0, conversion failure from `（`from` 后面：
  值是字符串则为它自己，`null`/`undefined` 则是这个词，其它为空）；`setTitle()` → `TypeError: Insufficient number of arguments.`。同步抛出。
- 已销毁：关闭后（含 `'closed'` 监听器里）`win.loadURL/close/getTitle/webContents` → **`TypeError: Object has been destroyed`**；
  `win.id` 与 `webContents.id` 仍可读；`win.destroy()` 再调不抛。`BrowserWindow.prototype.close.call({})` 同样是
  `TypeError: Object has been destroyed`（没有包装对象的东西都这样）；对别的类的实例（`win.webContents`）是
  `TypeError: Illegal invocation`。窗口实例上没有 symbol 属性。

### 验收（本机实跑）

- `cargo test -p verve-electron`（无头）：单元 8、`api` 7、`define` 5、`pipeline` 12，全过。`cargo test --workspace --no-fail-fast`：
  747 passed，0 failed，16 ignored。
- `cargo tree -p verve-electron | grep -c gpui` → 0。
- `electron.exe tests/electron/apps/hello/main.js`：开出标题 `Verve` 的 800×600 窗口，截图里画着 "hi"。
  `crates/verve-gpui/tests/electron_bin.rs`（Windows/macOS）跑真正的二进制：最后一个窗口关掉、`'closed'` 里开新窗口、再
  `process.exit(5)` → 退出码 5（修复前是 0）；无窗口 → 0；抛错 → 1。
- `cargo clippy --workspace --all-targets -- -D warnings` 过；`bash scripts/linux-check.sh`（整个 workspace，含 verve-gpui）过。
- release `electron.exe`（`verve` 特性 `wasm`，含 GPUI）：不带 JIT 27.6 MB；带 JIT 33.0 MB（上限 34 MB）。

### 没完成 / 不是本包范围的

- **退出规则**只看"没有窗口、没有定时器"：它不看网络、子进程和未决的 promise（P10 用 `app` 的生命周期取代它）。
- **`RendererView`** 没有 `display_list`/`images`（P30/P35 追加）；`DomView` 只画文本节点。
- **页面加载**：只解 `data:` URL，别的地址按 `DidFailLoad{ ERR_ABORTED }`；`document.URL` 一直是 `about:blank`（P20）。
- **`BrowserWindow`** 其余成员；`'close'` 没有 event 对象，`CloseAnswer` 恒为不阻止（P13）。`webContents` 暂时只有 `id`，
  所以 `close.call(win.webContents)` 现在是 `Object has been destroyed` 而不是 `Illegal invocation`（P13 把它做成另一个 `Kind`）。
- **Shell 窗口操作**：只实现创建、关闭往返、`SetTitle`；其余是 P34 的。输入（指针、键盘、滚动、焦点）是 P24/P32 的。
- 渲染器 `FromRenderer::Console` 到了主进程后被忽略（P13 做 `console-message`）；`Gone` 只打印（P13 做 `render-process-gone`）；
  没有设 `navigator.userAgent`（P20）。
- 主进程 panic 后内核 `forget` 掉出事的 guest，它手里的 `Arc<Hub>` 随之泄漏（进程马上结束，无碍）。

### 已知问题

1. `Cargo.lock` 里 `unicode-properties` 从 0.1.4 降到 0.1.3（`gpui_web` 钉死 `=0.1.3`）；其余只增不改。
2. `FromShell::Window{event: Closed}` 的去重归 Shell：真实 Shell（P34）与无头 Shell 都要保证只发一次；窗口打不开按 `Closed` 报。
3. `loadURL` 的 promise 立刻兑现（没有加载完成的回程消息，P13/P20）。
4. 线程建不起来：主进程 → 打印并以 1 退出；某页 → 窗口照旧，主进程收到 `FromRenderer::Gone { LaunchFailed }`。

### 给其他包的备注

- **P00**：`HeadlessShell::new(&hub)`（弱引用 Hub）；有 `received()`/`windows()`/`user_close()`/`exit_code()`，其余自己加。
- **P10**：参数解析在 `crates/verve-gpui/src/bin/electron.rs`；`app.getAppPath()`、`package.json` 的 `main`、flag、单实例锁是你的。
  `Hub::shutdown(code)` 已是 `app.exit` 该调的；`process.exit` 已经接上。
- **P12/P13**：`Hub::create_renderer(contents, prefs)` 返回 `RendererHandle`；`MainState::page(window)`；新类用自己的
  `define::Kind` 并 `bind`/`live`；原生类设 `new_only: Some(NewOnly::Native)`；构造选项用 `Fields::new`（宽松），Electron 用 JS 写的
  方法用 `Args::node`。`SendSync` 的答复是 `SyncReply::send(self, value)`。
- **P20**：替换 `RendererGuest::load` 与 `markup_of`；`Hub::preferences_of(contents)` 给导航时重建 isolate 用。
- **P30/P31/P34**：`shell.rs` 的 `WindowOp` 分支与 `headless.rs` 必须一致；`DomView::texts` 可整段删。
- **P34 追加的契约**：`Hub::observe_shell(observer)`（观察 Shell 发给主进程的每条 `FromShell`，在发送线程上、先于主进程；
  两种 Shell 的共用测试靠它）；`WindowEvent::FirstPaint`、`WindowEvent::NativeHandle(u64)`、`ToRendererInput::Visibility(bool)`
  （上表预留的三个变体，追加在各自枚举末尾，`main_guest`/`renderer_guest` 的 `_ => {}` 接住）。`FirstPaint` 是“窗口可见时画出的第一帧”，每窗口一次（真 Shell 在显示时强制画一帧；无头 Shell 在可见窗口的第一份快照到达时发）；`show: false` 的窗口显示前没有帧，所以隐藏窗口的 `ready-to-show` 要由 P13 按渲染器的第一份快照决定，不是它。`NativeHandle` 是 `HWND` / `NSView*`（Electron 的 `getNativeWindowHandle` 同）。
  `AppEvent::QuitRequested`（系统要求退出：与
  `app.quit()` 同一条路，`api/app/mod.rs::platform_event` 接到 `quit::quit`）。GPUI 在调用退出观察者之后立刻丢掉所有窗口，只等 200 毫秒且不再轮询 Shell，Windows 上随后 `exit(0)`；`WindowsWindow::drop` 只隐藏窗口，销毁留给一个不会再跑的任务，所以这条路上没有窗口会报 `Closed`：真 Shell 只能尽快发出 `QuitRequested`，主进程的 `before-quit` 否决与退出码拿不到；在 GPUI 关停之前截 `WM_QUERYENDSESSION` 才能做到更多，未做。
