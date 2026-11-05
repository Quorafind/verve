# P00 夹具应用与无头验收

**依赖**：P02  **规模**：M  **修改**：`crates/verve-electron/src/headless.rs`、`crates/verve-electron/tests/`、`tests/electron/apps/`

## 目标

所有 Electron 行为的验收都能**在没有显示器、没有 GPU、没有 GPUI** 的 CI 上自动跑：一个无头 Shell
冒充 GPUI 端，按 P02 的消息协议应答；一组小夹具应用覆盖每个模块；一个测试辅助库让每个包几行就能写端到端测试。

## 背景（先读）

- `P02` 的 `ShellPort`、`ToShell`/`FromShell`、`ToRendererInput`、`RendererView`。
- `tests/rebon.rs`：启动真实程序并断言输出的写法；`tests/common/mod.rs`：测试时钟。
- verve-dom 的 `set_layout`：无头时没有 GPUI 量盒子，需要一个"假布局"。

## 接口契约

### HeadlessShell（`headless.rs`，`pub`，测试和 `--headless` 共用）

```rust
pub struct HeadlessShell { /* 记录全部收到的 ToShell，维护窗口状态 */ }
impl ShellPort for HeadlessShell { … }
impl HeadlessShell {
    pub fn new(hub: &Arc<Hub>, opts: HeadlessOptions) -> Arc<Self>;
    pub fn windows(&self) -> Vec<HeadlessWindow>;          // id、标题、bounds、可见、焦点、是否已关
    pub fn received(&self) -> Vec<ToShell>;                // 全部历史（ToShell 需 derive Debug + Clone）
    pub fn user_close(&self, window: WindowId);            // 模拟点关闭按钮（走 CloseRequested 往返）
    pub fn resize(&self, window: WindowId, w: f32, h: f32);
    pub fn click(&self, window: WindowId, selector: &str); // 通过 Hub 让渲染器找到节点并 press
    pub fn type_text(&self, window: WindowId, text: &str);
    pub fn menu_click(&self, item: u32);
    pub fn snapshot(&self, window: WindowId) -> Option<verve_dom::Snapshot>;
}
pub struct HeadlessOptions {
    pub layout: FakeLayout,               // 默认 BlockStack：每个块元素占满宽度、每行文本 16px 高，依次往下排
    pub displays: Vec<Display>,           // 默认一个 1920x1080 @1x
    pub dark: bool,
}
```

- 窗口状态机与真实 Shell 一致：`show:false` 的窗口不可见、`Close` 先发 `CloseRequested`… 由本包实现，
  P34 实现真实版时以此为参照（**两边行为必须一样**，P34 会跑同一组测试的有头版本）。
- `FakeLayout::BlockStack` 让 `getBoundingClientRect`、`ResizeObserver`、`IntersectionObserver` 在无头下有合理的值。
  **P30 合并后**：布局在渲染器线程由 `verve-layout` 完成，无头模式改为 `FakeLayout::Engine`——同一个布局引擎配
  P30 提供的无头 `TextMeasurer`（每字 0.5em 宽），`BlockStack` 只作为 P30 之前的过渡。

### 测试辅助（`crates/verve-electron/tests/support/mod.rs`）

```rust
pub struct App { /* hub + headless shell + 退出码 */ }
impl App {
    pub fn launch(app_dir: &Path) -> App;                    // 等同 `electron <app_dir>`
    pub fn launch_script(main_js: &str, files: &[(&str, &str)]) -> App;  // 临时目录写入文件再启动
    pub fn run_until(&self, cond: impl Fn(&App) -> bool, timeout: Duration) -> bool;
    pub fn eval_main(&self, js: &str) -> String;             // 在主进程求值，结果 JSON
    pub fn eval_renderer(&self, contents: ContentsId, js: &str) -> String;
    pub fn console(&self) -> Vec<String>;                    // 主进程 + 渲染器的 console，按时间序
    pub fn wait_exit(self, timeout: Duration) -> i32;
}
```

### 夹具应用（`tests/electron/apps/`，每个一个目录，带 `package.json`）

