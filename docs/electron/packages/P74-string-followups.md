# P74 字符串的三个后续：数字转字符串、rope 内存、字符串迭代

**依赖**：P70、P71（均已合并）  **规模**：S–M  **修改**：`boa/core/string/`、`boa/core/engine/src/value/`（数值到字符串）、
`builtins/number/`（只限转字符串）、`builtins/string/string_iterator.rs`、对应测试、`boa/SU.md`

## 目标

P71 让 `+=` 变成线性，但 P60 套件的 `concat` 仍是 Node 的 8.1×（目标 5×）；P70 让字符串的 `for…of` 从约 500 ns 降到 73 ns，
剩下的在 `String::substring`。三件小事，各自独立 commit，都先测再改：

1. **数字转字符串**（`concat` 的大头）：P71 测得同一进程里 `x = 'ab' + i` 单独就占 5.7 ms 中的 2.3 ms；`'ab' + i` 先把 `i`（`int32`）
   经 `JsValue::to_string` → `JsString::from(i32)` 转成字符串，再拼接。
   - 先拆账：`i32` → `JsString` 一次多少纳秒、分配占多少；`concat_array` 每次调用里查静态字符串表（P71 报告提到）占多少。
   - 候选：`i32` 转换走栈上缓冲一次分配（不经 `String`/`format!`）；小整数（例如 0–255）用静态表；`concat_array` 在结果不可能命中静态表时
     （长度超过表里最长的串）跳过查表。只做测出有收益的。
   - **目标**：`concat` ≤ Node 的 5×；`'ab' + i` 循环的微探针前后对比。
2. **rope 的叶子合并**（P71 方案第 4 阶段）：32 万次追加的内存峰值约 66 MB（P71 之前约 20 MB），因为每次追加都留下一个 88 字节节点直到第一次读取。
   - 右侧是短串（例如 < 64 码元）且左侧是**未被共享的** rope（引用计数为 1）时，把短串并入右侧叶子（可以是一个预留容量的可增长叶子），不新建节点。
   - **目标**：32 万次追加的内存峰值 ≤ 30 MB；`concat` 时间不变慢；P71 的全部 rope 测试照旧通过，另加「共享的 rope 不被原地修改」的测试
     （`a = s + 'x'; b = s + 'y'` 之后 `a`、`b`、`s` 各自正确）。
   - 若测量表明合并让 `concat` 变慢或改动面过大，停下来在「状态」写明数字，不做。
3. **字符串迭代**：`StringIterator::advance` 每步经 `String::substring` 新建一个字符串。
   - 单个码元（非代理项对）的字符用静态表或已有的单字符字符串缓存（Boa 若已有就复用，没有就加，限 Latin-1 范围）；代理项对照旧建两码元的串。
   - **目标**：字符串 `for…of` 每元素（编译）≤ 35 ns（P70 后 72.8）。

## 背景（先读）

- `P71-rope-strings.md`「状态」（「Why `concat` misses 5x」、偏离方案的说明）；[`spikes/p64-rope.md`](../spikes/p64-rope.md) 第 4 阶段与「风险」。
- `P70-iterators.md`「状态」；`boa/core/string/src/vtable/rope.rs`、`lib.rs` 的 `concat_array`、`concat_lazy`；
  `boa/core/engine/src/value/mod.rs` 的 `to_string`；`builtins/string/string_iterator.rs`。

## 验收

