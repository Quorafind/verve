# P17 真实 `child_process`、`ELECTRON_RUN_AS_NODE` 与 `utilityProcess`

**依赖**：P03（`Permissions::child_process` 真正生效、`Cloned` 的字节编码）  **规模**：L  **修改**：`src/child_process.rs`（新建）、`src/child_process/{unix.rs,windows.rs,ipc.rs}`（新建）、`src/sockets.rs`（删掉 `child_process_module` 存根，改为转调新模块）、`src/network/reactor.rs`（管道与子进程退出）、`src/process.rs`（`process.kill`、子进程侧 `process.send`/`'message'`）、`src/cli.rs`（新 flag、Node 模式入口）、`src/permission.rs`（`process.permission.has('child')`）；`crates/verve-electron/src/api/utility_process.rs`；`electron` 二进制入口（P02 定下的位置）

## 目标

1. Node 22 的 `child_process` **完整可用**：`spawn`、`exec`、`execFile`、`fork`，以及 `spawnSync`、`execSync`、`execFileSync`；
   `ChildProcess` 的属性、方法、事件顺序与 Node 一致。只在授予权限时启用：
   - Electron 主进程、`nodeIntegration: true` 的渲染器：默认授予（P10/P21 设置 `Permissions { child_process: true, .. }`）。
   - `verve run`：默认拒绝，`-A`、`--allow-child-process`（Node 22 拼写）或 `--allow-run`（Deno 拼写）授予。
2. `ELECTRON_RUN_AS_NODE=1 electron.exe script.js` 把 `electron.exe` 当 Node 用（VS Code、各种 CLI 工具、`fork()` 都依赖它）。
3. `fork()` 的 IPC 通道与 Node 兼容：Verve fork Verve、Verve fork 真 Node（`execPath` 指向 node）、真 Node fork Verve（`execPath` 指向 electron.exe）三种组合都能 `send`/`'message'`。
4. Electron 的 `utilityProcess.fork` 与 `process.parentPort`。

## 背景（先读）

- `src/sockets.rs` 第 1454 行起：现在的 `child_process` 存根（`child_process_module`、`child_init`、`refused`、`spawn_error`），
  类和常量都在，`spawn` 等一律拒绝。本包用真实实现替换它，**保留**拒绝路径给未授权的 realm（未授权时行为与现在逐字相同）。
- `src/permission.rs` 的 `child_process(context)` 与 `Permissions::child_process`：注意现在的检查只在"受限 realm"时才看该位；
  P03 会把语义改成"位为真才允许执行"。`Permissions::all()` 目前**不**含 `child_process`（`..Self::default()`），P03 决定是否改，
  本包的 `-A` 处理以 P03 结果为准。
- `src/network/reactor.rs`：每个 realm 一个 mio 轮询器；`Command`/`Event` 枚举；`Read`/`Write`/`End`/`Close` 的读额度背压。
  子进程的 stdio 管道接入它，做法同 TCP 流。
- `src/network/tcp.rs` 与 `src/sockets.rs` 的 `net.Socket`：Node 中 `child.stdout` 等是 `net.Socket` 实例
  （`child.stdout instanceof net.Socket === true`）。本包让管道复用 Verve 的 `Socket` 类，只是底层 id 指向管道。
- `src/process.rs`：`process.kill`（约第 224 行，现在一律 `EPERM`）、`process.execPath`、`process.versions`（`NODE_VERSION = "22.20.0"`）。
- `src/cli.rs`：`main`/`main_with`/`Setup`、权限 flag 解析（第 236 行附近 `--allow-worker`）、退出规则（ref 计数：定时器、socket、worker…）。
- libuv `src/win/process.c`、`src/win/process-stdio.c`、`src/unix/process.c`：Node 的真实行为来源，Windows 部分照它实现。

## 接口契约

### 1. 模块结构

```
src/child_process.rs        JS 绑定：模块对象、ChildProcess 类、spawn/exec/execFile/fork、*Sync
src/child_process/spec.rs   SpawnSpec：从 JS 选项规范化出的纯 Rust 描述（见下）
src/child_process/unix.rs   std::process::Command + CommandExt::pre_exec（setsid、uid/gid、额外 fd 的 dup2）
src/child_process/windows.rs 自写 CreateProcessW（不用 std::process，理由见第 4 节）
src/child_process/ipc.rs    Node IPC 通道：json 与 advanced 序列化、NODE_CHANNEL_FD
```