| 目录 | 内容 | 被哪些包用 |
|---|---|---|
| `hello` | 一个窗口加载 `index.html`，显示 `process.versions` | P02 P10 P20 |
| `quick-start` | electron-quick-start 原样（MIT，带许可证） | P10 P13 P20 P21 P41 |
| `ipc` | send/on、invoke/handle、sendSync、reply、webContents.send、错误传递 | P12 |
| `preload-isolated` | contextIsolation + contextBridge 暴露 API | P21 |
| `preload-node` | nodeIntegration: true、contextIsolation: false | P21 |
| `multi-window` | 父子窗口、modal、`getAllWindows`、`window.open` + `setWindowOpenHandler` | P13 P34 |
| `lifecycle` | 记录 `ready`/`window-all-closed`/`before-quit`/`will-quit`/`quit` 顺序 | P10 |
| `single-instance` | `requestSingleInstanceLock` + `second-instance` | P10 |
| `protocol` | `protocol.handle('app', …)` 加载整站 | P16 P20 |
| `menu-tray` | 应用菜单、右键菜单、托盘 | P15 |
| `dialogs` | 各对话框（无头下 Shell 按预设答复） | P14 |
| `child` | spawn/exec/fork | P17 |
| `asar` | 构建时打成 `app.asar`（测试里用 Rust 生成，不依赖 npm 的 asar 包） | P11 |
| `react-vite` | 预构建好的 Vite + React 产物（dist 提交进仓库，不在测试时构建） | P20 P30 P41 |

每个夹具配一个 `expected.json`（冻结的真实 Electron 39 观测：事件顺序、console 输出），由
`tests/electron/record.cjs` 在装了真实 Electron 的机器上生成（脚本提交进仓库，生成结果也提交）。

### `--headless` 命令行开关

`electron --headless <app>`：用 HeadlessShell 跑，窗口操作打印到 stderr（`[window 1] create 800x600 "title"`），
供不方便写 Rust 测试的人手动验证。

## 步骤

1. `HeadlessShell` + `FakeLayout`。
2. 测试辅助库。
3. 夹具应用与 `record.cjs`；先用真实 Electron 录 `hello`、`ipc`、`lifecycle` 三份 `expected.json`。
4. 每个夹具一个**先标 `#[ignore]` 的**测试骨架（`tests/fixtures.rs`），对应包完成时去掉 ignore。
5. `--headless` 开关。

## 验收

- `cargo test -p verve-electron` 在没有 GPUI 依赖的情况下编译运行；`hello` 夹具的测试通过（依赖 P02 的最小链路）。
- 其余夹具测试存在并被 ignore，每个 ignore 上注明负责的包号。
- CI（GitHub Actions，Windows + Linux）跑无头测试。

## 不做

截图对比（P41 可选）；GPUI 有头测试（P34）。

## 状态

### 完成了什么

- **`HeadlessShell`**（`headless.rs` + `headless/window.rs` + `headless/layout.rs`，不依赖 GPUI）。
  `HeadlessShell::new(&hub, HeadlessOptions) -> Arc<Self>`（弱引用 Hub，同时让 Hub 开始记 console）；
  `windows() -> Vec<HeadlessWindow>`、`received() -> Vec<ToShell>`、`exit_code()`、`snapshot(window) -> Option<Snapshot>`、
  `user_close(window)`（走 `CloseRequested` 往返）、`resize(window, w, h)`、`click(window, selector)`、`type_text(window, text)`、
  `menu_click(item)`、`set_dark(bool)`；`headless::run(&hub, entry) -> i32` 是 `--headless` 的主循环。
  `HeadlessOptions { layout, displays, dark, trace }`（默认一块 1920x1080@1x、浅色、`BlockStack`、不 trace）。
  `HeadlessWindow { id, contents, title, bounds, visible, focused, minimized, maximized, fullscreen, closed }`。
