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
`globalShortcut` 由系统层处理；菜单加速键在页面之后动，只在页面没处理这个 key down 时（见“第一阶段契约变更”的“菜单加速键的顺序”，Electron 39.8.10 观察）。

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

- **事件字段**：`pointer*` 事件的 `detail` 恒为 0；`mousedown`/`mouseup`/`click`/`auxclick` 的 `detail` 是点击数（1、2、3；`dblclick` 是 2）；
  `mousemove`/`mouseover`/`mouseout`/`wheel`/`contextmenu` 的 `detail` 为 0（p32-review 的 `mouse` 探针）。`pointermove`/`pointerover`/`pointerout` 的 `button` 是 -1，鼠标事件是 0。
  按下时 `buttons` 是位掩码（左 1、右 2、中 4、后退 8、前进 16），抬起后为 0；移动时带着仍按住的按键。
  右键抬起的顺序是 `pointerup, mouseup, auxclick, contextmenu`；中键是 `pointerup, mouseup, auxclick`。
  第一次进入元素：`pointerover, mouseover, pointermove, mousemove`；换元素时先 `pointerout, pointerover, mouseout, mouseover`。
- **不做隐式指针捕获**：Chromium 的鼠标事件没有隐式捕获，`mouseup` 落在抬起时指针下的元素，`click` 落在按下与抬起共同的祖先上
  （verve-dom 的 `release` 已是这样）。所以 Shell 不固定目标：每个 `Pointer` 都带着它此刻命中的节点。GPUI/系统层的捕获
  （按住时指针移出窗口仍收到事件，Windows 的 `SetCapture`）照旧。`setPointerCapture` 是页面里的事，不经过 Shell。
- **`verve_dom::Pointer` 追加 `buttons: u16`**（按住的键的位掩码；`Default` 为 0）。移动事件用它填 `buttons`。
- **滚轮**：页面看到的 `deltaY` 是注入值的相反数（注入 `deltaY: 120` 向上滚，页面得到 `deltaY: -120`、`wheelDelta: 120`），
  `deltaMode` 恒为 0，所以 `ScrollDelta::Pixels(p)` → `deltaX = -p.x`、`deltaY = -p.y`，`Lines` 按 `LINE`（Windows 100/3、macOS 40）换成像素。
  Shift+竖直滚轮：页面收到的仍是 `deltaY`，`shiftKey: true`，默认动作是横向滚动；GPUI 在 Windows 上已把它换成 `x`，Shell 换回 `dy` 再发。
  Ctrl+滚轮（竖直）：页面先收到 `wheel`（`ctrlKey: true`，可取消），没被阻止才发 `zoom-changed`（Ctrl+横向滚轮不发）（页面 `deltaY < 0` 即向上滚为 `'in'`，否则 `'out'`），
  不滚动，也不改变缩放系数；要不要缩放是应用自己的事。注入的一次滚轮事件让 `zoom-changed` 触发了两次，Verve 每个滚轮事件只发一次。
  物理滚轮每一格滚多少无法用注入测得，仍按 P30 的 `LINE`。
- **消息**：Shell → 主进程新增 `FromShell::Page { contents, event: PageEvent }`，`PageEvent` 追加 `CursorChanged(String)`、
  `UpdateTargetUrl(String)`、`ZoomChanged { zoom_in: bool }`；它们变成 `webContents` 的 `cursor-changed`、`update-target-url`、`zoom-changed`
  （参数都是 `(event, 值)`）。`zoom-changed` 由渲染器在 `wheel` 没被阻止时经 `FromRenderer::Lifecycle` 发出，另两个由 Shell 命中后发。
  `RendererView.cursor` 不再使用：光标由 Shell 从 `Hitbox.cursor` 取得，渲染器并不知道指针在哪里。另追加：`PointerKind::Leave`（指针离开窗口）、
  `FromRenderer::KeyUnhandled(Key)`（页面没处理的 key down，见“按键”），`FromShell::Page` 追加在枚举末尾。
- **光标名**（`cursor-changed` 的 `type`）：`auto`/`default` → `pointer`（箭头叫 `pointer`）；`pointer` → `hand`；`text`、`crosshair`、`grab`、`grabbing`、
  `ew-resize`、`ns-resize`、`col-resize`、`row-resize`、`nwse-resize`、`nesw-resize`、`e-resize`、`n-resize`、`not-allowed`、`alias`、`copy`、`context-menu`、
  `vertical-text`、`none`、`move`、`wait`、`progress`、`help`、`zoom-in`、`zoom-out`、`cell` 同名；`no-drop` → `nodrop`；`all-scroll` → `move`。
  只在名字变化时发。`update-target-url`：指针进入 `a[href]` 时是解析后的地址，离开时是空串。