- 每项的前后数字（同一轮、5 个进程、中位与最好，记负载）；`perf-js.md` 追加一行；体积增量写进 `size.md`。
- `cargo test -p boa_string`（rope 的深链、共享、孤立代理项测试）；`boa/` 内 `cargo test -p boa_engine`（默认与 `BOA_JIT_THRESHOLD=0 --features jit`）、
  clippy `-D warnings`；根 `cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262：`built-ins/String`、`built-ins/Number`（`toString`）、`language/expressions/addition`、`language/expressions/template-literal`、
  `built-ins/StringIteratorPrototype` 前后逐项 0 回退。
- P60 套件字符串类各项不变慢。

## 不做

浮点数转字符串（`ryu_js`）本身；`Array.prototype.join`；展平后再追加的 O(n²)。

## 状态

分支 `electron/p74-string-followups`，三个代码 commit（字符串迭代、`int32` 拼接、rope 缓冲区；每个都带 `boa/SU.md` 条目与测试）加本节与 `perf-js.md`/`size.md`。
基线是 `docs/electron-plan` `c91b2938`，对比用另建的 detached worktree 里按同配置（`--release --features jit`）构建的二进制。
**三项都做了，三个目标都达到**；与任务书的差别见每项的「偏差」。

**1. 数字转字符串（`'ab' + i`）**

- 拆账（独立小 crate 里 `boa_string` 的 release 微基准，`opt-level 3`、fat LTO，7 轮取中位）：`itoa` 格式化到栈上 2.4 ns；`JsString::from(i32)` 含释放 28.0 ns
  （一次 `Box` 分配加释放本身 22.8 ns，所以分配占约八成）；`concat('ab', '12345')` 36.8 ns，其中静态串表查找（未命中）4.5 ns；
  二者连着做 67 ns。同一个循环在 JS 里每次 117 ns（空循环 2.4 ns），所以 Rust 侧约 67 ns，其余约 50 ns 是 `Add` 的分派、对两个操作数各做一次 `ToPrimitive`、
  上一个 `x` 的释放与赋值（推断，没有逐项隔离）。
- 候选里：「`i32` 走栈上缓冲一次分配」**本来就是现状**（`itoa::Buffer` 加 `from_slice_skip_interning`）；「小整数静态表」**没做**：融合之后中间串整个不存在，表只会对 ≤ 255 有用；
  「结果过长时跳过静态表查找」**没做**：`concat_array` 对长结果本来就跳过，对 `'ab123'` 这种短结果查找只占 4.5 ns（约 1.5 %），无法在不漏掉真命中的前提下跳过。
- 做的：`JsValue::add` 对 `String + int32`、`int32 + String` 走专门分支，不再对两边 `ToPrimitive`；`JsString::concat_i32`/`concat_i32_before` 把数字的十进制写到栈缓冲，
  结果不足 `ROPE_MIN_LEN` 时直接写进结果串（一次分配，没有中间 `JsString`，也没有静态表查找之外的多余工作）；更长时退回 `concat_lazy`。
- 结果（JIT，5 个进程中位）：`x = 'ab' + i` 每次 117 → 72 ns（−38 %）；`abi_big`（6 位数）118 → 74；`String(i)` 不在改动路径上，94 → 98（噪声）。套件 `concat`：5.6 → 2.9 ms 与第 2 项合计，
  只做本项时（`concat` 探针 `s += 'ab' + i` 每次）273 → 236 ns。
- 偏差：无。

**2. rope 的叶子合并**

- 拆账：320k 次 `s += 'ab' + i` 的峰值工作集 66.5 MB（复现 P71 的 66）；`s += 'ab'` 41.9 MB；空进程 16.2 MB。所以每次追加净增约 160 字节（`'ab' + i` 时：88 字节节点加 `'ab' + i` 那个小串）。
  Rust 侧一次追加（链 20,000 节点）建 43 ns、连同最后的整链释放 72 ns，其中释放 30 ns 是沿 20,000 个节点走一遍的缓存缺失。
- **与任务书的偏差（重要）**：任务书写的「左侧 rope 引用计数为 1 时原地并入」**行不通**：`s += x` 执行 `Add` 时 rope 至少有两个引用（保存 `s` 的变量和正被相加的操作数），
  引用计数在这里区分不出「之后没人再看这个 rope」。换成不依赖独占的方案：rope 的最后一段可以是一个**共享的 Latin-1 字节缓冲区**（`RopeState::Appended { left, buffer: Rc<RefCell<Vec<u8>>> }`），
  每个 rope 只读缓冲区的前 `len - left.len()` 个字节；只有当某个 rope 的长度恰好等于缓冲区**当前的末尾**时，才把新的短串追加到缓冲区末尾并建一个同样引用该缓冲区的新节点——
  追加写的是所有现存 rope 都看不到的位置，所以不会改变任何已有字符串的内容。`a = s + 'x'; b = s + 'y'`：`a` 追加在原位，`b` 发现缓冲区已被推过 `s` 的末尾，建普通节点。
  没有新的 `unsafe`（`Rc<RefCell<Vec<u8>>>`）。触发条件：`y` ≤ 64 个码元且 Latin-1、`x` 是未展平的全 Latin-1 的 rope；`x` 的右半是短串时（第二次追加）把右半和 `y` 拷进新缓冲区，
  `x` 已经以缓冲区结尾时原位追加。更宽的串、更长的 `y`、`x` 已展平或不是 rope，走原来的节点。
  节点从 88 字节变成 **96**（第三种状态需要一个标签；`node_size` 测试与 `size.md` 同步改）。
- 结果：320k 次 `s += 'ab' + i` 峰值 **66.5 → 16.3 MB**（空进程 16.2，目标 ≤ 30 MB 达到）；`s += 'ab'` 41.9 → 16.1 MB；循环时间不变慢，反而更快：
  `concat` 探针 236 → 147 ns，`s += f` 117 → 79，320k 次追加 132 → 87 ns/次（比较的是只含第 1 项的二进制与最终二进制，同一轮），
  快的原因是不再沿几万个节点的链释放，也少了每次一个节点的堆占用（推断）。
- 没覆盖：UTF-16 的追加（`x` 或 `y` 非 Latin-1）仍是一个节点一次追加，且节点大了 8 字节：320k 次 `s += '中' + i` 71.2 → 76.3 MB、62.6 → 68.1 ms（本包没有优化的情形，约慢 9 %）。
  展平后再追加仍是 O(n²)（任务书「不做」）。

**3. 字符串迭代**

- 拆账：`for (const c of text)` 每元素 74 ns（编译）/ 109 ns（解释），其中一步是 `String.prototype.substring`：把 `this`、两个整数参数各转一遍 `ToIntegerOrInfinity`、
  `clone` 字符串成值、`slice_unchecked` 新建一个 `SliceString`（一次分配）。
- 做的：`StaticJsStrings::latin1_char(byte)`（256 项的 `StaticString` 常量表，无分配）；`StringIterator::advance` 对 U+0000..U+00FF 的码点直接返回它，其余码点
  （含代理项对和孤立代理项）直接 `slice` 取，不再经 `substring` 内建；`advance` 因此不再用 `context`（形参保留，四种迭代器的 `advance` 同签名）。
- 结果（400k 个元素，5 个进程中位）：Latin-1 文本 **74.0 → 21.3 ns**（目标 ≤ 35 达到），含 `€`/汉字的文本 71.9 → 27.6；解释 109 → 38 与 93 → 49。
- 偏差：「Boa 若已有就复用」：Boa 没有单字符串表，加了；表在 `boa_string` 的 `common.rs`。体积 +18 KB（256 × `StaticString` 72 字节）。

**验收对照**

| 项 | 结果 |
|---|---|
| `concat` ≤ Node 的 5× | JIT **4.8×**（2.9 ms 对 0.6 ms；此前 5.6 ms，9.3×）；解释 5.2×（3.1 ms，此前 9.3×）。这一轮 Node 是 0.6 ms，P71 记的 8.1× 是按 0.7 ms |
| 32 万次追加峰值 ≤ 30 MB | 16.3 MB（此前 66.5） |
| 字符串 `for…of` 每元素 ≤ 35 ns | 21.3 ns（Latin-1），27.6 ns（含宽字符） |
| P60 字符串类不变慢 | 见下表 |
| 体积 | 未剥离的 release `verve.exe` 26,672,128 → 26,700,800 字节，+28,672（+0.11 %）：第 3 项 +22,528，第 1 项 +3,584，第 2 项 +2,560 |

P60 套件字符串类（`run.sh --runs 5`，同一轮、先后相邻，中位（最好）ms；负载 13–17 % 开始，跑完时被别的 worker 推到 55–68 %，所以只比较同一对）：

| 测试 | Node | JIT 前 | JIT 后 | 解释 前 | 解释 后 |
|---|---|---|---|---|---|
| `concat` | 0.6 (0.6) | 5.6 (5.4) | 2.9 (2.8) | 5.6 (5.5) | 3.1 (3.0) |
| `splitjoin` | 2.8 (2.6) | 11.8 (11.7) | 11.1 (10.5) | 12.4 (12.3) | 11.6 (11.2) |
| `regex` | 1.8 (1.7) | 24.9 (23.5) | 23.1 (22.9) | 25.2 (23.5) | 24.0 (23.6) |
| `json` | 166.1 (164.2) | 1265.0 (1229.3) | 1271.1 (1221.1) | 1257.0 (1224.9) | 1248.1 (1246.6) |
| `tokenize` | 2.4 (2.4) | 164.6 (159.1) | 166.6 (156.8) | 176.0 (171.5) | 175.3 (173.8) |
| `template` | 0.2 (0.2) | 1.8 (1.7) | 1.9 (1.8) | 1.0 (0.9) | 1.2 (0.9) |

`template` 在这一轮读数有抖动，用 15 个进程再跑一遍：JIT 1.7 (1.7) 对 1.7 (1.6)，解释 0.9 (0.9) 对 0.9 (0.9)，一样。`scaling.js` 的 44 个探针前后各 3 个进程：`concat` 20k/80k 7.6 (5.9) → 3.0 (2.9)、
27.9 → 12.2 ms，仍是线性；其余在噪声内（基线那一半是在 51–59 % 负载下跑的）；超过 10 % 的三个（`json-parse`、`map-iterate`、`split-multi-unicode`）各用 7 个进程相邻重测两次，
读数一样（负载 26–29 %）。

**验证（都实际跑过）**

- `boa/` 内 `cargo test -p boa_string`：49 个全过（原 44，新增：`concat_i32` 一个，rope 四个——共享缓冲区、共享的 rope 不被原地修改、展平后再追加与跨宽度、长段深链；
  `node_size` 改成 96）。miri（`CARGO_LOWPRIO_OFF=1 cargo +nightly miri test -p boa_string --lib --target x86_64-unknown-linux-gnu`）：49 个全过，74.8 s。
- `boa/` 内 `cargo test -p boa_engine`：1214 个全过；`BOA_JIT_THRESHOLD=0 cargo test -p boa_engine --features jit`：1265 个全过。`tests/rope.rs` 新增两个 `Context` 级测试
  （共享前缀的 rope 各自正确；数字与字符串两个方向的拼接，含 `int32` 边界、`-0`、`NaN`、`1e21`、255/256 码元的分界、宽字符），`tests/iterators.rs` 一个（每个 Latin-1 字符、`€`、代理项对与孤立代理项、NUL）。
- test262（`E:\test262`，pinned commit，release `boa_tester --features boa_engine/jit`，前后各跑一遍 `-vv`，输出排序后 `diff`；默认与 `BOA_JIT_THRESHOLD=0` 各一遍）：
  `built-ins/String`（2439 通过 / 2 忽略）、`built-ins/Number`（678 通过 / 1 失败，前后一致）、`language/expressions/addition`（95）、`language/expressions/template-literal`（114）、
  `built-ins/StringIteratorPrototype`（14）、`language/statements/for-of`（1424 通过 / 1 失败 / 10 忽略，前后一致）：十二次比较差异都是 **0 行**。
- 每个 commit 前：`boa/` 内 `cargo fmt --all --check`、`cargo clippy -p boa_string -p boa_engine --all-targets --features jit -- -D warnings`，根 `cargo fmt --all --check`、`bash scripts/style-check.sh`（提交钩子）。
- **没跑**：根 `cargo test -p verve`（构建纪律：总控统一跑；本包没改 `src/`）、`bash scripts/linux-check.sh`（没有平台分支）、`BOA_JIT_FUZZ_COUNT` 的 JIT 模糊测试（没改 JIT）。

**给总控的备注**

- 比较用的基线 worktree `verve-e-p74-base`（detached）和它的构建目录 `E:\cargo-target\verve-e-p74-base`、`verve-e-p74-t262-base`、`verve-e-p74-t262-after`、`verve-e-p74-mb` 合并后可以删掉；
  `boa/target` 是 `boa/` 里直接跑 cargo 产生的（已被 `.gitignore`）。
- 缓冲区方案对 `Rc<RefCell<..>>` 的 `borrow_mut` 假设单线程且 `flatten` 不会在追加期间持有 `borrow`；`boa_string` 的字符串本来就是 `!Sync`。
