# P22 verve-dom CSS 补齐

**依赖**：无（可与 P02 并行）  **规模**：L，**分两个 PR 交付**：P22a（拆分、`@layer`、现代颜色、数学函数、伪元素、`-webkit-app-region`）与 P22b（过渡与动画、Web Animations）  **修改**：`crates/verve-dom/src/{css.rs,lib.rs,state.rs,dom.rs,bind/element.rs,bind/window.rs,bind/platform.rs}`；可拆出 `crates/verve-dom/src/css/` 子模块（`css.rs` 已近 3000 行，建议本包先把它拆成 `css/{selector.rs,sheet.rs,cascade.rs,values.rs,media.rs}`，单独一个纯重构 commit）

## 目标

让真实应用（Vite + React/Vue、Tailwind v3/v4、组件库如 Ant Design/MUI/shadcn、VS Code 风格的主题变量）
产出的 CSS 在 verve-dom 里**算对**：选择器、层叠、值都和 Chromium 一致，并把绘制需要但 DOM 树里
没有的东西（伪元素、动画中的值、滚动条样式、拖动区域）通过快照交给 GPUI 侧（P30/P31）。

verve-dom 不做布局：凡是依赖布局的值（`%`、`calc()` 里含 `%`），本包负责**规范化**后交给绘制侧解析。

## 背景（先读）

- `crates/verve-dom/src/css.rs` 开头的模块注释：支持了什么、明确不支持什么。
- `parse_block`：`@media` 递归；`@supports`/`@layer`/`@scope`/`@starting-style` **无条件**展开；其他 at-rule
  （`@font-face`、`@keyframes`、`@property`、`@container`、`@import`）**丢弃**。
- `Pseudo` 枚举与 `Parser` 里伪元素的处理（`"before" | "after" | "first-line" | "first-letter"` 解析成永不匹配；
  `-webkit-` 前缀的未知伪类同样）。
- `set_declared`：已展开的简写——`padding/margin/inset/scroll-margin/scroll-padding`、`border*`、`outline`、
  `border-radius`、`background`（只取颜色和整串 image）、`gap`、`flex`、`flex-flow`、`overflow`、`font`
  （`font_longhands`）。
- `color()`：只认 `#hex`、`rgb[a]()`、`hsl[a]()` 和命名色。`length()`：`px/em/rem/…` 到像素。
- `initial_value`、`is_inherited`、`USER_AGENT_SHEET`。
- `Computed::refresh`（增量重算）、`Snapshot`/`SnapshotData`（`crates/verve-dom/src/lib.rs`）。
- `Environment { width, height, device_pixel_ratio, dark, reduced_motion, print }`。
- 现有测试：`crates/verve-dom/tests/css.rs`。

## 优先级（按真实应用的出现频率排序，按此顺序交付）

| # | 项 | 为什么 |
|---|---|---|
| 1 | `@layer` 顺序正确 | Tailwind v4 整个产物都在 `@layer theme, base, components, utilities` 里；现在只是按出现顺序展开，**未分层样式应压过分层样式、`!important` 反转**，错了会大面积样式错误 |
| 2 | 现代颜色：`oklch()`、`oklab()`、`lab()`、`lch()`、`hwb()`、`color-mix()`、`color(srgb …)`、相对颜色语法 `rgb(from var(--x) r g b / 50%)`、`transparent`/`currentColor` 在计算值中的处理 | Tailwind v4 的调色板全是 `oklch`；`color-mix` 用于透明度修饰 |
| 3 | `calc()`/`min()`/`max()`/`clamp()` 求值 | 几乎所有组件库都用 |
| 4 | `::before`/`::after` 生成内容 + `::marker` | 图标字体、清除浮动、徽标、列表符号 |
| 5 | 过渡与动画（`transition*`、`animation*`、`@keyframes`、Web Animations `animate()`/`getAnimations()`） | hover 效果、加载动画、抽屉/弹层进出场；且很多组件**等待 `transitionend`** 才移除节点，不发事件会卡住 |
| 6 | `@property` 与 `CSS.registerProperty` | Tailwind v4 用 `@property --tw-*` 声明初始值；不支持会导致 `var()` 失效 |
| 7 | 简写补全与逻辑属性 | `place-*`、`grid-template`/`grid-area`/`grid`、`background` 全部子属性、`transition`/`animation`、`text-decoration`、`list-style`、`columns`、`margin-inline/block`、`padding-inline/block`、`inset-inline/block`、`border-inline/block*`、`inline-size/block-size`（v1 只做水平书写方向 `ltr`，逻辑 → 物理映射） |
| 8 | `@supports` 正确求值 | `@supports not (…)` 现在会被错误应用 |
| 9 | CSS 嵌套（`&`、嵌套规则） | 部分构建产物与运行时 CSS-in-JS 保留原生嵌套 |
| 10 | `::-webkit-scrollbar*`、`scrollbar-width`/`scrollbar-color`、`::placeholder`、`::selection` | Electron 应用几乎都自定义滚动条；输入框占位符和选区颜色 |
| 11 | `:focus-visible` 语义 | 现在与 `:focus` 等价会导致鼠标点击出现焦点环 |
| 12 | `-webkit-app-region` | 无边框窗口拖动（P34 消费） |
| 13 | `env()` | `titlebar-area-*`（Window Controls Overlay）、`safe-area-inset-*` |
| 14 | `@container` 与容器单位 `cqw/cqh/cqi/cqb` | 新组件库开始使用；依赖布局回报 |
| 15 | `@font-face` 解析 | 交给 P20 取字体（本包只解析） |
| 16 | `@import` 语句 | 由 P20 取回，本包提供"在此位置插入一张样式表"的接口 |

