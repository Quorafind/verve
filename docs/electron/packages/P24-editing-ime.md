# P24 contenteditable、输入法、拖放与剪贴板事件

**依赖**：P23（实时 Range 依赖 `Dom` 的变更通知；shadow 内编辑依赖扁平树）  **规模**：L  **修改**：`crates/verve-dom/src/{lib.rs,dom.rs,state.rs,field.rs,bind/platform.rs(Range/Selection),bind/document.rs(execCommand),bind/events.rs,bind/window.rs(clipboard)}`；新增 `crates/verve-dom/src/{selection.rs,editing.rs,drag.rs}`；`crates/verve-electron/src/renderer_guest.rs`（对接 P02 消息）

## 目标

1. 富文本编辑器能用：ProseMirror/Tiptap、Lexical、Slate、Quill、CodeMirror 6（它用 contenteditable + `beforeinput`）在
   verve-dom 里接收键入、删除、回车、选区移动、撤销重做、粘贴。
2. 中文/日文输入法：`<input>`/`<textarea>` 和 contenteditable 都有正确的 `compositionstart/update/end` 序列和
   `isComposing`，候选框跟随光标（`RendererView.ime_rect`）。
3. 拖放：页面内 HTML5 拖放；从系统拖文件进窗口得到带路径的 `File`（`webUtils.getPathForFile` 可取）。
4. 剪贴板事件：`copy/cut/paste`（`clipboardData`）、`navigator.clipboard.readText/writeText/read/write`。

## 背景（先读）

- `crates/verve-dom/README.md`「Not yet」：没有组合事件（输入法文字作为已提交文本经 `insert_text` 到达）、
  不能在 contenteditable 里键入、界面不提供拖放。
- `crates/verve-dom/src/field.rs`：`Draft`（`<input>/<textarea>` 的文本、caret、anchor）——**已有**输入法支持：
  `marked()`、`replace_and_mark_utf16`、`replace_utf16`、`unmark`（UTF-16 偏移，平台的计数方式）及测试。
- `crates/verve-dom/src/lib.rs`：`Document::key_down/key_up/type_text/insert_text/edit_field/draft/commit_field`，
  内部 `key_target()`、`is_text_field()`、`insert_into(runtime, target, text, input_type)`（约 1596/1652 行设置 `inputType`）。
- `crates/verve-dom/src/bind/platform.rs`：`Range`（约 995 行起，`startContainer` 等是**普通 JS 属性**，不随 DOM 变更更新）、
  `Selection`（约 1341 行 `selection()`，范围存在全局隐藏对象 `__selection.__ranges`；`extend/modify/collapseToStart/deleteFromDocument/containsNode` 是空操作，约 1500 行）。
- `crates/verve-dom/src/bind/document.rs`：`execCommand`（约 264 行，只有 `copy`）、`queryCommand*`。
- `crates/verve-dom/src/bind/events.rs`：已有 `InputEvent.inputType` 字段、`ClipboardEvent`、`CompositionEvent` 类名、事件名表。
- `crates/verve-dom/src/bind/window.rs`：`navigator.clipboard`（约 908–960 行，写操作变成 `Request::CopyText`）。
- `crates/verve-dom/src/css.rs` 的 `editable()`：`contenteditable` 继承判断（`:read-write` 用）。
- P02：`ToRendererInput::{Key, Text, Composition(CompositionEvent), Pointer}`、`RendererView.ime_rect`。

## 接口契约

### 1. 实时 Range 与 Selection（`selection.rs`，需新增，替换 `bind/platform.rs` 的 JS 属性实现）

```rust
// 复用 P31 的类型（layout_provider.rs），不再另定义：offset 是文本节点内 UTF-16 码元，其他节点内为子节点下标。
pub type Boundary = (NodeId, usize);
pub struct RangeData { pub start: Boundary, pub end: Boundary }
/// 住在 `Dom` 里（`dom/ranges.rs`）：脚本持有的每个 Range（弱引用，对象被回收时槽位释放）与文档的选区。
pub struct Ranges { /* slab of (RangeData, refs), selection slot, direction */ }
```

- 实时 Range 的数据放在 `Dom` 里而不是 `State`：`Dom` 的每个变更入口（`detach`、`place`、`empty`、`set_text`、`replace_data`、`split_text`、
  `normalize`）直接调整边界，快照克隆 `Dom` 时选区随之带走（`Ranges` 的 `Clone` 只留选区，不复制脚本持有的其余 Range，也不共享回收队列），
  所以 `SnapshotData` 不另加 `selection` 字段——P31 实际经 `Document::selection_boundaries` 现读选区（`Snapshot.dom.selection()` 也可用），光标用 `Snapshot.dom.selection_focus()`
  （一个 `Boundary`；是否可见由 P32 的闪烁决定）。没有 `Caret` 类型。
