# P14 系统集成 A：dialog / shell / clipboard / nativeTheme / screen / Notification / powerMonitor / safeStorage / nativeImage

**依赖**：P02  **规模**：L  **修改**：`crates/verve-electron/src/api/{dialog,shell,clipboard,native_theme,screen,notification,power_monitor,power_save_blocker,safe_storage,native_image,system_preferences}.rs`；`crates/verve-gpui/src/shell.rs`（执行对话框、读显示器）；`crates/verve-electron/src/msg.rs`（只追加变体）；可选：`src/crypto.rs`（仅把 AES-CBC/GCM 辅助函数改为 `pub(crate)` 并经 `verve::crypto_primitives` 公开，见第 8 节）

## 目标

主进程 `require('electron')` 上的这些模块与 Electron 39 行为一致：

| 模块 | 范围 |
|---|---|
| `dialog` | `showOpenDialog`/`showOpenDialogSync`、`showSaveDialog`/`showSaveDialogSync`、`showMessageBox`/`showMessageBoxSync`、`showErrorBox`、`showCertificateTrustDialog`（抛 `ERR_NOT_SUPPORTED`） |
| `shell` | `openExternal`、`openPath`、`showItemInFolder`、`trashItem`、`beep`、`writeShortcutLink`/`readShortcutLink`（仅 Windows） |
| `clipboard` | `readText`/`writeText`、`readHTML`/`writeHTML`、`readRTF`/`writeRTF`、`readImage`/`writeImage`、`readBookmark`/`writeBookmark`、`readFindText`/`writeFindText`（macOS）、`readBuffer`/`writeBuffer`、`availableFormats`、`has`、`clear`、`write({text,html,image,rtf,bookmark})`、`type` 参数 `'selection'`（Linux） |
| `nativeTheme` | `shouldUseDarkColors`、`themeSource`（`system`/`light`/`dark`，写入后影响所有渲染器的 `prefers-color-scheme`）、`shouldUseHighContrastColors`、`shouldUseInvertedColorScheme`、`inForcedColorsMode`、`prefersReducedTransparency`、`'updated'` 事件 |
| `screen` | `getPrimaryDisplay`、`getAllDisplays`、`getDisplayNearestPoint`、`getDisplayMatching`、`getCursorScreenPoint`、`screenToDipPoint`/`dipToScreenPoint`/`screenToDipRect`/`dipToScreenRect`（Windows）、事件 `display-added`/`display-removed`/`display-metrics-changed` |
| `Notification` | `new Notification({title,subtitle,body,silent,icon,urgency,timeoutType,actions,closeButtonText,toastXml})`、`show`、`close`、`Notification.isSupported()`、事件 `show`/`click`/`close`/`reply`/`action`/`failed` |
| `powerMonitor` | `getSystemIdleState(threshold)`、`getSystemIdleTime`、`getCurrentThermalState`、`isOnBatteryPower`/`onBatteryPower`、事件 `suspend`/`resume`/`on-ac`/`on-battery`/`lock-screen`/`unlock-screen`/`shutdown`/`user-did-become-active`/`user-did-resign-active` |
| `powerSaveBlocker` | `start(type)`、`stop(id)`、`isStarted(id)` |
| `safeStorage` | `isEncryptionAvailable`、`encryptString`、`decryptString`、`setUsePlainTextEncryption`、`getSelectedStorageBackend`（Linux） |
| `nativeImage` / `NativeImage` | `createEmpty`、`createFromPath`、`createFromBuffer`、`createFromDataURL`、`createFromBitmap`、`createFromNamedImage`（macOS，其余返回空图）、`createThumbnailFromPath`；实例 `toPNG`、`toJPEG`、`toBitmap`、`getBitmap`、`toDataURL`、`getSize`、`getAspectRatio`、`getScaleFactors`、`isEmpty`、`resize`、`crop`、`addRepresentation`、`setTemplateImage`/`isTemplateImage`、`isMacTemplateImage` |
| `systemPreferences` | `getAccentColor`、`getColor`（Windows 系统色）、`isAeroGlassEnabled`、`getAnimationSettings`、`getMediaAccessStatus`（`granted`）、`askForMediaAccess`（resolve `true`）、macOS 专有方法存在且返回默认值 |
| `app.getFileIcon(path, {size})` | 返回 `NativeImage`；放在本包实现，由 P10 的 `app` 对象挂上 |