## 接口契约

### 1. `@layer`

- 实现 CSS Cascade 5：层声明顺序（`@layer a, b;` 和首次出现顺序）、嵌套层名 `a.b`、匿名层、
  `@import … layer(x)`。层叠排序键：`(origin, importance, layer_order, specificity, source_order)`，其中
  正常声明**未分层 > 后声明的层 > 先声明的层**，`!important` 时层顺序反转。
- `Rule` 增加 `layer: u32`（所在样式表的 `layers` 里的序号，未分层用 `LAYERLESS = u32::MAX`）；`Styler` 把它排成全局顺序。层按树分别排序（影子树有自己的 `@layer a, b`），一层自己的规则排在它的子层之后；匿名层在每张样式表里各是独立的层。
  排序键写成 `Rank { precedence, layer, specificity, order }`：`precedence` 是 P23 的来源×重要性×树（原 `scope::Layer`，改名以免和 `@layer` 混淆）。`revert-layer` 按完整的 `(precedence, layer)` 回退，没有更低的层时回到用户代理的值。

### 2. 颜色（`css::color` 扩展）

- 新增上表所有颜色函数，输出仍是 `[f32; 4]` sRGB（超色域裁剪）。`color-mix(in srgb|oklab|oklch|hsl…, a p%, b q%)`。
- 计算值文本：`getComputedStyle` 对 `oklch()` 返回 `oklch(…)` 原样（Chromium 行为：现代颜色函数保留其色彩空间），
  `rgb()/hsl()/#hex/命名色` 返回 `rgb(r, g, b)` / `rgba(r, g, b, a)`。以 Chrome 实测冻结。
- `currentcolor` 在计算值中保留为关键字（继承时按元素自己的 `color` 解析），`css::color` 的调用方提供
  `current: [f32; 4]` 参数（新增 `css::color_with(text, current)`）。

### 3. 数学函数

- 新增 `css::calc` 模块：解析 `calc/min/max/clamp/round/mod/rem/abs/sign` 及嵌套、`var()` 先替换。
- 单位：`px/em/rem/ex/ch/vw/vh/vmin/vmax/dvh/svh/lvh/dvw/…`（视口单位按 `Environment`）、`%`、`deg/rad/turn`、`s/ms`、无单位数。
- **计算值**：所有项都是绝对长度时折叠成 `Npx`；含 `%`（或容器单位、`auto` 不可）时输出规范化形式
  `calc(50% - 12px)`（百分比项合并、像素项合并）。
- 绘制侧接口（需新增，`pub`）：

```rust
/// A length as the drawer resolves it against its containing block: `px + percent% of basis`.
pub struct LengthCalc { pub px: f32, pub percent: f32 }
pub fn length_calc(text: &str, font_size: f32, root_font_size: f32, env: &Environment) -> Option<LengthCalc>;
```

  P30 用它喂给 taffy（taffy 支持 calc 回调）或在 GPUI 侧解析。

### 4. 伪元素进快照（需新增）

- 选择器引擎：伪元素成为 `Selector` 的一部分（`pseudo_element: Option<PseudoElement>`），只匹配"该元素的伪元素"；
  `PseudoElement = Before | After | Marker | Placeholder | Selection | FirstLine | FirstLetter | Backdrop | WebkitScrollbar(Part)`。
