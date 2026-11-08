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

### 做了什么

- **`accelerator.rs`**（`verve_electron::accelerator`）：全部修饰键别名、键名、标点与需要 Shift 的符号；`Accelerator::matches(&verve_dom::Key)` 按 `code` 比较（没有 `code` 时按键入的字符），修饰键必须完全相同。单元测试覆盖所有键名与非法输入。
- **`menu/`**（纯数据，主进程与两个 Shell 共用）：`roles.rs`（角色表，照 Electron 39.8.10 的 `menu-item-roles.ts`）、`sort.rs`（照 `menu-utils.ts` 与 `removeExtraSeparators`）、`book.rs`（`MenuBook`：跟随 `MenuOp`，`item_for(window, key)` 先窗口菜单再应用菜单，按 Electron 39 的规则：非 macOS 上每个 `enabled` 且 `registerAccelerator` 的项都在表里，不管 `visible` 与菜单栏是否隐藏；macOS 上 `visible: false` 的项只有 `acceleratorWorksWhenHidden` 才算；同一加速键取最后一项；禁用的子菜单不挡住里面的项）。
- **`msg/menu.rs`**：`MenuSpec`/`MenuItemSpec`/`MenuOp`/`TrayOp`/`TrayEvent`；`FromShell` 追加 `MenuWillShow`、`MenuWillClose`、`Tray`、`TrayBounds`、`Shortcut`，`MenuClicked` 追加 `triggered_by_accelerator` 与 `modifiers`。`ToRenderer::EditCommand` 已由 P13 追加，未再改。
- **`api/menu_item.rs`、`menu.rs`、`menu_roles.rs`、`menu_state.rs`、`menu_window.rs`**：`MenuItem` 照 `menu-item.ts` 逐步构造（`Object.keys(item)` 与 Electron 相同顺序）；`Menu` 的 `buildFromTemplate`/`append`/`insert`/`getMenuItemById`/`popup`/`closePopup`/`setApplicationMenu`/`getApplicationMenu`、`menu-will-show`/`menu-will-close`、radio 组（`checked` 是访问器）；`app.applicationMenu`；`BrowserWindow.setMenu`/`removeMenu`，`setMenuBarVisibility`/`setAutoHideMenuBar` 发 `SetMenuBarVisibility`（只有脚本说过 `setMenuBarVisibility(false)` 才隐藏）；窗口创建时（非 macOS）得到应用菜单；`will-finish-launching` 时若没人设过应用菜单就设 Electron 的默认菜单（`default-menu.ts`）。角色行为（`copy`/`selectAll` 等调聚焦页面的 `webContents` 方法，`close`/`minimize`/`togglefullscreen`/`reload` 调窗口方法，`quit` 调 `app.quit()`）按 `execute` 的规则实现。
- **向 Shell 同步**：每次 `Guest::tick` 的末尾（定时器与它们的微任务之后）读一遍已下发的菜单，与 Shell 手里的比较，不同才发一次整份 `Upsert`；所以一轮里改多少次只发一次。菜单对象被回收时发 `Drop`；`Tray` 对象被回收时销毁图标。
- **`api/global_shortcut.rs`**：`register`/`registerAll`/`isRegistered`/`unregister`/`unregisterAll`；`register` 在一次性通道上最多等 Shell 500 ms。
- **`api/tray.rs`**：`Tray` 全部方法与事件（`click`、`right-click`、`double-click`、`middle-click`、`mouse-*`、`drop-files`）；`getBounds` 用 Shell 最近一次报告的位置。
- **无头 Shell**（`headless/menu.rs`）：保存收到的菜单；`popup` 立刻打开并关闭（什么也没选）；托盘图标是可读的记录；全局快捷键默认接受，`accept_shortcuts(false)` 后拒绝，从不向系统注册；`press_key(window, key)` 注入按键——有 `before-input-event` 监听者时走 `KeyForContents`，否则先查 `MenuBook`，命中就是 `MenuClicked { triggered_by_accelerator: true }`，页面收不到这个键。
- **GPUI Shell**（`crates/verve-gpui/src/{menu,tray,shortcut,native}.rs`）：`muda` 画菜单栏（Windows 在每个窗口的 `HWND` 上 `init_for_hwnd`，macOS 是 `init_for_nsapp`）和弹出菜单，`tray-icon` 做托盘，`global-hotkey` 做全局快捷键；`shell.rs` 只多了一个分发。`window.rs` 只多了 `raw_handle()`。

### 没做 / 与任务书不同

