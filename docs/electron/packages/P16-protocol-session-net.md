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

Electron 主进程默认拥有全部网络权限：主进程 Runtime 设置 `Permissions::all()`。渲染器的权限由 `preload::permissions_for` 定：
- 出站网络到任意主机（Chromium 渲染器能访问任意网络）；没有 `listen`，没有 `dns` 模块；
- 磁盘与 Worker 保持内核默认（不受限，不是本包的改动）；
- 页面带 Node（未沙盒的 `nodeIntegration` 或 preload）时才是 `Permissions::all()`；
- Worker 拿不到网络（`set_worker_permissions` 没有被调用）。

`file:` 不靠权限挡，而是 `ElectronLoader` 挡：对 `file:` 地址，先问发起者是不是 `file:` 页面（或浏览器自己的 `loadURL`，没有发起者）且主机是本机，
不是就以 `ERR_ACCESS_DENIED (-10)` 拒绝，**即使 `file` 有处理器也一样**（Electron 39.8.10，探针 5：装了 `file` 处理器后，`app://` 页面对 `file:` 的 `fetch`、XHR、
`<img>`、`<script>` 仍都失败，`loadFile` 的页面仍读得到）。所以 `Loader::forwards("file")` 恒为假：页面对 `file:` 的 `fetch`/XHR 走加载器的规则，再由加载器问处理器。
本包不新增权限检查。

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

## 契约修订（阶段 1 实现时确定；与上文冲突处以本节为准）

### 内核钩子 `verve::browser_net`（P03 未实现，由本包落在 `src/browser_net.rs`）

- `BrowserNet` 不是 `Send + Sync`：它属于一个 realm，以 `Rc<dyn BrowserNet>` 交给 `Runtime::set_browser_net`。会跑宿主脚本的方法
  （`handle`、`pull`、`cancel`、`before_send`、`headers_received`、`notify`）都带 `cx: &mut Context`，在 realm 线程上被调用；
  `custom`、`user_agent`、`failure_style` 不带。主进程的 `net.request`/`net.fetch` 因此可以直接在主 realm 里跑 `protocol.handle` 的处理器。
- `Responder`、`Decider<T>` 是 `Send` 的，可在任何线程、任何时候（包括 `handle` 调用之内）使用；说的话进 realm 的信箱，下一拍（`run_due`）听见，
  realm 的 waker 被唤醒。丢弃未答的 `Responder` 以 `net::ERR_FAILED` 失败；丢弃未答的 `Decider` 让请求原样通过。
- 响应体按拉取驱动：新增 `BrowserNet::pull(id)`（响应流的读者要数据时调用，首次在头之后立即发生）与 `BrowserNet::cancel(id)`（读者放弃或 abort）。
  宿主每收到一次 `pull` 只发一块，这就是背压。
- `handle` 收到的请求体是整块 `Option<Vec<u8>>`（`fetch` 本来就先读完流式请求体再发送），所以**没有** `FromRenderer::ProtocolBody`。
- 失败的样子由 `BrowserNet::failure_style()` 决定：`Undici`（默认，`TypeError: fetch failed` + `cause`）、`Chromium`（页面的 `fetch`：`TypeError: Failed to fetch`）、
  `Net`（Electron 的 `net.fetch`：`Error: net::ERR_FILE_NOT_FOUND`，观察自 Electron 39.8.10）。
- `fetch_internal(context, input, init, InternalFetch { net, resource_type, initiator }) -> JsObject`：全局 `fetch` 就是它加上 realm 自己的钩子；
  `net.fetch` 显式传入主进程的 `BrowserNet`（每次请求一份，带 `bypassCustomProtocolHandlers`）；P20 的加载器仍用 `ProtocolRequest`，不经 `fetch_internal`。
  是否认领一个地址由宿主的 `custom()` 决定，内核没有 bypass 开关。
- `before_send` 的 `Decision::{Continue(RequestHead), Cancel, Redirect(url)}`，`headers_received` 的 `ResponseDecision::{Continue(ResponseHead), Cancel}`；
  取消的请求以 `ERR_BLOCKED_BY_CLIENT` 失败。`notify` 的 `NetEvent::{Sent, Started, Redirected, Completed, Failed}` 对应 webRequest 的
  `onSendHeaders`、`onResponseStarted`、`onBeforeRedirect`、`onCompleted`、`onErrorOccurred`。
- WebSocket 握手不经 `before_send`，登记给 P41。

### `msg/page.rs`

- 追加（不重排）：`FromRenderer::{Protocol { call, session, request }, ProtocolPull { call }, ProtocolCancel { call }}`，
  `ToRenderer::{ProtocolHead, ProtocolChunk, ProtocolEnd}`；`call` 就是内核的 `RequestHead::id`（进程内唯一）。`ProtoRequest` 带 `body: Option<Vec<u8>>`。
- P20 的 `ProtocolRequest`/`ProtocolResponse`（整块应答）保留给页面自己的子资源加载（`<script>`、`<link>`、`<img>`、文档）；页面的 `fetch`/XHR 对自定义 scheme
  改走内核 `fetch` 与 `BrowserNet`（流式、带 headers 与 body、可 abort）：verve-dom 的 `Loader` 新增 `forwards(scheme)` 与 `allows_fetch(scheme)`。
  主进程用同一个“处理器引擎”服务两条路。

### 对任务书的观察订正（Electron 39.8.10，见「状态」里的冻结清单）

- 旧的 `register*Protocol`/`intercept*Protocol` **不发** deprecation 警告（`process.on('warning')` 与标准错误都没有；`lib/browser/api/protocol.ts` 里也没有）。
  任务书与派发要求的“首次调用发一次警告”与观察不符，按观察不发。
