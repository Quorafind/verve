# P15 Menu / MenuItem / Tray / globalShortcut

**依赖**：P02、P34（Shell 窗口与原生句柄）、P14（`NativeImage`）  **规模**：M  **修改**：`crates/verve-electron/src/api/{menu,menu_item,tray,global_shortcut,accelerator}.rs`；`crates/verve-gpui/src/{menu.rs,tray.rs,shortcut.rs}`（新建）；`crates/verve-electron/src/msg/shell.rs`（在 `MenuOp` 上追加）

## 目标

| 模块 | 范围 |
|---|---|
| `Menu` | `new Menu()`、`Menu.buildFromTemplate(template)`、`Menu.setApplicationMenu(menu \| null)`、`Menu.getApplicationMenu()`、`Menu.sendActionToFirstResponder`（macOS，其他平台无操作）；实例 `popup({window,x,y,positioningItem,sourceType,callback})`、`closePopup(window)`、`append`、`insert`、`getMenuItemById`、`items`；事件 `menu-will-show`、`menu-will-close` |
| `MenuItem` | 构造选项 `click`、`role`、`type`（`normal`/`separator`/`submenu`/`checkbox`/`radio`/`header`/`palette`）、`label`、`sublabel`、`toolTip`、`accelerator`、`icon`、`enabled`、`acceleratorWorksWhenHidden`、`visible`、`checked`、`registerAccelerator`、`sharingItem`、`submenu`、`id`、`before`/`after`/`beforeGroupContaining`/`afterGroupContaining`；实例属性可写并**即时反映到原生菜单**（`enabled`、`visible`、`checked`、`label`） |
| `BrowserWindow` 上的菜单方法 | `setMenu(menu \| null)`、`removeMenu()`、`setAutoHideMenuBar`、`isMenuBarAutoHide`、`setMenuBarVisibility`、`isMenuBarVisible`（P13 调用本包的实现） |
| `Tray` | `new Tray(image, guid?)`；`destroy`、`setImage`、`setPressedImage`、`setToolTip`、`setTitle`/`getTitle`（macOS）、`setIgnoreDoubleClickEvents`、`displayBalloon`/`removeBalloon`/`focus`（Windows）、`popUpContextMenu`、`closeContextMenu`、`setContextMenu`、`getBounds`、`isDestroyed`；事件 `click`、`right-click`、`double-click`、`middle-click`、`balloon-show`/`balloon-click`/`balloon-closed`、`mouse-enter`/`mouse-leave`/`mouse-move`、`drop-files`（macOS） |
| `globalShortcut` | `register(accelerator, cb)`、`registerAll`、`isRegistered`、`unregister`、`unregisterAll` |
| 渲染器内快捷键 | 应用菜单/窗口菜单项的 `accelerator` 在**窗口获得焦点**时生效（Electron 行为），即使菜单栏隐藏；`webContents` 的 `before-input-event` 先于菜单快捷键（P13 发事件，本包查表） |

## 背景（先读）

- `P02` 的 `ToShell::Menu(MenuOp)` 占位、线程模型；`P34` 的窗口注册表与 `native_handle(WindowId)`。
- `P14` 的 `NativeImage`（菜单图标、托盘图标都用它，跨线程用 `Arc<NativeImage>`）。
- `src/sockets.rs` 的 `emitter_class`/`define`：`Menu`、`MenuItem`、`Tray` 是 EventEmitter。
- Electron 文档 `docs/api/menu.md`、`menu-item.md`、`tray.md`、`global-shortcut.md`、`accelerator.md`（39.x 分支）。
- GPUI 的菜单：`gpui::App::set_menus` 只在 macOS 生成原生菜单栏；Windows/Linux 上 Zed 自己画标题栏菜单。所以本包
  **不使用** GPUI 的菜单 API，统一用 muda（见下），macOS 上也用 muda 以保证三平台行为一致。

## 选型与体积

| 用途 | crate | 理由 |
|---|---|---|
| 应用菜单、窗口菜单栏、上下文菜单 | `muda`（`default-features = false`；Linux 需要 `gtk` feature 才有菜单栏——Linux 先只支持上下文菜单与托盘菜单，菜单栏用 P34 的自绘回退，见下） | Tauri 维护，三平台原生菜单；Windows 上 `Menu::init_for_hwnd(hwnd)` 把原生菜单栏挂到任意 HWND；macOS 上 `init_for_nsapp`；`ContextMenu::show_context_menu_for_hwnd`。增量 Windows ≈ 150 KB |
| 托盘 | `tray-icon`（`default-features = false`） | 同作者，和 muda 共用菜单类型；Windows Shell_NotifyIcon，macOS NSStatusItem，Linux libappindicator（运行时 dlopen，不链接） |
| 全局快捷键 | `global-hotkey` | Windows `RegisterHotKey`，macOS Carbon `RegisterEventHotKey`，Linux X11 `XGrabKey`；无 Wayland 支持（与 Electron 一致：Wayland 下 `register` 返回 `false`） |