```rust
pub(crate) struct SpawnSpec {
    pub file: String,                 // 解析 PATH 之前的程序名
    pub args: Vec<String>,            // 含 argv0 位置（args[0]），与 Node 的 spawnargs 一致
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,   // 已合并；Windows 上键按大小写不敏感去重并排序（CreateProcessW 要求）
    pub stdio: Vec<StdioSpec>,        // 下标即子进程 fd
    pub detached: bool,
    pub uid: Option<u32>, pub gid: Option<u32>,      // 仅 unix；Windows 上传了则 ENOTSUP
    pub windows_hide: bool,
    pub windows_verbatim_arguments: bool,
    pub shell: Option<String>,        // 已展开：true → 平台默认 shell
}
pub(crate) enum StdioSpec {
    Pipe { readable: bool, writable: bool },   // 'pipe' / 'overlapped'
    Ignore,                                    // 'ignore' → /dev/null 或 NUL
    Inherit(i32),                              // 'inherit' / 数字 fd / process.stdout 等
    Stream(reactor::Id),                       // 已打开的 net.Socket/fs fd 传给子进程
    Ipc,                                       // 至多一个
}
```

### 2. JS 层行为（逐条对照 Node 22 文档与源码 `lib/internal/child_process.js`、`lib/child_process.js`）

- `spawn(file, args?, options?)`：参数校验错误同 Node（`ERR_INVALID_ARG_TYPE`、`ERR_INVALID_ARG_VALUE`、参数含 `\0` 时 `ERR_INVALID_ARG_VALUE`）。
  启动失败里只有 `ENOENT`、`EACCES`、`EAGAIN`、`EMFILE`、`ENFILE` 是**异步**的；其余（如 `.bat` 的 `EINVAL`、Windows 上传 `uid` 的 `ENOTSUP`）同步抛 `spawn <file> <CODE>`：返回的 `ChildProcess` 在下一轮发 `'error'`，`pid === undefined`，
  `spawnfile`/`spawnargs` 已设置；成功则下一轮发 `'spawn'`。
- 选项：`cwd`（字符串或 `URL`）、`env`、`argv0`、`stdio`（`'pipe'`/`'overlapped'`/`'ignore'`/`'inherit'`/数组；数组元素还可为 `'ipc'`、
  `null`/`undefined`、`Stream`、非负整数、`process.stdin` 等）、`detached`、`uid`/`gid`、`serialization`（`'json'` | `'advanced'`）、
  `shell`（`true` 或路径）、`windowsVerbatimArguments`、`windowsHide`、`signal`（`AbortSignal`，中止时以 `killSignal` 杀并发
  `'error'`（`AbortError`））、`timeout`、`killSignal`（默认 `'SIGTERM'`）。
- 事件顺序：`'spawn'` → 数据 → `'exit'(code, signal)` → 所有 stdio 流结束且（有 IPC 时）通道断开后 `'close'(code, signal)`。
  有 IPC 时：`'message'(msg, sendHandle)`、`'disconnect'`。
- 属性：`pid`、`stdin`/`stdout`/`stderr`（`net.Socket`，非 pipe 时为 `null`）、`stdio` 数组、`connected`、`exitCode`、`signalCode`、
  `killed`、`spawnfile`、`spawnargs`、`channel`（有 IPC 时，含 `ref`/`unref`）。
- 方法：`kill(signal = 'SIGTERM')`（返回是否送达；数字或名字；未知名字抛 `ERR_UNKNOWN_SIGNAL`）、`send(message, sendHandle?, options?, callback?)`、
  `disconnect()`、`ref()`/`unref()`、`[Symbol.dispose]()`。`unref` 后子进程不阻止 `verve run` 退出（接入 `cli.rs` 的 ref 计数）。
- `exec(command, options?, callback?)`：`shell` 默认平台 shell；收集 stdout/stderr（`encoding` 默认 `'utf8'`，`'buffer'` 时为 Buffer）；
  `maxBuffer` 默认 `1024 * 1024`，超出时杀子进程并以 `ERR_CHILD_PROCESS_STDIO_MAXBUFFER` 回调；非零退出时 `error.code = 退出码`、
  `error.killed`、`error.signal`、`error.cmd`。`execFile(file, args?, options?, callback?)` 同理不经 shell。两者都要有
  `util.promisify.custom`，`promisify(exec)` 得到 `{ stdout, stderr }`，失败的 error 上也带 `stdout`/`stderr`。
- `fork(modulePath, args?, options?)`：`execPath` 默认 `process.execPath`，`execArgv` 默认 `process.execArgv`，stdio 默认
  `['pipe','pipe','pipe','ipc']`，`silent: false` 时 0–2 为 `'inherit'`；`env` 中加入 `NODE_CHANNEL_FD=<ipc 下标>` 与
  `NODE_CHANNEL_SERIALIZATION_MODE`。**在 Electron 中**（`process.versions.electron` 存在且 `execPath` 是自身）额外设置
  `ELECTRON_RUN_AS_NODE=1`——这与 Electron 行为一致。
