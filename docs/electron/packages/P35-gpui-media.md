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
// verve-layout（渲染器与 Shell 共用）
// ImageKey 在 P30 里已经是 `ImageKey(pub u64)`，没有单独的 generation 字段，也不加：
//   最高两位：`1x` = 渐变栅格（P30：散列 | 1<<63，第 62 位随意）；`01` = canvas 表面（第二阶段，generation 编进低位）；
//   `00` = 页面图片地址（已解析的 URL）的 62 位散列（`key_for_url`）。同一地址永远是同一个键；
//   两个地址有 2^-62 的概率撞键，撞了就显示同一张图（不处理）。
pub struct ImageFrames { pub width: u32, pub height: u32, pub frames: Vec<(Arc<[u8]> /* straight-alpha BGRA */, Duration)> }
pub struct ImageTable { /* ImageKey -> Arc<ImageFrames>，不可变快照；渲染器每次有新图就换一个新的 Arc */ }

// RendererView 追加（P02 契约追加表里 P35 那一行）
pub images: Arc<ImageTable>,
// PaintItem::Image 追加字段；bounds 是整张图（已按 object-fit/position 算好）要画进的矩形，
// clip 是可见范围（内容盒），fit 只记录样式里的关键字，Shell 不再用它
frame: u32,
// Fill 追加一种：背景图层（`background-image: url()`）。area/tile 都相对盒子的 border box 左上角
Fill::Picture { image: ImageKey, area: Rect, tile: Rect, step: [f32; 2], count: [u32; 2], frame: u32 }
// Snapshot 追加（verve-dom）：
//   image_sources: HashMap<NodeId, (String, f32)>   每个 <img> 现在显示的地址与像素密度（srcset 选出的）
//   base_url: String                                 解析 CSS url() 用的文档基地址
```

- **解码**：渲染器里有一个固定的解码线程（`ImageDecoder`），显示列表点名的、`Snapshot.images` 里有字节的地址交给它，结果回到页面线程后进 `ImageTable`；
  解码线程每完成一张就用 `verve::Waker` 叫醒页面线程（没有 waker 时才退回 `next_due` 的 5 ms 轮询）。显示列表不再点名的图片，其像素从表里退出。
  **所有**图片都在这个线程上解码（任务书原写"超过 1 MP 才放后台"，统一了更简单）；头部给出的像素数超过 64 MP 的图不解码。
- **画哪一帧**：Shell 只画 `frame`；第一阶段恒为 0，动图见第二阶段。
- **第一阶段的偏差**：`<img>` 的 `border-radius` 不裁图（`PaintItem::Image` 没有 `radii`）；每层最多 4096 块瓦片，超出的部分不画（`space`/`round` 都实现了）。

### 第二阶段的契约（SVG、canvas 表面、动图）

- **canvas 的键**：最高两位 `01`；第 61–32 位是 canvas 节点编号的低 30 位（槽位）；第 31–0 位是 `CanvasFrame.generation` 的低 32 位。
  `canvas_key(node, generation)`、`canvas_slot(key)`（在 `verve-layout/src/images.rs`）。布局从 `Snapshot.canvases` 读出当前 generation 写进
  `PaintItem::Image.image`；渲染器只在表里放**当前**键的画面（`CanvasFrame` 的预乘 RGBA 转成 straight-alpha BGRA），旧 generation 的键随之离开表；
  Shell 画到一个新 generation 时，立刻让同一槽位的旧 `RenderImage` 退役（不等 300 帧的宽限）。显示列表没点名的 canvas 不转换。
- **矢量图**：`DisplayList.vectors: Vec<VectorSpec { key: ImageKey, source: Arc<[u8]> }>` 追加在末尾，与 `rasters` 对称：布局在显示列表里放 SVG 文本，
  Shell 在 GPUI 线程上用 GPUI 已链接的 resvg 按设备像素尺寸栅格化。内联 `<svg>` 的键是序列化文本（含内容盒的 `width`/`height`）的 62 位散列，`00` 开头，
  所以文本不变就是同一个键；`<img src=x.svg>` 和背景的键仍是 `key_for_url`，`source` 是文档持有的字节（`Arc` 克隆，不复制）。
  Shell 的缓存键是 `(ImageKey, 设备像素宽, 设备像素高)`，尺寸变了就重新栅格化，不拉伸位图；一帧没画到的栅格 60 帧后还给图集。
  `<img src=x.svg>` 和背景里的 SVG 文本由渲染器放进 `ImageTable::vector(key)`（`PageImages` 对 SVG 字节不解码，直接把文本交给表；`ImageTable::with_vectors`），
  Shell 先找显示列表的 `vectors`，再找表。resvg/usvg 与 GPUI 同版本同特性（0.46.0，`text` `system-fonts` `memmap-fonts` `raster-images`），由 `verve-layout` 直接依赖，`cargo tree -d` 不出现第二份。
- **动图**：`ImageFrames.frames` 每帧带时长，另有 `loops: u32`（0 = 无限；GIF 读 NETSCAPE2.0 扩展，没有这个扩展就播一遍；`ImageFrames` 追加字段）。
  画哪一帧由渲染器定：`PageImages::animate` 为显示列表点名且多于一帧的图片各记一个起点，按已过的时间算出帧号，**改写显示列表里的 `frame`**（新 `revision`）再发布。
  页面何时醒来：`RendererGuest::next_due` 返回到下一次换帧的剩余时间（一个定时器，不是 5 ms 轮询）。显示列表不再点名的图片停止计时，再次点名时从第 0 帧重新开始。
  时长不超过 10 ms 的帧按 100 ms 算（Chromium 源码 `gif_image_decoder.cc` 的做法；本包的观察见「状态」）。
- **SVG 的几何**：`verve_dom::Layout` 追加 `svg_boxes: Vec<(NodeId, Rect)>`，是 usvg 树里每个有 `id` 的元素在 `<svg>` 内容盒坐标里的包围盒（序列化时给没有 `id` 的元素补一个）。
  文档的 `getBBox()` 读这张表；布局提供者还把这些盒子平移到页面坐标，放进 `rects`，`getBoundingClientRect()` 读到的就是它们。

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

### 第一阶段（步骤 1–3、7）

**做了**：

- **契约**（第一个提交，见「接口契约」）：`ImageKey` 仍是 `ImageKey(pub u64)`（P30 已定义，没有 generation 字段；canvas 的 generation 第二阶段编进低位，第 62 位留给 canvas 表面）；页面图片的键是已解析 URL 的 62 位散列 `key_for_url`。`ImageFrames`/`ImageTable`（`verve-layout/src/images.rs`）、`RendererView.images`、`PaintItem::Image.frame`、`Fill::Picture`、`Snapshot.image_sources`/`base_url`。
- **verve-dom**：
  - `canvas/sources.rs`：`srcset`（`x`/`w` 描述符）、`sizes`（媒体条件、`calc()`、`vw`）和 `<picture><source media type>` 的选择；`img.rs::sync` 改用它，`currentSrc`/`naturalWidth` 随之（自然尺寸 = 像素 ÷ 密度，向下取整）。
  - 样式里的图片：`background-image`/`list-style-image` 的 `url()` 和 `<video poster>` 在帧里被发现（`sync_images` 扫描改动/重算样式的节点），作为 `Request::Image` 交给加载器，只请求一次；字节到达或失败时 `touch()`，布局重做（`set_image_bytes` 原来漏了，评审后补上，`tests/images.rs` 有一条经 127.0.0.1 的 http 取图的测试）；背景图和 `<img>` 共用一个地址时，字节按 `Keep::Always` 保留（`<img>` 不再引用也不丢）。`poster=""` 不请求。
  - `background` 简写现在拆成逐层的 `background-image/-position/-size/-repeat/-origin/-clip`（原来整条文字塞进 `background-image`），并且总是写全部长属性（`background:red` 会把图片、位置、尺寸、重复复位；CSS 全局关键字抄进每个长属性，含 `var()` 的值整条保留）。
  - UA 样式表加了 `video { object-fit: contain }` 和 `audio:not([controls]) { display: none }`（Electron 39.8.10 观察到 `<audio>` 无 `controls` 时矩形为 0×0）。
- **verve-layout**：`<img>` 的固有尺寸（图片到了就是图片的尺寸；只给一边的属性时按比例；加载前 `width`/`height` 属性同时给出时作为宽高比提示）；尺寸按 `props.rs::Follow` 分三种，由 `layout.rs::scale_ratio_heights` 在第一遍之后补：给了宽（样式或属性）没给高，高 = 实际宽 ÷ 比例；给了高没给宽，宽 = 实际高 × 比例；都没给，用图片的尺寸并按 CSS 2.1 10.4 的表被两边的 `min-`/`max-` 限制（`constrain`，不再用 taffy 的 `aspect_ratio`，它会把一边的限制带到另一边，而 Chromium 对给定的那一边不这样做）；样式里的 `width`/`height`（包括 `auto`）压过属性；`<video>` 没有尺寸时按 `poster` 的尺寸（40×20 的 poster 得 40×20，Electron 39.8.10 观察到）；`object-fit`/`object-position` 自己算目标矩形，`PaintItem::Image.bounds` 是整张图的矩形、`clip` 是内容盒（不用 `PushClip`）；`background-image: url()` 多层、`size`（`cover/contain/长度/auto`）、`position`（含四值边偏移）、`repeat`（`repeat/no-repeat/repeat-x/-y/space/round`）、`origin`/`clip`，输出 `Fill::Picture`，每层最多 4096 块瓦片。
- **verve-electron**：`page_images.rs`——**一个**固定的解码线程（`verve-image-decoder`），显示列表点名且 `Snapshot.images` 里有字节的地址交给它（同一地址换了字节就重新解码），结果在页面线程汇成新的 `Arc<ImageTable>`（straight-alpha BGRA，用 P25 的 `decode`/`unpremultiply`）；解码线程用 `verve::Waker` 叫醒页面（没有 waker 才 5 ms 轮询）；头部像素数超过 64 MP 的图解码前就放弃；解码 panic 被 `catch_unwind` 接住，线程死了就复位 `pending` 并在下一个任务时重启；列表不再点名或文档不再持有的地址退出表；每个地址的键按字节分配的地址缓存，不在每帧对（可能几 MB 的）`data:` 地址做散列。`media_host.rs`：`MediaBackend` trait 和 `Unsupported`——`Load` 回 `Error { code: 4 }`，`Play` 回 `CommandFailed`，其余不回；`RendererGuest::drain` 里 `serve_media()`。`HeadlessShell::display_list(window)` 留下页面最后发布的显示列表和图片表供测试读。
- **verve-gpui**：`pictures.rs` 扩展（没有第二套机制）：`Pictures::set_table/image`，页面图片按键缓存成带全部帧的 `RenderImage`，表里换了新的 `ImageFrames`（`Arc` 指针不同）就重做并让旧的退役，没画的帧末放弃、下一帧还给图集；不透明度小于 1 时同渐变一样用 alpha 副本。`plan.rs`：`Shape::Picture`（`PaintItem::Image` 和 `Fill::Picture` 的每块瓦片）；`dom_element.rs` 用 `paint_image(visible, bounds, radii, …)`（GPUI 的 fork 里前一个矩形是可见范围，后一个是整张图的矩形）；`screenshot.rs` 用 tiny-skia `Pattern` 画（`render_with`、`png`/`write` 多了 `images` 参数；`render` 不变）。`Look` 多了第四项 `Arc<ImageTable>`，`Window::show_images`，`DomView::show_images`。

**Electron 39.8.10 的观察**（`show: false`、800×600、`--force-device-scale-factor` 1/2/3，scratch profile；脚本在草稿目录 `p35/obs/`）：

- `<img>` 加载失败：无 `alt`、无尺寸属性 → 16×16（破图图标）；有非空 `alt` → 一个 `16 + 文字宽` × 21 的行内盒，**忽略** `width`/`height` 属性；`alt=""` 或没有 `src` 且无 `alt` → 0×0；失败且只有 `width="100" height="50"` → 100×50。
- 4×2 的图：无属性 → 4×2；`width=100` → 100×50；`height=20` → 40×20；`max-width:2px` → 2×1；`width=100 height=100` → 100×100；`width:50%`（800 宽）→ 400×200（比例保持）。
- 无属性的 `<svg>`、`<canvas>`、`<video>` 是 300×150；`<svg viewBox="0 0 40 20">` 无尺寸时宽是容器宽、高按 2:1；`<svg width=50>` 是 50×150；`<audio>` 无 `controls` 是 0×0，有 `controls` 是 300×54；`<video src>` 指向不存在的文件：`error.code === 4`，`networkState` 3；没有 `src` 时 `error` 是 `null`。
- `object-fit`/`object-position`：用 `capturePage()` 量红色像素的包围盒（40×20 与 200×100 的图在 100×100 的盒里，全部组合）。**`object-position` 的长度对 `fill` 和 `cover` 也生效**（`fill` + `10px 20px` 的可见范围是 `[10,20,90,80]`）；`25% 75%` 的 `contain` 得 y=37.5，截图量得 38。DPR 2 时包围盒恰好翻倍。表在 `replaced.rs` 的测试里。
- `srcset`：**经 http 取图**时按规范选（DPR 1：`1x,2x,3x` → 1x；`1.5x,2.5x` → 1.5x；DPR 2 → 2x/2.5x；`src` 是 1x 候选；`150px` 的 `sizes` 在 DPR 1 选 200w、在 DPR 2 选 300w；`sizes` 的媒体条件按窗口宽度；`calc(50vw - 100px)`；无 `sizes` 是 `100vw`）。`naturalWidth` = 像素 ÷ 密度并向下取整（10÷1.5 → 6，而 `width` 是 7）。**图片用 `data:` URL 时，Chromium 总是选最大的候选**（与 DPR 无关），所以测试里的选择只在 `sources.rs` 的单元测试里按规范断言。`<picture>`：媒体不匹配的 `source` 被跳过，`type="image/webp"`/`image/png` 的被采用，不认识的 `type` 被跳过，`<source media="(min-width: 700px)">` 在 800 宽时被采用。

**没做 / 偏差**：

- **破图盒与 `alt` 文字**：图片加载失败、没有尺寸属性时没有 16×16 的图标盒，`alt` 文字没有排进行内（现在是 0×0）；快照里没有"加载失败"的信息，要加一个字段才能区分"还在加载"和"失败"。上面观察到的数字都在，留给下一次。
- **`list-style-image`**：URL 会被请求和解码，但 `list-style` 的标记（`::marker`）本身还没有画图（P31 的标记是文字）。
- **`image-rendering: pixelated`**：未做（GPUI 没有采样选择；任务书里的整数倍预放大没实现）。
- **解码线程**：所有图片都在那一个线程上解码（任务书原写大于 1 MP 才放后台）；`Snapshot.images` 里的图片只要文档还持有就一直在表里，**没有 256 MB 的 LRU**（背景图 `Keep::Always` 永不放；`<img>` 的在没有元素引用后释放）。
- **SVG**（`<img src=x.svg>` 和背景）：布局有尺寸（头部读出），解码返回 `Unsupported`，所以不画；第二阶段。
- `background-attachment`、`background-clip: text` 忽略；圆角盒的背景图只有铺满整个盒子时才带圆角，而且每块瓦片只在碰到区域那个角的地方圆（GPUI 与软件绘制一致）；`<img>` 的 `border-radius` 不裁图（`PaintItem::Image` 没有 `radii`），Chromium 会裁；`background-size` 对渐变层不起作用（原来就如此）。样式里的相对 `url()` 按文档地址解析，不是按样式表地址。
- `<img>` 的 `srcset`/`<source>` 在视口或 DPR 变化后不会重选（只在该元素的属性变化时选）；`<source>` 本身的属性变化不会让 `<img>` 重选。
- `PaintItem::Image.frame` 恒为 0（动图是第二阶段）。
- **GPUI 的取整**：比圆角半径还小的瓦片在 GPUI 里近乎方形（GPUI 对每块单独取整并限制半径）；GPUI 把只露出一部分的小图的子矩形取整到整个纹素（`window.rs`），位置会有偏差；软件绘制是精确的。
- **每帧的地址拷贝**：`Snapshot` 的 `images`/`image_sources` 每帧还要复制每个图片地址（大的 `data:` 地址有几 MB），只改了布局和解码线程这一侧的散列，没改快照。
- **评审后留下的**：
  - 每层超过 4096 块瓦片时多出的部分是空的（如 1×1 的瓦片铺 1000×800）；任务书说的"CPU 预铺"没做（要给预铺的图合成一个新键）；
  - flex 容器里 `<img>` 的拉伸（Chromium 把 `<img width=40>` 在 100px 高的 flex 行里拉成 40×100，Verve 是 40×20）没做，也没有对应测试；
  - `::before`/`::after` 里的 `url()` 不会被请求（`sync_images` 只读元素的样式）；
  - P25 的 `decode` 先预乘再由 `still` 反预乘，低 alpha 的颜色会量化（还没有 straight-alpha 选项）；
  - `image_size` 不看 EXIF 方向而 `decode` 会转，带旋转的手机 JPEG 固有尺寸会宽高颠倒；
  - 布局每次读 `<img>` 仍要对地址做一次散列（`snapshot.images.get`），只在布局重做时发生，不在每帧；
  - 解码后的像素在 CPU 上有两份（`ImageTable` 与 `RenderImage`）；
  - 页面图片在 GPUI 里不画之后保留 300 帧才还给图集，来回滚动不会重新拷贝和上传；列表不点名但文档还持有字节的图片，解码好的像素在 128 MiB 的预算内保留（最久没被点名的先丢），隐藏再显示不会空白；
  - 历史：gpui 测试在 `feat(gpui): Draw pictures and background tiles…` 之后单独一个提交，video/audio 的 UA 规则在 `feat(layout)` 里（要重排提交）；两者都没改。
- 顺序：测试 `tests/images.rs` 的 `srcset` 一条用 `data:` URL，只验证 `src` 与 `srcset` 的配合（见观察）。

**任务书「修改」之外动过的文件**：`verve-dom` 的 `css/shorthand.rs`、`css/values.rs`（UA 样式）、`snapshot.rs`、`update.rs`、`canvas/{document,image,img,mod}.rs`；`verve-electron` 的 `headless.rs`、`lib.rs`、`msg/input.rs`；`verve-gpui` 的 `shell.rs`、`window.rs`、`capture.rs`、`dom_view.rs`、`plan.rs`；`verve-layout` 的 `props.rs`、`layout.rs`、`paint.rs`、`paint_style.rs`、`display_list.rs`、`lib.rs`。

**给其他包**：P24/P43 二期：`RendererView` 只在末尾多了 `images`；`Look` 多了第四项。P33：`Fill::Picture` 的瓦片在 `lower_tiles` 里展开，块缓存可以直接缓存 `Shape::Picture`。

**要跑的测试**（一次一条）：

```
cargo test --locked -p verve-dom canvas::sources
cargo test --locked -p verve-dom css::shorthand
cargo test --locked -p verve-dom --test css
cargo test --locked -p verve-dom --test canvas
cargo test --locked -p verve-dom --test media
cargo test --locked -p verve-layout
cargo test --locked -p verve-electron --lib page_images
cargo test --locked -p verve-electron --lib media_host
cargo test --locked -p verve-electron --test images
cargo test --locked -p verve-electron --test headless
cargo test --locked -p verve-electron --test geometry
cargo test --locked -p verve-gpui --features jit --lib
cargo test --locked -p verve-gpui --features jit --test boxes
cargo clippy --locked --workspace --all-targets -- -D warnings
```

### 第二阶段（步骤 4–6、夹具、内存）

本阶段的代码只做了 `cargo check`（agent 不跑测试）；下面列的测试都还没有跑过，由协调者跑。

**做了**：

- **契约**（第一个提交，见「第二阶段的契约」）：canvas 的键、`DisplayList.vectors`、`ImageTable::vector`、`ImageFrames.loops`、`Layout.svg_boxes`。
- **内联 `<svg>`（步骤 4）**：`verve-layout/src/svg.rs`。
  - 布局阶段（`update()`，不是每次滚动）把 `<svg>` 子树连同计算样式写成 SVG 文本：属性照抄，`style` 里补 CSS 设出来的 `fill`/`stroke`/…（`PROPERTIES`），继承的 `color`/`font-*`/`visibility` 在根上和变化处写出（`currentColor` 因此按计算后的 `color` 解析），`display:none` 写出，`<style>`/`<script>`/`<foreignObject>` 不写；没有 `id` 的元素补 `__v<序号>`（按元素顺序编号，不按节点，所以两个相同图标的文本相同）；根的 `width`/`height` 换成内容盒的大小。
  - 键是文本的 62 位散列；相同文本在 `DisplayList.vectors` 里只放一份。`<svg>` 里面没有盒子（`flow.rs` 不展开它的子节点；`<canvas>` 的后备内容同样不展开）。
  - 栅格化在 Shell：`verve_layout::rasterize_svg`（resvg 0.46，与 GPUI 同版本同特性，`verve-layout` 直接依赖；`cargo tree -d` 不出现 image/resvg/usvg/tiny-skia/fontdb 的第二份）。GPUI 一侧 `Pictures::page` 按 `(键, 设备像素宽, 设备像素高)` 缓存（尺寸变了重画，不拉伸），60 帧没画到的还给图集；软件绘制（`screenshot.rs`）按 CSS 像素画。含 `<text` 的才载入系统字体（一次）。
  - `<img src=x.svg>` 与 `background-image: url(x.svg)`：`PageImages` 对 SVG 字节不解码，把文本放进 `ImageTable::vector`，Shell 同样按显示尺寸栅格化（第一阶段留下的 `Unsupported` 补上了）。
  - 几何：usvg 树里每个有 `id` 的节点的两个盒子——`getBBox`（元素自己的用户空间，不含描边，也不含元素自己的 `transform`）和 `getBoundingClientRect`（页面坐标，含 viewBox 缩放和所有 `transform`，也不含描边）。前者经 `Layout.svg_boxes` 进文档，后者并入布局的 `rects`。`<svg>` 根的 `getBBox` 是内容的并集，根自己的 client rect 仍是布局盒。盒子按文本缓存（同一文本只解析一次）。
- **canvas 表面（步骤 5）**：布局读 `Snapshot.canvases`，`read_surface` 给出 `Replaced`（固有尺寸 = 画布的像素数，`object-fit`/`object-position` 照常），键是 `canvas_key(节点, generation)`。`PageImages::sync_surfaces` 只转换显示列表点名的那个 generation（预乘 RGBA → straight BGRA），旧 generation 的键随即离开表；Shell 画到新 generation 时 `Pictures::image` 立刻让同一槽位的旧 `RenderImage` 退役。P25 的 `commit_canvases` 本来就在帧边界发布 generation。
- **动图（步骤 6）**：`page_frames.rs`：GIF、APNG、动画 WebP 逐帧读（`image::AnimationDecoder`，每帧已合成整图），读到 256 MiB 就停；GIF 的循环次数读 NETSCAPE2.0（没有扩展播一遍，计数 N 播 N+1 遍，0 无限）。`PageImages::animate` 为显示列表点名的多帧图各记起点，按时间算帧号并改写列表（`DisplayList::with_frames`，新 revision）；`next_due` 返回到下一次换帧的剩余时间（定时器，不是 5 ms 轮询）；不再点名的图停止计时，再点名从第 0 帧开始。
- **夹具与对比**：`tests/electron/apps/images/`（`gen.js` 在真 Electron 里画出 PNG/JPEG/WebP 并内联，`main.js` 抓图，`expected-chromium.png` 是 Electron 39.8.10 的 `capturePage()`）；`crates/verve-gpui/tests/images.rs` 两条 `#[ignore]` 的测试（见下）。对比脚本 `SCRATCH\p35\images-compare.ps1`。
- **内存**：`images.rs` 的 `two_hundred_megapixel_pictures_that_scroll_…`：写 200 张 1000×1000 的 PNG 和一个竖排、每 100 ms 滚 1500 px 的页面，每秒读一次进程工作集（PowerShell），二十秒；峰值要小于 400 MB，且最后一次读数小于峰值。显示列表只点名的图才解码、GPUI 对窗口外的 `Shape::Picture` 不上传（第一阶段评审留下的两项，第一阶段已做）。

