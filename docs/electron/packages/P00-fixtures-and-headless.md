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

（实现者完成后填写）
