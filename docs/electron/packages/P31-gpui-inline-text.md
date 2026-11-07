# P31 行内排版与文本

**依赖**：P30  **规模**：L  **修改**：`crates/verve-layout/src/inline/`（新）、`crates/verve-layout/src/fonts.rs`（新）；`crates/verve-gpui/src/text_measurer.rs`（新）、`dom_element.rs`（画字、装饰、选区）；`crates/verve-electron/src/renderer_guest.rs`（@font-face 注册通道）

## 目标

在 P30 的布局引擎里实现 CSS 的**行内格式化上下文**：一个块里的文字、`<span>/<a>/<b>/<code>` 等行内元素、
`inline-block`/`<img>` 等原子行内盒，按浏览器的规则断行、对齐、画出来；`getClientRects()` 给出行内元素每一行的盒子；
`Selection` 能画、能用鼠标拖出来；中文、日文、emoji、混排都正确。

## 关键决策

- **行布局自己做，塑形交给 GPUI 的平台文本系统**。GPUI 的 `StyledText`/`TextLayout` 只能处理"一段文字 + 若干 `TextRun`"，
  放不进原子行内盒，也不支持 `letter-spacing`、`text-indent`、`vertical-align`、`text-align: justify`、
  逐元素 `line-height`；`WhiteSpace` 只有 `Normal/Nowrap`，`TextAlign` 只有 `Left/Center/Right`。所以不用 `StyledText`。
- 做法：P30 的 `TextMeasurer::shape` 对"同一字体的一段文字"塑形（共享的 `Arc<TextSystem>` 上已公开的 `WindowTextSystem::layout_line`，见 P30 spike），
  拿到字形、x 偏移、字节下标；断行机会用 `unicode-linebreak`（UAX #14，体积小）计算；行内盒、原子盒按贪心算法填行。
- 画字用 `Window::paint_glyph(origin, font_id, glyph_id, font_size, color)` 直接画已塑形字形，GPUI 线程不重新塑形。
  emoji（彩色字形）用 `paint_emoji`；`TextMeasurer` 在 `PlacedGlyph` 上标记 `is_emoji`。
- 下划线、删除线用 `paint_underline` / `paint_strikethrough`（`UnderlineStyle.wavy` 对应 `text-decoration-style: wavy`）。

## 背景（先读）

- P30 文档（`LayoutEngine`、`TextMeasurer`、`DisplayList`、`PaintItem::Glyphs/Decoration`）。
- GPUI：`crates/gpui/src/text_system.rs`（`TextSystem::resolve_font`、`ascent/descent/x_height/cap_height`、`add_fonts`、
  `all_font_names`、`FontFallbacks`、`Font { family, features, fallbacks, weight, style }`）、`text_system/line_layout.rs`
  （`LineLayout`、`ShapedRun`、`ShapedGlyph { id, position, index, is_emoji }`）、`window.rs` 的 `paint_glyph/paint_emoji/paint_underline/paint_strikethrough`。
- su-editor 的中文排版经验（只读）：`F:\dev\sandbox-v2\su-editor` 最近的提交 "Chinese punctuation squeezed side by side, lines folded…"
  —— 标点挤压、行尾悬挂的处理可借鉴。
- verve-dom：`Snapshot.dom` 的文本节点、`Draft`（输入框文字由 P32 画，不走这里）、`Selection`/`Range` 的绑定在 `src/bind/`。

## 接口契约

```rust
// verve-layout
pub struct PlacedGlyph { pub id: u32, pub x: f32, pub byte: u32, pub is_emoji: bool, pub font: FontKey /* 实际画它的字体：用了回退时与 ShapedRun.font 不同 */ }
pub struct ShapedRun { pub font: FontKey, pub size: f32, pub glyphs: Vec<PlacedGlyph>, pub width: f32 }

/// 行内格式化上下文的结果：P30 用它确定块高度，DisplayList 用它出 Glyphs/Decoration 项。位置都是 CSS 像素，原点在块内容盒的左上角。
pub struct InlineLayout { pub lines: Vec<LineBox>, pub height: f32, pub first_baseline: f32, pub last_baseline: f32,
                          pub width: f32 /* 最宽一行伸到哪里（悬挂的文字也算） */ }
pub struct LineBox {
    pub rect: Rect, pub baseline: f32, pub fragments: Vec<Fragment>,
    pub spans: Vec<SpanBox>,                                // 本行上有内容的每个行内元素的 border box，外层在前
}
pub enum Fragment {
    Text(TextFragment),
    Atomic { node: NodeId, rect: Rect },                    // inline-block / img / inline-flex / 表单控件的 border box
}
pub struct TextFragment {
    pub node: NodeId /* 文本节点；生成的文字（::before、省略号）是所属元素 */, pub range: Range<usize> /* 文本节点里的 UTF-8 字节；生成的文字为空 */,
    pub run: Arc<ShapedRun> /* 字形位置从片段左缘算，字间距、词间距、两端对齐已计入 */, pub x: f32, pub width: f32, pub baseline: f32,
    pub paint: Arc<TextPaint>,
    pub stops: Vec<Stop>,                                   // 光标能停的地方：每个字符一个，再加末尾；Stop { byte: 文本节点里的字节, x: 从片段左缘算 }
    pub generated: bool, pub select_none: bool,
}
pub struct SpanBox { pub node: NodeId, pub rect: Rect, pub starts: bool /* 本行有元素的开头：画左边框与 padding */, pub ends: bool }
// 行内元素的开头与结尾不再是片段：它们在 LineBox.spans 里（getClientRects 与画背景边框都读它）。

impl LayoutEngine {
    pub fn client_rects(&self, node: NodeId) -> Vec<Rect>;                     // 行内元素每行一个盒子
    /// 一个块的行（块里没有行内内容则为 None）。
    pub fn inline_layout(&self, node: NodeId) -> Option<Arc<InlineLayout>>;
    /// 每个行内元素在每一行上的盒子（按元素）：交给 verve-dom 的 `Layout.fragments`。
    pub fn fragments(&self) -> impl Iterator<Item = (NodeId, Vec<Rect>)> + '_;
    /// 记下页面用 `@font-face` 注册的一个字体面；是新的就返回 true，下一次 `update` 全部重排（回退字体排过的文字重新整形）。
    pub fn register_font(&mut self, family: &str, weight: f32, italic: bool) -> bool;
    /// 文本命中：视口坐标 → (文本节点, UTF-16 偏移)（DOM 的偏移语义；片段里存 UTF-8，查询时换算）。selection 拖选、caretPositionFromPoint、document.caretRangeFromPoint 用
    pub fn text_position_at(&self, x: f32, y: f32) -> Option<(NodeId, usize)>;
    /// Range 的盒子（Range.getClientRects / getBoundingClientRect）；端点是 DOM 的 (节点, 偏移)：文本节点用 UTF-16 偏移，元素用子节点下标。
    /// 折叠的 range 返回一个宽度为 0 的盒子（编辑器的光标位置用它）。
    pub fn range_rects(&self, start: (NodeId, usize), end: (NodeId, usize)) -> Vec<Rect>;
}

// 字体数据（verve-layout 公开导出）：
pub enum Platform { Windows, Mac, Linux }            // Platform::current()：通用族名按它映射
pub struct FontRegistry { /* register / contains / best / generation：@font-face 注册表，按族名，best() 按字重与斜体匹配 */ }

// verve-dom（已实现）：
pub struct Layout { /* rects, scroll_offsets, scroll_extents, */ pub fragments: Vec<(NodeId, Vec<Rect>)> /* 行内元素每行的盒子：Element.getClientRects 的答案 */ }
pub type Boundary = (NodeId, usize);                                  // Range 的端点：节点与偏移
pub type RangeProvider = Box<dyn Fn(Boundary, Boundary) -> Vec<Rect>>;
impl Document {
    /// 让 `Range.getClientRects` / `getBoundingClientRect` 向 provider 要两个端点之间文字的盒子（先把布局刷新到最新）；没有 provider 就答起点所在元素的盒子。
    pub fn set_range_provider(&self, provider: Option<RangeProvider>);
    // 尚未提供（第二阶段）：text_position_at 的 provider 钩子。
}
```

