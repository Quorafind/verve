# P30 DomView 盒模型绘制

**依赖**：P02 P34  **规模**：L  **修改**：新建 `crates/verve-layout`；`crates/verve-gpui/src/dom_view.rs`、`dom_element.rs`；`crates/verve-dom/src/lib.rs`（加布局提供者钩子）；`crates/verve-electron/src/renderer_guest.rs`（接入布局引擎）；`crates/verve-electron/src/msg/input.rs`（`RendererView` 追加字段，见下）

## 目标

把渲染器的 `Snapshot` 画成和 Chromium 足够接近的盒子：块、flex、grid、定位、层叠、溢出滚动、边框、
背景、阴影、透明度。同时让页面脚本**同步**读到正确的几何（改完 DOM 立刻 `getBoundingClientRect()`
也是对的，像浏览器的强制重排一样）。

本包只管"盒子"；文本与行内排版是 P31，输入与表单控件是 P32，增量缓存是 P33，图片/SVG/canvas 是 P35。

## 关键决策：布局在渲染器线程，GPUI 只负责画

**不采用** su-editor 的做法（把每个 DOM 节点映射成一个 GPUI `Div`，让 GPUI 的 taffy 排版，再在每个节点下塞一个
`canvas()` 收集 bounds）。原因是 GPUI 的 `Style` 表达不了很多 CSS，而且 bounds 要等 GPUI 下一帧才能拿到，
脚本读到的永远是旧几何。逐项核对（GPUI 源码：Rebon 所用 fork `Quorafind/zed-c@e679b51`，`crates/gpui/src/style.rs`、`scene.rs`、`color.rs`）：

| CSS 需要 | GPUI `Style` 的能力 | 结论 |
|---|---|---|
| `display: inline / inline-block / contents / table / list-item` | `Display` 只有 `Block / Flex / Grid / None` | 不够 |
| `position: fixed / sticky` | `Position` 只有 `Relative / Absolute` | 不够 |
| `z-index`、层叠上下文 | 没有；只有树序 + `deferred().with_priority()` | 不够 |
| `grid-template-columns: 200px 1fr`、命名区域 | `GridTemplate { repeat: u16, min_size }`，只能 `repeat(n, minmax(_, 1fr))` | 不够 |
| 四边不同颜色的边框、`dotted/double/groove…` | 一个 `border_color`，`BorderStyle` 只有 `Solid / Dashed` | 不够 |
| 多色标渐变、`radial/conic-gradient` | `linear_gradient(angle, from, to)` 只有两个色标 | 不够 |
| `transform: rotate/scale` 一个盒子 | 只有 `svg()`/精灵图支持 `TransformationMatrix`；`Quad` 没有变换 | 不够 |
| 一个子树的整体 `opacity` | `Window::with_element_opacity` 是 `pub(crate)`；公开的只有 `Styled::opacity`（`Div` 画子树时套用） | 不改 fork，见「契约调整」 |
| `box-shadow`（含 inset、多层） | `Vec<BoxShadow>`，有 `inset` | 够 |
| block/flex/float 布局 | GPUI 自己依赖的 `taffy =0.13` 开了 `block_layout`、`flexbox`、`grid`、`float_layout` | **直接用 taffy** |

所以架构定为：

```
renderer-<id> 线程                                          OS 主线程（GPUI）
Document::update() → Snapshot + Changes
        │
        ▼
verve-layout::LayoutEngine（taffy 0.13 + P31 行内排版 + TextMeasurer）
  ├─ 节点盒子 rects（border box，视口坐标） → Document 的布局提供者（同步几何）
  └─ DisplayList（Arc，按层叠顺序排好的绘制项）
        │  放进 RendererView.display_list
        ▼
Hub.renderer_view(id) ──────────────────────────────────────▶ DomElement（自定义 gpui::Element）
                                                               prepaint：插 hitbox、拖动区
                                                               paint：paint_quad / paint_glyph /
                                                                      paint_path / paint_image
```

- `verve-layout` **不依赖 GPUI**。文字测量通过 `TextMeasurer` trait；verve-gpui 用 GPUI 的 `Arc<TextSystem>`
  实现它（字体 id、字形 id 与 GPUI 线程画字时用的是同一个 `TextSystem`，所以通用），无头模式用一个
  定宽假测量器（P00 的测试用）。
- GPUI 线程上不做布局，只按绘制项回放，帧时间稳定。
- 这改变了 P02 契约的数据流向：几何不再从 Shell 回传渲染器，`ToRendererInput::Layout` 不再使用（保留变体不删，
  标注废弃）；`RendererView` 追加 `display_list` 字段。**本包第一个 commit 只改 `docs/electron/` 和
  `msg/input.rs`，说明这一调整**（见 conventions）。

### 第一步的验证：已完成（2026-11-02，见 [`spikes/p30-text-measurement.md`](../spikes/p30-text-measurement.md)）

**结论：可行，不用改 fork 的文字部分。** 在 fork `Quorafind/zed-c@e679b519` 上（Windows 实测，macOS/Linux 待测）：

- `TextSystem` 与 `WindowTextSystem` 都是 `Send + Sync`；不需要 `App`/`Window`：`TextSystem::new(platform.text_system())`
  → `WindowTextSystem::new(Arc<TextSystem>)`；应用运行时用 `cx.text_system()` 拿**同一个** `Arc` 交给渲染器线程。
- 整行塑形用已公开的 `WindowTextSystem::layout_line` / `shape_text`（`text_system.rs:892/754`），主线程、工作线程、8 线程并发
  结果逐字节一致；CJK 回退在工作线程同样生效。
- `FontId` 按 `TextSystem` 实例区分，**必须共享应用的那一个**；这样渲染器线程塑形得到的 `FontId/GlyphId` 可以直接交给
  GPUI 线程的 `Window::paint_glyph`（尚未实跑，P30 实现时第一件事验证）。
- 注意：
  - DirectWrite 的状态在一把 `RwLock` 后面——塑形线程安全但串行，长布局可能卡住 GPUI 线程的栅格化：`TextMeasurer`
    自带缓存并预热字体。
  - `WindowTextSystem::finish_frame` 是 `pub(crate)`，工作线程持有的 `WindowTextSystem` 缓存不会释放：给 fork 打**一个字**
    的补丁（改为 `pub`），或定期重建这个包装。
  - 垂直度量：GPUI 给的是 Windows 的 ascent（Consolas 14.719 vs hhea 11.883），`line-height: normal` 用哪个要与 Chrome 对齐，
    P31 决定。
- 不改 fork（协调者决定：fork 在 GitHub 上，本包不推送）。不透明度分组见下面的「契约调整」；`WindowTextSystem::finish_frame` 属于工作线程上的文字塑形，归 P31。
- 退回方案（GPUI 线程布局、几何晚一帧）**不需要**。

## 背景（先读）

- `crates/verve-dom/src/lib.rs`：`Snapshot`（`dom`、`styles`、`scroll_offsets`、`viewport`、`media`）、`Changes`
  （`nodes/restyled/partitions/removed/restyled_all`）、`set_layout`/`update_layout`/`set_scroll_offsets`、`hit_test`、
  `Request::Scroll`、`ScrollRequest`、`css::length`、`css::color`。
- `crates/verve-dom/src/css.rs`：计算样式以文本保存（`snapshot.style(node, "padding-left")`），简写已展开成长写。
- 参考（只读）：`F:\dev\sandbox-v2\su-editor\crates\su-shell\src\editor\obsidian\view.rs` 的 `styled()`、`color_of`、
  `length_of`、`rotation`：CSS 文本 → 数值的解析可以借鉴，但本包不把节点变成 `Div`。