- 处理器抛错、拒绝、返回非 `Response` 都是 `net::ERR_UNEXPECTED`（-9），`Response.error()` 才是 `net::ERR_FAILED`（-2）；不是任务书写的“抛错 → ERR_FAILED”。
- `net.fetch` 的 `Response` 没有 `url`（空串）、`type` 是 `default`、`redirected` 为假；它建在 `ClientRequest` 上，`redirect: 'manual'` 以 `Error: Redirect was cancelled` 失败。
- `registerSchemesAsPrivileged` 在 ready 之后调用的错误是 `protocol.registerSchemesAsPrivileged should be called before app is ready`。
- 页面的 `fetch()` 对没有 `supportFetchAPI` 的 scheme 以 `TypeError: Failed to fetch` 拒绝且不调用处理器，XHR 不受此限。

### 阶段 2a 契约（cookie 罐与 webRequest；与上文冲突处以本节为准）

- **消息用类型化的结构，不用 `Cloned`**：`msg/net.rs` 追加 `Cookie`、`SameSite`、`CookieCause`、`CookieChange`、`WebRequestStage`、`WebRequestDetails`、`WebRequestReply`；
  `msg/page.rs` 末尾追加 `FromRenderer::{Cookies { session, changes }, WebRequest { call, session, stage, details }, Navigating}` 与
  `ToRenderer::WebRequestReply { call, reply }`。`details` 在主进程里才变成 JS 对象（补 `webContents`、`frame`），渲染器只带 Rust 数据。
  只有会改请求的三个阶段（`BeforeRequest`、`BeforeSendHeaders`、`HeadersReceived`）等回复；其余阶段 `call == 0`，不回复。
  `Navigating`：页面在同一个 isolate 里要载入别的文档了，它之前向主进程要的东西全部作废（取消仍在服务的处理器应答与读者）。Hub 对已载入过的页面换新 isolate
  （`renew_for_navigation`），旧 isolate 说不出话，所以那条路径由主进程在 `navigation::start` 里取消（`protocol_serve::cancel_all`）；`Navigating` 只留给不换 isolate 的载入。
- **罐在 `Session` 里**（`hub/cookies.rs`）：`CookieStore { jar: Mutex<Jar>, file, dirty, last_write, watched }`，渲染器线程与主进程线程都直接读写，不经消息。
  `Jar` 的每个会改罐的方法返回 `Vec<CookieChange>`，由调用者决定怎么告诉主进程（渲染器：`FromRenderer::Cookies`，且只在 `session.cookies` 被脚本取过之后才发；主进程：直接 emit `changed`）。
  `Jar` 是纯逻辑（RFC 6265 的域匹配、路径匹配、`Secure`、`HttpOnly`、`SameSite`、过期、host-only、`__Secure-`/`__Host-` 前缀）；`Set-Cookie` 行由 `cookie` crate（`default-features = false`）解析。
  没有公共后缀表：单标签的 `Domain` 只有等于主机名本身时才接受；这个缺口登记给 P41。
- **持久化**：`<storage_path>/Cookies.json`（默认会话是 `<userData>/Cookies.json`，`persist:x` 是 `<userData>/Partitions/x/Cookies.json`），只存有过期时间的 cookie
  （会话 cookie 不落盘，与 Chromium 默认一致）；`flushStore()`、`Hub::shutdown`、`CookieStore` 被丢弃时写；运行中一次改动若距上次写盘已过 30 秒才写。内存分区与 `fromPath` 之外无文件。
  `Hub` 记着 `userData`（`MainState::new` 与 `app.setPath('userData')` 告诉它），罐在第一次被用到时才按当时的路径读入。
- **`document.cookie`**：verve-dom 的 `CookieBackend`（`get(url) -> String`、`set(url, line)`）与 `Options::cookies`；页面线程的实现读写同一个 `CookieStore`，读不到 `HttpOnly`，写不了 `HttpOnly`。
- **`before_send` / `headers_received` / `notify` 的接线**（`net_watch.rs`，页面与主进程共用）：`before_send` 先附 `Cookie`，再依次问 `onBeforeRequest`、`onBeforeSendHeaders`（它们看得到已附的 `Cookie`，与 Electron 一致）；
  `headers_received` 先问 `onHeadersReceived`，再把（可能被改过的）`Set-Cookie` 存进罐，最后放行，所以 `fetch` 的 promise 兑现时 `document.cookie` 已经有它。
  `notify` 对应其余五个阶段。没有监听者或 filter 不匹配时不发任何消息（`Session::wants`，原子读）。
- **主进程的请求用不用 cookie**：`net.fetch` 默认 `credentials: 'include'`，用；`net.request` 默认不用，`useSessionCookies: true` 或 `credentials: 'include'` 才用，`credentials: 'omit'` 不用（观察自 Electron 39.8.10，探针 `probe5`，加上 `lib/browser/api/net-fetch.ts`）。
- **SameSite**：页面的请求按页面地址与请求地址的 site（没有公共后缀表时取主机名最后两段）判断，跨 site 的子资源请求不附 `Strict`/`Lax`/未指明的 cookie，也不存它们；主进程的请求不判断。
  凭证模式（`credentials: 'omit'`）内核没有告诉宿主，页面请求一律附 cookie，这一点登记给 P41。