`@font-face`（第二阶段改了：**不走 Hub 与 Shell 的来回**）：`TextSystem::add_fonts(&self)` 在共享的 `Arc<TextSystem>` 上是 `pub` 且线程安全，渲染器线程直接注册，
所以不加 `FromRenderer::Font` / `ToRendererInput::FontsChanged`（第一阶段写进 P02 的那一行作废）。通道是：

```rust
// verve-layout：测量器多三个方法。
trait TextMeasurer { /* … */
    /// 注册 `FontFace` 的字节，之后以 CSS 里的 `family` 别名使用；成功返回 true。默认不收（false）；无头测量器没有字体可拒，什么字节都收（true），
    /// 所以无头页的字体都算加载成功。GPUI 的 `add_fonts` 按字体自己 name 表里的族名登记，所以测量器读字节里的 name 表（nameID 16，没有就 1），
    /// 记一张别名表（CSS 族名 → 字体面），`resolve` 先查它。别名表是**页面自己的**，另一个页面看不到这个别名。
    fn add_face(&self, family: &str, weight: f32, italic: bool, bytes: Arc<[u8]>) -> bool { false }
    /// 页面用的测量器：字体面是页面自己的，文字系统与缓存各页共用。`Hub` 把 shell 的测量器经它交给每个页面。
    fn for_page(self: Arc<Self>) -> Arc<dyn TextMeasurer>;
    /// 一次布局结束（`LayoutEngine::update` 整体重排后、`replace` 之后）：放掉塑形留下的缓存。GPUI 公开 `WindowTextSystem::finish_frame` 之后在这里调它。
    fn finish_layout(&self) {}
}
// verve-dom：字体字节到手（`new FontFace(f, buffer)` 立即；`new FontFace(f, 'url(..)')` 在 `load()` 取回后）时，先问宿主的字体钩子，
// 钩子答 true 才把 face 置为 loaded，答 false 置为 error：
pub struct FontData { pub family: String, pub weight: f32, pub italic: bool, pub bytes: Arc<[u8]> }
pub type FontProvider = Box<dyn Fn(FontData) -> bool>;
impl Document {
    pub fn set_font_provider(&self, provider: Option<FontProvider>);
    /// 宿主取回了 `url()` 字体的字节（`load()` 的加载），交给 face 的字体钩子并按答案 settle；钩子的报错以脚本错误上报。
    pub fn settle_font_bytes(&self, runtime: &mut Runtime, face: FontFaceId, bytes: Vec<u8>);
}
```

`PageLayout` 装的钩子：`measurer.add_face(..)`，成功则 `LayoutEngine::register_font(..)`（下一次 `update` 整体重排）并让文档的布局失效；
`loaded`/`document.fonts.ready` 在钩子返回之后才 resolve，所以脚本在 `ready` 之后量到的宽度已经是新字体的。只支持 ttf/otf；woff/woff2 的字节注册失败，face 置为 `error`。
样式表里的 `@font-face` 规则 verve-dom 还没有读（P22 的），只有脚本的 `FontFace` 对象走这条通道。

## 规则

| CSS | 做法 |
|---|---|
| 空白处理 `white-space: normal/nowrap/pre/pre-wrap/pre-line/break-spaces` | 建行内内容时按规范折叠空白、保留换行；`nowrap/pre` 不在软换行机会断行 |
| `word-break: normal/break-all/keep-all`、`overflow-wrap: anywhere/break-word`、`line-break` | 对 UAX #14 的机会做增删；`break-word` 只在一行放不下单词时允许字内断 |
| CJK | 汉字之间默认可断；避头尾（`。，、）」` 不在行首，`（「` 不在行尾）按 UAX #14 已含；标点挤压 v1 不做 |
| `text-align: left/right/center/justify/start/end` | 行内整体偏移；`justify` 把剩余宽度分到断行机会（拉丁空格；CJK 字间），最后一行不两端对齐 |
| `text-indent` | 首行起点偏移（负值支持） |
| `line-height: normal/<number>/<length>/%` | `normal` = 字体 ascent+descent+lineGap（`TextMeasurer::metrics`）；行高 = 行内所有盒子 half-leading 后的并集 |
| `vertical-align` | v1：`baseline/middle/top/bottom/text-top/text-bottom/sub/super/<length>`；`sub/super` 同时缩小字号由 UA 样式表提供 |
| `letter-spacing`、`word-spacing` | 塑形后逐字形 / 逐空格加偏移 |
| `text-transform` | 建内容时转换（`uppercase/lowercase/capitalize`），保留原文到偏移的映射以便选区正确 |
| `text-overflow: ellipsis` | 仅 `overflow` 非 visible 且 `white-space: nowrap` 的块：最后一个放得下的位置截断，补 `…` 字形 |
| `-webkit-line-clamp` + `display: -webkit-box` | 只排 n 行，最后一行省略号 |
| `text-decoration-line/color/style/thickness`、`text-underline-offset` | `Decoration` 项；行内元素的装饰延伸到其所有后代文字 |
| `text-shadow` | v1：画一份偏移、用阴影色的字形（不模糊）；模糊忽略 |
| `direction: rtl`、`unicode-bidi` | v1 只支持整块 RTL 的对齐和行内顺序反转；混排双向算法（UBA）不做，记录 |
| `writing-mode` 竖排 | 不做 |
| 原子行内盒 | 宽高由 P30 布局其子树得出，作为不可断的一项放进行；基线 = 其最后一行文字基线，没有文字时为下边距边 |
| 行内元素的背景、边框、内边距 | 每行一段，首段画左边框/内边距，末段画右边（`box-decoration-break: slice`） |

### 字体选择