渲染器侧同名模块（`clipboard` 在 Electron 39 的沙箱渲染器里已不可用，非沙箱仍可用）：`clipboard` 和
`nativeImage` 在 `nodeIntegration: true`、`sandbox: false` 时也在渲染器版 `electron` 模块里出现，
实现共用同一套 Rust 代码（不经过主进程）。

## 背景（先读）

- `docs/electron/README.md` 第 2、3 节；`P02` 的线程模型与"同步规则"。
- `src/native.rs`：`class`、`function`、`closure`、`object`、`ClassSpec`——所有 JS 对象的做法。
- `src/sockets.rs` 的 `emitter_class`/`define`：原生类继承 `EventEmitter` 的现成写法（`Notification`、
  `nativeTheme`、`screen`、`powerMonitor` 都是 EventEmitter）。
- `src/buffer.rs`：返回 `Buffer` 的方式（`toPNG`、`readBuffer`、`encryptString` 都返回 Buffer）。
- `src/permission.rs` 的 `Permissions`：Electron 主进程默认 `Permissions::all()`（P10 设置），本包不再加权限检查。
- `crates/verve-gpui/src/shell.rs`（P02 产出）：Shell 处理 `ToShell` 的主循环。

## 选型与体积

| 用途 | crate | 理由 / 体积控制 |
|---|---|---|
| 文件/消息对话框 | `rfd`（`default-features = false`，Windows 用 `common-controls-v6`；Linux 用 `xdg-portal` 而非 `gtk3`） | 三平台原生对话框，Windows 上直接 COM `IFileDialog`/`TaskDialogIndirect`，增量 < 300 KB；Linux 走 portal 不链 GTK |
| 剪贴板 | `arboard`（features：`image-data`，关闭 `wayland-data-control` 外的可选项按平台开） | 支持文本、HTML、图片；GPUI 的剪贴板只在 Shell 线程可用，而 `clipboard.*Sync` 语义要在主进程线程上直接返回，所以不走 GPUI。RTF/书签/任意格式 `readBuffer`/`writeBuffer` 用 `windows-sys`（`Win32_System_DataExchange`，仓库已依赖 `windows-sys`）和 macOS `NSPasteboard`（`objc2-app-kit`，GPUI 已带入）补齐 |
| 打开外部链接/文件 | 自写：Windows `ShellExecuteW`（`windows-sys` `Win32_UI_Shell`）；macOS `NSWorkspace`；Linux `xdg-open`/portal `OpenURI`（经 P17 的进程启动，或直接 `std::process::Command`） | 不引入 `open` crate，几十行代码 |
| 回收站 | Windows `IFileOperation`（`FOF_ALLOWUNDO`），macOS `NSFileManager trashItemAtURL`，Linux freedesktop Trash 规范自写 | 不引入 `trash` crate（它拉 `chrono` 等，增量偏大）；若实现成本过高可改用 `trash`（`default-features = false`），PR 中写明体积 |
| 通知 | Windows：`windows-sys` 不覆盖 WinRT，使用 `windows` crate 的 `UI_Notifications` 仅此一个 feature；macOS：`UNUserNotificationCenter`（`objc2-user-notifications`）；Linux：D-Bus `org.freedesktop.Notifications`，用 `zbus`（`default-features=false`, `blocking`）——**仅 Linux 目标依赖** | Windows `windows` crate 只开一个 feature 时增量约 200–400 KB；不用 `notify-rust`（它在 Windows 上走 `winrt-notification`+`xml` 链，更大） |
| 图片编解码 | `image`（`default-features = false`, features `png`, `jpeg`, `ico`, `bmp`） | `nativeImage` 需要 PNG/JPEG 读写、ICO/BMP 读；不开 gif/webp/tiff/avif。P25 的图片解码复用同一依赖，**两包共用一份 features 列表**，在根 workspace `[workspace.dependencies]` 里声明 |
| safeStorage | Windows `CryptProtectData`/`CryptUnprotectData`（`windows-sys` `Win32_Security_Cryptography`）；macOS Keychain（`security-framework`，`default-features=false`）；Linux Secret Service（`zbus`）。对称加密全部用仓库已有的 `aes`、`ghash`、`pbkdf2`、`sha1`（`src/crypto.rs` 已有 GCM/CBC 实现，抽成 `pub(crate)` 供复用即可——这是对 `src/` 的唯一改动，若不想动内核就在本 crate 里用 `aes` 自写 CBC/GCM） | **必须与 Chromium `OSCrypt` 格式逐字节兼容**，这样用户把 Electron 换成 Verve 后，应用以前用 safeStorage 存的密文仍能解开。格式见下方第 8 节 |
| 空闲时间 / 电源 | Windows `GetLastInputInfo`、`GetSystemPowerStatus`、`SetThreadExecutionState`、`WM_POWERBROADCAST`/`WTSRegisterSessionNotification`（Shell 窗口过程收到后发消息）；macOS IOKit；Linux logind D-Bus | 全部自写 FFI，不引入新 crate（Linux 复用 `zbus`） |