- **按键**：`before-input-event` 先于页面的 `keydown`，`keyUp` 也发；`key`/`code` 见 `keymap.rs` 的表（测得：`Enter/Enter`、`Tab/Tab`、`" "/Space`、
  `ArrowLeft/ArrowLeft`、`F1…F24`、`1/Digit1`、`=/Equal`、`-/Minus`、`,/Comma`、`./Period`、`//Slash`、`;/Semicolon`、`[/BracketLeft`、`]/BracketRight`、
  `\\/Backslash`、`` ` ``/`Backquote`、`'/Quote`；`shift+a` 是 `A/KeyA`，`shift+1` 是 `!/Digit1`；单独的修饰键是 `Control/ControlLeft`、`Shift/ShiftLeft`、`Alt/AltLeft`、`Meta/MetaLeft`）。
  可打印键之后依次是 `keypress, beforeinput, textInput, input`；`ctrl+a` 没有 `keypress`。
- **Windows 上的字符只来一次**：GPUI 在 Windows 上先给 `KeyDown`，随后 `WM_CHAR` 又经输入处理器的 `replace_text_in_range(None, 字符)` 送来同一个字符。  Shell 把可打印键作为 `Key` 发给页面（verve-dom 在 `keydown` 之后自己输入字符），并把这个字符记进一个队列（只有文字来了才取走，key up 不清；  GPUI 在忙时会先处理后面的按键再处理已投递的 `WM_CHAR`，所以不能只记一个），输入处理器收到队列里的字符就丢掉；焦点字段变了就清空队列。  输入法提交的文字不是刚发出的按键字符，照常作为 `Composition(Commit)`/`Text` 发。
- **Shift 取自窗口**：GPUI 在 Windows 上把 `shift+1` 变成键 `!` 时会从 `Keystroke.modifiers` 里去掉 shift，所以 `Key.modifiers` 取自窗口当前按住的修饰键。
- **死键**：死键（`prefer_character_input` 且没有 Ctrl/Alt/Meta）发 `key: "Dead"`、不记入队列；组合出的字母（`é`）作为下一个键到达并由页面输入，AltGr 的字符只经文本到达。
- **菜单加速键的顺序（改了 P15 的约定，见 P15 的备注）**：页面先听到键；页面没处理这个 key down（没取消，且默认动作只是输入字符或什么也没做）才查菜单并点击。  观察（Electron 39.8.10，可见的屏幕外窗口，总控的 `probe-keys` 探针）：Ctrl+O/F6 页面先收到 keydown、keyup，随后菜单项被点击；页面取消 keydown 则不点击；  `before-input-event` 取消 keyDown 则页面没有 keydown 也没有 keyup，不点击，也没有 keyUp 的 `before-input-event`；除此之外什么也不吞。  没处理（点击）：空 input 里的 Ctrl+Z、普通 input 里的 Ctrl+B、body 上的 Ctrl+Left 和 Backspace、input 里的普通字母加速键（字符照样输入）；  处理了（不点击）：Ctrl+A、输入过后的 Ctrl+Z、input 里的 Ctrl+Left 和 Backspace。路径：键 → `before-input-event`（有监听时）→ 页面 → `FromRenderer::KeyUnhandled` → 主进程查菜单（`input_events::key_unhandled`）。macOS 没观察，规则相同。
- **`:focus-visible`**（实测，p32-review 的 `focus`、`focus2` 探针）：点击按钮不匹配，点击文本框匹配；Tab 到的元素匹配；脚本 `focus()` 在最近一次是键盘操作后匹配，  在点了空白处之后匹配，在鼠标点了按钮之后聚焦文本框匹配、聚焦按钮不匹配；点击已有可见焦点环的按钮不改变环；从鼠标聚焦的按钮出发，Shift 单独、Shift+A、F5、Escape、  普通字母之后环出现，Control 单独、Alt 单独、Ctrl+A、Alt+A、Meta+A、Ctrl+Shift+A 之后不出现。右键/中键按下也移动焦点。verve-dom 记 `Dom.focus_visible` 与（crate 内部的）`LastInput`。
- **缩放**：Shell 里还没有页面缩放（`setZoomFactor` 只记在主进程），坐标按系数 1 换算；接入时在 `input::local` 里除以系数。

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