- GPUI：`crates/gpui/src/element.rs`（`Element` trait 的 `request_layout/prepaint/paint`）、`window.rs` 的
  `paint_quad`、`paint_path`、`paint_layer`、`with_content_mask`、`insert_hitbox`、`insert_window_control_hitbox`、
  `scene.rs` 的 `PaintQuad`/`BorderStyle`/`TransformationMatrix`、`color.rs` 的 `Background`/`linear_gradient`。
- taffy 0.13 文档：`TaffyTree`、`Style`、`compute_layout_with_measure`、`Layout`。

## 接口契约

### `crates/verve-layout`

```rust
/// 字体：由 TextMeasurer 解析出来的不透明 id，跨线程可用（GPUI 实现里就是 gpui::FontId）。
#[derive(Copy, Clone, Hash, Eq, PartialEq)] pub struct FontKey(pub u64);

pub trait TextMeasurer: Send + Sync + 'static {
    /// font-family 列表（已拆好）+ 粗细 + 斜体 → 字体；找不到就按回退链（P31 定义）
    fn resolve(&self, families: &[String], weight: f32, italic: bool) -> FontKey;
    fn metrics(&self, font: FontKey, size: f32) -> FontMetrics;          // ascent/descent/x_height/cap_height
    /// 一段同一字体的文字塑形：每个字形的 id、x 偏移、对应的 UTF-8 字节下标
    fn shape(&self, text: &str, font: FontKey, size: f32) -> Arc<ShapedRun>;
}

pub struct LayoutEngine { /* taffy 树 + 节点映射 + 缓存 */ }
impl LayoutEngine {
    pub fn new(measurer: Arc<dyn TextMeasurer>) -> Self;
    /// 按 Changes 增量更新 taffy 树并重新布局；restyled_all 时整树重建。
    pub fn update(&mut self, snapshot: &Snapshot, changes: &Changes);
    /// 每个已渲染节点的 border box，视口坐标（已减去滚动偏移）
    pub fn rects(&self) -> impl Iterator<Item = (NodeId, Rect)> + '_;
    /// 行内元素跨行时的多个盒子（getClientRects），P31 填充
    pub fn client_rects(&self, node: NodeId) -> Vec<Rect>;
    /// 滚动容器：可滚动范围（scrollWidth/scrollHeight 来源）
    pub fn scroll_extent(&self, node: NodeId) -> Option<[f32; 2]>;
    /// 执行脚本的滚动请求：夹取到范围内，答复新的偏移
    pub fn apply_scroll(&mut self, request: &ScrollRequest) -> Vec<(NodeId, [f32; 2])>;
    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId>;
    pub fn display_list(&self) -> Arc<DisplayList>;
}

pub struct DisplayList { pub revision: u64, pub items: Vec<PaintItem>, pub rasters: Vec<RasterSpec>, pub root: Option<NodeId> /* P33 追加 chunks */ }

pub enum PaintItem {
    Box { node: NodeId, bounds: Rect, radii: [f32; 4], background: Vec<Fill>, border: Border, shadows: Vec<Shadow> },
    Glyphs { node: NodeId, origin: [f32; 2], font: FontKey, size: f32, color: [f32; 4], glyphs: Arc<[PlacedGlyph]> },   // P31
    Decoration { kind: DecorationKind, bounds: Rect, color: [f32; 4], wavy: bool },                                    // P31
    Image { node: NodeId, bounds: Rect, clip: Rect, image: ImageKey, fit: ObjectFit },                                 // P35
    Control { node: NodeId, bounds: Rect, kind: ControlKind },                                                         // P32
    PushClip { bounds: Rect, radii: [f32; 4] }, PopClip,
    PushOpacity(f32), PopOpacity,
    PushScroll { node: NodeId }, PopScroll,          // 滚动帧：其中的项按“该容器及外层帧偏移为 0”摆放，回放时平移 −offset(node)
    Hitbox { node: NodeId, bounds: Rect, cursor: Cursor, app_region: Option<AppRegionKind> },                          // P32/P34
    Scroller { node: NodeId, viewport: Rect, content: [f32; 2], offset: [f32; 2], scrollbar: bool },                  // 在自己的帧之外
}
pub enum Fill { Color([f32; 4]), Linear { angle: f32, stops: Vec<([f32; 4], f32)> }, Raster(ImageKey) /* 复杂渐变栅格化 */ }
pub struct Border { pub widths: [f32; 4], pub colors: [[f32; 4]; 4], pub styles: [BorderLine; 4] }
```

### verve-dom：布局提供者（同步几何）

```rust
/// 一次布局的结果：provider 的答复。
pub struct Layout {
    /// 每个已渲染元素的 border box（视口坐标，滚动已计入）。
    pub rects: Vec<(NodeId, Rect)>,
    /// 每个滚动容器（页面用根元素）的滚动偏移，已夹取到可达范围：`scrollTop`/`scrollLeft` 的答案。
    pub scroll_offsets: Vec<(NodeId, [f32; 2])>,
    /// 每个已渲染元素的 `[scrollWidth, scrollHeight]`（来源是 `LayoutEngine::scroll_extent`）。
    pub scroll_extents: Vec<(NodeId, [f32; 2])>,
}

impl Document {
    /// 脚本读几何（getBoundingClientRect、offset*/client*/scroll*、elementFromPoint、getComputedStyle 的
    /// width/height 用到的值）而当前布局已过期时调用：先取出快照（`Update`），再交给 provider 算，
    /// 用返回的 `Layout` 无声地替换 `rects`、`scroll_offsets` 与滚动范围（等同 set_layout，但不触发观察者回调，
    /// 也不发 `scroll` 事件）。provider 取走的 `Changes` 留给下一次 `update()`。
    pub fn set_layout_provider(&self, provider: Option<Box<dyn FnMut(&Update) -> Layout>>);
    /// 过期就现在布局（发布视图前用）。
    pub fn ensure_layout(&self);
    /// 把布局变化告诉 ResizeObserver/IntersectionObserver：有 provider 的页面每个回合末自己调用。
    pub fn deliver_layout(&self, runtime: &mut verve::Runtime);
}
```

"过期"的判定：自上次布局以来 `Changes` 非空（含动画改了样式的节点，它们不动任何修订号），或修订号、视口变化。
没有 provider 时行为不变（现有测试必须仍过）。

脚本的滚动：`scrollTo`/`scrollBy`/`scrollTop = …` 只把请求的偏移写进文档（`request_scroll`，一次 `scroll` 事件由它的定时器发），
布局在下一次读取时把它夹取到可达范围并写回，所以 `scrollTop = 9999` 立刻读到夹取后的值、`scrollBy` 只加一次。
`Request::Scroll` 里只有 `Reveal`（`scrollIntoView`）还要渲染器用 `apply_scroll` 处理；`To`/`By` 不需要。
`scrollWidth`/`scrollHeight` 读 `scroll_extents`，不受滚动偏移影响。

### `msg/input.rs` 追加

```rust
pub struct RendererView { /* P02 原有字段 */ pub display_list: Option<Arc<verve_layout::DisplayList>> }
```

`ToRendererInput::Layout` 在 `msg/input.rs` 里已经标了废弃（P02 之后），本包不再重复；变体保留。`display_list` 字段随 `verve-layout`
接入渲染器（`renderer_guest.rs` 依赖它）一起加进 `RendererView`，不在 crate 存在之前加。

### 契约调整（P30 实现前确定，2026-11-07）

- **不透明度分组不改 fork。** `PushOpacity`/`PopOpacity` 成对出现；回放它们的办法由 `DomElement` 决定（见下面的盒子绘制规则）。
  `finish_frame`、`TextMeasurer` 的 GPUI 实现、字形画法的核对属于 P31，本包只定义 `TextMeasurer` 和无头测量器。