**体积预算**：本包合计 ≤ 1.2 MB（Windows x64 release）。超出时在 PR 里列出每个依赖的增量（`scripts/size-report`，P01）。

## 接口契约

### 1. 哪些事必须在 Shell（OS 主线程）上做

| 调用 | 在哪执行 | 原因 |
|---|---|---|
| 所有对话框 | Shell | Windows 对话框需要拥有窗口（`HWND`）且模态于它；macOS `NSOpenPanel` 必须在主线程；GPUI 的消息循环在主线程 |
| `shell.openExternal` 等、`clipboard.*`、`safeStorage`、`nativeImage`、`powerMonitor` 查询 | 主进程线程直接调用 | 无主线程要求（Windows 剪贴板 `OpenClipboard(NULL)` 可在任意线程；macOS `NSPasteboard` 线程安全） |
| `screen.*` | 主进程读缓存 | 显示器数据来自 Shell 发来的 `FromShell::Displays`，主进程保存最新一份；`getCursorScreenPoint` 直接调用 OS（`GetCursorPos`/`NSEvent.mouseLocation`），不经 Shell |
| `Notification.show` | Windows/Linux 主进程线程直接调；macOS 经 Shell | macOS `UNUserNotificationCenter` 回调在主线程 |
| `powerMonitor` 事件、`nativeTheme` 系统变化 | Shell 侦测，发消息 | 来自窗口消息 / 系统通知中心 |

### 2. 追加到 `msg.rs` 的消息（只追加）

