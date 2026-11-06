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

### Spike 结论（先于其余实现提交）

**走方案 A（真隔离）。** 三项检查都通过，测试在 `crates/verve-dom/tests/worlds.rs`（`cargo test -p verve-dom --test worlds`，7 个）与内核的 `tests/worlds.rs`（5 个）：

1. 隔离 world 里 `appendChild(createElement('div'))`，主 world 看得到这个节点，两边的对象不相等、expando 互不可见（`a_node_has_an_object_in_each_world_and_expandos_are_not_shared`）；`Array.prototype.push = null` 不影响另一个 world 的 `[].push`、`Node.prototype.marker` 也不串（`a_built_in_of_one_world_is_not_the_others`）。
2. 跨 world 的监听器顺序：一个节点在各 world 的对象**共用同一份监听器列表**（`TargetData.listeners` 是 `Gc<…>`，`host_target_beside` 让新 world 的对象共用兄弟对象的那份），所以顺序就是添加顺序，不需要另编全局序号；每个监听器记下添加它的 world，分发时在该 world 里调用，`this`/`target` 换成该 world 的对象（`Counterpart` 钩子），`Event` 在该 world 里另做一个镜像（原型是该 world 的同名类），监听器对它做的 `preventDefault`/`stopPropagation` 带回原事件（`listeners_of_both_worlds_are_heard_in_the_order_they_were_added`、`a_listener_of_the_other_world_acts_on_the_event`）。`window` 的监听器与 `on…` 属性同理（`window_listeners_and_handlers_of_both_worlds_hear_an_event`）。
3. 主 world 删除节点后，隔离 world 持有的对象仍读得到 `isConnected === false`；`Document::sweep` 看所有 world 的对象（`State::has_object`），任一 world 持有就不回收；两边都放手后才回收（`a_node_an_isolated_world_holds_stays_after_the_page_removes_it`）。

一个 world 的 `State.wrappers`/`links`/`classes` 键里带 `WorldId`，当前 world 取自 `verve::current_world(context)`（当前正在运行的函数所属的 Realm），不是调用者的。


### 做了什么

**内核（`src/`，每项一个 `feat(kernel)` 提交，测试都在 `tests/` 里只用 `verve::native` 工具箱）**

- `Runtime::create_world` / `enter_world` / `world_global` 与自由函数 `make_world`（给在原生函数里只有 `Context` 的宿主用）、`current_world`、`world_realm`、`world_of_global`（`src/world.rs`）：建一个 Realm，在其中重装 Verve 的全部 web 全局（`globals::install`），事件循环、定时器队列、微任务队列共享。以前存在 `Context` 数据里、按类型取的几份类（`dom_events::Dom`、`WindowTarget`、`fetch`/`streams` 的 `Classes`）改存在 Realm 的 host-defined 里，由当前 Realm 取；`messaging::poll` 逐 world 轮询各自的端口。**`AbortController` 搬到 `src/dom_events/abort.rs`**，让 `dom_events.rs` 不超过 `ci/style/large-files.txt` 记的行数（并把该行数改小）。
- 跨 world 的监听器顺序：同一节点在各 world 的对象共用一份监听器列表（见 Spike），`dom_events/worlds.rs` 管跨 world 调用与事件镜像；`host_target_beside`、`set_world_counterpart` 是给宿主的入口。没有另编「全局注册序号」：共用一份列表就是添加序。
- `verve::blob::{file_with_path, path_of_file}`（`web.rs` 里实现，经 `blob` 模块公开；与任务书的签名相比多了 `kind`，且 `path_of_file` 要 `&mut Context`，因为路径存在只有 Rust 读得到的 `JsWeakMap` 里，脚本从 `File` 上找不到它）。给 P24（拖入文件）与 P32（`<input type=file>`）造带路径的 `File`。
- 动了的 `src/` 文件：`world.rs`（新）、`dom_events.rs`、`dom_events/{worlds,abort}.rs`、`fetch.rs`、`streams.rs`、`messaging.rs`、`web.rs`、`blob.rs`、`lib.rs`（导出）。除 `README` 允许的三项外没有别的改动。

**verve-dom**

