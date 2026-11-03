# P67 `verve run` 默认开代码缓存

**依赖**：P62  **规模**：M  **修改**：`src/code_cache.rs`、`src/code_cache_load.rs`（P62 新建）、`src/cli.rs`、`src/lib.rs`（只限缓存的公开 API 与 `CodeCacheStats`）、
`tests/code_cache.rs`、`docs/startup.md`；为降低首次运行成本而必须改 Boa 时，`boa/core/engine/src/code_cache.rs` 与 `boa/SU.md`

## 目标

代码缓存现在要加 `--code-cache=<dir>` 或 `VERVE_CODE_CACHE` 才开。暖缓存把 Rebon 的组合从 167 ms 降到 105 ms，
把 `require('typescript')` 从 1,089 ms 降到 169 ms（P62）——用户不知道这个开关就拿不到。本包让 `verve run` **默认开**，
并先解决默认开以后才暴露的三个问题：

1. **首次运行变慢**：`docs/startup.md` 实测冷缓存 212 ms，比不开缓存（167 ms）慢约 27%。只跑一次的脚本反而吃亏。
2. **旧缓存没人删**：每个构建一个 pack（上限 64 MB），"old ones are left for the embedder to delete"。默认开以后 CLI 就是这个调用方；
   开发时频繁重编译，几天就能堆出几 GB。
3. **多个进程同时写**：pack 是"临时文件改名"，写入前会合并磁盘上已有的条目，但两个进程的合并与改名之间有竞争窗口。
   原属 P42 第 1 节的并发 flush 修复与测试移到本包，P42 直接使用。

只改 CLI 的默认值：嵌入方的 `Runtime::new()` 仍然默认关（由嵌入方自己决定目录与密钥）；Electron 的接入仍归 P42。

## 背景（先读）

- `docs/startup.md`「Verve: the pack on disk」「Trust model」与测量表：pack 格式、键、HMAC 标签、密钥位置、目录权限、flush 时机、冷/暖数字。
- `src/code_cache.rs`：`flush`/`flush_now`、写入时的合并、`write_atomically`（约 484 行）；P62 之后 loader 入口在 `src/code_cache_load.rs`。
- `src/cli.rs` 约 48、92、280、490 行：`--code-cache` 与 `VERVE_CODE_CACHE` 的解析和使用。
- P62 的「状态」：CJS 进缓存后的冷/暖数字与 `CodeCacheStats` 的新字段。
- Chrome 的代码缓存策略（第一次加载不产生缓存，短时间内第二次加载才产生，第三次起使用）——第 2 节的候选方案之一。

## 接口契约

### 1. 默认目录与开关

