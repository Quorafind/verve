# P32 输入桥接与表单控件

**依赖**：P30（P31 用于文本字段测量）  **规模**：M  **修改**：`crates/verve-gpui/src/input.rs`（新）、`keymap.rs`（新）、`controls.rs`（新）、`ime.rs`（新）、`dom_element.rs`；`crates/verve-layout/src/controls.rs`（新）；`crates/verve-electron/src/renderer_guest.rs`（输入分发、`RendererView.cursor/ime_rect`）

## 目标

1. 把 GPUI 窗口里发生的鼠标、键盘、滚轮、输入法、文件拖入，翻译成 P02 的 `ToRendererInput`，渲染器再交给 verve-dom
   的输入方法（`pointer_move/pointer_down_on/pointer_up_on/wheel/key_down/key_up/insert_text/edit_field`），事件顺序与 Chromium 一致。
2. 指针样式（CSS `cursor`）、焦点环（`:focus-visible`）。
3. 原生表单控件的外观：`<input>`（text/password/search/email/url/tel/number/range/checkbox/radio/color/date/time/file）、
   `<textarea>`、`<select>`（含下拉弹层）、`<button>`、`<progress>`、`<meter>`，以及文本字段里的光标、选区和输入法预编辑。

## 背景（先读）

- verve-dom：`Pointer { x, y, button, count, modifiers }`、`Modifiers`、`Key { key, code, modifiers, repeat }`、`Key::char/named`、
  `Document::{pointer_move, pointer_leave, hover, pointer_down_on, pointer_up_on, press, wheel, key_down, key_up, type_text,
  insert_text, edit_field, draft, commit_field, choose, focus, focused, hover_sensitive, listens}`、`Draft`（`field.rs`：text、caret、anchor、marked）。
- 参考（只读）：su-editor `view.rs` 的 `pointer_of`（GPUI 按钮 → DOM 按钮号）和对 `<input>`/toggle/`<select>`/slider 的专门绘制；
  `live.rs` 的"输入框先在窗口本地显示、等宿主跟上"（乐观更新）—— 本包同样需要，见"延迟"一节。
- GPUI：`interactive.rs`（`MouseDownEvent { button, position, modifiers, click_count }`、`MouseUpEvent`、
  `MouseMoveEvent { pressed_button }`、`MouseExitEvent`、`ScrollWheelEvent { delta: ScrollDelta::Pixels|Lines, touch_phase }`、
  `KeyDownEvent { keystroke, is_held }`、`KeyUpEvent`、`ModifiersChangedEvent`、`FileDropEvent`、`MouseButton::{Left,Right,Middle,Navigate}`）；
  `platform/keystroke.rs`（`Keystroke { modifiers, key, key_char }`、`Modifiers { control, alt, shift, platform, function }`）；
  `input.rs`（`EntityInputHandler`：`text_for_range`、`selected_text_range`、`marked_text_range`、`unmark_text`、
  `replace_text_in_range`、`replace_and_mark_text_in_range`、`bounds_for_range`、`character_index_for_point`；`ElementInputHandler`）；
  `window.rs` 的 `handle_input`、`set_cursor_style`、`insert_hitbox`、`invalidate_character_coordinates`；`CursorStyle` 枚举。

## 接口契约

### 坐标

GPUI 事件的 `position` 是窗口逻辑像素（已除以 scale factor），等于 CSS 像素 × 缩放系数（`webContents.setZoomFactor`，
P13）。换算：`css = position / zoom`。无边框窗口里内容区原点就是窗口原点；有系统标题栏时 GPUI 给的已是内容区坐标。

### 鼠标

| GPUI | DOM |
|---|---|
| `MouseButton::Left/Middle/Right` | `button` 0/1/2 |
| `MouseButton::Navigate(Back/Forward)` | 3/4（同时由 P13 发 `app-command` 的 `browser-backward/forward`） |
| `click_count` | `Pointer.count`（`detail`、`dblclick`） |
| `Modifiers { control, alt, shift, platform }` | `ctrlKey, altKey, shiftKey, metaKey`（Windows 的 Win 键 = meta） |
| `MouseMoveEvent` | `PointerKind::Move`，带 `pressed_button` 时 `buttons` 位掩码 |
| `MouseExitEvent` | `pointer_leave` |

