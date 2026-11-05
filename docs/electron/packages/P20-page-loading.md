# P20 渲染器 Realm 与页面加载管线

**依赖**：P02、P03  **规模**：L  **修改**：`crates/verve-dom/src/{lib.rs,html.rs,state.rs,bind/document.rs,bind/element.rs,bind/fetch.rs,bind/xhr.rs,bind/window.rs}`；新增 `crates/verve-dom/src/load.rs`；`crates/verve-electron/src/{renderer_guest.rs,loader.rs}`

## 目标

让一个 webContents 像浏览器标签页一样**加载一个页面**：拿到 HTML，边解析边建文档，按 HTML 规范的顺序
执行 `<script>`（classic 同步/`defer`/`async`、`type=module`）、加载 `<link rel=stylesheet>`、`<img>`、
`@font-face`、图标，依次触发 `readystatechange`/`DOMContentLoaded`/`load`，并把这些阶段报告成 P02 的
`PageEvent`。`loadURL`/`loadFile`/`reload`/页面内导航都走这一条路。

完成后 electron-quick-start 的 `index.html`（内联 `<script>`、外链 `renderer.js`、外链 CSS）和
Vite 构建出的 React 应用（`<script type="module" crossorigin src="/assets/index-xxx.js">` +
`<link rel="stylesheet" href="/assets/index-xxx.css">`）都能完整加载。

## 背景（先读）

- `crates/verve-dom/README.md` 的「Not yet」：现在**什么都不加载**——`<img>`、`<link rel=stylesheet>`、
  `<script src>`、`<iframe>` 都不发 `load`。本包就是把这些补上。
- `crates/verve-dom/src/html.rs`：`parse_document(dom, document, markup)` / `parse_into` → 私有的
  `parse(dom, parent, markup, context)`，一次性解析整段文本，用 `Vec<Open>` 做开放元素栈。
  `<script>`/`<style>` 作为 raw text 读入（`raw_text`），**不执行**。
- `crates/verve-dom/src/bind/document.rs`：`readyState` 写死为 `"complete"`（约 495 行），
  `currentScript` 写死为 `null`（约 513 行），`document.write`。
- `crates/verve-dom/src/state.rs`：`State`（`url`、`sheets: Vec<SheetRecord>`、`SheetOwner`、
  `read_style_elements()` 只读 `<style>` 和 adopted sheets）。
- `crates/verve-dom/src/css.rs`：`parse_block` 丢弃 `@import` 语句和 `@font-face`/`@keyframes` 块；
  `Sheet::parse`。
- `crates/verve-dom/src/bind/fetch.rs`：DOM 的 `fetch` 包一层——相对地址按 `state.url` 解析，
  只放行 `http/https/data/blob`，其余抛 `TypeError`。`bind/xhr.rs` 建在这个 fetch 上。
- `crates/verve-dom/src/bind/element.rs`：`IMAGE_ACCESSORS`（`naturalWidth` 写死 0、`complete` 写死 true）。
- `crates/verve-dom/src/lib.rs`：`Options`、`install`、`Request::{Navigate, Reload, Open, Title}`。
- Verve 内核：`src/lib.rs` 的 `Runtime::load_module`/`load_esm`/`import_module`；`src/esm.rs` 只接受
  `file/data/blob/node` 方案的模块地址（其他抛 `ERR_UNSUPPORTED_ESM_URL_SCHEME`）；
  `src/module_hooks.rs`（`module.registerHooks` 的 resolve/load 链）。
- P02 的 `ToRenderer::Load`、`PageEvent`、`FromRenderer::Dom`。

## 设计决定

1. **导航 = 换一个 isolate**。跨文档导航（`loadURL`、`loadFile`、`reload`、`location.href = …`、
   点了 `<a>`、表单提交到新页面）时，渲染器**销毁旧的 `RendererGuest`、新建一个**——与 Chromium
   跨文档导航销毁旧文档的 JS 世界一致，也最简单：不需要"清空一个 Runtime"。`ContentsId` 不变，
   主进程上的 `webContents` 对象不变。同文档导航（`#hash`、`pushState`）verve-dom 已自己处理，
   只报 `DidNavigateInPage`。
   - 实现：Hub 持有 `ContentsId → RendererHandle`；`ToRenderer::Load` 到达时 Hub 先让旧 isolate
     发 `beforeunload`（可被取消时回 `will-prevent-unload`，见 P13）和 `unload`，然后丢弃它、
     用同一组 `WebPreferences` 新建，再在新 isolate 里执行加载。
   - 渲染器发出的 `Request::Navigate { url, replace }` 由 Hub 转给主进程，主进程先发 `will-navigate`
     （可 `preventDefault`），未阻止才发 `ToRenderer::Load`。
2. **可暂停的解析器**。parser-inserted 的同步 classic `<script>` 必须在解析到它时执行、能看到它之前
   的 DOM、能 `document.write`。把 `html.rs` 的 `parse` 改造为可恢复的结构体（见接口契约），在
   `</script>` 处交还控制权。`innerHTML`/`DOMParser`/`parse_into` 仍是一次解析完，且其中的脚本**不执行**
   （与浏览器一致）。
3. **加载 I/O 不在 JS 线程上阻塞**。`Loader::fetch` 是异步的：渲染器提交请求，结果作为任务回到
   渲染器的事件循环。唯一的例外是 parser-blocking 脚本和样式表：解析器在那里**暂停**，不阻塞线程，
   等资源到了再继续（期间定时器、微任务照常跑）。
4. **file:// 与自定义方案由 verve-electron 实现**；verve-dom 只认识 `Loader` trait，不知道 Electron。

## 接口契约

### verve-dom：`Loader`（需新增，`src/load.rs`）