- `spawnSync(file, args?, options?)`：返回 `{ pid, output, stdout, stderr, status, signal, error? }`；`input`（字符串/Buffer/TypedArray/DataView）
  写入 stdin；`timeout`、`killSignal`、`maxBuffer`（超出：`error.code === 'ENOBUFS'`）。`execSync`/`execFileSync`：成功返回 stdout；
  失败抛错，错误对象带 `status`、`signal`、`output`、`stdout`、`stderr`、`pid`。`execSync` 默认 stdio `'pipe'`，但子进程 stderr
  **同时**写到父进程 stderr（Node 行为，除非显式指定 stdio）。
- 同步版本阻塞调用者线程（这是语义），实现可用阻塞 I/O：子进程 stdout/stderr 各用一个临时线程读取，或 unix 上 `poll`，
  避免两管道互相写满死锁。**不**经过 reactor。

### 3. 异步实现：接入 reactor

- `reactor::Command` 追加：

```rust
Command::Spawn(Id /* 子进程 id */, SpawnSpec, Vec<Id> /* 为每个 Pipe/Ipc 预分配的流 id */),
Command::Kill(Id, i32 /* 信号号；Windows 上 0 表示探测 */),
```

  `reactor::Event` 追加：

```rust
Event::Spawned(u32 /* pid */),
Event::SpawnFailed(std::io::ErrorKind, i32 /* errno，用于 code 名 */),
Event::Exited(Option<i32> /* code */, Option<i32> /* signal */),
```

  管道流复用现有的 `Read`/`Write`/`End`/`Close` 命令与 `Data`/`Written`/`End`/`Closed` 事件——对 JS 的 `Socket` 而言它就是一条流。
- **Unix**：父端用 `mio::unix::pipe`（非阻塞）注册到轮询器；子进程退出用 `SIGCHLD` 自管道：安装一次 `SIGCHLD` 处理函数（`libc::sigaction`，
  `SA_RESTART | SA_NOCLDSTOP`），处理函数只往一个注册在 mio 上的非阻塞管道写 1 字节；轮询器读到后对所有跟踪中的 pid
  `waitpid(pid, WNOHANG)`。多个 realm 共享同一个处理函数：全局表 `pid → (realm 的 reactor 句柄, Id)`，由处理线程分发。
  Linux ≥ 5.3 可选用 `pidfd_open` 代替，但保留 `SIGCHLD` 路径作为通用实现。**不**为每个子进程开线程。
- **Windows**：
  - 管道：父端用 `CreateNamedPipeW`（`\\.\pipe\verve-<pid>-<随机>`，`FILE_FLAG_OVERLAPPED | PIPE_ACCESS_*`），子端用 `CreateFileW`
    打开同名管道、可继承、**非** overlapped（与 libuv `uv__create_stdio_pipe_pair` 相同）。父端交给 `mio::windows::NamedPipe`
    注册到轮询器。
  - 退出：`RegisterWaitForSingleObject(process_handle, callback, WT_EXECUTEONLYONCE)`，回调里 `GetExitCodeProcess` 后通过 reactor 的
    `mio::Waker` 投递 `Exited`。系统线程池执行回调，不新开线程。
- 进程句柄、pid、管道生命周期由 reactor 持有；realm 销毁时：未 `detached` 的子进程**不**被杀（Node 行为：父进程退出不杀子进程），
  只关闭父端管道与等待句柄。

### 4. Windows 进程创建细节（`windows.rs`）

不用 `std::process::Command`，原因：(1) 需要 `lpReserved2` 把额外 fd（含 IPC 管道）以 CRT 格式传给子进程，真 Node 子进程靠它找到
`NODE_CHANNEL_FD`；(2) 需要完全控制命令行拼接以复刻 libuv（`std` 的 `.bat` 防注入规则与 Node 不同）。照 libuv 实现：
- 程序查找 `search_path`：先 `cwd`，再 `PATH` 各项；文件名无扩展名时依次试 `""`（仅当文件名有点）、`.com`、`.exe`——**不用 `PATHEXT`**，
  与 libuv 一致。找不到 → `ENOENT`。
- 命令行：每个参数按 libuv `quote_cmd_arg` 规则加引号与转义反斜杠；`windowsVerbatimArguments: true` 时原样以空格拼接。
- `shell`：`shell === true` 时用 `process.env.ComSpec || 'cmd.exe'`，参数为 `['/d', '/s', '/c', '"' + command + '"']` 且自动
  `windowsVerbatimArguments = true`（Node 的 `normalizeSpawnArguments`）。
- 不经 shell 直接 spawn `.bat`/`.cmd` 文件：抛 `EINVAL`（Node 18.20.2/20.12.2 起的 CVE-2024-27980 修复行为）。
- 环境块：`UTF-16`，按 `RtlCompareUnicodeString` 不区分大小写排序；确保 `SYSTEMROOT` 等 libuv 要求的变量存在（`required_vars`）。
- 标志：`CREATE_UNICODE_ENVIRONMENT`；`windowsHide` → `STARTF_USESHOWWINDOW` + `SW_HIDE`，且对控制台程序加 `CREATE_NO_WINDOW`；
  `detached` → `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`。非 detached 的子进程放进一个全局 job object（`KILL_ON_JOB_CLOSE | SILENT_BREAKAWAY_OK`，同 libuv `uv__init_global_job_handle`），
  所以父进程退出时子进程随之结束，与 Node 一致。