- **依赖。** `verve-layout` 依赖 `verve-dom`（读 `Snapshot`、`Rect`、`ScrollRequest`、`css::color`/`length_calc`）和 `taffy =0.13.0`（与 GPUI
  同一份，只开 `std taffy_tree flexbox grid block_layout float_layout content_size`），**不依赖 GPUI**。复杂渐变（径向、圆锥、`repeating-*`）的栅格化是 verve-gpui 自己的逐像素代码（`raster.rs`），在 GPUI 线程上画；软件绘制（`--screenshot`）用已在依赖图里的
  `tiny-skia 0.11` 这一份，不增加第二份，PNG 用 `image 0.25` 的编码器。
- **`Fill::Raster` 的来源。** `DisplayList` 带 `rasters: Vec<RasterSpec>`（渐变的解析结果、尺寸、`ImageKey`），由 Shell 在 GPUI 线程上据此栅格化
  （按窗口缩放，每边最多 2048 设备像素，超出的由绘制方拉伸；straight alpha），渲染器线程不画；`Fill::Raster(key)` 引用它。`DisplayList.chunks` 仍由 P33 追加，`PaintItem::Image.frame` 由 P35 追加（见 P02 的追加表）。
- **`DisplayList.root` 与 `revision`。** `root: Option<NodeId>` 是页面的滚动容器，滚轮链在这里结束；页面不滚动（或 `html{overflow:hidden}`）时为 `None`。
  `revision` 在整个进程内唯一（静态 `AtomicU64`），导航换了引擎也不会重复，Shell 与截屏靠它判断是不是同一份列表。
- **`Hitbox` 的形状（P32、P34 消费）。** `Hitbox { node, bounds, cursor, app_region }`：`bounds` 是边框盒（所在帧的空间，CSS 像素），
  并只裁到**同一帧里**的裁剪（`overflow: hidden` 而不可手动滚动的祖先）；`cursor` 是计算样式 `cursor` 的枚举（`Cursor`，缺省 `Auto`；`cursor` 本来就继承，所以不必再找祖先）；`app_region` 是 `AppRegionKind`（`Drag`/`NoDrag`），来自
  `-webkit-app-region`（或标准名 `app-region`）。P34 的 `app_region::AppRegion` 是 `{ rect, drag }`：`rect` 取解析后的命中盒的 `bounds`，`drag = (kind == Drag)`。
  `pointer-events: none` 的元素不出 `Hitbox`。`Hitbox` 按绘制顺序排列。**P32、P34 不自己推栈**：用 `DisplayList::resolved_hitboxes(offset_of)`（窗口坐标、已按帧偏移平移并裁到所在裁剪）
  和 `DisplayList::hit_test(x, y, offset_of)`；`offset_of(node)` 是 Shell 本地预测的偏移（比列表新时），没有就读 `Scroller` 的。
- **滚动帧：`PushScroll { node }` / `PopScroll`（取代契约里的 `PushTranslate`/`PopTranslate`）。** 可手动滚动的容器（`overflow: auto/scroll` 且有内容可滚，页面在
  内）是一个*帧*：帧里的项按“该容器及其外层所有帧的偏移都为 0”摆放，回放方在 `PushScroll` 处平移 −offset(node)；偏移取 Shell 本地预测的（比列表新时），否则取
  该容器 `Scroller { node, offset }` 里的值。所以**一次滚动只改 `Scroller` 的 offset，列表里别的项一个字节不变**（P33 的块因此不重建，Shell 可以立刻本地滚动，
  渲染器的偏移随后为准）。容器自己的 `PushClip` 和 `Scroller` 在它的帧之外；帧里的 `PushClip` 才是帧内裁剪。不可手动滚动的容器（`overflow: hidden/clip`、
  没有内容可滚的）不是帧：它们的滚动偏移、`position: sticky` 的位移、`transform: translate` 都直接加在位置上。`Scroller` 多一个 `scrollbar: bool`
  （`scrollbar-width: none` 时为假）。`LayoutEngine::rect`/`rects`（脚本几何）仍是视口坐标，滚动已计入。
  v1 偏差：Shell 本地预测期间，帧内的 sticky 盒随所在帧一起走，渲染器重新发布时才纠正。
- **`LayoutEngine::update` 的形状。** `update(&Snapshot, &Changes) -> bool`（有变化为真）；另有 `rect`、`client_rects`、`scroll_extent`、
  `scroll_offset(s)`、`apply_scroll`（答复移动过的 `(NodeId, [f32; 2])`，由调用方交给 `Document::set_scroll_offsets`）、`hit_test`、`display_list`。
- **行内内容（P31 之前）。** 文字节点不进布局；`display: inline` 的元素自己不建盒，它的子元素像 `display: contents` 那样提升到父盒（`<a><div>…</div></a>` 照常排）；`inline-block`/`inline-flex`/`inline-grid` 与替换元素暂按块级盒参与
  块流，宽度 `auto` 时不收缩。P31 的行内格式化上下文接管这些，接口在 `tree.rs` 里分出“行内游程”的那一处。
- **`::before`/`::after`。** 生成盒没有 `NodeId`，P30 不为它们建盒；带文字的是 P31 的，没有文字的装饰性方块暂不画。

## CSS → taffy 映射规则

| CSS | 做法 |
|---|---|
| `display: block/flow-root/list-item` | `taffy::Display::Block`（`list-item` 的标记由 P22 的 `::marker` 生成内容提供） |
| `display: flex/inline-flex`、`grid/inline-grid` | `Flex`/`Grid`；`inline-*` 在外部作为原子行内盒参与 P31 的行布局 |
| `display: inline` | 不建 taffy 节点，交给 P31 的行内格式化上下文 |
| `display: inline-block` | 原子行内盒：自身一个 taffy 子树（`Block`），由 P31 放进行里 |
| `display: none` | 不建节点；`visibility: hidden` 建节点但不出绘制项 |
| `display: contents` | 不建自身节点，子节点提升到父节点 |
| `display: table*` | v1：`table` → `Grid`（列数取第一行单元格数，列宽 `auto`），`table-row` → `contents`，`table-cell` → `Block`；`colspan` 映射为 `grid-column: span n`。真实表格算法不做 |
| `float`、`clear` | taffy `float_layout` |
| `position: relative` | taffy `Relative` + inset |
| `position: absolute` | taffy `Absolute`；包含块 = 最近的非 static 祖先：taffy 只认父节点，所以**把绝对定位节点挂到其包含块对应的 taffy 节点下**（DOM 父子与 taffy 父子可以不同，映射表记录） |
| `position: fixed` | 同上，包含块是视口：挂到根，偏移不受滚动影响 |
| `position: sticky` | 布局时按 `relative` 处理；摆放时按最近滚动容器的偏移和 `top/bottom/left/right` 阈值夹取出位移，直接加进位置（不出父盒） |
| 尺寸、`min/max-*`、`box-sizing`、百分比、`auto`、`aspect-ratio` | taffy 原生支持；百分比基于包含块；`calc()` 由 P22 在计算样式里化简成 `px`/`%` 组合，taffy 0.13 不支持的混合 calc 退回 `auto` 并记录 |
| `margin`（含 `auto`）、`padding`、`border-*-width` | taffy 原生 |
| `gap`、`flex-*`、`align-*`、`justify-*`、`order` | taffy 原生（`order` 在建树时按值稳定排序子节点） |
| `grid-template-*`、`grid-area`、`grid-auto-*`、命名线 | taffy 0.13 原生支持轨道列表、`repeat()`、`minmax()`、`fr`；命名区域在建树时解析成行列号 |
| `overflow: hidden/clip/scroll/auto` | taffy `Overflow`；绘制时 `PushClip`；`scroll/auto` 且内容超出（或 `scroll`）→ `PushScroll` 帧和 `Scroller` 项；一轴非 `visible` 时另一轴按 `auto` |

## 层叠与绘制顺序

按 CSS 2.1 附录 E 的简化版生成 `DisplayList`：每个层叠上下文（根、`position` 非 static 且 `z-index` 非 auto、
`opacity<1`、`transform` 非 none、`position: fixed/sticky`）内依次：
自身背景边框 → 负 `z-index` 子上下文 → 块级流内子孙 → 浮动 → 行内内容（P31） → `z-index: auto/0` 的定位子孙（树序）
→ 正 `z-index` 子上下文（按值，同值按树序）。`hit_test` 按相反顺序。