命中：Shell 用当前显示列表的 `Hitbox` 项（P30）找到目标节点，放进 `ToRendererInput::Pointer.target`，渲染器调用
`pointer_down_on(target)` 等 `_on` 方法；目标为 `None`（命中过期）时渲染器用 `LayoutEngine::hit_test` 自己算。
按下后到松开之前**捕获指针**（目标不变，`setPointerCapture` 也走这里）。
`pointer-events: none` 的节点不产生 `Hitbox`。

### 滚轮

`ScrollDelta::Pixels(p)` → `deltaMode = 0`，`deltaX/Y = -p.x / -p.y`（GPUI 与 DOM 方向相反，实现时用真实 Electron 校对符号）；
`ScrollDelta::Lines(l)` → 按 Chromium 乘 40px 转成像素（`deltaMode` 仍为 0，与 Chromium 一致）。`shift` + 竖直滚轮 → 横向。
滚动本身的处理见 P30"滚动"一节：滚轮的目标是命中元素向上最近的、能往该方向滚的 `Scroller`，滚不动再链到外层。命中与光标用 `DisplayList::resolved_hitboxes`/`hit_test`，不要自己推帧的栈。`ctrl` + 滚轮 → 发给主进程做缩放（P13 的 `zoom-changed`），不滚动。

### 键盘

`Keystroke.key` 是 GPUI 的逻辑键名（小写，如 `"enter"`、`"backspace"`、`"left"`、`"f5"`、`"a"`），`key_char` 是产生的字符。
映射表 `keymap.rs`：

| GPUI `key` | DOM `key` | DOM `code`（US 布局推断） |
|---|---|---|
| `enter`/`tab`/`backspace`/`delete`/`escape`/`space` | `Enter`/`Tab`/`Backspace`/`Delete`/`Escape`/`" "` | `Enter`/`Tab`/`Backspace`/`Delete`/`Escape`/`Space` |
| `left`/`right`/`up`/`down` | `ArrowLeft`… | `ArrowLeft`… |
| `home`/`end`/`pageup`/`pagedown`/`insert` | `Home`/`End`/`PageUp`/`PageDown`/`Insert` | 同名 |
| `f1`…`f24` | `F1`… | `F1`… |
| 单个字符 | `key_char`（考虑 shift 后的字符） | `KeyA`…`KeyZ`、`Digit0`…`Digit9`、`Minus`、`Equal`、`BracketLeft`… |
| 修饰键（`ModifiersChangedEvent` 推断按下/抬起） | `Control`/`Shift`/`Alt`/`Meta` | `ControlLeft`/`ShiftLeft`/`AltLeft`/`MetaLeft` |

GPUI 不提供物理扫描码：`code` 只能按 US 布局从 `key` 推断（非 US 布局下与 Chromium 不同，记录为已知偏差）。
若需要精确 `code`，在 fork `Quorafind/zed-c` 给 `Keystroke` 加 `scan_code: Option<u32>`（可选，后续）。
`is_held` → `repeat: true`。
**顺序**：`keydown` →（未被阻止且产生字符、且没有输入法在组字）字符走 `insert_text` 路径的 `keypress/beforeinput/input` → `keyup`。
GPUI 的动作/快捷键系统不要拦截按键：DomElement 不注册 action，所有键交给页面；只有 P15 注册的菜单加速键和
`globalShortcut` 由系统层处理（与 Electron 一致：菜单加速键先于页面 `keydown`，除非 `before-input-event` 阻止，P13）。

### 输入法（IME）与文本输入

- DomElement 在有可编辑焦点（`<input>`/`<textarea>`/`contenteditable`，由显示列表 `Control` 项或快照的 `focused` 得知）时，
  在 `paint` 中调用 `window.handle_input(focus_handle, ElementInputHandler::new(bounds, entity), cx)`；实现
  `EntityInputHandler` 的实体是 `ImeBridge`（`ime.rs`），它不持有文字真相，只做转发：
  - `replace_and_mark_text_in_range` → `ToRendererInput::Composition(Update { text, selected_range })`（P24 定义 `CompositionEvent`，
    v1 至少：`Start`、`Update{text}`、`End{committed}`）；
  - `replace_text_in_range` → `ToRendererInput::Text(text)`（提交）或 `Composition(End)`；
  - `unmark_text` → `Composition(End{committed: current})`；
  - `text_for_range`/`selected_text_range`/`marked_text_range` → 读最近一次快照里焦点字段的 `Draft`（UTF-8 ↔ UTF-16 换算）；
  - `bounds_for_range` → 返回 `RendererView.ime_rect`（候选框跟随光标）；`character_index_for_point` → 用 P31 的文本命中。