- **观察（Electron 39.8.10，`probe5`）**：
  - `Cookie`：`name, value, domain, hostOnly, path, secure, httpOnly, session, expirationDate（仅持久）, sameSite`；host-only 的 `domain` 不带点，域 cookie 带点；API 设置不给 `sameSite` 得 `lax`，`Set-Cookie` 不给得 `unspecified`；
    `sameSite: 'no_restriction'` 存成 `secure: true`；回环主机上 `Secure` cookie 在 `http:` 也发；`get()` 按「路径长者在前，同长按建立先后」排序，覆盖写入的 cookie 排到最后；无名 cookie 的 `Cookie` 头只有值。
  - `set` 的错误：缺 `url` 为 `Error: Missing required option 'url'`；`url` 不能解析、`domain` 与 `url` 不符是 `Error: Failed to set cookie - The cookie was set with an invalid Domain attribute.EXCLUDE_INVALID_DOMAIN, DO_NOT_WARN, NO_EXEMPTION`；
    名字或值含 `;`/控制字符是 `Error: Failed to set cookie - The cookie contains ASCII control charactersEXCLUDE_DISALLOWED_CHARACTER, DO_NOT_WARN, NO_EXEMPTION`；`sameSite` 非法是 `Error: Failed to convert 'bogus' to an appropriate cookie same site value`；
    非对象是 `TypeError: Error processing argument at index 0, conversion failure from x`；`get()`、`remove()` 无参数是 `TypeError: Insufficient number of arguments.`；`remove` 的 `url` 不能解析时 promise 照常兑现。
  - `changed(event, cookie, cause, removed)`：`event` 是空 `Event`；设置是 `'explicit'`、`removed: false`；覆盖先发旧的 `'overwrite'`、`removed: true`，再发新的；`remove` 是 `'explicit'`、`removed: true`。
  - `webRequest`：`details` 的字段次序（`BeforeRequest`：`id, url, method, timestamp, resourceType, [frame, webContents, webContentsId,] referrer, [uploadData,] redirectURL`；
    `BeforeSendHeaders`/`SendHeaders`：同前，无 `redirectURL`，末尾 `requestHeaders`；`HeadersReceived`/`ResponseStarted`/`Completed`：`id, url, method, timestamp, resourceType, ip, fromCache, statusLine, statusCode, responseHeaders, [frame, webContents, webContentsId,] referrer, [uploadData]`，`Completed` 末尾加 `error: 'net::OK'`；
    `BeforeRedirect`：同 `HeadersReceived`，末尾 `redirectURL`；`ErrorOccurred`：`id, url, method, timestamp, resourceType, [frame, webContents, webContentsId,] referrer, error`）；
    页面的 `fetch` 与 XHR 都是 `resourceType: 'xhr'`；重定向的各跳共用一个 `id`；主进程的请求没有 `frame`/`webContents`/`webContentsId`，其 `referrer` 为 `''`；`timestamp` 是毫秒级的浮点时间；
    请求头名是大写词首（`User-Agent`），响应头值是数组；`statusLine` 形如 `HTTP/1.1 200 OK`；`uploadData` 是 `[{ type: 'rawData', bytes: Buffer }]`；通知阶段的监听器只收到 `details`。
  - `webRequest` 的错误：`onX(5)` 是 `TypeError: Must pass null or a Function`；filter 没有 `urls` 数组是 `TypeError: Parameter 'filter' must have property 'urls'.`；坏模式是 `TypeError: Invalid url pattern nonsense: Missing scheme separator.`
    （模式解析的其余文本取自 `electron.exe` 里的字符串表：`Invalid scheme.`、`Wrong scheme type.`、`Host can not be empty.`、`Invalid host wildcard.`、`Empty path.`、`Invalid port.`、`Invalid host.`）。
  - 取消的请求在页面里是 `TypeError: Failed to fetch`；`redirectURL` 的跳转被 `fetch` 当作重定向（`response.redirected === true`，`response.url` 是新地址）；
    `onBeforeSendHeaders` 里改的头到达服务器；`onHeadersReceived` 改的响应头页面读得到。

### 阶段 2b 契约（权限、代理、下载；与上文冲突处以本节为准）

- **权限**（观察自 Electron 39.8.10，`probe7`）：
  - 没有任何处理器时，`Notification.permission`、`Notification.requestPermission()`、`navigator.permissions.query({name:'notifications'})` 都是 `granted`。
  - `setPermissionRequestHandler((webContents, permission, callback, details) => …)` 只管「请求」：`Notification.requestPermission()` 走它，`callback(false)` 得 `'denied'`；`details` 是 `{ isMainFrame, requestingUrl }`。
  - `setPermissionCheckHandler((webContents, permission, requestingOrigin, details) => boolean)` 管「查询」：`Notification.permission` 与 `permissions.query` 各调它一次；`requestingOrigin` 形如 `http://127.0.0.1:52602/`（带结尾斜杠），`details` 是 `{ embeddingOrigin, isMainFrame, requestingUrl }`。
    只设查询处理器时，`requestPermission()` 仍是 `granted`（请求没有处理器，默认允许）。
  - `setPermissionRequestHandler(5)` 抛 `Must pass null or function`；传 `null` 取消处理器。
  - 页面一侧的消息是类型化的：`FromRenderer::Permission { call, session, permission, url, check_only }` 与 `ToRenderer::PermissionReply { call, granted }`。会话原子地记着两种处理器是否存在，
    没有处理器时页面当场得到 `granted`，不发任何消息（与 webRequest 一样，没人听就没有代价）。
  - verve-dom 加 `PermissionBackend`（`ask(ask, permission, url, check_only) -> Option<bool>`：`Some` 是当场的答案，`None` 表示稍后由 `Document::settle_permission` 给；`known(permission)` 给 `Notification.permission` 的同步读）、
    `Options::permissions`，以及页面里的 `Notification`（只有 `permission`、`requestPermission()`，构造函数只记标题，不显示）、`navigator.permissions.query` 与 `clipboard.readText()` 的 `clipboard-read` 询问（拒绝时 `NotAllowedError`）。没有后端的页面一律被拒。
    **同步读的缺口**：Electron 的 `Notification.permission` 直接问查询处理器；页面线程不能等主进程，所以有查询处理器时它在问过一次之前是 `default`，问过之后是上一次**查询**的答案（`requestPermission()` 的答案不记，评审所述的差异；未另行探针）。登记给 P41。