- **窗口状态机的规则**全部写在 `headless/window.rs` 的模块文档里（P34 的真实 Shell 照它实现、跑同一组测试）；要点：
  关闭后忽略一切操作且只发一次 `Closed`；创建时在主显示器工作区居中（`fullscreen` 则占满显示器），不发事件；
  `Show` = `Shown`+`Focus`，`Hide` = `Hidden`+`Blur`（只发真正变了的）；`SetBounds`：先夹到 min/max（max 为 0 = 不限），
  再 `Resized` 后 `Moved`；`Maximize` = `Resized`、`Moved`、`Maximized`；`Unmaximize` = `Resized`、`Unmaximized`（没有 `Moved`）；
  `Minimize` = `Minimized`、`Blur`；`Restore` = `Restored`、`Focus`；全屏 = `EnterFullScreen`、`Resized`、`Moved`，退出对称；
  用户拖边（`resize`）对不可调整大小的窗口无效，应用的 `SetBounds` 不受 `resizable` 限制。事件顺序是**真实 Electron 39.8.10
  （Windows）的观测值**，冻结在 `tests/electron/apps/window-state/expected.json`；只有 `Center` 与 `Destroy` 是本 Shell 自己定的。
  `Resized` 同时给页面发 `ToRendererInput::Viewport`，`Focus`/`Blur` 发 `Focus(bool)`；创建时发 `Viewport`、`ColorScheme`、
  （聚焦时）`Focus(true)`。其余操作（进度条、图标、透明度、任务栏…）只记进 `received()`。
- **`FakeLayout::BlockStack`**：块元素占满父宽、依次往下排，高度是内容高度；文字每行 16px、每个非空白字符 8px，超出行宽就折行
  （内联元素本身不拆）；`inline*` 的 `display` 与未设置 display 的元素排在行内；忽略 margin/padding/border/显式尺寸。
  每次页面发布新视图，Shell 就算一次布局，**和上次不同才**以 `ToRendererInput::Layout` 发回（不会因此循环）。
- **测试辅助** `tests/support/mod.rs`：`App::launch(dir)`、`launch_script(main_js, files)`、`launch_script_in(.., dark)`、
  `run_until(cond, timeout)`、`window(title)`、`page_text(window)`、`eval_main(js)`、`eval_renderer(contents, js)`（结果是 JSON，
  抛错则测试失败；`eval_main` 在全局作用域求值，要让测试看的东西，应用得挂在 `globalThis` 上）、`console()`、`exit_code(timeout)`、
  `wait_exit(self, timeout) -> i32`；`App` 析构时 `shutdown` 并删临时目录。`TIMEOUT` = 10 s。
- **Hub/Guest 的小改动**（任务书「修改」之外，为上面所必需）：`Hub::to_main` / `Hub::on_renderer` 公开（不等待的 job 入口）；
  `Hub::record_console` / `console_lines`（`ConsoleLine { contents, text }`，主进程行在 console sink 里记，页面行在
  `Hub::from_renderer` 里记，**只有开了才记**）；`MainGuest::eval`、`RendererGuest::{eval, click, type_text}`，
  `RendererGuest::input` 处理 `Layout`（`msg.rs` 里该变体的文档相应改了）；新文件 `probe.rs`（共用的 JSON 求值）。
- **夹具**（`tests/electron/apps/`）：`hello`、`quick-start`（electron/electron-quick-start 的最后一个提交
  `91a1d64abdeb31e7506d19434c46a3a541ffd3cb`，原样复制，只去掉 `.git`/`.github`；**许可证是 CC0 1.0，不是 MIT**，见 `SOURCE.md`）、
  `ipc`、`preload-isolated`、`preload-node`、`multi-window`、`protocol`、`menu-tray`、`dialogs`、`child`、`asar`（只有源码，包由
  P11 的测试用 Rust 打）、`react-vite`（Vite 7.3.7 + React 19.3.1，只提交 `dist/`，约 220 KB，构建方式见其 README）、
  `window-state`（本包追加：窗口事件顺序的冻结）。`lifecycle`、`single-instance` 是 P10 的，没建。