- `Range` 对象内部持有 slab 下标；`startContainer/startOffset/…` 为访问器，读 Rust 状态。
- **随 DOM 变更更新**（DOM 规范"live range"规则）：插入/删除节点、`splitText`、文本替换（`CharacterData.replaceData`）、
  `normalize`——在 `Dom` 的变更入口统一调用 `Ranges::on_*`（P23 的 `tree_revision` 处一并接入）。
- `Range` 完整方法：`setStart/End/Before/After`、`selectNode(Contents)`、`collapse`、`cloneRange`、`compareBoundaryPoints`、
  `comparePoint`、`isPointInRange`、`intersectsNode`、`deleteContents`、`extractContents`、`cloneContents`、`insertNode`、
  `surroundContents`、`toString`、`getBoundingClientRect/getClientRects`（依赖 P31 回报的文本片段，见下）、`createContextualFragment`。
- `Selection`：`anchorNode/Offset`、`focusNode/Offset`、`isCollapsed`、`rangeCount`、`type`、`getRangeAt`、`addRange`、`removeRange`、
  `removeAllRanges`、`empty`、`collapse`、`collapseToStart/End`、`extend`、`setBaseAndExtent`、`selectAllChildren`、
  `deleteFromDocument`、`containsNode`、`modify(alter, direction, granularity)`（character/word/line/lineboundary/paragraph；
  line 粒度需要布局——用 P31 回报的行盒，未回报时按段落近似）、`toString`、`getComposedRanges`。
- `selectionchange` 事件（`document` 上，异步合并）；`selectstart`。
- 输入框内的选区仍在 `Draft` 里（与浏览器一致：`<input>` 的选区不出现在 `getSelection()` 中，`selectionStart/End` 读 Draft）。
- **快照**：见上，选区在 `Dom` 里；`::selection` 样式由 P22 提供。

### 2. contenteditable 编辑（`editing.rs`，需新增）

- 可编辑宿主：`contenteditable=""|"true"|"plaintext-only"`、`document.designMode = "on"`；`isContentEditable`、`contentEditable` 反射已存在
  （`bind/element.rs` 约 1503/1521 行），保持。焦点进入可编辑宿主时 `document.activeElement` 为宿主。
- 键盘路径（`Document::key_down` 扩展）：焦点在可编辑宿主内时，按键依次产生 `keydown` →（未阻止）`beforeinput`（可取消，
  `inputType`、`data`、`dataTransfer`、`getTargetRanges()` 返回 `StaticRange`）→ 默认编辑动作 → `input`。默认动作至少：

| 键 | inputType | 默认动作 |
|---|---|---|
| 可打印字符 | `insertText` | 在选区处插入文本（替换非空选区） |
| Enter | `insertParagraph` | 拆分当前块（`<div>`/`<p>`，按 `defaultParagraphSeparator`，默认 `div`） |
| Shift+Enter | `insertLineBreak` | 插入 `<br>` |
| Backspace / Delete | `deleteContentBackward/Forward` | 删除一个字素簇（`unicode-segmentation`）或选区；跨块合并 |
| Ctrl+Backspace / Ctrl+Delete（mac：Alt） | `deleteWordBackward/Forward` | |
| Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y | `historyUndo/historyRedo` | 撤销栈（见下） |
| 方向键、Home/End、PageUp/Down（含 Shift 扩选、Ctrl 按词） | — | 移动选区（`Selection.modify`） |
| Ctrl+A | — | 选中可编辑宿主全部内容 |
| Ctrl+B/I/U | `formatBold/Italic/Underline` | `execCommand` 同名命令 |
| Tab | — | 默认移动焦点（与浏览器一致，不插入制表符） |

- `plaintext-only`：只插入纯文本、Enter 插入 `\n` 或 `<br>`（以 Chrome 实测为准）。
- 撤销栈：每个可编辑宿主一份（Chromium 是每文档一份，差异记在状态里；上限 1000 步；分组按"开放键入"规则）（和每个文本框，`Draft` 已有的也要接上 `historyUndo`）记录 DOM 变更序列（用 MutationRecord 同款数据），
  合并连续键入；脚本对 DOM 的直接修改不入栈（与浏览器一致——这正是编辑器自己管理撤销的原因）。