- **代理**：
  - `setProxy(config)`：`mode` 是 `direct`、`auto_detect`、`pac_script`、`fixed_servers`、`system` 之一，否则拒绝 `Invalid mode, must be one of direct, auto_detect, pac_script, fixed_servers or system`（观察）。
    `proxyRules` 按 Chromium 的格式读：`host:port`、`http://host:port`、`socks5://host:port`、`https://host:port`，以及 `http=foo:80;https=bar:81` 这样的逐 scheme 写法（逗号分隔的候选取第一个）。
    细处按 Chromium 的 `proxy_config.cc`/`proxy_string_util.cc` 读（读源码，不是观察）：`socks=h` 是 SOCKS4、端口 1080；无 scheme 的条目只有排在第一位时才算「全部 scheme」；列表里的 `direct://` 是一个候选（选中即直连）；scheme 不分大小写。
    解析结果是 `hub/proxy.rs` 的 `Proxy`，**每个请求**问它 `route(url)`：`http:` 代理给内核 `Route::Via { host, port }`，直连（含被绕过的、该 scheme 没有规则的）给 `Route::Direct`，`socks4`/`socks5`/`https` 代理给 `Route::Unsupported`，请求以 `ERR_PROXY_INVALID_CONFIG` 失败（P41 缺口）。
    只写了 `http=foo:80` 时，`https:` 请求直连，不向 foo 发 `CONNECT`。
  - 回环与链路本地地址（`localhost`、`127.0.0.0/8`、`::1`、`169.254.0.0/16`、`fe80::/10`）恒不走代理，有没有 `proxyBypassRules` 都一样（观察：`proxyRules: 'foo:80'` 时 `resolveProxy('http://127.0.0.1:5/')` 是 `DIRECT`）。
    `proxyBypassRules` 逗号或分号分隔，含子域、`*x`/`x.*.y` 通配、scheme 前缀、`<local>`、`a.b.c.d/n`；`resolveProxy` 与请求的路由用同一个函数匹配，二者不会不一致。`pacScript` 而无 `mode` 就是 `pac_script`。
  - `resolveProxy(url)`：`DIRECT`、`PROXY host:port`、`SOCKS5 host:port`、`HTTPS host:port`（观察）；没有 `url` 参数时兑现 `DIRECT`（观察）。`fixed_servers` 没有 `proxyRules`、`direct`、以及未给 `mode` 的空配置都是直连。
  - `auto_detect` 与 `pac_script` 按直连处理并警告一次。真 Electron 在本机上做 `auto_detect`/`pac_script` 后会崩溃（`probe7` 里两次），所以没有观察到它们的 `resolveProxy`；行为取自任务书。
  - `system` 只读系统代理：Windows 用 `WinHttpGetIEProxyConfigForCurrentUser`，其余平台读 `http_proxy`/`https_proxy`/`no_proxy` 环境变量；任何测试都不依赖其值，也从不改系统设置。
  - **生效的方式**：内核的 `BrowserNet` 有一个有默认实现的 `proxy(&self, url) -> Route`（`Realm`、`Direct`、`Via { host, port }`、`Unsupported(message)`）。`fetch` 在每一跳（含重定向）发请求前问它，把答案放在请求选项的 `__verveVia`（或 `__verveDirect`）上，
    代理 agent 的 `route()` 本来就先看它，连接池的键本来就含路由。`MainNet` 与 `RendererNet` 都用会话的 `Session::route(url)` 回答。所以：不同会话的请求永远各走各的代理，`webRequest` 监听者延迟决定也不会串；
    主进程的 Node `http`/`https` 与全局 `fetch` 不经会话，也就不走代理（与 Electron 一致）。没有「会话代理的代数」，也没有改 realm 代理的调用。
  - **WebSocket 缺口**：页面的 `WebSocket` 不经 `BrowserNet`，仍按 realm 自己的代理配置（页面 realm 里没有设置），即不走会话代理：登记给 P41。
- **下载**（观察自 Electron 39.8.10，`probe7`）：
  - 触发：`session.downloadURL(url[, { headers }])`、`webContents.downloadURL(url[, { headers }])`、导航到 `Content-Disposition: attachment` 的响应、`<a download>` 被点击。`downloadURL()` 无参数抛 `Insufficient number of arguments.`，坏地址不抛。
    导航到 attachment 时 `loadURL` 以 `ERR_FAILED (-2)` 拒绝，页面留在原处（Electron），同时发 `will-download`。Verve 的 `Load` 一发出就换掉 isolate，所以旧文档在响应到达前就没了：缺口，见「状态」。
  - `will-download(event, item, webContents)`：`event` 是空 `Event`；`session.downloadURL` 触发时 `webContents` 不是任何窗口的（`null`），其余是发起的页面。
    此时 `item` 已有：`getURL()`、`getURLChain()`、`getFilename()`（`Content-Disposition` 的 `filename`，否则地址路径的末段；`<a download="x.bin">` 的值优先；无扩展名且 `text/plain` 时补 `.txt`，`application/octet-stream` 不补）、
    `getMimeType()`、`getTotalBytes()`（没有 `Content-Length` 时为 `0`）、`getState()`（`progressing`）、`getSavePath()`（`''`）、`getReceivedBytes()`（`0`）、`isPaused()`、`canResume()`（`false`）、`getContentDisposition()`（没有则 `''`）、`hasUserGesture()`、`getLastModifiedTime()`、`getETag()`、`getStartTime()`。
  - 监听者同步 `item.setSavePath(path)` 则直接下载；`event.preventDefault()` 取消（不发任何 `updated`/`done`）；两者都没有则走保存对话框（`dialog.showSaveDialog`，含 `setSaveDialogOptions`），取消对话框即取消下载。
  - 下载中写 `<path>.crdownload`，完成后改名为 `<path>`（Chromium 的临时名是另一个，本版取任务书的名字，不是观察值）。`setSavePath` 之后立刻发一次 `updated`（`progressing`），之后至多每 100 ms 一次；`done(event, state)` 的 `state` 是 `completed`、`cancelled`、`interrupted`。
    `cancel()` 后 `done('cancelled')`，既没有 `<path>` 也没有 `.crdownload`（观察：目录里无残留）。`pause()` 后 `isPaused()` 为 `true`、`canResume()` 为 `true`、`getState()` 仍是 `progressing`，并发一次 `updated`；`resume()` 用 `Range: bytes=<已收>-` 续传，
    服务器回 `200`（不认 `Range`）则从头来过，回 `206` 则接着写。
  - 下载在主进程里经会话的 `net`（带会话 cookie 与 webRequest）取，写文件在主进程线程里同步进行。页面的 `Content-Disposition` 导航由 verve-dom 报 `Request::Download { url, filename }`（`<a download>` 同），主进程重新取一次地址（页面那次响应被丢弃）；缺口：该地址被取两次。