- **`tests/electron/record.cjs`**：`node tests/electron/record.cjs --electron <electron.exe> [--timeout ms] [app…]`；同一个文件在
  Electron 里（`-r`）作记录器，驱动端给每个应用独立的 userData/APPDATA，超时后按 PID 杀掉（Windows 用 `taskkill /PID /T /F`）。
  用 **Electron 39.8.10** 录了 `hello`、`ipc`、`window-state`，另外 `preload-isolated`、`preload-node`、`multi-window`、`protocol`、
  `child`、`asar`、`react-vite` 也录了（它们都 `show: false`、自己结束，顺便证明这些夹具在真 Electron 里能跑）。
  `menu-tray`（会放托盘图标）、`dialogs`（会等人点）、`quick-start`（不会自己退出）没录。
- **`tests/fixtures.rs`**：`hello` 实际在跑（窗口 800x600、页面 `document.title`、`process.versions` 列表、主进程 console 与录制一致、
  退出码 0）；其余每个夹具一个 `#[ignore = "P1x: …"]` 的测试，内容是"跑到结束并与 `expected.json` 逐进程比较 console、退出码 0"
  （`menu-tray`/`dialogs` 只比退出码；`react-vite` 点 `#inc` 看计数）。负责包：quick-start/lifecycle/single-instance P10，
  ipc P12，preload-* P21，multi-window 与 window-state P13，protocol P16（+P20），menu-tray P15，dialogs P14，child P17，asar P11，
  react-vite P20（+P30）。去掉 ignore 前这些测试都会立刻失败（已确认不会挂住）。
- **`electron --headless <app>`**：`crates/verve-gpui/src/bin/electron.rs` 里去掉开头的 `--headless` 并走 `verve_electron::headless::run`
  （P10 并入 `Launch` 时改掉），stderr 打印 `[window 1] create 800x600 "title"`、`[window 1] Close`、`[app] quit 0`；
  测试 `crates/verve-gpui/tests/electron_headless.rs`（不开窗口，任何机器都能跑）。
- **CI**：`.github/workflows/electron.yml`，Windows + Linux 跑 `cargo test --locked -p verve-electron`。**我无法运行它**；
  本机跑过的是 `cargo test -p verve-electron`（不带 `--locked`，但 `Cargo.lock` 已随提交更新）。

### 没完成 / 不是本包范围的

- `lifecycle`、`single-instance` 夹具与录制（P10）；`FakeLayout::Engine`（P30 之后）。
- Shell 不发 `ToRendererInput::Frame`（`requestAnimationFrame` 在无头下不会被驱动；发了会让动画页面空转），也不发
  `FromShell::Displays`（`HeadlessShell::new` 时主进程还没起来，消息会丢；P14 要决定怎么交付，例如在 `AppEvent::Ready` 时）。
- 对话框预设答复（P14）、菜单/托盘消息的处理（`ToShell::Menu` 现在是空枚举，P15）。
- 窗口的 `parent`/`modal` 关系、`always_on_top`、`closable: false` 等选项没有进状态机。
- `eval_*` 不 await promise。

### 已知问题 / 观察

1. 真 Electron 39.8.10（Windows）的观测，都在各 `expected.json` 里：首次 `show()` 前有一个 `move`；`minimize` 带 `move`；`restore`
   的 `focus` 发两次；`app.quit()` 的顺序是 `before-quit`、各窗口 `close`/`closed`、`will-quit`、进程 `exit`，**`quit` 事件在进程
   `exit` 之后才发**（所以记录器没记到它）；`app.quit()` 时没有 `window-all-closed`；`ipcMain.handle` 抛错时主进程还会
   `console.error("Error occurred in handler for 'fail': Error: boom …")`，渲染端收到的消息是
   `Error invoking remote method 'fail': Error: boom`；没有 CSP 的页面在开发模式下会向渲染端 console 写一条
   `Electron Security Warning`（`fixtures.rs` 过滤掉）；`ipc` 夹具退出得太快，没等到 `ready-to-show`。`events` 列表是**一次**运行的
   观测，跨进程的先后不保证稳定（多窗口的 `close` 顺序尤其），所以 `fixtures.rs` 只比较每个进程各自的 console 顺序。