- `kill`：信号 `0` → 探测（`GetExitCodeProcess == STILL_ACTIVE`）；`SIGTERM`/`SIGKILL`/`SIGINT`/`SIGQUIT`/其他 → `TerminateProcess(handle, 1)`，
  之后 `'exit'` 的 `signal` 参数报告所发的信号名、`code` 为 `null`（libuv 行为）。
- 依赖：仓库已有 `windows-sys`，追加 features `Win32_System_Pipes`、`Win32_System_Console`、`Win32_System_Environment`、`Win32_System_IO`。

### 5. IPC 通道（`ipc.rs`）

- 传输：父子之间一条双向流（unix：`socketpair(AF_UNIX, SOCK_STREAM)`；Windows：双工命名管道），子进程 fd 号为 stdio 中 `'ipc'` 的下标，
  经环境变量 `NODE_CHANNEL_FD` 告知。Windows 上该 fd 由 `lpReserved2` 的 CRT 表传递（`FOPEN | FPIPE` 标志，同 libuv）。
- `serialization: 'json'`（默认）：每条消息 `JSON.stringify(msg) + '\n'`，与 Node **逐字节兼容**；Node 内部消息（`cmd` 以 `NODE_` 开头）
  不分发给用户的 `'message'`。
- `serialization: 'advanced'`：Node 用 V8 序列化格式，Verve 无法逐字节兼容。规则：Verve ↔ Verve 时使用 `Cloned` 的字节编码
  （需要 P03 追加 `Cloned::to_bytes`/`Cloned::from_bytes`，带 4 字节长度前缀成帧）；检测到对端是真 Node（握手失败或收到无法解析的帧）
  时发 `'error'`（`ERR_IPC_CHANNEL_CLOSED` 风格的明确错误，信息注明 "advanced serialization is only supported between Verve processes"）。
  在 P41 登记。
- `send(message, sendHandle, options, callback)`：`sendHandle`（传递 `net.Socket`/`net.Server`/`dgram.Socket`）v1 不支持：同步抛
  `ERR_NOT_SUPPORTED` 风格错误（Node 无此错误码，用 `Error` + `code: 'ERR_FEATURE_UNAVAILABLE_ON_PLATFORM'`，信息说明原因）。
  `callback` 在写入完成后调用；通道已关闭时 `callback(ERR_IPC_CHANNEL_CLOSED)` 或无回调时发 `'error'`。返回值是否有背压（`false`）同 Node。
- **子进程侧**（`src/process.rs`）：启动时若 `NODE_CHANNEL_FD` 存在且 realm 有 child_process 权限或处于 Node 模式（`verve run`、
  `ELECTRON_RUN_AS_NODE`），则建立 `process.send`、`process.connected`、`process.disconnect`、`process.channel`，`process` 发
  `'message'`/`'disconnect'`；读取后从环境中删除 `NODE_CHANNEL_FD`（Node 行为）。通道是 ref 的：有监听者时阻止退出，
  `process.channel.unref()` 后不阻止。

### 6. `ELECTRON_RUN_AS_NODE`

- `electron` 二进制 `main` 的第一件事：若环境变量 `ELECTRON_RUN_AS_NODE` 存在且非空，则**不启动 GPUI 与 Electron 层**，直接调用
  `verve::cli::main_with(node_style_args, setup)`，其中 `node_style_args` 把 argv 当成 Node 的命令行解释：
  `electron.exe [node 选项] [script | -e code | -p code | -] [args]`，以及 `--version`/`-v`（打印 `v22.20.0`，即 `NODE_VERSION`）、
  `--help`。现在的 `verve::cli` 以 `verve run …` 为界面——需要在 `src/cli.rs` 新增 `pub fn main_node(args, setup) -> ExitCode`
  （Node 风格解析，复用 `run` 的实现），本包新增。
- Node 模式的权限：等同 `-A` 且含 `child_process`（Node 默认无权限模型）；Node 的 `--permission` 出现时才启用 Verve 的权限模型并
  按 `--allow-*` 授予。
- Node 模式下 `process.versions.electron` 仍存在、`process.type` 为 `undefined`、`require('electron')` 按普通包解析（真实 Electron 行为）。
- P40 的 fuses：若打包配置关闭了 `runAsNode` fuse，则忽略该变量。本包读取 P40 定义的 fuse 位置（P40 未合并时预留一个
  `fn run_as_node_allowed() -> bool { true }`）。

### 7. `utilityProcess`