## 状态

### 阶段 1（钩子、protocol、net；会话只做值类方法）

**做了什么**

- **内核钩子 `verve::browser_net`**（`src/browser_net.rs`、`src/browser_net/{custom,gate}.rs`，`src/network/fetch.rs` 接入）：见上文“契约修订”。
  未设置 `BrowserNet` 时 `fetch` 的路径不变：只有 `Fetch` 多了一个 `Option<Gate>`，`wire_headers` 拆成 `wire_list` + `header_object`（行为相同），
  `start` 拆成 `start`（权限与是否交给宿主）+ `dispatch`（发出）。`src/fetch.rs`、`src/network/http.rs` 没有变长。测试 `tests/browser_net.rs`（宿主应答、逐块拉取与背压、失败的三种样子、
  取消/abort、请求体与头到达宿主、`before_send`/`headers_received` 改头/取消/改址、`notify` 顺序、显式 `BrowserNet`；内核没有 bypass 开关，`bypassCustomProtocolHandlers` 是宿主的 `custom()` 不认领）。
- **verve-dom**：`Options::{standard_schemes, secure_schemes}`（`Options` 搬到新文件 `options.rs`，因为 `lib.rs` 在行数上限上）、`schemes.rs`（`location.origin`、`history` 同源判断、
  `isSecureContext` 改成按地址计算的 getter）、`Loader::{forwards, allows_fetch}`、页面 `fetch` 对 forwards 的 scheme 走内核 `fetch`（XHR 另走一份不受 `supportFetchAPI` 限制的闭包）。
- **verve-electron**：
  - `hub/sessions.rs`：`Session`（分区名、`persist:`、`fromPath`、已处理的 scheme、UA）、`Hub::{session_for, session_at, session_by_id, session_of, set_privileged, privileged}`。
  - `renderer_net.rs`：页面线程的 `BrowserNet`（`FromRenderer::{Protocol, ProtocolPull, ProtocolCancel}` / `ToRenderer::{ProtocolHead, ProtocolChunk, ProtocolEnd}`）；
    `loader.rs` 的 scheme 表改为会话的，`files.rs` 与 `api/mime.rs` 是页面加载与 `net.fetch` 共用的文件读取与唯一的 MIME 表（65 种）。
  - 主进程：`api/protocol.rs`（`handle`/`unhandle`/`isProtocolHandled`、`register*`/`intercept*`、`registerSchemesAsPrivileged`、`getStandardSchemes`）、
    `api/protocol_serve.rs`（处理器引擎：一份代码服务页面 `fetch`、页面子资源加载与主进程 `net.fetch`；每次拉取读一块）、`api/protocol_legacy.rs`（旧回调接口转成 `Response`）、
    `api/main_net.rs`（主进程请求的 `BrowserNet`，含 `file:`，asar 内可读）、`api/session.rs`（`defaultSession`、`fromPartition`、`fromPath`、UA、存储路径、`isPersistent`、`protocol`、`fetch`、
    `resolveHost`、`session-created`）、`api/net.rs`（`net.fetch`、`isOnline`/`online`、`resolveHost`）、`api/net_request.rs`（`ClientRequest`、`IncomingMessage`）。`webContents.session` 是真的会话对象。
- 测试：`tests/protocol.rs`、`tests/net.rs`（见下面“要跑的命令”）。

**没做 / 只做了一部分**

- asar 内文件经 `protocol.handle` + `net.fetch`：`file:` 读取走 `Hub::overlay()`（P11），`tests/protocol.rs` 有测试（打包一个 asar）。
- 旧的 `registerStreamProtocol` 的 Node `Readable` 是收齐后再发（不是流式）；`registerHttpProtocol` 的 `{url, method, headers}` 用主进程全局 `fetch` 取，不带 `session`/`uploadData`。Electron 对后者的行为（探针里 `cb({url})` 得到 `ERR_UNEXPECTED`）没有弄清，没有冻结。
- `net.request` 的上传不是真的分块：`write()` 的片段收齐后随 `end()` 一次发送（服务器收到完整正文；`chunkedEncoding` 只是被记下）。`login` 事件不触发，`useSessionCookies`、`credentials`、`referrerPolicy`、`cache`、`priority` 被接受但无效（cookie 在阶段 2）。
- `webPreferences.session`（用会话对象指定页面的会话）读，换算成分区名；`webContents.session` 是真的会话对象。`canvas` 的同源判断仍按 `url::Origin`。
- 阶段 2 的 cookie 罐、webRequest、权限、代理、下载、`--test session` 都没开始。`Session` 对象目前只有值类方法与 `protocol`、`fetch`、`resolveHost`。
- 页面线程的 `RendererNet::user_agent` 读会话的 UA，但没有人把页面 `navigator.userAgent` 接到 `session.setUserAgent`（verve-dom 的 `Options::user_agent` 在 `RendererGuest::new` 时没有设）。阶段 2。
- 体积：没有测（不跑 release 构建）。新增依赖：无。

**冻结的 Electron 39.8.10 观察**（探针在本包 scratch 里：`probe1`…`probe3`；JS 源码取自 `electron.exe` 里的 `lib/…` 压缩文本）

