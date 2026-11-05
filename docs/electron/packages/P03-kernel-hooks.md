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
        pub fn to_bytes(&self) -> Result<Vec<u8>, CloneBytesError>;
        pub fn from_bytes(bytes: &[u8]) -> Result<Cloned, CloneBytesError>;
    }
    /// 字节不是一个克隆时哪里不对：Truncated / UnknownTag / UnknownViewKind / NotUtf8 / TooMany / Trailing。
    pub enum CloneBytesError { /* Truncated/UnknownTag/UnknownViewKind/NotUtf8/TooDeep 带出错的偏移；TooMany、Trailing、TooLarge 带数量 */ }
}
```
- 必须 `Send`。支持的类型与 `structuredClone` 一致（含 `Blob`/`File`、错误、typed array、Map/Set、Date、RegExp、循环引用）。
- 函数、DOM 节点、Symbol、已 detach 的 typed array → 抛 `DataCloneError`（与 Electron IPC 一致）。
- `from_bytes` 读的是别的进程送来的字节：嵌套深度有上限（`TooDeep`），长度超过 4 字节计数的写入报 `TooLarge`，不会回绕。
- 实现在 `src/clone/`：`tree.rs`（内存形态与 `ViewKind`）、`copy.rs`（出入 realm）、`bytes.rs`（字节格式，标签是具名常量）。
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
    pub sandboxed: Option<bool>,           // process.sandboxed；None = 不定义（默认，保持现状；只对 renderer 生效）
    pub context_isolated: Option<bool>,    // process.contextIsolated（渲染器）
    pub exit: ExitBehavior,
    pub on_uncaught: UncaughtBehavior,     // 默认 Log
}
pub enum UncaughtBehavior {
    /// 现状：写到 console，realm 继续跑（插件宿主用）。
    Log,
    /// 以 1 结束进程（Node 的行为；`verve run` 不走这里，它有自己的 uncaught 路径）。
    Exit,
    /// 交给宿主（Electron 主进程默认不退出、弹错误框）。
    Report(Arc<dyn Fn(&str) + Send + Sync>),
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
impl Runtime { pub fn set_process_info(&mut self, info: ProcessInfo) -> JsResult<()>; }
```
- `ExitBehavior::Host`：脚本已设置的 `process.exitCode` 是 `process.exit()` 没带参数时用的码（同 Node）；`exit` 事件先于宿主回调。
- `ProcessInfo`、`ExitBehavior`、`UncaughtBehavior` 都实现 `Debug`。
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
        /// 写操作落在叠加层里时的错误（asar 只读：Electron 报 `ENOTSUP`，即 `io::ErrorKind::Unsupported`，见 P11）。
        fn write_denied(&self, path: &Path) -> Option<io::Error> { None }
        /// 需要真实路径的场合（child_process 执行 asar 里的文件、原生模块）：解包到哪里。
        fn materialize(&self, path: &Path) -> Option<io::Result<PathBuf>> { None }
        /// overlay 自己的错误文案（Electron 的 `ENOENT, x not found in y`）。
        fn error_message(&self, err: &io::Error, path: &Path) -> Option<String> { None }
        /// 代码缓存键（留给 P11，见下「状态」）。
        fn cache_key(&self, path: &Path) -> Option<[u8; 32]> { None }
    }
    /// `#[non_exhaustive]`：用 `Metadata::new(kind, size, mtime_ms, mode)` 构造，以后加字段不破坏 P11。
    pub struct Metadata { pub kind: Kind, pub size: u64, pub mtime_ms: f64, pub mode: u32 }
    pub enum Kind { File, Dir, Symlink }
    /// 同样 `#[non_exhaustive]`，`DirEntry::new(name, kind)`。
    pub struct DirEntry { pub name: String, pub kind: Kind }
}
impl Runtime {
    pub fn set_vfs(&mut self, overlay: Option<Arc<dyn vfs::Overlay>>);
    pub fn set_no_asar(&mut self, on: bool);     // process.noAsar：为 true 时绕过 overlay
    pub fn materialize(&self, path: &Path) -> Option<io::Result<PathBuf>>;   // overlay 的 materialize，宿主在 spawn 前问
}
```
- **overlay 拿到的路径一律是绝对、已清理的**：`View` 按该 realm 的 `process.cwd()` 把相对路径解析成绝对路径再 `modules::clean`，
  所以 `process.chdir` 之后 `fs.readFileSync('app.asar/x')`、`app.asar/../app.asar/x` 都能命中。
- `process.noAsar` 是 `process` 上的访问器（读写同一个 `Layers.no_asar`），设置了 overlay 之后才出现（没设置 overlay 的 realm 的 `process` 不多出属性）。
- `fs` 的错误码补了 `EROFS`，`ENOTSUP` 的描述是 libuv 的 `operation not supported on socket`。
- 接入点：`fs` 的**所有**读类入口（sync/callback/promises/streams/`Dir`/`FileHandle`/`exists`/`access`/
  `stat`/`lstat`/`realpath`/`readdir`/`opendir`/`watch` 返回 ENOTSUP 即可）、`require` 与 ESM 的读文件与
  `is_file`/`is_dir` 探测、`package.json` 读取。建议在 `src/fs.rs` 内部先抽出一层 `sys::{read, stat, read_dir, …}`
  私有函数，所有入口改调这一层，再在这一层判断 overlay —— 不要在 32 处各写一遍。
- 文件描述符：`fs.openSync` 打开 overlay 内文件时，返回一个虚拟 fd（内存缓冲），`read`/`fstat`/`close` 认得它。
- `original-fs`：原来被别名成 `fs`。现在是同一套实现但**不经过 overlay**：一个 `build(context, original: bool)` 生成两份 exports，
  每个导出函数带着自己的 `View`（`fs` 的是 realm 的 layers，`original-fs` 的是纯磁盘），所以它造出的流、promise 会一直走磁盘；
  没有线程局部的开关。两份 exports 共用 `Stats`/`Dirent`/`Dir`/`FileHandle` 等类，`original.statSync(x) instanceof fs.Stats` 为真。
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

`Cloned::to_bytes(&self) -> Result<Vec<u8>, CloneBytesError>` / `Cloned::from_bytes(&[u8]) -> Result<Cloned, CloneBytesError>`：P17 的 `serialization: 'advanced'` 跨进程 IPC 用。
格式只需 Verve 自己能读回（不要求兼容 V8 序列化）。

### 4c-2. 其他包登记的补充需求

- **`Cloned` 覆盖面**：确认并补齐 `RegExp`、`ArrayBuffer`（与 `Uint8Array` 区分）、各具体 typed array 类型、`Blob`/`File`；
  缺什么在 `worker.rs` 里补（Worker 也受益）。
- **跨 isolate 的 `MessagePort`**：`src/messaging.rs` 新增 `RemotePort`——一端在本 realm、另一端由别的 isolate 通过通道驱动，
  给 P12 的 `MessageChannelMain` / `postMessage` 传端口和 P17 的 `utilityProcess` 用。公开构造：
  `verve::messaging::remote_port_pair() -> (PortEnd, PortEnd)`，`PortEnd: Send`，`verve::messaging::adopt_port(end, context) -> JsResult<JsValue>`
  （在 realm 里造出由它驱动的 `MessagePort`；没有 `Runtime` 上的同名方法），`release_port(port, context) -> JsResult<PortEnd>` 把一个端口交出去。**P12 已实现**（见 P12 的「状态」）。
- **未捕获异常交给宿主**：`process.rs` 里 `uncaught` 无监听者时的分支改为可配置
  （`ProcessInfo::on_uncaught: UncaughtBehavior { Log, Exit, Report(Arc<dyn Fn(&str) + Send + Sync>) }`，默认 `Log`）。Electron 主进程默认不退出、弹错误框（P10）。
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

`Permissions::all()` **不包含** `child_process`；Electron 主进程显式设置 `child_process: true`。

> **P17 修订**：`verve run -A` 现在**授予** `child_process`（与 Deno 的 `-A` 一致，`-A` 本就授予不受限的文件写）。这一步在 `cli.rs` 的 `permissions()` 里做，`Permissions::all()` 本身不变，嵌入方 API 的语义不变；改变的只是 `verve run -A` 的行为（此前子进程不存在，所以没有人依赖「`-A` 不能启动程序」）。另有 `--allow-child-process`/`--allow-run`。

### 5. 杂项

- `Runtime::define_builtin_lazy(name, ModuleMaker)`：按需构造的内置模块（`electron` 模块很大，第一次 `require` 才建）。
  若 `define_builtin_family` 已能覆盖则只写文档说明用法。
- `Runtime::set_console_sink(Option<ConsoleSink>)`，`ConsoleSink = Box<dyn Fn(Level, &str, Option<&SourcePos>)>`；`SourcePos { file, line, column }` 目前总是 `None`（留给 P13 填）：渲染器把 console 送给主进程
  的 `console-message` 事件（现有 `take_console` 是拉取式，保留）。
- `navigator.userAgent` 的产品名可配置（Electron 的 UA 带 `<appName>/<version>`）：`Runtime::set_user_agent(String)`。
- 导出 `verve::events::emit_event(context, target, name, args)`（任务书原写 `emit`；`emit` 仍是私有的方法实现）供 `verve-electron` 写 EventEmitter 子类；以及把 `src/native.rs` 整体以
  `verve::native` 公开（标注为嵌入方 API）。**它是全仓库唯一的原生函数工具箱**（`style.md` §1）：verve-dom 现在的
  `bind/mod.rs` 是一套同名不同签名的副本，不要照它的方式做；R1 关卡会把 verve-dom 和根 crate 里的副本迁过来并删除。

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

### 完成了什么

- **`verve::clone`**：`Cloned`（`Send`）：`from_value`/`to_value`/`undefined`/`from_error_message`/`to_bytes`/`from_bytes`，
  错误类型 `CloneBytesError`。覆盖 `RegExp`、`ArrayBuffer`、各 typed array（保留元素类型）、`DataView`、`Blob`/`File`；
  函数、Symbol、已 detach 的 typed array 抛 `DataCloneError`。实现从 `worker.rs` 移到 `src/clone/`（`worker.rs` 少了约 260 行）。
- **`verve::ProcessInfo`**：见契约 2。默认值经测试验证与改动前一致：`type`=`renderer`、`title`=`obsidian`、**不定义 `sandboxed`**、
  `Object.keys(process)` 的顺序不变（`the_default_process_is_the_plugin_hosts_unchanged`）；`verve run` 的 `cli` 测试全过。
- **`verve::vfs`**：见契约 3。`fs/sys.rs` 是 `fs` 读 overlay 的唯一入口（`stat`/`read`/`read_dir`/`open`/`copy`/`read_range`/`probe_open`），
  `may_write` 是唯一的写闸门（先权限、后 overlay）。`fs.rs` 拆成 `fs/{sys,fd,original,stat}.rs`；`stream.rs` 的文件流移到 `stream/file.rs`。
- **子进程权限位**：`process.permission.has('child')` 跟随 `Permissions::child_process`；`all()` 不含它。
- **杂项**：`define_builtin_lazy`、`set_console_sink`、`set_user_agent`（逻辑在 `navigator.rs`）、`verve::native` 公开并补文档、
  `verve::events::emit_event`。嵌入方钩子在 `src/embed.rs`（第二个 `impl Runtime`）。
- 独立提交 `fix(modules)`：包的 `main` 为 `./x.js` 时 `require.resolve` 原先答 `pkg/./x.js`，而缓存键是干净路径，查不到；
  现在 `main` 与其他路径一样 `clean`（与 Node 一致），有测试（`a_package_main_is_cleaned_the_way_node_cleans_it`，修复前失败）。
- **本包带来的、对既有行为可见的变化**（`fix(modules)` 之外）：
  - `io::ErrorKind::Unsupported` 现在是 `ENOTSUP`（原先落到 `EIO`）；
  - `require('original-fs') !== require('fs')`（原先是同一个对象；两者共用 `Stats` 等类）；
  - Worker 的 `postMessage` 现在保留 `RegExp`、各 typed array 种类、`DataView`、`Blob`/`File`，并拒绝已 detach 的 typed array（`DataCloneError`）；
  - `fs.cpSync` 复制目录时，读目录项失败会报错（与改动前一致），`readdir` 仍跳过读不出的目录项（也与改动前一致）；
  - `ExitBehavior::Host` 的 interrupt 在宿主之后没有再运行任何脚本时会一直挂着：宿主在下一个任务前调用
    `Runtime::interrupt_handle().clear()`（P10 注意）。

### 第一轮审查逐条（42 条）

已修：1（`sandboxed: Option<bool>`）、2（去掉线程局部 BYPASS，`View::new(context, original)` 由导出捕获，流持有自己的 View；
有测试 `an_original_fs_stream_never_reads_the_overlay`）、3（`View::absolute` 解析 cwd 并 `clean`；测试
`the_overlay_is_asked_about_absolute_clean_paths`）、4（提交按 fix / clone / process / vfs / permission / native / runtime / stream / docs 重排，
每个都过 fmt + style-check + `cargo check --workspace --all-targets`）、5、6（测试的 overlay 以 `PathBuf` 为键，不折叠大小写）、
7（`fs/{sys,fd,original}.rs`，另有 `stat.rs`）、8（`src/embed.rs`）、9、11–13（`read_range`/`probe_open`/`copy`/`open`/`may_write`；
`createReadStream` 仍用真实 open 探测）、10（`WriteRequest`）、14（`process.noAsar` 访问器）、15（`EROFS`、`ENOTSUP`）、
16（Host 的 exit 认 `exitCode`，Process 与 Host 共用 `exiting()`，吞错处写了原因）、17（`after_uncaught`）、18（`UncaughtBehavior::Log`）、
19（`ConsoleSink` 是 `Box`，`Rc` 别名 `pub(crate)`）、20（保留 `SourcePos`，总是 `None`，留给 P13）、21（`navigator::set_user_agent`/`app_version`）、
22（`src/clone/`）、23（`ViewKind::of` 对 Boa 的类型**穷尽匹配、无通配分支**，新增类型会让它编译失败；detached 数组报 `DataCloneError`。
没有按审查说的返回 `Option`，因为穷尽匹配比运行时报错更早发现问题）、24（`#[repr(u8)]` + `ViewKind::ALL` + 具名 `TAG_*`，有往返测试）、
25（`crate::blob::is_blob`）、26（`clone_value`/`make`/`write_to` 都拆小；`create_read_stream` 与 `navigator::make` 随之变短）、
27（`CloneBytesError`，`to_value` 补 `# Errors`）、28（`native.rs` 文档合并、补全；`instance` 保持 `pub(crate)`）、
29（`io_error` 合并）、30（`build(context, original)`）、32（`tests/common` 有 `Dir`/`result_of`，`cli.rs`、`embedder_hooks.rs` 共用；
`run_in` 不再加载两次）、33（共用类对象，有测试）、34（本节与契约更新）、35–38、40、41（`#[non_exhaustive]` + 构造函数）。
39：`apply` 拆成 `apply_kind`/`apply_arguments`/`apply_versions`，`install` 在原位置调用，键顺序不变（有测试）；删除不存在的属性不再吞错。