## 盒子绘制规则（`DomElement::paint`）

- 背景色：`PaintQuad` 的 `background`。`linear-gradient` 两个色标 → GPUI `linear_gradient`；角度为 0/90/180/270
  的多色标 → 切成多个相邻的两色标 quad；其他（多色标斜角、`radial-gradient`、`conic-gradient`、`repeating-*`）→ `Fill::Raster`：
  由 Shell 在 GPUI 线程上用 verve-gpui 自己的代码（`raster.rs`）栅格化成图片，按 (渐变, 缩放) 缓存，每边最多 2048 设备像素（见「契约调整」）。
- 边框：四边颜色和样式相同 → 一个 `PaintQuad`（`border_widths`、`border_color`、`BorderStyle::Solid/Dashed`）。
  不同 → 每边用 `paint_path` 画梯形（含圆角时退回一个 quad 用上边颜色，记录为已知偏差）。`dotted` 画成 `Dashed`，
  `double/groove/ridge/inset/outset` 画成 `Solid`（`inset/outset` 按亮暗调整颜色）。
- 圆角：`corner_radii`；椭圆半径（`10px / 20px`）取较小值。
- `box-shadow`：GPUI `BoxShadow`（含 `inset`、多层，顺序与 CSS 一致：第一层在最上）。
- `opacity`：`PushOpacity`。GPUI 侧不改 fork：优先用公开的 `Styled::opacity` 套一层 `Div` 回放组内的绘制项；做不到就把 alpha 乘进组内每一项（重叠的后代各自混合，是已知偏差，写进「状态」）。
- `transform`：v1 只支持 `translate*()` 和 `matrix()` 中的平移分量（直接加进位置，不影响布局，影响 `rects` 与命中）；
  `scale/rotate/skew` 对盒子不生效（P35 对 `<img>`/`<svg>` 生效），并在 `console.warn` 只提示一次。动画由 P22 产出每帧计算值。
- `visibility: hidden`：不出 `Box`/`Glyphs`，但保留布局；`visible` 的后代仍要画。
- `outline`：画在盒子外侧的第二个 quad（不占布局）；`outline-offset` 支持。
- `cursor`、`-webkit-app-region`、`pointer-events: none`：体现在 `Hitbox` 项（P32/P34 消费）。

## 滚动

- 滚动偏移的唯一真相在渲染器：`Snapshot.scroll_offsets`。脚本的 `Request::Scroll` 由 `RendererGuest` 拦截，交给
  `LayoutEngine::apply_scroll` 夹取，再 `Document::set_scroll_offsets`（触发 `scroll` 事件），**不再发给 Shell**。
- 滚轮：目标是命中元素向上最近的、能往该方向滚的帧（`Scroller`），滚不动了就链到外一层，不是指针下最上面的 `Scroller`。Shell 先改本地偏移（回放时 `PushScroll` 平移，
  立即响应），同时发 `ToRendererInput::Wheel`；渲染器派发 `wheel` 事件，未被 `preventDefault` 就按同样的量 `apply_scroll` 并发布。
  Shell 收到新显示列表后以渲染器的偏移为准（本地预测值丢弃）；因为一次滚动只改 `Scroller`，这张列表和上一张只差偏移。
- 滚动条：v1 画 GPUI 风格的覆盖式细滚动条（悬停变粗），不占布局；`::-webkit-scrollbar` 样式不支持（记录）。
  `scrollbar-width: none` 时不画。

## 步骤

1. Spike（见上），结论写回本文。
2. 建 `verve-layout`：`TextMeasurer`、`LayoutEngine` 骨架、CSS 文本解析（长度/颜色/渐变/阴影/变换）模块，单测。
3. 建树：DOM → taffy（包含块重挂载、`contents`、`none`、表格近似、`order`），整树布局。先不做增量（P33）。
4. 层叠上下文与 `DisplayList` 生成；`hit_test`。
5. verve-dom 布局提供者钩子；`RendererGuest` 接入：每次 `publish` 前 `update` 布局、把 rects 交给 `set_layout`、
   显示列表放进 `RendererView`。
6. verve-gpui：`DomElement`（`request_layout` 占满窗口；`prepaint` 插 hitbox；`paint` 回放），替换 P02 的占位 DomView。
7. 无头 `TextMeasurer`（每字 0.5em 宽，ascent 0.8em）供测试。
8. 视觉测试（见验收）。

## 验收

- `cargo test -p verve-layout`：针对每条映射规则的布局单测（期望值取自 Chromium 对同一 HTML 的
  `getBoundingClientRect`，用无头测量器时只测不含文字的盒子）。至少覆盖：块流与外边距、flex 换行与对齐、
  `grid-template-columns: 200px 1fr`、命名区域、浮动、absolute 包含块、fixed、sticky、`display: contents`、`overflow` 裁剪、`z-index` 顺序。
- `cargo test -p verve-electron --test geometry`：脚本 `el.style.width='123px'; el.getBoundingClientRect().width` 同步得到 123
  （无头测量器 + 布局提供者）。
- 夹具 `tests/electron/apps/boxes/`（P00 的目录约定）：一页覆盖上述能力的静态 HTML。`verve-gpui` 提供
  `--screenshot <path>`（P43 也会用）把窗口渲染成 PNG；与 `tests/electron/apps/boxes/expected-chromium.png`（用真实 Electron 截取，
  同尺寸、DPR 1）逐像素对比，差异像素 < 3%（文字区域除外，mask 由夹具提供）。
- 2000 个绝对定位彩色方块的页面：GPUI 帧时间 p95 < 4 ms（release，P33 再优化增量）。

## 不做

文字与行内（P31）、表单控件外观（P32）、增量与缓存（P33）、图片/SVG/canvas（P35）、盒子的旋转缩放、
`filter`/`backdrop-filter`/`mix-blend-mode`/`clip-path`/`mask`（v1 忽略，记录在 P41 矩阵）、`::-webkit-scrollbar`、多列布局 `columns`。

## 状态

### 第一阶段：`verve-layout`（无 GPUI、无渲染器接线）

**做了**（步骤 2、3、4、7）：

- `crates/verve-layout`：`TextMeasurer`/`FontKey`/`FontMetrics`/`ShapedRun`/`PlacedGlyph`（形状取 P31 的契约）和无头测量器（每字 0.5em，ascent 0.8em）；
  CSS 文本读取（切分与函数读取用 verve-dom 导出的 `css::split_slices`/`function_parts`，长度含 `em/rem/vw` 与 `calc()`、线性/径向/锥形渐变及 `repeating-`、`box-shadow`、`transform`/`translate` 的平移部分、
  网格轨道/命名区域/`grid-area`/`grid-row`/`grid-column`），都有单元测试。
- 建树：按扁平树（影子 DOM 的宿主与槽位；`<slot>` 无显式 `display` 时按 `contents`）；`none`、`contents`、不在 flex/grid 里的行内元素、表格的行与行组不建盒（flex/grid 容器的行内孩子按规范“块化”，建盒）；绝对定位盒挂到包含块
  （最近的非 static 或有 transform 的祖先），fixed 挂到视口（或有 transform 的祖先），原位留一个同样式的“幽灵”节点（连尺寸一起，所以 flex/grid 父盒的对齐也算进静态位置），用来给两侧 inset 都是 `auto` 的轴找回静态位置
  （布局两遍）；竖向百分比 `padding` 在第一遍后换成像素（taffy 0.13 的块条目列表拿高度当基准，是它的缺陷）；`order` 稳定排序；表格按 `colspan`/`rowspan` 摆成网格（列数取最宽的一行，`border-spacing` 作 gap 和外圈 padding，`caption` 占首行）；根元素是 `flow-root`。