- **Linux 没有菜单栏、托盘、全局快捷键**：`muda`/`tray-icon`/`global-hotkey` 只在 `cfg(any(windows, target_os = "macos"))` 下编入，Linux 的 Shell 里 `popup` 立即关闭，`register` 返回 `false`（与 Electron 在 Wayland 上一致），托盘不显示。**请登记给 P41**；Linux 菜单栏的自绘（任务书里的 GPUI 元素）没做。
- **角色与 `click` 的顺序**：见「契约修订」，真 Electron 是 role 有行为时不调脚本的 `click`。
- **`MenuItem.sharingItem`/`shareMenu`、macOS `header`/`palette` 的新样式、Touch Bar**：按「不做」；`palette`/`header` 按 `normal` 画。
- **`displayBalloon`/`removeBalloon`/`focus`/`setPressedImage`**：`tray-icon` 没有对应接口，Shell 不处理（没有 `balloon-*` 事件）。`Tray.setTitle`/`getTitle` 在 Windows 上只记录。托盘事件的位置按系统 DPI 换算成 DIP，混合 DPI 的屏幕不精确。
- **`webContents.setIgnoreMenuShortcuts`** 仍是 P13 的空操作，没有接线。
- **`toggleDevTools` 角色**调用 `webContents.toggleDevTools()`，那是 P43 的空操作。`togglespellchecker` 角色有页面时只翻转页面 `session.spellCheckerEnabled`（页面的 `session` 没有 `spellCheckerEnabled` 属性时什么也不做）。
- **`isMinimizable` 等对 `minimize`/`togglefullscreen`/`close` 角色启用状态的影响**（`_isCommandIdEnabled` 里的特例）没做。
- **菜单栏占用的高度**：Electron 的窗口有应用菜单时内容区少 26 像素（P13 的观察）。无头 Shell 与 P13 的 `window_state.rs` 的 `FRAME` 没有跟着变，真 Shell 里 muda 的菜单栏会占去窗口内容区的高度——这是 P13/P34 的事。
- **体积**：见第三轮。依赖：`muda` 0.21.2、`tray-icon` 0.26.1、`global-hotkey` 0.9.0，都是 `default-features = false`（`global-hotkey` 无默认特性）。**重复**：`Cargo.lock` 多了 `windows-sys 0.59`（`global-hotkey` 要求，各版本都一样）；`windows-sys 0.61`、`objc2 0.6` 与已有的共用。`Cargo.lock` 只新增包（`keyboard-types 0.8.3`、`crossbeam-channel`、`dpi`、`muda`、`tray-icon`、`global-hotkey`、`windows-sys 0.59` 与 Linux 才用的 `x11rb` 等），已有包的依赖边没有改动（`git diff b1a27600 -- Cargo.lock` 只有新增）；其中 `keyboard-types` 是新包，不是共用的。
- **每轮读菜单的代价**：默认菜单一直是已下发的，所以每个 `Guest::tick` 都会读它的约 30 个菜单项；很小，但若 P44（性能）的基线里看到主进程的空转变慢，先看 `menu_state::sync`。

### 已知缺口（第三轮后仍在）