- `utilityProcess.fork(modulePath, args?, { env, execArgv, cwd, stdio, serviceName, allowLoadingUnsignedLibraries, respondToAuthRequestsFromMainProcess })`
  → `UtilityProcess`（EventEmitter）：`pid`、`stdout`/`stderr`（`stdio: 'pipe'` 时为 `Readable`）、`postMessage(message, transfer?)`、`kill()`；
  事件 `spawn`、`exit(code)`、`message(message)`、`error(type, location, report)`（Electron 34+）。子进程内：`process.parentPort`
  （`on('message', e => e.data / e.ports)`、`postMessage`）。只能在 `app` ready 之后调用。
- **实现为 isolate，不是 OS 进程**，与本方案"渲染器是 isolate"的原则一致：好处是 `MessagePortMain` 可直接在线程间转移
  （`postMessage(msg, [port])` 把端口交给工具进程，是常见用法，跨 OS 进程无法支持），内存更省；panic 只结束该 isolate，
  `kill()` 用 `Isolate::interrupt` + 丢弃。
  - 代价：`pid` 与主进程相同（Electron 中不同）；`process.exit()` 在工具 isolate 内只结束它自己（需在该 realm 的 `process.exit` 上挂钩）；
    `stdout`/`stderr` 是进程内流（工具 isolate 的 `console`/`process.stdout.write` 被路由过去），`stdio: 'inherit'` 写到主进程的
    stdout/stderr。在本文档"状态"与 P41 中写明 `pid` 差异。
  - 工具 isolate 的 Runtime：`Permissions` 与主进程相同（含 `child_process`），`process.type === 'utility'`，`require('electron')`
    只暴露 `net`、`systemPreferences`（Electron 中工具进程可用的子集）。
  - 消息序列化：`Cloned`（P03）；`transfer` 中的 `MessagePortMain` 由 P12 的端口机制处理——本包依赖 P12 提供
    `fn transfer_port(port) -> PortHandle` / `fn adopt_port(handle) -> JsObject`，P12 未合并时 `transfer` 非空抛错。

## 步骤

1. 与 P03 确认：`Permissions::child_process` 生效语义、`Permissions::all()` 是否含它、`Cloned` 字节编码。
2. `spec.rs`：从 JS 选项规范化 `SpawnSpec`（完全照 Node `normalizeSpawnArguments`，含 shell 展开、`argv0`、`env` 合并）。
   单元测试覆盖各平台的 shell 展开结果。
3. unix 后端 + reactor 管道 + `SIGCHLD` 自管道；`ChildProcess` 类与事件顺序；`kill`；`process.kill` 真实化。
4. Windows 后端（CreateProcessW、命名管道、`RegisterWaitForSingleObject`、`lpReserved2`、引号规则、`.bat` 规则）。
5. `exec`/`execFile`/`promisify.custom`；`*Sync` 三个。
6. IPC：json 模式双向（父与子两侧），advanced 模式 Verve↔Verve。
7. `cli.rs`：`--allow-child-process`/`--allow-run`、`main_node`；`electron` 入口的 `ELECTRON_RUN_AS_NODE` 分支。
8. `utilityProcess` 与 `process.parentPort`。
9. `process.permission.has('child')` 按权限位回答；README 里 `child_process` 的描述改为新行为（"Process execution … remain denied" 一句改写）。

## 验收

所有测试都用 `env!("CARGO_BIN_EXE_verve")` 作为子程序（`verve run -e …`），不依赖系统命令，三平台一致；另有少量平台测试用
`cmd.exe`/`/bin/sh`。

- `cargo test --test child_process`：
  - `spawn` 回显 stdin → stdout，事件顺序 `spawn, data…, exit, close`；`child.stdout instanceof net.Socket`。
  - 不存在的程序：同步返回、异步 `'error'`（`code: 'ENOENT'`、`syscall: 'spawn <file>'`、`path`、`spawnargs`），`pid === undefined`。
  - `exec('<verve> -e "process.exit(3)"')` 回调 `error.code === 3`；`promisify(exec)` 结果 `{stdout, stderr}`。
  - `maxBuffer` 超出、`timeout` 杀进程（`error.killed === true`、`signal === 'SIGTERM'`）、`AbortSignal` 中止。
  - `kill('SIGKILL')` 后 `exit` 回调 `(null, 'SIGKILL')`（Windows 同样报告信号名）。
  - 10 MB 输出通过管道不丢字节、不死锁，期间 realm 线程数不增加（与 `tests/network.rs` 的线程观测方法相同）。
  - 100 个并发子进程全部正确退出，线程数不随子进程数增长。
  - `spawnSync` 的 `input`、`status`、`ENOBUFS`；`execSync` 失败抛错带 `status`/`stderr`，且子进程 stderr 出现在父 stderr。
  - Windows：带空格与引号的参数原样到达子进程（子进程打印 `process.argv` 比对）；`spawn('x.bat')` 无 shell 时 `EINVAL`；
    `shell: true` 运行 `.bat` 成功；`windowsHide` 不报错。
  - unix：`detached: true` 后子进程是新会话首进程（子进程打印 `process.pid` 与 `getsid` 比对，可用 `ps`/`/proc`）。
  - `fork()`：父子 json IPC 双向、`disconnect` 事件两侧、子进程 `process.send` 回调；`advanced` 模式传 `Map`/`Date`/`Uint8Array`。
  - 未授权 realm：`spawn` 与现在的存根行为逐字相同（`ERR_ACCESS_DENIED` / 不可用的错误），由现有 `tests/permission.rs` 覆盖不回归。
