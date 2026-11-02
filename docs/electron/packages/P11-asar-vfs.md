# P11 asar 与虚拟文件系统

**依赖**：P03（`verve::vfs::Overlay`、`Runtime::set_vfs`、`original-fs` 绕过 overlay）  **规模**：M  **修改**：新建独立 crate `crates/verve-asar/`（`src/{lib,archive,overlay,integrity,extract}.rs`，依赖 `verve`，不依赖 GUI），加入 workspace members；`MainGuest`/`RendererGuest` 安装 overlay 由 P10 负责（`verve-electron` 依赖 `verve-asar`）；内核仅限 P03 遗留项（`src/vfs.rs`、`src/fs.rs`、`src/fs/sys.rs`、`src/modules.rs`）

## 目标

打包后的应用放在 `resources/app.asar` 里，Electron 让它**看起来像一个目录**：`require`、`import`、`fs.readFileSync`、
`fs.statSync`、`fs.readdirSync`、`__dirname` 等对 `…/app.asar/…` 路径照常工作，未打包部分从
`app.asar.unpacked/` 读。Verve 要做到逐条一致，且不比读普通文件慢（头只解析一次、内容按需定位读取）。

## 背景（先读）

- `src/fs.rs`：所有 `fs` 入口（`read_file`、`stat_at`、`read_dir`、`access`、`open`、`copy`、`make_dir`、`open_dir`、
  `watcher` 等，以及 `module()` 里组装同步/回调/promise 三种形态）；错误由 `fs_error`/`io_error` 生成（Node 的 `code`/`errno`/`syscall`/`path`）。
- `src/modules.rs`：`bare_name` 现在把 `original-fs` **映射成同一个 `fs`**——P03 会把它拆成不经 overlay 的独立实例；
  `load_file` 用 `std::fs::read_to_string` 读源码（第 321 行）、`package.json` 读取（第 172 行）、`canonicalize`（第 492 行）。
- `src/esm.rs`（第 284、811 行读文件）、`src/esm_resolve.rs`（第 60 行读 `package.json`、第 479 行 `canonicalize`）。
- `src/package_json.rs`、`src/permission.rs`（权限检查以真实路径为准；asar 内路径检查的是归档文件本身）。
- `src/code_cache.rs`：代码缓存的键。
- P03 定义的 `verve::vfs::Overlay`。本包**按 P03 的最终签名实现**；下面列出本包需要 overlay 提供的能力，
  P03 若缺哪一项，在本包 PR 里写明"需要 P03 追加"并与 P03 协调，不要在 `src/` 里绕开。

## asar 格式（`archive.rs`）

文件布局（全部小端）：

```
偏移 0   u32  = 4                      // 第一个 Pickle 的负载大小
偏移 4   u32  = header_size            // 第二个 Pickle（头）的总字节数
偏移 8   u32  = header_size - 4        // 第二个 Pickle 的负载大小
偏移 12  u32  = json_len               // JSON 字符串字节数
偏移 16  [u8; json_len]                // UTF-8 JSON
         填充到 4 字节对齐
偏移 8 + header_size                   // 内容区起点 base；每个文件的 offset 相对它
```

头 JSON：

```json
{ "files": {
    "package.json": { "size": 312, "offset": "0", "integrity": { "algorithm": "SHA256", "hash": "<hex>", "blockSize": 4194304, "blocks": ["<hex>", "…"] } },
    "dist":  { "files": { "main.js": { "size": 1024, "offset": "312", "executable": true } } },
    "native": { "files": { "addon.node": { "size": 40960, "unpacked": true } } },
    "latest": { "link": "dist/main.js" }
} }
```

- 目录：有 `files`；文件：有 `size`，打包的文件有 `offset`（**十进制字符串**，可能超过 2^53，用 `u64` 解析）；
  `unpacked: true` 的文件**没有** offset，内容在 `<archive>.unpacked/<相对路径>`；`executable: true` 影响 mode；
  链接：有 `link`（相对归档根的路径），解析时跟随，最多 40 层，循环报 `ELOOP`。
- 目录也可能有 `unpacked: true`（整个目录在 `.unpacked` 下）。
- `integrity`：可选。

