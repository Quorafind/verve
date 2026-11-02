# P35 图片 / SVG / canvas 表面

**依赖**：P30 P25（P20 的加载器提供字节）  **规模**：M  **修改**：`crates/verve-layout/src/replaced.rs`（新）、`images.rs`（新）；`crates/verve-gpui/src/images.rs`（新）、`dom_element.rs`；`crates/verve-electron/src/renderer_guest.rs`（图片/媒体表）

## 目标

把"替换元素"和图片类内容画出来：`<img>`、CSS `background-image: url()`、`list-style-image`、`<picture>/srcset` 的选择结果、
内联 `<svg>`、`<canvas>`（2D，像素由 P25 产出）、`<video>` 的当前帧（v1 有限支持，见下）。同时给布局提供固有尺寸
（`naturalWidth/naturalHeight`、`object-fit`、`aspect-ratio`）。

## 背景（先读）

- P30（`PaintItem::Image { node, bounds, clip, image: ImageKey, fit }`、`Fill::Raster`）、P25（canvas 2D 像素缓冲与图片解码）、
  P20（加载器：`file://`、自定义协议、`http(s)`、`data:`、`blob:` → 字节）。
- verve-dom：`Snapshot.media: HashMap<NodeId, MediaState>`、`MediaCommand`/`MediaCommandKind`（`Load/Play/Pause/Seek/…/Dispose`）、
  `MediaUpdate`（`Metadata/FrameReady/Playing/…`）、`media.rs` 顶部说明（"GUI 无关的媒体协议，宿主负责解码器、时钟与像素"）。
- GPUI：`elements/img.rs`（`img()`、`ImageSource::{Resource, Render(Arc<RenderImage>), Image(Arc<Image>), Custom}`、`ImageStyle`）、
  `assets.rs`（`RenderImage`：BGRA 帧，`RenderImage::new(frames)`）、`style.rs` 的 `ObjectFit::{Fill, Contain, Cover, ScaleDown, None}`、
  `window.rs` 的 `paint_image(bounds, corner_radii, data: Arc<RenderImage>, frame_index, grayscale)`、`elements/svg.rs`（`svg().path()`、
  `with_transformation(Transformation)`）、`svg_renderer.rs`（GPUI 自带 usvg/resvg 栅格化）、`elements/surface.rs`（macOS `CVPixelBuffer` 视频表面）。

## 设计

### 图片管线

```
渲染器线程                                                 GPUI 线程
<img src> / background-image / srcset 选中的 URL
  → P20 Loader 取字节（带缓存，按 URL）
  → 解码（P25 的 decode：png/jpeg/gif/webp/ico/bmp；svg 走下面的 SVG 路径）
     ├─ 只读头部拿尺寸：立刻给 naturalWidth/Height，触发布局
     └─ 全量解码成 RGBA → 转 BGRA（GPUI 要求）→ Arc<ImageFrames>
  → ImageTable: ImageKey → Arc<ImageFrames>（引用计数，节点删除时释放）
  → 'load' / 'error' 事件、img.decode() resolve、complete=true
DisplayList: PaintItem::Image { image: ImageKey, … }
RendererView 追加 images: Arc<ImageTable 快照>  ─────────▶  ImageKey → Arc<gpui::RenderImage>（首次见到时构造，缓存）
                                                           window.paint_image(bounds, radii, render_image, frame, false)
```

- 解码在渲染器线程会阻塞脚本：大图（> 1 MP）放到 Verve 已有的后台工作线程池（或新建一个固定 1 线程的解码线程，**不按图片开线程**），
  完成后作为任务回到渲染器线程。`img.decoding = 'sync'` 也异步解码（Chromium 也不保证同步）。
- `RenderImage` 只在 GPUI 线程构造（它带 GPUI 的 `ImageId`），同一 `ImageKey` 只构造一次；`ImageKey` 不再出现在任何显示列表 3 帧后丢弃，
  并调用 `window.drop_image` / `cx.drop_image`（GPUI 释放图集纹理；按实际 API 名）。