- `protocol.registerSchemesAsPrivileged` 在 ready 之后：`Error: protocol.registerSchemesAsPrivileged should be called before app is ready`。
- `protocol.handle` 同一 scheme 两次：`Failed to register protocol: <scheme>`；`unhandle` 未登记的：`Failed to unhandle protocol: <scheme>`；`handle` 不检查 scheme 的写法与 handler 的类型，不抛；
  scheme 不转小写（`handle('UPPER')` 之后 `isProtocolHandled('upper')` 为假）；`http`/`https`/`file` 的 `handle` 是拦截（`isProtocolHandled('https')` 初始为假）。
- 处理器抛错、拒绝、返回非 `Response`：`net.fetch` 以 `Error: net::ERR_UNEXPECTED` 失败（抛错还把栈写到标准错误）；`Response.error()`：`net::ERR_FAILED`。**与任务书不同**。
- 旧 `register*Protocol`/`intercept*Protocol` **不发任何 deprecation 警告**。**与任务书/派发不同**，按观察不发。`registerFileProtocol` 重复登记不抛（返回 `false`），`cb({error: -6})` 得 `net::ERR_FILE_NOT_FOUND`，
  `registerStringProtocol` 的回应没有 `content-type`，`registerBufferProtocol` 的 `mimeType`/`statusCode`/`headers` 生效。
- `net.fetch`：`Response` 无 `url`（`''`）、`type` 为 `default`、`redirected` 为假；`statusText` 取自 `IncomingMessage.statusMessage`（http 是 `OK`；探针 netfetch-http 那一行里的 `""` 是 `url`）；`file:` 的头只有 `content-type`（`application/json`）与 `last-modified`，无 `content-length`；
  失败是 `Error: net::ERR_…`（无 `cause`）；`redirect: 'manual'` 以 `Error: Redirect was cancelled` 失败；未知 scheme `net::ERR_UNKNOWN_URL_SCHEME`；`new URL` 解析失败是 `TypeError`。
- `net.request`：要求 ready（`net module can only be used after app is ready`）；`ftp:` 抛 `ClientRequest only supports http: and https: protocols`；`nonsense` 抛 `TypeError: Invalid URL`（`ERR_INVALID_URL`）；
  事件顺序 `finish`、`close`（请求体交出即关）、`redirect`、`response`；`redirect` 的参数是 `(302, 'GET', 新地址, 响应头)`，**跟随模式也发**；`manual` 不调 `followRedirect()` 以 `Redirect was cancelled` 失败；
  `error` 模式以 `Attempted to redirect, but redirect policy was 'error'` 失败；`abort()` 得 `finish`、`abort`、`close`；`IncomingMessage` 的 `statusMessage` 为 `OK`，`httpVersion` 为 `1.1`，`set-cookie` 是数组。
- 页面：标准+secure+supportFetchAPI 的 scheme：`location.origin === 'std://bundle'`、`isSecureContext === true`、`fetch('./data')` 的 `url` 为 `std://bundle/data`、`type` 为 `basic`；
  只 `standard`：`isSecureContext === false`，`fetch()` 以 `TypeError: Failed to fetch` 拒绝且**不调用处理器**，XHR 可用；`standard`+`secure` 无 `supportFetchAPI`：`fetch()` 拒绝、XHR 可用；
  非 standard：`origin === 'null'`、`localStorage` 抛 `SecurityError`、XHR 也失败。处理器收到的请求头只有 `accept`（和用户设的、`content-type`），`referrer` 为空。
- `session.fromPartition`：`''` 是默认会话；同名同对象；`persist:per` 的存储路径是 `<userData>/Partitions/per`；内存分区 `getStoragePath()` 为 `null`、`isPersistent()` 为假；默认会话持久且路径就是 `userData`；
  `fromPartition(5)` 抛 gin 的 `Error processing argument at index 0, conversion failure from `；默认 UA 形如 `Mozilla/5.0 (…) AppleWebKit/537.36 (KHTML, like Gecko) <app>/<version> Chrome/142.0.7444.265 Electron/39.8.10 Safari/537.36`。
- `net.resolveHost('localhost')`：`{ endpoints: [{ address: '::1', family: 'ipv6' }, { address: '127.0.0.1', family: 'ipv4' }] }`。

**未观察（没有对 Electron 39.8.10 探针，所以不算冻结）**：处理器自定义的 `statusText` 是否原样转给页面；standard scheme 的页面用 XHR 访问另一个自定义 scheme（跨源）；
无 origin 的（`bare`）页面在有 `supportFetchAPI` 时的 `fetch()`。P20 的 `page_has_node` 不看 `sandbox`（只决定页面脚本看到什么），而 `preload::permissions_for` 看：`!sandbox && (nodeIntegration || preload)`。

**已知缺口**：`localStorage` 在不透明 origin 上没有分区；处理器收不到 `accept`（探针 `std` 里有）；`new URL('app://…').origin` 对 standard scheme 仍是 `"null"`；
`file:` 应答没有 `last-modified`；页面载入别的文档时，主进程里仍在服务的处理器应答（`Served`）与读者没有被取消（阶段 2a 补）；Worker 没有 BrowserNet，也拿不到网络
（页面的 `Worker` 是完整的 Verve runtime，磁盘不受限，早于本包，登记给 P20/P41）；`fromPath` 与 `getStandardSchemes` 的文本未观察；session/protocol 在 ready 之前的限制未实施；
`registerHttpProtocol` 在 Verve 里成功，Electron 探针给的是 `ERR_UNEXPECTED`。

**第三轮**：`net.request('nonsense')` 抛带 `code` 的 `TypeError`；`session.getStoragePath()` 返回原生路径；自定义 scheme 应答的拉取是一块一次
（宿主在 `pull` 里还没给出块时，流不再重复拉取，`tests/browser_net.rs` 的拉取数恢复为精确的 4）；渲染器权限如上；`session.protocol` 是 `Protocol` 类的实例；
`IncomingMessage` 的读取状态在 `Requests` 里，不在脚本可见的属性上；页面关闭或渲染进程消失时取消它的 `protocol.handle` 应答；页面 XHR 对无 origin 的
自定义 scheme 页面失败（探针 3 `bare`）；`loadFile` + `file` 处理器、不带 Node 的页面读不了 `file:`、asar 内 `protocol.handle` + `net.fetch` 都有了测试；
探针 4（Electron 39.8.10）：`net.fetch` 的 `statusText` 对 `file:` 与 http 都是 `OK`；`file` 处理器 + `bypassCustomProtocolHandlers` 读到文件。
**还没做**：`last-modified`（阶段 2a 补）。电子包与内核的大提交已拆成小提交。