- 整树布局（taffy 0.13，`block_layout float_layout flexbox grid content_size`，关闭取整）；`LayoutEngine::update/rects/rect/client_rects/scroll_extent/scroll_offset(s)/apply_scroll/hit_test/display_list/unsupported`。
  `apply_scroll` 支持 `To`/`By`/`Reveal`（`scrollIntoView` 自内向外滚各层容器，`smooth` 当作立即）。
- 摆放：滚动偏移夹取、sticky（相对最近滚动容器的可视区，且不出父盒）、`transform` 平移、页面滚动范围（不含 fixed 与被裁剪的子孙）；每个盒两套矩形：`rect`（视口坐标，脚本读）和
  `paint_rect`（所在滚动帧的空间，显示列表用）。
- 层叠上下文与 `DisplayList`：CSS 2.1 附录 E 的简化（见 `paint.rs` 的模块文档）；裁剪、滚动帧与不透明度用“当前打开的效果栈”输出 `PushClip/PopClip/PushScroll/PopScroll/PushOpacity/PopOpacity`，
  绝对定位盒按包含块链裁剪；画布背景取根元素、没有则取 body；轮廓放在**所属**上下文末尾（不是最近结束的内层上下文）；`Hitbox`（帧内空间的边框盒∩同帧裁剪、`cursor`、`app_region`，
  `pointer-events:none` 不出）和 `Scroller`；`DisplayList::resolved_hitboxes`/`hit_test` 按 Shell 的预测偏移还原窗口坐标。
  层叠上下文的触发另含 `isolation: isolate`、`filter`、`mix-blend-mode`、`contain: paint/layout`、`will-change` 对应值（它们也是 fixed/absolute 的包含块，与 `transform` 同）。
- 测试：`tests/chromium.rs`（21 页无文字的页面，期望值取自 Electron 39.8.10 / Chromium 142 / Windows / DPR 1，视口 800×600；探针脚本在 `tests/chromium/`，README 说明如何重采）和 `tests/behavior.rs`（显示列表、裁剪、
  不透明度、命中、滚动帧、sticky、fixed、文档变化）。第一轮协调者的中央运行：62 通过 5 失败，已在第二轮修复（渐变角度、页面滚动重复扣减、影子 DOM 探针脚本、竖向百分比 padding）。

**没做 / 已知偏差**：

- **文字与行内（P31）**：文字节点不进布局；`display: inline` 的元素（不在 flex/grid 里时）透明（子元素提升）；`inline-block`/`inline-flex`/`inline-grid` 与替换元素按块级盒参与块流，宽度 `auto` 时不收缩
  （`inline-block` 映射成 `flow-root`）。`<canvas>`/`<iframe>`/`<video>`/`<embed>`/`<object>`/`<svg>` 默认 300×150，`<img>` 取 `width`/`height` 属性；表单控件的固有尺寸是 P32 的。
  P31 要在 `tree.rs` 的 `Builder::visit_children` 处接入：现在它逐个访问孩子，P31 在那里把一个块的行内孩子收成游程；行内内容的绘制接在 `paint.rs` 的 `Painter::context`（“P31”注释处，浮动之后、定位盒之前）。
  `position: relative` 的行内元素是它后代绝对定位盒的包含块，现在因为行内元素不建盒而被跳过——这也交给 P31（行内盒的包含块要用它的第一个行框）。
- **`::before`/`::after`** 没有 `NodeId`，不建盒（`Snapshot.pseudo` 的生成盒归 P31/P22 的接线）。
- **表格** 是网格近似：`border-spacing` 以 gap 和外圈 padding 近似；表格宽度 `auto` 时撑满（Chromium 收缩到内容），列宽按 `auto` 轨道分配，没有 `border-collapse: collapse` 的边框合并。
- **`calc()`** 里长度与百分比混用、`max-content`/`min-content`/`fit-content` 作尺寸：退回 `auto`，计入 `LayoutEngine::unsupported()`。`scale/rotate/skew` 同样计入（盒子不旋转缩放）。
- **滚动条覆盖式、不占布局**。Chromium 的经典滚动条会占 15px（`overflow: auto` 的容器里子盒宽 185 而不是 200，已观察），探针页因此都写了 `scrollbar-width: none`；
  带滚动条的真实页面与 Chromium 会差这 15px。`overflow: clip` 当 `hidden`；`html`/`body` 的 `overflow` 不上传给视口（`html { overflow: hidden }` 只是不出页面的 `Scroller`）。
- **滚动帧**取代契约里的 `PushTranslate`/`PopTranslate`（见上文“契约调整”）：一次滚动只改 `Scroller` 的 offset。不可手动滚动的容器（`overflow: hidden`）的偏移、sticky 位移和 `transform` 平移直接在位置里，
  所以 Shell 本地预测滚动时，帧内的 sticky 盒随帧一起走，渲染器重新发布才纠正（已知偏差）。滚轮的目标（命中元素向上最近的能滚的帧，链式）是 P32 的事，契约已写在 P30 的“滚动”一节。
- **整树重建**：`update` 在文档变化时重做整棵 taffy 树（P33 做增量）；滚动只重新摆放。幽灵节点用与真盒相同的样式，但百分比 margin 的基准是它所在的父盒而不是包含块，略有不同。`LayoutEngine::update` 之外的滚动不触发布局。
- 页面滚动范围不算 `transform` 位移后的盒子。`Fill::Raster` 的栅格化（tiny-skia）在第二阶段；这里只产出 `DisplayList.rasters`（渐变解析结果、尺寸、`ImageKey`）。
- **不透明度**：没有改 fork（按协调者决定）。核对过 e679b51：`Window::with_element_opacity` 只是把 alpha 乘进此后每个图元的颜色（`paint_quad`、`paint_glyph`… 各自读 `element_opacity`），
  `Styled::opacity` 走的也是它，所以**没有公开的“分组不透明度”路径，改成 `pub` 的补丁也不会消除偏差**。v1 的行为就是逐图元乘 alpha：重叠的后代各自混合（已知偏差，第二阶段由 `DomElement` 在
  回放 `PushOpacity` 时实现）。`finish_frame` 的补丁（`crates/gpui/src/text_system.rs:884` `pub(crate) fn finish_frame(&self) {` 改成 `pub fn finish_frame(&self) {`）是 P31 的：工作线程上的 `WindowTextSystem` 缓存释放。
- **第二轮顺带的范围外改动**：`crates/verve-dom/src/css/syntax.rs` 导出 `split_slices`（借用的切分，反斜杠转义）和 `function_parts`，`css::split_outside` 改为它的包装；verve-layout 用它们，不再自带一份切分/函数读取。
- 竖向百分比 `padding` 靠“布局两遍”修正；`outline-style: auto`（焦点环）画成实线；`matrix()` 保留平移分量，同时缩放/旋转的算作不支持；gradient 的 `ImageKey` 最高位置 1，与 P35 的图片键分开。

**观察到的 Chromium 行为**（Electron 39.8.10 / Chromium 142.0.7444.265，Windows，DPR 1，隐藏窗口 800×600；探针脚本在 `crates/verve-layout/tests/chromium/`）：

- `display: contents`/`none` 元素的 `getBoundingClientRect()` 是全零；`visibility: hidden` 的元素保留位置。
- `elementFromPoint` 在没有元素盖住的窗口内一点答复 `html`。
- 经典滚动条占 15px（见上）；`overflow: scroll` 的 `scrollWidth/Height` 不含滚动条。
- `aspect-ratio` 加定高、`width: auto` 的块级盒宽度由比例得出（90×60，不是撑满）。
- `float` 的 `width: auto` 收缩到内容（40px）；`overflow: hidden` 的块包含浮动；`clear: both` 的块落在浮动之下。
- 边距折叠：兄弟之间取大者，首个子盒的上边距穿过父盒（也穿过影子宿主）；负边距相加。
- 绝对定位盒在两侧 inset 都是 `auto` 时取静态位置（含 `margin-left` 与 `padding-top`）；包含块是祖先的 padding box，百分比 inset 与尺寸以它为准；`position: relative` 的 `left/top/right/bottom` 只移动自身；
  static 元素的 inset 无效。
