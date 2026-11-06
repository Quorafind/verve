# P16 protocol / session / net

**依赖**：P20（渲染器加载管线）、P12（`Cloned` 与 IPC 路由模式）、P03（下文"需要 P03 追加"的内核钩子）  **规模**：M（偏 L）  **修改**：`crates/verve-electron/src/api/{protocol,session,cookies,web_request,download_item,net}.rs`；`crates/verve-electron/src/msg/page.rs`（追加）；`crates/verve-dom/src/bind/document.rs`（仅 `document.cookie` 改接后端）；内核钩子由 P03 落在 `src/`

## 目标

| 模块 | 范围 |
|---|---|
| `protocol` | `registerSchemesAsPrivileged([{scheme, privileges}])`（只能在 `ready` 前调用）、`handle(scheme, handler)`、`unhandle`、`isProtocolHandled`；旧 API（Electron 25 起 deprecated，仍需支持，首次调用 `process.emitWarning` 一次）：`registerFileProtocol`、`registerBufferProtocol`、`registerStringProtocol`、`registerHttpProtocol`、`registerStreamProtocol`、`unregisterProtocol`、`isProtocolRegistered`、`interceptFileProtocol` 等 `intercept*`、`uninterceptProtocol`、`isProtocolIntercepted` |
| `session` | `session.defaultSession`、`session.fromPartition(partition, {cache})`、`session.fromPath(path)`；实例 `cookies`、`webRequest`、`protocol`（会话级）、`setUserAgent`/`getUserAgent`、`setProxy`/`resolveProxy`/`forceReloadProxyConfig`/`closeAllConnections`、`setPermissionRequestHandler`/`setPermissionCheckHandler`/`setDevicePermissionHandler`（后者存而不用）、`clearCache`/`getCacheSize`、`clearStorageData({origin,storages,quotas})`、`flushStorageData`、`clearAuthCache`、`clearHostResolverCache`、`setDownloadPath`、`downloadURL`、`getStoragePath`、`isPersistent`、`setPreloads`/`getPreloads`（P21 读取）、`setSpellCheckerEnabled` 等拼写检查（存值、无效果）、`setSSLConfig`（映射到 TLS 最小/最大版本）、`setCertificateVerifyProc`、事件 `will-download` |
| `Cookies` | `get(filter)`、`set(details)`、`remove(url, name)`、`flushStore()`、事件 `changed` |
| `WebRequest` | `onBeforeRequest`、`onBeforeSendHeaders`、`onSendHeaders`、`onHeadersReceived`、`onResponseStarted`、`onBeforeRedirect`、`onCompleted`、`onErrorOccurred`，各带 `filter {urls, types}` |
| `DownloadItem` | `setSavePath`/`getSavePath`、`setSaveDialogOptions`、`pause`/`isPaused`/`resume`/`canResume`/`cancel`、`getURL`/`getMimeType`/`getFilename`/`getTotalBytes`/`getReceivedBytes`/`getContentDisposition`/`getState`/`getURLChain`/`getLastModifiedTime`/`getETag`/`getStartTime`/`getCurrentBytesPerSecond`/`getPercentComplete`、事件 `updated`、`done` |
| `net` | `net.request(options)` → `ClientRequest`（Electron 自己的，`Writable`）与 `IncomingMessage`（`Readable`）；`net.fetch(input, init)`（含 `bypassCustomProtocolHandlers`）；`net.isOnline()`、`net.online`；`net.resolveHost(host, options)` |

## 背景（先读）

- `src/network/fetch.rs` 的 `begin`：`match url.scheme()` 目前只认 `http`/`https`/`data`/`blob`，其余以
  `unknown scheme` 拒绝。自定义 scheme 要从这里接入。