- 焦点字段的光标位置变化时调用 `window.invalidate_character_coordinates()`。
- `<input>` 内的组字效果（下划线）由本包画；`contenteditable` 的组字由 P24 负责 DOM 侧，绘制仍由本包按 `marked` 范围画下划线。

### 延迟（乐观回显）

键入到屏幕的延迟必须低于一帧。渲染器在另一线程，正常情况下一次 `Key` → 渲染器处理 → 发布新快照 < 1 ms，无需特别处理。
但渲染器忙（长任务）时：Shell 在本地对焦点字段的 `Draft` 副本应用同样的编辑并立即重画，直到渲染器的快照的
`applied`（P02 `RendererView.applied`）追上该输入的序号，再丢弃本地副本（su-editor `live.rs` 同款）。页面 `keydown`
里 `preventDefault` 的情况会在追上后被纠正，可接受。

### 指针样式

`Hitbox.cursor` → GPUI `CursorStyle`：`default/auto`→`Arrow`、`pointer`→`PointingHand`、`text`→`IBeam`、`crosshair`→`Crosshair`、
`grab`→`OpenHand`、`grabbing`→`ClosedHand`、`ew-resize`→`ResizeLeftRight`、`ns-resize`→`ResizeUpDown`、`col-resize`→`ResizeColumn`、
`row-resize`→`ResizeRow`、`nwse-resize`→`ResizeUpLeftDownRight`、`nesw-resize`→`ResizeUpRightDownLeft`、`not-allowed/no-drop`→`OperationNotAllowed`、
`alias`→`DragLink`、`copy`→`DragCopy`、`context-menu`→`ContextualMenu`、`vertical-text`→`IBeamCursorForVerticalLayout`；
GPUI 的 `CursorStyle` 没有"隐藏"，`cursor: none` 暂按 `Arrow`（记录；需要时在 fork 加 `Hidden`）；`url(...)` 自定义光标不支持。
在 `prepaint` 里对每个 hitbox 调 `window.set_cursor_style(style, &hitbox)`。

### 第一阶段契约变更（输入桥；数值取自 Electron 39.8.10 的实测）

下面几条改动或补充上文，以这里为准。实测在隐藏窗口、临时 profile 下用 `webContents.sendInputEvent` 注入，不碰桌面。

- **事件字段**：`pointer*` 事件的 `detail` 恒为 0，`mouse*`/`click`/`dblclick`/`auxclick` 的 `detail` 是点击数（1、2、3；`dblclick` 是 2）；
  `contextmenu` 的 `detail` 为 0。按下时 `buttons` 是位掩码（左 1、右 2、中 4），抬起后为 0；移动时带着仍按住的按键。
  右键抬起的顺序是 `pointerup, mouseup, auxclick, contextmenu`；中键是 `pointerup, mouseup, auxclick`。
  第一次进入元素：`pointerover, mouseover, pointermove, mousemove`；换元素时先 `pointerout, pointerover, mouseout, mouseover`。
- **不做隐式指针捕获**：Chromium 的鼠标事件没有隐式捕获，`mouseup` 落在抬起时指针下的元素，`click` 落在按下与抬起共同的祖先上
  （verve-dom 的 `release` 已是这样）。所以 Shell 不固定目标：每个 `Pointer` 都带着它此刻命中的节点。GPUI/系统层的捕获
  （按住时指针移出窗口仍收到事件，Windows 的 `SetCapture`）照旧。`setPointerCapture` 是页面里的事，不经过 Shell。
- **`verve_dom::Pointer` 追加 `buttons: u16`**（按住的键的位掩码；`Default` 为 0）。移动事件用它填 `buttons`。
- **滚轮**：页面看到的 `deltaY` 是注入值的相反数（注入 `deltaY: 120` 向上滚，页面得到 `deltaY: -120`、`wheelDelta: 120`），
  `deltaMode` 恒为 0，所以 `ScrollDelta::Pixels(p)` → `deltaX = -p.x`、`deltaY = -p.y`，`Lines` 按 `LINE`（Windows 100/3、macOS 40）换成像素。
  Shift+竖直滚轮：页面收到的仍是 `deltaY`，`shiftKey: true`，默认动作是横向滚动；GPUI 在 Windows 上已把它换成 `x`，Shell 换回 `dy` 再发。
  Ctrl+滚轮：页面先收到 `wheel`（`ctrlKey: true`，可取消），没被阻止才发 `zoom-changed`（页面 `deltaY < 0` 即向上滚为 `'in'`，否则 `'out'`），
  不滚动，也不改变缩放系数；要不要缩放是应用自己的事。注入的一次滚轮事件让 `zoom-changed` 触发了两次，Verve 每个滚轮事件只发一次。
  物理滚轮每一格滚多少无法用注入测得，仍按 P30 的 `LINE`。