```rust
/// What a page fetches as it loads: its scripts, sheets, images, fonts, icons — and what its
/// relative `fetch`/XHR go to when the scheme is not the network's.
pub trait Loader: 'static {
    /// Starts fetching `url`. `done` is called on the realm's thread (the embedder queues it
    /// back, e.g. as a runtime task) exactly once.
    fn fetch(&self, request: LoadRequest, done: Box<dyn FnOnce(LoadResult)>);
    /// Whether this loader answers `scheme` (file, app, custom ones registered by P16).
    /// http/https/data/blob go through Verve's own fetch unless the loader claims them.
    fn handles(&self, scheme: &str) -> bool;
}

pub struct LoadRequest {
    pub url: url::Url,
    pub destination: Destination, // Document | Script | Style | Image | Font | Icon | Fetch
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}
pub enum Destination { Document, Script, Module, Style, Image, Font, Icon, Fetch }

pub struct LoadResult {
    pub status: u16,                 // file:// 成功 200，找不到 → Err
    pub url: url::Url,               // 重定向后的
    pub mime: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Result<Vec<u8>, LoadError>,
}
pub struct LoadError { pub code: i32, pub description: String } // Chromium net error，如 -6 ERR_FILE_NOT_FOUND
```

`Options` 增加字段 `pub loader: Option<Box<dyn Loader>>`（`None` = 现状：什么都不加载，测试和
`verve-dom run` 保持不变）。

### verve-dom：加载入口（需新增，`Document` 上）

```rust
impl Document {
    /// Loads `url` into this (fresh) document: fetches it through the loader, parses it as it
    /// arrives, runs its scripts in HTML's order. Progress comes back through `take_load_events`.
    pub fn navigate_initial(&self, runtime: &mut verve::Runtime, url: &str);
    /// Same, with the markup already in hand (loadURL('data:…'), tests).
    pub fn load_html(&self, runtime: &mut verve::Runtime, url: &str, html: &str);
    /// What happened since last asked, in order.
    pub fn take_load_events(&self) -> Vec<LoadEvent>;
}
pub enum LoadEvent {
    Started, DomContentLoaded, Loaded,
    Failed { code: i32, description: String, url: String },
    Title(String), Icons(Vec<String>),
    /// The parser reached `<body>` with every blocking sheet in: something can be drawn.
    FirstPaintable,
}
```

渲染器 Guest 把 `LoadEvent` 一一映射为 `PageEvent`：`Started→DidStartLoading`、
`DomContentLoaded→DomReady`、`Loaded→DidFinishLoad`、`Failed→DidFailLoad`、`Title→TitleUpdated`、
`Icons→FaviconUpdated`、`FirstPaintable→FirstPaintable`。

### 可恢复解析器（`html.rs`，需新增）

```rust
pub struct StreamParser { /* 开放元素栈、插入模式、未消费的文本 */ }
pub enum Step { NeedMore, Script(NodeId), Done }
impl StreamParser {
    pub fn new(dom: &mut Dom, document: NodeId) -> Self;
    pub fn feed(&mut self, dom: &mut Dom, chunk: &str);   // 网络/文件分块到达
    pub fn finish(&mut self);                             // EOF
    pub fn step(&mut self, dom: &mut Dom) -> Step;        // 解析到下一个需要执行的脚本或末尾
    /// document.write during a script: goes in at the insertion point.
    pub fn insert(&mut self, text: &str);
}
```

现有 `parse_document` 改为 `StreamParser` 一次喂完再 `step` 到 `Done` 的包装，行为不变（现有测试必须全过）。

### 执行顺序（HTML 规范，必须照做）

| 脚本 | 何时执行 |
|---|---|
| 内联 classic，无属性 | 解析到 `</script>` 时；若前面有**未加载完的样式表**，先等它们（script-blocking style sheets） |
| 外链 classic `src`，无 `async`/`defer` | 暂停解析，取到后立即执行，再继续解析 |
| `defer`（仅外链 classic） | 解析结束后、`DOMContentLoaded` 之前，按文档顺序 |
| `async` | 取到就执行，不保证顺序；不阻塞解析 |
| `type=module`（外链或内联） | 默认 defer 语义；带 `async` 则取到图（含依赖）即执行 |
| 动态插入的 `<script>`（`createElement('script')` + 插入文档） | 插入时开始取，默认 async；`script.async = false` 的按插入顺序 |
| `type` 不是 JS MIME / `module` / 空 | 不执行（如 `text/template`、`application/json`）|
| `nomodule` | 不执行（我们支持 module） |

- 执行 classic 脚本期间 `document.currentScript` 是那个元素；module 脚本期间是 `null`。
- 执行出错：报到 console（`FromRenderer::Console`），触发 `window.onerror` / `error` 事件，继续解析。
- 外链脚本取失败：在元素上发 `error` 事件；成功执行后发 `load`。
- `readyState`：解析中 `"loading"` → 解析完、defer 执行完前 `"interactive"`（同时发
  `readystatechange`，然后 `DOMContentLoaded`）→ 所有子资源（图片、样式表、async 脚本、iframe）
  完成后 `"complete"`（`readystatechange`，然后 `window` 的 `load`，再 `pageshow`）。
- 动态插入的脚本（不论 classic/module）都要支持：现在 `bind/node.rs` 插入节点时需新增一个钩子
  "已连接的 `<script>` 元素第一次进入文档"→ 交给加载器。

### 模块脚本

- 地址按 `document.baseURI`（`<base href>` 优先，否则文档 URL）解析。
- 需要 Verve ESM 能从**非 file 方案**取源码：`app://`、`http://`、自定义方案。依赖 P03 新增的内核钩子
  （见文末"给其他包的备注"）——ESM 加载器遇到 `Loader::handles(scheme)` 的地址时，调用宿主给的
  同步取源回调（渲染器线程上的缓存命中）或异步取源；本包负责在执行 `<script type=module>` 前把
  整个静态依赖图**预取**进缓存，这样 ESM 加载器拿源码时总是同步命中。