**Electron 39.8.10（Chromium 142）的观察**（`show: false`，DPR 1，scratch profile；脚本在草稿目录 `p35/obs2/`）：

- `<svg width=200 height=100 viewBox="0 0 100 50" style="position:absolute;left:10px;top:20px">` 里：`<rect x=10 y=5 width=30 height=20 stroke-width=4>` 的 `getBBox()` 是 `10 5 30 20`（用户单位，不含描边），`getBoundingClientRect()` 是 `30 30 60 40`（不含描边，含 viewBox 的 2 倍）；`<g transform="translate(50,10)"><circle cx=10 cy=10 r=8>` 的 `<g>` 与圆都是 `getBBox` `2 2 16 16`（不含 `<g>` 自己的 `transform`）、client `114 44 32 32`；带 `transform="rotate(90)"` 的 path：`getBBox` `0 0 20 10`，client `-10 20 20 40`；`<svg>` 自己的 `getBBox` 是内容的并集 `-10 0 78 28`（含旋转后的 path）。
- GIF：帧时长 0 或 10 ms 的显示约 100 ms；20 ms 以上按文件里的时长（50 ms 实测周期 47 ms，100 ms 约 95–100 ms，110 ms 约 110 ms）。NETSCAPE2.0 计数 0 无限循环；没有这个扩展播一遍后停在最后一帧；计数 1 播两遍（红蓝红蓝）、计数 2 播三遍，都停在最后一帧。
- 夹具抓图时（`show: false`，加载后 2.5 s 的 `capturePage()`）GIF 还在第一帧（红）；评审的探针（`p35-review3\gif\out.json`）里被绘制的隐藏窗口里 GIF 是动的，所以不能说隐藏窗口不动画。夹具对比把 GIF 的 64×64 方块排除在外。（`capturePage()` 的采样间隔约 17 ms，上面的周期是这样量出来的。）