- **消息**：Shell → 主进程新增 `FromShell::Page { contents, event: PageEvent }`，`PageEvent` 追加 `CursorChanged(String)`、
  `UpdateTargetUrl(String)`、`ZoomChanged { zoom_in: bool }`；它们变成 `webContents` 的 `cursor-changed`、`update-target-url`、`zoom-changed`
  （参数都是 `(event, 值)`）。`zoom-changed` 由渲染器在 `wheel` 没被阻止时经 `FromRenderer::Lifecycle` 发出，另两个由 Shell 命中后发。
  `RendererView.cursor` 不再使用：光标由 Shell 从 `Hitbox.cursor` 取得，渲染器并不知道指针在哪里。
- **光标名**（`cursor-changed` 的 `type`）：`auto`/`default` → `pointer`（箭头叫 `pointer`）；`pointer` → `hand`；`text`、`crosshair`、`grab`、`grabbing`、
  `ew-resize`、`ns-resize`、`col-resize`、`row-resize`、`nwse-resize`、`nesw-resize`、`e-resize`、`n-resize`、`not-allowed`、`alias`、`copy`、`context-menu`、
  `vertical-text`、`none`、`move`、`wait`、`progress`、`help`、`zoom-in`、`zoom-out`、`cell` 同名；`no-drop` → `nodrop`；`all-scroll` → `move`。
  只在名字变化时发。`update-target-url`：指针进入 `a[href]` 时是解析后的地址，离开时是空串。
- **按键**：`before-input-event` 先于页面的 `keydown`，`keyUp` 也发；`key`/`code` 见 `keymap.rs` 的表（测得：`Enter/Enter`、`Tab/Tab`、`" "/Space`、
  `ArrowLeft/ArrowLeft`、`F1…F24`、`1/Digit1`、`=/Equal`、`-/Minus`、`,/Comma`、`./Period`、`//Slash`、`;/Semicolon`、`[/BracketLeft`、`]/BracketRight`、
  `\\/Backslash`、`` ` ``/`Backquote`、`'/Quote`；`shift+a` 是 `A/KeyA`，`shift+1` 是 `!/Digit1`；单独的修饰键是 `Control/ControlLeft`、`Shift/ShiftLeft`、`Alt/AltLeft`、`Meta/MetaLeft`）。
  可打印键之后依次是 `keypress, beforeinput, textInput, input`；`ctrl+a` 没有 `keypress`。
- **Windows 上的字符只来一次**：GPUI 在 Windows 上先给 `KeyDown`，随后 `WM_CHAR` 又经输入处理器的 `replace_text_in_range(None, 字符)` 送来同一个字符。
  Shell 把可打印键作为 `Key` 发给页面（verve-dom 在 `keydown` 之后自己输入字符），并记下这个字符，输入处理器收到同一个字符时丢掉；
  输入法提交的文字不是刚发出的按键字符，照常作为 `Composition(Commit)`/`Text` 发。
- **`:focus-visible`**（实测）：点击按钮不匹配，点击文本框匹配；Tab 到的元素匹配；脚本 `focus()` 在最近一次是键盘操作后匹配，
  在鼠标点了按钮之后聚焦文本框匹配、聚焦按钮不匹配；鼠标聚焦的按钮上按一个键后匹配。verve-dom 记一个“焦点是否可见”的标志，按这些规则维护。
- **缩放**：Shell 里还没有页面缩放（`setZoomFactor` 只记在主进程），坐标按系数 1 换算，`DomState.zoom` 留给缩放接入时用。

### 表单控件

控件在 `verve-layout` 里是原子盒，固有尺寸按 Chromium 默认（`input` 文本 ≈ 153×21 CSS px 含边框、checkbox 13×13、
`select` 高 19 …，以真实 Electron 量出来为准写成常量表）。显示列表出 `PaintItem::Control { node, bounds, kind }`：

