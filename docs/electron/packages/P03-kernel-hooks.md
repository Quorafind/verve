# P03 Verve 内核开放接口

**依赖**：无  **规模**：M  **修改**：`src/`（verve 根 crate）——本包是 M0/M1 中**唯一**集中改内核的包；P11、P17 在本包接口上补实现

## 目标

把 Electron 层需要、但现在藏在 verve 内部或写死了的东西，变成稳定的公开接口。做完之后，
`crates/verve-electron` 不需要再改 `src/` 就能实现 P10/P12/P13/P20/P21。
所有改动对 `verve run` 和现有嵌入方（Rebon）**零行为变化**：现有测试全过，默认值保持。

## 背景（先读）

- `src/worker.rs:81` `enum Cloned`、`:103` `clone_out`、`:224` `clone_in`：Worker 的结构化克隆。
- `src/process.rs`：`process` 对象的搭建（`:392` 起写死了 `title: "obsidian"`、`type: "renderer"`、
  `argv0: "electron"`）；`:507` `Setup`（`verve run` 用的进程形态）；`:896` `exit` 直接
  `std::process::exit`。
- `src/fs.rs`（32 处 `std::fs::` 调用）、`src/modules.rs:172/:321`、`src/esm.rs:284/:811`、
  `src/esm_resolve.rs:60`、`src/package_json.rs`：所有读盘的地方。
- `src/permission.rs`：`Permissions`、`read()`/`read_here()` 检查。
- `src/sockets.rs:1538` `child_process_module`：目前是拒绝执行的空壳。
- `src/lib.rs`：`Runtime` 的公开 API、`define_builtin`、`ModuleMaker`。

## 接口契约

### 1. 结构化克隆公开：`verve::clone`

```rust
pub mod clone {
    /// 一个 JS 值的线程无关副本：Send + 'static。
    pub struct Cloned(/* 包住 worker::Cloned */);
    impl Cloned {
        pub fn from_value(value: &JsValue, context: &mut Context) -> JsResult<Cloned>; // clone_out
        pub fn to_value(&self, context: &mut Context) -> JsResult<JsValue>;             // clone_in
        pub fn undefined() -> Cloned;
        pub fn from_error_message(name: &str, message: &str) -> Cloned;                 // IPC 错误用
    }
}
```
- 必须 `Send`。支持的类型与 `structuredClone` 一致（含 `Blob`/`File`、错误、typed array、Map/Set、Date、RegExp、循环引用）。
- 函数、DOM 节点、Symbol → 抛 `DataCloneError`（与 Electron IPC 一致）。
- 不需要 transfer list（MessagePort 跨线程传递由 P12 另行处理）。

### 2. 进程形态：`verve::ProcessInfo`

把 `process.rs` 里写死的值改成可配置，默认值保持现状：

```rust
pub struct ProcessInfo {
    pub title: String,                     // 默认 "obsidian"（保持现状）
    pub process_type: Option<String>,      // 默认 Some("renderer")；Electron 主进程 "browser"；None = 删除该属性（verve run）
    pub argv: Vec<String>, pub argv0: String, pub exec_path: String, pub exec_argv: Vec<String>,
    pub resources_path: Option<String>,    // process.resourcesPath
    pub default_app: bool,                 // process.defaultApp（仅为 true 时定义）
    pub electron_fields: bool,             // versions.electron/chrome、process.contextIsolated 等是否出现
    pub sandboxed: bool,                   // process.sandboxed
    pub context_isolated: Option<bool>,    // process.contextIsolated（渲染器）
    pub exit: ExitBehavior,
}
pub enum ExitBehavior {
    /// 现状：记下，不结束任何东西（插件宿主用）。
    Note,
    /// `verve run`：真的结束进程。
    Process,
    /// 交给宿主：Electron 主进程把它变成 app.exit(code)；回调在 JS 线程上调用，
    /// 调用后当前脚本以不可捕获的中断结束（同 Isolate::interrupt）。
    Host(Arc<dyn Fn(i32) + Send + Sync>),
}
impl Runtime { pub fn set_process_info(&mut self, info: ProcessInfo); }
```
- `Setup`（`verve run`）改为在内部构造一个 `ProcessInfo`，保持 `verve run` 的现有输出不变。
- `set_process_info` 必须在第一次 `require('process')`/读 `process` 之前也能生效，之后调用则就地更新已有的 `process` 对象。

