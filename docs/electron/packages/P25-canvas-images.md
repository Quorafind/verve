# P25 canvas 2D 与图片解码

**依赖**：无（可与 P02 并行；与 P20 对接图片字节、与 P35 对接位图交付）  **规模**：M  **修改**：`crates/verve-dom/src/{bind/platform.rs(Canvas 段、ImageData、Path2D),bind/element.rs(IMAGE_ACCESSORS),lib.rs(SnapshotData),state.rs}`；新增 `crates/verve-dom/src/canvas/{mod.rs,context2d.rs,path.rs,text.rs,image.rs,fonts.rs}`；`crates/verve-dom/Cargo.toml`

## 目标

1. `<canvas>` 的 2D 上下文**真正画出像素**：图表库（Chart.js、ECharts canvas 渲染器）、xterm.js 的 canvas 渲染、签名板、
   缩略图生成、`toDataURL` 导出都能工作。
2. 图片解码：`<img>` 有正确的 `naturalWidth/naturalHeight/complete/decode()`；`drawImage`、`createImageBitmap` 能拿到像素。
3. 画好的位图通过快照交给 GPUI 显示（P35 画到屏幕上）。

## 背景（先读）

- `crates/verve-dom/src/bind/platform.rs` 的 Canvas 段（约 1514–1830 行）：`font_size`、`text_width`（**按字符估算**宽度）、
  `context_2d`（状态只是 JS 属性，`CONTEXT_STATE`/`CONTEXT_NUMBERS`）、`CONTEXT_METHODS`（`measureText` 估算、
  `getImageData` 返回全零、渐变/图案是空对象）、`BLANK_PNG`（`toDataURL` 永远返回 1×1 透明 PNG）、`CANVAS_ACCESSORS`（宽高读写属性）、
  `CANVAS_METHODS`（`getContext('2d')` 外返回 `null`、`toBlob` 返回空 Blob、`transferControlToOffscreen` 抛错）、约 2785 行
  `canvas_context` 里把其余绘制方法注册成**空操作**；`construct_image_data`（约 1616 行）。
- `crates/verve-dom/src/bind/element.rs` 的 `IMAGE_ACCESSORS`（约 2715 行：`naturalWidth/Height` 写死 0、`complete` 写死 true）。
- `crates/verve-dom/src/media.rs`：音视频的"状态进快照、像素不进"模式（`MediaStore::snapshot` → `SnapshotData.media`）——本包的
  位图交付照此模式扩展，但这次快照里**要带像素**。
- `crates/verve-dom/src/lib.rs`：`SnapshotData`、`Changes`、`Document::frame`。
- P20：图片字节由 `Loader` 取回（`Destination::Image`），P20 调用本包的 `image_size`。
- GPUI 已依赖 `image` crate 用于 `img()` 解码——**本包使用与 GPUI 相同的 `image` 版本**（看 workspace 锁定的 GPUI rev 的 `Cargo.lock`），
  以免二进制里出现两份解码器。

## 设计决定

1. **光栅化：`tiny-skia`**（纯 Rust、CPU、体积约 0.4 MB、API 与 Skia/Canvas 对应：路径、描边、渐变、图案、混合模式、裁剪掩码）。
   不用 GPU：渲染器线程不碰 GPUI/wgpu；位图交给 Shell 上传为纹理。