**没做 / 偏差**：

- 第一阶段留下的三项：`<img>` 的 `border-radius` 裁剪（`PaintItem::Image` 仍没有 `radii`）、破图盒与 `alt`、flex 里 `<img>` 的拉伸，**都没做**（预算花在 SVG/canvas/动图/夹具上）；观察到的数字仍在上面第一阶段的记录里。
- canvas 没有记录脏矩形（只整张转换、整张上传；任务书说记录，但没有使用者，留给 P33）。每个 generation 都在页面线程上整张转 BGRA（1920×1080 约 8 MB）。
- 动图改写显示列表是整张列表的拷贝（`with_frames`，每次换帧一次）：条目很多的页面换帧会有 O(条目) 的开销，P33 的块缓存之后应只改动有动图的块。动图只在显示列表点名时计时，没有"页面不可见就停"的判断（渲染器不知道窗口是否被遮住；`hidden` 的页面本来不出帧）。
- 无限循环的动图会让 `--screenshot` 等不到"列表 600 ms 不变"（它每 100 ms 换一次 revision）；夹具的 GIF 只播两遍。`getBBox` 与 `getBoundingClientRect` 对嵌套带 `transform` 的元素取的是 usvg 的盒子（上面的观察对得上），但 `<use>` 引用的、`<text>`（字体用系统字体，度量与 Chromium 不同）、`<foreignObject>`（不支持）没有对照观察。
- 动画 WebP 与 APNG 的解码走同一个函数，但没有测试数据；GIF 的 disposal 由 `image` 的 `GifDecoder` 处理，没有单独测。
- 256 MB 的 LRU 仍然没有（第一阶段说过）。
- 夹具：`expected-chromium.png` 是在这台机器上用 Electron 39.8.10 抓的；JPEG、WebP 的解码器（Chromium 的 libjpeg-turbo/libwebp，Verve 的 `image`）有差别，边缘按 3% 的预算放过。

