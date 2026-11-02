# P21 preload、contextBridge 与上下文隔离

**依赖**：P20、P12  **规模**：L  **修改**：`crates/verve-dom/src/{state.rs,bind/node.rs,bind/mod.rs,bind/events.rs,lib.rs}`（多 world 包装）；`crates/verve-electron/src/{renderer_guest.rs,api/context_bridge.rs,api/web_frame.rs,api/web_utils.rs,preload.rs}`

## 目标

按 Electron 39 的 `webPreferences` 语义运行渲染器：

| 组合 | 行为 |
|---|---|
| 默认：`contextIsolation: true`、`sandbox: true`、`nodeIntegration: false` | preload 在**隔离 world** 里跑，只能 `require` 沙箱白名单；页面看不到 Node；通过 `contextBridge` 暴露 API |
| `sandbox: false`（`contextIsolation` 仍为 true） | preload 在隔离 world，拥有完整 Node `require` |
| `contextIsolation: false` | preload 与页面共享同一 world；`contextBridge` 抛错（与 Electron 一致） |
| `nodeIntegration: true` + `contextIsolation: false` | 页面脚本也有 `require`、`module`、`process`、`Buffer`、`global`、`setImmediate` |

外加渲染进程侧的 `webFrame`、`webUtils`。完成后 electron-quick-start（preload 里改 DOM）、官方
contextBridge 示例（`window.api.ping()` → `ipcRenderer.invoke`）、electron-vite 模板的 preload 都能跑。

## 背景（先读）

- Electron 文档：`context-isolation.md`、`sandbox.md`、`api/context-bridge.md`（含"参数/错误/返回值类型支持"表）、
  `api/web-frame.md`、`api/web-utils.md`、`tutorial/process-model.md#preload-scripts`。
- Boa：`boa/core/engine/src/context/mod.rs` 的 `Context::create_realm()`（新建 Realm，只有 ECMAScript
  内置对象）和 `enter_realm(realm) -> Realm`（切换当前 Realm，返回旧的）。同一个 `Context`、同一个堆，
  两个 Realm 有各自的全局对象和各自的内置原型（`Array.prototype` 不同）。
- verve-dom 的"一个节点一个对象"：`crates/verve-dom/src/state.rs` 的 `State.wrappers: HashMap<NodeId, WeakJsObject>`、
  `links`、`classes: HashMap<&'static str, (JsObject, JsObject)>`（构造函数和原型）；
  `crates/verve-dom/src/bind/node.rs` 的 `wrap` / `make_wrapper` / `existing_wrapper` / `link` / `relink`；
  `state::shared(context)` 用 `context.get_data::<Shared>()`——**按 Context 存，不按 Realm**，两个 Realm 共享同一个 `State`。
- 事件：`crates/verve-dom/src/bind/events.rs` 的 `parent()`（事件路径）；监听器存在节点的 JS 对象上
  （Verve 的 `dom_events::host_target`）。
- Verve 内核的全局对象安装：`src/web.rs` 的 `install(context)` 等——只装在 `Runtime::new` 时的默认 Realm。
- `Runtime::global_require()`（`src/lib.rs`）：Electron 的 `window.require`。
- `Document::set_stylesheet` / `remove_stylesheet`（`crates/verve-dom/src/lib.rs`）：`webFrame.insertCSS` 用它。

## 先做 Spike（第一周，必须先交一份结论）

**问题**：verve-dom 能否让同一个 DOM 节点在两个 Realm 各有一个包装对象？

做法：在分支上把 `wrappers` 的键改成 `(WorldId, NodeId)`、`classes` 改成按 world 存，给隔离 world
`install_classes` 一次，跑下面三个检查：

1. 隔离 world 里 `document.body.appendChild(document.createElement('div'))`，主 world 看得到这个 div，
   且两边的 `div` 对象**不相等**、各自的 expando 互不可见；
2. 两个 world 各在同一按钮上 `addEventListener('click')`，`press(button)` 后两个监听器都被调用，
   顺序与注册顺序一致（Chromium 行为：一个节点只有一份监听器列表，跨 world 按注册顺序）；
3. 主 world 删除节点后，隔离 world 持有的包装仍能读 `isConnected === false`，`Document::sweep` 不会提前回收
   被任一 world 持有的子树。

**决策点**：
- 三项都能在 ~1 周内做稳 → 走**方案 A：真隔离**（下文"接口契约"按 A 写）。
- 做不到（尤其第 2 项的跨 world 监听器顺序、第 3 项的 GC）→ 走**方案 B：同 Realm 降级**，并在 PR 与
  `docs/electron/compat.md`（P41）里写清行为差异。