2. **文字：渲染器线程自带字体栈**——`fontdb`（系统字体发现 + 加载 `@font-face` 字节）+ `rustybuzz`（整形：连字、字距、复杂文字）+
   `skrifa`（字形轮廓与度量），轮廓交给 tiny-skia 填充。不能用 GPUI 的文本系统：它在 Shell 线程、不是 `Send`，而 `fillText`/`measureText`
   是同步 API。
   - 取舍：canvas 文字的像素与 GPUI（Windows 上是 DirectWrite）画的 DOM 文字会有亚像素级差异（hinting、抗锯齿）；但**前进宽度**来自
     同一个字体文件的 `hmtx`/整形结果，与 DOM 文字宽度基本一致——这对"用 `measureText` 估算 DOM 文字宽度"的库（xterm.js 的单元格宽度、
     文本自适应组件）最重要。字体族解析规则（`sans-serif` → 系统默认、回退链）与 P31 **共用一张映射表**（放在本包 `canvas/fonts.rs`，
     `pub` 出来给 P31 读同一份配置）。
   - 体积：fontdb + rustybuzz + skrifa ≈ 1.2 MB。用 feature `canvas`（默认开）控制，P01 记录。
   - **与 P30 对齐（总控注）**：P30 的方案是在渲染器线程通过 `Arc<gpui::TextSystem>` + 公开的 `layout_line` 补丁做文字度量
     （`verve_layout::TextMeasurer`）。如果 P30 的一天 spike 证实可行，`measureText` 改为走同一个 `TextMeasurer`，
     保证 canvas 与 DOM 的文字宽度**完全一致**；自带字体栈只负责把字形轮廓栅格化进像素缓冲（GPUI 的字形栅格结果在图集里，拿不到像素）。
     届时再评估能否去掉 `rustybuzz`（只留 `skrifa` 取轮廓）以省体积。P30 spike 失败则维持本节原方案。
3. **`<img>` 不在渲染器解码像素**：渲染器只读图片头拿尺寸（`image::ImageReader::into_dimensions`），把**原始字节**（`Arc<[u8]>`）交给
   Shell，由 GPUI 自己解码显示。只有 `drawImage`/`createImageBitmap`/`getImageData` 需要像素时才在渲染器解码（按 URL 缓存，LRU 64 MB）。
4. **位图交付：帧末提交**。绘制命令立即落到 canvas 的像素缓冲；在帧末（`Document::frame` 的 rAF 回调之后，或无绘制器时微任务检查点后）
   把有变化的 canvas 的缓冲"提交"成不可变帧 `Arc<[u8]>` 放进快照。提交时若上一帧的 `Arc` 已无人持有则复用内存，否则复制（双缓冲）。

## 接口契约

### 快照（`crates/verve-dom/src/lib.rs`，需新增字段）

```rust
pub struct SnapshotData {
    // … 现有字段 …
    /// Each canvas's last committed frame: premultiplied RGBA8, row-major, `width * 4` stride.
    pub canvases: HashMap<NodeId, CanvasFrame>,
    /// Each `<img>`'s (and CSS `background-image: url()`'s, `content: url()`'s) bytes as fetched,
    /// for the drawer to decode; keyed by the resolved URL.
    pub images: HashMap<String, ImageBytes>,
}
#[derive(Clone)]
pub struct CanvasFrame { pub width: u32, pub height: u32, pub generation: u64, pub pixels: Arc<[u8]> }
#[derive(Clone)]
pub struct ImageBytes { pub mime: Option<String>, pub bytes: Arc<[u8]>, pub natural: (u32, u32) }
```

- canvas 提交新帧时把该节点放进 `Changes.nodes`（因而进入它所在分区的 `Changes.partitions`）。
- `images` 只保留当前文档仍引用的 URL（节点移除/`src` 改变后在下次 `update()` 清理）。CSS `url()` 图片的字节由 P20 取回后放入
  （P20 负责"样式里出现 `url()` 时去取"——在 P20 的「给其他包的备注」中补充）。

### 图片（`canvas/image.rs`）

```rust
/// Reads only the header: what P20 calls as bytes arrive.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)>;
/// Full decode to premultiplied RGBA8 (first frame of an animation), honouring EXIF orientation
/// as `image-orientation: from-image` does.
pub fn decode(bytes: &[u8], mime: Option<&str>) -> Result<DecodedImage, DecodeError>;
pub struct DecodedImage { pub width: u32, pub height: u32, pub pixels: Arc<[u8]> }
```

