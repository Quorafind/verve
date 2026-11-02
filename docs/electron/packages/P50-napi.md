# P50 N-API 原生模块（v2）

**依赖**：M4 完成（v1 发布之后）；P17（真实进程，用于测试工具链）  **规模**：XL  **修改**：新建 `crates/verve-napi/`（需新增）；`src/modules.rs`（`.node` 的加载路径，现为拒绝）、`src/process.rs`（`process.dlopen`，现为拒绝）、`src/permission.rs`（`addon` 权限）；可执行文件的链接参数（导出符号）；可能需要改 `boa/`（外部 ArrayBuffer，记 `boa/SU.md`）

## 目标

让使用 **Node-API（N-API）** 的 `.node` 原生模块在 Verve 的 Electron 里加载并正常工作：先 Windows x64，再 macOS、Linux。
目标模块是 ABI 稳定的 N-API 模块（node-addon-api、napi-rs、纯 C N-API 写的），它们的预编译二进制无需针对 Verve 重新编译。

**明确不支持**直接使用 V8/NAN C++ API 的模块（如 `better-sqlite3`、旧版 `node-pty`、`robotjs`）：它们链接 V8 的 C++ 符号，
在没有 V8 的运行时里不可能工作。对这些模块的替代路线（例如用 Rust 实现真正的 `node:sqlite`）另立包，不在本包内。

## 背景（先读）

- `src/modules.rs` 约 300–320 行：`.node` 文件现在被拒绝，抛 `ERR_DLOPEN_DISABLED`（commit c24a9cb1 "refuse a required native addon by name"）。
- `src/process.rs` 约 243 行：`process.dlopen` 抛 `ERR_DLOPEN_FAILED`；约 435–438 行：`process.versions` 已报 `napi: '10'`、`modules: '136'`。
  **核实** Electron 39 的 `NODE_MODULE_VERSION` 并让 `modules` 与之一致（prebuild-install/node-gyp-build 用它选二进制）。
- `src/permission.rs` 约 495 行："No processes, native addons or WASI here, ever."——`process.permission.has('addon')` 恒为 false。
  本包把它改为由 `Permissions` 授权（Electron 主进程默认授权，`verve run` 默认不授权，`--allow-addons` 开启）。
- `src/isolate.rs`：每个 isolate 一个线程、一个 Boa 堆；`Runtime::set_waker`（`src/lib.rs`）：从别的线程唤醒宿主——
  异步工作与线程安全函数的回调靠它回到 JS 线程。
- `src/cli.rs`：进程何时退出（ref 计数）——线程安全函数和异步工作的 `ref`/`unref` 要接入同一套活跃度计数。
- Boa：`JsValue`/`JsObject` 是带根计数的 GC 指针，GC 是**非移动**的标记清除；`JsObject::downgrade`（verve-dom 已用的 Boa 改动，见 `boa/SU.md`）提供弱引用。
- Node 的规范来源：Node 22 文档的 Node-API 章节、`src/js_native_api.h`/`node_api.h`/`js_native_api_types.h`，以及 Node 源码树
  `test/js-native-api/` 与 `test/node-api/` 里的 C 测试插件与其 JS 测试——本包的一致性测试套件。

## 可行性：符号如何解析到 Verve

