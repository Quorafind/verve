# P33 增量渲染与帧调度

**依赖**：P30（P31、P32 合并后再做最后的基准）  **规模**：M  **修改**：`crates/verve-layout/src/incremental.rs`（新）、`display_list.rs`；`crates/verve-gpui/src/dom_element.rs`、`frames.rs`（新）、`trace.rs`（新）；`crates/verve-electron/src/renderer_guest.rs`（帧时钟）

## 目标

让 P30/P31 的"每次全量布局、全量出显示列表、全量回放"变成按变化增量进行，并把 `requestAnimationFrame` 的节奏
与显示器刷新对齐，达到：

| 场景（release，Windows，1080p，DPR 1.5） | 目标 |
|---|---|
| 2000 行列表（每行 3 个 span + 1 图标）滚动 | 60 fps，GPUI 帧时间 p95 < 8 ms，无掉帧 |
| 同一页面改一行文字（React setState） | 输入到上屏 < 16 ms，渲染器布局 < 2 ms |
| electron-quick-start 首帧 | 渲染器从 `Load` 到首个显示列表 < 40 ms（不含进程启动，P42 管总时间） |
| 空闲 | 两个线程 CPU ≈ 0（无动画、无定时器时不出帧） |

## 背景（先读）

- verve-dom：`Changes`（`nodes`、`restyled`、`partitions`、`removed`、`restyled_all`）、`Document::set_partition`、
  `FrameClock::Host`、`Document::frame(runtime, now)`、`Document::wants_frame()`、`Request::Frame`、`Snapshot`（`Arc`，克隆便宜）。
  verve-dom README 的 "Partitions" 一节：su-editor 用分区只重画设置页的一行。
- 参考（只读）：su-editor `view.rs` 末尾的 `DomView`（一个分区一个 GPUI view，`dirty` 标记，未变化时复用上一帧）。
- GPUI：`view.rs` 的 `AnyView::cached(style)` / `Entity::cached`（视图级缓存：未 `notify` 的视图复用上一帧的布局与绘制）、
  `window.rs` 的 `request_animation_frame`、`on_next_frame`、`refresh`；`WindowOptions.inactive_frame_interval`（失焦窗口降帧）。
- taffy：`TaffyTree::mark_dirty`、布局缓存（只重算脏节点祖先链）。

## 设计

### 1. 布局增量（渲染器线程）

- `LayoutEngine` 持久保留 taffy 树和 `NodeId ↔ taffy::NodeId` 映射（P30 已建）。
- 每次 `update(snapshot, changes)`：
  - `changes.removed`：删 taffy 节点与缓存。
  - `changes.nodes`（子节点变化或自身属性/文本变化）：重建该节点的 taffy 子节点列表；文本变化只使该块的
    `InlineLayout` 缓存失效。
  - `changes.restyled`：只把**影响布局**的属性变化（`display/position/width/margin/font-*/…`）转成 taffy `mark_dirty`；
    纯绘制属性（`color/background/opacity/box-shadow/visibility/cursor/outline/transform: translate`）**不重排**，只重出该节点的绘制项。
    建一张"属性 → 影响层级"表：`Layout` / `Paint` / `Composite`（opacity、translate 只改合成参数）。
  - `restyled_all`：保留树结构，所有节点重新取样式、全量标脏。
- 塑形缓存：`(文本, FontKey, 字号, letter-spacing)` → `Arc<ShapedRun>`，LRU 上限 8 MB。断行结果按 (块, 可用宽度) 缓存。

### 2. 显示列表分块（Chunk）

- `DisplayList.chunks: Vec<Chunk>`，`Chunk { id: ChunkId, revision: u64, range: Range<usize> /* items 下标 */, bounds: Rect, translate: [f32; 2], opacity: f32, clip: Option<Rect> }`。
- 一个块 = 一个层叠上下文，或一个分区（`set_partition` 标记的子树），或一个滚动容器的内容。块内容只有在其节点有 Layout/Paint 级变化时才
  重生成并 `revision += 1`；只有 `Composite` 级变化时只改块头的 `translate/opacity`。
- 滚动容器内容是独立块：滚动只改块的 `translate`，不重生成项（滚动不触发布局、不触发重出显示列表）。P30 的契约是 `PushScroll { node }`/`PopScroll` 包住帧里的项（项按偏移为 0 摆放），
  偏移只在 `Scroller` 项里：块的 `translate` 就是 −offset(node)，块里的项在滚动时一个字节不变。
- 跨线程传递：`Arc<DisplayList>` 内的每个块的 items 也是 `Arc<[PaintItem]>`，新列表复用未变块的 `Arc`（零拷贝）。

### 3. GPUI 侧回放

- `DomElement` 为每个 `ChunkId` 保留一个缓存记录：上次的 `revision` 与（可选）GPUI 的视图实体。
- 方案 A（先做）：每帧全量回放所有可见块的 items（裁剪到视口外的块整块跳过：`bounds` 与可视区不相交就不画）。GPUI 的 `paint_quad/paint_glyph`
  只是往 scene 里压图元，几千个图元 < 1 ms。