2. P02 的缺口（被夹具碰到，非本包修）：解析期的 `<title>` 不会变成窗口标题（只有 JS 设置 `document.title` 才发 `Request::Title`）
   ——真 Electron 里页面标题会盖过 `title` 选项（文档说法，未录）；Verve 的 `process.versions.electron` 是 `39.2.0`，录制用的是 `39.8.10`。
3. 夹具里的 `quit()` 写法（`app ? app.quit() : process.exit(0)`）、`hello` 用 `data:` URL 代替 `loadFile`，都是为了在 P10/P20
   之前也能跑；P20 之后把 `hello` 改成 `win.loadFile('index.html')`。
4. 仓库的 `core.autocrlf=true`：工作区文件是 CRLF，`git` 会提示 LF→CRLF；对 `expected.json` 的 JSON 解析没有影响。

### 给其他包的备注

- **P10**：`App::launch` 解析入口的那一行（`entry_of`）等你的 `launcher::parse` 并入后改成调它；`--headless` 在 `electron.rs`
  只是去掉第一个参数并走 `headless::run`，并入 `Launch` 时请带上；`lifecycle`、`single-instance` 夹具与 `fixtures.rs` 里对应的
  两个 ignore 测试（已留好，写 `replays("lifecycle")`）归你；你的退出规则变化后，`tests/pipeline.rs` 与 `tests/headless.rs` 里
  "没有窗口就退出"的几个测试（`a_script_that_makes_no_window_ends_the_app_at_once` 等）要跟着改。若加 `AppEvent::Ready`，
  `HeadlessShell` 应在 `start_main` 之后发它（现在没有）。真 Electron 的 `quit` 事件晚于进程 `exit`。
- **P12**：`ipc` 夹具与 `expected.json` 已就绪，去掉 `ipc_messages_travel_both_ways` 的 ignore。`App::eval_renderer` 可以直接调
  页面里的 `ipcRenderer`（`nodeIntegration` 页面）。
- **P13**：窗口事件顺序有真实录制（`window-state`），`headless/window.rs` 的规则就是它；`multi-window` 夹具与
  `window-state` 测试归你。`'ready-to-show'` 在 Verve 里还没有，`hello` 用 `win.once('ready-to-show', () => win.show())` 兜住。
- **P20**：`hello`/`react-vite`/`protocol`/`quick-start` 需要你的页面加载器；`RendererGuest::click`/`type_text`/`eval` 与 `Layout`
  输入是我加的，你替换 `load` 时保留。`react-vite` 需要 `<script type="module">`。
- **P34**：`headless/window.rs` 是纯函数（`Window::apply(op, display) -> Vec<WindowEvent>`），建议把它的测试搬成对 `ShellPort` 的
  通用测试，用来跑真实 Shell；`Center`、`Destroy` 没有 Electron 观测。
- **P41**：`fixtures.rs` 的 `replays(name)` 就是矩阵的一行：跑夹具、逐进程比较 console 与退出码；`expected.json` 的格式是
  `{ electron, events[], console[{from,text}], exitCode }`，用 `record.cjs` 重录。
- **P30**：`FakeLayout::BlockStack` 的字符宽 8px、行高 16px 要和你的无头 `TextMeasurer`（每字 0.5em）对齐；入口只有
  `FakeLayout::boxes`，换成 `Engine` 时在它的 `match` 里加一支。

### 验证（本机，Windows，`CARGO_BUILD_JOBS=6`）

- `cargo test -p verve-electron`：通过（lib 17、`api`、`define`、`headless` 5、`jit`、`pipeline` 12、`fixtures` 1 通过 14 ignored）。
- `cargo tree -p verve-electron | grep -c gpui`：0。
- `cargo test -p verve-gpui`：通过（`electron_bin` 2 个开真实 GPUI 窗口，`electron_headless` 1 个）。
- `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check`、`bash scripts/style-check.sh`：通过。
- `bash scripts/linux-check.sh`（`x86_64-unknown-linux-gnu` 与 Zig 0.15.2 已装）：退出码 0。
