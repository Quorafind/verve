# P02 骨架、线程与消息契约

**依赖**：无（第一个做）  **规模**：M  **修改**：新建 `crates/verve-electron`、`crates/verve-gpui`；根 `Cargo.toml` 的 workspace members

## 目标

建立两个新 crate 和 `electron` 可执行文件的空架子，定下**所有包共用的线程模型与消息类型**，
跑通一条最小链路：`electron.exe main.js` → 主进程 isolate 执行 `main.js` → 它调用
`new BrowserWindow().loadURL('data:text/html,<h1>hi</h1>')` → 渲染器 isolate 解析出文档 → Shell
（GPUI）打开窗口并把 `<h1>` 的文字画出来（只需最朴素的绘制，P30 再做真）。

## 背景（先读）

- `src/isolate.rs`：`Guest` trait、`Isolate`、`Outbox`、`InterruptHandle`。所有 JS 线程都用它。
- `crates/verve-dom/src/lib.rs`：`install`、`Document`、`Snapshot`、`Update`、`Request`、输入方法、`set_layout`。
- `crates/verve-dom/tests/isolate.rs`：文档放在 isolate 里、另一线程读快照的现成例子。
- `src/cli.rs`：`verve run` 的事件循环（什么时候退出、怎样睡到下一个到期点）。
- 参考（只读）：`F:\dev\sandbox-v2\su-editor\crates\su-shell\src\editor\obsidian\view.rs`——su-editor 把计算样式映射成
  GPUI `Div` 的做法。**本项目不沿用这种做法**（布局在渲染器线程，见 P30），只参考它的样式值解析（颜色、长度）。

## crate 划分

```
crates/verve-electron   (lib + bin "electron")
  src/lib.rs            pub mod 声明、ElectronOptions、run()
  src/ids.rs            WindowId / ContentsId / 计数器
  src/msg.rs            本文定义的全部消息类型（唯一来源）
  src/hub.rs            Hub：路由与注册表
  src/main_guest.rs     MainGuest: isolate::Guest（主进程）
  src/renderer_guest.rs RendererGuest: isolate::Guest（渲染器）
  src/api/mod.rs        require('electron') 的两种形态：main / renderer
  src/api/*.rs          各模块（后续包填充），本包只放空模块和 BrowserWindow 的最小版
  src/bin/electron.rs   入口：解析参数 → verve_gpui::run(shell_options, hub)
  依赖：verve、verve-dom、boa_engine、crossbeam-channel（或 std mpsc）
  **不依赖 GPUI**（这样无头测试可以不编 GPUI）

crates/verve-gpui       (lib)
  src/lib.rs            run(hub, opts)：启动 GPUI Application，主循环
  src/shell.rs          处理 ToShell 消息：开关窗口等
  src/dom_view.rs       DomView：Snapshot → GPUI 元素（本包只画文本节点）
  依赖：gpui、gpui_platform、verve-electron（只用 msg/hub 类型）、verve-dom（只用 Snapshot 类型）
```

`electron` 二进制放在 `verve-electron` 里，但它需要 GPUI：用 feature `gui`（默认开）引入
`verve-gpui`，关掉 `gui` 时 `electron` 以无头模式运行（P00 用）。为避免循环依赖，把 bin 放到
`crates/verve-gpui/src/bin/electron.rs` 也可以——**二选一，在本包定下来并写回本文档**。推荐：
bin 放在 `verve-gpui`，`verve-electron` 保持纯逻辑库。

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
| `dialog.show*Sync` | P14 | 对话框在 OS 主线程弹出，主进程线程等结果 |
| `globalShortcut.register` / `isRegistered` | P15 | 等 Shell 注册结果，500 ms 超时后按失败处理 |
| `clipboard.*`（若走 Shell） | P14 | 推荐直接用 arboard 在主进程线程完成，不走 Shell |

新增例外必须先改这张表。

## 消息类型（`crates/verve-electron/src/msg.rs`）

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
}

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
}

// ---------- Shell → 主进程 ----------
pub enum FromShell {
    Window { window: WindowId, event: WindowEvent },
    App(AppEvent),
    MenuClicked { menu_item: u32, window: Option<WindowId> },     // P15
    Displays(Vec<Display>),                                       // screen 模块缓存（P14）
}

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
    /// macOS 点 Dock 图标；Windows 无。
    Activate { has_visible_windows: bool },
    OpenFile(std::path::PathBuf), OpenUrl(String),
    ThemeChanged { dark: bool },
    /// 第二个实例启动（P10 的单实例锁）
    SecondInstance { argv: Vec<String>, cwd: std::path::PathBuf },
}

pub struct Display { pub id: u32, pub bounds: Rect, pub work_area: Rect, pub scale_factor: f32, pub primary: bool }
pub struct Rect { pub x: f32, pub y: f32, pub width: f32, pub height: f32 }