### 阶段 1：输入桥（已完成；控件、`<select>`、`range`、文件拖入、乐观回显属阶段 2，未开始）

**做了什么**

- `keymap.rs`（`verve_gpui::keymap`，公开）：GPUI 的 `Keystroke`/`Modifiers` → `verve_dom::Key`/`Modifiers`，修饰键的按下/抬起由前后两次状态推出
  （`ModifiersChangedEvent`）。表里的每一行都有测试（`cargo test -p verve-gpui --test keymap`），数值取自 Electron 39.8.10 的实测。
- `input.rs`：GPUI 的按下/抬起/移动/离开/滚轮/按键 → `ToRendererInput`。目标节点由 Shell 在 `Plan.hitboxes`（`resolved_hitboxes` 的结果）里找，
  `click_count` → `Pointer.count`，按住的键 → `Pointer.buttons`，Navigate 键 → 3/4。映射是不需要窗口的纯函数，单测直接构造 GPUI 的事件结构。
  `KeyRoute` 是按键的路径：页面有 `before-input-event` 监听时先发 `FromShell::KeyForContents`，否则直接交给页面；菜单加速键不在这里查——页面先听到键，
  页面没处理的 key down 由渲染器报 `FromRenderer::KeyUnhandled`，主进程查菜单并点击（顺序与理由见上“菜单加速键的顺序”，P15 的备注已改）。
- `wheel.rs`：页面看到的 `deltaX/deltaY` 是内容移动量的相反数；Shift+滚轮在 Windows 上被 GPUI 换成了横向，Shell 换回 `deltaY` 并让渲染器横向滚动；
  Ctrl+滚轮不滚动、不预测，渲染器在页面没阻止 `wheel` 时发 `zoom-changed`（`in`/`out`）。
- 光标：`cursor.rs` 把 `Hitbox.cursor` 变成 GPUI 的 `CursorStyle`（在 `paint` 里 `set_cursor_style`，这点沿用 P30），`auto` 在 Shell 里解析
  （链接 → 手，文本框/可编辑区 → I-beam，按快照缓存）。`cursor-changed`（Electron 的名字：箭头叫 `pointer`、手叫 `hand`、`no-drop` 叫 `nodrop`）和
  `update-target-url`（链接地址按页面的 base 解析，离开时为空串）由 Shell 在指针移动时发，只在变化时发；经新的 `FromShell::Page`。
- 输入法：`ime.rs` 的 `ImeBridge` 是 `EntityInputHandler`，只转发：`replace_and_mark_text_in_range` → `Composition(Update)`，
  组字中的 `replace_text_in_range` → `Composition(Commit)`，否则 → `Text`，清空 → `Cancel`；`text_for_range`/`selected_text_range`/`marked_text_range` 读最近一份快照里
  焦点字段的 `Draft`（UTF-8 ↔ UTF-16 用 `Draft` 自带的换算）；`bounds_for_range` 用 `ime_rect`；`ime_rect` 或焦点字段变了就 `invalidate_character_coordinates`。
  只有焦点在可输入的元素上时 `paint` 才登记输入处理器（`handle_input`）。
- 焦点：窗口焦点 → `ToRendererInput::Focus`（原来就有，渲染器现在交给 `Document::set_window_focus`）；`DomView` 持有 GPUI 的 `FocusHandle`，
  窗口打开时聚焦它，点击页面时重新聚焦；Tab/Shift+Tab 由 verve-dom 移动焦点；`:focus-visible` 有了焦点来源（verve-dom 的 `LastInput`）。
- 让位于 P43 的拾取：拾取层存在时（`DomState.picking`）桥不转发指针和滚轮。
- verve-dom：`Pointer.buttons`；`pointer*` 事件和 `contextmenu` 的 `detail` 为 0（测得），右键抬起先 `auxclick` 再 `contextmenu`（测得）；
  Navigate 键只发 `auxclick`。
- 验收：`cargo test -p verve-electron --test input`（无头）：点击顺序、`dblclick` 的 `detail === 2`、其余键的 `auxclick`/`contextmenu`、
  `shift+a` → `A`/`KeyA`、组字提交、Tab 与 `:focus-visible`、窗口焦点、Shift/Ctrl+滚轮、`cursor-changed`/`update-target-url`。

**没做 / 只做了一部分**