**任务书「修改」之外动过的文件**：`verve-dom` 的 `layout_provider.rs`、`state.rs`、`bind/svg.rs`、`canvas/image.rs`、`canvas/mod.rs`；`verve-electron` 的 `page_layout.rs`、`renderer_guest.rs`（只在 `next_due`/`publish` 里加几行）、`lib.rs`；`verve-gpui` 的 `plan.rs`、`app_region.rs`（测试里补 `vectors` 字段）；`Cargo.lock`（`verve-layout` 多了对 `resvg` 的依赖，图里没有新包）。

**给其他包**：`DisplayList` 多了 `vectors`，`ImageFrames` 多了 `loops`，`verve_dom::Layout` 多了 `svg_boxes`（字面量构造的要补，`..Default::default()` 的不用）。P33：`DisplayList::with_frames` 是动图换帧的入口；`Pictures` 对 canvas 键按槽位（`canvas_slot`）退役。P32 二期：没有碰 `PaintItem::Control`。

**要跑的测试**（一次一条）：

```
cargo test --locked -p verve-layout --lib svg
cargo test --locked -p verve-layout --lib images
cargo test --locked -p verve-layout --test images
cargo test --locked -p verve-dom --test layout_provider
cargo test --locked -p verve-electron --lib page_frames
cargo test --locked -p verve-electron --lib page_images
cargo test --locked -p verve-electron --test images
cargo test --locked -p verve-electron --test headless
cargo test --locked -p verve-gpui --features jit --lib
cargo test --locked -p verve-gpui --features jit --test boxes
cargo test --locked -p verve-gpui --features jit --test text_fixture
cargo clippy --locked --workspace --all-targets -- -D warnings
# 开真窗口：
cargo test --locked -p verve-gpui --features jit --test images -- --ignored the_picture_the_electron_binary
cargo test --locked -p verve-gpui --features jit --test images -- --ignored --nocapture two_hundred
# 或者：SCRATCH\p35\images-compare.ps1 [-Regenerate] [-Memory]
```