- `Document::create_world` / `verve_dom::create_world(context)`：新 world 里重装窗口的全部类、`window`、`document`；`State.wrappers/links/classes` 键里带 `WorldId`；`State::class_of` / `class_prototype` 取当前 world 的类；`bind::node::relink` 对每个 world 各做一遍；`sweep` 看所有 world（`State::has_object`）。
- `Document::navigate_initial_with(runtime, url, before)`：在文档已是 `loading`、还什么都没取的时候先跑 `before`，preload 就挂在这里（`document.readyState === 'loading'`）。**这是 `load.rs` 里一处任务书没列的小改动**。
- `css/cascade.rs` 的 `Origin::User`（`insertCSS(css, { cssOrigin: 'user' })`）：夹在用户代理和作者之间，`!important` 在作者之上、用户代理之下；`SheetOwner::User`。单独一个提交。
- `verve_dom::{insert_stylesheet, remove_stylesheet}`（`webFrame.insertCSS` 用）。

**verve-electron**

- `preload.rs`：`prepare`（装页面的设置、去掉没有 Node 的页面上的 Node 全局、为 `contextIsolation` 的页面建 preload 的 world）与 `run`（文档开始加载时跑 preload；失败发 `FromRenderer::PreloadError`，控制台先写 `Unable to load preload script: <path>` 再写错误）。沙箱 preload：`require` 只认 `electron`（及 `electron/renderer`、`electron/common`）与 `events`/`timers`/`url`（及 `node:` 前缀），其余抛 `Error: module not found: <name>`；`process` 是裁剪过的（键与 Electron 39.8.10 观察到的一致，`env` 是真实环境）；`this` 是 `window`；`__filename` 未定义。非沙箱：Node 的 `require`（相对 preload 所在目录），`this` 是 `exports`。相对路径的 `preload` 不跑（Electron 同，且没有 `preload-error`）。`additionalArguments` 追加到 `process.argv`。
- `api/context_bridge.rs`：`exposeInMainWorld`、`exposeInIsolatedWorld`、`executeInMainWorld` 与 `bridge_value`（规则见代码头注释，数据来自 `bridge-matrix` 的 Electron 39.8.10 记录：暴露的 API 深度冻结、属性 `writable: false, configurable: false, enumerable: true`；函数是目标 world 里的新函数、参数与返回值反向/正向穿越、`this` 丢弃、回程还原为原函数、每次穿越新建；Promise 代理；错误一律成目标 world 的 `Error`（只带 `message`、`stack`）；抛出非 Error 变 `An unknown exception occurred in the isolated context…`；返回值穿越失败的错误带 `Uncaught ` 前缀；对象深拷贝自有可枚举键（含 Symbol 键）、getter 取值、原型丢失、环与共享引用保持；稀疏数组的洞变 `undefined`；深度上限 1000；`WeakMap`/`Proxy` 抛 `An object could not be cloned.`；DOM 节点是目标 world 里同一节点的对象；日期、正则、Map、Set、ArrayBuffer、Blob、`File`（带路径的仍带路径）按结构化克隆）。
- `api/web_frame.rs`：缩放（`setZoomFactor`/`setZoomLevel` 发 `FromRenderer::ZoomChanged`，主进程收到后不记（没有人读，所以没有字段）；`getZoomFactor`/`zoom-changed` 与 Shell 的缩放是 P13/P30 的，它们要用就自己在 `main_guest` 的 `ZoomChanged` 分支里留）、`insertCSS`/`removeInsertedCSS`（含 `cssOrigin: 'user'`）、`executeJavaScript`/`executeJavaScriptInIsolatedWorld`、`setIsolatedWorldInfo`（记录）、`getResourceUsage`、`clearCache`、框架树（只有顶层：`top` 每次是新对象，但类每个 world 只造一次，其余 `null`）、拼写检查相关抛不支持。`api/web_utils.rs`：`getPathForFile`。
- `nodeIntegration: true` 且 `contextIsolation: false` 的页面：`require`（相对页面所在目录）、`module`、`__filename`、`__dirname`、`process`、`Buffer`、`global`、`setImmediate`；`nodeIntegration` 但有隔离时 Node 只在 preload 的 world 里；页面没有 Node 时 `process`/`Buffer`/`global`/`setImmediate`/`clearImmediate` 从页面的全局上删掉（沙箱 preload 在页面 world 里跑时拿着它们的引用）。`Runtime::set_web_modules` 改为「页面有 Node 则关」。
- `ipcRenderer` 每个 world 一个（在该 world 里造，主进程发来的消息逐 world 触发，`invoke` 的结果在发起调用的 world 里造）。`event.sender` 是各 world 自己的 `ipcRenderer`，它的方法在原型上、自有可枚举键只有 `_events…`，所以经 `contextBridge` 暴露 `ipcRenderer.on` 的回调时，回调拿到的 `event.sender` 拷贝里没有方法（与 Electron 一样）。
- `FromRenderer::{PreloadError, ZoomChanged}`、`WebPreferences.additional_arguments`（`browser_window.rs::web_preferences` 里几行；P13 把解析搬进 `api/web_preferences.rs` 时顺手搬走）。
- 主进程：`contents_ipc::preload_error` 在 P12 的 `contents_object` 上发 `preload-error(event, path, error)`。