- **`wheel` 在 window/document/html/body 上默认被动**（Chromium 如此）：内核的 `addEventListener` 只认显式的 `passive`，没有按目标给默认值，
  所以这些节点上的 `wheel` 监听器 `preventDefault()` 仍能阻止滚动。需要内核知道事件目标是不是 window/document/html/body，没在本阶段做。
- **Navigate 键的 `app-command`**（`browser-backward/forward`）：P13 没有这个事件的来源，只发了按钮 3/4 给页面。
- **`character_index_for_point` 返回 `None`**：点到文本位置要问渲染器（`Document::text_position_at`），而平台是同步问的。
  后果：重新转换/触摸屏上的某些输入法定位不准，微软拼音的普通输入不受影响。
- **组字下划线**：`<input>` 内预编辑文字的下划线（`Draft` 的 `marked` 范围）要由文本字段的绘制来画，属阶段 2 的 `controls.rs`，现在预编辑文字显示了但没有下划线。
- **文本上的 I-beam**：`auto` 光标只在链接和文本框上解析，普通文字上仍是箭头。
- **`cursor: none`** 按箭头显示（GPUI 没有隐藏光标），自定义 `url(...)` 光标不支持；`cursor-changed` 不带图片参数。
- **页面缩放**：Shell 里还没有缩放，坐标系数恒为 1；`setZoomFactor` 接入后要在 `input::local` 里除以它（没有 `DomState.zoom` 字段）。
- **`code` 按 US 布局推断**；Windows 的 AltGr（Ctrl+Alt）的字符只经文本到达页面（`keydown` 的 `key` 是 GPUI 给的字符）；死键发 `Dead`，其 key up 的 `key` 是重音字符本身（GPUI 的 key up 不带 `prefer_character_input`）。
- `before-input-event` 的 `isComposing` 恒为 `false`（原有）。
- 已知的 Chromium 差异：`setPointerCapture` 的实测在隐藏窗口里卡死了探针（没有结论），所以页面级指针捕获没有做进桥；只做了系统层的鼠标捕获（GPUI 已有）。
- macOS/Linux 没有实测：`WM_CHAR` 去重（`Input.typed`）是按 Windows 的行为写的，其他平台的文字到达路径若不同，要看 `replace_text_in_range` 是否重复输入。

**第二轮（复查与总控的修改）**

- 改了：Shift 取自窗口（GPUI 在 Windows 上会从 `Keystroke` 里清掉 shift）；`WM_CHAR` 去重改成队列；死键发 `Dead`；悬停和滚轮用 Shell 命中的节点（`pointer_move_on`、`wheel_on`）；
  焦点环规则按观察重写（修饰键、点空白处、右/中键聚焦）；`Document` 的指针/键盘新代码放进 `person.rs`（`lib.rs` 从 1634 行降到约 1523 行），`detail`/`button` 按事件类别给，边界事件顺序同 Chromium，Alt+字母不再输入；
  菜单加速键改为页面先听到键、没处理才点击（`KeyUnhandled`，P15 文档同步），三份吞键规则合成主进程里的一份，`Control::menu_item_for_key` 删除；`before-input-event` 取消的 key down 不再有 key up；
  `FromShell::Page` 移到枚举末尾；Ctrl+横向滚轮不发 `zoom-changed`；候选框（单行输入框）放在输入框下边缘；`ImeBridge` 的组字状态记最后一次更新的文字，焦点字段变了就作废；
  光标的“文本框”判断与 Chromium 一致（只读和未知 `type` 也是 I-beam），链接的 `href` 查找不再解析地址，指针留在同一节点上时不再重算；Shift→`deltaY` 的换算只在 Windows 的行滚动上做；
  `sendInputEvent` 的 `leftButtonDown` 等修饰键填进 `buttons`；测试：组字序列、被取消的 Tab、`contextmenu` 的字段、菜单键的五个测试。
- **没改（留到阶段 2 或以后）**：
  - 拆分 `55ef5e820`：要重写历史，由总控处理。
  - `url` 依赖换成 verve-dom 的辅助函数：不省体积，没做。
  - 候选框跟着插入符水平移动、`<textarea>` 的候选框位置：要渲染器量出插入符的矩形（`caret_rect_of` 现在返回整个框），超过本轮范围。
  - `replace_text_in_range` 的范围参数（macOS 长按重音）：macOS 没验证。
  - 滚轮在 Shell 里命中两次（`input::hit` 与 `DisplayList::wheel`）：每次滚轮一次，不是每次移动。
  - `auto_cursors` 仍在每个快照版本清空（现在每个节点的开销是一次 `href` 属性查找）。
  - Ctrl+滚轮按整格累计才缩放（Chromium 从源码看是这样，没有观察到）：精确触摸板会连发。
  - 把 `Page.window` 之类死字段、`RendererView.cursor` 字段本身删掉：字段保留（避免动 P24 也在改的 `RendererView`），文档里标为不再使用。
  - GPUI `test-support`：阶段 2。

