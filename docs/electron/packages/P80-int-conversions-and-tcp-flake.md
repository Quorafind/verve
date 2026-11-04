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

未开始。