// ---------- 主进程 → 渲染器 ----------
pub enum ToRenderer {
    Load { url: String, options: LoadOptions },      // loadURL / loadFile / reload
    Ipc { channel: String, args: Vec<Cloned>, sender_id: ContentsId /* 0 = 主进程 */ },
    InvokeReply { call: u64, result: Result<Cloned, Cloned /* Error 克隆 */> },
    SendSyncReply { call: u64, value: Cloned },      // 也可直接走 oneshot，见 P12
    ExecuteJavaScript { call: u64, code: String, user_gesture: bool },
    SetZoomFactor(f64),
    Stop,
}

// ---------- 渲染器 → 主进程 ----------
pub enum FromRenderer {
    Ipc { channel: String, args: Vec<Cloned> },
    Invoke { call: u64, channel: String, args: Vec<Cloned> },
    SendSync { channel: String, args: Vec<Cloned>, reply: oneshot::Sender<Cloned> },
    /// ipcRenderer.sendTo / sendToHost
    SendTo { target: ContentsId, channel: String, args: Vec<Cloned> },
    ExecuteJavaScriptReply { call: u64, result: Result<Cloned, Cloned> },
    Lifecycle(PageEvent),
    /// verve-dom 的 Request（Navigate/Open/Title/Alert/CopyText/Fullscreen…）
    Dom(verve_dom::Request),
    Console { level: verve::Level, message: String, line: u32, source: String },
    Gone { reason: GoneReason },                     // 'render-process-gone'
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
}
```

`MenuOp`、`CompositionEvent`、`GoneReason`、`LoadOptions`、`PointerKind`、`TitleBarStyle` 在本包给出最小定义，
后续包（P15/P24/P13/P20/P32/P34）在其上**追加**。

已登记的追加（各包实现时加入，本表是汇总）：

| 变体 | 包 |
|---|---|
| `FromShell::MenuClicked` 增加字段 `triggered_by_accelerator: bool`（占位变体，允许 P15 改字段） | P15 |
| `ToRenderer::EditCommand(EditCommand)`（菜单 role：copy/paste/undo/selectAll…） | P15 |
| `ToRenderer::SetProxy(…)` | P16 |
| `ToRenderer::InspectorEval { call, code }`、`FromRenderer::InspectorEvalReply { call, result }`、`ToShell::Inspector(…)` | P43 |
| Shell 提供 `native_handle(WindowId) -> Option<RawWindowHandle>`（对话框父窗口、muda 挂菜单） | P34（P14/P15 使用） |
| `AppEvent::Ready`（Shell 启动完成，`app` 的 `ready` 依据） | P10 |
| `WindowEvent::FirstPaint`（`ready-to-show` 的精确依据）、`WindowEvent::NativeHandle(u64)` | P13 / P34 |
| `ToRenderer::BeforeUnload`、`ToRenderer::Unload`、`FromRenderer::BeforeUnloadReply { prevented }` | P13 |
| `ToRenderer::WindowMessage`、`FromRenderer::WindowProxyOp`（`window.open` 返回的代理） | P13 |
| `FromRenderer::ContextMenu(ContextMenuParams)` | P13 |
| `FromShell::KeyForContents`（`before-input-event`，Shell 不等 JS：先投递给主进程，主进程决定后再转渲染器） | P13 / P32 |
| `ToRendererInput::Visibility(bool)`（`document.visibilityState`） | P13 / P34 |
| `FromRenderer::Font(..)`、`ToRendererInput::FontsChanged`（`@font-face` 注册到 GPUI 文字系统） | P31 |
| `ToRendererInput::Choose { select, value }`（`<select>` 下拉弹层选中） | P32 |
| `RendererView.images`、`PaintItem::Image.frame`、`DisplayList.chunks` | P35 / P33 |
| `FromRenderer::{ProtocolRequest, ZoomChanged, PreloadError}`、`ToRenderer::ProtocolResponse` | P20 / P21 |
| `ToRendererInput::{Drag, Paste, ClipboardContents}` | P24 |
| `CompositionEvent` 具体化为 `Update { text, selection } \| Commit \| Cancel` | P24 |

verve-dom 侧的配套改动（由 P20/P23/P24 在 verve-dom 内完成）：`Request::Open(String)` → `Open { url, name, features }`；
`CopyText` → `WriteClipboard { text, html, custom }`；新增 `ReadClipboard { call }`；`ScrollRequest::Reveal` 增加 `block`/`inline`/`smooth`；
`SnapshotData` 增加 `pseudo`（P22）、`canvases`/`images`（P25）、`fonts`（P20）、`selection`/`caret`（P24），以及平铺树遍历
`Snapshot::flat_children`/`flat_parent`（P23）——**P30 的布局必须遍历平铺树**，否则 shadow DOM 内容画不出来。

滚动归渲染器所有：渲染器自己执行 `verve_dom::Request::Scroll`（不再转给 Shell）；Shell 对滚轮做即时视觉滚动，
随后以渲染器回报的偏移为准（P30/P33）。

修正：
- `FromRenderer::SendTo`：Electron 28 起已移除 `ipcRenderer.sendTo`，此变体保留给 `sendToHost`/内部用途，`ipcRenderer` 不再产生它。
- 文中 `oneshot::Sender` 用 `std::sync::mpsc::SyncSender<Cloned>`（容量 1）实现，不引入新依赖。

## Hub（`hub.rs`）

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
    pub fn renderer_view(&self, id: ContentsId) -> Option<RendererView>; // Shell 每帧取
    pub fn set_shell_waker(&self, waker: Arc<dyn Fn() + Send + Sync>); // 有新 ToShell/新 view 时唤醒 GPUI
    pub fn shutdown(&self, exit_code: i32);
}
```

