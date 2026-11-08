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
- 性能：按 `Dom` 的修订号缓存结果（`Dom` 需要一个整体变更计数，已有 `styles_revision`，复用已有的 `revision`——它随每次变更移动），同一修订内重复访问 O(1)。
- 同一个 `parent.childNodes` 多次读取返回**同一个对象**（缓存在节点包装上的隐藏属性）。
- `querySelectorAll` 仍返回静态 `NodeList`（规范如此）。
- `namedItem`、`item`、`HTMLCollection` 的命名属性访问（`form.elements.username`）、`RadioNodeList`。

### 4. 其他

- `<template>`：`template.content` 是**独立文档**（"inert" 文档）中的 fragment，内部脚本/图片不加载、自定义元素不升级；
  `importNode`/`cloneNode` 出来才升级。现有 `template_contents` 保留但改为挂在 inert 文档下。
- `document.fonts`（`FontFaceSet`，替换 `bind/platform.rs` 的桩）：`new FontFace(family, source, descriptors)`、`load()`、
  `status`、`loaded`；`document.fonts.add/delete/has/forEach/size/ready/check(font)/load(font)`；`loading`/`loadingdone`/`loadingerror` 事件。
  **契约变更（审查后）**：字体字节的获取归宿主。`face.load()`/`document.fonts.load()` 对带地址的 face 发
  `Request::LoadFont { face: FontFaceId, url }`，宿主取完后调 `Document::settle_font(runtime, FontFaceId, FontOutcome)`
  （`Loaded`/`Failed`），取代先前的 `settle_font(runtime, &JsValue, bool)`。`FontFace` 的状态存在 Rust 里（`Face`），脚本对象只读写它。
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

分支 `electron/p23-dom-tree`。`cargo test -p verve-dom` 全部通过（含新增的 `custom_elements`、`collections`、`fonts`、
`inner_text`、`window_state`，以及 `shadow` 的扩充）；`cargo clippy -p verve-dom --tests` 在本包新增的文件上无警告
（其余警告是既有代码的）。未跑 `--workspace`、release、体积测量。

### 做了什么

- **Shadow DOM**（前两个 commit，DeepSeek 起头，我核对后沿用）：shadow root 进 `Dom`（连接性、`flat_children`/`flat_parent`、
  `Snapshot` 同名方法）、`<slot>` 分配（命名/manual、`assignedNodes/Elements`、`assignedSlot`、合并的 `slotchange`）、
  样式作用域（shadow 内 `<style>`/`adoptedStyleSheets`、`:host`/`:host()`/`:host-context()`、`::slotted()`、`::part()`、扁平树继承、
  外层 normal 胜/内层 `!important` 胜）。
- **事件与焦点**：监听器在 shadow 树外时 `target`/`relatedTarget` 重定向为宿主；closed 树的节点从 `composedPath()` 隐藏；
  被分配的节点的事件经过它的 slot；`composed:false` 的事件（页面自己的 `load`/`change`/`scroll`…）不越过 shadow root；
  `document.activeElement` 返回宿主、`shadowRoot.activeElement`、`:focus` 匹配含焦点的宿主、`delegatesFocus`。
- **自定义元素**（新模块 `bind/custom.rs`，自 `platform.rs` 搬出）：解析器/`innerHTML` 创建的元素在进入文档时升级，`define` 按文档顺序升级
  既有元素；`attributeChangedCallback`（只对 `observedAttributes`，初始属性在升级时按序送达）；`customElements.upgrade(root)`；
  构造器加了属性/子节点时 `createElement` 抛 `NotSupportedError`；回调在触发它的 DOM 方法返回前执行（统一在 `with_dom` 末尾冲刷）。
- **实时集合**（新模块 `bind/live.rs`）：`childNodes`、`children`、`getElementsBy*`、`form.elements`、`select.options/selectedOptions`、
  `table.rows/tBodies`、`section.rows`、`row.cells`、`document.forms/images/links/scripts` 都是 Proxy，按 `Dom::revision` 缓存；
  `[SameObject]` 的访问器同一个对象；`HTMLCollection` 支持 `form.elements.name` 命名属性。