### 没做 / 与 Electron 的已知差异

- **electron-vite 默认模板的夹具**：没做。脚手架要从 npm 下载并执行远端的生成器，被环境的权限拒绝，没有绕过。
- `.mjs` preload（仅 `sandbox: false`）：没做。`exposeInIsolatedWorld` 对 999 以外的编号会按需建 world，但没有 `setIsolatedWorldInfo` 的 CSP/来源执行。
- 缩放只记录：文档视口不随 `webFrame` 缩放重算，Shell 的缩放等 P30。
- `crashReporter` 只有 `addExtraParameter`/`removeExtraParameter`/`getParameters` 三个空实现；`nativeImage` 已接上（P14 合并后，`preload.rs::electron_exports` 用 `api::native_image::make`），桥矩阵里 `require('electron')` 的键与 Electron 一致，不再有已知差异。
- 沙箱 `process` 的 `getHeapStatistics`/`getCPUUsage`/`getProcessMemoryInfo`/`getSystemMemoryInfo`/`getBlinkMemoryInfo` 返回全 0 的记录，`uptime()` 为 0，`getCreationTime()` 为 `null`；`argv` 是原 `argv` 加 `additionalArguments`（Electron 的 argv 里还有 Chromium 的开关）。
- Node 的内置模块里只有 `events` 每个 world 一份（`modules.rs` 的缓存按 world 分键；沙箱 `require('events')` 与 `process`、`ipcRenderer` 继承的 `EventEmitter` 都是 preload world 自己的，所以页面的 `Object.prototype` 不在它们的原型链上）。`Buffer`、`setImmediate`、`process` 等全局每个 world 自己造。其余 Node 内置（`fs`、`path`、`stream`…）的 `require` 缓存仍所有 world 共用页面 Realm 造的那一份：页面改 `Function.prototype` 之类的 getter，仍可能在 `sandbox: false` 的 preload `require` 它们并用到时，以 preload 的对象为 `this` 运行。沙箱 preload 的 `require` 只放行 `events` 与 `timers`/`url`，碰不到这个口子。
- 经桥的值：带偏移的类型化数组视图只拷贝视图覆盖的字节（Electron 拷贝整个底层 buffer 并保留偏移）；`Event` 没有自有的 `isTrusted`；`window` 经桥不会被深拷贝（给目标 world 自己的 `window`）；`Number`/`String` 包装对象按结构化克隆。
- 事件镜像：跨 world 的监听器拿到的是该 world 里同类的另一个 `Event` 对象（状态拷贝，`preventDefault`/`stopPropagation`/`stopImmediatePropagation` 带回；`composedPath` 等按拷贝时的路径）。
- 没有 `about:blank` 初始文档的 preload：preload 在 `Load` 到来、文档开始加载时才跑，没有 `Load` 的页面不跑。
- 其他 world 里的观察器（`MutationObserver` 等）：回调在创建它的 world 里调用，但交付的记录对象的类取的是交付时当前 world 的，混用 world 时可能是页面的类。

### 给其他包的备注