- 层叠：对每个元素额外为有匹配规则的伪元素算样式（继承自宿主元素）。`::before/::after` 只有 `content`
  计算值不为 `none`/`normal` 时才"存在"。
- **不放进 DOM 树**（`childNodes` 看不到，与浏览器一致）。快照新增：

```rust
pub struct SnapshotData {
    // … 现有字段 …
    /// Generated boxes: an element's ::before/::after/::marker and the styles of its
    /// ::placeholder, ::selection, ::-webkit-scrollbar parts, ::backdrop.
    pub pseudo: Arc<HashMap<(NodeId, PseudoElement), PseudoBox>>,
}
pub struct PseudoBox { pub style: Arc<Style>, pub content: Vec<ContentItem> }
pub enum ContentItem { Text(String), Image(String /* url */), OpenQuote, CloseQuote }  // 计数器在遍历时已填成 Text，不会到绘制侧
```

- `content` 支持：字符串（含转义 `"\f101"`）、`attr(x)`、`open-quote/close-quote`、`url()`、`counter()/counters()`
  （计数器 `counter-reset/increment/set` 的值在快照时按文档顺序算好，填进 `Text`）、`none/normal`。
- `Changes` 里伪元素变化归到宿主节点的 `restyled`。
- `getComputedStyle(el, '::before')` 返回伪元素样式（现在第二个参数被忽略）。

### 5. 过渡与动画

**决定：时间轴与插值在 verve-dom 里按帧计算**（而不是交给 GPUI）。理由：`getComputedStyle` 必须读到
动画中的值、`transitionend`/`animationend`/`animationiteration` 事件必须在正确的帧发、Web Animations API
（`el.animate()`、`getAnimations()`、`finished` promise）本来就在 JS 线程。性能代价由"动画覆盖层"控制：

- `State` 新增 `animations: AnimationStore`：每个运行中的过渡/动画 `(node, property, from, to, timing, start, duration, delay, iterations, direction, fill, play_state)`。
- 触发：`Computed::refresh` 发现某节点的可过渡属性计算值变化且 `transition-property` 覆盖它 → 新建过渡（处理
  中断反向等 CSS Transitions 规则的简化版：被中断时从当前值开始）；`animation-name` 出现/变化 → 新建动画。
- 推进：`Document::frame(runtime, now)`（`FrameClock::Host`）或定时器帧（`FrameClock::Timer`）里，计算每个动画
  当前值，写入**覆盖层** `animated: HashMap<NodeId, Style>`（不是重跑层叠）；快照里 `styles` 读取时覆盖层优先
  （`Computed` 增加 overlay 合并，或 `Snapshot::style` 先查 overlay）。只把有动画的节点放进 `Changes.restyled`。
- 插值（v1）：长度/数字/百分比（含 `calc` 规范形式之间）、颜色（在 oklab 插值，Chromium 对 legacy 颜色在 sRGB 插值——
  以实测为准）、`opacity`、`transform`（函数列表逐项插值，不匹配时矩阵分解插值）、`box-shadow`、`filter`（v2 可只离散）。
  其他属性离散（50% 处切换）。
- 缓动：`linear`、`ease*`、`cubic-bezier()`、`steps()`、`linear()` 函数。
- 事件：`transitionrun/transitionstart/transitionend/transitioncancel`、`animationstart/animationiteration/animationend/animationcancel`，
  `elapsedTime`、`propertyName`/`animationName`、`pseudoElement`。
- `prefers-reduced-motion`（`Environment.reduced_motion`）只影响 media query，不自动关动画（与浏览器一致）。
- Web Animations：`Element.animate(keyframes, options)` 返回 `Animation`（`play/pause/cancel/finish/reverse`、
  `currentTime`、`playbackRate`、`finished`/`ready` promise、`onfinish`）；`getAnimations()`（元素与 `document`）
  返回 CSS 过渡/动画对应的 `CSSTransition`/`CSSAnimation` 对象。现有 `animate()` 返回"已完成的动画"
  （`bind/element.rs` 约 1153 行）、`getAnimations()` 返回空数组（约 1151 行），本包替换。
- 没有 `FrameClock::Host` 的绘制器时（无头测试），时间按 `Runtime` 的时钟推进；测试用可控时钟。

### 6. `@property` / `CSS.registerProperty`

- 解析 `syntax`、`inherits`、`initial-value`；已注册的自定义属性：不继承时不往下传、未设置时取初始值、
  按 `syntax` 校验与计算（`<length>` 折叠成 px、`<color>` 规范化），可被过渡插值。