1. `font-family` 列表逐个：通用族映射（Windows：`sans-serif`→`Segoe UI`，`serif`→`Times New Roman`，`monospace`→`Consolas`，
   `system-ui/-apple-system/BlinkMacSystemFont`→`Segoe UI`；macOS：`-apple-system/system-ui`→`.SystemUIFont`，`monospace`→`Menlo`；
   Linux：交给 fontconfig 名字 `sans-serif` 等）。
2. 已通过 `@font-face` 注册的名字优先于系统字体。
3. 回退链（GPUI `FontFallbacks`）：Windows `Microsoft YaHei UI`、`Yu Gothic UI`、`Malgun Gothic`、`Segoe UI Emoji`、`Segoe UI Symbol`；
   macOS `PingFang SC`、`Hiragino Sans`、`Apple SD Gothic Neo`、`Apple Color Emoji`；Linux `Noto Sans CJK SC`、`Noto Color Emoji`。
   按 `lang` 属性调整中日韩优先顺序（`zh-TW` → `Microsoft JhengHei UI` / `PingFang TC`）。
4. `font-weight` 数值直接给 GPUI `FontWeight(f32)`；`font-style: italic/oblique` → `FontStyle::Italic/Oblique`；
   `font-feature-settings`、`font-variant-numeric: tabular-nums` → GPUI `FontFeatures`。
5. 字形缺失时 GPUI 的 `MissingGlyph` 报告：按回退链换字体重塑那一段。

### 选区与光标

- 文档选区（非输入框）：由 `range_rects` 得到盒子，画在文字下面（`::selection` 的颜色由 P22 提供，默认系统高亮色）。
- 鼠标拖选：P32 把 `pointer_down/move/up` 交给 verve-dom，verve-dom 用 `text_position_at` 设置 `Selection`（P24 负责 DOM 侧），本包只提供命中与绘制。
- `user-select: none` 的节点不参与命中。
- 链接（`<a href>`）的 `cursor: pointer` 由 UA 样式表给出，进 `Hitbox` 项。

## 第一阶段的决定（协调者，2026-11）

- **`line-height: normal`**：布局取 `ascent + descent + line_gap`（`TextMeasurer::metrics` 给的值），不自己取整；GPUI 给的是 Windows 的 ascent（Consolas 14.719，hhea 是 11.883），Chromium 取哪张表要在真实 Electron 39.8.10 里观察，
  测量器据此取整，结果记在第二阶段的状态里。无头测量器的 `normal` 就是 1 em。
- **断行库**：`unicode-linebreak`（0.1.5，已在依赖图里：Linux/macOS 上 cosmic-text 用它；Windows 的 `electron.exe` 里它是新代码，约 21 KB），不引入第二套 UAX #14。
- **GPUI 不打补丁**：`WindowTextSystem::finish_frame` 是 `pub(crate)`，在公开之前，渲染器线程每排若干次版（或缓存超界）就重建它的 `WindowTextSystem`。
- **输入桥是 P32 的**：键盘、光标闪烁、输入法不在本包；本包给出选区模型、命中（点 → 文本位置）与绘制。

## 第二阶段的契约（画字）

```rust
// verve-layout
pub struct SpanPaint { pub background: Rgba, pub border: Border, pub radii: [f32; 4] }   // 行内元素的背景、边框、圆角（背景只有纯色）
pub struct SpanBox { /* … */ pub paint: Option<Arc<SpanPaint>> }                         // None：既没有背景也没有边框
impl LayoutEngine {
    /// 文档选区（DOM 端点：文本节点用 UTF-16 偏移，元素用子节点下标）；变了就重建显示列表，返回是否变了。
    pub fn set_selection(&mut self, selection: Option<((NodeId, usize), (NodeId, usize))>) -> bool;
}
```

显示列表里每个有 `InlineLayout` 的盒，在它所在层叠上下文的“浮动之后、定位盒之前”按下面的顺序出项，所有项都在盒所在的裁剪、滚动、不透明度之内：
1. 每个 `SpanBox` 一个 `PaintItem::Box`（背景与边框；`starts` 为假时去掉左边框，`ends` 为假时去掉右边框，该侧也不圆角：`box-decoration-break: slice`）；
2. 选区：每行一个 `PaintItem::Box`，高度是**行盒**的高度（Chromium 填满行盒，不是 `range_rects` 的内容区高度），颜色默认 `#3390FF` 加 45% 不透明度（`::selection` 是 P22 的）；
3. 每个文本片段：先是 `text-shadow` 的 `Glyphs`（偏移、阴影色，不模糊；emoji 没有单色的形状，阴影里不出），再是**下划线与上划线**的 `Decoration`，再是 `Glyphs`（`origin` 是片段左缘与基线，`PlacedGlyph.x` 从 `origin[0]` 算起），
   最后是**删除线**的 `Decoration`（CSS 2.1 附录 E 7.2.1.4.1 与 Chromium 的 `TextFragmentPainter`：下划线、上划线在字下，删除线在字上；`bounds` 是线的矩形，`wavy` 对应 `text-decoration-style: wavy`；`double/dotted/dashed` 画成实线）；
4. 原子行内盒照旧是它们自己的盒子。

`Glyphs` 与 `Decoration` 的字段不变。`Shape::Glyphs` / `Shape::Decoration`（verve-gpui `plan.rs`）是它们的窗口坐标版本，不透明度乘进颜色。
`Glyphs` 的回放按 `PlacedGlyph.font` 切段：`is_emoji` 的用 `paint_emoji`，其余用 `paint_glyph`。
verve-dom：`Document::selection_boundaries(&self, runtime) -> Option<(Boundary, Boundary)>`（窗口选区第一个 range 的两端）；
`document.caretPositionFromPoint(x, y)` 与 `caretRangeFromPoint(x, y)` 建在 `text_position_at` 之上。
渲染器线程的测量器：`Hub::set_text_measurer(Arc<dyn TextMeasurer>)` 由 Shell 启动时装上（`GpuiMeasurer`，共享 `cx.text_system()`）；没装就是无头测量器。

## 步骤

1. `text_measurer.rs`（verve-gpui）：用 `Arc<TextSystem>` 实现 `TextMeasurer`（resolve 带缓存；shape 按 (文本, 字体, 字号) 缓存）。
2. `fonts.rs`：通用族映射、回退链、`@font-face` 注册表。
3. 行内内容收集：从块的子孙收集文本、行内边界、原子盒，处理空白与 `text-transform`。
4. 断行与行盒构建、对齐、`vertical-align`、行高。
5. 与 P30 的 taffy 测量函数对接（块的内容高度、`min-content/max-content` 宽度：最长不可断段 / 不换行全宽）。
6. `DisplayList` 出 `Glyphs`/`Decoration`/行内背景边框；GPUI 侧回放。
7. 命中与 `client_rects`/`range_rects`，接到 verve-dom 的提供者钩子。
8. `@font-face` 通道（契约追加，第一个 commit 先改文档）。

## 验收