### 第二阶段第二轮（评审与中心运行后的修补）

同样只做了 `cargo check` 和一次 workspace clippy 以外的事都没有跑；测试由协调者跑。

**改了**：

- **MUST 1（安全）**：`svg.rs::parse` 把 `usvg::Options::image_href_resolver` 换成只认 `data:`（`resolve_string` 恒为 `None`），默认的字符串解析器会对任何存在的路径（含 UNC）`fs::read`。检查过：全仓库只有这一处构造 `usvg::Options`（GPUI 自己的 `SvgRenderer` 读素材，不是我们的路径）；嵌套 SVG 用同一份选项；`feImage` 走同一个 `image_href_resolver`；字体只来自系统字体库（`fontdb`），没有按路径读字体。测试 `an_image_that_names_a_file_draws_nothing` 写一个真的临时 PNG，旧代码会画出红点。内联 `<image>` 的非 `data:` 地址不取（Chromium：http 页面与 `<img>` 里的 SVG 也不取）。
- **MUST 2**：`<use href="#id">` 指向图外（图标精灵）时，把目标连同它们的目标复制进尾部的 `<defs>`（有已访问集合，里面的元素不补 id）。`url(#…)` 的渐变没有复制（Chromium 也不用 `display:none` 的 `<svg>` 里的渐变；而渲染着的 `<svg>` 里的渐变没有处理，留着）。
- **MUST 3**：只写合法的 XML 属性名（无冒号，或 `xlink:`/`xml:`），其余丢掉。**MUST 4**：根元素不再写 `opacity`/`mix-blend-mode`/`isolation`/`filter`/`clip-path`/`mask`（布局已把它们用在整张图上）。
- **MUST 5（内存）**：我没有开窗口，所以**没有量到**各处的张数；按代码推算：页面线程的表里原来有 200 张（`names_in` 点名整张列表，约 800 MB），GPUI 的 `Pictures` 再按 300 帧的宽限留着画过的图。改成：`names_in` 只点名窗口上下各一屏之内的图（用列表自己的滚动位移）；`Pictures` 对没画的页面图改用 64 MiB 的字节预算（最久没画的先丢），没画的矢量栅格当帧就丢，同一个键只留一个尺寸；`PageImages` 的 128 MiB 闲置预算不变。测试让页面把 `loaded` 和 `scrolled` 写进 `result.json` 并断言（200 张都加载、滚过 90000 px），要求峰值大于 100 MB（真的解码了）、最后一次读数低于峰值的 90%。**需要协调者再跑一次并把各秒的数字贴回来。**
- **MUST 7**：渲染器处理 `ToRendererInput::Visibility`：`document.set_visibility`，`PageImages::set_hidden`：隐藏时 `next_frame` 返回 `None`，`animate` 按隐藏那一刻算帧，重新显示时各起点后移隐藏的时长（有单元测试）。
- **MUST 8**：`DisplayList::same_but_frames`；`Capture` 用它判断"没变"，无限循环的动图不再让 `--screenshot` 等满 30 s（`a_screenshot_is_taken_of_a_page_with_an_animation_that_never_ends`，`#[ignore]`）。由 `requestAnimationFrame` 重画的 canvas 仍然会让它等不到（记在下面）。
- **MUST 6**：夹具对比按部分（17 块，按 `index.html` 里的矩形）：每块不同像素占自己面积 < 5%，且非白像素不少于期望的 80%，GIF 方块仍排除。
- **MUST 9**：`paint_command` 的阴影分支移到 `paint_shadow`（P32 的 `Control` 分支可以直接加在那里）；矢量文本放进图片元组，`page_picture` 回到 7 个参数。**MUST 10**：同一个键只留一个尺寸，没画到的矢量栅格当帧就还回。**MUST 11**：`with_frames` 用 `reframe` 辅助函数，嵌套回到 3 层。
- **中心的三项**：canvas 测试失败的原因：任务里画的 canvas 只在下一次 `take_update` 里提交（`commit_canvases`），而发布前的 `ensure_layout` 看不到它（它只在有"改动"时才重排），所以第二个 generation 永远不进显示列表；现在 `publish` 先调 `document.commit_canvases()`（现在是 `pub`）。`map_entry` 改用 `entry` API。内存见上。
- **SHOULD（做了）**：SVG 固有尺寸（属性边界、`em`、被截断的 UTF-8 字符、只有 `viewBox` 时自然尺寸 300×150；**按比例铺满块宽没做**，布局里还是 300×150）；APNG 的 `num_plays` 与 WebP 的 `ANIM` 循环次数（Electron 39.8.10：N 播 N 遍，`p35-review3\gif\out.json`）；`fill`/`stroke` 等绘制属性继承（单独的 `feat(dom)` 提交；Electron 39.8.10 观察：`.btn{fill:red}` 在 `<div>` 上，`<rect>` 画红，`getComputedStyle(rect).fill` 为 `rgb(255, 0, 0)`）；GIF 循环测试改成确切的 `[0,1,0,1]`；契约里 `vectors` 在 `root` 之后；`page_frames` 里失效的链接；`rasterize` 用 `unpremultiply`；"隐藏窗口不动画"的说法改正；替换元素的判断有了名字 `replaced::has_no_inner_boxes`。