- sticky 在 `overflow: auto` 的容器里滚动 100px 后 `top: 10px` 的盒子停在 y=10；`bottom: 5px` 的在可视区内时不动。
- `transform: translate(...)` 改变 `getBoundingClientRect` 和命中，不改变后面盒子的位置；百分比以自身为准。
- 命名区域的网格里 `1fr` 行吃掉剩余高度（300 − 50 − 40 = 210）。
- 页面滚动 300px 后：流内盒上移 300，绝对定位盒（包含块是初始包含块）上移 300，fixed 不动，`position: sticky; top: 20px` 停在 y=20；`documentElement` 的 rect 也上移。
- flex/grid 容器里的 `<span>` 是块级的 flex/grid 项（`<span style="width:16px;height:16px">` 得 16×16 的盒）；`display: inline-block` 的项也块化。
- `border-spacing: 4px` 的表格：单元格离表格边 4px，间距 4px，表格高 28。

**给其他包**：

- **P31**：`TextMeasurer` 在 `measure.rs`；`BoxProps` 不带文字属性，你在 `props.rs` 里加；`LayoutEngine::measurer()` 取测量器；`DisplayList` 里 `Glyphs`/`Decoration` 已定义未产出。
- **P32/P34**：`Hitbox` 的形状见“契约调整”；`app_region.rs` 要的矩形来自 `DisplayList::resolved_hitboxes(offset_of)` 里 `app_region` 非空的项（`AppRegionKind::Drag` ↔ P34 的 `AppRegion { rect: bounds, drag: true }`），
  顺序即绘制顺序（后者覆盖前者）；`no-drag` 后代在 `drag` 祖先之后。键盘与指针的转发桥是 P32 的，这里没有做。滚轮目标见“契约调整”。
- **P33**：`LayoutEngine::update` 是整树重建，`DisplayList.revision` 在整个进程内唯一，只在列表变了时换新号，内容相同时 `Arc` 不变；一次滚动只让 `Scroller` 的 offset 不同；`chunks` 由你追加。
- **P35**：`ImageKey` 和 `PaintItem::Image` 已定义；`Fill::Raster` 的 `ImageKey` 来自 `DisplayList.rasters`。

**要跑的测试**（一次一条）：

```
cargo test --locked -p verve-layout
cargo clippy --locked -p verve-layout --all-targets -- -D warnings
```

第二轮还动了 verve-dom，所以也要 `cargo test --locked -p verve-dom --test css`（切分器的行为不变）和 `cargo check --locked -p verve-dom -p verve-electron --all-targets`。

### 第二阶段 A：接进页面（无 GPUI）

**做了**：

- 第一阶段遗留两处：表格的 `width` 按 border-box 算（`border-spacing` 变成的外圈 padding 之前被加在 `width` 之外，多了 8px）；`place.rs` 的 `scroll_state` 返回具名的 `ScrollState`（clippy `type_complexity`）。都以 fixup 并入原提交。
- **verve-dom 布局提供者**（`layout_provider.rs`）：`Document::set_layout_provider(Option<LayoutProvider>)`，`LayoutProvider = Box<dyn FnMut(&Update) -> Layout>`，
  `Layout { rects, scroll_offsets, scroll_extents }`（契约见上）。脚本读几何（`getBoundingClientRect`、`offset*`/`client*`/`scroll*`、`img.width`、观察者、`elementFromPoint`/`elementsFromPoint`）
  或指针对准（`hit_test`、`press`）时，若文档过期，就 `take_update()` 出快照交给 provider，无声地换掉 `rects`、`scroll_offsets`（夹取后的）与 `scroll_extents`。
  “过期”按任务书的规则：自上次布局以来有未见的 `Changes`（`view.changed`/`restyled`，其中包括动画只改样式、不动任何修订号的节点）；另外 `(dom.revision + state.revision, 视口)` 与上次不同也算
  （脚本改了文档但还没被 `absorb` 时，靠它）。provider 取走的 `Changes` 存在 `View.carried`，下一次 `Document::update()` 并回，所以绘制方一个变化也不会少（`sweep` 会清掉其中已释放的节点）。
  没有 provider 时行为不变。另有 `Document::ensure_layout()` 和公开的 `Document::deliver_layout()`（观察者）。provider 以弱引用持有文档，重入时不再被问。
  为了不超 `lib.rs` 的行数上限，`Changes`/`Update`/`Document::update`/`snapshot` 搬到 `update.rs`（`pub use` 不变），加了 `Changes::merge`。没有动 `Options`。
  `scrollTop`/`scrollLeft`（含 `window.scrollY`）读时走 `fresh`，`scrollWidth`/`scrollHeight` 读 `scroll_extents`（没有 provider 时仍是子孙矩形的并集）；
  `clientWidth`/`clientHeight`/`offset*` 仍取自元素的 border box 矩形（没有扣边框和滚动条，P31/P32 之后再细化）。
- **`RendererGuest`**（`page_layout.rs`）：`PageLayout` 持有 `LayoutEngine`（无头测量器，P31 换真的）并装成 provider；`publish` 先 `ensure_layout()`，再 `update()`，`RendererView.display_list` 带上列表；
  只要列表变了（比如滚动）也会发布。`deliver_layout()` 每个回合在 `tick()` 里、`beat::run` 和 `pump` 之后调用一次（回调提的请求由随后的 `drain` 取走）：没有 Shell 报告盒子，ResizeObserver/IntersectionObserver 靠它在无头下也会触发（它们原先随 Shell 的 `Layout` 消息来）。
  `drain` 不再整树布局：观察者读几何走 `element::painted` → `layout_provider::fresh`，只在有人读且文档过期时才布局，所以一批 100 条消息只布局一次。
  `Request::Scroll`：`To`/`By` 在文档里已经做完（`request_scroll` 写偏移，下一次读时布局夹取并写回，`scroll` 事件由它的定时器发，只一次），渲染器不再重放；`Reveal` 经 `apply_scroll` 处理，
  再 `set_layout`、`set_scroll_offsets`。这些请求**不再发给 Shell**；同一批里别的请求先入 outbox，滚动后处理。
- **无头 Shell**：删掉 `FakeLayout`/`BlockStack`、`HeadlessOptions.layout`、公开的 `verve_electron::FakeLayout`、`headless/layout.rs`，不再发 `ToRendererInput::Layout`；`RendererGuest::input` 忽略它（变体保留，已废弃）。布局全在页面线程。
  无头窗口的视口等于窗口的 bounds（宽高）；真 Electron 的 `BrowserWindow` `width`/`height` 含窗口边框，除非 `useContentSize`，所以两者在有边框时不一样。
- 测试：`crates/verve-dom/tests/layout_provider.rs`（一次布局只问一次、变化仍进 `update()`、无 provider 不变、视口变化、读不触发观察者、动画改样式后重布局、滚动夹取与 `scrollBy` 不重复、`scrollHeight` 不随滚动变），
  `crates/verve-electron/tests/geometry.rs`（`style.width='123px'` 后同步读 123、`elementFromPoint`、容器与页面滚动及夹取、`scrollBy` 两次得 100、ResizeObserver 在 observe 之后和宽度变化之后各触发一次）。
- **任务书「修改」之外动过的文件**：`verve-dom` 的 `state.rs`（`layout_hook`/`laid`/`scroll_extents`/`layout_key`）、`bind/document.rs`（`elementFromPoint` 读前布局）、`bind/element.rs`（`painted`/`scrolled`/`content_size`）、`bind/window.rs`（`scrollY`）、`canvas/img.rs`（`img.width`）；
  `verve-electron` 的 `lib.rs`（去掉 `FakeLayout` 导出）、`Cargo.toml`（依赖 `verve-layout`）、`headless.rs`、`msg/input.rs`。