- `cargo test -p verve-layout --test inline`（无头测量器）：空白折叠各模式、`break-all`/`keep-all`、`text-indent`、`justify`、
  `line-height` 数值与长度、`vertical-align: middle`、原子行内盒基线、行内元素跨两行时 `client_rects` 返回两个盒子、ellipsis。
- `cargo test -p verve-gpui --test text`（需要真实字体，Windows CI 上跑）：`<p>中文English混排😀</p>` 每个字形都有非零宽度、
  没有缺字（无 `MissingGlyph`）；`Microsoft YaHei UI` 被选为中文回退。
- 夹具 `tests/electron/apps/text/`：段落、标题、列表、代码块、链接、中英混排、RTL 一段；截图与真实 Electron 对比，
  每行基线位置差 ≤ 2px、行数一致（文字像素本身不比，字体渲染不同）。
- 脚本 `document.createRange()` 选中一个跨行 `<span>`，`range.getClientRects().length === 2`。
- `@font-face` 加载一个 woff2 以外的 ttf 字体（woff2 解码 v1 不做，见"不做"），`document.fonts.ready` 后测得宽度变化。

## 不做

完整双向算法、竖排、`ruby`、`hyphens: auto`、`text-shadow` 模糊、`font-display` 除 swap 外的值、woff/woff2 解码
（若需要，P20 加 `woff2` 解码后交来 ttf 字节；记录在 P41）、`initial-letter`、标点挤压与行尾悬挂。

## 状态

### 第一阶段：行内排版（`verve-layout`，无头测量器）

**做了**（步骤 2、3、4、5、7，以及步骤 8 的契约文档）：

- **契约先行**：第一个提交只改文档：`PlacedGlyph.font`（实际画这个字形的字体，用了回退时与请求的不同）、`FromRenderer::Font { family, weight, italic, bytes }` / `ToRendererInput::FontsChanged`（P02 的追加表）、`text_position_at`/`range_rects` 的偏移是 UTF-16、
  `line-height: normal` = `metrics` 的 `ascent + descent + line_gap`（布局不取整，取整是测量器的事）。`Decoration`、`Glyphs`、`Box` 不变（行内背景边框用 `Box`，选区也是 `Box`）。
- **`fonts.rs`**（只有数据与规则）：通用族→平台名（Windows/macOS/Linux）、回退链（按 `lang` 调整 CJK 顺序）、`@font-face` 注册表 `FontRegistry`（按族名，`best()` 按槽位/字重匹配，`generation()` 供布局判过期）；
  `LayoutEngine::register_font` 登记后下一次 `update` 整体重排。通道（`FromRenderer::Font` 等）是第二阶段。
- **`inline/`**（新）：
  - `space.rs`：`white-space` 六种（`normal/nowrap/pre/pre-wrap/pre-line/break-spaces`）的折叠与保留、跨文本节点的折叠状态、tab 展到制表位、CJK 之间的换行不留空格（韩文除外）、`text-transform`（含 `ß`→`SS`；`capitalize` 在同一块里跨文本节点接着算词首，空格、换行与原子盒之后重新算）；
    每个输出字符记着它来自文本节点的哪个字节，选区由此换回偏移。
  - `breaks.rs`：UAX #14 用 **`unicode-linebreak` 0.1.5**（`Cargo.lock` 里早已有它（`cosmic-text` 在 Linux/macOS 的 GPUI 文字栈里用它），所以没有第二份表、`Cargo.lock` 只多了 `verve-layout` → `unicode-linebreak` 这一条边；**但在 Windows 上 GPUI 走 DirectWrite，不带 `cosmic-text`，所以对 `electron.exe` 来说它是新代码（约 21 KB，协调者量体积时要计入）**；没有另选 `icu_segmenter`，那样要带数据且 Boa 那份是另一套特性）。
    `word-break: break-all/keep-all`、`overflow-wrap: break-word/anywhere`（旧的 `word-break: break-word` 当 `anywhere`）、`<wbr>`、`break-spaces` 的每个空格可断。
  - `style.rs`：从计算样式读出字体（族、字号、字重、斜体 → `measurer.resolve/metrics`）、`line-height`、`white-space`、`text-align`、`text-indent`、`letter/word-spacing`、`vertical-align`、`direction`/`dir`、`text-decoration`（简写与长写、线型、颜色、粗细）、`text-shadow`、
    `text-overflow`、`-webkit-line-clamp`。级联把继承的相对长度（`1.5em`）原样传给子元素，所以 `inherited_length` 找到写下它的祖先、用那个祖先的字号算。
  - `collect.rs`：把块的内容收成 `Item`（文本、行内元素的起止、原子行内盒、强制换行、`<wbr>`），文本塑形一次（整个文本节点），按字符边界算 `stops`（含字距、词距）；`::before`/`::after` 的文字（引号、`attr()`/计数器已由快照算好）作为生成文本。
  - `chunks.rs`：按可断处切成“块”（chunk）：一段不可断的内容，行起止与元素边界（左边界归后一块、右边界归前一块）都在其中。
  - `lines.rs`：贪心填行；放不下就换行，空行上还放不下且 `overflow-wrap` 允许时在字符处断开（不在组合符前断）；同一文本节点在一行里是一个片段；末尾空格悬挂不占宽（`pre`/`break-spaces` 除外）；最小内容宽度 = 最长不可断块（`anywhere` 时是最宽的一个字符）。
  - `finish.rs`：行尾去空格、`text-align`（`left/right/center/start/end/justify`，RTL 整块镜像）、`text-indent`（首行，可为负、可为百分比）、`justify`（拉丁文拉空格，没有空格时拉相邻 CJK 之间；最后一行和强制换行结束的行不拉）、
    `text-overflow: ellipsis`（仅 `overflow` 非 visible 的块）、`-webkit-line-clamp` + `display: -webkit-box`、每个行内元素每行一个 `SpanBox`（`getClientRects` 的来源）。
  - `vertical.rs`：每个行内盒的 line-height 盒（`half-leading`），`vertical-align`：`baseline/sub/super/text-top/text-bottom/middle/<length>/<percentage>/top/bottom`；`top`/`bottom` 在其余盒定出行高后贴边、不够高就撑高（`top` 向下撑、`bottom` 向上撑）。
  - `query.rs`：`span_rects`、`range_rects`（元素端点按子节点下标换成文本位置）、`position_at`（点 → 最近的文本节点与 UTF-16 偏移，只在块的边框盒内，`user-select: none` 的文字不参与）。