```rust
// ToShell 追加
ToShell::Dialog { call: u64, parent: Option<WindowId>, spec: DialogSpec, reply: DialogReply },
ToShell::Notification { id: u32, op: NotificationOp },          // 仅 macOS 走这里
ToShell::SetThemeSource(ThemeSource),                             // 让 Shell 给窗口切换亮暗标题栏

pub enum DialogSpec {
    Open { title: Option<String>, default_path: Option<PathBuf>, button_label: Option<String>,
           filters: Vec<FileFilter>, properties: OpenProps, message: Option<String> },
    Save { title: Option<String>, default_path: Option<PathBuf>, button_label: Option<String>,
           filters: Vec<FileFilter>, name_field_label: Option<String>, shows_tag_field: bool,
           properties: SaveProps },
    MessageBox { kind: MessageKind, buttons: Vec<String>, default_id: Option<usize>,
                 cancel_id: Option<usize>, title: String, message: String, detail: String,
                 checkbox_label: Option<String>, checkbox_checked: bool, icon: Option<PathBuf>,
                 no_link: bool, normalize_access_keys: bool },
    ErrorBox { title: String, content: String },
}
pub struct FileFilter { pub name: String, pub extensions: Vec<String> }   // ['*'] = 所有文件
pub struct OpenProps { pub open_file: bool, pub open_directory: bool, pub multi_selections: bool,
                       pub show_hidden_files: bool, pub create_directory: bool,
                       pub prompt_to_create: bool, pub no_resolve_aliases: bool,
                       pub treat_package_as_directory: bool, pub dont_add_to_recent: bool }
pub struct SaveProps { pub show_hidden_files: bool, pub create_directory: bool,
                       pub treat_package_as_directory: bool, pub show_overwrite_confirmation: bool,
                       pub dont_add_to_recent: bool }
pub enum MessageKind { None, Info, Error, Question, Warning }

/// 答复回到哪里：异步版走主进程消息队列；同步版走一次性通道（见第 3 节）。
pub enum DialogReply { Async, Sync(std::sync::mpsc::SyncSender<DialogResult>) }

// FromShell 追加
FromShell::DialogResult { call: u64, result: DialogResult },
FromShell::Theme { dark: bool, high_contrast: bool, inverted: bool, forced_colors: bool,
                   reduced_transparency: bool, accent: [u8; 4] },
FromShell::Power(PowerEvent),                                    // suspend/resume/on-ac/...
FromShell::NotificationEvent { id: u32, event: NotificationEvent },

pub enum DialogResult {
    Open { canceled: bool, file_paths: Vec<PathBuf>, bookmarks: Vec<String> },
    Save { canceled: bool, file_path: Option<PathBuf>, bookmark: Option<String> },
    MessageBox { response: usize, checkbox_checked: bool },
    Closed,                                                       // 父窗口被销毁等
}
```

`AppEvent::ThemeChanged`（P02 已有）由 `FromShell::Theme` 取代为主；保留 `ThemeChanged` 以兼容 P02 的骨架代码，
本包把两者都转成 `nativeTheme` 的 `'updated'`。

### 3. 同步对话框的取舍（必须照此实现）

Electron 的 `showOpenDialogSync`/`showMessageBoxSync` 会阻塞主进程 JS 直到用户关掉对话框。本方案：

- **允许阻塞主进程线程**（这是 API 语义，应用就是要停在这里），**但绝不阻塞 Shell**：
  主进程发 `ToShell::Dialog { reply: DialogReply::Sync(tx) }` 后在 `rx.recv()` 上等。
- 这是 P02 同步规则 1（"主进程不得阻塞等待其他线程"）的**唯一明确例外**，在 P02 文档"同步规则"下方追加一条注明
  （本包 PR 里单独一个 docs commit）。
- 安全条件：Shell 弹对话框时不得反过来等主进程（Shell 本来就不等 JS），所以不会死锁。
  阻塞期间渲染器照常运行；渲染器的 `sendSync` 会排队等主进程，这与 Electron 相同。
- 阻塞期间主进程收到的 `FromShell`/`FromRenderer` 消息照常进队列，对话框返回后再处理。
- Shell 收到 `Sync` 请求时若对应父窗口已销毁或 Shell 正在退出，必须发送 `DialogResult::Closed`，主进程把它当作 canceled。
- 实现方式：Shell 在 OS 主线程调用 rfd 的**异步** API（`AsyncFileDialog`/`AsyncMessageDialog`），用 GPUI 的
  `cx.spawn` 等待 future，不在 Shell 上阻塞 GPUI 消息循环；拿到结果后发 `FromShell::DialogResult` 或写 `tx`。
- `showErrorBox` 在 `app` ready 之前调用时（Electron 允许），Shell 可能还没起来：此时直接在主进程线程用
  rfd 的同步 `MessageDialog` 弹出（Windows `MessageBoxW` 无主线程要求；macOS 上改为写 stderr）。

### 4. 父窗口与模态