- `CSS.registerProperty({ name, syntax, inherits, initialValue })`、重复注册抛 `InvalidModificationError`。

### 7–9. 简写、`@supports`、嵌套

- 简写：上表全部；`getComputedStyle` 读简写时按 Chromium 规则序列化（能序列化才返回，否则空串）。
- `@supports`：求值 `(prop: value)` 用"本层叠是否认识该属性且能解析该值"，支持 `not/and/or`、`selector(…)`。
- 嵌套：CSS Nesting（`&`、隐式 `&` 前缀、嵌套 at-rule `@media` 在规则内），展开成平铺规则，特异性按 `:is()` 计算。

### 10. 滚动条、占位符、选区

- `::-webkit-scrollbar`、`-thumb`、`-track`、`-corner`、`-button` 的样式进 `pseudo`（宿主可以是任一元素或 `html`）；
  `scrollbar-width: none|thin|auto`、`scrollbar-color`、`scrollbar-gutter` 作为普通属性。P30 按此画滚动条。
- `::placeholder`（`input`/`textarea` 有 placeholder 时）、`::selection`（任意元素）样式进 `pseudo`。

### 11. `:focus-visible`

- `Dom` 新增焦点来源 `FocusOrigin { Keyboard, Pointer, Script }`（`Document::focus`、键盘 Tab、指针按下分别设置）。
  `:focus-visible` = 聚焦且（来源是键盘，或元素是文本输入类，或脚本聚焦且上一次交互是键盘）。

### 12. `-webkit-app-region`

- 保证 `-webkit-app-region`/`app-region: drag|no-drag|none` 进入计算值（**不继承**，但 P34 计算拖动区域时按
  "drag 的盒子减去其后代中 no-drag 的盒子"处理）。加测试锁定它不会被层叠丢掉。

### 13. `env()`

- `env(name, fallback)` 在 `substitute` 阶段替换。`Environment` 新增 `env: HashMap<String, String>`
  （`titlebar-area-x/y/width/height`、`safe-area-inset-*`、`keyboard-inset-*`）；未知名用 fallback；
  `Document::set_env(runtime, map)` 需新增，变化时重算（P34 在 `titleBarOverlay` 时设置）。

### 14. `@container`

- `container-type`/`container-name`/`container` 简写；`@container name (min-width: 400px)` 依据**上一次布局回报**
  （`State.rects`）里最近的容器祖先的内容盒尺寸求值；`Document::set_layout` 时若有容器尺寸跨过任何查询阈值，
  标记其子树重算（每帧最多一次，避免布局↔样式振荡）。容器单位同理。

### 15. `@font-face`（只解析）

```rust
pub struct FontFaceRule {
    pub family: String,
    pub sources: Vec<FontSource>,           // url(…) format(…) / local(…)
    pub weight: (u16, u16), pub style: String, pub stretch: String,
    pub unicode_range: Vec<(u32, u32)>, pub display: String,
    pub base_url: Option<String>,           // 样式表自己的 URL，相对地址据此解析
}
impl Document { pub fn font_faces(&self) -> Vec<FontFaceRule>; }   // 需新增，P20 读取
```

`Sheet` 需要知道自己的 base URL（`<style>` 用文档 URL，`<link>`/`@import` 用样式表 URL）：`Sheet::parse_with_base(text, base)`。

### 16. `@import`

- `parse_block` 记录 `@import url(x) [layer(…)] [supports(…)] [media]` 到 `Sheet.imports`；提供
  `Sheet::resolve_import(index, Sheet)` 让 P20 填入取回的样式表；级联时被导入表的规则按位置展开。

## 步骤

1. 纯重构：拆 `css.rs` 为子模块，测试全过，单独 commit。
2. 按优先级表逐项交付，每项一个 commit + 测试。1–5 是本包的必交部分；6–16 可在本包内继续，
   若时间不足，把剩余项写进「状态」节交回。
3. 每项先用 Chrome 实测（`getComputedStyle` 输出、事件顺序），期望值冻结进测试。

## 验收