- **进树**（`flow.rs`、`atoms.rs`、`tree.rs`）：块的孩子只有行内内容时，块自己就是 taffy 的叶子（带 `Ifc` 做上下文，量度函数给内容的最小/最大宽度和给定宽度下的高度）；
  有块级孩子时，每段行内内容放进**匿名块**（`BoxNode.anonymous`，不画、不进 `by_node`/`rects`、不出 `Hitbox`），行内元素包着块时在块的两边各收一段（行内元素的边在两段里各出现一次）；
  flex/grid 容器里的文字每段是一个匿名项；只有可折叠空白的一段不建盒（所以块与块之间的缩进空白不影响任何旧布局）；浮动与绝对定位的盒不拆行内内容，排在这段内容之后（内容之前没有文字时排在前面）。
  行内元素没有盒；`display: inline-block/inline-flex/inline-grid` 与替换元素（`<img>` 等）是**原子行内盒**：不是叶子的 taffy 子节点，单独布局（见下），由行摆放。
- **原子行内盒的宽度与基线**：taffy 的量度函数不能回头调 taffy，所以原子盒在页面树之外单独布局：先按最小/最大内容宽度量一遍，再按“块的内容宽度”取 `min(max, max(min, 可用))`（收缩适配）定宽，再量高与基线
  （基线 = 盒内最后一行文字的基线，没有文字或 `overflow` 非 visible 时是下边缘）。一轮页面布局后读每个块实际的内容宽度，与原子盒量时用的不同就重量（最深的先量），至多六轮（`BoxTree::compute`）。
  这修好了 `inline-block` 宽度 `auto` 时被拉满的问题。
- **进 verve-dom**（任务书范围之外，改得很小）：`Layout.fragments`（行内元素每行的盒子，provider 一并交上来，`getClientRects` 用）、`Document::set_range_provider` 与 `bind/range_rects.rs`
  （`Range.getClientRects`/`getBoundingClientRect` 问 provider；没有 provider 时仍是起点元素的盒）；`platform.rs` 里这两个方法换成对新文件的调用，行数只减不增。verve-electron 的 `PageLayout` 装上两个 provider。
- **测试**：模块内的单元测试（空白、断点、块切分、`stops`、字体匹配、UTF-16 换算）与 `tests/inline.rs`（无头测量器，字宽 0.5em、ascent 0.8em）：空白各模式、`br`、`break-all`/`keep-all`/`break-word`/`anywhere`、`text-indent`、
  `text-align`（含 RTL）、`justify`、`line-height`（数字/长度/百分比）、`vertical-align`（长度/middle/top）、原子行内盒的基线、`inline-block` 收缩适配、浮动按文字收缩、`<img>` 撑高行、
  行内元素跨两行有两个盒子、行内元素的 padding/border、字距与 `text-transform`、`ellipsis`、`line-clamp`、匿名块、行内元素包着块、生成文本、`text_position_at`、`range_rects`，
  以及脚本里 `document.createRange()` 选中跨行的 `<span>` 得 `getClientRects().length === 2`。

**没做 / 已知偏差**：

- **全部测试都没有跑过**（规则：我只跑 `cargo check`）。数字是按无头测量器的规则手算的，算错的话是测试的错，请把失败发回来。
- **文字不绕浮动**：浮动在块级孩子之间时，匿名块按普通块排（taffy 的叶子无法知道浮动占了哪一段），文字会与浮动重叠。要做需要自己实现浮动的行盒避让（P33 或之后）。
- **`position: relative` 的行内元素**仍不是后代绝对定位盒的包含块，也不按 `top/left` 偏移；原子行内盒的 `position: relative` 偏移也没有应用（原子盒的位置全由行给出）。
- **列表标记**（`::marker`、`list-style`）没有；`::before`/`::after` 只当行内文字处理（`display` 不是 `inline` 的也当行内，`content: url()` 的图被忽略），没有自己的 `NodeId` 也没有盒子。
- 字体特性（`font-feature-settings`、`font-variant-numeric: tabular-nums`）没读：`resolve` 的签名没有地方放，第二阶段随 GPUI 测量器一起定。`text-justify`、`text-align-last`、`hyphens`、`line-break`、`initial-letter`、双向算法（只有整块 RTL 的镜像）、竖排都没做。
- `line-height: normal` 只用块与元素自己主字体的 `metrics`，回退字体（比如 CJK）更高时行高不随之增加（Chromium 会）；这要等真字体的第二阶段对照。
- **`vertical-align: sub/super`** 的位移是我按 Blink 的实现记的（上一级字号的 `1/5 + 1` 向下、`1/3 + 1` 向上），**没有在 Electron 39.8.10 里观察过**，第二阶段用真字体探针核对；`middle` 用上一级的 x 高度的一半。
- 文本装饰（`text-decoration`）的传播按 DOM 祖先估算（遇到原子行内盒、浮动、绝对定位就停），数据在 `TextPaint.decorations`，第一阶段不画。
- 显示列表第一阶段没有变化：还没有 `Glyphs`/`Decoration`/行内背景边框/选区项；行内元素没有 `Hitbox`，点击行内元素（`<a>` 的文字）仍命中它所在的块——这两件事在第二阶段（`paint.rs` 的“P31”注释处接）。
- 整树仍在每次文档变化时重排；有原子行内盒的页面至少布局两轮（第一轮还不知道块的内容宽度，第二轮按它重量原子盒），至多六轮；没有原子盒的页面一轮。原子盒的最小/最大内容宽度在没有百分比、里面没有原子盒时只量一次，之后各轮只在宽度变了才重新布局它。
- `Fragment` 与契约的差别：契约里的 `InlineStart`/`InlineEnd` 换成了 `LineBox.spans: Vec<SpanBox>`（每个有内容的行内元素每行一个边框盒，带 `starts`/`ends`），因为它们的两个用处（`getClientRects`、画背景边框）要的都是一个矩形；
  `Fragment::Text` 是 `TextFragment`（节点、UTF-8 范围、塑形结果、x、宽、基线、`TextPaint`、可放光标的位置 `stops`、`generated`、`select_none`），其中 `run.glyphs[i].byte` 是相对该片段自己文字起点的字节。`InlineLayout` 另有 `last_baseline` 与 `width`（最宽一行，量内容宽度用）。
  `LayoutEngine` 另有 `inline_layout(node)`、`fragments()`、`register_font(...)`。

**第二轮（评审与中央测试之后）**：

- 中央测试的失败：`pre_breaks_only_at_newlines` 与 `characters_remember_the_byte_they_came_from` 是期望写错了（`Builder::read(Item::Break)` 把换行放进它结束的那一块；“a  é” 里 é 在字节 3），已改；
  `capitalize` 只大写第一个词是真错（`after_letter` 在空白与换行处没有清零），已改；
  `word_break_and_overflow_wrap…` 是代码错：taffy 对固定宽度或拉满的块用 `PerformLayout`，量度函数收到的是“已知宽度 = Definite(可用)”，旧代码取 `max(最小内容宽度, …)`，
  `overflow-wrap: break-word` 的最小内容宽度是整个单词，所以块按单词那么宽排版、高度只有一行。现在固定宽度、百分比宽度与流内块（拉满）按可用宽度排版（`atoms.rs` 的 `fills_room`），收缩适配的盒仍不窄于最小内容宽度。