`dialog.showOpenDialog(win, opts)`：`win` 是 `BrowserWindow` 时取其 `WindowId`，Shell 用该 GPUI 窗口的
原生句柄做 rfd 的 `set_parent`（GPUI `Window` 实现 `raw_window_handle::HasWindowHandle`，P34 在 Shell 里提供
`fn native_handle(WindowId) -> Option<RawWindowHandle>`，本包调用它；若 P34 尚未合并，本包先在 shell.rs 里写
一个同签名的私有函数，P34 合并时替换）。参数 `win` 省略时无父窗口。参数形态 `(options)` 与 `(window, options)`
都要识别（Electron 以第一个参数是否为 `BrowserWindow` 判断）。

### 5. nativeTheme 与渲染器

`nativeTheme.themeSource = 'dark'` 后：主进程更新自己的状态 → 发 `ToShell::SetThemeSource` → 经 Hub 向每个渲染器发
`ToRendererInput::ColorScheme { dark }`（P02 已定义），渲染器 `Document::set_color_scheme`。`shouldUseDarkColors`
= `themeSource` 为 `system` 时取系统值，否则取强制值。新建渲染器时 Hub 用当前值初始化（`verve_dom::Options::dark`）。

### 6. NativeImage 的内部表示

```rust
pub struct NativeImage { reps: Vec<Representation>, template: bool }   // 原生 class 的 data
pub struct Representation { scale: f32, width: u32, height: u32, rgba: Arc<[u8]> }   // 非预乘 RGBA8
```

`toBitmap()`/`getBitmap()` 按平台返回 BGRA（Electron 行为：Windows/Linux BGRA，macOS RGBA）。
`createFromPath` 识别 `@2x`/`@3x` 后缀并自动加载同名多倍图。`NativeImage` 跨线程（主进程 → Shell，用于 Tray/
窗口图标/通知/对话框图标）以 `Arc<NativeImage>` 传递，**不要**经过 `Cloned`；IPC 里传 NativeImage 时，
Electron 会序列化成 `{ ... }` 普通对象——本包不处理，P12 遇到时按 Electron 行为输出空对象。

### 7. 错误与平台差异

- 平台专有 API 在其他平台：方法存在，返回 Electron 文档规定的默认值（如 `readShortcutLink` 非 Windows 抛
  `Error`，与 Electron 一致；`getAccentColor` 在 Linux 返回 `''`）。逐个对照 Electron 文档的 "Platform" 标注。
- `shell.openExternal` 返回 Promise，失败 reject `Error('Failed to open: …')`；`openPath` resolve 错误字符串
  （成功为 `''`）——Electron 的怪行为，照抄。

### 8. safeStorage 的密文格式（Chromium `OSCrypt`，Electron 39 所用）

| 平台 | 主密钥 | 密文 |
|---|---|---|
| Windows | `<userData>/Local State` JSON 的 `os_crypt.encrypted_key`：base64 解码后是 `"DPAPI"` 前缀 + DPAPI blob，`CryptUnprotectData` 得 32 字节密钥；文件或字段不存在时生成随机密钥并按同格式写回（保留文件中其他字段） | `"v10"` + 12 字节 nonce + AES-256-GCM 密文 + 16 字节 tag |
| macOS | Keychain 通用密码，service `"<app.getName()> Safe Storage"`，account `"<app.getName()>"`；不存在时生成 16 字节随机串 base64 后写入 | 密钥 = PBKDF2-HMAC-SHA1(password, salt `"saltysalt"`, 1003 次, 16 字节)；`"v10"` + AES-128-CBC（IV 为 16 个空格 `0x20`，PKCS#7） |
| Linux | Secret Service 可用：钥匙串里 `application=<app name>` 的密码 → `"v11"`；不可用：固定口令 `"peanuts"` → `"v10"`，此时 `getSelectedStorageBackend()` 返回 `'basic_text'`，`isEncryptionAvailable()` 仍为 `true`（与 Electron 一致） | 密钥 = PBKDF2-HMAC-SHA1(password, `"saltysalt"`, 1 次, 16 字节)；AES-128-CBC，IV 同上 |