- `--test child_process_node`（`#[ignore]`，需要 `VERVE_NETWORK_NODE` 指向真 Node 22，与 `tests/network.rs` 同一变量）：
  Verve fork 真 Node、真 Node fork Verve（`execPath: electron.exe` 并设置 `ELECTRON_RUN_AS_NODE`）json IPC 往返。
- `cargo test -p verve-electron --test run_as_node`：`ELECTRON_RUN_AS_NODE=1 electron -e "console.log(process.versions.node)"` 输出
  `22.20.0`，不开窗口、不初始化 GPUI（无头 CI 上可运行即证明）；`electron --version`（Node 模式）输出 `v22.20.0`。
- `--test utility_process`：`utilityProcess.fork` 后 `postMessage` 往返；把 `MessageChannelMain` 的一端转给工具进程后与主进程直接通信；
  `kill()` 后发 `exit`；工具 isolate 内死循环被 `kill()` 终止。
- 体积增量 ≤ 150 KB。

## 不做

`sendHandle`（传递 socket/server 句柄）、`advanced` 序列化与真 Node 互通、`stdio` 中 fd ≥ 3 的额外管道在 Windows 上传给非 Verve/非 Node
的程序（CRT 表对它们无意义但无害，照传）、`cluster` 模块的真实实现（仍按现状拒绝）、`utilityProcess` 的独立 OS 进程模式、
`process.on('SIGINT')` 等信号监听（另立任务）。

## 状态

### 完成了什么

- **权限**：`Permissions::child_process` 为真才启动程序；为假时行为与原来逐字相同（受限 realm 抛 `ERR_ACCESS_DENIED`/`ChildProcess`，其余为 `ENOSYS` 存根，
  存根搬到 `src/child_process/denied.rs`，`sockets.rs` 降到 1458 行、退出 `large-files.txt`）。每个入口在调用时再查一次位，不只在建模块时。
  `verve run` 新增 `--allow-child-process`（Node 拼写）、`--allow-run`（Deno 拼写）；`-A` 在 `cli.rs` 里加上该位（`Permissions::all()` 仍不含）。
  `process.permission.has('child')` 跟随该位（P03 已做）。
- **`spawn`/`exec`/`execFile`/`fork` 与 `spawnSync`/`execSync`/`execFileSync`**，事件顺序 `spawn` → 数据 → `exit` → `close`；`child.stdout instanceof net.Socket`；
  `kill`（名字/数字、`ERR_UNKNOWN_SIGNAL`）、`killed`/`signalCode`、`ref`/`unref`（接入 `network::keeps_alive`）、`timeout`、`killSignal`、`signal`（`AbortError`）、
  `maxBuffer`（`ERR_CHILD_PROCESS_STDIO_MAXBUFFER`/`ENOBUFS`）、`encoding`、`cwd`（字符串或 URL，默认 realm 的 cwd 而非进程 cwd）、`env`、`argv0`、`detached`、
  `windowsHide`、`windowsVerbatimArguments`、`shell`（Node 的 `normalizeSpawnArguments` 规则）、`stdio`（`pipe`/`overlapped`/`ignore`/`inherit`/`ipc`/0–2 号 fd/带 `fd` 的流）、
  `promisify(exec|execFile)`（`{stdout, stderr}`，失败的 error 带二者）、`execSync` 的 stderr 同步写到父进程 stderr。启动失败异步：`'error'` 然后 `'close'`（负 errno），`pid === undefined`，
  `spawnfile`/`spawnargs` 已设。进程退出时销毁 `stdin`、冲刷没人读的输出流（Node 的 `onexit`/`flushStdio`）。
- **`process.kill(pid, signal)`** 对有授权的 realm 真实发送（Windows：`TerminateProcess`，信号 0 探测）；`ESRCH`/`EPERM` 同 Node。
- **Windows 后端**（`windows.rs`）：手写 `CreateProcessW`，照 libuv——`search_path`（先 cwd 再 `PATH`，`.com`/`.exe`，不用 `PATHEXT`）、`quote_cmd_arg`、环境块（大小写不敏感排序 + `required_vars`）、
  `lpReserved2` 的 CRT fd 表、`.bat`/`.cmd` 无 shell 时 `EINVAL`、`windowsHide`/`detached` 标志、命名管道（父端 overlapped、子端普通句柄；IPC 与 fd≥3 的子端也 overlapped 以便子进程自己轮询）、
  `RegisterWaitForSingleObject` 退出通知（无线程）。**与 libuv 的差别**：用 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 把可继承句柄限定为子进程自己的几个——libuv 不限，
  结果是孙进程会攥着父进程的 stdout 管道不放（测试里它让整个 harness 挂住）。