- 评审的 MUST：M1 契约写全（`SpanBox`、`LineBox.spans`、`TextFragment`、`InlineLayout.{last_baseline,width}`、`LayoutEngine::{inline_layout,fragments,register_font}`、`Layout.fragments`、`RangeProvider`、`Boundary`、`fonts::{FontRegistry,Platform}`，P30 与 P02 的文档也补了）；
  M2 一段文字在保留的换行处结束，范围与末尾光标位置取换行的字节（也用上了 `Segment::Newline` 的字节，M9）；M3 见上；M4 原子盒后的断行按它所在元素（最内层打开的行内元素，否则块）的 `white-space`/`word-break`；
  M5 原子盒的百分比宽度、最小/最大宽度、内外边距按块的内容宽度先换成长度再单独布局（高度的百分比按 `auto`）；M6 没有基线的原子盒以下外边距边贴基线；M7 `Element.getClientRects` 先 `fresh`；
  M8 见上；M10 一个片段的字形用按字节排序的下标二分找，不再每个片段扫全部字形。
- SHOULD：`bolder`/`lighter` 走到写下它的祖先再算一步；继承来的百分比 `line-height` 按写下它的祖先的字号；`keep-all` 只把字母与数字（`is_alphanumeric`）连在一起；
  `pre-wrap` 行尾的空格悬挂但留在片段里（有 stops，光标可以停在后面；以换行结束的行里不悬挂，除非溢出），整个片段的宽度含它，行本身不含；两端对齐的行上，行内元素的盒到两端对齐后的行尾；
  被块拆开的行内元素只有第一段有起始边、最后一段有结束边（`Plan::Resume`/`Plan::Pause`）；空的行内元素有外边距/内边距/边框就占一行（`style::has_edges`）；折叠的 range 返回一个宽度为 0 的盒子（`caret_rect`）；
  `Document::set_position_provider` 与 `Document::text_position_at`（`PageLayout` 装上）；UTF-16 换算与 `unquote` 用 verve-dom 导出的（`utf8_offset_to_utf16`、`utf16_offset_to_utf8`、`css::unquote`）；
  `style::Len` 并入 `value::Dim`；`style::Reader` 的方法改名为 `text`/`text_or`，与 `props::Reader` 同名（两个结构体没有合并：一个能读生成盒的样式表、能找写下继承值的祖先，另一个要记 `unsupported` 与百分比 padding，合并要重写 props，留给 P33 之后）；
  `FontRegistry::best` 按 CSS Fonts 4 的顺序匹配字重（400–500 先试不细于请求、不超过 500 的）；`ranks()` 每次布局只算一次（`OnceLock`）；原子盒的最小/最大内容宽度缓存（见上）；
  新增文字很多的页面：`tests/electron/apps/text-2000/`（2000 段，在窗口里量帧时间）与 `tests/inline.rs` 里被 `#[ignore]` 的 `two_thousand_paragraphs_lay_out`（无头，release 下量布局时间）。
- 仍然没做、也没在这一轮做的（第二阶段或之后）：
  - `-webkit-line-clamp` 只在块直接有行内内容时数行，不会数穿块级孩子；也没有对被截掉的内容裁剪；
  - `display: inline-table` 当块处理；
  - `text-indent` 没有算进最小/最大内容宽度；
  - 双向文字：RTL 里夹的 LTR 片段在跨元素时不重排（只有整块镜像）；
  - `inline-flex` 的基线按最后一行算，Flexbox §8.5 要的是第一个 flex 项的基线；
  - 影子宿主与 slot 里的文字：范围偏移按 `flat_children` 走，没有为 slot 的分配另做处理；
  - 原子行内盒（`<img>`、`inline-block`）不出 range 的盒子（选中一个图片得不到矩形）；
  - `document.caretPositionFromPoint`/`caretRangeFromPoint` 的绑定（钩子 `Document::text_position_at` 有了，对象的构造留给第二阶段）；
  - `pre-wrap` 行尾悬挂空格的 range 盒子的宽度含空格——这一点没有在 Electron 39.8.10 里观察过，第二阶段用真字体核对；
  - 两端对齐的行里，悬挂的空格不参与拉伸，但以空格以外的东西结束的 `Gap::Cjk` 行没有单独测试。

**给第二阶段（接着做）**：

- `paint.rs` 的 `Painter::context` 里 “P31” 注释处接：对每个有 `InlineLayout` 的盒（`BoxTree.inline[index]`，内容盒原点 = `Placed.rect` 加边框与内边距），按行出：行内元素的 `Box`（背景、边框）→ 选区 `Box` → `Glyphs`（按字形的 `font` 切段）→ `Decoration` → 原子盒照旧（`flow()` 里已画）。
- 真测量器（`text_measurer.rs`）要给 `metrics` 取整，`shape` 要在回退字体处给字形标 `font`；`WindowTextSystem` 每若干次布局重建。
- 通道（`FromRenderer::Font`、`FontsChanged`）接到 `LayoutEngine::register_font`，`document.fonts.ready`。
- 选区：快照里的文档选区还没有（`Snapshot` 没带），`range_rects` 已可用，需要 verve-dom 把选区交出来。

**要跑的测试**（一次一条）：

```
cargo test --locked -p verve-layout --lib
cargo test --locked -p verve-layout --test inline
cargo test --locked -p verve-layout --test behavior
cargo test --locked -p verve-layout --test chromium
cargo test --locked -p verve-dom --test layout_provider
cargo test --locked -p verve-electron --test geometry
cargo test --locked -p verve-electron --test headless
cargo clippy --locked -p verve-layout -p verve-dom -p verve-electron --all-targets -- -D warnings
```


### 第二阶段：真字体、画字、选区、`@font-face`

**做了**：

- **`GpuiMeasurer`**（`crates/verve-gpui/src/text_measurer.rs`）：`TextMeasurer` 建在 App 共享的 `Arc<TextSystem>` 上（Shell 启动时 `Hub::set_text_measurer(...)` 装上，之后建的页面都用它；没装就是无头测量器）。
  `FontKey` = GPUI `FontId` 的数字，渲染器线程与绘制线程同一个实例所以 id 通用；`resolve` 在族名表里挑第一个**已安装**的族（`all_font_names`）作主字体、其余作 `FontFallbacks`（所以未安装的 `Helvetica` 不会让 GPUI 掉进 `.ZedMono`），带缓存；
  `shape` 用 `WindowTextSystem::layout_line`，按 (文本, 字体, 字号) 缓存，回退字体换字时每个字形带着画它的 `font` 与 `is_emoji`。
  **`finish_frame` 的替代**：测量器按字节算缓存：`WindowTextSystem` 塑形累计超过 4 MB（文字加字形）后，在**一次布局结束时**整个重建；已塑形表超过 32 MB 清空；`metrics` 表超过 8192 项清空（之前按条数 4096/40000，`pre.textContent += line` 这类越长越大的文本节点每改一次留一份，O(n²)）。
  这个“布局结束”有了调用点：`TextMeasurer::finish_layout()`（默认空），`LayoutEngine::update`（整体重排后）与 `replace` 的末尾各调一次，`GpuiMeasurer` 在里面做上面的重建。
  **补丁公开 `WindowTextSystem::finish_frame` 之后**：`finish_layout` 里改成调 `finish_frame`，删 `shaped_bytes` 与 `SHAPED_BUDGET`。
  **`line_gap`**：GPUI 的 `FontMetrics::line_gap` 读得到（DirectWrite 的 `lineGap` = hhea 总行高减去 Windows ascent+descent，Arial 67/2048），但 `TextSystem` 没有公开取它的函数，所以测量器带一张小表（`LINE_GAPS`：Arial、Times New Roman、Yu Gothic (UI)、SimSun），
  其余字体按 0。**补丁公开 `TextSystem::font_metrics`（或 `line_gap`）之后**删这张表。
  **取整**：`metrics` 把 ascent、descent、line_gap 各自四舍五入（Blink 的 `SimpleFontData` 就是这样），布局里 `line-height: normal` = 三者之和。