- 动图 GIF/APNG/WebP：`ImageFrames` 带每帧时长；渲染器按帧时钟（P33 `Frame`）推进当前帧号，放进 `PaintItem::Image.frame`（追加字段），
  页面不可见时停止推进。
- 内存上限：解码后像素总量超过 256 MB 时按最近最少使用淘汰不可见图片（再次可见时重新解码）。

### 布局（固有尺寸与 object-fit）

- 替换元素在 `verve-layout` 里是原子盒，taffy 测量函数返回固有尺寸；`width/height` 属性作为表现提示（`aspect-ratio: auto w / h`，避免加载前跳动）。
- 未加载/加载失败：0×0（有 `alt` 时 Chromium 显示替代文字与破图图标，v1：失败时若有 `alt` 按行内文本排 `alt`）。
- `object-fit`：`fill/contain/cover/scale-down/none` → GPUI `ObjectFit` 同名（`none` 时按固有尺寸）；`object-position`：GPUI 不支持，
  由本包自己算目标矩形 + `PushClip` 裁剪，再用 `ObjectFit::Fill` 画到算好的矩形（所以统一自己算，不依赖 GPUI 的 fit）。
- `image-rendering: pixelated` → 最近邻：GPUI 不支持采样方式选择，v1 在解码阶段按显示尺寸整数倍放大预缩放（只在放大倍数 ≥ 2 时），记录偏差。
- `srcset`/`sizes`/`<picture><source media type>`：按 `devicePixelRatio` 和布局宽度选源（选择逻辑放 P20 或本包，**本包负责**，因为需要布局宽度）。

### background-image

- `url()` 层：与 `<img>` 同一管线。`background-size: cover/contain/<len>`、`background-position`、`background-repeat`（`repeat` 时按瓦片
  多次 `paint_image`，裁剪到盒子；瓦片数 > 500 时先在 CPU 拼一张大图）、`background-clip: border-box/padding-box/content-box`、多层背景（按 CSS 顺序，最后一层在最下）。
- 渐变层见 P30（`Fill::Linear` / `Fill::Raster`）。

### 内联 `<svg>`

- 布局：`<svg>` 是替换元素，尺寸取 `width/height` 属性或 CSS，否则按 `viewBox` 比例，默认 300×150。
- 绘制：把 `<svg>` 子树（含 `<use>` 引用、`<style>`、`currentColor` 解析为计算后的 `color`、`fill/stroke` 等计算样式）序列化为 SVG 文本，
  用 usvg 解析 + resvg 栅格化成 `ImageFrames`（GPUI 已经依赖 resvg/usvg —— 直接复用同一版本，**不重复引入**；确认 `cargo tree -d` 无重复），
  按 (序列化文本哈希, 设备像素尺寸) 缓存。子树变化（`Changes.nodes` 包含其子孙）时重新序列化。
- 图标类小 SVG（≤ 64px，单色，大量重复，如 lucide/Material 图标）：同一文本哈希只栅格化一次，颜色在序列化时已固定。
- `<img src="x.svg">` 与 `background-image: url(x.svg)`：同样用 resvg 按显示尺寸栅格化（缩放后重新栅格化，不拉伸位图）。
- SVG 元素的 DOM 几何：`getBBox()`、`getBoundingClientRect()` 用 usvg 树算出的包围盒回填（通过 P30 的布局提供者），满足图表库（ECharts/d3 svg 模式）的测量。
- 动画（SMIL）、`<foreignObject>`：不支持。

### canvas 2D 表面

- P25 在渲染器线程维护每个 `<canvas>` 的像素缓冲（tiny-skia `Pixmap`，尺寸 = `canvas.width × canvas.height`）。每次绘制命令后标脏。
- 帧边界（rAF 回调后或任务结束时）若脏：把缓冲转 BGRA，生成新的 `ImageKey` 版本（同一 canvas 复用一个 key，带递增 `generation`），
  GPUI 侧见到 generation 变化时重建 `RenderImage`（并释放旧的）。
