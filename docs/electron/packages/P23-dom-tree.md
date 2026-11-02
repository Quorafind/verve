# P23 verve-dom DOM 补齐

**依赖**：无（可与 P02 并行；与 P22 同时改 `css.rs` 时注意协调，见下）  **规模**：L  **修改**：`crates/verve-dom/src/{dom.rs,lib.rs,state.rs,css.rs(仅作用域部分),bind/node.rs,bind/element.rs,bind/document.rs,bind/events.rs,bind/platform.rs,html.rs}`

## 目标

补上真实前端框架和 Web Components 依赖、而 verve-dom 现在缺失或近似实现的 DOM 语义：

1. **Shadow DOM 真正参与渲染**（现在 shadow 树不在"已连接"状态，既不层叠也不进绘制——Lit、Shoelace、
   Ionic、Fluent UI Web Components 等会**什么都画不出来**）、样式作用域、`<slot>` 分配、事件重定向；
2. 解析器创建的自定义元素升级、`attributeChangedCallback`、`customElements.upgrade`；
3. 实时集合（`childNodes`、`children`、`getElementsBy*`、`form.elements`、`select.options`…）；
4. `<template>` 内容、`document.fonts`（真正的 `FontFaceSet`）、依赖布局的 `innerText`、`getClientRects`、
   `scrollIntoView` 选项等。

## 背景（先读）

- `crates/verve-dom/README.md`「Not yet」：集合不是实时的；解析器创建的自定义元素不升级、
  `attributeChangedCallback` 不调用；shadow root 样式不作用域、事件不跨 shadow 重定向。
- `crates/verve-dom/src/dom.rs`：`Dom`、`Node`、`Slot.connected`、`set_connected`、`is_connected`
  （"是否在窗口文档里"）、`root_of`、`descendants`/`subtree`、`Mutation`。**`Dom` 不知道 shadow root**。
- `crates/verve-dom/src/state.rs`：`shadow_roots: HashMap<NodeId, (NodeId, bool)>`（宿主 → (root, open)）、
  `shadow_hosts`（root → 宿主）、`custom_elements`、`custom_nodes`、`custom_waiting`、`template_contents`。
- `crates/verve-dom/src/bind/platform.rs`：`attach_shadow`（约 224 行，root 是一个独立 fragment）、自定义元素
  （`custom_class`、`construct_html_element`、约 419 行的 connected/disconnected 通知、`whenDefined`、
  `upgrade` 为空操作）、`document.fonts`（约 960 行，全是桩）。
- `crates/verve-dom/src/bind/events.rs`：`parent()`——事件路径已能从 shadow root 走到宿主，但没有重定向，
  `composedPath()`、`composed` 标志未按规范处理。
- `crates/verve-dom/src/bind/node.rs`：`childNodes`（约 439 行）每次读取返回 `node_list(children)` 快照数组；
  `children`（约 884 行）。`bind/document.rs` 的 `tagged_collection`（约 620 行）、`collection()`。
- `crates/verve-dom/src/bind/element.rs`：`slot` 的 `assignedNodes` 返回空数组（约 3795 行）；`getClientRects`
  （约 1100 行，只返回一个盒子）；`scrollIntoView`（约 1110 行，`Request::Scroll(Reveal)`，忽略选项）；
  `innerText`（约 1455 行，`inner_text(dom, id)`，按 DOM 文本拼接，不看样式）。
- `JsProxyBuilder` 在 `bind/element.rs`（`dataset`、`style` 约 656/1236/1334 行）和 `bind/window.rs` 中已有用法——实时集合可照此做。
- `crates/verve-dom/tests/{basics.rs,arena.rs,observers.rs}`。

## 接口契约

### 1. Shadow DOM 进入 `Dom`（需新增/修改）

- `Node` 增加 `shadow_root: Option<NodeId>`；shadow root 节点增加 `host: Option<NodeId>`、`mode`、`delegates_focus`、
  `slot_assignment`（`named`/`manual`）。`State.shadow_roots/shadow_hosts` 改为从 `Dom` 读取（保留为兼容访问器或删除）。
- **连接性**：宿主已连接时其 shadow 树也"已连接"（`set_connected` 递归进入 `shadow_root`）。`isConnected` 对 shadow 树内
  节点返回 true；`getRootNode()` 返回 shadow root，`getRootNode({ composed: true })` 返回 document。
- `Dom::descendants`/`subtree` **不**进入 shadow 树（保持 DOM 树语义，`querySelectorAll` 不穿透）；新增
  `Dom::shadow_including_descendants`。
- **扁平树**（绘制和层叠继承用，需新增）：

```rust
impl Dom {
    /// The children a box is drawn with: a host's shadow root's children (slots in place), a
    /// slot's assigned nodes (or its own children as fallback), anything else's own children.
    pub fn flat_children(&self, id: NodeId) -> Vec<NodeId>;
    /// The parent in the flat tree: what an assigned node is drawn inside, the host for a
    /// shadow root's top level.
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId>;
}
```

  `Snapshot` 暴露同名方法；**P30 按扁平树绘制**。被分配到 slot 的节点在扁平树里位于 slot 下；没有被分配的宿主子节点不绘制。
