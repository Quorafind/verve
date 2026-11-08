# P80 两处小修：整数转换的饱和、一个时好时坏的 TCP 测试

**依赖**：P76（已合并）  **规模**：S  **修改**：`boa/core/engine/src/value/`（`to_int8`/`to_uint8`/`to_int16`/`to_uint16` 及同类转换）、
`src/` 里 TCP 套接字选项与超时相关的代码或 `tests/` 里对应的测试（只限查明的原因）、对应测试、`boa/SU.md`

## 目标

### 1. 小整数转换在大浮点数上饱和

P76 发现：`to_int8`、`to_uint8`、`to_int16`、`to_uint16` 先 `as i64`（≥ 2^63 时饱和到 `i64::MAX`）再取余，
所以 `new Int8Array(1)[0] = 3.4e38` 存的是 `-1`，规范（`ToInt8` 等：先 `truncate`，再对 2^8 取模）与 Node 都是 `0`。
解释器、辅助函数与编译代码（把 ≥ 2^63 的浮点交给辅助）三者目前一致地错。本包按规范修，三者一起变对。
顺带检查 `to_int32`/`to_uint32`/`to_big_int64` 等同族转换有没有同样的饱和问题，有则一并修。

### 2. `tcp_socket_options_and_inactivity_timeout_are_chainable` 在整套并发跑时出 `EIO`

P66、P72、P76 都记过：`cargo test -p verve` 整套并发时这个测试偶尔以 `EIO`（Windows `os error 10022`，`WSAEINVAL`）失败，单独跑总能通过。
查明原因（例如对尚未连接或已关闭的套接字设置选项、端口或时序的竞争），按原因修代码或修测试；**不得**只靠重试或加长睡眠掩盖，
若最后确实只是测试本身的时序假设，写明证据再改测试。

## 要做的事

1. 第 1 项：先写在修复前失败的测试（每种元素类型、`±2^63`、`±2^64`、`1e300`、`-0`、`NaN`、`Infinity`、`2^53+1` 附近），与 Node 输出逐字一致；
   修复后解释、`BOA_JIT_THRESHOLD=0` 两种一致。不得让常见值（`int32` 范围内）的路径变慢：给出 `typed` 前后对比。
2. 第 2 项：读测试与 `src/` 里设置 `setNoDelay`/`setKeepAlive`/`setTimeout` 的代码，找出能在并发下出 `WSAEINVAL` 的调用；
   能稳定复现最好（例如并发跑同一测试多份、或在本机制造负载），写进「状态」。修复后 `cargo test -p verve --features jit` 整套连续跑 10 次不出现。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、clippy `-D warnings`（有无 `jit`）、`cargo fmt`；
  根 `cargo test -p verve`（有无 `jit`）、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `built-ins/TypedArray`、`built-ins/TypedArrayConstructors`、`built-ins/DataView`、`built-ins/Atomics`（除计时类）前后逐项 0 回退。

## 不做

其他数值转换的性能工作；网络模块的其他行为。

## 状态

已完成，两个提交（`8206b56a`、`3569e2db`）。

**第 1 项（`8206b56a`）。** `to_int8`、`to_uint8`、`to_int16`、`to_uint16` 改为取 `ToInt32` 的低位（`f64_to_int32` 对任何有限浮点都精确，2^N 整除 2^32），不再经 `as i64`；
int32 范围内的路径没有多任何一步（反而少了 `is_nan`/`is_zero`/`is_infinite` 三次判断）。同族检查：`to_int32`/`to_uint32` 本来就走 `f64_to_int32`，无问题；`to_big_int64`/`to_big_uint64` 用 `BigInt` 的 `mod_floor`，无饱和；`to_uint8_clamp` 不取模。
测试 `tests_typed_elements/huge.js`（期望输出 `huge.txt` 取自 Node 22，逐字节）覆盖 `Int8`、`Uint8`、`Uint8Clamped`、`Int16`、`Uint16`、`Int32`、`Uint32`、`Float32`、`Float64` 数组，`DataView` 的六个整数 setter，`String.fromCharCode`，`Atomics.store/add`，
值取 `±2^63`、`±2^64`、`3.4e38`、`1e300`、`-0`、`NaN`、`±Infinity`、`2^53` 前后等；解释、默认阈值与阈值 0 的强制编译三种模式都与 `huge.txt` 比较。修复前 `Int8Array(1)[0] = 3.4e38` 得 `-1`（Node 得 `0`）而失败。
`docs/jit.md` 的相关句子随之更新，`boa/SU.md` 有条目。
`typed`（`scripts/js-bench/run.sh --runs 9 --only typed`，release + `jit`，基线 `2cb3f702` 与本分支逐轮交替，CPU 负载约 15–25 %）：
JIT 中位数（基线对本分支）224.4/224.1、274.2/231.7、221.3/235.9 ms；解释 1260.5/1301.2、1354.2/1323.4、1302.2/1341.2 ms；Node 4.1–5.1 ms。互有高低，在噪声内，无可测变化。

**第 2 项（`3569e2db`）。** 原因：套接字还在连接中时，Windows 对它设 `TCP_NODELAY`/keep-alive 返回 `WSAEINVAL`（10022），反应器把它报成 `EIO` 的 `error` 事件。
`setNoDelay`/`setKeepAlive` 在 `connect()` 后立刻发命令，连接慢（整套并发、机器负载高）时命令先于连接完成到达反应器，所以只在并发下偶发。
可稳定复现：新测试 `tcp_options_set_while_connecting_do_not_fail`（同时发起 300 个连接并立刻设选项）在还原 `reactor.rs` 到 `2cb3f702` 后三次运行三次失败，每次约 96 个 `EIO`；修复后通过。
修法：反应器对仍在连接的套接字跳过这两个选项；JS 侧在 `Event::Connected` 到达时会重发，不丢设置。没有重试或加睡眠。
`cargo test -p verve --features jit --no-fail-fast` 连续 10 次：全部退出码 0，`EIO`/`10022` 出现 0 次（HEAD `3569e2db`）。

**验证**（`CARGO_BUILD_JOBS=6`）：`boa_engine` 默认 1245、`--features jit` 1307、`BOA_JIT_THRESHOLD=0 --features jit` 1307 个测试全过；`boa` 里 `clippy -p boa_engine --all-targets [--features jit] -D warnings` 与 `cargo fmt --check` 过；
根 `cargo clippy --workspace --all-targets -D warnings`、`cargo fmt --all --check`、`style-check` 过；`cargo test -p verve` 有无 `jit` 全过。
test262 `built-ins/TypedArray`（1446 个，通过 1438）、`TypedArrayConstructors`（738，736）、`DataView`（561，550）、`Atomics`（389，380）（`-vv`，基线 `2cb3f702` 对本分支）：0 失败，逐项输出 0 差异。
注意：测试器没有 `jit` 特性，这是解释器的比较；编译码的覆盖来自 `huge.js`（阈值 0）。