- 默认目录：Windows `%LOCALAPPDATA%\Verve\code-cache\`；其他平台 `$XDG_CACHE_HOME/verve/code-cache/`，未设时 `~/.cache/verve/code-cache/`。
  与密钥目录（`%LOCALAPPDATA%\Verve\code-cache-secret`、`~/.config/verve/…`）分开，信任模型不变：密钥不在缓存目录里。
  目录的创建与权限检查照现有 `set_code_cache_dir_with_user_secret`。
- 优先级：`--no-code-cache` > `--code-cache=<dir>` > `VERVE_CODE_CACHE` > 默认目录。`VERVE_CODE_CACHE=0`（以及 `off`、`false`）表示关；
  空值视为未设置（现状）。`USAGE` 与 `cli.rs` 文件头的表格同步更新。
- **默认开绝不能让一次运行失败或多打输出**：默认目录建不了、不可写、权限检查不过、密钥读不了时，静默不用缓存继续运行
  （只在显式给了 `--code-cache`/`VERVE_CODE_CACHE` 时才像现在一样打印一行 `verve: code cache … not used`）。
  只读的家目录、CI 容器、`HOME` 未设置都要覆盖到。
- `-e`/`-p` 的源码本身不进缓存（与现状一致），它 `require`/`import` 的文件照常进。`worker_threads` 与 `child_process` 启动的 `verve` 子进程沿用同一规则。

### 2. 首次运行的成本

先**测清楚冷缓存多出的 45 ms 花在哪**（多编译了 lazy 本来不编译的函数？写盘与 HMAC？进程退出时同步 flush？），写进 `docs/startup.md`，再选方案。候选：

- **第二次才写**（Chrome 的做法）：第一次运行只记下"见过"（例如 pack 索引里只存键、不存代码，或单独的小文件），照 lazy 正常运行；
  第二次运行发现见过才编译全部函数并写入；第三次起读缓存。代价：要两次才变快。
- **把多出的编译与写盘挪出关键路径**：主脚本与事件循环结束后、进程退出前再编译剩余函数并写入；或只写已编译的部分，
  其余函数在以后的运行里首次调用时补写。要说明进程总时间（不只是脚本内时间）怎么变。
- 其他实现者认为更好的方案。

方案选定前写一段比较（各自的冷/二次/暖时间、实现量、对信任模型的影响），总控确认后再实现。

### 3. 旧缓存的淘汰

- 打开 pack 时更新它的修改时间（或在索引头里记最近使用时间），作为"最近使用"的依据。
- 每次运行在**空闲时**（首次 flush 之后，不在启动路径上）检查缓存目录：删除超过 **14 天**未使用的其他构建的 pack；
  目录总大小超过 **512 MB** 时按最近使用从旧到新删除其他构建的 pack，直到低于上限。当前构建的 pack 不删。
  多个已安装的 Verve 版本共用默认目录时，它们各自的 pack 都在用，不会互相清掉。
- 检查本身限频（例如目录里放一个时间戳文件，一天最多查一次），并且失败只是跳过。
- 淘汰函数做成可复用的（P42 用它淘汰 Electron 应用的旧 generation 目录）。

### 4. 并发写

- 临时文件名带进程号与随机后缀，互不覆盖；改名前重新读一次磁盘上的 pack 合并（现状已合并，确认竞争窗口内最多丢失另一个进程**一次**的新增条目，且永不损坏）。
- 测试：4 个进程同时加载不同模块并 flush，重复若干轮，最终 pack 可读、标签全部有效、每个进程的条目在下一轮后都在。
- 在 Windows 上目标文件被另一个进程打开时改名会失败：这时放弃本次写入（下次再写），不报错、不重试阻塞。

## 验收

- 测量（release `verve`，各 5 次取中位，照 `docs/startup.md` 的格式记机器负载）：

  | 负载 | 不开缓存 | 默认开，第一次 | 默认开，第二次 | 默认开，之后 |
  |---|---|---|---|---|
  | `console.log('hi')` 单文件 | | | | |
  | Rebon 的组合（`docs/startup.md` 同一套） | | | | |
  | `require('typescript')`（P62 同一套） | | | | |

  进程总时间：第一次 ≤ 不开缓存的 **105%**；单文件脚本任何一次都不慢于不开缓存 **2 ms** 以上；"之后"一列与现在的暖缓存数字相当。
  同时记每种负载 pack 的磁盘大小。
- 测试（`tests/code_cache.rs` 与 `src/cli.rs` 的测试，用临时目录覆盖 `LOCALAPPDATA`/`XDG_CACHE_HOME`/`HOME`）：
  不带参数时用默认目录并命中；`--no-code-cache`、`VERVE_CODE_CACHE=0` 时不读不写；默认目录不可写或 `HOME` 未设置时正常运行、stderr 无输出；
  按时间与按大小的淘汰（不删当前构建）；第 4 节的并发测试。
- `docs/startup.md` 的 Tests 一节列出的整套配置照旧全部通过；`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --check`、`scripts/style-check.sh` 通过。
- `docs/startup.md` 改写「Off by default」相关段落，记默认目录、开关、淘汰规则与上表。

## 不做

Electron 的缓存目录与 generation（P42）；嵌入 API 的默认值；经典 `<script>` 缓存；懒解析。

## 状态

（实现者完成后填写）