- `<slot>` 分配：命名分配（`slot` 属性 ↔ `name`）、默认 slot、`assign()`（manual 模式）；`slot.assignedNodes({flatten})`、
  `assignedElements()`、`element.assignedSlot`；分配变化时发 `slotchange`（微任务里，合并）。
- 层叠作用域（`css.rs`，与 P22 协调：P22 改值与层叠顺序，本包只加"作用域"维度）：
  - shadow root 里的 `<style>` 和 `adoptedStyleSheets` 只作用于该 shadow 树；
  - `:host`、`:host(sel)`、`:host-context(sel)` 在 shadow 样式里匹配宿主；`::slotted(sel)` 匹配分配进来的节点；
  - `::part(name)` 与 `part`/`exportparts` 属性（外部样式选中 shadow 内部带 part 的元素）；
  - 继承沿**扁平树**（分配到 slot 的节点从 slot 继承）；
  - 层叠顺序：外部（宿主所在树）规则与 shadow 内 `:host` 规则冲突时，外部正常声明胜、`!important` 时 shadow 内胜（CSS Scoping 规则）；
  - `State.read_style_elements` 需按树分组样式表（`SheetOwner` 增加所属 root），`Styler` 按节点所在树选择适用的规则集。
- 事件（`bind/events.rs`）：
  - `composed: false` 的事件不越过 shadow root；UA 产生的 UI 事件（click、input、key*、pointer*、focus 等）`composed: true`；
  - **重定向**：监听器在 shadow 树外时，`event.target` 是宿主；`relatedTarget` 同理；
  - `composedPath()` 按规范返回（closed 模式的 shadow 内节点对外不可见）；
  - `focus`：`delegatesFocus`、`document.activeElement` 返回宿主、`shadowRoot.activeElement`；`:focus` 匹配宿主。
- `element.shadowRoot`（closed 时 null）、`attachShadow` 的合法宿主检查（自定义元素 + 规范列出的标签）、声明式 shadow DOM
  （`<template shadowrootmode="open">`，由 `html.rs` 解析时直接附加）。

### 2. 自定义元素

- 解析器（`html.rs`，包括 `innerHTML`、整页加载）创建已定义标签的元素时：**同步构造**（整页解析时）或排队升级（片段解析时），
  与规范的"元素创建时查找定义"一致；`customElements.define` 之后把文档中已存在的同名元素**升级**（按文档顺序）。
- 回调：`constructor`、`connectedCallback`、`disconnectedCallback`、`adoptedCallback`、`attributeChangedCallback(name, old, new)`
  （仅 `static observedAttributes` 列出的属性；`set_attr`/`remove_attr`/解析时的初始属性都触发）、`formAssociated` 元素的
  `formAssociatedCallback/formResetCallback/formDisabledCallback`（v1 可只做 `ElementInternals` 的 `setFormValue`、`form`、`labels`、
  `states`(CustomStateSet + `:state()` 伪类)）。
- 回调时机：custom element reactions 队列，在原生 DOM 方法返回前（CEReactions）执行——实现为"每个绑定的 DOM 变更方法末尾冲刷队列"。
- `customElements.upgrade(root)`、`getName(ctor)`、`whenDefined` resolve 时机、`is="…"` 自定义内置元素（v1 可不支持，登记）。
- 构造中 `this.attributes`/children 访问的规范约束（构造期间不能有子节点和属性）——违反时报 `NotSupportedError`。

### 3. 实时集合

- `NodeList`（`childNodes`）、`HTMLCollection`（`children`、`getElementsByTagName/ClassName/Name`、`document.forms/images/links/scripts`、
  `form.elements`、`table.rows`、`select.options`、`tBodies`、`cells`）改为**实时**：一个 Proxy（`JsProxyBuilder`），持有
  `(root, filter)`，每次 `length`/索引/迭代时从 `Dom` 读取当前结果。
- 性能：按 `Dom` 的修订号缓存结果（`Dom` 需要一个整体变更计数，已有 `styles_revision`，新增 `tree_revision`），同一修订内重复访问 O(1)。
- 同一个 `parent.childNodes` 多次读取返回**同一个对象**（缓存在节点包装上的隐藏属性）。
- `querySelectorAll` 仍返回静态 `NodeList`（规范如此）。
- `namedItem`、`item`、`HTMLCollection` 的命名属性访问（`form.elements.username`）、`RadioNodeList`。

### 4. 其他

- `<template>`：`template.content` 是**独立文档**（"inert" 文档）中的 fragment，内部脚本/图片不加载、自定义元素不升级；
  `importNode`/`cloneNode` 出来才升级。现有 `template_contents` 保留但改为挂在 inert 文档下。