- `src/fetch.rs` 的 `outgoing`（`Request` 拆成发送用结构）、`fetched_response`（构造 `Response`）。
- `src/network/http.rs`：HTTP/1.1 客户端（`ClientRequest`/`IncomingMessage` 的 Node 版），`net.request` 基于它。
- `src/network/proxy.rs` 与 `Runtime::set_proxy_env`：现有代理只支持 HTTP 代理与 `NO_PROXY`。
- `src/lib.rs` 的 `Runtime::set_storage_dir`（IndexedDB 目录）；verve-dom 的 `Options::storage`（`localStorage` 后端）。
- `crates/verve-dom/src/bind/document.rs` 约第 460 行：`document.cookie` 目前存在全局隐藏属性 `__cookies` 里，只在内存。
- `src/os.rs`：网络接口枚举（`net.isOnline` 复用）。
- P20 的资源加载器（`Loader`）：页面、`<script src>`、`<link>`、`<img>` 的 URL 都经过它；本包让它认识自定义 scheme。
- Electron 文档 `protocol.md`、`session.md`、`cookies.md`、`web-request.md`、`download-item.md`、`net.md`、`client-request.md`（39.x）。

## 需要 P03 追加的内核钩子

本包不直接改 `src/`，请 P03 提供（若 P03 已合并，则本包开一个小 PR 追加到 P03 的模块里，并更新 P03 文档）：

```rust
// src/browser_net.rs（新增，pub mod browser_net）
/// 一个"浏览器网络上下文"：只影响 WHATWG `fetch`（以及建在它上面的 XHR、页面加载），
/// 不影响 Node 的 `http`/`https`/`net`——与 Chromium 中渲染器网络和 Node 网络分离一致。
pub trait BrowserNet: 'static {
    /// 非 http/https/data/blob 的 scheme，或被拦截的 http(s)/file：返回 Some 则由宿主应答。
    fn custom(&self, url: &url::Url) -> bool;
    /// 宿主应答一次请求：异步，结果经 `Responder` 回到 realm 线程（在 run_due/网络 beat 中投递）。
    fn handle(&self, request: RequestHead, body: Option<Vec<u8>>, respond: Responder);
    /// 发送前：附加 Cookie、改 UA、webRequest 的 onBeforeRequest/onBeforeSendHeaders。
    /// 返回 Decision::Continue(head) | Cancel | Redirect(url)。异步（webRequest 监听者在主进程）。
    fn before_send(&self, head: RequestHead, decide: Decider);
    /// 收到响应头后：Set-Cookie 入罐、onHeadersReceived（可改头、可重定向）。
    fn headers_received(&self, url: &url::Url, status: u16, headers: Vec<(String, String)>, decide: HeadersDecider);
    /// 完成/失败/重定向的通知（onCompleted/onErrorOccurred/onBeforeRedirect），不等待。
    fn notify(&self, event: NetEvent);
    fn user_agent(&self) -> Option<String>;
}
pub struct RequestHead { pub id: u64, pub method: String, pub url: String, pub headers: Vec<(String, String)>,
                         pub resource_type: ResourceType, pub referrer: String, pub initiator: Option<String> }
pub struct Responder(/* 内部：通道 + realm waker */);
impl Responder {
    pub fn head(&self, status: u16, status_text: String, headers: Vec<(String, String)>);
    pub fn chunk(&self, bytes: Vec<u8>);       // 流式响应体，受 fetch 现有的背压机制约束（读者落后时暂停回调）
    pub fn end(self);
    pub fn fail(self, error: NetError);        // 映射为 TypeError: fetch failed + cause（code: 'ERR_FAILED' 等）
}
impl Runtime { pub fn set_browser_net(&mut self, net: Option<Box<dyn BrowserNet>>); }
```

- `fetch` 在 `begin` 里：`browser_net.custom(&url)` 为真时走 `handle`；否则 http(s) 走原路径，但发送前调用
  `before_send`、收到头后调用 `headers_received`。**未设置 `BrowserNet` 时行为与现在完全相同**（`verve run` 不受影响）。
