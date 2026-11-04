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

未开始。