- import map：`<script type="importmap">` 必须在第一个模块脚本之前出现；解析其 `imports`/`scopes`，
  通过 `module.registerHooks` 同款的 Rust 侧 resolve 钩子应用（不写 JS）。
- `import.meta.url` 是模块的真实 URL（`app://…/assets/index.js`），不是文件路径。
- `nodeIntegration: true` 时，`file://` 页面里的 `require()` 仍走 Node 的 CommonJS（P21）。

### 样式表、图片、字体、图标

- `<link rel=stylesheet href>`（含 `media` 属性、`disabled`、`title` 备选样式表忽略）：取到后作为新的
  `SheetOwner`（需新增 `SheetOwner::Link(NodeId)`）进入层叠；移除元素即移除样式表；`link.sheet` 返回
  CSSStyleSheet。发 `load`/`error`。在 `<head>` 里的视为 render-blocking：`FirstPaintable` 要等它们。
- `@import url(...)`：`css.rs` 的 `parse_block` 现在丢弃 `@import`；改为记下 URL，由加载器取来，
  按 `@import` 的位置插入该样式表的规则（级联顺序：被导入的规则在导入它的样式表之前）。
- `<img src/srcset>`：取到字节后交给 P25 的解码器拿尺寸（本包只需尺寸，P25 负责像素），设置
  `naturalWidth/naturalHeight/complete/currentSrc`，发 `load`/`error`；`img.decode()` 返回的 promise
  随之 settle。`loading="lazy"` 在 v1 当作 eager。像素交给绘制侧的方式由 P25/P35 定义。
- `@font-face`：P22 把它解析成 `FontFaceRule`；本包取字体文件字节，交给 `document.fonts`
  （P23 实现真正的 `FontFaceSet`），并通过快照把字体数据交给 Shell 注册进 GPUI 文本系统（需新增
  `SnapshotData.fonts: Vec<FontData>`，P31 消费）。
  P23 的接口：`document.fonts` 里的 `FontFace` 调 `load()` 时 `Document::take_requests()` 给出 `Request::LoadFont { face, url }`；
  取完（或失败）后本包调 `Document::settle_font(runtime, face, FontOutcome::Loaded | Failed)`，`status`、`loaded`、`ready` 与事件随之变化。
- `<link rel=icon|shortcut icon|apple-touch-icon>`：收集 href → `LoadEvent::Icons`。
- `<title>` 改变 → `LoadEvent::Title`（现有 `Request::Title` 也要继续发）。
- `<base href>`、`<meta http-equiv="Content-Security-Policy">`（v1 解析并忽略，PR 中记录）、
  `<meta charset>`（v1 只支持 UTF-8，其他编码用 `encoding_rs` 解码；体积考虑：只在遇到非 UTF-8 时才需要，
  可以先只做 UTF-8 + BOM 检测并在 P41 登记）。

### fetch / XHR 的相对地址与自定义方案

- `bind/fetch.rs` 改为：地址按 `document.baseURI` 解析；若 `loader.handles(scheme)`，走 `Loader::fetch`
  （`Destination::Fetch`），构造 `Response`；否则交给原来的 Verve fetch（http/https/data/blob）。
  `file://` 页面里 `fetch('./data.json')` 在 Electron 中可用，这里也必须可用。
- `XMLHttpRequest` 同理（`bind/xhr.rs` 建在 fetch 上，改 fetch 即可，补测试）。

### verve-electron：`ElectronLoader`（需新增，`crates/verve-electron/src/loader.rs`）

- `file://`：读文件（经过 P11 的 asar 虚拟文件系统，`app.asar/index.html` 要能读到）；MIME 按扩展名
  （小表：html、js、mjs、css、json、svg、png、jpg、gif、webp、ico、woff、woff2、ttf、otf、wasm、map）。
  找不到 → `LoadError { code: -6, "ERR_FILE_NOT_FOUND" }`。目录 → `-2`。
- `data:`、`blob:`：交给 Verve 已有实现。
- `http/https`：交给 Verve 的 fetch（同一套 `Permissions`；渲染器默认网络权限策略在 P16 定）。
- 自定义方案：查 P16 的 `protocol` 注册表（`protocol.handle(scheme, handler)` 的 handler 在**主进程**
  上执行，返回 `Response`）；渲染器发 `FromRenderer`（需新增变体 `ProtocolRequest { call, request }`）
  给主进程，主进程回 `ToRenderer::ProtocolResponse { call, response }`（需新增）。本包只定义这两个
  消息并实现 file/data/http，自定义方案的主进程侧由 P16 完成。
- `loadFile(path, { query, search, hash })` = `loadURL(pathToFileURL(path) + …)`（P13 负责拼 URL，本包收 URL）。

### `window.open` 与 `<iframe>`

- `window.open(url, name, features)`：verve-dom 现有 `Request::Open(String)`；扩展为
  `Request::Open { url, name, features }`（需改 enum，同步修改现有测试）。主进程按
  `webContents.setWindowOpenHandler`（P13）决定 `deny` 或新建 `BrowserWindow`；v1 返回值：
  `window.open` 在渲染器里返回 `null`（与 `nativeWindowOpen` 但跨 isolate 无法共享 window 对象的现实
  一致——**这是已知差异**，在 P41 登记；`opener.postMessage` 不可用）。
- `<iframe>`：**v1 不做**。元素存在、`contentWindow`/`contentDocument` 为 `null`、不发 `load`。
  在 P41 登记。v2 方案：每个 iframe 一个子 `RendererGuest`，Shell 把它当子视图画进父文档的盒子里。

## 步骤