```rust
pub enum ControlKind {
    TextField { draft: Draft, placeholder: Option<String>, password: bool, multiline: bool, scroll: [f32; 2], focused: bool },
    Checkbox { checked: bool, indeterminate: bool }, Radio { checked: bool },
    Button { label_inline: bool /* 内容由普通行内排版画 */ },
    Select { label: String, open: bool }, Range { min: f64, max: f64, value: f64 },
    Progress { value: Option<f64> /* None = 不确定 */ }, Meter { value: f64, low: f64, high: f64, optimum: f64 },
    Color { value: [u8; 3] }, Date { text: String }, File { label: String },
}
```

- 页面写了 `appearance: none` 或给控件设置了背景/边框时，按普通盒子画（P30），只保留文字、光标、选区。
- 外观：v1 用一套与 Windows 11 Chromium 默认相近的平面风格（不追求平台原生），配色随 `prefers-color-scheme` 和 `accent-color`。
- 文本字段：文字按 P31 规则单行/多行排版，光标闪烁（530 ms，失焦停止），选区用 `::selection` 颜色，水平/垂直滚动保持光标可见，
  `password` 显示 `•`。
- `<select>` 下拉：Shell 用 GPUI `deferred(...).with_priority(..)` 在窗口内画弹层（选项列表来自快照），选择后发
  `ToRendererInput` 的新变体 `Choose { select: NodeId, value: String }`（**追加到 P02 契约**）→ `Document::choose`。
  超出窗口时 v1 夹在窗口内（不开新系统窗口）。
- `type=date/time/color/file`：点击时分别交给 P14 的系统对话框（`file` 用打开对话框；`color`/`date` v1 用窗口内简单弹层）。
- `range`：拖动滑块直接 `edit_field` 写 `value` 并发 `input`，松开发 `change`。
- 焦点环：`:focus-visible` 匹配（键盘导航后）时画 2px `outline`（颜色 `accent-color` 或系统强调色），由 P22 的 UA 样式表给出，本包只保证 Tab 导航能触发 `:focus-visible`。

### 文件拖入

`FileDropEvent::{Entered, Pending, Submit, Exited}` → `dragenter/dragover/drop/dragleave`（P24 负责 `DataTransfer` 内容，本包负责事件来源和坐标）。
`File` 对象的路径通过 `webUtils.getPathForFile`（P21）取得。

## 步骤

1. `keymap.rs` + 单测（表格每一行）。
2. `input.rs`：GPUI 事件 → `ToRendererInput`；指针捕获；Shell 侧命中。渲染器侧分发到 verve-dom。
3. 指针样式与 `RendererView.cursor`。
4. `ImeBridge`（`EntityInputHandler`）与 `ime_rect`；中文微软拼音、日文 IME 手测。
5. `verve-layout` 的控件固有尺寸与 `ControlKind` 生成；`controls.rs` 绘制。
6. `<select>` 弹层、`range` 拖动、`file` 对话框接 P14。
7. 乐观回显。

## 验收

- `cargo test -p verve-gpui --test keymap`。
- `cargo test -p verve-electron --test input`（无头，直接注入 `ToRendererInput`）：点击按钮 → `pointerdown, mousedown, pointerup, mouseup, click` 顺序；
  双击 → `dblclick` 且 `detail===2`；`shift+a` 在 input 里得到 `A` 且 `keydown.key==='A'`、`code==='KeyA'`；组字 `Start/Update('ni')/End('你')`
  后 input 值为 `你` 且触发 `compositionstart/compositionupdate/compositionend` 和一次 `input`（`isComposing` 正确）；`<select>` 选择触发 `input`+`change`。
- 夹具 `tests/electron/apps/forms/`：每种控件一个；手动验收清单（截图附 PR）：微软拼音输入"加载"无残留拼音、候选框位于光标下、
  Tab 在控件间移动并出现焦点环、checkbox 点击切换、range 拖动。
- 渲染器里跑一个 300 ms 的忙循环期间连续键入，屏幕即时回显，忙完后值一致。

## 不做

触摸与手写笔事件（`touch*`、`pressure`）、`<input type=week/month>`、平台原生外观控件、`::-webkit-*` 控件伪元素样式、
系统级下拉弹窗（新窗口）、自定义 `url()` 光标、`inputmode`/虚拟键盘、`showPicker()` 之外的日期选择细节。

## 状态

（实现者完成后填写）