- `popUpContextMenu`/`menu.popup` 在 Windows 上阻塞 OS 线程，期间 GPUI 的前台任务是否停转**没有验证**（`TrackPopupMenu` 的模态循环会分发已投递的消息，也许照常运行）；弹出时选中的项先于 `menu-will-close` 送出，回调在其后。
- GPUI Shell 的 `tray.popUpContextMenu` 忽略它收到的菜单和位置（`tray.rs` 的 `TrayOp::PopUpContextMenu` 只调 `icon.show_menu()`，显示的是 `setContextMenu` 设的那个，位置由系统定）。
- radio 项在 muda 里画成带勾的复选项。muda 点击时自己翻转勾，Shell 在点击后按 `Menus` 里最近一次的状态把勾设回（`Menus::restore_check`），弹出菜单（`popup`）与菜单栏、托盘菜单（`Native::heard`）都一样；点中的 radio 随后由脚本的 `Upsert` 就地改成选中。**没有在真机上验证。**
- 形状不变的 `Upsert`（标签、勾、可用、加速键、图标变了，项的个数、嵌套与种类没变）现在就地更新（`Menus::upsert` → `update_in_place`），不再卸下重挂菜单栏；形状变了才整份重建。**没有在真机上验证。**
- 菜单栏上的菜单不发 `menu-will-show`/`menu-will-close`（只有弹出菜单发）。
- Alt、F10、Alt+字母可能打不开 muda 的菜单栏：GPUI 自己对 `WM_SYSKEYUP` 回 0（`gpui_windows` 的 `events.rs`），没在真机上试。
- `MenuClicked.modifiers` 从 Shell 来时总是空的（muda 不给）。`ToShell::Menu(TrayCreate)` 在 `release()` 之后到达时仍会建图标。
- 加速键按物理 `code` 匹配，Chromium 按布局的 VKEY；两者在非美式键盘上不同（例如法语键盘上 `Ctrl+A` 在 `KeyQ` 的位置）。没有改，因为 GPUI 给的是 `code`。
- `Plus` 现在是 Shift 加 `=` 键（`CmdOrCtrl+Plus` 即 Ctrl+Shift+=），`+` 两边的空白裁掉，几个键取最后一个；这三条都是在 Electron 39.8.10 上用 `globalShortcut.register` 的转换（先于 `ready` 检查）观察到的，只有「最后一个键」具体是哪个观察不到。
- 体积：第三轮做了下面「体积」里的几处。总控量过：相对 b1a27600 增加 504,832 字节，超过任务书的 400 KB 预算；2026-11-07 总控在 electron.exe 上限提高到 36 MB 后接受了这个数。没做：默认菜单的静态表（它会多出代码而不是少）、把 `read_nested` 等大函数拆开。
- 8d72126c 没有拆成 Menu/MenuItem、窗口菜单、默认菜单、按键查找四个提交：这些文件互相引用，拆开每个提交都要编译通过，改动量大，没做。
- 菜单栏的 muda 加速键不触发（GPUI 的循环没有 `TranslateAcceleratorW`，见下），P32 之前页面里按菜单的快捷键走不通。

### 第三轮做了什么

- 正确性：就地更新与勾的恢复（上）；主进程路径上被菜单拿走的键，其 key up 也不再到页面（`MenuState::swallow`/`release`，`tests/menu.rs` 有测试）。
- 保真：`registerAll` 全有或全无并返回布尔（`registerAll([])` 是 `true`）；`register`/`registerAll`/`unregister`/`unregisterAll` 先转换参数再查 `ready`（`isRegistered` 不查 `ready`，`unregisterAll` 查）；`setMenu(空菜单)` 不挂菜单栏；构造选项 `autoHideMenuBar` 发给 Shell；`togglespellchecker` 有页面时由角色处理、不调脚本的 `click`；`copy_options` 是 `for…in`（含继承的可枚举键）；`Menu` 不再调 `EventEmitter` 构造函数，`Object.keys(new Menu())` 与 Electron 相同，探针的 "menu inst"/"menu props" 两行现在在比较里。
- 去重：`string_of`/`text`/`role_of` 并成 `menu_state::text`；`this_object`/`this_menu_item` 并成 `menu_state::this_object`；`field`/`assign` 换成 `native::get` 与 `set`；`overrideProperty`/`overrideReadOnlyProperty` 共用 `override_method`；`accelerator::System` 并入 `menu::roles::Os`；gpui 里 muda 与 global-hotkey 的加速键转换并成 `native::key_parts`。
- 结构：`FromShell` 的五个新变体移到枚举末尾；`VERVE_NO_SYSTEM_DIALOGS` 写进了 `conventions.md`；gpui 里剩下的 `let _ =` 都有理由。
- 体积（**没有量**，期望是缩小）：`verve_electron::keyed::Keyed` 是按序搜索的小表，替掉 `attached`、`window_menus`、托盘注册表和 gpui 里的 `menus`/`bars`/`icons`/`taken`，少了每种值各一份 `BTreeMap::insert`/`remove`（约每份 3 KiB）；`menu_state::text`/`flag` 与 `override_property`/`override_read_only` 标了 `#[inline(never)]`，不在 `read_nested`、`MenuItem::init` 里内联十几份；`muda`/`global-hotkey` 的加速键转换合一。
- 新测试：被回收的 `Tray` 在 Shell 里销毁、被回收的弹出菜单发 `Drop`（`tests/menu.rs`，用 `boa_engine::gc::force_collect`）。

### 我在真 Electron 39.8.10（Windows）上观察并冻结的东西

探针与回答在 `tests/electron/menu/observe/main.js`、`expected/probe.txt`（135 行；`tests/menu.rs` 与其中约 75 个标签逐行比较）：