**给其他包的备注**

- **P23**：verve-dom 新增 `Options::{standard_schemes, secure_schemes}`、`window.isSecureContext` 改为按地址计算的 getter、`location.origin`/`a.origin`/`pushState` 同源判断经 `Schemes`、`Loader::{forwards, allows_fetch}`。
- **P30**：verve-dom 的 `Options` 搬到了 `crates/verve-dom/src/options.rs`（`lib.rs` 仍 `pub use`），并发改 `Options` 的请改那里。

- **P20**：页面 `fetch`/XHR 对自定义 scheme 现在带 headers、body 与 abort（走内核 `fetch`），你登记的“fetch 的 body 与 headers 没有传给处理器”已解决。
- **P30**：`renderer_guest.rs` 我只加了 `renderer_net::install`、`scheme_names` 和两个 `Options` 字段、三条 `ToRenderer` 分支。
- **阶段 2 的人**：`RendererNet` 的 `before_send`/`headers_received`/`notify` 还是默认实现；cookie 与 webRequest 在它们里面接，`MainNet` 同理（主进程那份有 `cx`，可直接跑监听者）。

**要跑的命令（逐个跑）**

```
cargo test -p verve --test browser_net
cargo test -p verve --test fetch --test fetch_network
cargo test -p verve --lib browser_net
cargo test -p verve-dom --lib schemes
cargo test -p verve-dom --test fetch --test load
cargo test -p verve-electron --lib
cargo test -p verve-electron --test protocol
cargo test -p verve-electron --test net
cargo test -p verve-electron --test page_load
```


### 阶段 2a（cookie 罐、webRequest、阶段 1 的两条遗留）

**做了什么**

- **cookie 罐**（`hub/cookies.rs`）：`Jar`（纯逻辑，时钟是参数）与 `CookieStore`（锁、文件、`watched`）。RFC 6265 的域匹配/路径匹配、`Secure`（回环主机视同安全传输）、`HttpOnly`、`SameSite`、过期、host-only、
  `__Secure-`/`__Host-` 前缀；`Set-Cookie` 行由 `cookie` crate（`default-features = false`，只解析）读，罐是自写的。新增依赖只有 `cookie 0.18.2`（它拉 `version_check` 作构建依赖，`time` 本来就在图里），
  `Cargo.lock` 只多了这两处，没有顺带升级别的包。持久化 `Cookies.json`（见契约），`Hub::set_user_data` 由 `MainState::new` 与 `app.setPath('userData')` 通知，`Hub::shutdown` 写盘（没被「启动」过的临时应用不写）。
- **`session.cookies`**（`api/cookies.rs`）：`get`/`set`/`remove`/`flushStore`、`changed(event, cookie, cause, removed)`；错误文本与 `Cookie` 的字段取自 Electron 39.8.10 的观察（契约里列了）。页面线程改的罐经 `FromRenderer::Cookies` 到主进程发 `changed`，
  且只在脚本取过 `session.cookies` 之后才发。
- **请求的接线**（`net_watch.rs`，页面与主进程共用一个 `Watch`）：`before_send` 附 `Cookie` 再问 `onBeforeRequest`/`onBeforeSendHeaders`，`headers_received` 先问 `onHeadersReceived` 再存 `Set-Cookie`，`notify` 对应其余五个阶段。
  页面的 `RendererNet`（`renderer_net.rs`）直接读写 `SessionState`，没有主进程往返；`MainNet`（`net.fetch`/`net.request`）同一份代码，`net.fetch` 默认用罐（`credentials: 'omit'` 不用），`net.request` 看 `useSessionCookies`/`credentials`。
- **webRequest**（`api/web_request.rs`、`api/match_pattern.rs`）：八个阶段的 `onX([filter, ]listener|null)`，每阶段一个监听者，后设覆盖前设；match pattern 的解析与匹配（含 `filter.types`）；`details` 的字段与次序、`uploadData`（Buffer）、`requestHeaders`/`responseHeaders` 的形状；
  `callback({cancel, redirectURL, requestHeaders, responseHeaders, statusLine})`。没有监听者或 filter 不匹配时页面不发任何消息（`Session::web_request_messages()` 在发送处计数，测试断言）；主进程自己的请求直接在主 realm 里跑监听者。
- **verve-dom**：`CookieBackend` 与 `Options::cookies`，`document.cookie` 的读写转给它；没有它时仍是窗口内的旧实现。
- **阶段 1 遗留**：页面载入别的文档时（`RendererGuest::load`），之前它向主进程要的东西作废：加载器的未决询问丢掉，`FromRenderer::Navigating` 让主进程取消仍在服务的处理器应答（`Served`）与它们的读者（`protocol_serve::cancel_all`，现在也认加载器的询问）；
  等 `webRequest` 回复的请求被取消。`file:` 的应答有了 `last-modified`（磁盘文件的修改时间；`app.asar` 内的没有）。
- **测试**：`tests/session.rs`（9 个）、`tests/protocol.rs` 一个、`tests/net.rs` 一个、`hub::cookies`、`api::match_pattern`、`files`、`net_watch` 的单元测试、verve-dom 的 `tests/cookie_backend.rs`。

**没做 / 只做了一部分**