- **Electron 39.8.10 实测（冻结在 tests/editing.rs、tests/ime.rs 头注）**：脚本调用 `execCommand` 只发 `input`（不发 `beforeinput`，
  `inputType` 见表：`bold→formatBold`、`strikeThrough→formatStrikeThrough`（产生 `<strike>`）、`createLink→insertLink`（`data` 为地址）、
  `removeFormat→formatRemove`，`unlink/insertHTML/formatBlock` 的 `inputType` 为空串），`selectAll` 只发 `selectstart`/`selectionchange`；
  键盘路径是 `keydown → keypress → beforeinput → textInput → input → keyup`，`selectionchange` 在之后异步到达（晚于微任务、早于 `setTimeout(0)`）；
  行首/行尾空格写成 `&nbsp;`；`plaintext-only` 的 `getTargetRanges()` 恒为 `[]`，Enter 插入 `\n` 文本，`inputType` 为 `insertLineBreak`。
- `execCommand`（`bind/document.rs` 扩展）：`insertText`、`insertHTML`、`delete`、`forwardDelete`、`insertParagraph`、`insertLineBreak`、
  `bold/italic/underline/strikeThrough`（包 `<b>/<i>/<u>/<s>`，`styleWithCSS` 为 false）、`createLink/unlink`、`formatBlock`、
  `insertOrderedList/insertUnorderedList`、`selectAll`、`undo/redo`、`copy/cut/paste`（`paste` 只在受信任的用户手势中、
  且主进程允许时生效——Electron 中 `paste` 命令可用；读取剪贴板见 §5）、`removeFormat`；`queryCommandState/Enabled/Value/Supported` 如实回答。
  每个命令都产生对应 `inputType` 的 `beforeinput/input`。

### 3. 输入法组合（需新增 `Document` 方法）

```rust
impl Document {
    /// An input method is composing `text` at the caret (replacing what it composed before);
    /// `selection` is where its caret sits inside `text`, in UTF-16.
    pub fn compose(&self, runtime: &mut verve::Runtime, text: &str, selection: Option<std::ops::Range<usize>>);
    /// The composition, committed as `text` (may differ from the last composed text).
    pub fn commit_composition(&self, runtime: &mut verve::Runtime, text: &str);
    /// The composition, abandoned (Escape, focus lost): what was composed is removed.
    pub fn cancel_composition(&self, runtime: &mut verve::Runtime);
    /// Where the caret is drawn, for the input method's candidate window (viewport coords).
    pub fn caret_rect(&self) -> Option<Rect>;
}
```

- 事件序列（以 Chrome on Windows 实测冻结）：第一次 `compose` → `compositionstart` → `beforeinput(insertCompositionText)` →
  `compositionupdate` → 文本变化 → `input(insertCompositionText, isComposing: true)`；后续 `compose` 无 `compositionstart`；
  `commit_composition` → `beforeinput` → `compositionupdate`? → `input(isComposing: true)` → `compositionend` →（Chrome 不再发额外 input）。
  组合期间的 `keydown`/`keyup` 有 `isComposing: true`（实测）；`keydown` 的 `key: "Process"`、`keyCode: 229` 取自 UI Events 与 Chromium 的
  `VKEY_PROCESSKEY`，探针里是探针自己发的这两个值、Electron 原样回显，**不是实测**。
- 文本框：用 `Draft::replace_and_mark_utf16` / `replace_utf16` / `unmark`（已有）。contenteditable：组合文本作为选区处的临时文本节点
  （Chromium 直接改 DOM，脚本可见），提交后保留，取消时删除。
- Electron 39.8.10 实测（`webContents.debugger` 的 `Input.imeSetComposition`/`Input.insertText`；输入框、文本域、contenteditable 相同）：
  首次更新 `compositionstart(data "")` → `compositionupdate` → `beforeinput(insertCompositionText, isComposing: true, 不可取消)` → `input`；
  提交 `compositionupdate(提交文本)` → `beforeinput` → `textInput` → `input(isComposing: true)` → `compositionend`，没有 `insertText` 的 `input`；
  取消（空串）同样先 `compositionupdate("")`、`beforeinput(data "")`、`textInput("")`、`input(data null)`，再 `compositionend("")`；
  组合期间 `keydown`/`keyup` 的 `isComposing` 为 true（`key: "Process"`、`keyCode: 229` 是探针自己发的值，Electron 回显，未独立观察）；
  contenteditable 的 `getTargetRanges()` 是上一次组合文本的范围。