**方案 B（降级）的定义**：preload 在页面的同一个 Realm 里，以 CommonJS 包装函数的形式执行
（`(function (exports, require, module, __filename, __dirname, process, Buffer, global) {…})`，由 Rust 构造，
不是 JS 垫片）；`contextBridge.exposeInMainWorld` 仍按下文的**拷贝/代理规则**处理值（所以 API 行为一致）；
`process.contextIsolated` 仍报 `true`。与真实 Electron 的差异：
1. 页面能污染 preload 看到的内置原型（`Array.prototype.push = …` 影响 preload 代码）——**无安全隔离**；
2. preload 写到 `window.xxx` 的属性页面可见；
3. DOM 节点上的 expando 两边共享；
4. 跨"world"的 `instanceof` 总是成立。
降级模式下启动时在 console 打一条一次性警告：`Verve: contextIsolation runs in compatibility mode (no isolation)`。

## 接口契约（方案 A）

### verve-dom：多 world（需新增）

```rust
/// A JavaScript world over the one document: the page's (0) and the isolated ones preloads run in.
pub type WorldId = u32;
pub const MAIN_WORLD: WorldId = 0;

impl Document {
    /// Makes a world: a new realm of the runtime's context, given the window's classes and its
    /// own `window`/`document` objects over the same nodes. Answers its realm, to enter.
    pub fn create_world(&self, runtime: &mut verve::Runtime, id: WorldId) -> Result<boa_engine::realm::Realm, String>;
    pub fn world_realm(&self, id: WorldId) -> Option<boa_engine::realm::Realm>;
}
```

- `State.wrappers` → `HashMap<(WorldId, NodeId), WeakJsObject>`；`links` 同理；`classes` → 每个 world 一份；
  当前 world 由 `context` 当前的 Realm 决定（在 `State` 里保存 `Realm → WorldId` 的映射；`wrap` 先查当前 world）。
- 事件分发：一个节点在各 world 的包装对象各存一份监听器。分发时对路径上的每个节点，按**全局注册序号**
  合并各 world 的监听器后依次调用，每次调用前 `enter_realm` 到该监听器所属 Realm。为此给 Verve 的
  `dom_events` 监听器记录加一个单调递增的注册序号（若需改 `src/dom_events.rs`，与 P03 协调）。
  `Event` 对象本身：每个 world 需要自己的 `Event` 包装（Chromium 如此）；v1 可以让隔离 world 的监听器
  拿到同一个 Event 对象（Realm 不同的对象跨用是合法的 JS），**登记为已知差异**。
- 存活：任一 world 的包装存活即保住节点子树（`Document::sweep` 检查所有 world）。
- 隔离 world 也需要 Verve 的 web 全局对象（`console`、定时器、`Event`/`EventTarget`、`fetch`、`URL`、
  `TextEncoder`…）：依赖 P03 提供"在当前 Realm 安装 Verve 全局对象"的内核入口
  （如 `verve::install_globals(context)`，在 `enter_realm` 之后调用）。定时器、微任务队列两个 world 共享
  （同一个事件循环，与 Chromium 一致）。

### preload 执行（`crates/verve-electron/src/preload.rs`，需新增）

- 时机：P20 加载管线里"文档对象已建、解析尚未开始"的钩子（需 P20 暴露 `on_document_created`）。
  此时 `document.readyState === 'loading'`，preload 里加的 `DOMContentLoaded` 监听器会触发。
- 来源与顺序：`session.registerPreloadScript`/`session.setPreloads`（P16）中的脚本先，`webPreferences.preload` 后。
- 包装：CommonJS 函数包装（Rust 侧构造），参数 `exports, require, module, __filename, __dirname`，外加
  沙箱模式下的 `process, Buffer, global, setImmediate, clearImmediate`。`.mjs` preload：仅 `sandbox: false`
  时支持（Electron 28+ 规则），走 `Runtime::load_esm`。
- 沙箱 `require` 白名单（`sandbox: true`）：`electron`（只含 `contextBridge`、`crashReporter`、`ipcRenderer`、
  `nativeImage`、`webFrame`、`webUtils`）、`events`、`timers`、`url`；其他一律抛
  `Error: module not found: <name>`（与 Electron 文字一致，用真实 Electron 核对后冻结）。
  沙箱 `process` 只有：`type`、`platform`、`arch`、`version(s)`、`env`（**空对象**? 以 Electron 实测为准）、
  `argv`、`execPath`、`pid`、`sandboxed`、`contextIsolated`、`isMainFrame`、`crash`/`hang`/`getHeapStatistics` 等
  （逐个核对 `sandbox.md`）。