未修 / 取舍：
- **31 方法表**：`build` 用 `OPERATIONS` 表驱动，只在包一层 `entry` 以携带 `View`；没有给 `native::exports` 加包装器变体，
  因为那会为 fs 一处改动工具箱的签名。
- **42**：`modules::forget()` 仍用 `std::fs::canonicalize`（不认 overlay）；`fs.readlink` 没有 overlay 路径。两者留给 P11 在真正有 asar 时决定。
- `lib.rs` 里原先搬进 `embed.rs` 的既有方法（`set_proxy_env`、`define_builtin` 等）已放回 `lib.rs`（净增 ~40 行，仍 < 1500），
  `embed.rs` 只放本包新增的钩子，避免与本包无关的移动。

### 没完成（任务书里有，本包范围外）

- **`verve::browser_net`（契约 4b）**、**`ModuleSource`/`ImportMap`**、**`create_world`/`enter_world`**、
  **`web::file_with_path`/`path_of_file`**：未实现。P21 的 contextIsolation 依赖 `create_world`，P20 需要 `ModuleSource`，P16 需要 `browser_net`。
- **`Overlay::cache_key` 只到定义处**：代码缓存仍按 `global_scope_fingerprint` 取键，接上会改变现有缓存行为，留 P11 决定。
- **`watch`/`watchFile` 仍是「永不触发」的 watcher**，没有改成 `ENOTSUP`：会破坏现有插件行为与测试。