**还没做 / 记下来**：

- SVG 在 `<img>` 和背景里按另一个比例画时被拉伸（Chromium：有 `viewBox` 时保持比例，`[10,0,80,40]`，评审的 `p35-review3it\out.json`）；SVG 的 `getBBox` 对 `<use>` 实例、带 `transform` 的 `<use>`、嵌套 `<svg>`、空 `<g>` 的边角情况；CSS `transform` 作用在 SVG 元素上（Chromium 会移动 20px）；`<use>` 引用的 `url(#…)` 渐变（渲染着的另一个 `<svg>` 里）；cf02017ea 的历史没有拆出 `feat(dom)`；11ff5c30a 没拆。
- canvas 重画会整页重排（P25 的 `commit_canvases` 把节点标为改动，`LayoutEngine::update` 整棵重建，每个内联 SVG 重新写一遍），每个 generation 在 CPU 上拷三次；应在只有 canvas 提交且尺寸不变时把键补进现有列表。`requestAnimationFrame` 重画的 canvas 让 `--screenshot` 等不到静止。
- 现在有两份系统字体库（GPUI 的 `SYSTEM_FONT_DB` 和 Verve 的）；第一次遇到 `<text` 的 SVG 时页面线程会阻塞到系统字体载入；d3 里每个标签量一次 `getBBox` 的写法会让整页重排、整张图重新解析；SVG 里没有命中测试（没有盒子，`event.target` 是 `<svg>`）；脏矩形没有记录（任务书要求）。
- 体积：本轮新增的是纯 Rust 代码（`reframe`/`same_but_frames`、预算淘汰、`<defs>` 复制、`set_hidden`），没有新 crate 和新解码路径。

**本轮的测试命令**（在上面的清单之外加）：

```
cargo test --locked -p verve-layout --test images
cargo test --locked -p verve-layout --lib
cargo test --locked -p verve-dom --lib canvas::image
cargo test --locked -p verve-electron --test images
cargo test --locked -p verve-electron --lib
cargo test --locked -p verve-gpui --features jit --lib
cargo test --locked -p verve-gpui --features jit --test images -- --ignored the_picture_the_electron_binary
cargo test --locked -p verve-gpui --features jit --test images -- --ignored a_screenshot_is_taken
cargo test --locked -p verve-gpui --features jit --test images -- --ignored --nocapture two_hundred
cargo clippy --locked --workspace --all-targets -- -D warnings
```