1. `Loader` trait、`Options.loader`、`LoadRequest/LoadResult`；`Options.loader = None` 时行为与现在完全一致。
2. `StreamParser` 重构 `html.rs`，现有 `parse_document`/`parse_into` 变为包装，跑通全部现有测试。
3. 加载状态机（建议 `src/load.rs` 里 `struct PageLoad`，挂在 `State` 上）：解析循环、脚本队列
   （parser-blocking / defer / async / 插入顺序）、阻塞样式表计数、子资源计数、`readyState` 与事件。
4. `currentScript`、`readyState`、动态脚本插入钩子、`script.async/defer/src/type/noModule` 反射属性。
5. `<link rel=stylesheet>`、`@import`、`<img>` 尺寸、图标、`<base>`。
6. 模块脚本：依赖图预取 + 与 P03 内核钩子对接；import map。
7. fetch/XHR 相对地址与 Loader。
8. verve-electron：`ElectronLoader`（file/data/http），`RendererGuest` 处理 `ToRenderer::Load`、
   导航换 isolate、`LoadEvent → PageEvent`。
9. `Request::Open` 扩展与主进程侧的转发（P13 消费）。

## 验收

- `cargo test -p verve-dom`：现有全部通过；新增 `tests/load.rs`，用一个内存 `Loader`（`HashMap<Url, bytes>`）验证：
  - 内联与外链脚本、`defer`、`async`、`type=module` 的执行顺序与 Chromium 一致（用一个记录顺序的
    全局数组；期望值从 Chrome 实测冻结，附测试页面源码于 `tests/fixtures/load/`）；
  - 阻塞样式表推迟其后的内联脚本；`getComputedStyle` 在脚本里读到外链 CSS 的值；
  - `document.write` 在解析期插入；`readyState` 三态与 `readystatechange`/`DOMContentLoaded`/`load` 顺序；
  - `currentScript`；动态插入脚本；`onerror`；`<img>` 的 `naturalWidth` 与 `load`；`@import` 级联顺序；
  - `fetch('./x.json')` 在 `app://` 文档里经 Loader 取到。
- `cargo test -p verve-electron --test page_load`（无头，P00 夹具）：
  - electron-quick-start 夹具：`did-start-loading → dom-ready → did-finish-load` 事件顺序，`renderer.js`
    改了 DOM；
  - Vite+React 夹具（产物提交在 `tests/electron/apps/vite-react/dist`，不需要 npm）经 `loadFile` 加载后
    `#root` 有内容；
  - `loadURL` 到不存在的文件 → `did-fail-load` code -6；
  - `location.href = 'page2.html'` → 新 isolate（旧页面的全局变量不见了）、`will-navigate`、`did-navigate`。

## 不做

`<iframe>`、`<webview>`、Service Worker、HTTP 缓存、`<link rel=preload/modulepreload>` 的优化语义（当普通
预取或忽略均可）、`srcset` 的密度选择（取第一个）、非 UTF-8 文档编码（登记）、CSP 执行。

## 给其他包的备注

- **P03**：内核钩子由本包实现，形状见 P03 §4c-3：`Runtime::set_module_source(Option<ModuleSource>)`（`ModuleSource = Rc<dyn Fn(&Url) -> Option<Rc<str>>>`，
  同步、命中预取缓存）与 `Runtime::set_import_map(Option<ImportMap>)`（Rust 侧解析，不写 JS）。本包在执行模块脚本前预取整个静态依赖图。
- **P02**：`FromRenderer` 追加 `ProtocolRequest`，`ToRenderer` 追加 `ProtocolResponse`；
  `verve_dom::Request::Open` 由单字段改为 `{ url, name, features }`。
- **P25**：提供 `fn image_size(bytes) -> Option<(u32, u32)>`（只读头部，不解码全图），本包调用。本包必须应答 `verve_dom::Request::Image(url)`：取回后 `Document::set_image_bytes(runtime, url, mime, bytes)`，失败 `Document::fail_image(runtime, url)`；样式中的 `url()` 图片（`background-image`、`content`、`mask` 等）同样走 `set_image_bytes`。
- **P22/P23/P31**：`@font-face` 规则、`FontFaceSet`、`SnapshotData.fonts` 的分工见上。

## 状态

**已完成（分支 `electron/p20-page-loading`）**

- 内核（`feat(kernel)`）：`Runtime::set_module_source(Option<ModuleSource>)`（`ModuleSource = Rc<dyn Fn(&Url) -> Option<Rc<str>>>`，**同步**、宿主从预取缓存回答）与
  `Runtime::set_import_map(Option<ImportMap>)`（`ImportMap::parse(json, base)`，`imports` + `scopes`，Rust 侧解析）。`src/esm.rs` 对宿主地址
  （非 `file/data/blob/node`）按网页规则解析相对说明符，`import.meta.url` 是真实地址。契约已改在 P03 §4c-3 与本文件备注里。测试：`tests/module_source.rs`。
- verve-dom：`Loader`/`LoadRequest`/`LoadResult`/`LoadError`/`Destination`/`LoadEvent`/`Ready`（`src/load.rs`），`Options::loader`；
  `html::StreamParser`（`feed/finish/step/insert`，`parse_document` 现在是它的包装，现有测试全过）；`Document::{navigate_initial, load_html, pump_loads, is_loading, take_load_events, url}`。
  - 脚本：classic 同步 / `defer` / `async` / `type=module`（含 `importmap`，静态依赖图预取进缓存）/ 动态插入（含 `script.async = false` 的顺序执行）；
    `document.currentScript`、`document.readyState`、`readystatechange`/`DOMContentLoaded`/`load`/`pageshow`；`document.write` 在解析期同步读入（其中的脚本在 `write` 返回前执行）；
    出错 → `window` 的 `ErrorEvent`（消息带 `Uncaught `），未被取消则进 console；外链失败 → 元素 `error`；`innerHTML` 里的脚本不执行。
  - `<link rel=stylesheet>`（阻塞其后的脚本，`SheetOwner::Link`，移出文档即移除，按文档顺序与 `<style>` 排序）、`@import`（被导入的在导入者之前）、`<img>`（`Request::Image` 经加载器应答；
    放进页面的 `<img>` 当场发起）、`@font-face`/`FontFace.load()`（`Request::LoadFont` 经加载器，应答 `settle_font`）、图标、标题、`<base href>`（`State::base_url`，`baseURI`）、
    事件属性（`onload="…"` 等变成处理器，`<body onload>` 归 `window`）。
  - `fetch`/XHR 的相对地址按 `<base>` 解析，加载器认领的方案（`file`、`app`…）经 `Loader::fetch`，其余交给 Verve 的 fetch（`http/https/data/blob`）。
    页面里 `location.href = …` 在加载器认领的方案/`file:` 上放行（原来只放行 http(s)）。`window.open` → `Request::Open { url, name, features }`。