实现前用真实 Electron 39 在同一台机器、同一 `userData` 下加密几段文本，把密文和 `Local State` 冻结成测试样本
（Windows 的 DPAPI 样本只在同一用户下可解，测试标 `#[ignore]` 并写明准备方法；格式解析部分用非 DPAPI 的单元测试覆盖）。
`app.getName()` 与 `userData` 来自 P10；P10 未合并时从 `ElectronOptions` 读取。

## 步骤

1. `[workspace.dependencies]` 加 `image`、`rfd`、`arboard`（及平台依赖），写好每行用途注释。
2. `nativeImage`（其他模块都要用它）+ 测试：PNG/JPEG 往返、`@2x` 加载、`resize`/`crop`、`toDataURL`。
3. `clipboard` 全部方法；Windows 上 HTML 用 `CF_HTML` 格式（含 `Version:0.9` 头与偏移量），RTF 用注册格式
   `Rich Text Format`，书签用 `UniformResourceLocatorW`；`readBuffer(format)` 按名注册剪贴板格式。
4. `shell`、`safeStorage`、`powerSaveBlocker`、`powerMonitor` 查询类方法、`systemPreferences`、`app.getFileIcon`
   （Windows `SHGetFileInfoW` → HICON → RGBA）。
5. 消息追加；Shell 侧实现对话框执行、显示器枚举（启动时与 `WM_DISPLAYCHANGE` 时发 `FromShell::Displays`）、
   主题/电源事件侦测。
6. `dialog` 异步与同步版本；`screen`；`nativeTheme`；`Notification`；事件分发。
7. 渲染器版 `electron` 模块挂上 `clipboard`/`nativeImage`（按 webPreferences 决定，P21 会细化暴露规则）。
8. P02 文档追加同步对话框例外说明（单独 docs commit）。

## 验收

- `cargo test -p verve-electron --test system_a`（无头）：
  - `nativeImage` 往返、尺寸、dataURL 与真实 Electron 冻结样本一致（PNG 字节可不同，像素必须一致）。
  - `clipboard.writeText/readText`、`writeHTML/readHTML`、`writeImage/readImage` 往返（Windows CI 上真实剪贴板；
    无剪贴板环境标 `#[ignore]`）。
  - `safeStorage.encryptString` → `decryptString` 往返；**解密真实 Electron 39 在同一用户下加密的冻结样本**（Windows）。
  - 无头 Shell（P00）收到 `ToShell::Dialog`，模拟回复后 `showOpenDialog` resolve 正确对象；`showMessageBoxSync`
    在无头 Shell 回复前阻塞、回复后返回 `response`，其间渲染器定时器继续运行（证明 Shell/渲染器未被阻塞）。
  - `nativeTheme.themeSource='dark'` 后渲染器里 `matchMedia('(prefers-color-scheme: dark)').matches === true`，
    并触发 `'updated'`。
  - `screen.getPrimaryDisplay()` 返回无头 Shell 注入的显示器；`display-added` 事件。
- 手动（GUI，Windows）：`tests/electron/apps/system-a/`（本包新增夹具）里每个按钮依次弹出打开/保存/消息对话框、
  通知、`openExternal` 打开浏览器、`showItemInFolder` 打开资源管理器并选中文件、`trashItem` 进回收站。
- `scripts/size-report` 显示本包增量 ≤ 1.2 MB。

## 不做

`desktopCapturer`、`systemPreferences` 的 macOS 用户默认值读写（`getUserDefault` 返回 `undefined`）、
`shell.openExternal` 的 `activate`/`workingDirectory` 以外的选项、`Notification` 的 `toastXml` 自定义模板之外的
Windows 交互式 toast 激活器（COM 激活器注册，留到 P41 发现真实需求再做）、`dialog` 的 `securityScopedBookmarks`（返回空）。

## 状态

（实现者完成后填写）