```rust
pub struct Archive {
    pub path: PathBuf,            // 归档文件的真实路径
    base: u64,                    // 内容区起点
    root: Entry,
    file: std::fs::File,          // 只读打开，定位读（Windows seek_read / Unix read_at），不加锁
    meta: std::fs::Metadata,      // 用来伪造 Stats，并检测归档被替换（size + mtime）
}
pub enum Entry {
    Dir { children: BTreeMap<String, Entry>, unpacked: bool },
    File { size: u64, offset: Option<u64>, unpacked: bool, executable: bool, integrity: Option<Integrity> },
    Link(String),
}
impl Archive {
    pub fn open(path: &Path) -> Result<Arc<Archive>, AsarError>;
    pub fn lookup(&self, inner: &str) -> Result<&Entry, AsarError>;   // 跟随链接
    pub fn read(&self, inner: &str) -> Result<Vec<u8>, AsarError>;
    pub fn read_range(&self, inner: &str, at: u64, len: usize) -> Result<Vec<u8>, AsarError>;
    pub fn list(&self, inner: &str) -> Result<Vec<(String, EntryKind)>, AsarError>;
    pub fn copy_out(&self, inner: &str) -> Result<PathBuf, AsarError>; // 解到临时目录，见下
    pub fn unpacked_path(&self, inner: &str) -> PathBuf;               // <archive>.unpacked/<inner>
}
```

- **归档缓存**：进程级 `Mutex<HashMap<PathBuf, Arc<Archive>>>`，主进程和所有渲染器共享（overlay 是 `Send + Sync`）。
  每次 `lookup` 前不重新 stat；归档被替换的检测只在 `open` 时做（Electron 也不检测运行中替换）。
- **头大小上限**：`header_size` 超过 256 MiB 或 JSON 解析失败 → `Invalid package <path>`。
- **integrity**：默认不校验（Electron 只在 `EnableEmbeddedAsarIntegrityValidation` fuse 打开时校验）。提供
  `AsarOptions { validate_integrity: bool }`；开启时每个文件首次读取按 `blockSize` 分块 SHA-256 校验，
  失败 → 进程以致命错误结束（Electron 行为），错误信息写 stderr。

## 路径识别

一个路径"在归档里" ⟺ 从左到右逐段拼接时，**某一段以 `.asar` 结尾**且该前缀是一个可解析的 asar 文件；
之后的部分是归档内路径（统一成 `/` 分隔、去掉前导分隔符）。

