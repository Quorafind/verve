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

分支 `electron/p14-system-a`，基于 `docs/electron-plan` `125d2d8c`。Windows 全部实现并实测；macOS、Linux 只写了 `cfg` 分支，没有在这两个平台上跑过。

### 完成了什么

| 模块 | 状态 | 实现 |
|---|---|---|
| `nativeImage` / `NativeImage` | 完成 | `src/pixels.rs`（纯 Rust：表示、缩放因子选择、非预乘 RGBA 与 Chromium 预乘 BGRA 位图互转、PNG/JPEG 编码、`resize`/`crop`）、`api/native_image.rs`（对象）。`createFromPath` 依 Chromium 规则读 `@2x`；`toBitmap` 在 Windows/Linux 是预乘 BGRA（macOS RGBA）。`createFromNamedImage` 返回空图，`createThumbnailFromPath` 拒绝（`ERR_NOT_SUPPORTED`） |
| `clipboard` | 完成（Windows） | `os/clipboard.rs`（接口）、`os/clipboard_windows.rs`（`windows-sys` 直接调用，格式同 Chromium：`CF_UNICODETEXT`、`HTML Format`（带偏移头）、`Rich Text Format`、`PNG`、`UniformResourceLocatorW`、自定义格式名）。剪贴板用一个 message-only 窗口当所有者（`OpenClipboard(NULL)` 后 `SetClipboardData` 会间歇失败，实测出现过）。**未用 `arboard`**。macOS/Linux：`clipboard_memory.rs`，只在进程内保存，没有系统剪贴板 |
| `dialog` | 完成（消息与无头路径）；真实 Shell 受 GPUI 限制，见下 | `api/dialog.rs`：选项校验和报错文字逐字取自 Electron 39 内嵌的 `lib/browser/api/dialog.ts`（从 `electron.exe` 里取出核对）；`show*Dialog`/`showMessageBox` 返回 Promise，`show*Sync` 在主进程线程上等一次性通道（P02 同步规则 1 的例外，已在 P02 文档里写明条件）。无头 Shell：`HeadlessShell::queue_dialog_answer`/`hold_dialogs`/`dialogs`/`answer_dialog`，默认答复是取消 / 默认按钮。`showErrorBox` 发 `ToShell::ErrorBox`；`showCertificateTrustDialog` 校验参数后拒绝 `ERR_NOT_SUPPORTED` |
| `screen` | 完成 | `api/screen.rs`；Hub 保存最后一次 `FromShell::Displays`（`Hub::displays`），所以 `ready` 处理函数里就能 `getPrimaryDisplay()`（解决 P00 记的“启动时发出的显示器丢失”）；`display-added`/`display-removed`/`display-metrics-changed` 由前后两次报告的差异产生；无头 Shell 的 `set_displays` 注入并触发。`getCursorScreenPoint` 直接调 `GetCursorPos`。`dipToScreen*`/`screenToDip*` 只按所在显示器的缩放因子换算（没有物理原点，多显示器混合缩放时不精确） |
| `nativeTheme` | 完成 | `api/native_theme.rs` + Hub：`Hub::set_theme_source` 给每个渲染器发 `ColorScheme`、给 Shell 发 `SetThemeSource`；`Hub::dark()` 是新渲染器 isolate（含 P20 每次导航新建的）初始 `prefers-color-scheme` 的来源（`RendererGuest::new` 读它）；Shell 报告的系统主题经 `Hub::input` 换成“强制值或系统值”。`'updated'` 在 `shouldUseDarkColors` 实际变化时触发一次（实测 Electron：`dark`→`light`→`system` 共 2 次）。高对比度、减少透明度读系统（`SystemParametersInfoW`、注册表）。Shell 侧不改窗口标题栏明暗（`SetThemeSource` 目前被忽略） |
| `shell` | 大部分 | `openExternal`（`ShellExecuteW`）、`openPath`、`showItemInFolder`（`explorer /select`）、`trashItem`（`SHFileOperationW` + `FOF_ALLOWUNDO`；相对路径一律拒绝为 `Failed to parse path`，因为回收站只认完整路径，否则会直接永久删除）、`beep`。`writeShortcutLink`/`readShortcutLink` **未做**，调用抛 `ERR_NOT_SUPPORTED` |
| `safeStorage` | 完成（Windows） | `src/oscrypt.rs`（格式：`v10` + nonce + AES-256-GCM + tag；`Local State` 里 `os_crypt.encrypted_key` = base64(`DPAPI` + DPAPI blob)，其他字段保留）、`api/safe_storage.rs`、`os::dpapi_*`。已用真实 Electron 39 做的样本验证能解开（见下）。macOS/Linux 返回“不可用”，**未实现** Keychain / Secret Service / `peanuts` |
| `powerSaveBlocker` | 完成 | `SetThreadExecutionState`（主进程线程，所有 blocker 汇总） |
| `powerMonitor` | 只有查询 | `getSystemIdleState`/`getSystemIdleTime`/`getCurrentThermalState`/`isOnBatteryPower`/`onBatteryPower`；**事件（`suspend`/`resume`/`lock-screen`…）未做**（需要 Shell 的 `WM_POWERBROADCAST`） |
| `systemPreferences` | Windows 形态 | 实测 Electron 39 的 Windows 版只有 `getAccentColor`、`getColor`、`getAnimationSettings`、`getMediaAccessStatus`，按此实现；任务书说的 macOS 方法/`askForMediaAccess`/`isAeroGlassEnabled` 在真实 Electron 的 Windows 版里是 `undefined`，所以**不定义** |
| `Notification` | 占位 | 类存在，`isSupported()` 为 `false`，`show()` 触发 `'failed'`。系统 toast **未做** |
| `app.getFileIcon` | **未做** | |
| 渲染器版 `electron` | 完成 | `nodeIntegration: true` 且 `sandbox: false` 的页面 `require('electron')` 有 `clipboard`、`nativeImage`（`api::install_page` 里一个分支）。P21 复用 `api::native_image::make(context)`（造出 `nativeImage` 模块对象），`api::clipboard::make` 同理 |