- P02 的 `ToRendererInput::Composition(CompositionEvent)` 定义为：

```rust
pub enum CompositionEvent {
    Update { text: String, selection: Option<std::ops::Range<usize>> },
    Commit(String),
    Cancel,
}
```

  `RendererGuest` 映射到上面三个方法；每次处理后把 `caret_rect()` 写入 `RendererView.ime_rect`（P32 用它设置 GPUI 的
  `InputHandler::bounds_for_range`）。

### 4. 拖放（`drag.rs`，需新增）

- **页面内拖放**（HTML5 DnD）：`draggable="true"` 元素（以及选中文本、链接、图片的默认可拖）；指针按下后移动超过 4px 开始拖动：
  `dragstart`（可取消，`dataTransfer.setData/setDragImage/effectAllowed`）→ 移动中对指针下目标 `dragenter/dragover/dragleave`
  （`dragover` 必须 `preventDefault` 才可放）→ 松开 `drop`（`dataTransfer.getData/files/items/types`、`dropEffect`）→ 源上 `dragend`。
  拖动中不发 `mousemove/pointermove` 给页面（与浏览器一致，发 `pointercancel`）。
  入口：复用 `Document::pointer_down/move/up`，内部状态机判定拖动。拖动图像：v1 不画（`setDragImage` 记录），登记。
- **从系统拖入**（需新增）：

```rust
pub struct ExternalDrag { pub files: Vec<std::path::PathBuf>, pub text: Option<String>, pub uri_list: Vec<String>, pub html: Option<String> }
impl Document {
    pub fn drag_enter(&self, runtime: &mut verve::Runtime, data: ExternalDrag, pointer: &Pointer) -> DropEffect;
    pub fn drag_over(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> DropEffect;
    pub fn drag_leave(&self, runtime: &mut verve::Runtime);
    pub fn drop(&self, runtime: &mut verve::Runtime, pointer: &Pointer) -> DropEffect;
}
```

  `dataTransfer.files` 中的 `File` 由 P03 的"带路径 File"构造（`webUtils.getPathForFile(file)` 返回该路径，`file.path` **不存在**——
  Electron 32 起已移除）。文件内容惰性读取，读取经过渲染器的文件系统权限（Electron 渲染器中拖入的文件可读）。
  若页面不处理 `drop`（未 `preventDefault` 的 `dragover`/`drop`），浏览器会导航到该文件——Electron 中同样会发 `will-navigate`；
  按此实现（发 `Request::Navigate` 到 `file://…`）。
- P02 需追加 `ToRendererInput::Drag(DragInput)`（`Enter{data,pointer}/Over{pointer}/Leave/Drop{pointer}`），P34 从 GPUI 的
  文件拖放事件（`FileDropEvent`）产生它。
- 拖出到系统（`webContents.startDrag(item)`，主进程 API）：v1 不支持，登记（P13 中抛 `ERR_NOT_SUPPORTED`）。

### 5. 剪贴板事件与 `navigator.clipboard`

- 快捷键 Ctrl/Cmd+C/X/V（以及菜单 role `copy/cut/paste`，P15 发 `ToRenderer` 需新增 `EditCommand(Copy|Cut|Paste|SelectAll|Undo|Redo|Delete)`）：
  - copy/cut：对焦点目标（或 `document.activeElement`/`body`）发 `copy`/`cut`（`ClipboardEvent`，`clipboardData` 为可写的 `DataTransfer`）；
    未阻止时默认取当前选区（文本框取 Draft 选区）的 `text/plain` 与 `text/html`；cut 再删除选区（产生 `deleteByCut` 的 beforeinput/input）。
    结果写剪贴板：`Request::CopyText` 扩展为 `Request::WriteClipboard { text: Option<String>, html: Option<String>, custom: Vec<(String, Vec<u8>)> }`
    （修改 `crates/verve-dom/src/lib.rs` 的 `Request`，同步现有测试）。
  - paste：渲染器需要剪贴板内容——**剪贴板由 Shell/主进程持有**。流程：Shell 在发出 paste 键之前读取剪贴板，随
    `ToRendererInput::Paste(ClipboardData { text, html, files, image_png })`（需新增）一起送达；`Document::paste(runtime, data)`（需新增）
    发 `paste` 事件，未阻止时按 `insertFromPaste` 插入（contenteditable 插 HTML 时做基本清理：去掉 `<script>`、事件属性；文本框插纯文本）。