- 归档路径本身（`…/app.asar`）当作**目录**（`statSync(...).isDirectory() === true`）。
- 前缀以 `.asar` 结尾但不是合法归档（比如普通目录叫 `x.asar`）→ 按普通路径处理（缓存"不是归档"的判断，避免重复 stat）。
- Windows 上大小写不敏感地匹配 `.asar`；`\` 与 `/` 都接受。
- `process.noAsar === true`（运行时可改，每次判断时读，P03 需提供读这个标志的途径，或由 Electron 层在属性 setter 里调用
  `overlay.set_enabled(false)`——**推荐后者**）或环境变量 `ELECTRON_NO_ASAR` 非空 → 关闭识别，全部按普通路径。
- `original-fs` 永远不识别（P03 保证）。

## Electron 对 fs 的补丁清单（overlay 必须覆盖的行为）

以 Electron 39 `lib/node/asar-fs-wrapper.ts` 为准。下表中"归档内"指路径在归档里且未被 `noAsar` 关闭：

| API | 归档内的行为 |
|---|---|
| `readFileSync` / `readFile` / `promises.readFile` | 读归档内容；`encoding`、`flag`（只允许读）照常；`unpacked` 读 `.unpacked` 真实文件 |
| `statSync` / `stat` / `lstatSync` / `lstat` / `promises.stat`/`lstat` | 伪造 `Stats`：`dev/ino/uid/gid/nlink/blksize/atime/mtime/ctime/birthtime` 取自**归档文件本身**；`size` 取条目大小（目录为 0）；`mode` = 类型位（`S_IFREG`/`S_IFDIR`/`S_IFLNK`）| 归档文件的权限位，`executable` 再加 `0o111`；`lstat` 对 `link` 条目返回符号链接；`stat` 跟随；`bigint: true` 照常 |
| `existsSync` / `exists` | 条目存在即 `true` |
| `accessSync` / `access` / `promises.access` | 存在且 `mode` 不含 `W_OK` → 成功；含 `W_OK` → `EACCES`（见错误表）；`unpacked` 条目对真实文件做 access |
| `realpathSync` / `realpath` / `realpathSync.native` / `realpath.native` / `promises.realpath` | 归档前缀部分取真实路径，归档内部跟随 `link` 后原样拼接 |
| `readdirSync` / `readdir` / `promises.readdir` | 列条目名；`withFileTypes: true` 返回 `Dirent`（类型来自条目）；`recursive: true` 照常 |
| `opendirSync` / `opendir` | 返回在条目列表上迭代的 `Dir` |
| `openSync` / `open` / `promises.open` / `createReadStream` | 只读打开：`unpacked` → 打开真实文件；打包的文件 → `copy_out` 解到临时文件后打开它（Electron 的做法）；带写标志 → 与操作系统对"父路径是文件"的错误一致（`ENOTDIR`） |
| `copyFileSync` / `copyFile` / `promises.copyFile` / `cpSync`（源在归档内） | 从归档读出写到目标；目标在归档内 → `ENOTDIR` |
| `mkdirSync` / `mkdir`（在归档内） | `ENOTDIR` |
| `writeFile*`、`appendFile*`、`unlink*`、`rename*`、`rm*`、`rmdir*`、`symlink*`、`chmod*`、`utimes*`、`truncate*`（归档内） | 不补丁：结果与操作系统一致，即 `ENOTDIR`（归档是文件）；overlay 必须让它们**不要**落到 "把 app.asar 当目录创建" 的路径上 |
| `watch` / `watchFile`（归档内） | `ENOENT`（v1 不支持监视归档内容） |
| `fs.Stats` 以外的 `statfs` | 对归档文件本身 |
| 模块加载：`require`、`require.resolve`、`import`、`import()`、`import.meta.resolve`、`package.json` 读取、目录 `index.js`、扩展名补全 | 全部经 overlay：`modules.rs`/`esm.rs`/`esm_resolve.rs`/`package_json.rs` 里的 `std::fs::*` 调用由 P03 改成走 overlay |
| `.node` 原生模块在归档内 | 先按 `unpacked` 路径（打包器会把 `.node` 放到 `.unpacked`）；`load_file` 仍拒绝（P50 前），错误信息不变 |
| `process.dlopen` | 同上 |

**错误**（与 Electron 的 `createError` 一致，`errno` 是 libuv 负数）：

| 情况 | `message` | `code` | `errno` |
|---|---|---|---|
| 归档内找不到 | `ENOENT, <inner path> not found in <archive path>` | `ENOENT` | `-2`（Windows 上 Node 的 ENOENT errno 是 `-4058`，用 `fs.rs` 现有的 `code_of` 映射保持平台一致） |
| 把文件当目录 | `ENOTDIR, not a directory` | `ENOTDIR` | `-20`（平台映射同上） |
| 写访问 | `EACCES: permission denied, access '<path>'` | `EACCES` | `-13` |
| 归档损坏 | `Invalid package <archive path>` | 无 | |

另外要带上 Node 常规的 `syscall`、`path` 字段（Electron 的错误对象也有）。

## Overlay 需要提供的能力（与 P03 的签名对齐）

```rust
// 由 P03 定义；本包实现。这里列"需要什么"，不是最终签名。
trait Overlay: Send + Sync {
    /// 路径归谁管：普通文件系统 / 某个 asar 的条目 / unpacked（换成另一个真实路径）
    fn classify(&self, path: &Path) -> Classified;   // Real | Virtual(VirtualRef) | Redirect(PathBuf)
    fn stat(&self, v: &VirtualRef, follow: bool) -> io::Result<VirtualStat>;
    fn read(&self, v: &VirtualRef) -> io::Result<Vec<u8>>;
    fn read_range(&self, v: &VirtualRef, at: u64, len: usize) -> io::Result<Vec<u8>>;
    fn list(&self, v: &VirtualRef) -> io::Result<Vec<(String, FileKind)>>;
    fn realpath(&self, v: &VirtualRef) -> io::Result<PathBuf>;
    fn materialize(&self, v: &VirtualRef) -> io::Result<PathBuf>;   // copy_out，给 open/execFile 用
    fn error(&self, kind: OverlayErrorKind, v: &VirtualRef, syscall: &str) -> OverlayError; // 定制 message
}
```

如 P03 的签名没有 `error` 定制（Electron 的 message 与 Node 不同）或没有 `materialize`，需要 P03 追加。

## `copy_out`（临时解包）

- 目录：`<temp>/verve-electron-asar-<pid>/`，进程退出时删除（`Drop` + 尽力而为）。
- 同一条目只解一次（缓存 `inner path → temp path`）；保持文件名不变（放在 `<hash>/` 子目录里），`executable` 条目在 Unix 上 `chmod +x`。
- 用于：`open*`/`createReadStream` 打包文件、`child_process`（P17）。

## 给 P17 的约定（`child_process` 与 asar）

与 Electron 一致：
- `execFile`/`execFileSync`/`spawn`/`spawnSync`/`fork` 的**可执行文件路径**在归档内：
  条目 `unpacked` → 换成 `.asar.unpacked` 下的真实路径；否则 `materialize` 后执行临时文件。
- `cwd` 在归档内 → 换成 `.asar.unpacked` 对应目录（存在时），否则报 `ENOTDIR`（Electron 报错 `spawn ENOTDIR`）。
- `fork(modulePath)`：`modulePath` 在归档内照常（子进程是 Verve 的 `ELECTRON_RUN_AS_NODE` 模式，overlay 在子进程里同样安装）。
- 参数里出现的其他 asar 路径不改写。
- 入口是 `Runtime::materialize(path)`（内核已有），由 `AsarOverlay` 实现上面的规则；没有全局的 `resolve_for_exec`（overlay 属于 realm）。

## 代码缓存

asar 内脚本的缓存键 = `归档真实路径 + 条目 offset + 条目 size + 归档 mtime`（不要对内容做哈希，太慢）；
`unpacked` 条目按普通文件。需要 `code_cache.rs` 接受虚拟路径的键——若 P03 没有，写入"需要 P03 追加"。

## 步骤

1. `archive.rs`：解析 + 读取 + 链接跟随 + 单元测试（用测试里现写的归档：写一个最小的 asar 打包函数 `tests/support/asar.rs`，不依赖 npm 的 `@electron/asar`）。
2. 用 npm `@electron/asar` 打出的真实归档做 ignored 测试（夹具准备方式写进 PR）。
3. `overlay.rs`：实现 P03 trait；路径识别与 `noAsar`。
4. `MainGuest`、`RendererGuest`（含 `nodeIntegration: false` 时的 preload）安装同一个 overlay 实例。
5. `copy_out`（实现为 `Overlay::materialize`）。
6. 冻结 Electron 观测：写一个脚本 `tests/electron/asar/observe.js`，在真实 Electron 39 里对一组路径调用上表所有 API，
   输出 JSON；结果放 `tests/electron/asar/electron-39.json`，测试逐项比对（时间戳、ino 等不稳定字段只比类型）。

## 验收

- `cargo test -p verve-asar`：上表每一行至少一个用例；错误 message/code 与冻结观测一致。
- 夹具应用 `tests/electron/apps/packaged/`（P00）打成 `app.asar` 放到 `resources/`，`electron.exe` 直接启动：
  主进程 `require('./lib')`、ESM `import`、读取 JSON、`fs.readdirSync(__dirname)` 都正确；渲染器的 preload 从归档加载。
- 性能：10 000 个文件、50 MB 的归档，`Archive::open` ≤ 20 ms；`require` 500 个归档内模块与解包成目录相比耗时差 ≤ 10%。
- `process.noAsar = true` 后 `fs.statSync('…/app.asar').isFile() === true`；`require('original-fs')` 始终如此。

## 不做

写入/修改归档；`asar` 打包工具本身（`@electron/asar` 的 CLI）；Electron fuses 的二进制补丁（P40 记录约定即可）。

## 状态

### 完成了什么

- **独立 crate `crates/verve-asar`**（不是 `verve-electron`：P02 尚未合并，两边都建会冲突）。公开面：`Archive::{open, path, header_hash}`、
  `AsarOverlay`（实现 `verve::vfs::Overlay`）、`AsarOptions { validate_integrity, on_integrity_failure, disabled }`、`AsarError`、`IntegrityFailure`。
  P10 依赖它并 `rt.set_vfs(Some(Arc::new(AsarOverlay::new(..))))`；主进程与各渲染器共用同一个 `Arc`，归档只打开一次。
- 格式解析（Pickle 头、`offset` 十进制字符串按 `u64`、`unpacked`、`executable`、`link` 最多 40 层、目录 `unpacked` 向下继承、
  头上限 256 MiB 是 Verve 自己的防护）、定位读（无锁）、路径识别（任一段以 `.asar` 结尾且为合法归档；普通目录 `x.asar` 缓存为"不是归档"，
  不存在的名字不缓存）、归档真实路径在 `open` 时算一次。名字含分隔符或为 `.`/`..` 的头条目被丢弃；链接目标在 Windows 上按 `/` 和 `\` 切分。
- 覆盖 `stat`/`lstat`/`read`/`read_dir`/`realpath`/`write_denied`/`access_denied`/`materialize`/`error_message`/`cache_key`。
  `fs` 的 sync/回调/promise/流/`openSync`/`opendir`/`copyFile`/`cp`，`require`/`import()`/`require.resolve`、`package.json` 的 `main`、`original-fs`、
  `process.noAsar`、`.asar.unpacked`、归档内写入、归档内缺失文件，均有测试。
- **integrity**：`validate_integrity` 开启时，每次读取打包文件，比较整个文件的 SHA-256 与头里的 `integrity.hash`（Electron 的 `fs` 读取路径
  `ValidateIntegrityOrDie` 如此；`blocks` 是 Chromium 流式校验用的，不读）。失败返回 `AsarError`（`is_integrity()`），并先调用
  `on_integrity_failure`；**库本身不打印也不退出**，Electron 的 `LOG(FATAL)` 等价物由 P10 在回调里做。
  `Archive::header_hash()` 返回头 JSON 的 SHA-256（十六进制）。
- **`materialize`**（`Runtime::materialize`，P17 调用）：unpacked 文件 → `.asar.unpacked` 下真实路径；打包文件 → 解到随机名的私有目录
  （`<temp>/verve-electron-asar-<pid>-<16 位十六进制>`，`DirBuilder` 0o700、必须新建；文件 `create_new`、0o600/0o700，不跟随链接；
  每个条目只解一次，不同条目的解包互不等待；overlay 释放时删除）；目录/归档本身 → `.asar.unpacked` 对应目录（存在时），否则 `ENOTDIR`。
- **内核的 P03 遗留项（各自独立提交）**：`Overlay::lstat`；`Kind::mode` 公开；`Overlay::access_denied`（`accessSync(.., W_OK)`：归档内打包文件
  报 Electron 的 `EACCES: permission denied, access '<归档内路径>'`，unpacked 文件问真实文件）；`modules::forget` 经 overlay 的 `realpath`；
  `verve::encoding::hex`（去掉 `code_cache`/`crypto`/`npm` 里的三份副本）。

### 与 Electron / 任务书不同、已知差异

- 写入归档内路径：`write_denied` 返回 `ENOTDIR`（P03 的测试 overlay 用 `ENOTSUP`）。归档文件本身不拦截，交给磁盘，与 Electron 一致。
- `mode` 按 Electron 的 `statMode`：`0o644`+类型位；目录、链接、可执行再加 `0o111`；链接再加 `0o022`。时间戳取归档文件的 mtime
  （Electron 是固定值）；链接的 `size` 取目标路径长度。`Metadata` 只有 `kind/size/mtime/mode`，所以 `dev`/`nlink`/`ino`/`uid`/`gid` 是内核的默认值
  （Electron：`dev=1`、`nlink=1`、`blksize=4096`、真实 uid/gid、递增的 `ino`）；要补齐需要 R1/P03 给 `Metadata` 加字段。
- 链接成环：返回 `ENOENT, … not found in …` 而不是 `ELOOP`（`io::ErrorKind` 没有稳定的 `FilesystemLoop`；Electron 对成环也答找不到）。
- 打包文件的 `open`/`createReadStream`/`fs.promises.open` 沿用 P03 的 `Handle::Overlay`（整文件读入内存），不解到临时文件；只有 `materialize` 才落盘。
- `errno` 沿用 `fs.rs` 的平台映射（Windows 上 ENOENT 为 -4058），不是 Electron 写死的 -2。`Invalid package` 的 `code` 是 `EIO`，Electron 无 code。
- 损坏的 `x.asar` 本身当作普通文件读，访问其内部才报 `Invalid package`。
- 头里**一条**记录畸形（缺 `size`/`offset` 等）会让整个归档判为 `Invalid package`，而 Electron 只让那一条找不到。
- `.asar` 后缀在所有平台都不区分大小写（与 Electron 一致）。
- `ELECTRON_NO_ASAR`：库不读环境变量。Electron 只在 `process.type` 既非 `browser` 也非 `renderer` 时才应用它；P10 按此规则决定是否传
  `AsarOptions { disabled: true, .. }`（或干脆不安装 overlay）。
- 没做：`watch`/`watchFile` 对归档内路径报 `ENOENT`（P03 的永不触发的 watcher 未改，overlay trait 没有对应入口）；`statfs`；
  代码缓存键没有接 `cache_key`：`code_cache` 以源码内容为键，asar 里的模块照样命中，改成"归档路径+偏移+mtime"会变弱；`cache_key` 已实现并测试，是否接入留给 P42；
  `readlink` 对归档内路径仍是操作系统的答案（Electron 也不补丁它）。
- 没做：用 `@electron/asar` 打的真实归档的 ignored 测试、冻结 Electron 39 观测 JSON（`tests/electron/asar/`）、`electron.exe` 夹具验收、
  `require` 500 个模块 vs 目录的性能对比：这些依赖 npm/真实 Electron/P00。行为按 `asar-fs-wrapper.ts`、`asar_util.cc`、`@electron/asar` 的源码写，
  **未与真实 Electron 39 逐项比对**。`ELECTRON_NO_ASAR` 由 P10 负责，这里只有 `disabled` 的测试。
- 10 000 个文件的头解析有测试，但带墙钟上限，标了 `#[ignore]`（`cargo test -p verve-asar -- --ignored`）；release 下 ≤ 20 ms 未测。

