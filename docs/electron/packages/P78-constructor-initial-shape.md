# P78 `new` 出来的对象直接用构造器的最终形状

**依赖**：P69、P75（均已合并）  **规模**：M  **修改**：`boa/core/engine/src/builtins/function/mod.rs`（`function_construct`）、
`object/shape/`、`object/jsobject.rs` 与 `object/property_map.rs`（只限按预测形状分配）、`vm/`（只限记录预测所需的状态）、对应测试、
`boa/SU.md`、`docs/jit.md`、`docs/electron/perf-js.md`、`docs/electron/size.md`

## 目标

P69 把 `oo` 从 1106 ms 降到 725 ms，冲刺目标 600 ms 没到。它量出的剩余大头是：`new CX(i)` 比空构造的 `new C0()` 多 183 ns，
几乎都是构造器里 `this.a = …`、`this.b = …` 一次次走形状转换（查转换表、换形状、存储扩容）。
本包让 `function_construct` 按这个构造器**上一次构造结束时的形状**预先准备新对象，使构造器里的属性写入走已缓存的快路径。

目标（P60 `oo`，JIT，中位数）：**承诺 ≤ 650 ms，冲刺 ≤ 600 ms**；`new CX(i)` 与 `new C0()` 的差给前后纳秒数。

## 背景（先读）

- `P69-call-cost.md`「状态」：微探针表（`new C0()`、`new CX(i)`、`new D0()`、`new DX(i)`）与「`this` with the initial shape」一节。
- `object/shape/`：共享形状的转换表、`root_shape`、存储与形状的对应关系；`P75` 对唯一形状与共享形状的校验论证。
- V8 的「slack tracking」思路可以参考，但**不要求**照搬；这里要的是最小的、可证明不改变可观察行为的做法。

## 要做的事

1. **方案**（由实现者定，写明为什么不改变语义）。候选：
   - 只预留存储容量：记下构造器上次构造结束时的属性数，新对象按此预分配，省去扩容。
   - 预测转换链：记下上次的最终共享形状（弱引用）；新对象仍从根形状起步，属性写入沿已有的转换走，但让 `set` 的内联缓存直接命中。
   - 直接以最终形状分配并把槽位置为「未定义」：**只有在能证明属性顺序、`Object.keys`、`in`、`hasOwnProperty`、getter/setter 与 Proxy 观察都不变时才可用**，否则不选。
   每个候选写出测得的收益，选一个；放弃的写明原因。
2. **失效**：构造器的 `prototype` 被换掉、构造器里条件分支导致形状不同、子类（`super()` 返回的对象）、`Reflect.construct` 带不同的 `newTarget`、
   构造器返回别的对象、构造过程中抛错：各种情形下预测都只能是「快一点或不快」，不能是「错」。
3. **内存**：预测状态挂在哪（函数对象、代码块、`Vm`）、多大、何时释放，写进「状态」；`size.md` 追加一行。

## 测试

- 差分测试（解释、`BOA_JIT_THRESHOLD=0`、热后 OSR 三种一致，并与 Node 输出一致），每个用例打印 `Object.keys`、`Object.getOwnPropertyNames`、
  `JSON.stringify` 与原型：属性个数不同的多次构造；条件添加属性；构造中途 `delete this.a`；构造中途抛错再构造；构造器里 `Object.defineProperty(this, …)`；
  `prototype` 被换成别的对象或带 setter 的对象（setter 拦截 `this.a = …`）；`Reflect.construct(C, [], D)`；派生类与基类交替；类字段与私有字段；循环中途触发垃圾回收。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 `language/statements/class`、`language/expressions/class`、`language/expressions/new`、`built-ins/Reflect`、`built-ins/Object` 前后逐项 0 回退；
  除 `staging` 外解释与强制编译逐项 0 差异（计时类 `Atomics.notify` 例外照旧）。
- P60 套件：`oo` 达到承诺目标；`alloc`、`vdom`、`closure`、`json` 不变慢（同一轮，中位差 ±3%）。`perf-js.md` 追加一行。

## 不做

调用帧瘦身（P69「状态」说明那是重新设计）；编译代码直接调用编译代码；对象字面量的形状预测。

## 状态

未开始。