- **其他**：声明式 shadow DOM（整页/`DOMParser` 解析时 `<template shadowrootmode>`）；`document.fonts` 真正的 `FontFaceSet` + `FontFace`
  （新模块 `bind/fonts.rs`）；`innerText` 按渲染文本收集（crate 级模块 `inner_text.rs`，纯 `&Dom` + 样式查找）；`getClientRects` 用 `Document::set_fragments`
  的逐行矩形（未报告则退回一个）；`scrollIntoView({block,inline,behavior})`、`scrollIntoViewIfNeeded`（`ScrollRequest::Reveal` 带
  `block/inline/smooth`，新增 `verve_dom::Align`）；`Document::set_visibility`、`Document::set_window_focus`（`document.hidden`、
  `visibilityState`、`hasFocus()`、`visibilitychange`、window `focus`/`blur`）。

### 审查后的整改（第二轮）

- 长文件不再增长：`dom/shadow.rs`（shadow 表、slot 分配、扁平树、`host_chain`）、`dom/reclaim.rs`、`css/scope.rs`（作用域、`ScopeIndex`、
  `Layer`、`matched_rules`）、`css/styler.rs`、`bind/slot.rs`、`window_state.rs`（`Align`/`ScrollRequest`/`FontFaceId`/窗口状态/`Snapshot` 扁平树）、
  `inner_text.rs`（crate 级，纯 `&Dom` + 样式查找）。`dom.rs`/`css.rs`/`lib.rs`/`element.rs`/`dom_events.rs` 都低于棘轮上限。
- `src/dom_events.rs` 的越界改动单独成 commit（`feat(events): …`，消息里写了理由）：重定向与 `composedPath` 过滤只能在构造路径、设置 `target` 的
  `dispatch_value` 里决定。父节点钩子连同新的 `EventScope` 移入 `src/dom_events/hooks.rs`，`dispatch_value` 只多两行。
- 契约变更（docs commit `docs(electron): Record P23's font loading contract`）：`Request::LoadFont { face: FontFaceId, url }` +
  `Document::settle_font(runtime, FontFaceId, FontOutcome)`；`FontFace` 状态在 Rust（`Face`），`Ready` 枚举，只读访问器。
- 机器检查：`cargo fmt --all --check`、`bash scripts/style-check.sh`、`cargo clippy -p verve-dom --all-targets -- -D warnings`
  通过；`cargo test -p verve-dom` 与 `cargo test --test web_events` 通过（`tests/react.rs` 在机器繁忙时偶发过一次
  `nothing matches …`，单独重跑 5 次全过，疑似既有的时序脆弱，未在本包范围内追）。
- 仍在用 `bind/mod.rs` 里与 `verve::native` 同名不同签名的辅助（`text`、`list`、`array`…）：R1 债，未新增副本；P03 公开 `verve::native` 后迁移。

### 没做 / 已知问题

- 与简报/审查相比**推迟**的：`own_style` 仍超过 80 行（只拆出了 `matched_rules`），`Parser::compound` 保持原样；`inner_text` 用实时
  `style_of` 而不是缓存的 `Computed`（缓存在回合中可能过时，`getComputedStyle` 同理）；`ScopeIndex` 没有再持有 `trees`/`own`——
  它们是每条规则的一个字段（`RuleEntry`），避免并行向量。
- 审查提到的行为差异：`::part()` 规则按普通 author 层排序（没有 `part` 专属层）；`innerText` 对每个节点调一次 `style_of`，
  O(n·深度)，大树上会慢，待有缓存的 `Computed` 可用时改。（`Dom::is_shown` 对未被分配的宿主子节点已改为不显示。）
- `exportparts`（及 `part` 的转发）没做，`::part()` 只匹配直接的 shadow 树里的 `part`。
- `<template>.content` 仍在同一文档里，不是独立的 inert 文档（脚本/自定义元素是否升级取决于它不在文档树里，现状够用，但
  `importNode` 才升级这一点没有单独建模）。