- **Unix 后端**（`unix.rs`）：`std::process::Command`，所有管道都是 `UnixStream::pair`（同 libuv），fd≥3 在 `pre_exec` 里先挪开再 `dup2`，`detached` → `setsid`，`uid`/`gid`。
- **reactor 接入**（`child_process/pipes.rs`）：`Command::{Pipe,Track,Kill,Reap}`、`Event::Exited`；管道复用 `Read`/`Write`/`End`/`Close` 与 `Data`/`Written`/`Finished`/`End`/`Closed`，
  读额度一次一块（64 KiB），写一次一块在途。`reactor.rs` 只加了几行钩子，管道逻辑全在新模块里。
- **IPC**（`ipc.rs`、`fork.rs`）：`fork` 的 `execPath`/`execArgv`/`silent`/`stdio`/`serialization`、`NODE_CHANNEL_FD`、`NODE_CHANNEL_SERIALIZATION_MODE`、Electron 里自动 `ELECTRON_RUN_AS_NODE=1`；
  `send`（回调、背压返回值、`ERR_IPC_CHANNEL_CLOSED`）、`'message'`、`NODE_` 内部消息分流、`disconnect`（两侧事件）、`channel.ref/unref`。
  `serialization: 'json'` 是 Node 的线格式（换行分隔），`'advanced'` 是结构化克隆字节 + 4 字节长度（仅 Verve↔Verve）。
  **子进程侧**：`verve run` 起来时若有 `NODE_CHANNEL_FD` 就接上（`process.send`/`connected`/`disconnect`/`channel`/`'message'`），读完后从 `process.env` 删掉；
  只有存在 `message`/`disconnect` 监听者时通道才阻止退出。
- **`cli::main_node`**（`ELECTRON_RUN_AS_NODE` 的入口）：`--version`（`v22.20.0`）、`--help`、`-e`、`-p/--print`、脚本；Node 模式等同 `-A`（含 child），`process.versions.electron` 保留，`process.type` 无。
  新增 `-p/--print`（`verve run` 也能用）。`process::Setup` 多了 `electron` 字段。
- **工具箱**：`native::invalid_arg_type`/`invalid_arg_value`（Node 的 `ERR_INVALID_ARG_TYPE/VALUE` 措辞）。测试辅助（`Ran`/`verve`/`wait`）从 `tests/cli.rs` 挪到 `tests/common`。

### 没完成

- **`utilityProcess` 与 `process.parentPort`**：推迟。仓库里没有 `crates/verve-electron`、P02 的 Hub、P12 的 `MessagePortMain`（`transfer_port`/`adopt_port`），
  `transfer` 非空的行为也无从依赖。要等 P02（isolate 与消息枢纽）和 P12 合并。`--test utility_process`、`cargo test -p verve-electron --test run_as_node` 因此未做；
  `main_node` 的行为由根 crate 的 `tests/run_as_node.rs`（`harness = false`，自己充当 electron 二进制）覆盖。
- **`electron` 二进制入口的 `ELECTRON_RUN_AS_NODE` 分支**：入口属于 P02，本包只提供 `verve::cli::main_node`。**fuse `runAsNode`** 未接（P40 未合并，也没有预留函数——没有调用者就不留）。
- **与真 Node 的 IPC 互通**：Windows 的 `lpReserved2` 表按 libuv 格式写，json 线格式是 Node 的，理论上两个方向都通；但没有真 Node 可测，`--test child_process_node` 没写。
  真 Node 当子进程时，IPC 子端我们建的是 overlapped 句柄，libuv 的 `uv_pipe_open` 会识别并走 IOCP——未验证。
- **`sendHandle`**（按任务书不做）：抛 `ERR_FEATURE_UNAVAILABLE_ON_PLATFORM`。
- **`stdio` 里的 `Stream`**：只支持 0–2 号 fd 与带 `fd` 属性的流；`fs.openSync` 得到的 fd 与已打开的 `net.Socket` 传给子进程未做（Verve 的 fd 表不是 OS fd）。
  `spawnSync` 里 fd≥3 的 `pipe` 被当作 `ignore`。
