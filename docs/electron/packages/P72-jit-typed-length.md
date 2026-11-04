# P72 JIT：类型化数组 `length` 的站点缓存

**依赖**：P65、P66（均已合并）  **规模**：S  **修改**：`boa/core/engine/src/builtins/typed_array/object.rs`（只拆分
`typed_array_length_shortcut`，单独一个移动 commit）、`vm/jit/`、`docs/jit.md`、`boa/SU.md`、对应测试

## 目标

P66 之后 `typed` 的 JIT 列是 608 ms，目标 450。剩下的大头是 `f64.length`：一轮 140 ns 里占 65 ns。它每次都走 P65 的
`typed_array_length_shortcut`，沿原型链确认没有被遮蔽或改写的 `length` 访问器，然后才读内部槽。原型链检查的结果在一个
编译站点上几乎从不变化，可以缓存；读内部槽（长度、是否分离、可变长缓冲的当前长度）每次都要做。

**目标**：`f64.length` 编译后 ≤ 15 ns；P60 套件 `typed` JIT 列 ≤ 450 ms。其余各项不变慢。

## 背景（先读）

- `P66-jit-coverage.md`「状态」第 2 步「Left out」中「Typed-array `length` site cache」一条，以及微探针数字（`docs/jit.md`「Results (phase 2.5)」）。
- `builtins/typed_array/object.rs` 的 `typed_array_length_shortcut`：先查对象自身没有 `length`，再沿原型链走普通对象，找到访问器并确认是内建 getter。
- `docs/jit.md`「Property caches and elements」：现有属性站点缓存怎样按形状校验、怎样失效。

## 要做的事

1. **移动 commit（不改行为）**：把 `typed_array_length_shortcut` 拆成两半：**守卫**（「这个对象的 `length` 就是内建 getter」）与**读取**（由内部槽算出长度，
   含分离与可变长缓冲）。解释器照旧两半连着调，P65 的测试原样通过。
2. **站点缓存**：`GetLengthProperty` 遇到类型化数组时，按 `docs/jit.md` 现有的属性缓存方式缓存守卫的结论，用能证明结论仍成立的形状
   （接收者的形状、链上各原型的形状或等价的失效机制）校验；命中时只做读取。由实现者设计校验条件，并在 `docs/jit.md` 写明它为什么够。
3. 若测量显示类型化数组元素读写（P66 后读 23.3、写 26.2 ns）在同一循环里也值得同样的处理，写出数字，交总控决定，**本包不做**。

## 测试

差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致），覆盖：九种元素类型；分离的缓冲（循环中途分离）；可变长缓冲在循环中途缩小与增长；
实例上定义自有 `length`；`%TypedArray%.prototype` 的 `length` 被改成别的访问器或数据属性（在编译之前与之后）；中间原型被插入带 `length` 的对象；
`Object.setPrototypeOf` 改接收者的原型；同一站点交替出现类型化数组、普通数组与字符串。

## 验收

- `f64.length` 微探针与 P60 套件 `typed` 的前后对比（同一轮，5 个进程），`perf-js.md` 追加一行。
- `docs/jit.md`「Testing」的整套测试全部通过（同 P66 的验收），test262 `built-ins/TypedArray`、`built-ins/TypedArrayConstructors`
  解释与强制编译逐项 0 差异。
- 体积增量写进 `size.md`。

## 不做

元素读写的进一步内联（见第 3 项）；解释器侧的 `length` 缓存；其他内建访问器。

## 状态

**完成，一项目标差 2 ns。** 分支 `electron/p72-jit-typed-length`，基线 `docs/electron-plan` `345f1645`。

**提交**：移动 commit（`typed_array_length_shortcut` 拆成 `typed_array_length_guard` 与 `typed_array_length_read`，解释器连着调，
行为不变，P65 的 35 个类型化数组测试原样通过）；站点缓存加测试与 `boa/SU.md`；`docs/jit.md` 的说明与数字；`perf-js.md`、
`size.md`、本节。

**校验方案**（`vm/jit/typed_length.rs` 顶部与 `docs/jit.md`「Property caches and elements」有完整论证）：站点记下接收者与链上每个原型
（至多 3 个，到持有访问器的那个为止）的形状和属性存储长度、访问器槽位、固有 getter 的位，并只在垃圾回收计数未变时信任。
关键发现：`Float64Array.prototype` 与 `%TypedArray%.prototype` 是**唯一形状（unique shape）**，不像共享形状那样「改了就换形状」——
它们**原地增加属性**而身份不变，所以光比形状指针会漏掉「之后在原型上定义了自有 `length`」。删除、数据/访问器互换、改原型都会换新的唯一形状；
原地增加会让存储变长，所以再比存储长度。槽里的值是形状管不到的（`defineProperty` 换 getter 形状不变），所以每次命中都比 getter 的位。
接收者形状相同但不是类型化数组（`Object.create(Float64Array.prototype)`）由读取一半拒绝。**读取一半从不缓存**（分离、缩小不改任何形状）。
两个检查各自必需：分别拿掉后 `tests_typed_length.rs` 都会失败（试过）。

**数字**（发布版 `verve --features jit`，Windows，`345f1645` 对本分支，同一轮前后，5 个进程，中位数（最好）；机器共享，
负载见 `docs/jit.md`「Results (typed-array `length`, P72)」与 `perf-js.md`）：