- **Chromium 的数字（Electron 39.8.10，Windows 10，DPR 1，隐藏窗口，探针在 `SCRATCH\p31\probe1`）**，冻结在 `crates/verve-gpui/tests/text.rs` 的头注里：
  `line-height: normal` 一行 `Hxg`：Segoe UI 13/16/20px = 17/21/27，Arial 15/18/23，Consolas 15/19/23，Times New Roman 16/18/23，Courier New 15/18/23，Microsoft YaHei UI 16px = 20；
  基线：Segoe UI 16px 在行顶下 17，Consolas 15，Times New Roman 14（= 取整后的 ascent，也就是 GPUI/DirectWrite 的 Windows ascent，**不是 hhea**：Consolas hhea 是 11.883，Chromium 用 14.719 → 15）；
  行里夹中文或 emoji 不让行变高（21 → 21，YaHei UI 比 Segoe UI 矮；回退字体更高时 Chromium 会加高，我们仍不会，见下）；`Hxg` 宽 Segoe UI 28.125、Arial 28.453、Consolas 26.391、Times 27.563；
  `<sub>`/`<sup>` 的位移与第一阶段记下的一致：`sub` 向下 `size/5 + 1`、`sup` 向上 `size/3 + 1`（父字号 16px：4.2 与 6.33；16px 行里 `<sub>` 盒顶在 10.516、`<sup>` 在 0、行高 27.516）。
  **探针里的另一个发现**：这台 zh-CN 机器上 Electron 的 `sans-serif`、`serif`、`monospace` 全都落到 Microsoft YaHei（`Hxg` 宽 30.594），所以夹具用显式的族名，不依赖通用族。
- **显示列表出字**（`crates/verve-layout/src/paint/text.rs`，契约见上面“第二阶段的契约”）：行内元素的 `Box`（`SpanPaint`：纯色背景、边框、px 圆角；`slice` 去掉另一行那一侧的边框与圆角）→ 选区 `Box`（行盒高度）→ 每个文本片段的阴影 `Glyphs`、`Glyphs`、`Decoration`。
  全部在盒自己的裁剪与滚动框之内（块自己是滚动容器时，它的文字在它自己的框里）；被祖先裁剪完全盖掉的行不出项。文字在层叠上下文里排在浮动之后、定位盒之前。
  装饰的几何是近似的（下划线顶在基线下 `size/10`、线粗 `max(1, size/16)`、删除线在 x 高一半、上划线在 ascent），`double/dotted/dashed` 画成实线，`wavy` 用 GPUI 的波浪下划线；没有用字体自己的下划线位置与粗细。
- **GPUI 回放**（`plan.rs`、`dom_element.rs`）：`Shape::Glyphs`/`Shape::Decoration`（窗口坐标，不透明度乘进颜色）；`paint_glyph`（emoji 用 `paint_emoji`），下划线 `paint_underline`、删除线 `paint_strikethrough`。
  **帧成本**：plan 仍是每个 list 与滚动预测算一次；回放时 `Glyphs` 先按（窗口、它自己的裁剪）剔除，只有看得见的行才去问文本系统（2000 段的页面每帧只有几十行）。
  软件画家（`--screenshot`）没有字体：`Glyphs` 不画，`Decoration` 画成一条杠，所以 `--screenshot` 的图里没有字，没有任何测试比较窗口里字的像素。
- **选区**：`LayoutEngine::set_selection`（DOM 端点），`PageLayout::show_selection` 在每次发布前从 `Document::selection_boundaries` 读窗口的选区；颜色 `#3390FF` 加 45% 不透明度，文字仍是原色（Chromium 选中文字变白，`::selection` 是 P22 的）。
  命中（点 → 文本位置）是 `text_position_at`（第一阶段），现在绑到 `document.caretRangeFromPoint` 与 `document.caretPositionFromPoint`（`bind/caret.rs`）；**鼠标拖选等 P32 转发指针**，到时 P32 调 `Document::text_position_at` 设选区即可。
- **`@font-face` 通道**：见上面的契约：渲染器线程直接 `add_fonts`（不加 `FromRenderer::Font`/`FontsChanged`，P02 表里那一行划掉了）。`Document::set_font_provider`（verve-dom）在字节到手时（`new FontFace(f, buffer)`，或 `load()` 取回 `url()` 后的 `settle_font_bytes`）问宿主，
  答 true 才 `loaded`；`PageLayout` 的钩子 = `measurer.add_face` + `LayoutEngine::register_font` + 让文档的布局失效，`document.fonts.ready` 在这之后 resolve，所以之后量到的宽度是新字体的。
  GPUI 的 `add_fonts` 按字体自己 name 表的族名登记：`add_face` 读字节里的 `name` 表（`font_name.rs`：nameID 16，没有就 1；Windows 平台、UTF-16BE；美国英语在前），逐个问文字系统“有没有这个族”，取第一个有的；都没有就返回 false，**不会把能用的面换成解析不到的名字**。
  同一份字节（页面重载、另一页）按字节的哈希只 `add_fonts` 一次；同一别名的第二个字重/斜体（常规 + 粗体）各自登记，`resolve` 按字重与斜体取最近的。别名表（CSS 族名 → 字体面）是**页面自己的**：`Hub` 经 `TextMeasurer::for_page()` 给每个页面一个测量器，页面之间共用文字系统与缓存（`resolved` 按替换后的名字键，`runs` 按 (字体, 字号, 文本)），所以页面 B 的 `font-family: Brand` 看不到页面 A 的 `Brand`，A 注册也不清别人的缓存。
  **做不到的**：`add_fonts` 进的是 app 共用的 `TextSystem`，是全局的、撤不掉：字体字节一经任何页面声明就留在 app 的字体集里，内部族名与系统已装的族同名（如 Consolas）时，DirectWrite 先搜自定义集合，所以这个面对**所有页面**盖过系统同名族；`delete`/`clear` 一个 `FontFace` 不会反注册它。
  “某个族系统有没有”由文字系统自己答（`resolve_font` 不落到回退即有），按名字缓存：`Microsoft YaHei`、`SimSun`、`SimHei` 这类在中文系统里本地化名为“微软雅黑”等的族不再被丢掉。
  只有 ttf/otf（woff/woff2 的字节 `add_fonts` 不收，face 置为 `error`）；样式表里的 `@font-face` 规则 verve-dom 还没读（P22），只有脚本的 `FontFace` 走这条通道。