真实 Shell（`crates/verve-gpui/src/dialog.rs`）用 GPUI 自带的平台提示：`App::prompt_for_paths`/`prompt_for_new_path` 和 `Window::prompt`，没有链接 `rfd`。结果是零体积，不阻塞 GPUI 循环，但 GPUI 的接口不带：文件过滤器、标题、起始目录（保存框只用 `defaultPath` 的目录和文件名）、消息框的标题/复选框/图标。复选框的结果原样返回传入值。消息框需要一个窗口（`parent` 或任意一个窗口），一个窗口都没有时按“关闭”答复，主进程当作取消。这些是 GPUI 接口的限制；要补齐需要 `rfd` 或直接用 `IFileDialog`/`TaskDialogIndirect`，在 P41 发现真实需求后再做。`ErrorBox` 画在任意一个窗口上，没有窗口时（`ready` 之前、加载出错）由主进程线程调用 `Hub::error_box`：真实 Shell 允许时（`Hub::allow_system_boxes`，`verve_gpui::run_with` 在环境变量 `VERVE_NO_SYSTEM_DIALOGS` 未设置时调用）Windows 用 `MessageBoxW(NULL, …)` 在主进程线程上弹（同 Electron，不进 GPUI 循环），其他平台只有标准错误；无头 Shell 从不允许，所以自动化测试不会弹出真实对话框，GPUI 的二进制测试设置 `VERVE_NO_SYSTEM_DIALOGS=1`。`showErrorBox` 同时写标准错误。父窗口已不在时按“关闭”答复。

### 没做的

`shell.writeShortcutLink`/`readShortcutLink`、`app.getFileIcon`、`nativeImage.createThumbnailFromPath`、`Notification` 的系统 toast、`powerMonitor` 事件、`safeStorage` 的 macOS/Linux、`clipboard` 的 macOS/Linux 系统剪贴板、`FromShell::Theme` 的高对比度等字段（主进程直接读系统，所以没加这个变体；P02 表里写的 `FromShell::Theme(ThemeState)` 和 `ThemeState` 没有加）、`ToShell::Notification`、`securityScopedBookmarks`。`dialog` 的 `icon` 只认路径字符串，不认 `NativeImage`。

### 冻结的 Electron 观测（Electron 39.8.10，Windows）