- `image` crate `default-features = false`，只开 `png`、`jpeg`、`gif`、`webp`、`ico`、`bmp`（BMP 体积很小，Windows 剪贴板/图标常见）。
  SVG 图片（`<img src="x.svg">`）：渲染器只需尺寸（读 `<svg width/height/viewBox>`），像素交给 GPUI（它用 resvg 渲染 SVG）；
  `drawImage(svgImg)` 需要像素时：v1 不支持（画透明，console 警告一次），登记到 P41。AVIF：不支持（登记）。
- `HTMLImageElement`：`naturalWidth/naturalHeight`（P20 加载后写入节点状态）、`complete`（无 `src` 或已加载/已失败为 true）、
  `currentSrc`、`decode()`（已加载则在后台线程解码并缓存后 resolve，失败 reject `EncodingError`）、`width/height` 属性在无属性时
  返回布局宽高（来自 `State.rects`，未布局时用 natural 尺寸）。
- `new Image()` 设置 `src` 时触发加载（经 P20 的 Loader）。

### 2D 上下文（`canvas/context2d.rs`，替换 `bind/platform.rs` 的桩）

- 内部数据：`Canvas2d { pixmap: tiny_skia::Pixmap, state_stack: Vec<DrawState>, path: PathBuilder, generation }`，挂在 canvas 节点的 Rust 状态上
  （`State` 新增 `canvases: HashMap<NodeId, Canvas2d>`），JS 上下文对象只持有节点 id；`canvas.width/height` 赋值重建 pixmap 并清空状态
  （即使值相同——规范如此）。最大尺寸 16384×16384、总像素 ≤ 268M，超出则上下文失效（与 Chrome 一致：绘制无效、`toDataURL` 返回 `"data:,"`）。
- 必须实现（逐项对照 HTML 规范 canvas 章节）：
  - 状态：`save/restore/reset`、`globalAlpha`、`globalCompositeOperation`（全部 26 种，映射 tiny-skia `BlendMode`）、
    `imageSmoothingEnabled/Quality`、`filter`（v1 只支持 `none` 与 `blur(px)`，其余忽略并保持属性值）；
  - 变换：`translate/rotate/scale/transform/setTransform(含 DOMMatrix 参数)/getTransform/resetTransform`；
  - 样式：`fillStyle/strokeStyle` 接受颜色（用 `css::color`，P22 扩展后自动支持 oklch 等）、`CanvasGradient`（linear/radial/conic，
    `addColorStop`）、`CanvasPattern`（`createPattern(image|canvas|ImageBitmap, repetition)`、`setTransform`）；
  - 线：`lineWidth/lineCap/lineJoin/miterLimit/setLineDash/getLineDash/lineDashOffset`；
  - 路径：`beginPath/closePath/moveTo/lineTo/bezierCurveTo/quadraticCurveTo/arc/arcTo/ellipse/rect/roundRect`、`fill(fillRule)`、
    `stroke`、`clip(fillRule)`、`isPointInPath/isPointInStroke`（替换现在恒为 false 的实现）；`Path2D`（构造自另一个 Path2D 或
    **SVG path 字符串**、`addPath(path, matrix)`），所有路径方法也在 `Path2D` 上；
  - 矩形：`fillRect/strokeRect/clearRect`；
  - 阴影：`shadowColor/shadowBlur/shadowOffsetX/Y`（tiny-skia 无模糊：用三次盒式模糊近似高斯，σ = blur/2）；
  - 文字：`font`（完整 CSS font 简写解析，复用 `css.rs` 的 `font_longhands`）、`textAlign/textBaseline/direction/letterSpacing/wordSpacing/
    fontKerning/fontStretch/fontVariantCaps/textRendering`、`fillText/strokeText(maxWidth)`、`measureText` 返回完整 `TextMetrics`
    （`width`、`actualBoundingBox*`、`fontBoundingBox*`、`emHeight*`、`*Baseline`，全部来自真实字体度量）；
  - 图像：`drawImage`（3/5/9 参数；源为 `HTMLImageElement`（已加载）、`HTMLCanvasElement`、`ImageBitmap`、`OffscreenCanvas`；
    `<video>` 源 v1 画透明并登记）、`createImageData/getImageData/putImageData`（`ImageData` 是**非预乘** RGBA，与 pixmap 的预乘格式互转；
    `getImageData` 的 `colorSpace`/`willReadFrequently` 选项接受并忽略）；
  - 其他：`getContextAttributes`（`alpha:false` 时底色为不透明黑）、`canvas` 反向引用、`isContextLost() === false`。