**期望变了的现有测试**（文字在 P31 之前不进布局，且 `BlockStack` 忽略边距而真布局不忽略）：

- `tests/headless.rs`：`blocks_stack_over_the_full_width_and_text_is_sixteen_pixels_a_line` 改名并去掉“每字 8px、按钮和文字一样宽”的断言（那是假布局的规则）；页面改成给 div/p/button 明确 `height:16px`、边距清零，断言 `a=[0,0,500,16]`、`p=[0,16,500,16]`、`b=[0,32,500,16]`。
  `resizing_a_window…`：文字折行变成“块宽等于窗口宽”，`[0,0,40,32]` → `[0,0,40,16]`。
- `tests/web_contents.rs::input_events_click_and_type_in_the_page`：按钮/链接/可编辑 div 原靠 16px 一行的假高度，现在在样式里明确 `height:16px` 并清边距，坐标 (5,5)/(5,21)/(5,37) 不变。
- `tests/fixtures.rs::a_react_app_built_by_vite_…` 仍 `#[ignore]`（理由写着 P30 和 P20）：点击靠 `press(node)`，不依赖几何，P30 已不是障碍；是否放开由协调者试跑后定。

**已知偏差 / 给后面的人**：

- 每次 `publish` 前若文档变了就整树重新布局（P33 做增量）。**给 P33**：`request_scroll` 里的 `touch()` 会移动引擎的键（修订号），所以每次 `scrollTop =` 都走整树重建，引擎“只重新摆放”的快路径走不到；
  `publish` 在文档变了时会取两份 O(n) 的快照（provider 一份、`update()` 一份）。
- 滚轮在渲染器上仍只派发 `wheel` 事件，滚动目标与本地预测是第二阶段 B 的事。`ToRendererInput::ScrollOffsets` 仍未处理。
- Electron 行为：本阶段的新测试断言的是 Verve 自己的规则（同步读几何、夹取到可滚范围），没有再对照 Electron 采样；窗口高度 300 取自 `BrowserWindow` 的 `height`。

**要跑的测试**（一次一条）：

```
cargo test --locked -p verve-layout
cargo test --locked -p verve-dom --test layout_provider
cargo test --locked -p verve-dom
cargo test --locked -p verve-electron --test geometry
cargo test --locked -p verve-electron --test headless
cargo test --locked -p verve-electron --test web_contents
cargo test --locked -p verve-electron
cargo clippy --locked -p verve-layout -p verve-dom -p verve-electron --all-targets -- -D warnings
```

### 第二阶段 B：GPUI 绘制

**做了**：

- **`verve-layout`**：`DisplayList::placed(offset_of)` 是唯一跟着“帧 / 裁剪 / 不透明度栈”走的地方（`resolved_hitboxes` 建在它上面，行为不变），同一趟还建出帧表（每个 `PushScroll` 的节点和它外层的帧），
  每个 `Placed` 记着自己最内层的帧；`Placed::hitbox()` / `Placed::scroller(offset_of)` 把一项解析到窗口坐标。新增 `resolved_scrollers` 和 `wheel(x, y, delta, offset_of)`。
  **滚轮的目标是命中元素的滚动链，不是几何上第一个包含指针的 `Scroller`**：取指针处最上面的（已裁剪的）命中盒，链是“命中的节点自己（若它是滚动容器），再从最内层向外的各帧，最后是页面”
  （`DisplayList.root`：页面自己的滚动容器，页面不滚动时为 `None`），第一个能往这个方向动的容器拿到这一格，返回它和夹取后的新偏移。因此 `position: relative; overflow: auto` 嵌套时内层先滚
  （两者都落在层叠上下文的中间层，列表里外层的 `Scroller` 在前），盖在列表上的不滚动的浮层把滚轮交给页面而不是它下面的列表，两个叠着的滚动容器里上面那个先滚。
  列表的 `revision` 现在是进程内唯一的（静态 `AtomicU64`）：导航会换新的 isolate 和引擎，以前每个引擎从 1 数起，新页面的第一张列表会和旧页面最后一张撞号，被窗口当成同一张丢掉。
  单元测试：滚轮链、不透明度叠乘；`tests/behavior.rs`：嵌套的定位滚动容器、不滚动的浮层、叠着的兄弟滚动容器、跨页面的列表号不重复。
- **渲染器上的滚轮**：`ToRendererInput::Wheel` 先派发 `wheel` 事件（`dy` 为 DOM 的 `deltaY`：正为向下），没被阻止才用 `DisplayList::wheel` 找容器，经 `apply_scroll(By)` 滚、`set_scroll_offsets` 通知页面。
  `webContents.sendInputEvent({type: 'mouseWheel'})` 的 `deltaX`/`deltaY` 是 Blink `WebMouseWheelEvent` 的符号（正为滚轮朝远离人的方向转，页面向上/向左滚），传给页面前取反。
  **在真 Electron 39.8.10（Windows，DPR 1）观察到**：`deltaY: 70` 让页面的 `wheel` 事件 `deltaY` = -70，顶端不动；`deltaY: -20` 得 `deltaY` = 20，`scrollY` = 20；`deltaX: 50` 得 `deltaX` = -50。
  （另外，窗口刚建好时的第一个 `sendInputEvent` 被丢掉，没有 `wheel` 事件。）`geometry.rs` 有一条测试（容器 → 页面链、反向、`preventDefault`），用 Electron 的符号。