| 平台 | `.node` 如何找到 `napi_*` | Verve 需要做的 |
|---|---|---|
| Windows | node-gyp 构建的插件从 `node.exe` 导入（通过 `node.lib`），并链接 `win_delay_load_hook`：延迟加载 `node.exe`/`iojs.exe` 时改为返回 `GetModuleHandle(NULL)`（宿主进程映像）。napi-rs 的 Windows 构建在运行时从宿主映像按名查找符号。 | `electron.exe` **导出**全部 `napi_*`/`node_api_*` 符号（`#[unsafe(no_mangle)] pub extern "C"` + 链接器 `/DEF:napi.def` 或 `/EXPORT`），使对宿主映像的 `GetProcAddress` 能找到。实测：插件是否还硬编码导入 `electron.exe` 的名字（Electron 的 rebuild 产物），若是则 hook 同样解析到宿主映像，名称无关。 |
| Linux | 插件里 `napi_*` 是未定义符号，由动态链接器在全局符号表中解析 | 可执行文件以 `-Wl,--export-dynamic-symbol=napi_*`（或 `--dynamic-list`）导出；`dlopen(RTLD_LAZY)`（Node 的默认标志，`process.dlopen` 的 `flags` 参数照传） |
| macOS | 插件以 `-undefined dynamic_lookup` 链接，从主可执行文件解析 | 主可执行文件导出这些符号（`-Wl,-exported_symbols_list`）；`dlopen` |

模块注册两种方式都要支持：插件导出 `napi_register_module_v1(env, exports)`（及可选的 `node_api_module_get_api_version_v1()`），
或在静态构造函数里调用 `napi_module_register(napi_module*)`（旧式）——后者发生在 `dlopen` 期间，需要在调用 `dlopen` 前设置
"当前正在加载的模块"线程局部变量来接住它。

## 接口契约

### 1. `crates/verve-napi`

```rust
/// 在 runtime 上启用 .node 加载：注册 process.dlopen 与 require 的 .node 扩展处理。
pub fn install(runtime: &mut verve::Runtime) -> Result<(), String>;
```

- verve 根 crate 只暴露加载钩子（`modules.rs` 在遇到 `.node` 时调用已注册的 `AddonLoader`，未注册则保持现有的拒绝行为与错误码），
  不依赖 `verve-napi`；`verve-electron` 依赖 `verve-napi` 并在主进程（以及 `nodeIntegration: true` 且未 sandbox 的渲染器）里 install。
- 每个 (插件, isolate) 一个 `napi_env`。同一个 `.node` 文件在进程里只 `dlopen` 一次，在每个 isolate 里各调用一次注册函数
  （N-API 插件天然是 context-aware 的，与 Electron 的要求一致）。

### 2. 值与生命周期的表示

- `napi_value`：当前句柄作用域栈里的槽位下标（+1，0 留作 NULL），槽位持有 `JsValue`（因此被 GC 根住）。作用域关闭时截断栈；
  `napi_open_escapable_handle_scope`/`napi_escape_handle` 把值复制到父作用域。每次从 JS 调进原生回调时自动开一个作用域。
- `napi_ref`：引用表中的槽位；计数 > 0 时持强 `JsValue`，= 0 时持弱引用（`JsObject::downgrade`），对原始值按 Node 的规则处理（v10 允许对所有值建引用）。
- `napi_wrap`/`napi_unwrap`/`napi_remove_wrap`/`napi_type_tag_object`：对象上一个 JS 不可见的内部槽（用 Boa 的原生数据或隐藏符号属性，选不可被脚本枚举/篡改的方式）。
- 终结器：Boa 回收对象时不能运行 JS。终结器回调**排队**到 GC 之后、回到事件循环时执行（对应 `node_api_post_finalizer` 的语义）；
  纯原生的终结器（不调用 JS 的）也统一排队，简化正确性。需要一个"对象被回收"的通知：弱引用表 + 每轮事件循环检查已失效的弱引用（与 `Document::sweep` 同一思路），或在 Boa 里加终结钩子（若改 Boa，记 `SU.md`）。
- `napi_get_buffer_info`/`napi_get_arraybuffer_info`/`napi_get_typedarray_info` 返回数据指针：Boa 的 ArrayBuffer 数据在不 resize/detach 时地址稳定——
  在文档里写明假设，并对可调整大小的 ArrayBuffer 返回错误或固定其大小。
- `napi_create_external_arraybuffer`/`napi_create_external_buffer`：原生内存作为 ArrayBuffer 的后备存储。Boa 目前只支持自有 `Vec<u8>`，
  **需要 Boa 支持外部后备存储**（带释放回调）；在此之前用复制实现并标记为 `Partial`（写入不可见，会破坏某些模块）——这是本包最大的引擎改动风险点。