- `ResourceType` 取 Electron webRequest 的 `resourceType` 枚举：`mainFrame`、`subFrame`、`stylesheet`、`script`、`image`、
  `font`、`object`、`xhr`、`ping`、`cspReport`、`media`、`webSocket`、`other`。`fetch`/XHR 填 `xhr`；P20 的加载器填对应类型
  （加载器调用的是同一个 `fetch` 内部入口，带 `resource_type` 参数——P03 为此给 `crate::network::fetch` 加一个
  `pub fn fetch_internal(context, head, body) -> JsPromise` 入口，P20 使用）。
- WebSocket 的握手也应经 `before_send`（类型 `webSocket`），P03 视成本决定是否首版包含；不包含时在 P41 登记。

## 接口契约

### 1. 架构：谁持有什么

```
主进程线程
  SessionRegistry：partition 名 → Session（Rust 结构，Arc<Mutex<SessionState>>，可被多个线程读）
  SessionState { user_agent, proxy, cookie_jar, storage_path, permission handler 是否存在,
                 web_request 各事件是否有监听者 + filter, protocol 表（scheme → 处理方式）, privileged schemes }
  JS 侧：Session / Cookies / WebRequest / Protocol 原生对象，handler 函数只在主进程 realm 里
渲染器线程
  RendererBrowserNet: BrowserNet  —— 持有 Arc<SessionState> 和 Hub 句柄
    custom()/user_agent()/Cookie 附加与入罐：直接读写 SessionState，不经主进程（热路径）
    handle()（自定义 scheme）、before_send/headers_received（仅当有 webRequest 监听者）：发消息到主进程，异步等答复
```

- Cookie 罐（`cookie_jar`）在 Rust 侧，`Arc<Mutex<_>>`：渲染器 `fetch`、`document.cookie`、主进程 `net`、`session.cookies.*`
  都直接操作它；变化时向主进程投递 `CookieChanged`，由主进程发 `changed` 事件。持久化分区写
  `<userData>/Partitions/<name>/Cookies.json`（默认会话为 `<userData>/Cookies.json`），**不追求与 Chromium 的 SQLite 格式兼容**
  （迁移时 cookie 丢失可接受，P41 登记）；退出与 `flushStore()` 时写盘，运行中最多每 30 秒写一次。
- `document.cookie`：给 verve-dom 加 `CookieBackend` trait（`get(url) -> String`、`set(url, line)`），`Options` 增加
  `cookies: Option<Box<dyn CookieBackend>>`；未设置时保持现在的内存实现。改动只限 `bind/document.rs` 的 `cookie` 访问器和
  `Options`——在 PR 描述里通知 P23。
- 新渲染器创建时（P13/P20），按其 `webPreferences.partition`/`session` 找到 `SessionState`，构造 `RendererBrowserNet` 并
  `Runtime::set_browser_net`；同时：`set_storage_dir(<storage_path>/IndexedDB)`、verve-dom
  `storage = FileStorage(<storage_path>/Local Storage/<origin>.json)`、`set_proxy_env(...)`。内存分区（无 `persist:` 前缀）
  这些都不落盘。
- 主进程自己的 `net.fetch`/`net.request` 也走 `BrowserNet`（主进程 Runtime 上设置一个以 defaultSession 为准的实例，
  `net.request({session})` 可指定会话）；**Node 的 `fetch` 全局函数在主进程里不经过会话**（Electron 中主进程的全局
  `fetch` 是 Node/undici 的，不带 Cookie）——因此主进程的 `BrowserNet` 只给 `net` 模块用，不装到全局 `fetch` 上。
  P03 需让 `fetch_internal` 可显式传入 `BrowserNet`，而不仅是 Runtime 级设置。

### 2. 追加到 `msg/page.rs`