### 3. 虚拟文件系统叠加层：`verve::vfs`

给 P11（asar）用。Electron 的 `fs` 能透明读 `app.asar` 内部，`original-fs` 不能。

```rust
pub mod vfs {
    pub trait Overlay: Send + Sync {
        /// path（已规范化的绝对路径）是否落在本叠加层里；不在则返回 None，走真实文件系统。
        fn stat(&self, path: &Path) -> Option<io::Result<Metadata>>;
        fn read(&self, path: &Path) -> Option<io::Result<Cow<'static, [u8]>>>;
        fn read_dir(&self, path: &Path) -> Option<io::Result<Vec<DirEntry>>>;
        fn realpath(&self, path: &Path) -> Option<io::Result<PathBuf>>;
        /// 写操作落在叠加层里时的错误（asar 只读：EROFS 风格，见 P11）。
        fn write_denied(&self, path: &Path) -> Option<io::Error> { None }
        /// 需要真实路径的场合（child_process 执行 asar 里的文件、原生模块）：解包到哪里。
        fn materialize(&self, path: &Path) -> Option<io::Result<PathBuf>> { None }
    }
    pub struct Metadata { pub kind: Kind, pub size: u64, pub mtime_ms: f64, pub mode: u32, /* 其余按 fs.Stats 需要 */ }
    pub enum Kind { File, Dir, Symlink }
    pub struct DirEntry { pub name: String, pub kind: Kind }
}
impl Runtime {
    pub fn set_vfs(&mut self, overlay: Option<Arc<dyn vfs::Overlay>>);
    pub fn set_no_asar(&mut self, on: bool);     // process.noAsar：为 true 时绕过 overlay
}
```
- 接入点：`fs` 的**所有**读类入口（sync/callback/promises/streams/`Dir`/`FileHandle`/`exists`/`access`/
  `stat`/`lstat`/`realpath`/`readdir`/`opendir`/`watch` 返回 ENOTSUP 即可）、`require` 与 ESM 的读文件与
  `is_file`/`is_dir` 探测、`package.json` 读取。建议在 `src/fs.rs` 内部先抽出一层 `sys::{read, stat, read_dir, …}`
  私有函数，所有入口改调这一层，再在这一层判断 overlay —— 不要在 32 处各写一遍。
- 文件描述符：`fs.openSync` 打开 overlay 内文件时，返回一个虚拟 fd（内存缓冲），`read`/`fstat`/`close` 认得它。
- `original-fs`：目前在 `modules.rs:43` 被别名成 `fs`。改为：`original-fs` 是同一套实现但**不经过 overlay**
  （做法：同一个 maker 带一个标志，生成两份 exports）。
- 权限检查：先权限、后 overlay（asar 路径按其字面路径检查，属于 `app.asar` 所在目录的授权）。

### 4. 子进程权限位

`Permissions` 已有"保留的 `child_process` 位"（README：reserved bit does not enable execution）。改为：
```rust
pub struct Permissions { …, pub child_process: bool /* 默认 false */ }
```
本包只让这个位**存在并可查询**（`process.permission.has('child')` 跟随它）；真正执行由 P17 实现。

### 4b. 浏览器网络钩子：`verve::browser_net`（P16/P20 用）

WHATWG `fetch`（以及 verve-dom 的 XHR）需要能接入自定义 scheme、cookie、webRequest 和 UA，但**不影响** Node 的 `http`/`https`。