- 非沙箱：完整 Node `require`（相对 preload 文件所在目录解析）。
- preload 抛错：主进程 `webContents` 发 `preload-error`（`event, preloadPath, error`），页面继续加载。
- `webPreferences.additionalArguments` → 追加到渲染器 `process.argv`。

### contextBridge（`api/context_bridge.rs`）

- `exposeInMainWorld(apiKey, api)`、`exposeInIsolatedWorld(worldId, apiKey, api)`、
  `executeInMainWorld({ func, args })`（Electron 35+）。
- `contextIsolation: false` 时调用 → 抛 `Error('contextBridge API can only be used when contextIsolation is enabled')`。
- `apiKey` 已存在于目标 `window` → 抛 `Error('Cannot bind an API on top of an existing property on the window object')`。
- 在目标 world 的 `window` 上定义属性（可写性/可配置性以 Electron 实测为准并冻结）。
- **值穿越规则**（递归，按 Electron 文档表；实现为 Rust 函数 `bridge_value(value, from_realm, to_realm, depth)`）：

| 源值 | 到对面 |
|---|---|
| string / number / boolean / null / undefined / bigint | 原样 |
| Object（普通对象） | 深拷贝**自有可枚举**键，原型丢失（对面是对面 Realm 的 `Object.prototype`）；getter 在拷贝时求值 |
| Array | 深拷贝（对面 Realm 的 `Array`） |
| Function | 代理：对面得到一个新函数，调用时把 `this` 丢弃、参数按本表反向穿越、在源 Realm 执行、返回值按本表穿越；抛出的错误按 Error 规则穿越 |
| Promise | 代理：对面得到一个新 Promise，源 Promise settle 时用穿越后的值 settle |
| Error | 拷贝 `message`（以及 `name`/`stack`? 以 Electron 实测冻结），对面 Realm 的 `Error` |
| 可结构化克隆的（Date、RegExp、ArrayBuffer、TypedArray、DataView、Map、Set、Blob/File…） | 结构化克隆（复用 Verve 的 `clone_out`/`clone_in`，P03 开放） |
| DOM 节点 | 对面 world 中同一节点的包装（方案 A）；方案 B 原样 |
| Symbol、WeakMap/WeakSet、Proxy 之外的不可克隆值 | 抛 `Error`（文字以 Electron 实测冻结） |
| 循环引用 | 以 Electron 实测为准（预期：同一对象在一次穿越中只拷贝一次并保持引用关系，或抛错）；深度上限 1000 |

- 已代理过的函数穿越回来时还原为原函数（不层层套代理）。

### webFrame（`api/web_frame.rs`，渲染器的 `require('electron').webFrame`）

| API | 实现 |
|---|---|
| `setZoomFactor(f)` / `getZoomFactor()` / `setZoomLevel(l)` / `getZoomLevel()` | 发 `FromRenderer`（需新增 `ZoomChanged(f64)`）给 Shell 缩放；文档视口按 CSS px 重算（`Document::set_viewport`）；level ↔ factor：`factor = 1.2^level` |
| `setVisualZoomLevelLimits(min, max)` | 记录，Shell 的捏合缩放用（v1 可只记录） |
| `insertCSS(css, { cssOrigin })` → key | `Document::set_stylesheet(key, css)`；`cssOrigin: 'user'` 需要级联里的 user origin（`css.rs` 的 `Origin` 需新增 `User`，P22 协调） |
| `removeInsertedCSS(key)` | `Document::remove_stylesheet(key)` |
| `executeJavaScript(code, userGesture?, callback?)` → Promise | 在主 world 求值；结果是 Promise 时等待；结果结构化克隆 |
| `executeJavaScriptInIsolatedWorld(worldId, scripts[], userGesture?)` | 在指定 world；`scripts` 为 `{ code, url? }` |
| `setIsolatedWorldInfo(worldId, { securityOrigin, csp, name })` | 记录 |
| `getResourceUsage()` | 返回结构正确的数字（images/scripts/cssStyleSheets/xslStyleSheets/fonts/other） |
| `clearCache()` | 无操作 |
| `top` / `parent` / `firstChild` / `nextSibling` / `findFrameByName` / `findFrameByRoutingId` / `getFrameForSelector` / `routingId` / `frameToken` | v1 只有顶层 frame：`top === webFrame`，其余 `null` |
| `setSpellCheckProvider` / `isWordMisspelled` / `getWordSuggestions` | 不支持（抛 `ERR_NOT_SUPPORTED`，见 conventions） |