- 跨域污染（tainted canvas）：Electron 默认 `webSecurity: true`，跨源图片画上去后 `getImageData/toDataURL` 抛 `SecurityError`；
  `file://` 与自定义特权方案（P16 `registerSchemesAsPrivileged` 的 `corsEnabled`）视为同源。v1 实现"来源是否同源"的标记位。
- 编码导出：`toDataURL(type, quality)` / `toBlob(cb, type, quality)`：`image/png`（默认）、`image/jpeg`（quality 默认 0.92）；
  `image/webp` 不支持有损编码时按规范**回退为 PNG**。Blob 用 Verve 的 `Blob` 构造（现有代码已如此）。
- `getContext` 的其他类型：`'webgl'/'webgl2'/'webgpu'` 返回 `null`（保持）；`'bitmaprenderer'` 实现（`transferFromImageBitmap`）。

### ImageBitmap / OffscreenCanvas

- `createImageBitmap(source[, sx, sy, sw, sh][, options])`：源支持 `HTMLImageElement`、`HTMLCanvasElement`、`ImageData`、`Blob`、`ImageBitmap`、
  `OffscreenCanvas`；选项 `imageOrientation`、`premultiplyAlpha`、`colorSpaceConversion`（忽略）、`resizeWidth/Height/Quality`。
  `Blob` 源在后台线程解码，完成后作为任务回到渲染器线程 resolve。`ImageBitmap.close()`、`width/height`。
- `new OffscreenCanvas(w, h)`：`getContext('2d')`（同一实现）、`convertToBlob({type, quality})`、`transferToImageBitmap()`。
  **v1 只在主线程**（页面 realm）；`transferControlToOffscreen()` 与在 Worker 中使用 OffscreenCanvas 抛 `NotSupportedError` 并登记（Worker realm 没有 verve-dom）。

### 字体（`canvas/fonts.rs`）

```rust
pub struct FontStack { /* fontdb::Database + 已加载的 @font-face */ }
impl FontStack {
    pub fn system() -> Self;                                     // 惰性扫描系统字体（首次使用 canvas 文字时，不在启动时）
    pub fn add_web_font(&mut self, family: &str, descriptors: &FontFaceRule, bytes: Arc<[u8]>);  // P20/P23 调用
    pub fn resolve(&self, families: &[&str], weight: u16, style: Style, ch: char) -> Option<FaceId>;  // 含逐字回退
}
/// Generic family → concrete families, per platform; shared with P31 so canvas and DOM text agree.
pub fn generic_families(generic: &str) -> &'static [&'static str];
```

## 步骤

1. 依赖与 feature：`tiny-skia`、`image`（与 GPUI 同版本、最小 features）、`fontdb`、`rustybuzz`、`skrifa`；feature `canvas`。用 P01 的脚本记录体积增量。
2. `canvas/image.rs`：`image_size`、`decode`、解码缓存；`HTMLImageElement` 访问器与 `decode()`；`SnapshotData.images`。
3. `canvas/context2d.rs` + `path.rs`：状态、变换、路径、填充描边、裁剪、合成、渐变图案、阴影、`get/putImageData`；替换 `bind/platform.rs` 中的桩
   （删除估算宽度的 `text_width`/`font_size`，`BLANK_PNG` 只保留给尺寸为 0 的 canvas）。