```rust
pub mod browser_net {
    pub trait BrowserNet: Send + Sync {
        /// 自定义 scheme（protocol.handle 注册的）：Some = 由宿主应答。
        fn handle_scheme(&self, request: &RequestHead, body: Option<Body>) -> Option<Pending<Response>>;
        /// 发出前：webRequest.onBeforeRequest / onBeforeSendHeaders、Cookie 头注入、UA 覆盖。
        fn before_send(&self, request: &mut RequestHead) -> Decision;   // Continue | Cancel | Redirect(url)
        /// 收到后：Set-Cookie 落盘、onHeadersReceived。
        fn after_receive(&self, request: &RequestHead, response: &mut ResponseHead);
    }
    pub struct RequestHead { pub url: String, pub method: String, pub headers: Vec<(String, String)>, pub resource_type: ResourceType, pub contents_id: Option<u32> }
}
impl Runtime { pub fn set_browser_net(&mut self, net: Option<Arc<dyn BrowserNet>>); }
/// fetch 的内部入口，带资源类型、可显式指定 BrowserNet：P20 加载器与 P16 的 net.fetch 共用。
pub fn fetch_internal(context: &mut Context, head: RequestHead, body: Option<Body>) -> JsResult<JsObject /* Promise<Response> */>;
```
`Pending`/`Body`/`Response` 复用 `src/fetch.rs` 现有的内部类型（公开最小必要部分）。具体字段以 P16 文档为准，本包负责接入点。

### 4c. 克隆的字节形式

`Cloned::to_bytes(&self) -> Vec<u8>` / `Cloned::from_bytes(&[u8]) -> Result<Cloned>`：P17 的 `serialization: 'advanced'` 跨进程 IPC 用。
格式只需 Verve 自己能读回（不要求兼容 V8 序列化）。

### 4c-2. 其他包登记的补充需求

- **`Cloned` 覆盖面**：确认并补齐 `RegExp`、`ArrayBuffer`（与 `Uint8Array` 区分）、各具体 typed array 类型、`Blob`/`File`；
  缺什么在 `worker.rs` 里补（Worker 也受益）。
- **跨 isolate 的 `MessagePort`**：`src/messaging.rs` 新增 `RemotePort`——一端在本 realm、另一端由别的 isolate 通过通道驱动，
  给 P12 的 `MessageChannelMain` / `postMessage` 传端口和 P17 的 `utilityProcess` 用。公开构造：
  `verve::messaging::remote_port_pair() -> (PortEnd, PortEnd)`，`PortEnd: Send`，`Runtime::adopt_port(PortEnd) -> JsValue`。
- **未捕获异常交给宿主**：`process.rs` 里 `uncaught` 无监听者时的分支改为可配置
  （`ProcessInfo::on_uncaught: UncaughtBehavior { Exit, Report(Arc<dyn Fn(String)>) }`）。Electron 主进程默认不退出、弹错误框（P10）。
- **overlay 的错误文本**：`Overlay` 增加 `fn error_message(&self, err: &io::Error, path: &Path) -> Option<String>`，
  让 asar 报出 Electron 的格式（`ENOENT, x not found in y`）。
- **代码缓存键**：asar 内路径的缓存键由 overlay 提供（`fn cache_key(&self, path) -> Option<[u8; 32]>`，P11 用 asar 头哈希 + 条目偏移）。

### 4c-3. 渲染器需要的内核入口（P20/P21/P24 登记）

- **非 file: URL 的 ESM 源钩子**：`src/esm.rs` 目前对 `app://`、`http(s)://`、自定义 scheme 抛 `ERR_UNSUPPORTED_ESM_URL_SCHEME`。
  新增 `Runtime::set_module_source(Option<Arc<dyn ModuleSource>>)`，`ModuleSource::load(url) -> Pending<(String /*源码*/, String /*最终 URL*/)>`，
  异步完成；以及 import map 的解析钩子 `Runtime::set_import_map(ImportMap)`（`imports` + `scopes`，按 HTML 规范解析）。