- 异常：`napi_throw*` 设置环境的待抛异常；原生回调返回时若有待抛异常则抛出；`napi_get_last_error_info` 按线程局部记录。
- 每个 `extern "C"` 入口都用 `catch_unwind` 包住，panic 转为 `napi_generic_failure`，绝不跨 FFI 展开。
- 线程检查：除线程安全函数相关 API 外，所有 API 校验调用线程是 env 所属 isolate 线程，否则返回错误（Node 在这种情况下是未定义行为，我们选择安全失败）。

### 3. 异步

- `napi_create_async_work`/`queue`/`cancel`：`execute` 在一个固定大小的工作线程池（默认 4，与 DNS 池同理不按任务开线程）上运行，
  `complete` 经 `Runtime::set_waker` 唤醒后在 isolate 线程运行。排队中的工作计入进程活跃度。
- 线程安全函数（`napi_create_threadsafe_function` 等）：有界/无界队列、`napi_tsfn_blocking`/`nonblocking`、`acquire`/`release`/`abort`、
  `ref`/`unref` 接入活跃度；isolate 结束时按 Node 语义调用 finalize。
- `napi_get_uv_event_loop`：返回 `napi_generic_failure`（不提供 libuv）。依赖 libuv 的插件不支持，登记到 P41。
- 异步上下文（`napi_async_init`/`napi_make_callback`）：映射到 Verve 的 `async_hooks`/`AsyncLocalStorage` 实现能表达的程度。

### 4. 安全

- 原生代码绕过整个权限模型：只有 `Permissions` 授予 `addon` 时才能加载；`process.permission.has('addon')` 如实回答。
- 插件崩溃会结束整个进程（Verve 是单进程，没有 Electron 的进程隔离）：在 `docs/electron/` 写明，并在主进程加载插件时于
  `VERVE_ELECTRON_LOG=load` 下记录。
- 渲染器 `sandbox: true`（Electron 默认）时不安装加载器，与 Electron 一致。

## Node-API 函数分组与实现优先级

| 优先级 | 分组 | 内容 |
|---|---|---|
| P0 | 加载与基础 | 模块注册两种方式、`napi_get_undefined/null/global/boolean`、数字（int32/uint32/int64/double）、字符串（utf8/latin1/utf16 的创建与读取）、`napi_typeof`、强制转换（`coerce_to_*`）、对象属性（`get/set/has/delete_property`、`*_named_property`、`*_element`、`get_property_names`、`get_all_property_names`、`define_properties`）、函数（`create_function`、`call_function`、`get_cb_info`、`get_new_target`、`new_instance`）、错误（`throw*`、`create_*error`、`is_error`、`is_exception_pending`、`get_and_clear_last_exception`、`get_last_error_info`）、句柄作用域、`strict_equals`、`instanceof`、`get_version`、`get_node_version` |
| P1 | 对象模型与数据 | 数组、`ArrayBuffer`/`TypedArray`/`DataView`/`Buffer`（创建、复制、`*_info`、`is_*`、`detach_arraybuffer`）、`define_class`、`wrap`/`unwrap`/`remove_wrap`、引用、`create_external`/`get_value_external`、终结器（`add_finalizer`）、实例数据（`set/get_instance_data`）、类型标记、Promise（`create_promise`、`resolve/reject_deferred`、`is_promise`）、`Date`、`Symbol`、`BigInt`（含 words）、`object_freeze`/`seal`、`run_script`、`get_prototype` |
| P2 | 异步与环境 | async work、threadsafe function、`napi_async_init/destroy`、`make_callback`、`open/close_callback_scope`、清理钩子（`add/remove_env_cleanup_hook`、`add_async_cleanup_hook`）、`adjust_external_memory`、`fatal_error`/`fatal_exception`、`get_uv_event_loop`（失败） |
| P3 | 新增 API（Node-API 10 及 `node_api_*`） | `node_api_symbol_for`、`node_api_create_property_key_*`、`node_api_post_finalizer`、`node_api_create_external_string_*`（可复制实现）、`node_api_create_syntax_error`/`throw_syntax_error`、外部 ArrayBuffer 的真实实现（依赖 Boa 改动） |