- `globalShortcut` 先转换全部参数、再查 `ready`：`register('NotAKey', f)` 在 `ready` 之前是 gin 的转换失败，`register('Ctrl+A', f)` 才是下面的 `ready` 错误；`unregister` 与 `unregisterAll` 在 `ready` 之前也抛同样的错，`registerAll([], f)` 在 `ready` 之后是 `true`；加速键里 `Ctrl + A`、` Ctrl+A`、`A+B` 都能转换，`Ctrl++` 与 `Ctrl+ ` 不能。
- `globalShortcut.register` 在 `ready` 之前是 `Error: globalShortcut cannot be used before the app is ready`；`isRegistered` 在 `ready` 之前返回 `false` 不抛；非法加速键是 gin 的 `TypeError: Error processing argument at index 0, conversion failure from NotAKey`（`registerAll` 里是 `conversion failure from ` 后面为空）；缺回调是 `Insufficient number of arguments.`。
- `new Tray()` 是 `TypeError: Insufficient number of arguments.`（先于 `ready` 检查）；`ready` 之前是 `Error: Cannot create Tray before app is ready`；参数不是路径或 `NativeImage` 是 `Error: Argument must be a file path or a NativeImage`。
- `Menu.getApplicationMenu()` 在 `ready` 之前是 `null`，之后是默认菜单（Windows：File/Edit/View/Window/Help，Help 在未打包时有四个链接）；`menu.popup()` 没有窗口是 `Error: Cannot open Menu without a BaseWindow present`。
- 全部角色的标签、默认加速键、`registerAccelerator`（只有 cut/copy/paste/pasteAndMatchStyle 是 `false`）、类型（`togglespellchecker` 是 `checkbox`，带子菜单的是 `submenu`）、各 `*Menu` 角色展开的子菜单。未知角色保留名字、标签为空。
- `Object.keys(new MenuItem({label}))` 与带 `id`/`userData`/`click`/`accelerator`/`icon` 的顺序；`type`/`role`/`accelerator`/`icon`/`submenu`/`commandId` 只读；模板和 `insert` 的全部错误文案；`before`/`after`/`*GroupContaining` 的排序结果；radio 的 `checked` 设置器（设成 `false` 也会选中自己）。
- **没有观察到**：无效加速键在 `win.setMenu` 时不输出任何警告（stderr 为空），所以 Verve 也静默忽略，而不是任务书里写的 `console.warn`；点击事件 `event` 的键名（`shiftKey`/`ctrlKey`/`altKey`/`metaKey`/`triggeredByAccelerator`）来自文档，没有原生点击可以观察；托盘与真正的全局快捷键因不能碰桌面而没有探。

### GPUI 的 Windows 消息循环（P34 要求确认的）

`gpui_windows` 的 `platform.rs`（`run` 与 `run_foreground_task`）里**没有** `TranslateAcceleratorW`：每个 `WM_KEYDOWN`/`WM_SYSKEYDOWN` 先作为 `WM_GPUI_KEYDOWN` 发给窗口，GPUI 不处理才 `TranslateMessage`+`DispatchMessageW`。muda 的 Windows 实现也不自己翻译加速键（它的 `Menu::haccel()` 要调用方放进 `TranslateAcceleratorW`），所以 muda 菜单栏上的加速键在这里**不会触发**，只有菜单文字里显示。因此走「统一路径」：按键先查 `MenuBook`。

### 给其他包的备注

- **P32（输入桥）**：**页面先听到键，菜单的加速键只在页面没处理这个 key down 时才动**（第五轮改了：原先的规则是“菜单先拿走，页面听不到”，**不对**，别照它做）。Electron 39.8.10、可见的屏幕外窗口观察（总控的 `probe-keys` 探针，`keys-out.json`、`keys2-out.json`）：
  - Ctrl+O、F6 带菜单项：页面先收到 keydown 和 keyup，然后菜单项被点击；页面取消 keydown：不点击；`before-input-event` 取消 keyDown：页面既没有 keydown 也没有 keyup，不点击，也没有 keyUp 的 `before-input-event`。
  - “没处理”是：没被取消，且 keydown 的默认动作没有做取消之外的事——**输入这个字符不算**。会点击：空 input 里的 Ctrl+Z、普通 input 里的 Ctrl+B、body 上的 Ctrl+Left 和 Backspace；不点击：input 或 body 里的 Ctrl+A（选中）、输入过后的 Ctrl+Z（撤销）、input 里的 Ctrl+Left（移动光标）、input 里的 Backspace（删除）。input 里的普通 `Q` 加速键：字符 `q` 被输入，菜单项也被点击。
  - 除了“被 `before-input-event` 取消的 key down 的 key up”，**什么也不吞**。
  - 路径：Shell 把 key down 发给页面（页面有 `before-input-event` 监听时先作为 `FromShell::KeyForContents` 发给主进程，监听器没取消才交给页面）；渲染器的 `Document::key_down_unhandled` 说没处理，就发 `FromRenderer::KeyUnhandled(key)`；主进程（`input_events::key_unhandled`）查菜单并点击（`triggered_by_accelerator: true`）。查找只在 verve-electron 里做一次，GPUI Shell 与无头 Shell 共用；`Control::menu_item_for_key` 已删。macOS 没有观察，规则相同。
  - 原先第四轮的吞键记录规则（Shell、无头 Shell、主进程各一份）只剩主进程 `MenuState::swallow/release` 里这一条：被 `before-input-event` 取消的 key down 记下 `(页面, code)`，它的 key up 找到记录就吞掉。`tests/menu.rs` 对应改了五个测试。