- 阶段 2b 的权限、代理、下载与 `--test session` 的其余部分没有开始。`Session` 对象还没有 `setPermissionRequestHandler`、`setProxy`、`downloadURL`、`on('will-download')`。
- 没有公共后缀表（单标签的 `Domain` 只在等于主机名时接受）；也没有 Chromium 的淘汰规则（每域 180、总共 3000 个）。登记给 P41。
- 内核没有把 fetch 的凭证模式告诉宿主，所以页面的请求一律附 cookie（含跨源），跨 site 时只按 `SameSite` 过滤；site 取主机名最后两段。登记给 P41。
- 写盘：改动后若距上次写已过 30 秒才写，没有定时器；之间的改动在 `flushStore()`、退出、`CookieStore` 被丢弃时写。会话 cookie 不落盘。`app.setPath('sessionData')` 不影响 cookie 的位置（只认 `userData`）。
- `details`：`ip` 只在地址本身是 IP 字面量时给；`fromCache` 恒为 `false`；`frame` 是页面的主框架对象，子框架的请求也是；页面自己载入的文档、`<script>`/`<link>`/`<img>` 子资源不经页面线程的 `fetch`，所以
  文档载入（若走 `fetch`）的 `resourceType` 是 `xhr` 而不是 `mainFrame`，由加载器取的子资源（只有 `file:`、`data:` 与自定义 scheme）不经 `webRequest`——与 Electron 不同，没有逐项核实。
- WebSocket 握手不经 `before_send`（内核没接），登记给 P41。`onBeforeSendHeaders` 里对 `Cookie` 头的改动照样被发送，不再被罐覆盖。
- 没有跑任何测试（只跑了 `cargo check`）；测试期望都来自 `probe5`/`probe6` 的观察，没有拿真 Electron 对跑过。

**给其他包的备注**

- **P23**：verve-dom 新增 `CookieBackend`（`get(url) -> String`、`set(url, line)`）与 `Options::cookies`；`document.cookie` 的访问器只在 `bind/document.rs` 里多走一步 `jar_of`。页面的 `document.cookie` 看不到 `HttpOnly`，写不了 `HttpOnly`。
- **P30**：`renderer_guest.rs` 这一阶段又动了四处：`Options::cookies`、`ToRenderer::WebRequestReply` 一条分支、`load()` 开头的 `abandon`、两处 `set_page`。
- **P20**：`ElectronLoader::abandon()`；加载器的询问现在以 `Asker::Loader(contents)` 登记，页面载入别的文档时被取消。
- **阶段 2b 的人**：`Session` 的状态在 `hub/sessions.rs`，`webRequest` 与 `cookies` 的对象表在 `NetState`（`api/mod.rs`）；`MainNet::for_request(cx, session, (bypass, cookies))` 是主进程请求的唯一入口。

**要跑的命令（逐个跑）**

```
cargo test -p verve-dom --test cookie_backend
cargo test -p verve-electron --lib
cargo test -p verve-electron --test session
cargo test -p verve-electron --test protocol
cargo test -p verve-electron --test net
cargo test -p verve-electron --test page_load
cargo test -p verve --test browser_net
```

### 第四轮（评审与阶段 2a 的集中测试后的修正）

- **安全**：装了 `file` 处理器后，P20 的 `file:` 来源规则被跳过，任何页面都能读本地文件。现在 `ElectronLoader::fetch` 在问处理器之前先过 `file_allowed`，不满足以 `ERR_ACCESS_DENIED` 拒绝；
  `forwards("file")` 恒为假，页面对 `file:` 的 `fetch`/XHR 因而走加载器。探针 5（Electron 39.8.10，装了 `file` 处理器并传 `bypassCustomProtocolHandlers`）：`app://` 页面的 `fetch`、XHR、`<img>`、`<script>`
  都失败（`TypeError: Failed to fetch`、XHR `error`），`loadFile` 页面的 `fetch`、XHR、`<script>` 成功（`<img>` 因内容不是图片而 `error`）。
- **页面载入 http 地址失败的原因（阶段 2a 的六个 `--test session`）**：不带 Node 的页面从全局对象上拿掉了 `Buffer`，而内核的流、文件与网络用全局 `Buffer` 造 `Buffer`，页面的 http 响应就读不出来。
  现在内核记着自己造出的 `Buffer` 类（`native::buffer_class`），不再读全局。这是静态读代码得出的结论，没有跑过。
- 页面载入别的文档时，Hub 换新的 isolate，旧 isolate 的 `Navigating` 说不出口；取消仍在服务的处理器应答改由主进程在 `navigation::start` 里做。
- `cookies.rs` 的路径测试期望错了（无路径的 cookie 的默认路径是 `/a`，也匹配 `/a`）；`session.rs` 的 JSON 数字比较与过长的测试已修；`support::report_of` 不再 `unwrap`。
- 其他：`preload::permissions_for` 按 `!sandbox && (nodeIntegration || preload)`；`fromPartition('path:/x')` 与 `fromPath` 不再撞键；空 body 的应答（HEAD、204）也通知宿主放手；
  `net.request` 的上传体在应答后释放（`Requests.table` 里每个请求的小记录仍留到应用结束：`with_req` 之后的访问要求它在）；`webPreferences.session` 与 `webContents.session` 有了测试；
  `net.fetch` 的 `file:` 应答断言 200、`OK`、处理器只被问一次。
- **没做**：`cx` 改名 `context`（按派发留到阶段 2b 之后）；`tests/browser_net.rs` 的 `<= 4` 与 `tests/protocol.rs` 的 `1..=5` 改成精确值（要跑一次才知道确切的数）；
  把 protocol.rs 里几处断言挪到更早的提交；`Kind` 与 `define::Kind` 重名、`protocol_serve::report` 与 `define::report` 重名、六处 promise 的 ok/err 写法、三处头对循环、`RequestHead` → `ProtoRequest` 的两份、
  `64 * 1024` 与重定向状态码的命名、`native::exports` 化、`Schemes::new` 等公开项的文档、测试夹具的去重、`cancel_all` 的测试、未探针的三条（见阶段 1 的「未观察」）。