三者的事件接收器都是全局 channel（`MenuEvent::receiver()`、`TrayIconEvent::receiver()`、`GlobalHotKeyEvent::receiver()`），
**只在 Shell 线程读**，在 GPUI 主循环每次被唤醒时 drain（并给三者 `set_event_handler` 设置回调去调用
`hub.wake_shell()`，避免轮询）。

**Linux 菜单栏**：不开 muda 的 `gtk` feature（会链 GTK3，增量数 MB），Linux 上 `setApplicationMenu`/`setMenu` 由
P34 的 Shell 自绘一条菜单栏（GPUI 元素）+ muda 上下文菜单弹出子菜单。若 P34 没有余力，Linux 菜单栏先不显示，但
菜单项的 `accelerator` 仍按下文"快捷键表"生效——在 P41 矩阵登记。

## 接口契约

### 1. 菜单的所有权与同步

- **JS 对象是真相源**：`Menu`/`MenuItem` 是主进程 Boa 上的原生对象，数据（`MenuModel`）存在 Rust 侧：

```rust
pub type MenuId = u32;      // 每个 Menu 实例一个
pub type MenuItemId = u32;  // 每个 MenuItem 实例一个（全局唯一，FromShell::MenuClicked 用它）

pub struct MenuItemSpec {
    pub id: MenuItemId,
    pub kind: MenuItemKind,              // Normal | Separator | Submenu(MenuSpec) | Checkbox | Radio | Header | Palette
    pub role: Option<Role>,
    pub label: String,                   // 已按 role 补默认标签、已处理 '&' 助记符
    pub sublabel: String, pub tool_tip: String,
    pub accelerator: Option<Accelerator>,
    pub icon: Option<Arc<NativeImage>>,
    pub enabled: bool, pub visible: bool, pub checked: bool,
    pub register_accelerator: bool, pub accelerator_works_when_hidden: bool,
}
pub struct MenuSpec { pub id: MenuId, pub items: Vec<MenuItemSpec> }
```

- 任一属性被 JS 修改（`item.enabled = false`、`menu.append(...)`）后，主进程在**本轮微任务结束时**合并一次，向 Shell
  发送受影响的完整 `MenuSpec`（简单正确优先，不做细粒度 diff；菜单很小）。
- Shell 侧持有 `MenuId → muda::Menu/Submenu` 和 `MenuItemId → muda::MenuItem/CheckMenuItem/IconMenuItem` 映射；
  muda 的菜单项 id 设为 `MenuItemId` 的十进制字符串。

### 2. 在 `MenuOp` 上追加（`msg/shell.rs`）

```rust
pub enum MenuOp {
    /// 新建或整体替换一个菜单的内容
    Upsert(MenuSpec),
    Drop(MenuId),
    SetApplicationMenu(Option<MenuId>),
    SetWindowMenu { window: WindowId, menu: Option<MenuId> },     // setMenu / removeMenu
    SetMenuBarVisibility { window: WindowId, visible: bool, auto_hide: bool },
    Popup { menu: MenuId, window: WindowId, at: Option<(f32, f32)>, positioning_item: Option<usize>, call: u64 },
    ClosePopup { window: Option<WindowId> },
    // 托盘
    TrayCreate { tray: u32, image: Arc<NativeImage>, guid: Option<String> },
    TraySet { tray: u32, op: TrayOp },
    TrayDestroy { tray: u32 },
    // 全局快捷键（register 是同步返回 bool 的 API，见第 5 节）
    ShortcutRegister { id: u32, accelerator: Accelerator, reply: std::sync::mpsc::SyncSender<bool> },
    ShortcutUnregister { id: u32 },
    ShortcutUnregisterAll,
}
pub enum TrayOp {
    Image(Arc<NativeImage>), PressedImage(Option<Arc<NativeImage>>), ToolTip(String), Title(String),
    ContextMenu(Option<MenuId>), PopUpContextMenu { menu: Option<MenuId>, at: Option<(f32, f32)> },
    CloseContextMenu, IgnoreDoubleClick(bool),
    DisplayBalloon { icon: Option<Arc<NativeImage>>, icon_type: String, title: String, content: String,
                     large_icon: bool, no_sound: bool, respect_quiet_time: bool },
    RemoveBalloon, Focus,
}

// FromShell 追加（P02 已有 MenuClicked { menu_item, window }，沿用）
FromShell::MenuWillShow { menu: MenuId }, FromShell::MenuWillClose { menu: MenuId, call: Option<u64> },
FromShell::Tray { tray: u32, event: TrayEvent, bounds: Rect, position: Option<(f32, f32)>, modifiers: KeyModifiers },
FromShell::Shortcut { id: u32 },
FromShell::TrayBounds { tray: u32, bounds: Rect },            // getBounds 的缓存来源
```