- **在新 Realm 里装 Verve 全局**：`Context::create_realm()` 只有 ECMAScript 内建。新增
  `Runtime::create_world(&mut self) -> WorldId` / `Runtime::enter_world(WorldId)`：建 Realm 并装上 `console`、定时器、`fetch`、
  `URL`、`TextEncoder` 等 Verve 全局（共享同一个事件循环与定时器队列）。P21 的 contextIsolation 依赖它。
- **带宿主路径的 `File`**：`File` 内部可携带 `Option<PathBuf>`（JS 不可见），`verve::web::file_with_path(context, bytes, name, path)`
  与 `verve::web::path_of_file(&JsValue) -> Option<PathBuf>`。给 P24（拖入文件）和 P21（`webUtils.getPathForFile`）。
- **跨 world 的监听器顺序**（仅 P21 方案 A 需要）：`dom_events` 中同一节点不同 Realm 注册的监听器按全局注册序号触发。
  先做 P21 的 spike 再决定是否实现本项。

### 4d. `Permissions::all()` 与子进程

`Permissions::all()` **不包含** `child_process`（保持 `verve run -A` 现有语义不变）；Electron 主进程显式设置 `child_process: true`。

### 5. 杂项

- `Runtime::define_builtin_lazy(name, ModuleMaker)`：按需构造的内置模块（`electron` 模块很大，第一次 `require` 才建）。
  若 `define_builtin_family` 已能覆盖则只写文档说明用法。
- `Runtime::set_console_sink(Option<Box<dyn Fn(Level, &str, Option<SourcePos>)>>)`：渲染器把 console 送给主进程
  的 `console-message` 事件（现有 `take_console` 是拉取式，保留）。
- `navigator.userAgent` 的产品名可配置（Electron 的 UA 带 `<appName>/<version>`）：`Runtime::set_user_agent(String)`。
- 导出 `verve::events::emit`（或等价的公开辅助）供 `verve-electron` 写 EventEmitter 子类；以及
  `native.rs` 里的 `class`/`function`/`closure`/`object`/`ClassSpec` 等工具以 `verve::native` 公开（标注为嵌入方 API）。
  verve-dom 目前怎样拿到这些的，照同一方式即可（先查 `crates/verve-dom` 用的是什么）。

## 步骤

1. `verve::clone` 包装 + 测试（往返所有支持类型；函数报 `DataCloneError`；跨线程 `Send`）。
2. `ProcessInfo` + 把 `process.rs` 的写死值换成读配置；`Setup` 走同一路径。测试：三种 ExitBehavior。
3. 抽出 `fs` 的 `sys` 层 → 接 overlay；`original-fs` 分离；模块加载接 overlay。测试：用一个内存 overlay
   （测试里写一个 `HashMap<PathBuf, Vec<u8>>` 实现）验证 `readFileSync`、`readdirSync`、`statSync`、
   `existsSync`、`createReadStream`、`fs.promises.readFile`、`require('./x')`、`import('./x.mjs')`、
   `package.json` 的 `main`/`exports` 都从 overlay 读到；`original-fs` 读不到；写入报错。
4. `child_process` 权限位；`set_console_sink`；`set_user_agent`；`native` 公开。
5. 跑全量 `cargo test`，以及 `cargo test --test rebon`（有 Rebon checkout 时）确认零回归。

## 验收

- `cargo test`（全部现有测试）通过，`verve run` 输出不变。
- 新增测试 `tests/embedder_hooks.rs` 覆盖上面 1–5。
- `cargo doc` 无警告，新公开项都有文档注释。

## 不做

asar 解析本身（P11）；子进程执行（P17）；Electron 模块（P10+）。

## 状态

（实现者完成后填写）