探针在 `tests/electron/system-a/observe/`（`run.cjs` 起 Electron，临时 profile，`show: false`，不开对话框/通知/浏览器），输出在 `tests/electron/system-a/expected/`：`native-image.json`（位图字节序与预乘、`@2x`、`crop`/`resize` 尺寸、空图、错误文字）、`system.json`（显示器字段、`nativeTheme` 默认值与事件次数、`powerMonitor`/`powerSaveBlocker`/`systemPreferences` 形态、`clipboard.availableFormats()` 每次写入后的结果、`shell` 错误文字）、`safe-storage.json` 与 `tests/electron/system-a/safe-storage/`（`Local State` 和密文样本）。几个值得注意的观测：`clipboard.write*` 每次写入替换全部格式；`writeBookmark` 后 `availableFormats()` 为空而 `readBookmark().url` 有值；`has('text/plain')` 是 `false`（`has` 按注册格式名查）；Electron 的 `safeStorage.decryptString(Buffer.from('v10abc'))` 会让进程崩溃（`out_of_range`），Verve 抛普通错误；`safeStorage.getSelectedStorageBackend` 在 Windows 上不是函数。

### 改了的现有测试

`tests/fixtures.rs` 的 `dialogs_answer_from_the_shells_presets` 取消了 `#[ignore]`，并断言四行输出（`message box 0 false` 等）。没有其他现有测试被改。

### 手动检查清单（给人跑，没有在自动测试里做；自动测试从不打开真实对话框、通知、浏览器或资源管理器）

`electron tests/electron/apps/system-a`，依次按钮：

1. `open-file`、`open-directory`、`save-file`：弹出系统打开/保存框，取消和选择两种答复都看一遍（过滤器和标题不会生效，见上）。
2. `message-box`、`message-box-sync`：消息框带按钮；同步版期间页面不卡（改窗口大小、`theme` 按钮之外的页面动画仍运行）。
3. `error-box`：错误框。
4. `notification`：应该返回 `supported: false`（占位）。
5. `open-external`：默认浏览器打开 example.com。
6. `show-item`：资源管理器打开并选中 `main.js`。
7. `trash-item`：临时文件进回收站。
8. `clipboard`：写入后回读，剪贴板文字恢复为原来的。
9. `theme`：切换亮暗，页面背景/`prefers-color-scheme` 随之变化。
10. `screen`、`power-save`、`safe-storage`：返回值合理。

本包没有在真实 GPUI Shell 上看过这些按钮（只编译了 `verve-gpui`），上表“真实 Shell”一栏是未验证的。

### 已知问题

1. 真实 Shell 的对话框受 GPUI 接口限制（见上）。
2. `dipToScreen*` 在多显示器混合缩放时不精确。
3. `Notification.isSupported()` 与真实 Electron（`true`）不同，是有意的。
4. `trashItem` 在路径存在但回收失败时的错误文字是 Verve 自己的（`Failed to move <path> to the trash`），没有观测。
5. 非 Windows 的 `os/other.rs`、`clipboard_memory.rs` 没有在对应平台上编译过。

### 体积

见 `docs/electron/size.md` 的 P14 一节。

### 给其他包的备注

- **P13**：`browser_window::window_id_of(value, context)`（`define::id_of` 的一行包装）是 `dialog` 区分 `BrowserWindow` 与选项对象的唯一入口；签名带 `context`，因为 `define` 的对象表在 realm 里。请保留。`BrowserWindow` 的 `icon` 选项不由本包处理。
- **P15**：托盘/菜单图标用 `pixels::NativeImage`（`Arc` 传给 Shell）；`api::native_image::image_of(value)` 从 JS 值取出一份拷贝。
- **P21**：沙箱 preload 的 `electron` 白名单要 `nativeImage` 时调用 `api::native_image::native_image_module(context)`。
- **P34**：`ToShell::Dialog`/`ErrorBox` 在 `shell.rs` 里只有分发，逻辑在 `dialog.rs`；`ToShell::SetThemeSource` 目前被忽略，若要画亮暗标题栏在这里接。真实 Shell 要补文件过滤器/标题/复选框时在 `dialog.rs` 里换 `rfd` 或 `IFileDialog`。
- **P41**：`Notification`、`shell.*ShortcutLink`、`app.getFileIcon`、`powerMonitor` 事件是尚未做的，发现真实需求时再做。

### 动了 P10 的文件

`api/app/platform.rs` 的 `read_dword` 由私有改为 `pub(crate)`（`os/windows.rs` 读注册表要用）；`verve-dom` 的 `canvas::{premultiply, unpremultiply}` 改为公开，`nativeImage` 与画布共用同一套预乘算法。