| | 前 | 后 | 目标 |
|---|---|---|---|
| `typed` JIT 列 | 643.9 (618.5) ms | 368.2 (365.9) ms | ≤ 450，**达成** |
| `typed` 解释器列 | 1304.1 (1291.0) | 1308.3 (1291.5) | 不变 |
| `s += f.length` 探针 | 55.7 (54.5) ns | 17.0 (16.5) ns | ≤ 15，**差 2 ns** |
| 元素读 / 写 / 稠密数组 `length` | 15.2 / 17.6 / 5.5 | 15.3 / 17.8 / 5.3 | 不变 |

循环本身 3.3 ns，所以 `length` 的净成本从 52.4 降到 13.7 ns。剩下的是辅助调用本身（推断：元素读同样是一次带 shortcut 的辅助调用，15.3 ns）、
接收者与原型的借用、读缓冲长度。想压到 15 以下需要把守卫改成机器码比较、只留读取一个辅助，没有试。体积 +1,536 字节（`size.md`）。

**第 3 项（元素读写，交总控决定，没做）**：读 15.3、写 17.8 ns，减去 3.3 的循环是 11.9 与 14.5 ns，稠密数组 `length`（内联）只有 2.0 ns。
`typed` 现在一轮约 61 ns（368 ms 除以 6M 轮），其中 `length` + 两次读 + 一次写约 52 ns。读写内联后若各降到几 ns，`typed` 可到 200 ms 左右
（推断，没量）。代价大：要在机器码里认 `TypedArray` 的内部布局（缓冲区、偏移、长度或可变长、元素类型、分离）并检查，那是 P65 的类型。

**测试**：`vm/jit/tests_typed_length.rs`，三种模式（解释、`BOA_JIT_THRESHOLD=0` 等价的 Eager、热后 OSR 的 Hot）一致，并与 Node 22 的输出逐行相同
（脚本里 `detach` 在 Node 用 `structuredClone` 转移，在测试里是注册的原生函数）：十一种元素类型及其子类、循环中途分离与分离后、可变长缓冲
缩小与增长（跟随长度的视图、定长视图、带偏移的视图）、实例上定义自有 `length`（编译前、中途、中途删除、自有 getter）、
`%TypedArray%.prototype.length` 换成别的访问器 / 数据属性 / `undefined` / 删掉（编译前与中途，包括恢复）、计数 getter 的调用次数、
类型化数组各自原型上加 `length`、中间原型插入（有无 `length`）、过深的子类链（超出站点的 3 个）、`Object.setPrototypeOf` 改接收者的原型、
同一站点交替类型化数组 / 数组 / 字符串 / 对象、与类型化数组同形状的普通对象（抛 `TypeError`）、原型变字典模式、循环里触发垃圾回收；
另有一个测试数站点填充次数（热访问只填一次，每次回收各丢一次）。

**验证**（`CARGO_BUILD_JOBS=6`，`VERVE_CODE_CACHE=0`，临时配置目录）：`boa_engine` 带 `jit` 默认与 `BOA_JIT_THRESHOLD=0` 各 1252 个 + 集成全过；
`BOA_JIT_FUZZ_COUNT=3000` 过（274 s）；`boa` 里 `boa_engine` 有无 `--features jit` 的 `clippy --all-targets -D warnings` 过，`cargo fmt`、
`style-check` 过；Verve 无 feature（446）、有 feature（449，第一遍里 `tcp_socket_options_and_inactivity_timeout_are_chainable` 出 `EIO`，
与 P66 记的是同一个，`--no-fail-fast` 重跑全过）、`VERVE_JIT_THRESHOLD=0`（449）、`VERVE_JIT=0`（449）全过，`tests/jit.rs` 3 个过；
test262 `built-ins/TypedArray`（2868 项）与 `built-ins/TypedArrayConstructors`（1444 项）`BOA_JIT=0` 对 `BOA_JIT_THRESHOLD=0` 逐项比较（`-vv` 输出，含严格模式各一项）
**0 差异**。没有跑整个 test262，也没有在 `electron` 二进制（P63）下量同一组数字。

**写测试时撞到的解释器老 bug（与本包无关，没修，基线二进制上就有）**：解释器对原型上属性的内联缓存只认接收者的形状，原型本身变了不失效
（`docs/jit.md` 的风险一节早已写着这一点）。复现：

```js
class A {}  const a = new A();  const f = (o) => o.x;
A.prototype.x = 'p';  f(a); f(a);
delete A.prototype.x;
Object.defineProperty(A.prototype, 'x', { get() { return 'g'; }, configurable: true });
f(a);   // 返回 getter 函数本身，Node 返回 'g'
```

另一种后果是 `index out of bounds` 的 panic（`vm/opcode/get/property.rs`：`delete` 之后再给原型换一个属性更多的原型，缓存槽位越界）。
因为 JIT 的辅助只在解释器缓存命中时才命中，两边错得一样。测试里每个场景用 `new Function` 造一个自己的读取函数，让各自的缓存从空开始，避开它。
这一条值得单独一个包。

**给其他包的备注**：`SiteCache` 末尾追加了字段（`chain`、`lens`、`objects`、`getter`、`depth`），机器码读的偏移没变；`jit_get_length` 多了一个站点参数、
`GetLengthProperty` 现在像 `GetPropertyByName` 一样走 `site_helper`；`vm/jit/tests.rs` 里 `Mode`、`SHOW`、`context` 改成了 `pub(super)`，供新测试用。
P69、P70 若也改 `compiler.rs`/`helpers.rs`/`layout.rs`，冲突应只在这几处。"前"二进制是 `345f1645` 在本工作树里构建后复制到暂存目录
`verve-before.exe`；没有另建工作树。