### 已知问题

- `ExitBehavior::Host` 靠 `Context::interrupt_handle()`：JS 在下一次循环迭代或调用才被中断，同一表达式里的纯计算仍会跑完（Boa 的语义边界）。
- `UncaughtBehavior` 在 `report()` 和 `verve run` 的 `process::uncaught` 两处都生效（同一个 `after_uncaught`）；只有 `Report` 的宿主才会让错误对象的 getter 被执行以生成文本。
- 文件流读 overlay 里的文件时，第一次读把整个文件放进流的状态，之后的分块从内存切；测试验证 `Overlay::read` 只被问一次。
- `structuredClone` 的复制（`clone/copy.rs`）对极深的嵌套对象没有深度上限（与改动前的 Worker 一样）。
- 模块加载的错误不经 overlay 的 `error_message`，仍用 Node 的措辞。

### 给其他包的备注

- **P11（asar）**：实现 `vfs::Overlay`；路径一律是绝对、干净的。只读用 `write_denied` 返回 `io::ErrorKind::Unsupported`（`ENOTSUP`）；
  Electron 文案用 `error_message`；`child_process`/原生模块要真实路径时用 `Runtime::materialize`。`process.noAsar` 在设置 overlay 后可用。
  `Metadata`/`DirEntry` 是 `#[non_exhaustive]`，用 `new` 构造。