- `navigator.clipboard.writeText/write` → `Request::WriteClipboard`，promise 立即 resolve；`readText/read`：发
  `Request::ReadClipboard { call }`（需新增），Shell 回 `ToRendererInput::ClipboardContents { call, data }`（需新增）后 resolve。
  Electron 中这些在有焦点的页面上默认允许（权限由 P16 的 `setPermissionRequestHandler` 决定，未设置时允许）。
- 图片：`ClipboardItem` 支持 `image/png`（字节来自 Shell；P25 不需要介入）。

## 步骤

1. `selection.rs`：Rust 侧实时 Range/Selection，替换 JS 属性实现；接入 `Dom` 变更；`selectionchange`；快照中的选区与光标。
2. `editing.rs`：contenteditable 的键入/删除/回车/移动、`beforeinput` 可取消、撤销栈；`execCommand` 全表。
3. 输入法：`Document::compose/commit_composition/cancel_composition/caret_rect`（文本框复用 `Draft`，contenteditable 新实现）；渲染器对接 P02 消息。
4. 剪贴板：`Request::WriteClipboard/ReadClipboard`、`Document::paste`、`EditCommand` 处理。
5. 拖放：页面内状态机、`ExternalDrag` 入口、带路径 File。
6. 用真实编辑器验收（下）。

## 验收

- `cargo test -p verve-dom`：现有全部通过；新增：
  - `tests/selection.rs`：插入/删除节点后 Range 边界按规范移动；`extractContents/surroundContents`；`Selection.modify('extend','forward','word')`；
  - `tests/editing.rs`：contenteditable 中 `type_text("你好")`、Enter 产生新 `<div>`、Backspace 跨块合并、`beforeinput` 被 `preventDefault` 时 DOM 不变、
    撤销恢复、`execCommand('bold')` 产生 `<b>` 与对应 inputType；
  - `tests/ime.rs`：文本框与 contenteditable 的组合事件序列与 Chrome on Windows 冻结值一致（`isComposing`；`Process`/229 取自规范，见上）、取消时内容恢复、`caret_rect` 有值；
  - `tests/drag.rs`：页面内拖放事件序列；`ExternalDrag` 两个文件 → `drop` 中 `files.length === 2`、`webUtils.getPathForFile` 返回路径（此项在 verve-electron 测）；
  - `tests/clipboard.rs`：选中文本后 `EditCommand::Copy` 产生 `WriteClipboard{ text, html }`；`copy` 监听器 `setData` 覆盖默认；`paste` 插入文本框与 contenteditable。
- **真实编辑器**（`#[ignore]`，`verve::npm` 取包，参考 `tests/dsh.rs`）：
  - ProseMirror（prosemirror-view + example-setup）：键入一段文字、回车、Backspace 后 `view.state.doc.textContent` 正确；
  - Lexical 或 Tiptap：同上，含一次输入法组合；
  - CodeMirror 6：键入与撤销后 `view.state.doc.toString()` 正确（CodeMirror 的测量依赖布局回报——无头测试用固定行高的假布局，
    写一个测试用的 `set_layout`/`set_fragments` 填充器）。

## 不做

拼写检查与自动更正；双向文本（RTL）的光标移动；表格编辑命令（`insertRow` 等）；拖出到系统（`startDrag`）；
拖动预览图像的绘制；`contenteditable` 中图片缩放手柄；`document.execCommand('paste')` 之外的剪贴板权限细节。

## 给其他包的备注

- **P02**：`CompositionEvent` 按 §3 定义；追加 `ToRendererInput::{Drag(DragInput), Paste(ClipboardData), ClipboardContents{call,data}}`、
  `ToRenderer::EditCommand(EditCommand)`；`verve_dom::Request` 的 `CopyText` 改为 `WriteClipboard{…}`，新增 `ReadClipboard{call}`。
- **P03**：带路径的 `File` 构造（与 P21 的需求相同）。
- **P31**：画 `SnapshotData.selection`/`caret`；回报文本片段与行盒（`Document::set_fragments`，P23 定义）供 `Range.getClientRects`、
  `Selection.modify(line)`、点击定位光标（`caretPositionFromPoint`/`caretRangeFromPoint` 需本包实现，依赖片段回报）。
- **P32**：GPUI `InputHandler` ↔ `CompositionEvent`，`ime_rect` ↔ `bounds_for_range`；Ctrl+V 之前读取剪贴板并随 `Paste` 送达。
- **P34**：GPUI 文件拖放事件 → `ToRendererInput::Drag`。
- **P15**：菜单 role → `ToRenderer::EditCommand`。

## 状态

（实现者完成后填写）