### webUtils（`api/web_utils.rs`）

- `getPathForFile(file)`：返回宿主创建该 `File` 时记录的绝对路径；页面自己 `new File()` 的返回 `''`。
  路径存在 `File` 的内部槽里（不是 JS 可见属性）——需要 P03 提供"带路径的 File"内核构造函数
  （如 `verve::web::file_from_path(context, path, name, mime)`，惰性读取内容）。来源：P24 的拖放、P32 的
  `<input type=file>`。

### nodeIntegration（`nodeIntegration: true` 且 `contextIsolation: false`）

- 页面 world 全局：`require`（`Runtime::global_require()`，相对页面文件所在目录；非 file:// 页面相对
  `process.cwd()`）、`module`、`exports`、`__filename`、`__dirname`（file:// 页面时为 HTML 文件路径/目录）、
  `process`（`type: 'renderer'`）、`Buffer`、`global`（=== `window`）、`setImmediate`、`clearImmediate`。
- `nodeIntegration: true` 但 `contextIsolation: true`：Node 只在 preload 的隔离 world（Electron 行为），页面没有。
- `nodeIntegrationInWorker`、`nodeIntegrationInSubFrames`：v1 不支持，忽略并登记。

### process（渲染器）

`process.type === 'renderer'`、`process.contextIsolated`、`process.sandboxed`、`process.isMainFrame === true`、
`process.contextId`（字符串）。由 `RendererGuest` 在建 Runtime 后设置（Verve 的 `src/process.rs` 若无写入入口，
与 P03 协调）。

## 步骤

1. Spike（见上），交结论文档（写进本文「状态」节）再继续。
2. 方案 A：verve-dom 多 world（包装、类、事件合并、sweep）；或方案 B：同 Realm 包装执行。
3. preload 执行器：时机钩子、包装函数、沙箱 require 白名单与沙箱 `process`、`preload-error`。
4. contextBridge 与 `bridge_value`（含函数/Promise 代理与回程还原）。
5. webFrame、webUtils。
6. nodeIntegration 的全局注入。
7. 测试与实测冻结：在真实 Electron 39 上跑 `tests/electron/apps/bridge-matrix/`（一张值类型 × 方向的表），
   把输出冻结为期望值。

## 验收

- `cargo test -p verve-dom --test worlds`（方案 A）：Spike 的三项检查。
- `cargo test -p verve-electron --test preload`（无头）：
  - 默认 webPreferences：preload 能 `require('electron').ipcRenderer`，`require('fs')` 抛 `module not found`；
    页面 `typeof require === 'undefined'`；`window.api` 由 `exposeInMainWorld` 提供；
  - `bridge-matrix` 的每一格与冻结的 Electron 结果一致（函数代理、Promise、Error、对象原型丢失、
    Symbol 抛错、回程函数还原）；
  - 方案 A：页面 `Array.prototype.push = null` 后 preload 中的 `[].push` 正常；
  - `contextIsolation: false` 时 `contextBridge.exposeInMainWorld` 抛错；
  - `nodeIntegration: true, contextIsolation: false`：页面 `require('path').join` 可用，`__dirname` 正确；
  - `webFrame.insertCSS` 后 `getComputedStyle` 生效，`removeInsertedCSS` 后恢复；`executeJavaScript` 返回值；
  - preload 抛错 → 主进程收到 `preload-error`，页面照常 `did-finish-load`。
- electron-quick-start、electron-vite 默认模板（产物提交在夹具里）无头加载成功。

## 不做

子 frame 与 `nodeIntegrationInSubFrames`；Worker 里的 Node；拼写检查；`webFrame` 的多 frame 树；
真正的 CSP/`securityOrigin` 执行。

## 给其他包的备注

- **P03**：①在当前 Realm 安装 Verve 全局对象的入口；②开放结构化克隆 `clone_out/clone_in`（README 已列）；
  ③带路径的 `File` 构造；④渲染器 `process` 的字段写入入口；⑤（方案 A）`dom_events` 监听器的全局注册序号。
- **P20**：暴露"文档已建、解析未开始"的钩子供 preload 使用。
- **P22**：`css.rs` 的 `Origin` 增加 `User`（`insertCSS` 的 `cssOrigin: 'user'`）。
- **P02**：`FromRenderer` 追加 `ZoomChanged(f64)`、`PreloadError { path, error }`。
- **P16**：`session.registerPreloadScript` / `setPreloads` 的注册表供本包读取。

## 状态

（实现者完成后填写；Spike 结论写在这里）