```rust
// 渲染器 → 主进程
FromRenderer::Protocol { call: u64, session: SessionId, request: ProtoRequest },   // 自定义 scheme 请求
FromRenderer::ProtocolBody { call: u64, chunk: Option<Vec<u8>> },                  // 请求体（上传流），None = 结束
FromRenderer::WebRequest { call: u64, session: SessionId, stage: WebRequestStage, details: Cloned },
FromRenderer::Permission { call: u64, session: SessionId, permission: String, details: Cloned, check_only: bool },
FromRenderer::Download { url: String, headers: Vec<(String, String)>, suggested_name: Option<String> },

// 主进程 → 渲染器
ToRenderer::ProtocolHead { call: u64, status: u16, status_text: String, headers: Vec<(String, String)> },
ToRenderer::ProtocolChunk { call: u64, bytes: Vec<u8> },
ToRenderer::ProtocolEnd { call: u64, error: Option<NetError> },
ToRenderer::WebRequestReply { call: u64, response: Cloned },   // {cancel, redirectURL, requestHeaders, responseHeaders, statusLine}
ToRenderer::PermissionReply { call: u64, granted: bool },

pub type SessionId = u32;
pub struct ProtoRequest { pub url: String, pub method: String, pub headers: Vec<(String, String)>,
                          pub referrer: String, pub has_body: bool, pub resource_type: ResourceType }
pub enum WebRequestStage { BeforeRequest, BeforeSendHeaders, SendHeaders, HeadersReceived,
                           ResponseStarted, BeforeRedirect, Completed, ErrorOccurred }
pub struct NetError { pub code: i32 /* Chromium net error，如 -2 FAILED、-3 ABORTED、-6 FILE_NOT_FOUND、-105 NAME_NOT_RESOLVED */,
                      pub name: String /* 'net::ERR_FILE_NOT_FOUND' */ }
```

响应体大时分块（每块 ≤ 64 KiB），渲染器端用 `Responder::chunk` 逐块喂给 `fetch` 的流。背压：主进程读 handler 返回的
`Response.body`（`ReadableStream`）时**按渲染器的消费节奏**拉取——再追加一个
`FromRenderer::ProtocolPull { call: u64 }`，渲染器的 `fetch` 流每次需要数据时发一个，主进程收到才读下一块
（首块无需等待）。同时进行的请求数不设上限。

### 3. `protocol.handle` 的语义

- handler：`(request: GlobalRequest) => Response | Promise<Response>`，`request` 是主进程 realm 的标准 `Request`
  （用 `src/fetch.rs` 构造，`body` 为可读流，来自 `ProtocolBody`）。返回值必须是 `Response`，否则以
  `net::ERR_UNEXPECTED` 失败并 `console.error`。handler 抛错/拒绝 → `net::ERR_FAILED`。
- handler 内常见写法 `return net.fetch(pathToFileURL(p).toString())`：`net.fetch` 支持 `file:` URL（读文件，
  Content-Type 按扩展名，用一张内置的小 MIME 表，**不引入 `mime_guess`**——它带完整数据库约 100 KB；表放在
  `api/mime.rs`，覆盖 Web 常用 60 种左右），并经过 P11 的 asar 虚拟文件系统（`app.asar` 内路径可读）。
  `net.fetch` 对已 `handle` 的 scheme 默认会再次进入 handler，`bypassCustomProtocolHandlers: true` 时不进入。
- `handle('https', ...)`/`handle('file', ...)` 是拦截：Electron 允许，对应 `BrowserNet::custom` 对这些 scheme 返回真。
- 旧 API 映射到同一张表：`registerFileProtocol` 的回调 `callback(filePath | {path, headers, statusCode})`、
  `registerBufferProtocol` 的 `callback(Buffer | {data, mimeType, charset, headers, statusCode})`、
  `registerStringProtocol`、`registerHttpProtocol` 的 `callback({url, method, session, uploadData})`（再发一次请求）、
  `registerStreamProtocol` 的 `callback(stream.Readable | {data, headers, statusCode})`。`callback({error: -6})` 以对应
  `NetError` 失败。
- 会话级：`ses.protocol.handle` 只影响该会话的渲染器；全局 `protocol` 即 `defaultSession.protocol`。

### 4. `registerSchemesAsPrivileged` 的效果