- 主进程 Guest 收到 `FromShell`/`FromRenderer` 后在自己线程上分发成 JS 事件。
- 渲染器的 `FromRenderer::Lifecycle`/`Dom`/`Console` 先到 Hub，由 Hub 转给主进程（主进程上的
  `webContents` 对象发事件）；`Dom(Request::Title)` 等同时也要让 Shell 知道（更新窗口标题）：
  **Hub 负责拷贝转发，Guest 不直接认识 Shell**。
- 无头模式：`Hub` 的 Shell 端由 P00 的 `HeadlessShell` 实现（同一套消息），用来在测试里断言。
  所以把"Shell 端"抽象成 trait：

```rust
pub trait ShellPort: Send + Sync {
    fn send(&self, msg: ToShell);
    fn wake(&self);
}
```

## 主进程 Guest / 渲染器 Guest

- `MainGuest::new(hub, entry)`：建 `verve::Runtime`；`process.type = 'browser'`；
  `define_builtin("electron", api::main_module)`；`tick` 里依次：处理入站消息队列 → `run_due` →
  `run_workers` → 微任务。`next_due` 合并 Runtime 的定时器和入站消息。
- `RendererGuest::new(hub, contents, prefs)`：建 Runtime + `verve_dom::install(Options{ frames: FrameClock::Host, .. })`；
  `process.type = 'renderer'`；`define_builtin("electron", api::renderer_module)`；`publish` 返回
  `RendererView`（只在 `Document::update()` 有变化或 `ran` 时）。
- 退出条件（与 Electron 一致）：`app.quit()` 或最后一个窗口关闭且无 `window-all-closed` 监听者时
  退出（P10 细化）。本包只需：主进程脚本结束且没有窗口、没有定时器 → 退出。

## API 定义辅助（精炼要求）

Electron 有几十个类、几百个方法，绝大多数是"校验参数 → 发一条消息 / 读一个缓存值"。本包必须先给出一套声明式辅助
（`crates/verve-electron/src/api/define.rs`），后续包都用它，不手写样板。形态由实现者定，至少做到：

- 一张表声明一个类：类名、是否继承 `EventEmitter`、构造函数、方法（名字、参数个数、实现函数）、getter/setter、静态方法；
  继承关系（`class X extends BrowserWindow`、`util.inherits`）由辅助统一处理。
- 参数提取与校验一行一个（`args.string(0, "url")?`、`args.object_opt(1)?`、`args.f64_or(2, 800.)`），错误自动是 Electron/Node 的格式。
- "原生对象 ↔ Rust 句柄（WindowId/ContentsId）"的映射与已销毁检查（`Object has been destroyed`）由辅助统一处理。

用本包的最小 `BrowserWindow` 证明这套辅助好用；R1 关卡会检查后续包是否都用了它。

## 本包要交付的最小 `BrowserWindow`

只做：`new BrowserWindow({width,height,show,title})`、`loadURL(dataUrl)`、`webContents.id`、
`'closed'` 事件。完整版是 P13。

## 步骤

1. 建两个 crate，加入 workspace；锁定 GPUI rev（与 `F:\dev\sandbox-v2\rebon\app\Cargo.toml` 当前所用同一个，
   写在注释里）。
2. 写 `msg.rs`、`ids.rs`、`hub.rs`、`ShellPort`。
3. `MainGuest`、`RendererGuest`，带最小 `electron` 模块。
4. `verve-gpui`：GPUI app + 每个 `CreateWindow` 开一个窗口 + DomView 把所有文本节点按文档顺序
   竖排画出来（占位；P30 建 verve-layout 后换成回放 DisplayList，本包 `display_list` 字段可先恒为 None）。
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

（实现者完成后填写）