- verve-electron：`loader.rs` 的 `ElectronLoader`（`file:` 经 overlay（asar）读，MIME 小表，找不到 -6，目录 -2；`data:`；自定义方案经 `FromRenderer::ProtocolRequest`/
  `ToRenderer::ProtocolResponse` 问主进程）；`RendererGuest` 用它加载、把 `LoadEvent` 映射成 `PageEvent`、`tick` 里 `pump_loads`、`next_due` 只在有请求不是在等主进程时才 5 ms 看一眼；
  `Hub::to_renderer(Load)` 在页面已加载过时**换一个 isolate**（`renew_for_navigation`：`stop` 旧的、同一组 `WebPreferences` 新建）；渲染器的 `Request::Navigate` → 主进程
  `MainGuest::page_navigates`（现在无条件放行，回 `ToRenderer::Load`）；`Request::Reload` 由 guest 译成 `FromRenderer::Reload { url }`；`win.loadFile(path, { query, search, hash })`；
  `Hub::record_page_events`/`page_events`（`HeadlessShell::new` 里打开）；主进程用页面的 `PageEvent::TitleUpdated` 设窗口标题。
- 测试：`crates/verve-dom/tests/load.rs`（+ `tests/fixtures/load/`），`crates/verve-electron/tests/page_load.rs`。夹具改动：`hello` 改 `win.loadFile('index.html', { query: process.versions })`（页面自己从
  `location.search` 填版本列表），其测试按页面标题 `Hello Verve` 找窗口（Electron 里窗口标题随页面）；`quick-start` 取消忽略（没有 preload，页面本身加载）；`react-vite` 的点击测试仍忽略
  （P30 布局），渲染由 `page_load` 覆盖。

**冻结的 Chromium 观察**（Electron 39.8.10 / Chromium 142.0.7444.265，隐藏窗口 `loadFile`，探针在本包 scratch 里，页面在 `tests/fixtures/load/`）：`readyState` 在解析完（`interactive`，同时 `readystatechange`）
之后 defer 脚本与 module 脚本**按文档顺序**执行（含内联 module），再 `DOMContentLoaded`、`readystatechange: complete`、`load`；async 脚本在解析结束后才到；动态内联脚本在插入时同步执行；
`async=false` 的动态脚本按插入顺序；失败的外链脚本 `error`；样式表之后的内联脚本能读到样式（`getComputedStyle`），而 link 的 `load` 事件在**该脚本之后**；`document.write` 写入的脚本在 `write` 返回前执行；
module 脚本期间 `currentScript` 为 `null`；`ErrorEvent.message` 为 `Uncaught Error: boom`；被导入的样式在导入者之前。

**第三轮复查的修复**（探针对着 Electron 39.8.10，`show:false`）：

- `file:` 模块不能从托管页面（`https:`/`app:`/`data:`/自定义方案）加载，静态与动态 import 都是 `TypeError: Failed to fetch dynamically imported module: <url>`（Chromium 另在控制台写 `Not allowed to load local resource: <url>`，本实现未写这一行）。
- 事件属性只编译窗口自己文档里、不在 `<template>` 内的节点；`DOMParser`/`createHTMLDocument` 里的 `<body onfocus>` 不会碰 `window`；删除或置空属性会清掉处理器；`onpagereveal`/`onpageswap` 归 `window`。
- `document.write`：只读到插入点（`inserted - tail`），多层嵌套/写入的外链脚本写两次不再越界；对别的文档的 `write` 不走解析器；`async`/`defer`/module 脚本里的 `write` 被忽略并在控制台**警告**
  `Failed to execute 'write' on 'Document': It isn't possible to write into a document from an asynchronously-loaded external script unless it is explicitly opened.`（三种脚本在 Chromium 里文字相同、级别都是 warning）。
- 每个文档的 isolate 有代数：`Hub::from_renderer*` 和主进程的作业都核对代数，被替换的文档不再被执行也不再被听见；`Isolate::retire` 让旧 isolate 不再跑任何作业/定时器。
- 解析器脚本之间做微任务检查点，外链脚本的 `load` 在其微任务之后；`through_fetch` 链被页面改坏时 `fail()` 而不是卡住；字面 `import()` 预取失败不再让整个脚本失败（运行时可 `catch`），预取扫描器认正则字面量；
  外链 `defer`/module 的 `write` 忽略；页面自己的导航被拒（`data:` → 控制台 `Not allowed to navigate top frame to data URL: <url>`，别处的 `file:` → `Not allowed to load local resource: <url>`）不再抛 `SecurityError`，页面留在原地（`blob:` 与其他方案 Chromium 不作声）。