## 优先支持的模块与测试

1. **一致性套件**：把 Node 22（与 Electron 39 对应的版本）源码树 `test/js-native-api/*` 与 `test/node-api/*` 的 C 插件用 `cc` crate 在测试时编译
   （需要 MSVC 或 clang，测试标 `#[ignore]` 并说明工具链要求），用 Verve 跑它们的 JS 测试（这些测试用 `assert`、`common` 辅助模块，
   提供一个最小 `common` 替身作为测试夹具）。逐个登记通过/跳过。
2. **真实模块**（预编译二进制，离线准备，照 P41 的 prepare 约定，`VERVE_NAPI_PACKAGES` 指向暂存目录）：
   - napi-rs 生态的小模块：`@node-rs/crc32`、`@node-rs/argon2`（P0/P1/P2 全覆盖的冒烟）；
   - `bufferutil`、`utf-8-validate`（`ws` 的可选加速，常见于 Electron 应用）；
   - `keytar`（node-addon-api，Electron 应用常用，已归档但仍广泛存在）；
   - `sqlite3`（node-addon-api，大量 async work）；
   - `@parcel/watcher`（线程安全函数）；
   - `sharp`（体量大，作为最后的压力测试）；
   - `node-pty`：实施时核实其当前版本是否已迁到 N-API，是则加入，否则登记为不支持。

## 分阶段

| 阶段 | 内容 | 出口标准 |
|---|---|---|
| A | Windows x64：导出符号、加载与两种注册、P0 分组 | 一致性套件中 P0 相关测试通过；`@node-rs/crc32` 可用 |
| B | P1 分组；终结器排队；`addon` 权限 | `bufferutil`、`keytar` 可用；P0+P1 一致性测试通过 |
| C | P2 分组：工作线程池、线程安全函数、活跃度 | `sqlite3`、`@parcel/watcher` 可用；进程在有未释放 TSFN 时不提前退出、释放后正常退出 |
| D | macOS、Linux 的符号导出与加载 | 三平台跑通阶段 A–C 的真实模块测试 |
| E | P3 分组；Boa 外部后备存储 | 外部 buffer 写入双向可见；`sharp` 可用 |

## 风险

- **外部 ArrayBuffer** 需要改 Boa 的 ArrayBuffer 存储，影响面大（类型化数组、结构化克隆、transfer）。
- **GC 交互**：终结器、弱引用与 Boa 回收时机；必须保证回收期间绝不进入 JS，且排队的终结器不会访问已释放的 env。
- **符号导出与 LTO**：导出的符号不能被剥离；对体积有小幅影响（P01 测量）。
- **未定义行为的插件**：Node 上"碰巧能跑"的插件（跨线程调用非线程安全 API 等）在我们的安全检查下会报错——记录而非放宽。
- **进程隔离缺失**：插件崩溃即整个应用崩溃。
- **模块版本号**：`process.versions.modules` 不对会让 prebuild 选错二进制（对 N-API 二进制通常按 `napi` 版本选，影响较小，但要验证）。

## 验收

见各阶段出口标准；另外：未授权 `addon` 时 `require('x.node')` 仍抛 `ERR_DLOPEN_DISABLED`（现有错误码不变）；
`cargo test -p verve-napi` 在没有 C 工具链的机器上也通过（需要工具链的测试为 `#[ignore]`）。

## 不做

V8/NAN C++ API 模块；libuv API；WASI；为单个模块写专门的兼容补丁。

## 状态

（实现者完成后填写）
