# P75 原型属性的内联缓存在原型变化后不失效

**依赖**：P70、P72（均已合并）  **规模**：S–M  **修改**：`boa/core/engine/src/vm/inline_cache/`、`vm/opcode/get/property.rs`、
`vm/opcode/set/property.rs`、`vm/jit/helpers.rs` 与 `vm/jit/` 中读写内联缓存的部分、`object/shape/`（只限缓存校验需要的东西）、对应测试、
`boa/SU.md`、`docs/jit.md`（「Risks」与「Property caches and elements」）

## 目标

P72 写测试时发现（总控在 `c91b2938` 的发布二进制上复现，P72 之前就有）：解释器对**原型上的属性**的内联缓存只按接收者的形状做键。
缓存项记下「接收者形状 → 槽位，带 `PROTOTYPE` 标志」，命中时直接读 `shape.prototype()` 的 `storage[slot.index]`，**不检查原型自己变没变**。
后果有两种：

```js
// 1. 读到旧的槽：原型上的数据属性换成访问器后，返回 getter 函数本身
function A() {} A.prototype.x = 'd';
const a = new A(); const f = (o) => o.x;
for (let i = 0; i < 10; i++) f(a);
delete A.prototype.x;
Object.defineProperty(A.prototype, 'x', { get() { return 'g'; }, configurable: true });
f(a);   // Node：'g'；Verve：getter 函数

// 2. 越界 panic：原型删掉属性后存储变短
function B() {} B.prototype.p = 1; B.prototype.q = 2; B.prototype.x = 'd';
const b = new B(); const g = (o) => o.x;
for (let i = 0; i < 10; i++) g(b);
delete B.prototype.p; delete B.prototype.q; delete B.prototype.x;
g(b);   // Node：undefined；Verve：panicked at vm/opcode/get/property.rs:61: index out of bounds
```

JIT 的辅助只在解释器缓存命中时命中，所以两层错得一样。`docs/jit.md`「Risks」早已记着这一条。本包修掉它：
**缓存项命中前必须能证明原型上那个槽仍是同一个属性、同样的种类（数据/访问器）**，否则当作未命中走通用路径。
任何情况下都不得 panic。

## 背景（先读）

- `vm/inline_cache/mod.rs`（`InlineCache`、`CacheEntry`、`get`、`set`），`vm/opcode/get/property.rs` 与 `set/property.rs` 里带 `PROTOTYPE` 的分支，
  `vm/jit/helpers.rs` 里读缓存的两处。
- `P72-jit-typed-length.md`「状态」的校验方案：`Float64Array.prototype` 一类的**唯一形状**原地增加属性而身份不变，
  删除、数据/访问器互换、改原型会换新的唯一形状。P72 的 `vm/jit/typed_length.rs` 用「形状 + 存储长度 + 槽内容」校验，可以参考，
  但这里要的是**解释器与 JIT 共用的**修法。
- `object/shape/`：共享形状与唯一形状的转换规则。

## 要做的事

1. **先确认范围**：读缓存与写缓存（`SetPropertyByName` 经原型上的 setter）各有哪些情形会读到过期的槽；原型是共享形状和唯一形状时分别怎样；
   缓存只记直接原型还是能记更远的原型。写进「状态」，每种情形一个复现。
2. **修法**（由实现者定，写明为什么够）：倾向于在缓存项里同时记下**原型的形状**（弱引用，与接收者形状同样处理），命中时一起比较；
   唯一形状原地变化的情形用 P72 那样的附加条件（存储长度，或一个在唯一形状上「删除/改种类」时递增的计数），选开销最小又能证明正确的那一种。
   读槽一律用不会越界的方式（越界即未命中），作为最后一道保险，不靠它保证正确。
3. **JIT**：`vm/jit/helpers.rs` 跟着同一个校验走；若机器码里有内联的原型槽读取，也要加上同样的检查。
4. **性能**：多出的比较不得让 P60 套件变慢（`oo`、`vdom`、`alloc`、`closure`、`tokenize` 同一轮比较，中位差 ±3% 内）；给出一次原型属性读
   命中路径的前后纳秒数（微探针）。

## 测试

- 差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致，并与 Node 输出一致）：上面两个复现；原型属性 删除 / 重新定义为访问器 / 访问器改回数据 /
  改值（不应失效，照旧命中且值正确）/ 在原型上新增同名属性遮蔽更远原型上的；`Object.setPrototypeOf` 改接收者或原型的原型；原型变字典模式；
  经原型上 setter 的写入在 setter 被删除或换掉后；内建原型（`Array.prototype`、`Map.prototype`）上的同样操作；循环中途触发垃圾回收。
- 新测试修复前失败（第二个是 panic），修复后通过；「状态」贴修复前的输出。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262（除 `staging`）解释与强制编译逐项比较 0 差异（计时类 `Atomics.notify` 例外照旧）；`built-ins/Object`、`language/expressions/object`、
  `language/statements/class` 前后逐项 0 回退。