- `cargo test -p verve-dom`：现有全部通过；新增：
  - `tests/css_layers.rs`：Tailwind v4 的 `@layer` 片段（产物提交在 `tests/fixtures/tailwind4/`）里 utilities 覆盖 base、
    未分层压过分层、`!important` 反转；
  - `tests/css_values.rs`：颜色函数表（每个函数若干样例 → `[f32;4]` 与 `getComputedStyle` 文本）、`calc` 折叠与规范化；
  - `tests/css_pseudo.rs`：`::before { content: attr(data-x) }` 出现在 `snapshot.pseudo`，`childNodes` 不含它；计数器；
    `getComputedStyle(el, '::after')`；
  - `tests/css_animation.rs`：可控时钟下 `opacity` 过渡在 50% 时为中间值、到点发 `transitionend` 一次；`@keyframes` +
    `animation-iteration-count: 2` 发一次 `animationiteration`；`el.animate().finished` resolve；中断过渡发 `transitioncancel`；
  - `:focus-visible`：鼠标点击按钮不匹配、Tab 聚焦匹配；
  - `-webkit-app-region` 进入计算值；`env(titlebar-area-height, 30px)`。
- 性能：`tests/css.rs` 增加一个 5000 节点 + Tailwind 产物的基准（`#[ignore]`），全量层叠时间写进「状态」节，
  与本包开始前对比不劣化超过 20%。

## 不做

布局（P30）；竖排与 RTL 的逻辑属性映射（只做 `ltr` 横排）；`@page`/打印；`@scope` 的真正作用域（继续展开）；
`:has()` 的增量失效优化（保持现状）；`::first-line/::first-letter` 的实际切分（样式进快照，切分交给 P31，v1 可不做）；
`::view-transition` 与 View Transitions API；`@counter-style` 自定义；`paint()`/Houdini。

## 给其他包的备注

- **P30**：读取 `LengthCalc`、`snapshot.pseudo`（`Before/After/Marker` 作为首/末子盒、`WebkitScrollbar` 画滚动条）、
  动画覆盖层后的样式。
- **P31**：`::selection`、`::placeholder`、`::first-line/::first-letter` 样式。
- **P20**：`font_faces()`、`Sheet.imports`/`resolve_import`、`Sheet::parse_with_base`。
- **P21**：`Origin::User`（`webFrame.insertCSS({ cssOrigin: 'user' })`）。
- **P34**：`-webkit-app-region`、`Document::set_env`。

## 状态

### P22a（分支 `electron/p22a-css`）

**完成**

- `css.rs` 拆成 `css/{mod,selector,sheet,cascade,values,media,syntax,layer,color,calc,pseudo,boxes,counters}.rs`（加上 P23 的 `scope.rs`、`styler.rs`）；`css.rs` 不再在 `ci/style/large-files.txt` 里。另把 `Snapshot` 移到 `snapshot.rs`、`getComputedStyle` 绑定移到 `bind/computed.rs`，`lib.rs` 1835→1790、`platform.rs` 2355→2259，棘轮已下调（均相对 P25 合并后的值）。
- `@layer`：声明顺序、嵌套层（一层自己的规则在子层之后）、每张样式表各自的匿名层、按树分别排序（影子树有自己的层序）、`!important` 反转、`revert-layer` 按完整 `(precedence, layer)` 回退（没有更低的层时回到用户代理值）。P23 的 `scope::Layer` 改名 `Precedence`。
- 颜色：`oklab/oklch/lab/lch/hwb/color(srgb|srgb-linear|display-p3)/color-mix()`、相对颜色（通道名）；`color-mix` 按透明度预乘（oklab/oklch/hsl/srgb）；计算值：旧记法写成 `rgb()/rgba()`，新色彩空间保留原文，`currentcolor` 保留关键字。
- `calc()/min/max/clamp/round/mod/rem/abs/sign`：全绝对长度折成 `px`，含 `%` 的写成 `calc(50% - 12px)`；`LengthCalc`/`length_calc`/`length` 共用一张单位表；`rem` 取根元素的字体大小；只有"整个值就是一个数学函数"才计算，`grid-column: 1 / -1`、`aspect-ratio: 16 / 9` 原样保留。
- 伪元素：`::before/::after/::marker/::placeholder/::selection/::first-line/::first-letter/::backdrop/::-webkit-scrollbar*`，也认 CSS 2 的单冒号写法；`SnapshotData::pseudo`（`Arc<HashMap>`）；`content`（字符串及转义、`attr()`、`url()`、引号、`/` 后的替代文本、`counter()/counters()`）；计数器按整个文档顺序重算（兄弟的 `counter-reset` 接管、后代的嵌套、不渲染的不计数）；只为能有该伪元素的元素计算；伪元素的变化把宿主记入 `Changes.restyled`；`getComputedStyle(el, pseudo)` 的目标存在 `Handle` 上，无效伪元素得到没有值的声明。
- `-webkit-app-region` 的测试（规则、层、`var()`、`!important`、内联；不继承；缺省 `none`）。
- 测试：`css_layers`(10)、`css_values`(17)、`css_pseudo`(17)、`css_app_region`(2) 及 `css.rs` 里调整的期望；`cargo test -p verve-dom` 全部 163 个通过、4 个 ignored（含新的基准）。`cargo clippy -p verve-dom --all-targets -- -D warnings` 通过。