- **P34**：`ToShell::Menu` 的分发在 `shell.rs` 一行，具体在 `native.rs`（读开关的函数叫 `touches_desktop`）；`VERVE_NO_SYSTEM_DIALOGS` 的意思是“Shell 不碰桌面”：现在也关掉菜单栏、托盘、全局快捷键（GPUI 的二进制测试都设了它，所以它们不会把菜单放到桌面上）。Windows 上 `Popup` 在 OS 线程里阻塞（`TrackPopupMenu` 自己抽消息）到菜单关闭才返回；这在嵌套消息循环里是否与 GPUI 的前台任务相安无事**没有在真机上验证**，见下面的手动清单。`ClosePopup` 在 Windows 上无法实现（弹出菜单阻塞着）。
- **P13**：`setMenu`/`removeMenu` 已实现；`setIgnoreMenuShortcuts` 没接线；`isMenuBarVisible` 等标志仍由 `window_state.rs` 记录（初值与真 Electron 不一致：观察到隐藏窗口 `isMenuBarVisible()` 初始 `false`，`setAutoHideMenuBar(true)` 之后读到 `true`，本包没改）。内容区高度见上。
- **P24**：菜单角色 `undo`/`redo`/`cut`/`copy`/`paste`/`pasteAndMatchStyle`/`delete`/`selectAll` 调的是 `webContents` 同名方法，P13 已把它们接到 `ToRenderer::EditCommand`；渲染器侧没有就什么也不发生。
- **P41**：Linux 的菜单栏、托盘、全局快捷键（见上）；macOS 的菜单与托盘代码照 muda/tray-icon 的接口写了，**没有编译过**。

### 手动清单（Windows 桌面，用户亲自跑；自动化从不跑）

`cargo test -p verve-gpui --test menu_tray -- --ignored` 会把菜单栏、托盘图标和 `Ctrl+Shift+X` 在桌面上放半秒后退出。要慢慢看，用 `MENU_TRAY_STAY=1`：

1. 窗口只有 `MENU_TRAY_STAY=1` 时才显示（`global.win`/`global.tray` 持有，否则托盘对象会被回收、图标消失，正是本包照抄的那个坑）。在没有 `VERVE_NO_SYSTEM_DIALOGS` 的 PowerShell 里：`$env:MENU_TRAY_STAY='1'; electron.exe tests\electron\apps\menu-tray --user-data-dir=<临时目录>`。
2. 窗口有菜单栏：File（Open `Ctrl+O`、分隔线、Exit）、Edit（Copy/Paste）；按 `Alt+F` 能展开（助记符），点 Open 控制台出现 `menu: open`。
3. 按 `Ctrl+O` 触发 Open（页面先收到这个键，没取消它菜单项才动，见上面 P32 的备注）。
4. 通知区出现图标，悬停显示 `menu-tray fixture`，右键出现菜单（只有 Exit）；选 Exit 退出应用。
5. 另一个窗口有焦点时按 `Ctrl+Shift+X`，控制台出现 `shortcut: pressed`。
6. 退出后通知区的图标消失（没有残留的幽灵图标）。

### 要跑的测试

```
cargo test --locked -p verve-electron --lib accelerator
cargo test --locked -p verve-electron --lib menu::
cargo test --locked -p verve-electron --lib keyed
cargo test --locked -p verve-electron --test menu
cargo test --locked -p verve-electron --test fixtures menus_a_tray_and_a_shortcut_are_built
cargo test --locked -p verve-electron --test pipeline
cargo test --locked -p verve-electron --test browser_window
cargo test --locked -p verve-electron --test web_contents
cargo test --locked -p verve-electron
cargo test --locked -p verve-gpui --features jit
```

`--test menu` 里 `menus_trays_and_shortcuts_answer_as_electron_39_does` 只在 Windows 上编入。我没有运行过任何测试（只 `cargo check`），所以期望值都是照源码与探针推演出来的；失败请把输出发回。