`privileges` 字段：`standard`、`secure`、`bypassCSP`、`allowServiceWorkers`、`supportFetchAPI`、`corsEnabled`、`stream`、`codeCache`。
- 必须在 `app` 的 `ready` 之前调用，否则抛错（Electron 文案）。注册表随 `ElectronOptions` 下发给所有之后创建的渲染器
  （渲染器创建都在 ready 之后，所以只读即可）。
- `standard`：URL 按"有 host 的标准 URL"解析（`app://bundle/index.html` 的相对路径、`location.origin === 'app://bundle'`）。
  `url` crate 对非特殊 scheme 的 `scheme://host/path` 已保留 authority，相对解析正确；但 **origin** 对非特殊 scheme
  是不透明的（`null`）——verve-dom 的 `location.origin` 与 `localStorage` 分区需要按特权表把它当作元组 origin。
  这部分在 verve-dom 加 `Options::standard_schemes: Vec<String>`，`origin` 计算处查表（本包改动，通知 P23）。
- `secure`：`window.isSecureContext === true`（verve-dom 读同一张表）。
- `supportFetchAPI`：未设置时，渲染器里对该 scheme 的 `fetch()` 以 `TypeError: Failed to fetch` 拒绝（Chromium 行为）；
  页面加载与 `<script src>` 等不受此限制。
- `corsEnabled`/`bypassCSP`：Verve 渲染器 v1 不实施 CORS 与 CSP（P20 决定；若 P20 实施 CORS，则此处查表）。
- `allowServiceWorkers`/`codeCache`/`stream`：存值。`codeCache` 为真时 P42 对该 scheme 的脚本启用代码缓存。

### 5. webRequest 的代价控制

- 每个会话每个阶段维护"是否有监听者 + 合并后的 URL filter"（Electron 每阶段只允许一个监听者，后设覆盖前设，
  `listener = null` 移除）。渲染器只在 URL 匹配 filter 时才发 `FromRenderer::WebRequest`，否则零开销。
- URL 模式按 Chrome 扩展 match pattern 语法：`<all_urls>`、`*://*.example.com/*`、`https://*/*.png`；实现 `api/match_pattern.rs`。
- `onBeforeRequest` 的 `callback({cancel, redirectURL})`，`onBeforeSendHeaders` 的 `callback({cancel, requestHeaders})`，
  `onHeadersReceived` 的 `callback({cancel, responseHeaders, statusLine})`；其余阶段只通知。`details` 字段对照 Electron 文档
  完整提供（`id`、`url`、`method`、`webContentsId`、`webContents`（主进程侧补）、`frame`（`null`）、`resourceType`、`referrer`、
  `timestamp`、`uploadData`、`statusCode`、`statusLine`、`fromCache: false`、`ip` 等）。
- 主进程的 `net.request`/`net.fetch` 同样经过 webRequest（Electron 行为）。

### 6. 权限请求

渲染器中触发权限的 API（`Notification.requestPermission`、`navigator.clipboard.readText`、`navigator.permissions.query`、
`getUserMedia`（不支持，直接拒绝）、`navigator.geolocation`（不支持））经 verve-dom 的权限入口（需要 verve-dom 追加
`Options::permissions: Option<Box<dyn PermissionBackend>>`，P23 若未提供则本包追加）→ `FromRenderer::Permission` →
主进程调用 `setPermissionRequestHandler((webContents, permission, callback, details) => …)`；没有 handler 时**默认允许**
（Electron 行为）。`check_only` 对应 `setPermissionCheckHandler`，同步语义——渲染器侧 `navigator.permissions.query` 本来是
异步的，所以仍走异步消息即可。

### 7. setProxy