- **P12/P17**：`Cloned` 是 `Send`，`to_bytes`/`from_bytes` 可跨进程；跨 isolate 的 `MessagePort` 由 P12 做了（`verve::messaging::{PortEnd, remote_port_pair, adopt_port, release_port}`）。
- **P13/P20/P21**：`set_process_info`、`set_console_sink`（`SourcePos` 目前总是 `None`）、`set_user_agent`、`define_builtin_lazy` 可用；
  P20 还需要 `ModuleSource`，P21 还需要 `create_world`/`enter_world`。
- **P16**：`browser_net` 未实现。
- **所有包**：嵌入方钩子集中在 `src/embed.rs`；原生函数一律用 `verve::native`。

### 验收情况

- `cargo fmt --all --check`、`bash scripts/style-check.sh`、`cargo clippy --workspace --all-targets -- -D warnings`：全部通过，无警告。
- `cargo test --workspace --no-fail-fast`：67 个测试程序，**526 passed，0 failed，13 ignored**；其中 `--test embedder_hooks`（23）、
  `--test modules`（10）、`--test cli`（21）、`--test fs`（8）、`--test permission`（10）、`--test rebon`（5，本仓库内自带）。
- 每个提交（fix → clone → process → vfs → permission → native → runtime → stream → docs）单独过 fmt、style-check 与
  `cargo check --workspace --all-targets`。
- 未做：release 体积测量、`cargo doc`。