- 布局：替换元素，固有尺寸 = `width/height` 属性（CSS 像素）；CSS 尺寸不同时缩放显示。
- `toDataURL/toBlob/getImageData` 由 P25 直接读缓冲，与本包无关。
- 性能：1920×1080 的 canvas 每帧全量上传约 8 MB，60 fps 下可接受但浪费；v1 记录脏矩形，P33 之后可考虑局部上传（GPUI 不支持局部更新纹理，需 fork 改动，暂不做）。

### `<video>` / `<audio>`（v1 范围）

- verve-dom 已有 GUI 无关的媒体协议：渲染器把 `MediaCommand` 交给宿主，宿主回 `MediaUpdate`。本包实现一个**最小宿主**：
  - `<audio>`：v1 不出声（不引入音频解码与输出栈，体积考虑），`Load` 后回 `MediaUpdate::Error { code: 4 /* MEDIA_ERR_SRC_NOT_SUPPORTED */ }`。
  - `<video>`：同样回不支持；`poster` 属性的图片按 `<img>` 画。
  - 预留接口：`trait MediaBackend { fn handle(&mut self, cmd: MediaCommand) -> Vec<MediaUpdate>; fn current_frame(&self, node: NodeId) -> Option<Arc<ImageFrames>> }`，
    后续可接平台解码（Windows Media Foundation / macOS AVFoundation，经 GPUI `surface()` 在 macOS 零拷贝），作为可选 feature，不进默认 20 MB 预算。
- 在 P41 兼容矩阵中登记"音视频不支持"。

## 接口契约

```rust
// verve-layout / verve-electron 共用
#[derive(Copy, Clone, Hash, Eq, PartialEq)] pub struct ImageKey { pub id: u64, pub generation: u32 }
pub struct ImageFrames { pub width: u32, pub height: u32, pub frames: Vec<(Arc<[u8]> /* BGRA */, Duration)> }
pub struct ImageTable { /* ImageKey -> Arc<ImageFrames>，不可变快照，Arc 共享 */ }

// RendererView 追加（P02 契约追加，本包第一个 commit 只改文档与 msg.rs）
pub images: Arc<ImageTable>,
// PaintItem::Image 追加字段
frame: u32,
```

## 步骤

1. `ImageTable` 与 `RendererView.images`；GPUI 侧 `ImageKey → RenderImage` 缓存与释放。
2. `<img>`：加载、头部尺寸、`load/error` 事件、`decode()`、`complete`、`naturalWidth`；布局固有尺寸；`object-fit/position`。
3. `background-image` 多层、平铺、定位、尺寸。
4. 内联 SVG 序列化 + resvg 栅格化 + 缓存；`getBBox`。
5. canvas 表面（与 P25 对接）。
6. GIF 动画帧推进。
7. 媒体最小宿主（不支持）与 `poster`。

## 验收

- `cargo test -p verve-electron --test images`（无头）：`data:` PNG 的 `<img>` 触发 `load`，`naturalWidth` 正确；坏图触发 `error`；
  `img.decode()` resolve；`object-fit: cover` 的 `Image` 项目标矩形与裁剪矩形符合预期；`<canvas>` 画一个红色矩形后显示列表出现对应 `ImageKey`，
  generation 递增；内联 SVG 的 `getBBox()` 宽高正确；`<video>` 触发 `error` 且 `error.code === 4`。
- 夹具 `tests/electron/apps/images/`：PNG/JPEG/GIF 动图/WebP/SVG 图标网格/背景平铺/canvas 时钟动画；截图与真实 Electron 对比差异像素 < 3%（动画帧除外）。
- 内存：加载 200 张 1 MP 图片的滚动页面，进程工作集 < 400 MB，滚出视口后回落。

## 不做

音视频播放、WebGL/`OffscreenCanvas` 的 GPU 路径、SVG SMIL 动画与 `<foreignObject>`、`filter` 作用于图片、CSS `image-set()`（可映射到 srcset，可选）、
`border-image`、`mask-image`、颜色管理（ICC 配置文件忽略，按 sRGB）。

## 状态

（实现者完成后填写）