- **verve-gpui**（新依赖 `verve-layout`、同一份 `tiny-skia 0.11` 与 `image 0.25`，`Cargo.lock` 只多了这三行边）：
  - `plan.rs`：把显示列表降成 `Plan { commands, hitboxes }`：窗口坐标里的形状序列（阴影 → 背景层 → 内阴影 → 边框，滚动条拇指，拇指由 `Scroller` 自己的 `Placed` 算）和解析后的命中盒，不透明度乘进每个颜色，裁剪取矩形。
    整个 `plan()` 只走一趟 `DisplayList::placed`，不再每盒分配 `Vec`。两个绘制后端共用它。
  - `dom_element.rs`：`DomElement`（`request_layout` 铺满、`prepaint` 为**每个**命中盒插一个 GPUI 命中盒，`cursor: auto` 也映射成箭头，这样 `cursor: auto` 的背景盖在 `cursor: pointer` 的按钮上时不会显示手；
    `paint` 逐形状用 `with_content_mask` + `paint_quad` / `paint_drop_shadows` / `paint_inset_shadows` / `paint_image`）。计划按（列表的 `Arc` 指针，预测的代数）缓存，一帧只算一次。
  - `wheel.rs`：`ScrollWheelEvent` → `DisplayList::wheel`（Shell 本地预测偏移，`window.refresh()`；指针位置先减去页面在窗口里的原点）并把 `ToRendererInput::Wheel` 发给渲染器。
    **每一张送到的列表**（包括 `preventDefault` 的或没动任何东西的滚轮发布的同一张）都清掉预测，以渲染器的偏移为准，否则视图会一直停在没人确认的预测上，而渲染器的矩形和 P32 的命中测试没有滚动。
  - `raster.rs` / `pictures.rs`：`Fill::Raster` 的栅格化（径向、圆锥、`repeating-*`；逐像素、预乘混色），**由我们自己的代码在 GPUI 线程上画**（不是契约原先写的渲染器线程上的 tiny-skia；契约已改），
    每边最多 2048 设备像素（更大的元素以低一点的缩放画，`paint_image` 拉伸）。GPUI 的 sprite 管线按 straight alpha 混合（SRC_ALPHA / INV_SRC_ALPHA，着色器里 `color.a *= opacity`），所以栅格先 `unpremultiply`
    （用 verve-dom 的 `canvas::unpremultiply`）再转 BGRA；`opacity < 1` 的盒子只缩放 alpha 通道。缓存按（渐变，缩放）；每张图保留一份淡出副本（最近一次的不透明度），一帧里没画到的图在帧末放弃，
    下一帧开头还给 GPUI 的图集，所以不透明度渐变不会每帧留下一份新图。
  - `dom_view.rs`：`DomView` 现在只放列表、预测偏移、计划缓存、图片缓存与背景色，替换了只画文字的占位；`show` 返回 `Shown`（`New`/`Redraw`/`Unchanged`），`window.rs` 的 `show_list` 对没变的列表不再算 `regions_of`、不 `notify`；`window.rs`/`shell.rs` 改读 `RendererView.display_list`，`app_region::regions_of` 把列表里带 `-webkit-app-region` 的命中盒（窗口坐标）变成 P34 的 `AppRegion { rect, drag }`，只在它变了时交给 `set_app_regions`（`Control::set_app_regions` 仍可用）。
  - `screenshot.rs`/`capture.rs`：`electron --screenshot <path> [app]`（也接受 `--screenshot=<path>`）等第一个窗口的页面 600 ms 不变后，用同一份 `plan` 经 tiny-skia 画成 PNG（页面最后一次绘制的 CSS 像素尺寸，缩放 1），写出后退出；
    画不出来 30 s 后退出码为 1，并且盖过 Hub 的退出码。PNG 用 `image 0.25` 的 `PngEncoder` 编码（`webContents.capturePage()` 之后要用软件绘制，所以留在默认构建里），`gpui` 不再开 tiny-skia 的 `png-format`（那会多带一份 png 0.17 的编码器）。
    阴影的模糊用 verve-dom 导出的 `canvas::blur_pixels`，渐变线的公式在 `verve_layout::gradient_line`（布局、栅格、软件绘制共用）。
    **`--screenshot` 是用第二个、软件的绘制器重画显示列表，不是窗口本身：GPUI 路径（`DomElement` / `pictures.rs`）没有像素测试。**
  - `VERVE_FRAME_STATS=1`（`frame_stats.rs`）：窗口连续出帧，每 240 帧在标准错误打印一行 `frames: … built p50 … p95 … , apart p50 … p95 …`（一帧的构建耗时与帧间隔；计时从 `prepaint` 开始，到 `paint` 结束，命中盒的遍历算在内）。
- **夹具**：`tests/electron/apps/boxes/`（400×300、无文字：实色、边框、圆角、线性渐变、不透明度、`z-index`、阴影、flex、grid、`overflow: hidden`、径向渐变）和 `expected-chromium.png`
  （真 Electron 39.8.10，Windows，`capturePage()`，`--force-device-scale-factor=1`，隐藏窗口 `frame: false` 400×300；`VERVE_BOXES_PNG=<path> electron tests/electron/apps/boxes` 重采）。
  `tests/electron/apps/boxes-2000/`：2000 个绝对定位盒，800×600。
- **测试**：`crates/verve-gpui/tests/boxes.rs`：`the_boxes_page_is_where_and_what_color_chromium_drew_it`（不开窗：布局 + 软件绘制，探针点 ±8，整图差异超过 48 的像素 < 3%，与验收一致；
  探针包括 `z-index` 重叠处（标记的顺序与叠放顺序不同：`high`、`behind`、`low`，z-index 2、0、1）、线性和径向渐变）；两条 `#[ignore = "opens real windows"]`：
  `the_picture_the_electron_binary_writes_of_the_boxes_page_is_close_to_chromiums`（真窗口 + `--screenshot`，同样的比较；比较的是软件绘制器的图，不是窗口的像素）和
  `two_thousand_boxes_are_replayed_within_a_frame`（读 `VERVE_FRAME_STATS` 的 p95 < 4 ms，应在 release 下跑；`taskkill` 只在 Windows，别处用 `kill -9`）。
  `expected-chromium.png` 用真 Electron 39.8.10 重采（标记换了顺序，视觉不变；边缘的抗锯齿与旧图差最多 36）。`plan.rs`、`raster.rs`（含 2048 的上限）、`pictures.rs`（straight alpha、淡出副本、放弃没画的图）、
  `dom_view.rs`（列表号、预测被每张列表清掉、计划缓存）、`frame_stats.rs`、`screenshot.rs`、`app_region.rs` 有单元测试。

**已知偏差**（v1）：

- 裁剪是矩形：`overflow: hidden` 的圆角不会裁圆内容。不透明度逐形状相乘（同一组里重叠的后代各自混合），GPUI fork 的 `with_element_opacity` 补丁帮不了分组，见第一阶段。
- 多色边框画成四条矩形（没有斜接）；`dotted` 画成 `dashed`；`double/groove/ridge/inset/outset` 画成实线。软件绘制（截图）里 `dashed` 也是实线、内阴影不画。
- 命中盒每个都插入 GPUI（`auto` 映射成箭头）；`cursor: none` 和 `wait`、`help` 等没有对应图案的显示箭头。拖拽区域只在新列表到来时重算，用列表自己的偏移，不含 Shell 的滚动预测。
- 滚动条是覆盖式细拇指（6 px，圆角，半透明灰），不接受点击或拖拽；经典滚动条的 15 px 占位同第一阶段。
- 滚轮：一格 = 3 行 = 100 px（Windows 的 Chromium；macOS 的 Chromium 是一行 40 px，代码里按平台取，别的平台暂用 40，只有 Windows 观察过）；只做 `Lines` 与触摸板的 `Pixels`。预测期间渲染器的新列表一到就覆盖预测（连续快速滚动时可能回跳一下）。
  `ToRendererInput::Wheel.dy` 为正向下；`sendInputEvent` 的符号见上（已观察）。
- 渲染没有观察到 Chromium 之外的行为；探针点的颜色来自上面的 Chromium 图。截图与真窗口的比较没有在本机跑（见下面的命令）。
- **绘制规则里没做到的**（也列在 `plan.rs` 的模块文档里）：圆角盒的多色边框画成方形条（规则是用上边颜色画一个 quad）；多于两个色标的渐变（沿轴的条带）丢掉圆角；
  外阴影整个画出，没有从盒子里挖掉，所以半透明的盒子会透出阴影；斜向的两色渐变与 Chromium 有差别，因为 GPUI 的线性渐变着色器按长边归一化（可以考虑改成栅格化）；
  `dashed` 在软件绘制里是实线、内阴影不画。
- **给 P32**：滚轮的行长度按平台取（见上）；GPUI 断言 `set_cursor_style` 在 paint 里调用，不是 P32 文档说的 prepaint；Chromium 里 window/document/html/body 上的 `wheel` 监听器默认是 passive
  （自 73 起），`src/dom_events.rs` 的 `listener_options` 还没这样做（内核的事，这里没改，记给内核和 P32）；命中盒现在每个都进了 GPUI 的帧。

**给其他包**：P31 的 `Glyphs`/`Decoration`、P35 的 `Image`、P32 的 `Control` 在 `plan.rs` 的 `plan()` 里都是 `_ => {}`：在那里加形状，两个后端各加一个分支；P33 的块缓存可直接按 `DisplayList::placed` 的顺序切。P34：`app_region::regions_of` 已接好。

**要跑的测试**（一次一条，窗口测试在有桌面的机器上）：

```
cargo test --locked -p verve-layout
cargo test --locked -p verve-electron --test geometry
cargo test --locked -p verve-electron
cargo test --locked -p verve-gpui --features jit --lib
cargo test --locked -p verve-gpui --features jit --test boxes
cargo test --locked -p verve-gpui --features jit --test boxes -- --ignored --test-threads=1
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --release --features jit -p verve-gpui --bin electron      # 量体积；2000 盒的帧时间用 release 跑上面的 ignored 测试
```

