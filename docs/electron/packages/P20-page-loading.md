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

- **P03**：需要内核新增"宿主提供的 ESM 取源回调"——`Runtime::set_module_source(Box<dyn Fn(&Url) -> Option<ModuleSource>>)`
  之类，让 `src/esm.rs` 在遇到非 `file/data/blob/node` 方案时向宿主要源码（同步、命中缓存），并让
  `import()` 对这些方案不再抛 `ERR_UNSUPPORTED_ESM_URL_SCHEME`。还需要 Rust 侧的 resolve 钩子用于 import map。
- **P02**：`FromRenderer` 追加 `ProtocolRequest`，`ToRenderer` 追加 `ProtocolResponse`；
  `verve_dom::Request::Open` 由单字段改为 `{ url, name, features }`。
- **P25**：提供 `fn image_size(bytes) -> Option<(u32, u32)>`（只读头部，不解码全图），本包调用。
- **P22/P23/P31**：`@font-face` 规则、`FontFaceSet`、`SnapshotData.fonts` 的分工见上。

## 状态

（实现者完成后填写）