**性能**（`cargo test -p verve-dom --test css -- --ignored --nocapture`，debug 构建，同一台机器，取 5 次最好）：5000 个元素（含文本共 7505 个节点）、7 个类、4004 条 Tailwind 形状的规则，全量层叠（"之前"的数字是在 `docs/electron-plan` 的临时 worktree 里跑同一个测试量的，两边各取 5 次最好）：P22a 之前 662–694 ms，P22a 725 ms（约 +6%，在 20% 之内）；解析 28 ms、建索引 9 ms。第一版曾是 1320 ms：`*, ::before, ::after { … }` 这类重置让每个元素都算两个没有 `content` 的伪元素，现在没有 `content` 声明的 `::before/::after` 不再计算。数字是 debug 构建的，release 的绝对值会小得多，比值参考；没有测 release（全工作区 fat LTO，太慢）。

**留给 P22b**：`transition*`/`animation*`/`@keyframes`、`Element.animate()`/`getAnimations()`、动画事件、`prefers-reduced-motion` 之外的时间轴（`State.animations`、`FrameClock` 接线、`bind/animation.rs`、`src/animation.rs`）。评审指出的动画问题（`transition-property: all`、移除 `animation-name` 时取消、Timer 时钟下启动帧、只结束自己的动画、`is_time` 把 `spin-forms` 当成时间等）都属于 P22b，P22a 里没有为它留钩子。

**未做（优先级表 6–16）**：`@property`/`CSS.registerProperty`、简写补全与逻辑属性、`@supports` 求值、CSS 嵌套、`@container`、`@font-face` 与 `@import` 解析。其中 `:focus-visible`（需要 `FocusOrigin`）交给 P32（输入），`env(titlebar-area-*)` 与 `Document::set_env` 交给 P34（窗口），测试 `env(titlebar-area-height, 30px)` 随它们走。

**已知差异**

- `calc(infinity * 1px)`（Tailwind v4 的 `rounded-full`）写成 `33554400px`，Chrome 写成 `3.35544e+07px`，值相同；`calc(10px / 2px)` 在 Chrome 里无效，这里原样保留。
- 影子树里 `::part()` 规则与树内规则按 `@layer` 比较，而不是按层叠上下文（`scope.rs` 的 `Precedence` 只区分 `:host`/`::slotted`）。

- 只在整个值是数学函数时才计算：`margin: 1px calc(…)` 展开后的长属性会算，`box-shadow: 0 0 calc(…) red` 这种函数在值中间的不算；普通的 `1.5em`/`2rem` 保持原文（绘制侧用 `length` 解析）。
- `ex`/`ch` 按 0.5em 算；`lh`/`rlh`、容器单位不解析；`round()` 不支持取整策略关键字。
- 相对颜色的通道只认通道名，通道里写 `calc(l * 0.8)` 不算；`lab()/lch()` 不能做相对颜色的起点；`color-mix` 在 `lab/hwb/srgb-linear` 等空间里按 sRGB 混合。
- 新色彩空间的计算值保留原文，不像 Chrome 那样写成解析后的 `oklch(0.628 0.2577 29.23)`；`color-mix()` 同样保留原文。
- 内联样式里的 `revert-layer` 回到未分层的作者样式（而不是分层样式）。
- 写在不匹配的 `@media` 里的 `@layer` 声明仍会参与层序。
- `ContentItem` 没有 `Counter` 变体（计数器已填成 `Text`）；`::marker` 只为 `li`/`summary`/`display: list-item` 生成，列表项隐含的 `list-item` 计数器未实现；`::-webkit-scrollbar*` 只为 `html`、`body`、`textarea` 和 `overflow: auto|scroll|overlay` 的元素生成；`::backdrop` 只为 `dialog` 和带 `popover` 的元素。
- `p::before span` 这类伪元素不在末尾的选择器按规范视为无效并整条丢弃。
- `bind/window.rs` 里新增了一处对遗留 `bind::opt_string` 的调用（R1 迁到 `verve::native` 时一并处理）。