`Menu.popup` 是异步的（Electron 中 `popup` 立即返回，`callback` 在关闭时调用）；`call` 用于把 `MenuWillClose`
对应回 `callback`。

### 3. 点击、checkbox 与 radio 的语义

Shell 收到 muda 的点击 → `FromShell::MenuClicked { menu_item, window }`（`window` = 当前聚焦窗口或弹出菜单的窗口）。
主进程按 Electron 顺序处理：
1. `checkbox`：先翻转 `checked`；`radio`：同组（相邻且同为 radio，被 separator 隔断）其余取消，自身选中。
2. 调用 `click(menuItem, browserWindow | undefined, event)`，`event` 含 `shiftKey`/`ctrlKey`/`altKey`/`metaKey`/`triggeredByAccelerator`。
3. 有 `role` 且无 `click` 时执行 role 行为（下表）；有 `click` 时**只**调 `click`（Electron 行为）。
4. 状态变化后按第 1 节重新下发 `MenuSpec`。

### 4. Role 全表与行为

实现位置：`api/menu.rs` 的 `fn perform_role(role, window, hub)`。"→ 渲染器"表示向聚焦窗口的 webContents 发命令，由
P13/P24 实现具体编辑行为（本包只负责路由，定义 `ToRenderer::EditCommand(EditCommand)` 追加变体）。

| role | 默认标签（en） | 默认快捷键 | 行为 |
|---|---|---|---|
| `undo` / `redo` | Undo / Redo | `CmdOrCtrl+Z` / `Shift+CmdOrCtrl+Z`（Win `Ctrl+Y`） | → 渲染器 `document.execCommand('undo'/'redo')` |
| `cut` / `copy` / `paste` / `pasteAndMatchStyle` / `delete` / `selectAll` | Cut / Copy / Paste / Paste and Match Style / Delete / Select All | `CmdOrCtrl+X/C/V`、`Shift+CmdOrCtrl+V`、无、`CmdOrCtrl+A` | → 渲染器对应编辑命令 |
| `reload` / `forceReload` | Reload / Force Reload | `CmdOrCtrl+R` / `Shift+CmdOrCtrl+R` | `webContents.reload()` / `reloadIgnoringCache()` |
| `toggleDevTools` | Toggle Developer Tools | `Alt+CmdOrCtrl+I`（Win/Linux `Ctrl+Shift+I`） | `webContents.toggleDevTools()`（P43） |
| `resetZoom` / `zoomIn` / `zoomOut` | Actual Size / Zoom In / Zoom Out | `CmdOrCtrl+0` / `CmdOrCtrl+Plus` / `CmdOrCtrl+-` | 改 `webContents` zoomLevel ±0.5（P13） |
| `togglefullscreen` | Toggle Full Screen | `F11`（mac `Ctrl+Cmd+F`） | `win.setFullScreen(!win.isFullScreen())` |
| `window` / `minimize` / `close` / `zoom` / `front` | Window / Minimize / Close / Zoom / Bring All to Front | `CmdOrCtrl+M`、`CmdOrCtrl+W` | 最小化 / 关闭（触发 close 事件）/ 最大化切换 / macOS 前置 |
| `quit` | Quit `<App>`（Win `Exit`） | `CmdOrCtrl+Q`（Win 无） | `app.quit()` |
| `about` / `hide` / `hideOthers` / `unhide` / `services` | About `<App>` 等 | `Cmd+H` 等 | macOS 原生；Windows/Linux `about` 显示 `app.setAboutPanelOptions` 的信息框（经 P14 dialog），其余无操作 |
| `editMenu` / `viewMenu` / `windowMenu` / `fileMenu` / `appMenu` / `help` / `recentDocuments` / `clearRecentDocuments` / `shareMenu` / `startSpeaking` / `stopSpeaking` / `toggleSpellChecker` / `showSubstitutions` 等 macOS 专有 role | 按 Electron `lib/browser/api/menu-item-roles.ts` | — | `editMenu`/`viewMenu`/`windowMenu`/`fileMenu`/`appMenu` 展开成 Electron 定义的默认子菜单（照抄那张表，含平台分支）；macOS 专有 role 在其他平台为隐藏项 |