- `with_dom` 的收尾移进一个非泛型的 `settle`；`order_sheets` 原地插入排序；嵌套 `@import` 用路径式排位（c, a, L），解析器造的 `<link>` 由解析器标记（`Dom::mark_parser_made`），`<link>` 的 `load` 在其 `@import` 到齐之后。
- `clone_one` 复制 "already started"；`<template>` 里解析器造的脚本不标 started，放进页面（克隆或内容本身）时执行（Chromium：两者都执行）。
- 模块按重定向后的地址解析其导入（`ImportMap::redirect` 把请求地址指向终点）；XHR 经文档自己的 `fetch`（`Load::dom_fetch`），页面换掉 `window.fetch` 不影响；`a.href`、`pushState`、`FontFace` 按 `<base>` 解析；import map 的 scope 匹配时忽略引用者的片段（内联 module 的 `#module-N`）。
- import map：非字符串地址存成封锁项；多个 import map 合并（先到先得，Chromium 133+ 同；Electron 39 里两张图都生效）；外链 module 求值时抛错 → `load`，解析失败（`ERR_MODULE_NOT_FOUND` 或解析错误，内核给解析错误打隐藏标记）→ `error`。
- 页面的 `require` 在 `file:` 页面加载时改以页面所在目录为基准；`Load::{handled, forced, ordered, linked, import_ranks, …}` 随节点释放清理；`FromRenderer::Reload` 补进 P02 的契约表；
  `page_reloads` 与 `page_navigates` 共用一个函数；`page_load` 的 reload 测试只取前两次 `did-navigate`；`loadFile` 的 `query` 用内核的 `querystring.stringify`；网络方案表只有一份（`load::is_network`）。

体积（第三轮复查后）：`cargo build --release -p verve-gpui --bin electron --features jit` 为 33,833,472 字节，基线 `4ffdcaa6` 为 33,653,760（+179,712，+0.53 %）；`with_dom` 收尾移入非泛型 `settle` 之前是 34,089,472。

**未完成（第三轮复查登记）**

- 页面 `fetch` 到自定义方案时请求的 body 与 headers 没有传给主进程的处理器（`load/net.rs::fetch_response` 固定发空 body、空 headers）：要把 `Request`/`init` 的 headers 与 body（异步取字节）带进 `LoadRequest`。场景：`fetch('app://x/api', { method: 'POST', body: 'a=1' })`。交给 P16。
- 页面自己导航到 `blob:`（Chromium 允许同源 blob 页面）：这里静默不动，因为加载器不认 `blob:`。
- 第一个 module 脚本开始之后才出现的 import map 被忽略（Chromium 133+ 会合并，已解析过的说明符不变）。
- 托管页面 `import` 本地 `file:` 时 Chromium 的那行控制台错误没有写。

**未做 / 已知差异（登记到 P41）**

- `<iframe>` 不做（按任务书）；`window.open` 在渲染器返回 `null`。
- `link.sheet`、`document.styleSheets` 不含 `<link>` 的样式表（CSSOM 只认 `<style>`）；`@import` 的 `media`/`layer`/`supports` 条件被忽略。
- 字体：取到字节后只 `settle_font`，没有 `SnapshotData.fonts`（P31 消费前不做）。`<meta charset>`、非 UTF-8 编码、CSP（解析并忽略，没有执行）不做；`srcset` 只看 `src`；`loading=lazy` 当 eager。
- 图片的 `load`/`error` 事件由 P25 排成 0 ms 定时任务，所以在窗口 `load` **之后**触发（Chromium 在之前）；`complete` 在 `load` 时已为真。
- 内联 `type=module` 脚本的 `import.meta.url` 是 `文档地址#module-<节点号>`（缓存键不能撞），Chromium 是文档地址本身。
- `file:` 页面的 module 不预取（内核自己从磁盘/asar 读）；没有 `nodeIntegration` 的页面按浏览器的规则解析（裸说明符报 `Failed to resolve module specifier`），有的才走 Node 式解析。两个依赖图共享的模块只取一次，每个图都等它到齐才执行。
- 导航时不发 `beforeunload`/`unload`（P13 的 `will-prevent-unload`）；`DidNavigateInPage`（`#hash`、`pushState`）没有从 verve-dom 报出来；`loadURL` 的 promise 仍立即 resolve（P13 改成等 `did-finish-load`/`did-fail-load`）。
- http(s) 子资源经 Verve 的 fetch（`net.rs::through_fetch`）：有单元覆盖（`data:` 走同一条路），没有对着真实网络测过；失败一律 -2（没有细分 `ERR_CONNECTION_REFUSED` 等）。
- 渲染器的网络权限策略仍是 Verve 默认（P16 定）。

**给其他包的备注**

- **P13**（`PageEvent` → `webContents` 事件）：`DidStartLoading` → `did-start-loading`（+`did-start-navigation`）；`DidNavigate{url}` → `did-navigate`（+`did-frame-navigate`），**在响应到达时**发，位于
  `did-start-loading` 之后、`dom-ready` 之前；`FirstPaintable` → `ready-to-show` 的条件之一（`<body>` 已读且阻塞样式表已到）；`TitleUpdated(t)` → `page-title-updated`（可取消；现在 `MainGuest` 直接设窗口标题，
  P13 把它改成事件之后）；`FaviconUpdated(urls)` → `page-favicon-updated`；`DomReady` → `dom-ready`；`DidFinishLoad` → `did-frame-finish-load`、`did-finish-load`、`did-stop-loading`；
  `DidFailLoad{code, description, url}` → `did-fail-load(event, code, description, url, true, …)` 然后 `did-stop-loading`。顺序是 `Started, Committed, FirstPaintable, Title, [Icons], DomContentLoaded, Loaded`
  （`Failed` 之前只有 `Started`）。导航钩子：`MainGuest::page_navigates(contents, url, replace)`——P13 包上可取消的 `will-frame-navigate`/`will-navigate`，不放行就不发 `ToRenderer::Load`。
  `Hub::record_page_events()` / `Hub::page_events(contents)` 是测试用的记录。`loadFile` 目前在 `api/browser_window.rs` 里几行拼 URL（相对路径按 `app_path`），P13 的 `BrowserWindow` 接手时一并带走。
