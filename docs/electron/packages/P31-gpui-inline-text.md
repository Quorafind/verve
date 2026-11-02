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
pub struct PlacedGlyph { pub id: u32, pub x: f32, pub byte: u32, pub is_emoji: bool }
pub struct ShapedRun { pub font: FontKey, pub size: f32, pub glyphs: Vec<PlacedGlyph>, pub width: f32 }

/// 行内格式化上下文的结果：P30 用它确定块高度，DisplayList 用它出 Glyphs/Decoration 项。
pub struct InlineLayout { pub lines: Vec<LineBox>, pub height: f32, pub first_baseline: f32 }
pub struct LineBox { pub rect: Rect, pub baseline: f32, pub fragments: Vec<Fragment> }
pub enum Fragment {
    Text { node: NodeId /* 文本节点 */, range: Range<usize> /* UTF-8 字节 */, run: Arc<ShapedRun>, x: f32, style: TextPaint },
    Atomic { node: NodeId, rect: Rect },                    // inline-block / img / inline-flex / 表单控件
    InlineStart { node: NodeId }, InlineEnd { node: NodeId } // 行内元素的边界：算 getClientRects、画背景边框
}

impl LayoutEngine {
    pub fn client_rects(&self, node: NodeId) -> Vec<Rect>;                     // 行内元素每行一个盒子
    /// 文本命中：视口坐标 → (文本节点, UTF-8 偏移)。selection 拖选、caretPositionFromPoint、document.caretRangeFromPoint 用
    pub fn text_position_at(&self, x: f32, y: f32) -> Option<(NodeId, usize)>;
    /// Range 的盒子（Range.getClientRects / getBoundingClientRect）
    pub fn range_rects(&self, start: (NodeId, usize), end: (NodeId, usize)) -> Vec<Rect>;
}

// verve-dom 需要暴露（若 P23/P24 尚未提供，本包补上）：
// Snapshot 里的文档选区：Option<(anchor: (NodeId, usize), focus: (NodeId, usize))>，偏移是 UTF-16（DOM 语义），
// verve-layout 负责 UTF-16 ↔ UTF-8 换算。
// 以及 Document::{text_position_at, range_rects} 的提供者钩子（与 P30 的布局提供者同一机制）。
```

`@font-face`：P20 的加载管线拿到字体字节后，渲染器发 `FromRenderer::Font { family, weight, style, bytes: Arc<[u8]> }`
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

（实现者完成后填写）