- **测试**（全部没有跑过）：`verve-layout --test text_paint`（无头：字形、装饰、行内盒、选区、顺序、裁剪）；`verve-gpui --lib`（plan 的 Glyphs/Decoration）；`verve-dom --test fonts`/`layout_provider`（字体钩子、caret、选区端点）；
  `verve-gpui --test text`（Windows 真字体、无窗口：混排每个字形有宽度且无缺字、YaHei UI 为中文回退、Chromium 的行高/基线/宽度/sub/sup、注册的面改变宽度）；`--test text_fixture`（夹具的每个块的行数与每行上下沿、块的位置与高度，与 Chromium 差 ≤ 2px）；
  两个 `#[ignore = "opens real windows"]`：夹具的 `--screenshot`（只验退出码与尺寸）与 `text-2000` 的帧时间（p95 < 4 ms，release）。夹具与 Electron 的基准：`tests/electron/apps/text/`（`expected-chromium.json` 与 `.png`，`VERVE_TEXT_JSON=… VERVE_TEXT_PNG=… electron tests/electron/apps/text` 重做）。

**没做 / 已知偏差**：

- 回退字体比主字体高时，行高不随之增加（Chromium 会）：`line-height: normal` 只用主字体的 `metrics`（这台机器的探针里 YaHei UI 比 Segoe UI 矮，没有遇到）。
- 行内元素还没有 `Hitbox`（`<a>` 的点击与 `cursor: pointer`）；`position: relative` 的行内元素、`::before`/`::after` 的盒子、列表标记（`::marker`）仍如第一阶段所记。
- 选中文字不变白；`text-shadow` 不模糊；`text-decoration-style` 的 `double/dotted/dashed` 画实线；下划线位置与粗细是近似。
- **emoji 不跟不透明度与颜色 alpha**：GPUI 的 `paint_emoji` 不带颜色，`with_element_opacity` 在 fork 里是 `pub(crate)`。淡到 alpha 约为 0 的 emoji 不画，半透明的画满不透明。用户备好的 fork 补丁（`with_element_opacity` 公开）之后在 `paint_glyphs` 里包一层即可。已写进 `plan.rs` 的偏差表。
- **字形的纵向偏移被丢掉**：`ShapedGlyph.position.y` 没进 `PlacedGlyph`，所以靠它定位的符号（夹具里“تُكتب”的 damma 之类）位置不对。
- **行内元素各自的 `visibility`** 没读：`visibility` 只看文字所在的块（`TextPaint`/`SpanPaint` 不带它）。
- **`user-select: none` 的文字不再被选区高亮**（`selection_rects` 跳过它），但 `range_rects` 与 `getClientRects` 仍给它的盒子。
- **字体在 app 里是全局的**：见上面“做不到的”。页面之间的别名表是分开的，字节不是。
- **注册的面只在第一个族名时有效**：DirectWrite 的回退只搜系统字体，所以 `font-family: system-ui, Brand` 里的 `Brand` 不会被用到。
- **`FontFace` 在构造时就注册**（有字节时），而不是 `document.fonts.add`；`delete`/`clear` 不反注册。
- **下划线与上划线现在画在字下**，删除线在字上（Chromium 的 `TextFragmentPainter`）；契约在“第二阶段的契约”里改了。
- **`LINE_GAPS` 里 Yu Gothic (UI) 与 SimSun 两行没有在 Electron 里探过**（按字体表算的）；Arial 与 Times New Roman 探过。
- **选区**仍是每个文本片段一个矩形（不是每行一个），相邻片段之间可能有缝；`selection_rects` 的按行过滤仍是 O(行 × 段)；选区项的 `node` 是 0。
- **`caretPositionFromPoint`** 返回普通对象（没有 `CaretPosition` 类和 `getClientRect()`），视口外不返回 null，非有限参数不抛；这三件没做。
- 原子行内盒仍在 `flow()` 里与块一起画，在浮动之前（附录 E 要它们跟着行画）。
- `font-feature-settings` 与 `font-variant-numeric` 没接（`resolve` 没地方放）。
- “在窗口里验证 `paint_glyph` 画出的字对”没有做（我不开窗口）：测试 `mixed_chinese_english_and_emoji_shape_to_real_glyphs` 证明渲染器线程塑出的 id 非缺字、字体 id 来自同一个 `TextSystem`，但画到屏上要你在窗口里看一眼（`electron tests/electron/apps/text`）。
- 体积：第二阶段新增 `GpuiMeasurer`、出项、回放与字体通道，没有新依赖（`unicode-linebreak` 是第一阶段的，约 21 KB）。中央测得超预算（+445,440 字节，预算 300 KB，新目标 ≤ 350 KB）；这一轮的削减：`ShapedRun.glyphs` 改成 `Arc<[PlacedGlyph]>`（片段与显示列表共用，不再每次建列表复制）；`paint_command` 的大分支拆成小函数；重复的辅助函数合并（`set_boundary`、测试支撑）。我没有量，预期的缩减在几 KB 到十几 KB；单态化最多的 `paint/text.rs` 与 `text_measurer.rs` 请用 cargo bloat 看。
- **改了“修改”范围之外的文件**：verve-dom 的 `bind/`、`state.rs`、`load.rs`、`window_state.rs`（字体钩子、选区端点、caretRangeFromPoint）；verve-electron 的 `hub/`（`set_text_measurer`，在第一个页面建出之后再调会 panic）、`page_layout.rs`、`renderer_guest.rs`；verve-gpui 的 `plan.rs`、`screenshot.rs`、`lib.rs`、`font_name.rs`（新）；verve-layout 的 `measure.rs`、`engine.rs`、`paint.rs`（滚动条画在块内文字之上：`flow_with_floats` 在文字之后按内层到外层补 `scroller`）。

**要跑的测试**（一次一条）：

```
cargo test --locked -p verve-layout --test text_paint
cargo test --locked -p verve-layout --lib
cargo test --locked -p verve-dom --test fonts
cargo test --locked -p verve-dom --test layout_provider
cargo test --locked -p verve-electron --test geometry
cargo test --locked -p verve-gpui --features jit --lib
cargo test --locked -p verve-gpui --features jit --test text
cargo test --locked -p verve-gpui --features jit --test text_fixture
cargo test --locked -p verve-gpui --features jit --test text_fixture -- --ignored   (真窗口)
VERVE_FRAME_STATS=1 target\release\electron.exe tests\electron\apps\text-2000      (release 帧时间)
```