- **P12**：换页面时旧渲染器只在一处被拆：`hub.rs` 的 `renew_for_navigation`（调 `stop(&slot)` 再 `spawn_renderer`）——挂起的 `invoke`/`sendSync` 等清理接在 `stop` 前；`RendererGuest::apply` 的 `match` 留了 `_ => {}`，
  你的 IPC 分支加在那里；`Hub::preferences_of` 仍是同一组偏好。
- **P16**：协议处理器在主进程。`MainGuest::from_renderer` 现在忽略 `FromRenderer::ProtocolRequest { call, request }`；你的处理器跑完后回 `hub.to_renderer(contents, ToRenderer::ProtocolResponse { call, response })`。
  因为导航换 isolate，**每个新 isolate 要重新登记方案**：在 `RendererGuest::new` 里按 Hub 里的注册表调 `register_scheme(scheme)`（现在只提供这个方法，没有 Hub 侧注册表）。
  登记后的方案的 `<script type=module>`、样式、`fetch` 都经它；`app://` 的整个模块图在运行前预取，`protocol` 夹具的 `a_site_is_served_from_a_custom_scheme` 仍由你打开。
- **P21**：preload 要在页面脚本之前跑：入口是 `RendererGuest::load`（`navigate_initial` 之前）与 `RendererGuest::new`（isolate 此时是新的）；`document.readyState` 为 `loading` 时 preload 的 `DOMContentLoaded` 监听才有效，
  这在 `navigate_initial` 内的 `begin` 之前挂上即可。`quick-start` 的 preload 没跑，所以它的 `*-version` 还是空的。
- **P30**：`FirstPaintable` 与 `Layout` 输入不变；`react-vite` 的点击测试等布局（`App::shell.click` 需要盒子）。
- **P41**：上面「未做 / 已知差异」每一条。

**体积**：33,838,592 字节（基线 `4ffdcaa6` 33,653,760，+184,832；第三轮复查后 33,833,472，第三轮复查前 34,089,472），见 `size.md` 的 P20 一节。

**评审第一批修订（已做，均为 `fixup!` 提交）**：页面不带 Node 解析（`Runtime::set_web_modules`，`nodeIntegration` 为假时由 `RendererGuest::new` 打开，P21 在 `nodeIntegration` 页面关掉；
错误文本取自 Electron 39.8.10：`Failed to resolve module specifier 'fs'`、`Failed to fetch dynamically imported module: node:fs`）；`LoadRequest::initiator`，`file:` 只给 `file:`
发起者且只给本机（非 `file:` 页面的 `fetch` → `TypeError: Failed to fetch`，XHR/`<script>`/`<img>` 报错——控制台的 `Not allowed to load local resource` 没有复现；`file://server/share`
在 file 页面里 3 秒内没有完成，按拒绝处理；`read_file` 走 asar overlay 再走磁盘，**不**经 Verve 的 `Permissions`，因为这是浏览器的同源文件策略而不是脚本权限）；页面自己的导航：
`data:`、`blob:` 一律拒绝，`file:` 只能从 `file:` 页面（Electron 观察：http→file、任何→data 被拦，file→file 放行）；协议请求号 Hub 内全局唯一（`Hub::protocol_calls`）；
模块图按 URL 记等待者（`Load::mods/waiters/pending`），乱序到达与共享模块已测；`<template>` 内容惰性（`Dom::in_template`，解析器不在 template 内暂停脚本）。

**评审第二批（已做，均为 `fixup!` 提交；编号同评审信）**

- 1 `document.write`：`StreamParser` 保存「插入点」（离已写入部分末尾的距离，`enter_script`/`leave_script` 即 HTML 的 old insertion point），两次 `write` 按序拼接；有阻塞脚本待决时不步进；
  只在解析器插入的脚本（及其中动态脚本）运行时接受写入，async 脚本/定时器的写入被忽略并在控制台报 `Failed to execute 'write' on 'Document': It isn't possible to write into a document from an asynchronously-loaded external script unless it is explicitly opened.`
  （文本取自 Blink 源码的记忆，**没有**在 Electron 39 里对照观察——探针未做）。解析结束后的 `write`（隐式 `document.open`，应清空文档）：**登记**，仍追加到 `<body>`。
- 2 事件属性：属性设置（`Dom::watch_attrs` 的日志）与元素首次进入文档时各读一次，移动不再重复；body 的窗口反射清单取自 HTML 规范；`<body onload>` 能挂上。
- 3 重定向：`through_fetch` 取 `response.url` 作 `LoadResult::url`。4 子资源用安装时留下的原始 `fetch`（`Load::original_fetch`）。
- 5 模块：`web_resolve` 返回枚举 `Web`，绝对 `data:/blob:/file:` 在宿主引用者下交回内核；`ModuleSource` 返回 `Rc<str>`；动态 `import("字面量")` 与 `export … from` 的说明符在预取时一并扫描（`statement()` 按记号读，跳过注释与字符串，
  不再越过 `;`/下一个标记去找 `from`）。**登记**：`file:` 模块所导入的宿主地址不预取；非字面量动态 `import()` 无法预取（给 P41）。
- 6 样式表：`SheetOwner::Import(node, rank)`，`order_sheets()` 在每次到达后调用（迟到的 link 守住元素位置，`@import a; @import b;` → a, b, link）；`media` 不匹配（按当前环境）则不取；
  只有解析器在 `<head>` 里创建、且 `media` 匹配的 link 阻塞脚本与首次绘制，其余只拖住 `load`。**登记**：`href`/`rel` 改变后不重取、移出又插回的 link 不再取（`linked` 集合）、`media` 不随环境变化重新判断。