4. `canvas/text.rs` + `fonts.rs`：字体栈、整形、`fillText/strokeText/measureText`。
5. 帧末提交与 `SnapshotData.canvases`；`Changes` 标记。
6. 导出编码、`ImageBitmap`、`OffscreenCanvas`、`bitmaprenderer`、污染标记。
7. 像素级测试与真实库测试。

## 验收

- `cargo test -p verve-dom`：现有全部通过；新增 `tests/canvas.rs`：
  - **参考图对比**：20 个绘制用例（矩形、圆弧、贝塞尔、描边线帽线连接、虚线、线性/径向/圆锥渐变、图案、裁剪、全部合成模式中的 6 个、
    阴影、变换、`drawImage` 9 参数、`putImageData`、文字左/中/右对齐与各基线），参考 PNG 由 Chrome 生成并提交在
    `crates/verve-dom/tests/fixtures/canvas/`（附生成用的 HTML）；比较允许每通道 ±8 的差异且差异像素 < 1%（文字用例放宽到 < 5%）；
  - `getImageData` 与 `putImageData` 往返（非预乘 ↔ 预乘，半透明像素误差 ≤ 1）；
  - `toDataURL()` 能被 `decode` 解回相同尺寸与像素；`toDataURL('image/webp')` 回退为 PNG；
  - `measureText('Hello').width` 与 Chrome 在同一系统字体下的值相差 < 1px（Windows 上用 Segoe UI/Arial 测；结果冻结）；
  - 跨源图片污染后 `getImageData` 抛 `SecurityError`；
  - 提交语义：同一帧内多次绘制只产生一次新 `generation`；未绘制的帧不产生新帧；
  - `<img>`：加载 PNG/JPEG/GIF/WebP/ICO 后 `naturalWidth/Height` 正确、`decode()` resolve、损坏数据 reject。
- **真实库**（`#[ignore]`，`verve::npm` 取包）：Chart.js 4 画一个柱状图，`toDataURL` 结果与 Chrome 参考图对比（宽松阈值）；
  xterm.js 的 canvas 字符宽度测量（`measureText('W')`）与 Chrome 一致。
- 性能（`#[ignore]` 基准）：1920×1080 canvas 每帧 `clearRect` + 1000 个 `fillRect` + 提交 ≤ 8 ms（release）；结果写入「状态」节。
- 体积：本包新增依赖的 release 体积增量写入「状态」节（目标 ≤ 2.5 MB，含 `image` 与 GPUI 共享部分不重复计）。

## 不做

WebGL/WebGL2/WebGPU；Worker 中的 OffscreenCanvas；`<video>` 作为 `drawImage` 源；SVG 图片的像素（交给 GPUI 显示，canvas 中不支持）；
AVIF/HEIC/JPEG XL；`filter` 除 `blur()` 外的函数；`drawFocusIfNeeded`/`scrollPathIntoView`；色彩管理（统一按 sRGB）；`captureStream`（保持桩）。

## 给其他包的备注

- **P20**：调用 `image_size`；图片字节写入 `SnapshotData.images`（键为解析后的 URL）；CSS 中的 `url()`（`background-image`、`content`、
  `list-style-image`、`border-image`、`mask`）也需要取回并放入 `images`——请在 P20 范围内处理"样式中出现的 url() 资源"。
  `@font-face` 字节同时交给本包的 `FontStack::add_web_font`。
- **P35**：读 `SnapshotData.canvases`（预乘 RGBA8，按 `generation` 判断是否需要重新上传纹理）和 `SnapshotData.images`（原始字节交给 GPUI 解码；
  动画 GIF 由 GPUI 处理）。GPUI 的 `RenderImage` 需要 BGRA：转换在 P35 做。
- **P31**：使用本包 `generic_families` 同一张通用字体族映射表。
- **P01**：feature `canvas` 可关闭以测量最小体积。

## 状态

（实现者完成后填写）