默认标签与 `menu-item-roles.ts` 逐项核对（PR 中附核对表）。`label` 中 `&` 是 Windows/Linux 助记符，`&&` 是字面 `&`，
macOS 上去掉 `&`。

### 5. Accelerator

`accelerator.rs`：解析 Electron 语法 `"CommandOrControl+Shift+Z"`，结果：

```rust
pub struct Accelerator { pub modifiers: Mods /* cmd, ctrl, alt(option), shift, super(meta), alt_gr */, pub key: AccelKey }
```

- 修饰键：`Command`/`Cmd`、`Control`/`Ctrl`、`CommandOrControl`/`CmdOrCtrl`、`Alt`、`Option`、`AltGr`、`Shift`、`Super`、`Meta`；大小写不敏感。
- 键：`0-9`、`A-Z`、`F1-F24`、标点 ``) ! @ # $ % ^ & * ( : ; : + = < , _ - > . ? / ~ ` { ] [ | \ } "``、`Plus`、`Space`、`Tab`、`Capslock`、`Numlock`、
  `Scrolllock`、`Backspace`、`Delete`、`Insert`、`Return`/`Enter`、`Up`/`Down`/`Left`/`Right`、`Home`/`End`、`PageUp`/`PageDown`、
  `Escape`/`Esc`、`VolumeUp`/`VolumeDown`/`VolumeMute`、`MediaNextTrack`/`MediaPreviousTrack`/`MediaStop`/`MediaPlayPause`、
  `PrintScreen`、`num0-num9`、`numdec`/`numadd`/`numsub`/`nummult`/`numdiv`。
- 非法字符串：`MenuItem` 中忽略快捷键并 `console.warn`（Electron 行为）；`globalShortcut.register` 抛 `TypeError`。
- 转换：`Accelerator → muda::accelerator::Accelerator`、`→ global_hotkey::hotkey::HotKey`、`→` 与 verve-dom `Key` 比对的
  `fn matches(&self, key: &verve_dom::Key) -> bool`（窗口内快捷键用）。

**窗口内快捷键**（菜单栏隐藏、或 Linux 无原生菜单栏、或 `acceleratorWorksWhenHidden`）：Windows 上原生菜单栏挂在 HWND 时
系统通过 muda 处理加速键表（`TranslateAcceleratorW`——GPUI 的消息循环是否调用它要在 P34 确认；若不调用，统一走下面的
路径）。统一路径：Shell 在把按键发给渲染器**之前**查当前窗口的有效快捷键表（窗口菜单优先，其次应用菜单），命中则
发 `FromShell::MenuClicked { triggered_by_accelerator: true }`（给 `MenuClicked` 追加该字段）并**不**把按键发给渲染器；
但 P13 的 `before-input-event` 需要先于菜单：顺序为 Shell → 主进程询问 `before-input-event`（异步，P13 定义）→ 未阻止则
查快捷键表 → 未命中再给渲染器。为了不让每次按键都绕主进程，P13 只在有 `before-input-event` 监听者时走询问路径，
本包负责把"有效快捷键表"随 `MenuSpec` 一起缓存在 Shell。

### 6. globalShortcut 的同步返回

`register` 需同步返回 `boolean`。主进程发 `ShortcutRegister { reply }` 后 `recv()` 等 Shell 回答——这同 P14 的同步对话框一样
属于"主进程阻塞等 Shell"的例外，但**有界**（Shell 只做一次系统调用就回答）。加 500 ms 超时，超时返回 `false` 并警告。
在 P02 同步规则的例外清单里追加这一条（与 P14 共用一个列表）。`app` ready 之前调用 `register` 抛错
（Electron：`globalShortcut cannot be used before the app is ready`）。

### 7. 生命周期

- `Menu`/`Tray` 的 JS 对象被 GC 时：`Menu` 若仍是应用菜单/窗口菜单/托盘上下文菜单则不得销毁原生菜单（Shell 侧引用计数，
  主进程 `Drop(MenuId)` 只是减引用）。`Tray` 在 JS 侧被 GC 时**销毁托盘图标**——这是 Electron 的著名坑，照抄（应用会把它
  存到全局变量里）。