- 方案 B（A 达不到目标时）：每个块对应一个 `Entity<ChunkView>`，用 `AnyView::cached(...)` 包起来；`revision` 变了才 `cx.notify()` 该实体，
  未变的块 GPUI 直接复用上一帧图元（su-editor 分区的做法）。块的平移/透明度用外层包装元素实现，不使 `ChunkView` 失效。
- 长列表：不需要特殊 API —— 视口裁剪 + 块缓存即可。页面自己做虚拟列表（react-window 等）时只有少量节点。

### 4. 帧调度

- 渲染器用 `FrameClock::Host`：`requestAnimationFrame` 回调只在收到 `ToRendererInput::Frame { now }` 时跑。
- 渲染器发布的 `RendererView.wants_frame = document.wants_frame()`（有 rAF 回调、CSS 动画/过渡在跑、或 `Request::Frame`）。
- Shell：某窗口的 view `wants_frame` 为真时，在该窗口 `window.request_animation_frame()`；在 GPUI 的帧回调（`on_next_frame`）里给对应渲染器发
  `Frame { now }`（`now` 用 `performance.now()` 同一时钟：渲染器 Runtime 的时钟，Hub 提供换算），并画**当前**最新的显示列表（不等渲染器）。
- 渲染器收到 `Frame`：跑 rAF 回调 → 推进 CSS 动画（P22）→ `update` → 布局增量 → 发布。目标是在下一个 vsync 前发布；赶不上就画上一帧（不阻塞 GPUI）。
- 失焦/最小化窗口：GPUI `inactive_frame_interval` 设 1 s（与 Chromium 后台节流相近）；最小化或 `hide()` 时停止发 `Frame`，
  `document.visibilityState = 'hidden'`、`visibilitychange`（P13 的 `backgroundThrottling: false` 时不节流）。
- 没有任何变化与动画时两边都不出帧（空闲 CPU ≈ 0）：Hub 的 `set_shell_waker` 只在有新 view 时唤醒 GPUI。

### 5. 测量与追踪

- `--trace=<file>`（electron 可执行的命令行开关，P10 负责解析，本包负责写）：输出 Chrome Trace Event JSON（可用 `chrome://tracing` / Perfetto 打开），
  事件：`renderer.update`（快照）、`layout`（含脏节点数）、`display_list`（重生成块数）、`shell.paint`（图元数）、`frame`（vsync 间隔）、`ipc`（P12 可接入）。
- `VERVE_FPS=1`：窗口角落叠加帧时间曲线（GPUI `deferred` 绘制）。
- 基准 `crates/verve-gpui/benches/`（criterion 不引入，避免体积，用简单的 `#[test] #[ignore]` 计时）：
  `list_2000_scroll`（模拟 300 帧滚动，统计 p50/p95/max 帧时间）、`text_edit_latency`、`first_display_list`。

## 接口契约

- `DisplayList` 追加 `chunks`（P30 已预留字段）与上述 `Chunk` 结构。
- `RendererView.wants_frame` 语义如上；`ToRendererInput::Frame { now }` 的 `now` 是渲染器时钟的毫秒。
- `LayoutEngine::update` 返回 `UpdateStats { relayout_nodes, rebuilt_chunks, reused_chunks, shaped_runs, cache_hits }` 供 trace 使用。

## 步骤

1. 先做 trace 和基准（没有测量不做优化），记录 P30/P31 全量版本的基线数据写进本文"状态"。
2. 属性影响层级表 + 布局增量。
3. 显示列表分块与 `Arc` 复用；滚动只改平移。
4. GPUI 侧视口裁剪（方案 A），测；不达标再做方案 B。
5. 帧时钟：`wants_frame`、`request_animation_frame`/`on_next_frame`、后台节流、`visibilitychange`。
6. 复测并把结果表填进"状态"。

## 验收

- `cargo test -p verve-layout --test incremental`：改一个节点的 `color` → `relayout_nodes == 0`、只重建一个块；改 `width` → 只有其祖先链重排；
  滚动容器滚动 → `rebuilt_chunks == 0`。
- `cargo test -p verve-gpui -- --ignored list_2000_scroll` 在 release 下 p95 < 8 ms；结果写入 PR。
- 夹具 `tests/electron/apps/list2000/`：手动滚动无可见卡顿；`--trace` 输出可被 Perfetto 打开。
- 空闲页面 10 秒内 GPUI 线程与渲染器线程 CPU 时间 < 50 ms（用 trace 或系统计数器测）。

## 不做

GPU 合成层/离屏纹理缓存（GPUI 不提供）、多线程布局、平滑滚动动画（v1 滚轮即时滚动；`scroll-behavior: smooth` 由渲染器在多帧内插值 `apply_scroll` 实现，可选）。

## 状态

（实现者完成后填写：基线数据、优化后数据）