**给 GPUI 测试平台的发现**：GPUI 的 `test-support` 特性有 `TestAppContext`/`VisualTestContext`（`simulate_event`、`simulate_keystrokes`、`simulate_input`、
`simulate_mouse_down` 等），在内存里驱动窗口，不碰桌面。把 `DomView` 放进测试窗口、注入 `MouseDownEvent` 和输入法调用，就能自动测 `ImeBridge` 和
`KeyRoute`（现在这两个只能靠手测）。代价：给 `verve-gpui` 的 dev-dependencies 加 `gpui` 的 `test-support`（拉进 `collections`/`http_client` 的测试支持，编译时间变长），
再写一个小的测试夹具；估计 200 行以内。没有加。

**测试命令**（我只跑了 `cargo check`）

    cargo test -p verve-dom --test input
    cargo test -p verve-gpui --test keymap
    cargo test -p verve-gpui --lib --features jit
    cargo test -p verve-electron --test input
    cargo test -p verve-electron --test headless --test web_contents --test menu

**手测清单**（在真窗口里，需要一个人；Windows）

1. 打开有链接和按钮的页面：点链接，页面导航；点按钮，`click` 触发；双击一个带 `dblclick` 监听的元素，监听里 `detail === 2`。
2. 在 `<input>` 里输入英文、大写（Shift+a）、数字、标点；退格、Delete、方向键、Home/End 工作；输入的每个字符只出现一次。
3. Tab / Shift+Tab 在按钮、输入框、链接间移动，焦点环出现（键盘导航后）；用鼠标点按钮不出现焦点环，点输入框出现。
4. 滚轮滚动页面和内层可滚动区域；Shift+滚轮横向滚动；Ctrl+滚轮不滚动（应用监听 `webContents.on('zoom-changed')` 时能收到）。
5. 光标：悬停链接是手，悬停输入框是 I-beam，`cursor: crosshair`/`not-allowed`/`col-resize` 的元素上形状对应；`webContents.on('cursor-changed')` 收到 `hand`/`text`/`pointer`；
   悬停链接时 `update-target-url` 收到地址，移开收到空串。
6. 菜单加速键：设了 `Ctrl+O` 之类加速键的菜单，在页面里按下，页面先收到 `keydown`/`keyup`，菜单项随后触发；页面在 `keydown` 里 `preventDefault()` 则菜单项不触发；在输入框里按 `Ctrl+A` 是全选、不触发菜单项，`Ctrl+B`（没有页面用途）触发。给 `webContents` 加 `before-input-event` 监听后，监听先收到键，`preventDefault()` 后页面和菜单都收不到（keyup 也不再到监听器和页面）。
7. 微软拼音：在 `<input>` 里输入“jiazai”，选“加载”：输入框里没有残留拼音、预编辑文字带下划线（下划线绘制属阶段 2）、候选框在输入框的下边缘（不是输入框的一半高度处；还不跟着插入符水平移动）；
   提交后只有一次 `input`，`compositionstart/update/end` 各自出现。Esc 取消组字后输入框是空的。
7a. 死键布局（美式国际、德语、法语等，Windows 设置里加一个）：输入 `´` 再输入 `e`，输入框里只有 `é`（不是 `´é`）；输入 `´` 再输入 `x`，得到 `´x`；`keydown` 的 `key` 先是 `Dead` 再是 `é`。
7b. Shift+数字和标点（`!`、`:`、`+`）：页面 `keydown` 的 `shiftKey` 为 `true`；Ctrl+Shift+`=` 触发带 `CommandOrControl+Plus` 加速键的菜单项。
7c. 在输入框里很快地连敲一串字符（或让页面忙一会儿时敲）：每个字符只出现一次。
   提交后只有一次 `input`，`compositionstart/update/end` 各自出现。Esc 取消组字后输入框是空的。
8. 窗口失焦再回来：`document.hasFocus()` 先假后真，`window` 的 `blur`/`focus` 事件各一次。
9. 打开检查器的元素拾取：拾取期间页面收不到鼠标事件，退出后恢复。