### 测试

- `cargo test -p verve-asar`：单元测试（格式、链接、unpacked、integrity、畸形头、名字加固、解包目录不被占用）与 `--test asar`（`fs`/`require`/`import`/`original-fs`/
  `noAsar`/写入/access/mode/解包/完整性回调）全过；`cargo test --test embedder_hooks` 含 `lstat` 与 `W_OK` 用例。

### 给其他包的备注

- **P10**：依赖 `verve-asar`，在主进程与每个渲染器的 runtime 上安装同一个 `Arc<AsarOverlay>`。要做的事：
  `--verve-skip-asar-integrity` → `validate_integrity: false` 并打印警告；`on_integrity_failure` 里打印并终止进程（Electron 的 `LOG(FATAL)`）；
  `ELECTRON_NO_ASAR` 非空且 `process.type` 既非 `browser` 也非 `renderer` 时 `disabled: true`；fuse `EnableEmbeddedAsarIntegrityValidation` 打开时，
  用 `Archive::open(..).header_hash()` 与 exe 资源里的 `ElectronAsarIntegrity` 比对（本包只做文件摘要校验）。`process.noAsar` 由内核提供。
- **P17**：`Runtime::materialize(path)` 给出可执行的真实路径；`cwd` 在归档内时先 `materialize`，返回 `ENOTDIR` 即报 `spawn ENOTDIR`。
- **P42**：`Archive::header_hash()` 就是头 JSON 的 SHA-256；`AsarOverlay::cache_key` 可用但未接入 `code_cache`。
- **P40**：本 crate 没有 `.asar` 写入器（只有测试里的 `tests/support`）；`ElectronAsarIntegrity` 的比对移到了 P10。
- **R1/P03**：`Metadata` 需要 `dev/ino/uid/gid/nlink` 才能与 Electron 的 `Stats` 一致。