- P60 套件不变慢；`perf-js.md` 追加一行；`docs/jit.md`「Risks」改写这一条。

## 不做

内联缓存的其他改进（多态容量、megamorphic 策略）；P72 的类型化数组 `length` 缓存（已有自己的校验）。

## 状态

**完成。** 分支 `electron/p75-prototype-inline-cache`，基线 `docs/electron-plan` `2e902b19`。

**范围（先确认的）**：缓存只记**直接原型**上的槽（更远的原型会让 `PROTOTYPE` 位被置两次，槽就是 `NOT_CACHEABLE`），读缓存和写缓存（经原型上的 setter；原型上的数据属性写入永不缓存，因为接收者不是原型）
都读 `shape.prototype().properties().storage[slot.index]`，不看原型自己变没变。会读到过期槽的情形，每一种在 `vm/jit/tests_prototype_cache.rs` 里有一个场景（编号即场景号）：

| 情形 | 修复前（解释器；Eager、Hot 相同） |
|---|---|
| 原型上数据属性删除后重新定义为访问器（1，13，14，15，20，27，39，42：普通、字典模式、`Array.prototype`、`Object.prototype`、循环中途 GC、多态） | 返回 getter 函数本身（`fn get`），Node 返回 `"g"` |
| 原型删掉属性后存储变短（2，8，11，16，18，26，30，32，33，34，36） | `index out of bounds` panic（`get/property.rs`、`set/property.rs`） |
| 删掉原型上更靠前的属性再加回，`x` 的槽位移动（3） | 读到别的属性的值（`3` 而非 `"d"`） |
| 原型变字典模式后删除（13、14）、唯一形状原型上属性删除（22、24、25：`Array.prototype`、`Map.prototype`、`Object.prototype`） | 删除后仍读到旧值，`undefined` / `number` 不出现 |
| 接收者是唯一形状（超过 1024 次转换），自身新增同名属性遮蔽原型的（17）；全局对象上经 `Object.prototype` 的名字读，之后定义同名全局（28） | 仍读到原型的值——唯一形状原地增加属性，形状身份不变 |
| 访问器原型上 setter 被删、改成数据属性、改成只读数据（30，32，33，34，36） | panic（如上） |
| 唯一形状上同种类属性改写属性位（只读，43） | 缓存槽带旧的位，只读之后仍写进去（`0,1,…,15`，Node 停在 `11`） |
| setter 设为 `undefined` 后严格模式写（45） | 缓存命中后静默不做事，Node 抛 `TypeError` |
| getter 在缓存填入前改了原型自己（38） | 用旧槽配新形状，返回 `1`，Node 返回 `"back"` |

没问题的：改值（5）、换 getter 函数（6）、`Object.setPrototypeOf` 改接收者或原型的原型（10，11 里的 panic 是删除造成的）、原型上新增同名属性遮蔽更远的（7，8：更远的槽本来就不缓存）、
`Map.prototype` 访问器替换（23）。全部修复前失败的场景共 28 个/45 个；两个任务书里的复现是场景 1 和 2。

修复前的输出：整个测试在场景 2 panic：`panicked at core\engine\src\vm\opcode\get\property.rs:62:43: index out of bounds: the len is 1 but the index is 3`；
逐场景（panic 记为 PANIC）：#1 `["\"d\"","fn get"]` 对 Node `["\"d\"","\"g\""]`，#2 PANIC（Node `["\"d\"","undefined"]`），#3 `["\"d\"","3"]`（Node `["\"d\""]`），
#17 `["\"p\""]`（Node `["\"p\"","\"own\""]`），#28 `["\"o\""]`（Node `["\"o\"","\"g\""]`），#43 十五个递增值（Node 十二个），#45 `["1",…,"12"]`（Node 末尾多一个 `"threw TypeError"`），其余同上表。

**校验方案**（`vm/inline_cache/mod.rs`，完整论证在 `docs/jit.md`「Property caches and elements」）：

1. 缓存项里在接收者形状之外记下**原型的形状**（弱引用）；命中时 `shape.prototype()` 的当前形状必须等于它，不等就把这一项删掉（下一次访问重新记，不会堆积出多余的项）。
2. 共享形状不可变，接收者形状与原型形状相同就蕴含槽、种类（数据/访问器及 get/set 位）、原型对象、接收者没有同名自有属性都没变。
3. 唯一形状：删除、改种类、改原型本来就换新形状；**同种类的属性位改变原来是原地的**（`unique_shape.rs` 里的 `TODO: invalidate the pointer`），现在也换新形状（开销：一次表移动，不复制）。
   原地增加属性只追加槽，不动已有的槽，所以唯一形状的**原型**不需要额外条件；但唯一形状的**接收者**增加的自有属性可以遮蔽原型的，形状却不变，所以**唯一形状的接收者不记原型槽**（走慢路径，自有槽照旧记）。
   这比 P72 的「存储长度」条件更省：命中路径上没有额外比较，因为不在缓存里。