- `ElementInternals` 没做（`attachInternals()` 仍返回空对象）：`setFormValue`、`form`、`labels`、`states` 与 `:state()`、
  `formAssociatedCallback` 都没有。`adoptedCallback` 没有（`adoptNode` 保持现状）。`is="…"` 按计划不做。
- 整页解析时自定义元素是**解析完后按文档顺序升级**，不是解析中同步构造（构造器里看得到完整的子树）；P20 接整页加载时再决定要不要
  改成同步构造。`classList`/`style` 的变更不触发 `attributeChangedCallback`（只覆盖 `setAttribute/removeAttribute/解析`）。
- `document.fonts`：`new FontFace(family, bytes)` 同步变 `loaded`（规范是先 `loading` 再异步）；带地址的 face 停在 `loading`，由宿主
  调 `Document::settle_font` 结束；`FontFaceSetLoadEvent.fontfaces` 没有（事件是普通 `Event`）；`@font-face` 规则怎样进集合归 P20。
- 声明式 shadow DOM 只在整页解析（`parse_document`，`DOMParser` 也走它）里生效，`innerHTML` 里不生效（符合规范），但
  `setHTMLUnsafe`/`parseHTMLUnsafe` 没有。`DOMParser` 在真实 Chrome 里默认也不附加，这里附加了——偏差。
- `innerText` 的 10 个样例的期望值来自规范的算法，**没有**在真实 Chrome 上跑过冻结；`<select>`/`<input>`
  等替换元素、`::first-letter`、`text-transform` 的语言规则没处理。
- 没写需要 `verve::npm` 的 `#[ignore]` 真实库测试（Lit、Shoelace）。
- `checkVisibility()`、`elementsFromPoint` 按扁平树、`compareDocumentPosition`/`isEqualNode` 在 shadow 树下的核对没做。
- 共享的 `:focus-within` 沿 shadow-including 祖先，没把 slot 的扁平祖先算进去。

### 契约变更与跨包事项

- **改了根 crate `src/dom_events.rs`**（简报"修改"一行没列，但事件重定向在不改它的前提下做不到：路径和 `target` 都在那里的
  `dispatch_value`/`composedPath` 里决定）。只新增了 `EventScope { retarget, visible }` 与 `set_event_scope`；没有宿主设置时行为不变。
  提交单独（同上，`feat(events): …`），请合并时留意与动 `dom_events.rs` 的包是否冲突；钩子位于 `src/dom_events/hooks.rs`。
- `ScrollRequest::Reveal(NodeId)` 变成 `Reveal { node, block, inline, smooth }`（简报已写明）；消费方（P30/P32）要跟进。
- 新增公开 API：`Document::{set_visibility, set_window_focus, set_fragments, settle_font}`、`verve_dom::Align`、
  `Snapshot::{flat_children, flat_parent}`、`Dom::{flat_children, flat_parent, shadow_root, shadow_host, shadow, is_shadow_root}`（给 P30；
  其余 shadow/slot 辅助都是 `pub(crate)`）、`Request::LoadFont`、`FontFaceId`/`FontOutcome`（`take_attr_changes`、`watch_attrs` 已是 `pub(crate)`，`tree_revision` 已删，用 `revision`）。
- `SnapshotData` 本包没有新增字段。
- 对其他包：**P30** 按 `Snapshot::flat_children` 画；**P31** 调 `set_fragments`；**P32/P13/P34** 调 `set_window_focus`/`set_visibility`；
  **P20** 用 `settle_font` 结束 face 的加载，整页解析的同步构造由它决定。

### 验收项的验证程度

实际跑过：shadow（连接性、扁平树、slot/`slotchange`、样式作用域、`::part`、继承、事件重定向、`composedPath`、焦点）、自定义元素
（innerHTML 升级、define 升级顺序、`observedAttributes`、回调先于方法返回、`upgrade`、构造器约束）、实时集合、`FontFaceSet`、`innerText`
（10 个样例）、可见性/窗口焦点/`scrollIntoView`/`getClientRects`、声明式 shadow DOM、`arena.rs`（含既有的 shadow 树回收用例）。
只 `cargo check` 过：无。未做：真实库测试、Chrome 对照。