- 7 动态脚本：空脚本不标记 `started`，之后设 `src`/`text` 才运行（属性日志与子文本进入驱动）；`async` getter 对页面创建的脚本为 true；`type=module nomodule` 照常运行；`createContextualFragment` 的脚本在插入页面时运行
  （`Dom::let_scripts_run`）。8 `connected` 只走最外层根（`outermost`），svg 脚本只运行一次。
- 9 `fetch`/XHR/图片/`navigate`/`http_url` 都用 `base_url()`，并按（DOM 修订号, 文档地址）缓存。10 外链模块求值抛错 → `load`（找不到/语法错 → `error`）。
- 11 import map：非字符串地址只丢这一条并在控制台警告；回退检查（`pkg/../x`）；前缀只对「非 URL 或特殊方案」生效；内联模块按 base URL 解析。
- 12 `renew_for_navigation` 重置 `gone`（单测：重载后第二次崩溃再报）。13 不再固定 1 ms 轮询：只有「有请求不是在等主进程的答复」时才以 5 ms 看一眼（网络），
  等主进程的答复（`ProtocolResponse` 是一个 job，本来就会唤醒页面）时不轮询，所以没人回答的请求不再空转。**登记**：http(s) 子资源仍靠 5 ms 的查看，没有 Verve 侧的完成唤醒。
- 14 **登记**：`ElectronLoader::file_for` 仍在 JS 线程同步读文件——Verve 没有给文件系统用的线程池。15 `location.reload()` 现在是 `FromRenderer::Reload { url }`，`MainGuest::page_reloads`（目前与导航同样放行并换 isolate）；
  跨文档的会话历史（`history.back()`）：**登记**。
- 16 `fixture()` 搬进 `tests/support/mod.rs`；`the_ready_state_names` 覆盖三种；`hello` 用真 Electron 39.8.10 重录，`expected.json` 与已有内容一致（无差异）。
- 17 `Parser::scan` 拆成 `comment/cdata/declaration/tag/open_element`；`handlers.rs`/`script.rs` 的嵌套压平（`Found`、`collect`、`prepare`）；新代码改用 `verve::native`；`let _ =` 改成上报；`cx` 改名；去掉 `_replace` 与 `rejection` 的未用参数、
  `StreamParser::is_finished`；`ProtocolRequest/ProtocolResponse` 挪到枚举区；`destination` 用 Fetch 的名字（`document/script/style/image/font`，`fetch()` 为空串）；相对说明符只在 `verve::url_like`/`is_relative` 里写一处；
  `loadFile` 的查询按 `encodeURIComponent` 编码（`x y` → `x%20y`，与 Node 的 `querystring` 一致；路径仍用 `url` crate 的 `from_file_path`，工具库里没有对外的等价物）；`Load` 的 `parsed/dcl` 合成枚举 `Phase`（`painted` 与它们无关，仍是布尔）。
- 18 `marks.rs` 保留（`dom.rs` 的行数棘轮），`free` 现在忘掉 `started`。
- 顺带修好的既有缺陷：`launcher.rs::scratch_profile` 的目录名 `verve-electron-<pid>-<n>` 与测试 `App` 的脚本目录同名，并行测试里一个应用结束时 Hub 会删掉另一个的目录（`page_load` 偶发 `-6`）；改为 `verve-electron-start-…`。

**评审第二批：未做**

- **登记**：用 `module.registerHooks` 注册 resolve 钩子的页面，`import` 走 `module_hooks::resolve`，`set_local_modules(false)` 的本地文件拒绝不检查这条路径（只有开了 `nodeIntegration` 才可能到达，而那样的页面本来就能 `require('fs')` 读盘）。
- 上面标「登记」的各项。

**范围说明（超出「修改」清单的改动）**：`dom.rs`/`dom/marks.rs`——`started` 标记放在 `Dom` 里；pin/partition 方法为了让 `dom.rs` 不超过 1769 行的棘轮挪进 `marks.rs`；`storage.rs`——为 `lib.rs` 的行数上限从 `lib.rs` 搬出；
`canvas/mod.rs`——一行再导出 `sync_image`，让放进页面的 `<img>` 当场发起；`Hub::to_renderer` 变成 `self: &Arc<Self>`——换 isolate 需要 `Arc<Hub>`；`launcher.rs`——上述临时目录同名；
`src/import_map.rs`/`src/lib.rs`/`src/embed.rs`——`ImportMap::warnings`、`url_like`/`is_relative` 公开、`ModuleSource` 返回 `Rc<str>`。
`src/isolate.rs`——`Isolate::retire`（换页面时旧 isolate 要让它里面的脚本停下、已排队的 job 与到期的定时器不再跑、不再向宿主发任何东西、线程结束；`Isolate::interrupt` 只停正在跑的那一段，做不到这些；
有独立的 `feat(kernel)` 提交和 `tests/isolate.rs` 的测试）；`Runtime::global_require_in`——页面的 `require` 以页面所在目录为基准；`pub use querystring::query_string` 与 `src/querystring.rs` 的改动——`loadFile` 的 `query`
按 Node 的 `querystring` 编码；`pub use esm::is_parse_failure`——区分模块解析失败（元素 `error`）与求值抛错（`load`）；`Runtime::set_local_modules(bool)`——**文档**是否可以读本地文件（`file:` 模块规则按文档的 URL 而非引用者），
verve-dom 在 `install` 时先关（页面在第一次加载前不读本地文件），在 `start_load` 里按文档地址的 scheme 设，内核拒绝每一个 `file:` 模块地址（入口、静态/动态 `import`、import map 目标），契约见 P03 §4c-3。第四轮复查：`through_fetch` 取到的重定向后地址改读安装时留下的 `Response.prototype.url` 原生 getter
（`Load::original_response_url`），页面改写 `Response.prototype.url` 无效（没有离线可用的测试：该路径只在 http(s) 且加载器不认领时走真实网络）。