- `document.fonts`（`FontFaceSet`，替换 `bind/platform.rs` 的桩）：`new FontFace(family, source, descriptors)`、`load()`、
  `status`、`loaded`；`document.fonts.add/delete/has/forEach/size/ready/check(font)/load(font)`；`loading`/`loadingdone`/`loadingerror` 事件。
  `@font-face` 来的条目由 P20 加入并驱动状态；`check()` 依据已加载的字体族。字体字节通过 P20 的 `SnapshotData.fonts` 交给绘制侧。
- `innerText`（getter）：按 HTML 规范的"rendered text collection"——跳过 `display:none`、块级元素前后换行、`<br>`、
  `white-space` 折叠、`text-transform`、`<p>` 前后两个换行、表格单元格制表符；需要 `Computed`（`State` 中已有层叠结果，
  `getComputedStyle` 同源）。未连接或 `display:none` 的元素返回 `textContent`。
- `getClientRects()`：v1 对块级元素返回边框盒一个矩形；对行内元素返回每一行一个矩形——需要 P31 在 `set_layout` 之外回报
  行内片段（需新增 `Document::set_fragments(runtime, [(node, Vec<Rect>)])`），未回报时退回一个矩形。
- `scrollIntoView({ block, inline, behavior })` / `scrollIntoView(bool)`：`ScrollRequest::Reveal` 增加 `block/inline/smooth` 字段
  （修改 `crates/verve-dom/src/lib.rs` 的 `ScrollRequest`）；`scrollIntoViewIfNeeded`（Chromium 私有，Electron 应用常用）。
- `element.checkVisibility()`、`getBoundingClientRect` 在 transform 下（v1 不考虑 transform，登记）、`elementsFromPoint` 按扁平树。
- `Node.compareDocumentPosition`、`contains`、`isEqualNode` 在 shadow 树下的行为核对。
- `document.activeElement`、`document.hasFocus()`（由窗口焦点 `ToRendererInput::Focus` 驱动，P32 调用 `Document::set_window_focus` 需新增）。
- `document.visibilityState`/`hidden`/`visibilitychange`（窗口最小化/隐藏时，`Document::set_visibility` 需新增，P13/P34 调用）。

## 步骤

1. Shadow root 进入 `Dom` + 连接性 + `flat_children/flat_parent`（先让 Lit 的组件能被层叠和绘制）。
2. slot 分配与 `slotchange`。
3. 层叠作用域（`:host`、`::slotted`、`::part`、shadow 内样式表、扁平树继承）。
4. 事件重定向、`composedPath`、焦点委托。
5. 自定义元素：解析器创建、升级、`attributeChangedCallback`、CEReactions、`ElementInternals` 子集。
6. 实时集合。
7. `<template>` inert 文档、声明式 shadow DOM、`FontFaceSet`、`innerText`、`getClientRects`、`scrollIntoView` 选项、可见性与窗口焦点。

## 验收

- `cargo test -p verve-dom`：现有全部通过；新增：
  - `tests/shadow.rs`：shadow 内 `<style>` 不影响外部、`:host`/`::slotted`/`::part` 生效、扁平树结构、`slotchange`、
    事件重定向（外部监听器 `target` 为宿主）、closed 模式 `composedPath` 隐藏内部、`getRootNode`；
  - `tests/custom_elements.rs`：`innerHTML` 创建的已定义元素被升级、`define` 后升级既有元素且顺序正确、
    `attributeChangedCallback` 只对 `observedAttributes` 触发（含解析时初始属性）、回调在 DOM 方法返回前执行；
  - `tests/collections.rs`：`const c = el.children; el.append(x); c.length` 增加；`childNodes` 同一对象；`form.elements.name`；
  - `innerText` 对照 Chrome 冻结的 10 个样例；
  - **真实库**（`#[ignore]`，用 `verve::npm` 取包）：Lit 3 的一个带 slot 和样式的组件渲染出正确的扁平树与样式；
    Shoelace 或 Fluent UI Web Components 的一个按钮可点击（参考现有 `tests/dsh.rs` 的写法）。
- `tests/arena.rs` 的回收测试扩展到 shadow 树：断开的宿主连同 shadow 树被 `sweep` 回收。

## 不做

`is="…"` 自定义内置元素（登记）；完整 `ElementInternals` ARIA 反射；`<iframe>`（见 P20）；Trusted Types；
`Document.adoptNode` 跨文档的完整语义（保持现状）；带 transform 的几何。

## 给其他包的备注

- **P30**：按 `Snapshot::flat_children` 遍历绘制，而不是 `dom.children`。
- **P22**：`css.rs` 作用域维度由本包加；双方改同一文件时，P22 先完成"拆分 css.rs"的重构 commit，本包在其后开工这一部分。
- **P31**：回报行内片段 `Document::set_fragments`。
- **P20**：`@font-face` 条目进入 `FontFaceSet` 的接口；整页解析时的自定义元素同步构造。
- **P13/P32/P34**：`Document::set_visibility`、`Document::set_window_focus`。

## 状态

（实现者完成后填写）