`config`：`mode`（`direct`、`auto_detect`、`pac_script`、`fixed_servers`、`system`）、`proxyRules`、`proxyBypassRules`、`pacScript`。
- `fixed_servers`：解析 `proxyRules`（`http=foopy:80;ftp=foopy2`、`foopy:8080`、`socks5://…`），转换成 `verve::ProxyEnv`，
  调用每个属于该会话的渲染器 Runtime 的 `set_proxy_env`（经 `ToRenderer` 追加 `SetProxy(ProxyEnv)`），以及主进程 `net` 用的配置。
  `socks`/`https` 代理：Verve 现有代理只支持 HTTP，Promise 仍 resolve，但请求按 Verve 规则以 `ERR_PROXY_INVALID_CONFIG` 失败；
  P41 登记。
- `system`：读取系统代理（Windows `WinHttpGetIEProxyConfigForCurrentUser`，`windows-sys` 的 `Win32_Networking_WinHttp`；
  macOS `SCDynamicStoreCopyProxies`；Linux 环境变量）。`direct`：清空。`auto_detect`/`pac_script`：resolve 但按 `direct` 处理，
  `console.warn` 一次。
- `resolveProxy(url)` 返回 `'DIRECT'` 或 `'PROXY host:port'`。

### 8. 下载

触发：`webContents.downloadURL`、`session.downloadURL`、渲染器中导航到 `Content-Disposition: attachment` 的响应或
`<a download>` 被点击（verve-dom 的 `Request::Navigate`/`Open` 由 P13 识别后转为 `FromRenderer::Download`）。
主进程创建 `DownloadItem`，发 `session` 的 `will-download(event, item, webContents)`；监听者同步调用 `item.setSavePath` 则直接
下载，否则弹保存对话框（经 P14 的 `dialog`，`setSaveDialogOptions` 生效）；`event.preventDefault()` 取消。下载在主进程用
Verve 的 HTTP 客户端流式写文件（`<path>.crdownload` 临时名，完成后改名），`updated` 事件每 ~100 ms 节流一次。
`resume` 用 `Range` 请求；服务器不支持时从头开始。

### 9. net

- `net.request(options | url)`：返回 Electron 的 `ClientRequest`（`Writable`；`setHeader`/`getHeader`/`removeHeader`/`write`/`end`/
  `abort`/`followRedirect`/`getUploadProgress`；事件 `response`、`login`（v1 不触发）、`finish`、`abort`、`error`、`close`、
  `redirect`），`response` 是 `IncomingMessage`（`Readable`；`statusCode`、`statusMessage`、`headers`、`rawHeaders`、`httpVersion*`）。
  行为差异要照 Electron：默认**跟随**重定向（`redirect: 'follow'`），`redirect: 'manual'` 时发 `redirect` 事件由用户调用
  `followRedirect()`；`useSessionCookies` 默认 `false`。基于 `src/network/http.rs` 的客户端实现，复用其 TLS/代理/权限。
- `net.fetch`：主进程 realm 的 `fetch_internal` + 会话的 `BrowserNet`（见第 1 节）。
- `net.isOnline()`：存在至少一个非回环、已启用、有地址的网络接口（复用 `src/os.rs` 的接口枚举）。
- `net.resolveHost(host, {queryType, source, cacheUsage, secureDnsPolicy})`：基于 Verve 的 DNS（`dns.lookup` 的实现），
  返回 `{ endpoints: [{ address, family: 'ipv4' | 'ipv6' }] }`。

### 10. 主进程权限

Electron 主进程默认拥有全部网络权限：P10 为主进程 Runtime 设置 `Permissions::all()`，渲染器同样（Chromium 渲染器能访问
任意网络）。本包不新增权限检查。

## 步骤

1. 与 P03 对齐 `BrowserNet`/`fetch_internal` 钩子（若 P03 已合并则先提追加 PR）。
2. `SessionState`、`SessionRegistry`、`session` 模块的值类方法（UA、存储路径、`isPersistent`、`getStoragePath`）。
3. Cookie 罐（RFC 6265：域/路径匹配、`Secure`、`HttpOnly`、`SameSite`、过期、host-only；公共后缀不做，P41 登记）+
   `Cookies` API + 持久化 + verve-dom `CookieBackend`。选型：用 `cookie` crate（`default-features = false`）只做
   `Set-Cookie` 解析，罐自写；**不用 `cookie_store`**（拉 `publicsuffix`/`idna` 数据，增量大）。