- `app` 退出时 Shell 先销毁所有托盘图标（Windows 不清理会残留幽灵图标）。

## 契约修订（实现前确定，以此为准）

下列各点在读过 Electron 39.8.10 自带的 `menu.ts`/`menu-item.ts`/`menu-item-roles.ts`/`default-menu.ts` 并观察真 Electron 之后改动了上文：

- **点击顺序**：`MenuItem.click` 先翻转 checkbox/radio（`checked` 的 setter 对 radio 总是“选中自己、取消同组”），再试 role 行为；role 有行为**且**目标（窗口或 `webContents`）存在时只做 role，**不**调 `click`；否则调 `click(menuItem, browserWindow, event)`。上文第 3 节“有 `click` 时只调 `click`”不对。
- `MenuItemSpec.role` 是 Electron 存的小写字符串（`Option<String>`），不是枚举；平台差异（标签、默认快捷键）在生成 `MenuItemSpec` 时按当前平台定下，Shell 不再查表。
- `MenuItemSpec.label` 保持原样（含 `&` 助记符），由 Shell 的平台层处理；`&&` 是字面 `&`。
- `FromShell::MenuClicked` 追加 `triggered_by_accelerator: bool` 与 `modifiers: Modifiers`；`ToRenderer::EditCommand` 已由 P13 追加，不再改。
- **默认应用菜单**：Electron 在 `will-finish-launching` 时若没人设过应用菜单，就设 `File/Edit/View/Window/Help`（`default-menu.ts`），所以 `Menu.getApplicationMenu()` 在 `ready` 时非空；P15 照做。
- `globalShortcut.isRegistered` 由主进程的记录回答（`ready` 之前也不抛错），只有 `register`/`registerAll` 等 Shell。
- Linux：`muda`、`tray-icon`、`global-hotkey` 只在 Windows 与 macOS 编入；Linux 上菜单栏、托盘、全局快捷键都是空操作（`register` 返回 `false`，与 Electron 在 Wayland 上一致），登记给 P41。

## 步骤

1. `accelerator.rs` + 单元测试（全部键名、修饰键别名、非法输入）。
2. `MenuItem`/`Menu` 原生类、模板构建（含 `before`/`after`/`*GroupContaining` 排序算法，照 Electron
   `lib/browser/api/menu-utils.ts`）、role 默认值表、`MenuSpec` 生成与合并下发。
3. Shell：muda 菜单构建、`init_for_hwnd`（Windows）/`init_for_nsapp`（macOS）、`show_context_menu_for_hwnd`、事件 drain。
4. 点击语义（checkbox/radio/role/click）、`EditCommand` 路由。
5. 窗口内快捷键表与按键拦截。
6. `Tray`（tray-icon）与事件；`getBounds` 缓存。
7. `globalShortcut`（global-hotkey）。
8. P02 同步规则例外清单追加 globalShortcut 一条（docs commit）。

## 验收

- `cargo test -p verve-electron --test menu`（无头）：
  - `buildFromTemplate` 对 Electron 文档里的完整示例模板生成的 `MenuSpec` 与冻结结果一致（标签、快捷键、role 展开、排序）。
  - 无头 Shell 注入 `MenuClicked` → `click` 被调用、参数正确；radio 组互斥；checkbox 翻转先于 `click`。
  - 修改 `item.enabled` 后无头 Shell 收到新的 `Upsert`，且一轮微任务内多次修改只下发一次。
  - 注入快捷键按键：命中菜单快捷键时渲染器收不到 `keydown`；未命中时收到。
  - `globalShortcut.register` 在 ready 前抛错；ready 后返回无头 Shell 的答复。
- 手动（Windows GUI）：`tests/electron/apps/menu-tray/`（本包新增）—— 原生菜单栏显示、助记符、快捷键、右键菜单、
  托盘图标与右键菜单、气泡、`Ctrl+Shift+X` 全局快捷键在应用失焦时触发。
- 体积增量 ≤ 400 KB（Windows）。

## 不做

macOS Touch Bar（`TouchBar` 类存在，`setTouchBar` 无操作）、`MenuItem.sharingItem`/`shareMenu`、Linux GTK 原生菜单栏、
Wayland 全局快捷键、菜单项 `palette`/`header` 的 macOS 14 新样式（按 `normal` 渲染）。

## 状态

（实现者完成后填写）
