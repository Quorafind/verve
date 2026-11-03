# P30 DomView 盒模型绘制

**依赖**：P02 P34  **规模**：L  **修改**：新建 `crates/verve-layout`；`crates/verve-gpui/src/dom_view.rs`、`dom_element.rs`；`crates/verve-dom/src/lib.rs`（加布局提供者钩子）；`crates/verve-electron/src/renderer_guest.rs`（接入布局引擎）；`crates/verve-electron/src/msg.rs`（`RendererView` 追加字段，见下）

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
| 一个子树的整体 `opacity` | `Window::with_element_opacity` 是 `pub(crate)` | 需要改 fork |
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
  `msg.rs`，说明这一调整**（见 conventions）。

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
- 仍需要的 fork 补丁：`Window::with_element_opacity` 改 `pub`（不透明度分组）、`finish_frame` 改 `pub`。写进 fork 的一个 commit。
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

pub struct DisplayList { pub revision: u64, pub items: Vec<PaintItem>, pub chunks: Vec<Chunk> /* P33 */ }

pub enum PaintItem {
    Box { node: NodeId, bounds: Rect, radii: [f32; 4], background: Vec<Fill>, border: Border, shadows: Vec<Shadow> },
    Glyphs { node: NodeId, origin: [f32; 2], font: FontKey, size: f32, color: [f32; 4], glyphs: Arc<[PlacedGlyph]> },   // P31
    Decoration { kind: DecorationKind, bounds: Rect, color: [f32; 4], wavy: bool },                                    // P31
    Image { node: NodeId, bounds: Rect, clip: Rect, image: ImageKey, fit: ObjectFit },                                 // P35
    Control { node: NodeId, bounds: Rect, kind: ControlKind },                                                         // P32
    PushClip { bounds: Rect, radii: [f32; 4] }, PopClip,
    PushOpacity(f32), PopOpacity,
    PushTranslate([f32; 2]), PopTranslate,
    Hitbox { node: NodeId, bounds: Rect, cursor: Cursor, app_region: Option<AppRegion> },                              // P32/P34
    Scroller { node: NodeId, viewport: Rect, content: [f32; 2], offset: [f32; 2] },
}
pub enum Fill { Color([f32; 4]), Linear { angle: f32, stops: Vec<([f32; 4], f32)> }, Raster(ImageKey) /* 复杂渐变栅格化 */ }
pub struct Border { pub widths: [f32; 4], pub colors: [[f32; 4]; 4], pub styles: [BorderLine; 4] }
```

### verve-dom：布局提供者（同步几何）

```rust
impl Document {
    /// 脚本读几何（getBoundingClientRect、offset*/client*/scroll*、elementFromPoint、getComputedStyle 的
    /// width/height 用到的值）而当前布局已过期时调用：先 update() 出快照，再交给 provider 算，
    /// 用返回的 rects 替换 state.rects（等同 set_layout，但不触发观察者回调重入）。
    pub fn set_layout_provider(&self, provider: Option<Box<dyn FnMut(&Update) -> Vec<(NodeId, Rect)>>>);
}
```

"过期"的判定：自上次布局以来 `Changes` 非空或视口变化。没有 provider 时行为不变（现有测试必须仍过）。

### `msg.rs` 追加

```rust
pub struct RendererView { /* P02 原有字段 */ pub display_list: Option<Arc<verve_layout::DisplayList>> }
```

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
| `position: sticky` | 布局时按 `relative` 处理；绘制时按最近滚动容器的偏移和 `top/bottom/left/right` 阈值夹取出平移量（`PushTranslate`） |
| 尺寸、`min/max-*`、`box-sizing`、百分比、`auto`、`aspect-ratio` | taffy 原生支持；百分比基于包含块；`calc()` 由 P22 在计算样式里化简成 `px`/`%` 组合，taffy 0.13 不支持的混合 calc 退回 `auto` 并记录 |
| `margin`（含 `auto`）、`padding`、`border-*-width` | taffy 原生 |
| `gap`、`flex-*`、`align-*`、`justify-*`、`order` | taffy 原生（`order` 在建树时按值稳定排序子节点） |
| `grid-template-*`、`grid-area`、`grid-auto-*`、命名线 | taffy 0.13 原生支持轨道列表、`repeat()`、`minmax()`、`fr`；命名区域在建树时解析成行列号 |
| `overflow: hidden/clip/scroll/auto` | taffy `Overflow`；绘制时 `PushClip`；`scroll/auto` 且内容超出 → `Scroller` 项 |

## 层叠与绘制顺序

按 CSS 2.1 附录 E 的简化版生成 `DisplayList`：每个层叠上下文（根、`position` 非 static 且 `z-index` 非 auto、
`opacity<1`、`transform` 非 none、`position: fixed/sticky`）内依次：
自身背景边框 → 负 `z-index` 子上下文 → 块级流内子孙 → 浮动 → 行内内容（P31） → `z-index: auto/0` 的定位子孙（树序）
→ 正 `z-index` 子上下文（按值，同值按树序）。`hit_test` 按相反顺序。

## 盒子绘制规则（`DomElement::paint`）

- 背景色：`PaintQuad` 的 `background`。`linear-gradient` 两个色标 → GPUI `linear_gradient`；角度为 0/90/180/270
  的多色标 → 切成多个相邻的两色标 quad；其他（多色标斜角、`radial-gradient`、`conic-gradient`、`repeating-*`）→ `Fill::Raster`：
  在渲染器线程用 P25 的 tiny-skia 栅格化成图片，按 (渐变文本, 尺寸) 缓存。
- 边框：四边颜色和样式相同 → 一个 `PaintQuad`（`border_widths`、`border_color`、`BorderStyle::Solid/Dashed`）。
  不同 → 每边用 `paint_path` 画梯形（含圆角时退回一个 quad 用上边颜色，记录为已知偏差）。`dotted` 画成 `Dashed`，
  `double/groove/ridge/inset/outset` 画成 `Solid`（`inset/outset` 按亮暗调整颜色）。
- 圆角：`corner_radii`；椭圆半径（`10px / 20px`）取较小值。
- `box-shadow`：GPUI `BoxShadow`（含 `inset`、多层，顺序与 CSS 一致：第一层在最上）。
- `opacity`：`PushOpacity`，GPUI 侧用 fork 里改为 `pub` 的 `with_element_opacity`。
- `transform`：v1 只支持 `translate*()` 和 `matrix()` 中的平移分量（`PushTranslate`，不影响布局，影响 `rects` 与命中）；
  `scale/rotate/skew` 对盒子不生效（P35 对 `<img>`/`<svg>` 生效），并在 `console.warn` 只提示一次。动画由 P22 产出每帧计算值。
- `visibility: hidden`：不出 `Box`/`Glyphs`，但保留布局；`visible` 的后代仍要画。
- `outline`：画在盒子外侧的第二个 quad（不占布局）；`outline-offset` 支持。
- `cursor`、`-webkit-app-region`、`pointer-events: none`：体现在 `Hitbox` 项（P32/P34 消费）。

## 滚动

- 滚动偏移的唯一真相在渲染器：`Snapshot.scroll_offsets`。脚本的 `Request::Scroll` 由 `RendererGuest` 拦截，交给
  `LayoutEngine::apply_scroll` 夹取，再 `Document::set_scroll_offsets`（触发 `scroll` 事件），**不再发给 Shell**。
- 滚轮：Shell 在 `Scroller` 项上收到滚轮时，先本地平移该容器的绘制（立即响应），同时发
  `ToRendererInput::Wheel`；渲染器派发 `wheel` 事件，未被 `preventDefault` 就按同样的量 `apply_scroll` 并发布。
  Shell 收到新显示列表后以渲染器的偏移为准（本地预测值丢弃）。
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

（实现者完成后填写）