4. 记入前再核对一次：槽来自可能运行过 getter 的查找，所以记入时在持有槽的形状里按名字查，索引与属性位（去掉缓存位）必须一致才记。
5. 通过缓存调用 setter 时，存储里不是函数（`set: undefined`）当未命中，让 `[[Set]]` 决定（严格模式抛错）。
6. 最后一道保险：槽的读取一律 `storage.get(..)`，越界是未命中，不 panic；上面几条使它不可达，不靠它保证正确。

JIT：辅助 `cached_data_property`、`jit_set_property` 走同一个 `InlineCache::slot_for`（不改缓存），站点缓存只为共享形状的自有槽填，机器码里没有内联的原型读取，所以不需要改机器码。

**提交**：`bf2c3c4f` fix(vm) 校验原型形状（含唯一形状改属性位、`boa/SU.md`）；`a9355b34` test(vm) 45 个场景 + 两个解释器 only 的复现；`729a4739` docs(jit)；
`80474b92` perf(vm) 单次借用、不升级形状；`e972e098` refactor(vm)；`ffc797e8` perf(vm) 比较原型形状时不克隆原型；本节与 `perf-js.md` 是最后一个 docs 提交。

**测试**：`vm/jit/tests_prototype_cache.rs`——45 个场景，解释、Eager（`BOA_JIT_THRESHOLD=0` 等价）、Hot（OSR）三种一致且与 Node 22 的输出逐行相同；
`vm/inline_cache/tests.rs`——两个复现（无 `jit` 特性也跑）、「原型换形状后项被替换而不堆积」、「唯一形状接收者不记原型槽」。修复前 28 个场景失败（上表），其中场景 2 等 11 个是 panic。

**性能**（发布版 `verve --features jit`，`2e902b19` 对本分支，机器共享，负载见 `perf-js.md`）：原型数据属性读命中 17.4 → 19.0 ns（+1.6 ns；微探针，`-A`，5 次取最好，减去 4.6 ns 循环；
修复一开始是 +4 ns，之后三次削减：单次借用且不升级形状、不克隆原型），`a.push` 循环每轮 86 → 88 ns，解释器列在噪声内。
P60 五项同一轮（`--runs 7`，负载 4–15 %），中位数变化：`oo` +2.0 %、`vdom` +3.5 %（最好值 +2.0 %）、`alloc` +1.0 %、`closure` -1.6 %、`tokenize` +1.6 %（编译列）；解释器列 +2.0、-5.0、+2.6、+1.3、-3.2 %。
`vdom` 的中位数略过 3 %，该项方差大（前后两个进程之间可差 5 %），最好值在 ±2 % 内；**没有再重复一轮**。较早一轮（削减前，负载 15–35 %）`alloc` 编译列 +5.7 %，已被后面的修改去掉。

**验证**（`CARGO_BUILD_JOBS=6`）：`boa_engine` 默认 1215、`--features jit` 1269、`BOA_JIT_THRESHOLD=0 --features jit` 1269 个全过（最后一次提交后重跑了 jit 与默认的库测试）；
`BOA_JIT_FUZZ_COUNT=3000 cargo test -p boa_engine --features jit --lib random_programs_match_the_interpreter`：`1 passed`，384 s；`boa` 里 `clippy --all-targets -D warnings` 有无 `jit` 都过；
`cargo fmt --all --check`、`style-check` 过；根 `cargo test -p verve` 有无 `--features jit` 全过；
test262（`E:\test262`，`d86b2294`，release `boa_tester --features boa_engine/jit`，关 LTO 构建，`-vv` 逐条比对）：`built-ins`（23812）、`language`（23724）、`annexB`（1086）、`intl402`（3357）、`harness`（116）
`BOA_JIT=0` 对 `BOA_JIT_THRESHOLD=0` **0 差异**（`Atomics.notify` 例外这次也没有出现差异）；没有跑 `staging`（任务书排除）。
`built-ins/Object`（3411）、`language/expressions/object`（1170）、`language/statements/class`（4367）前后（`2e902b19` 对本分支）解释与强制编译各比一次，**0 回退、0 差异**。

**没做 / 注意**：没有在 `electron` 二进制（P63）下测；性能数字是单机共享、一轮的结果；Linux 交叉检查（`linux-check.sh`）没跑（没有平台分支）；
`boa/` 里 `get/name.rs`（全局名读取）也走同一缓存，修了，场景 28 覆盖；顺带发现并一起修了**自有槽**的两个同源问题（唯一形状改属性位、getter 改动后用旧槽配新形状），范围略超任务书标题，但属同一校验。
没碰的：读缓存的 `number` 类的内联机器码、多态容量。