- **P13**：`executeJavaScriptInIsolatedWorld` 用 `preload::world_numbered(number, context)` 取（按需建）world，再用 `api::web_frame::evaluate(code, world, context)`（在该 world 里求值并报告错误）；`webContents.getZoomFactor`/`zoom-changed` 要自己在 `main_guest` 的 `FromRenderer::ZoomChanged` 分支里记因子（P21 不记）；`renderer_guest.rs` 里 P21 只多了 `preload::prepare`（`new`）、`preload::run`（经 `navigate_initial_with`）与 `require_from` 里一处，`BeforeUnload`/`Unload` 的分支互不相干。`WebPreferences` 解析搬走时带上 `additional_arguments`。
- **P14**：`nativeImage` 已在 `preload.rs::electron_exports` 里接上；非沙箱 `nodeIntegration` 页面的 `clipboard`/`nativeImage` 走 `renderer_module`（也是 `electron_exports`）。
- **P16**：`session.setPreloads`/`registerPreloadScript` 的注册表接在 `preload::run`：现在只读 `webPreferences.preload`，把它们的路径列表放在它前面逐个 `run_preload` 即可（`ran` 标志按「整批」算）。
- **P24 / P32**：造带路径的 `File` 用 `verve::blob::file_with_path(context, bytes, name, kind, path)`（在要交给的 world 里调用）；`webUtils.getPathForFile` 就读它；经 `contextBridge` 的 `File` 保留路径。
- **P41**：「没做 / 已知差异」每一条；`tests/preload.rs` 的 `KNOWN_DIFFERENCES` 是矩阵里还不一样的行。
- **P22 / P23**：`Origin::User`；`SheetOwner::User`。

### 验证与体积（未完成的部分）

- 开发时在本机跑过并通过：`cargo test -p verve --test worlds --test file_path` 与相关内核测试、`cargo test -p verve-dom`（全部）、`cargo test -p verve-electron --test preload`（12 个，含 289 行的 bridge-matrix，`KNOWN_DIFFERENCES` 之外逐行一致）、`tests/fixtures.rs`（`preload-isolated`、`preload-node` 取消忽略并通过，`quick-start` 现在检查版本号被 preload 填上）、`cargo clippy --workspace --all-targets -- -D warnings`（干净）。
- **没有跑**：`cargo test -p verve`（含 `--features jit`）、`-p verve-gpui` 的全量、release 体积（`electron.exe` 的字节数未测）。机器过载，协调者统一跑。
- `tests/pipeline.rs::a_main_script_that_never_returns_is_interrupted_by_shutdown` 在本机负载下失败（`for(;;){}` 的主脚本没被中断）；与本包无关的竞态嫌疑（关停在脚本开始前就发出），未在基线上对照。

### 复审后改的（隔离与结构）

- 隔离：桥的拷贝一律在**来源** world 里读（`is_enumerable`、`Cloned::from_value`、blob、`promise.then` 都在来源 world 里跑，不再对页面可改的目的 world 内置传来源对象）；`DomEvents` 跨 world 镜像事件时，`detail`/`data`/`error`/`reason`/`promise`/`ports` 等字段按结构化克隆进目的 world（克隆不了则 `null`），类按来源事件自己的 Web 类名（`web_class` 登记的原型）在目的 world 里找，不读页面的 `constructor.name`；观察器（Mutation/Resize/Intersection）记下造它的 world，在那个 world 里造记录并回调。
- 漏洞：非节点非窗口的目标（`matchMedia`、`document.fonts`）本来就属于监听者的 world，不再被跳过；`Held` 现在是真正的 `Trace`（`Dom`、`Classes`、`Messaging` 等派生 `Trace`，经 `Gc` 持有），丢弃 Runtime 能回收 Realm（`tests/worlds.rs::a_dropped_runtime_frees_its_realms`）。
- 改了简报清单之外的文件，原因：`api/ipc_renderer.rs`（每 world 一个 `ipcRenderer`、按 world 触发与结算）、`api/contents_ipc.rs`（`preload-error` 事件；`zoom` 字段已删）、`main_guest.rs` 与 `msg.rs`（`PreloadError`/`ZoomChanged`/`additional_arguments` 的消息）、`api/browser_window.rs`（`additionalArguments` 的解析）、`api/mod.rs`（挂 `context_bridge`/`web_frame`/`web_utils`）；verve-dom 的 `bind/*.rs`、`css/cascade.rs`、`css/styler.rs`（`class(world, …)` 签名让每个 world 有自己的类和包装，用户样式表来源）、`load.rs`（`navigate_initial_with`）；内核的 `fetch.rs`、`streams.rs`、`messaging.rs`、`modules.rs`、`vm.rs`、`web.rs`、`blob.rs`、`native.rs`、`lib.rs`、`dom_events.rs` 与 `dom_events/{worlds,abort}.rs`（每 realm 的类数据、`in_world`/`in_realm`、`native::error`、`events` 按 world 分键）。
- 公共辅助：`verve::in_world`/`verve::in_realm`（进 realm、运行、还原）取代各处手写；`native::error` 取代两处同样的 `Error` 构造；`preload::error_parts` 是错误名与消息的唯一读法。