- **Review 修复**：`.bat`/`.cmd` 按请求的文件名与找到的路径双检（尾随点/空格，CVE-2024-36138）；env/args/argv0/shell/file 里的 NUL 拒绝；`disconnect` 先 `end()` 排空再关；等待句柄先注销再关进程句柄；Windows 信号只认 0/2/3/9/15（其余 `ENOSYS`）；全局 job object；`windowsHide` 仅在没有 inherit 时加 `CREATE_NO_WINDOW`；同步版本不再轮询；`spawn` 带 `'ipc'` 即设置 `NODE_CHANNEL_FD`；`main_node` 认 `--permission`、忽略未知 Node 选项、不解析 Verve 子命令。
- **仍未做（review 的次要项）**：`child.kill()` 对已退出进程的 ESRCH/EPERM 不能同步得知（命令走 reactor，返回的是「进程还没报告退出」）；退出时不移除 `AbortSignal` 的监听；`pipes.rs` 的多个 bool 未改成枚举；`cli::parse`/`script` 未拆；Unix 子进程在 realm 被丢弃而未回收时会成为僵尸（长寿命多 realm 宿主需要进程级回收器，`pidfd_open` 为后续）；Windows 上 IPC 子端的 overlapped 假设（真 Node 父进程）未用 `FileModeInformation` 校验；`stdio:'inherit'` 在 index≥3 未支持。
- **Unix 的退出检测**用 reactor 每圈（≤20 ms）`try_wait`，不是 `SIGCHLD` 自管道：全局信号处理函数与多 realm 分发在没有 Unix 机器可测的情况下风险大于收益，延迟 ≤20 ms。
  Windows 是事件驱动的。
- **`[Symbol.dispose]`**、`child.stdio` 里的 `overlapped` 区分、`process.on('SIGINT')`（任务书不做）。
- 没做 release 体积测量（`scripts/size-report` 未跑）；新依赖为 0，只是 mio 开了 `os-ext`、windows-sys 加了 `Win32_System_Console`/`Win32_System_Pipes`。

### 已知问题 / 发现

- **Unix 代码没在真机跑过**：本机是 Windows。`unix.rs`/`launch.rs` 单独抽出来用 `--target x86_64-unknown-linux-gnu` 做过类型检查（整个 crate 在该 target 上因 `ring` 需要 C 工具链而无法检查），测试里的 unix 分支（`sh -c` 回显）没有运行。
- 一个预先存在的流问题：`readable.once('data', fn)` 不会让 Verve 的流开始流动（`on('data')` 才会），所以子进程 `stdout.once('data')` 永远等不到。我没有改 `stream.rs`（超出本包）；测试里改用 `on`。
- `fork()` 默认的 `execArgv` 是 `process.execArgv`；对 `verve run -e <source>` 它含 `-e <source>`，原样传下去子进程会重跑同一段脚本。Node 为此把 `-e`/`-p` 及其源码从继承的 `execArgv` 里去掉（靠 `process._eval`），我们照做（`cli::eval` 设置 `process._eval`），有不传 `execArgv` 的测试。第一版没做，测试时 fork 炸了一百多个进程。
- Windows 上 `process.kill(pid)` 杀掉的子进程 `exit` 事件是 `(1, null)`（libuv 同），只有 `child.kill()` 会报信号名。

### 给其他包的备注

- **P02**：`electron` 入口读 `ELECTRON_RUN_AS_NODE` 后调 `verve::cli::main_node(std::env::args_os(), setup)`；`Permissions { child_process: true, .. }` 由 P10/P21 设置。
  `utilityProcess` 需要 Hub 与端口转移，见上。
- **P11（asar）**：`child_process` 要跑 asar 里的文件时需 `Runtime::materialize`；本包的 `spawn` 未接，`spec.file` 和 `cwd` 目前按原样交给系统。
- **P40**：`runAsNode` fuse 在 `main_node` 的调用方判断即可。

### 验收情况

- 新增 `tests/child_process.rs`（18 个通过，1 个 `#[ignore]` 的 100 并发）、`tests/run_as_node.rs`（`harness = false`）、`src/child_process/{spec,ipc,windows}.rs` 的单元测试。
- 覆盖：事件顺序、`ENOENT`、退出码与 `SIGKILL`、参数引号（空格/引号/反斜杠/`^&|`/`%`/`*`）、env/cwd/argv0、`shell: true`（Windows 的 `cmd`、Unix 的 `sh`）、`where`、`.bat` 的 `EINVAL` 与 shell 运行、
  10 MiB 管道不丢字节、慢读者背压、`exec`/`promisify`/`maxBuffer`/`timeout`/`AbortSignal`、`spawnSync` 的 `input`/`ENOBUFS`/`ETIMEDOUT`、`execSync` 的 stderr、`fork` 的 json 与 advanced、`disconnect` 两侧、`process.kill`、未授权。
- 既有的 `cli`、`permission`、`node`、`node_misc`、`embedder_hooks`、`network` 测试全过（`cli.rs` 里两处断言随语义改：`--allow-child-process` 不再是 9，受限进程的 `-A` 对照项删去）。
