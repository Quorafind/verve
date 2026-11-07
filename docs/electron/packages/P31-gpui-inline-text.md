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

`@font-face`：P20 的加载管线拿到字体字节后，渲染器发 `FromRenderer::Font { family, weight, italic, bytes: Arc<[u8]> }`
（**追加到 P02 契约**），Hub 转给 Shell，Shell 调 `cx.text_system().add_fonts(...)` 后回 `ToRendererInput::FontsChanged`
（追加），渲染器把用到该 family 的块标脏重排。字体加载完成前按回退字体排（等于 `font-display: swap`），并让
`document.fonts.ready` 在全部加载后 resolve。

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