4. `protocol.handle` + 渲染器 `RendererBrowserNet::handle` + 流式应答与背压；`file:` 支持与 MIME 表；旧 API 映射。
5. `registerSchemesAsPrivileged` 与 verve-dom 的 `standard_schemes`/`isSecureContext`。
6. webRequest（match pattern、各阶段、details）。
7. 权限请求。
8. `setProxy`/`resolveProxy`。
9. `net.request`/`net.fetch`/`isOnline`/`resolveHost`。
10. 下载与 `DownloadItem`。

## 验收

- `cargo test -p verve-electron --test protocol`（无头）：
  - `registerSchemesAsPrivileged([{scheme:'app', privileges:{standard:true, secure:true, supportFetchAPI:true}}])` +
    `protocol.handle('app', req => net.fetch(pathToFileURL(join(dir, new URL(req.url).pathname)).toString()))`：
    `win.loadURL('app://bundle/index.html')` 能加载页面、相对路径的 `<script src="./a.js">` 与 `<link>`、页面内
    `fetch('./data.json')`；`location.origin === 'app://bundle'`；`isSecureContext === true`。
  - 同上但 `app.asar` 内的文件（与 P11 联测）。
  - 旧 `registerFileProtocol` 能完成同样加载并发出一次 deprecation 警告。
  - handler 返回 1 MiB 流式响应，渲染器读到完整字节；读者暂停时主进程不再拉取（断言拉取次数）。
  - 未设置 `supportFetchAPI` 时页面 `fetch('app://…')` 拒绝，页面加载仍成功。
- `--test session`：
  - 本地 HTTP 服务器（测试内起，Verve `http.createServer` 或 Rust `TcpListener`）`Set-Cookie` → 渲染器后续 `fetch` 带 `Cookie`、
    `document.cookie` 可见（非 HttpOnly）、`session.defaultSession.cookies.get({})` 可见、`changed` 事件触发。
  - `persist:a` 分区的 cookie 与 `localStorage` 重启后仍在（同一测试内销毁并重建 Hub）；内存分区不在。
  - `webRequest.onBeforeRequest({urls:['*://*/blocked*']}, (d, cb) => cb({cancel:true}))` 拒绝匹配请求、不影响其他请求；
    `onBeforeSendHeaders` 改头后服务器收到新头；无监听者时不产生任何 `FromRenderer::WebRequest` 消息（计数断言）。
  - `setPermissionRequestHandler` 拒绝 `notifications` 后渲染器 `Notification.requestPermission()` 得到 `'denied'`。
  - `setProxy({proxyRules:'http://127.0.0.1:<port>'})` 后渲染器请求经过测试内的代理。
  - 下载：`will-download` 中 `setSavePath`，文件内容完整，`done` 的 `state === 'completed'`；`cancel()` 后 `'cancelled'` 且无残留文件。
- `--test net`：`net.request` 默认跟随重定向、`manual` 模式的 `redirect` 事件、POST 分块上传、`net.fetch` 读 `file:`。
- `cargo test`（根 crate）：未设置 `BrowserNet` 时 `tests/fetch.rs`、`tests/fetch_network.rs` 全部照旧通过。
- 体积增量 ≤ 250 KB。

## 不做

HTTP 缓存（`clearCache` resolve、`getCacheSize` 返回 0）、Chromium Cookie SQLite 迁移、公共后缀列表、`login` 事件与 HTTP 认证、
`setCertificateVerifyProc` 以外的证书 UI、`serviceWorkers`、`extensions`、`setCodeCachePath`、`setBluetoothPairingHandler`、
`setUSBProtectedClassesHandler`、`netLog`（对象存在，`startLogging` resolve，不写文件）、PAC 脚本、SOCKS 与 HTTPS 代理（受 Verve 现状限制）。

## 状态

（实现者完成后填写）
