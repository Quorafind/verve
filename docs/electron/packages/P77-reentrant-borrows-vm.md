# P77 借用重入排查：`vm/` 与 `object/`

**依赖**：P73、P75（均已合并）  **规模**：S–M  **修改**：`boa/core/engine/src/vm/`、`boa/core/engine/src/object/`、`boa/core/engine/src/environments/`
（只限修复可达的借用重入），对应测试、`boa/SU.md`

## 目标

P73 扫完了 `builtins/`：持有对象（`GcRefCell`）借用期间运行用户代码，用户代码再要可变借用同一对象，就会 `BorrowError`/`BorrowMutError` panic。
修了数组迭代器、`matchAll` 迭代器和 `Intl.Collator` 三处。它的「状态」最后写着：`vm/`、`object/` 没有看。
P75 在内联缓存里又发现一类相同性质的问题（命中时借着接收者去读原型）。本包用同样的方法把 `vm/`、`object/`、`environments/` 扫一遍，修掉可达的。

## 背景（先读）

- `P73-reentrant-borrows.md`「状态」：扫描方法、判断规则（只有会被可变借用的类型才可能出事）、清单格式、Node 对照的做法。
- `P75-prototype-inline-cache.md`「状态」：`try_borrow` 失败按未命中处理的写法。
- `object/internal_methods/`（普通对象与 Proxy 的内部方法、`get`/`set` 上的 getter/setter 调用）、`object/jsobject.rs`、`object/operations.rs`；
  `vm/opcode/` 里在借用期间调用 `call`/`construct`/`get`/`set`/转换的处理函数；`vm/jit/helpers.rs`；`environments/`（`with` 对象环境、全局对象环境）。

## 要做的事

1. **扫描**：读上述目录下所有 `borrow`/`borrow_mut`/`try_borrow*`/`downcast_*` 的守卫存活期，看其中有没有可能运行用户代码的调用（getter/setter、Proxy 陷阱、
   `valueOf`/`toString`/`Symbol.toPrimitive`、回调、终结器）。每一处写进「状态」的清单：可达（附复现）或不可达（附理由）。
2. **修复**：可达的每一处，先写在修复前 panic 的测试，再改成「读出所需的值、放开守卫、再运行用户代码」；需要回写时重新借用并重新校验。
   语义按 Node 22 的输出（与规范不一致时照 Node，写明）。
3. **JIT**：辅助函数里的借用都应当是 `try_borrow`，失败走慢路径；不是的列出并修掉。

## 测试

- 每个可达点一个复现测试（修复前 panic，修复后通过），并与 Node 输出一致；解释、`BOA_JIT_THRESHOLD=0` 两种都跑。
- 「状态」贴修复前的 panic 信息。

## 验收

- `boa/` 内 `cargo test -p boa_engine`（默认、`--features jit`、`BOA_JIT_THRESHOLD=0 --features jit`）、模糊测试 `BOA_JIT_FUZZ_COUNT=3000`、
  clippy `-D warnings`（有无 `jit`）、`cargo fmt`；根 `cargo test -p verve`（有无 `jit`）、`cargo fmt --all --check`、`bash scripts/style-check.sh`。
- test262 中与修改相关的目录（至少 `built-ins/Proxy`、`built-ins/Object`、`language/statements/with`、`language/expressions/assignment`）前后逐项 0 回退、0 panic。
- 碰到热路径（`vm/opcode/get`、`set`、`call`）的修改，给出 P60 `oo`、`vdom`、`closure` 同一轮前后对比，中位差 ±3% 内。

## 不做

`builtins/` 的再次扫描（P73 已做）；与借用无关的正确性问题（发现了写进「状态」，不修）。

## 状态

**完成**。分支 `electron/p77-reentrant-borrows-vm`，从 `docs/electron-plan` `c28e026c` 切出，三个修复提交（各带 `boa/SU.md` 条目与测试）：
`fix(object)`（类的字段初始化）、`fix(object)`（绑定函数的 `instanceof`）、`fix(jit)`（编译代码读对象时改用不 panic 的借用），外加本文档提交。
改动只在 `object/operations.rs`、`object/jsobject.rs`、`builtins/typed_array/object.rs`、`builtins/array/array_iterator.rs`、`builtins/iterable/mod.rs`、`vm/jit/call.rs`、
`tests/reentrant.rs`、`boa/SU.md`；`vm/opcode/` 的热路径（`get`/`set`/`call` 的处理函数）没有动。

### 判断规则

同 P73：持有共享借用只会在**别处要求同一对象的可变借用**时 panic。区别在于这里的“可变借用”不挑类型：`Object` 整个 `GcRefCell` 在每次属性写、
`defineProperty`、私有元素添加、`[[SetPrototypeOf]]`、`preventExtensions` 时都被 `borrow_mut`，所以**任何**跨用户代码持有的 `borrow()` 或 `downcast_ref::<T>()`
（不论 `T`）都会被用户代码里一次对该对象的普通写入打 panic。跨用户代码持有可变借用更糟，但这三个目录里已经没有（见下）。
`JsObject::is::<T>()` 与 `vtable()` 不借用（文档里“Panics if mutably borrowed”的说法过时了，没有动）。

### 扫描清单

方法：读 `vm/`、`object/`、`environments/` 下所有 `borrow`/`borrow_mut`/`try_borrow*`（约 185 处）与 `downcast_ref::<_>`/`downcast_mut::<_>`（约 55 处；
第一遍的 grep 因 turbofish 漏掉了后一类，第二遍补全）的守卫存活期，再用 45 个“用户代码里写接收者”的脚本动态验证：
getter/setter、键的 `toString`、`valueOf`/`Symbol.toPrimitive`、`Symbol.hasInstance`、Proxy 的 13 个陷阱、`with` 的 `has`/`unscopables`、全局 getter/setter、
类字段/静态块/私有名、`super` getter、`Reflect.construct` 的代理 `newTarget`、迭代器、`arguments`、thenable、标签模板、`for-in`、内联缓存的原型 getter、`Symbol.species`。
解释与 `BOA_JIT_THRESHOLD=0` 都跑，修复后 0 个 panic（脚本 `zz_scratch.rs` 在 scratchpad `p77/`，没有提交）。

**可达：2 处（已修）。JIT 路径上另有 5 处会 panic 的借用，不可构造，但按任务书第 3 条改成 `try_*`。**

| 文件 | 函数 | 守卫存活期间的用户代码 | 复现 | 修复前 panic |
|---|---|---|---|---|
| `object/operations.rs` | `initialize_instance_elements` | 持有类构造函数的 `downcast_ref::<OrdinaryFunction>()`（整个对象的共享借用）循环调用 `define_field`；字段初始化器是用户代码，`private_*_add`/`create_data_property_or_throw` 还要借用 `this` | `class X { static n = 0; id = ++X.n }; new X()`（初始化器写类）；`class A { constructor() { return Y } } class Y extends A { f = 1; #p = 2 } new Y()`（`this` 就是类本身，不需要用户代码也 panic） | `Object already borrowed: BorrowMutError`（`internal_methods/mod.rs:1154`、`:1035`，私有方法 `operations.rs:1093`） |
| `object/operations.rs` | `ordinary_has_instance` | 持有绑定函数的 `downcast_ref::<BoundFunction>()` 时调用 `instance_of(target)`，即目标的 `@@hasInstance`（用户代码） | `function F(){}; const bf = F.bind(); Object.defineProperty(F, Symbol.hasInstance, {value(){ bf.x = 1; return true }}); ({}) instanceof bf` | `Object already borrowed: BorrowMutError`（`vm/opcode/set/property.rs:73`） |

第一处不是刻意构造的写法：类字段里给静态计数器加一（`id = ++X.count`）就会触发。

修法：前者在块内把 `[[PrivateMethods]]` 与 `[[Fields]]` 复制出来（两个列表都为空时直接返回，`new A()` 的热路径没有多出开销），放开借用后再逐个执行，
与规范“fields 取自开始时的值”一致；后者先把目标函数读出来再放开借用。

**JIT 路径上不用 `try_borrow` 的借用**。要 panic 得先有跨用户代码的可变借用，而本包与 P73 之后 `vm/`、`object/`、`environments/`、`builtins/` 里没有，所以构造不出 panic；
单元测试在持有 `borrow_mut()` 时直接调这些函数，基线上 3 个 panic，修复后通过：

| 位置 | 原来 | 现在 |
|---|---|---|
| `typed_array_element_get_shortcut`/`typed_array_element_set_shortcut`（`jit_get_element`/`jit_set_element`，也是解释器 `get`/`set` 的快路径） | `downcast_ref::<TypedArray>()` | `try_downcast_ref`，失败返回 `None` 走慢路径 |
| `typed_array_length_guard`/`typed_array_length_read`/`is_intrinsic_length_getter`（`jit_get_length` 经 `SiteCache::typed_array_length`） | `borrow()`、`downcast_ref` | `try_borrow`/`try_downcast_ref`，失败当“不是” |
| `vm/jit/call.rs::compiled_callee`（编译代码的调用） | `downcast_ref::<OrdinaryFunction>()` | `try_downcast_ref` |
| `ArrayIterator::advance_dense`（`jit_iterator_next`） | `downcast_mut::<ArrayIterator>()` | `try_downcast_mut`，失败 `Declined` |
| `advance_quietly` 的 Map/Set/String 迭代器 | `advance` 内 `downcast_mut` | 先 `try_borrow_mut().is_ok()`，否则 `Declined` |

新增 `JsObject::try_downcast_ref`/`try_downcast_mut`（crate 内）。`compiled_callee` 与 `advance_quietly` 这两行没有单独的测试（私有函数，只能靠持有借用触发）。
其余 JIT 辅助函数本来就是 `try_borrow`/`try_borrow_mut`：`cached_data_property`、`jit_get_length` 的数组分支、`jit_set_property`、`jit_get_element`/`jit_set_element` 的数组分支、
`Array::plain_dense_array`、`typed_length.rs` 的 `holds`/`remember`、`inline_cache` 读原型；`binding_value` 读的环境绑定是叶子借用。

**不可达**（守卫内不运行用户代码，或释放后才运行）：

- `object/jsobject.rs`：`downcast_ref/mut` 与 `borrow*` 的封装本身；`get_property`（临时守卫，无调用）；`RecursionLimiter`（线程局部 `RefCell`，闭包内无调用）；`Debug::fmt`（只读自身）；`prototype()`/`extensible()`/`set_prototype`/`insert`（叶子）。
- `object/internal_methods/mod.rs`、`string.rs`：`ordinary_get_own_property`、`ordinary_define_own_property`（先 `__get_own_property__`/`__is_extensible__`，再调只有叶子 `borrow_mut` 的 `validate_and_apply_property_descriptor`）、
  `ordinary_delete`、`ordinary_own_property_keys`、`ordinary_prevent_extensions`/`is_extensible`；`ordinary_get`/`ordinary_set` 本身不借用。字符串外来对象的 `JsString` 克隆后才用。
- `object/operations.rs`：`length_of_array_like`（数组的 `length` 槽恒为数字，`to_u32` 不运行用户代码）、`private_element_find`（克隆后返回）、`private_field_add`/`private_method_or_accessor_add`（`borrow_mut` 只在末尾追加）、
  `private_set`（调用 setter 前 `drop`）、`is_array_abstract`/`get_function_realm`（递归 Proxy/绑定函数的目标，只有 `try_data` 克隆，不触发陷阱）。
- `object/mod.rs`（构造器构建）、`object/builtins/*`（`JsSet`/`JsPromise`/`JsArrayBuffer`/`JsDataView`/`JsTypedArray` 等 Rust API 封装：块内读取，不调用转换）、`object/shape/*`（不运行 JS）。
- `vm/opcode/get/{property,name}.rs`：守卫在调用 getter 前 `drop`；`set/property.rs`：`cached_set` 在 setter 前 `drop`，`set_new_own_property`/`set_dense_property` 只处理数据；
  `set/private.rs`、`define/own_property.rs`（`define_new_own_property` 只在 `is_ordinary()` 且无同名键时直接插入）、`define/class/*`、`push/class/*`、`push/environment.rs`、`set/class_prototype.rs`、`function.rs`（`SetHomeObject`/`GetHomeObject`）：
  `downcast_mut::<OrdinaryFunction>()` 都是对刚创建的函数的一条语句，`set_function_name` 等可观察的调用在它之前；
  `push/array.rs`（`push_dense` 是叶子）、`call/mod.rs` 的 spread（`to_dense_indexed_properties` 克隆）、动态 `import()` 的 `RefCell<&mut Context>`（`module.load` 自己建 `RefCell`，不重入）、
  `meta`（`import.meta` 的 `RefCell` 里只调宿主钩子 `init_import_meta`，不是用户 JS）、`generator`/`await`/`yield_stm.rs`（`AsyncGeneratorClose` 调 `complete_step` 前 `drop`；`yield_stm` 持有 `borrow_mut` 时的 `into_opaque` 只建错误对象）。
- 绑定函数的 `[[Call]]`/`[[Construct]]`（`builtins/function/bound.rs`）持有 `downcast_ref::<BoundFunction>()` 到函数结束，但 `target.__call__()`/`__construct__()` 只返回 `CallValue::Pending`，
  真正的调用由解释器之后执行，那时守卫已释放。绑定原生函数、绑定 Proxy 的陷阱里写绑定函数自己的两个测试作回归，修复前也通过。
- Proxy 的 13 个内部方法（`builtins/proxy`）：`downcast_ref::<Proxy>()....try_data()` 是一条语句，克隆出目标与处理器后守卫就释放；`revoke` 的 `downcast_mut` 是叶子。
- `arguments` 外来对象（`MappedArguments` 的 `get/set/delete` 只读写环境绑定）、模块命名空间（`ModuleNamespace` 只在创建时 `downcast_mut`，之后只读）。
- `vm/inline_cache`：`entries` 是 `GcRefCell`，`get`/`set` 在叶子里借用；命中时读原型用 `try_borrow`（P75）。`vm/code_block.rs`：惰性编译的 `RefCell` 里不运行用户代码。
- `environments/runtime`：声明式环境的 `bindings`/`this` 等 `GcRefCell` 都是叶子借用；对象环境（`with`、全局对象）在 `find_runtime_binding`/`get_binding`/`set_binding`/`delete_binding` 里先 `clone()` 对象再 `has_property`/`get`/`set`，没有守卫；
  模块环境 `get` 里对 `accessor` 的 `clone().borrow()` 与 `borrow_mut()` 是有意错开的两个 `RefCell`。
- `FinalizationRegistry` 清理、GC 终结器：只做 Rust 侧清理，用户回调经任务队列执行（P73 已覆盖）。

### Node 对照（Node 22.21.1）

| 用例 | Node 与 Verve（release + `jit`；解释、`VERVE_JIT_THRESHOLD=0` 各一遍）输出 |
|---|---|
| 字段初始化器写类 `class X { static n = 0; id = ++X.n }` | `1 2 2`（修复前 panic） |
| 基类构造函数返回派生类（公有字段 + 私有字段 + `#p in Y`） | `true 1 true`（修复前 panic） |
| 同上，私有方法 | `7`（修复前 panic） |
| `instanceof` 绑定函数，`@@hasInstance` 写绑定函数 | `true 1`（修复前 panic） |
| 绑定原生函数（`forEach`、`Promise`）回调里写绑定函数 | `2 5`（修复前也不 panic） |
| 绑定 Proxy 的 `apply`/`construct` 陷阱里写绑定函数 | `1 1 2`（修复前也不 panic） |

Node 与规范在这几处没有差异，不需要“照 Node”的取舍。脚本在 scratchpad `p77/`（`f1.js`…`f6.js`）。

### 验收

- 新测试 `tests/reentrant.rs` 9 个：类字段/私有方法 3 个、`instanceof` 1 个（以上修复前 panic）、绑定函数回归 2 个（修复前也通过）、JIT 借用单元测试 3 个（修复前 panic；`array_iterator_step` 需 `--features jit`）。
- `boa/`：`cargo test -p boa_engine` 默认 1238 全过；`--features jit` 1295 全过；`BOA_JIT_THRESHOLD=0 --features jit` 1295 全过；
  `BOA_JIT_FUZZ_COUNT=3000 cargo test -p boa_engine --features jit --lib random_programs_match_the_interpreter` 1 个测试通过（197 s）；
  `cargo clippy -p boa_engine --all-targets -- -D warnings` 有、无 `--features jit` 都过；每个提交的 `cargo fmt`、根 `cargo fmt --all --check`、`bash scripts/style-check.sh` 都过。
- 根：`cargo clippy --workspace --all-targets -- -D warnings` 过；`cargo test -p verve` 默认 449、`--features jit` 452 全过。
- test262（`E:\test262`，release `boa_tester`，基线 `c28e026c` 对改动后，`compare`）：`built-ins/Proxy` 311、`built-ins/Object` 3411、`language/statements/with` 181（5 个既有失败）、
  `language/expressions/assignment` 485（1 个既有失败）、`language/statements/class` 4367、`language/expressions/class` 4059、`language/expressions/instanceof` 43、`built-ins/Function` 509（1 个既有失败）、
  `built-ins/Reflect` 153、`language/expressions/super` 94、`language/expressions/new` 59、`language/expressions/compound-assignment` 454：**全部 0 差异、0 panic**。
- P60（release + `jit`，每格 7 个进程取中位数（最好），ms，基线与改动后同一轮相邻测量，整机负载 2–27%）：

| 测试 | 基线 JIT | 之后 JIT | 基线解释 | 之后解释 |
|---|---|---|---|---|
| `oo` 第 1 轮 | 694.3 (683.6) | 706.2 (694.7) | 769.3 (750.5) | 767.8 (753.7) |
| `oo` 第 2 轮 | 695.1 (687.5) | 692.0 (682.5) | 765.2 (757.8) | 772.6 (761.8) |
| `vdom` 第 1 轮 | 48.4 (46.0) | 49.1 (48.5) | 50.8 (49.2) | 50.5 (48.7) |
| `vdom` 第 2 轮 | 48.6 (47.3) | 48.5 (47.1) | 51.1 (49.2) | 51.7 (49.5) |
| `closure` 第 1 轮 | 442.4 (435.2) | 448.9 (444.1) | 654.6 (646.0) | 657.2 (646.5) |
| `closure` 第 2 轮 | 440.7 (433.8) | 434.3 (427.3) | 656.6 (638.3) | 654.3 (636.4) |

  中位数差都在 ±3% 内（最大 `oo` JIT +1.7%、`closure` JIT +1.5%）。早先在没有“两个列表都为空就返回”的第一版上量过一次：`oo` 解释 +2.2%/+4.2%，加了早退之后消失；
  那一版的 `closure` 解释路径 +2% 到 +4%，本包没有任何改动碰到它（无类、无类型化数组），终版上复测为 +0.4%/-0.4%，按噪声处理。另有两轮被他人的构建抬高了负载（26–47%），丢弃。

### 没做 / 已知问题

- `builtins/` 没有重扫（P73 已做）。
- 非借用问题：没有发现要登记的。仅一条文档问题：`JsObject::is`、`is_ordinary` 等的 “Panics if the object is currently mutably borrowed” 已过时（`is` 只读 `Gc` 的类型信息，不借用），没改。
- `JsObject::downcast_ref`/`downcast_mut` 作为公开 API 仍会 panic，行为不变；本包保证的是这三个目录里引擎自己不会在用户代码运行期间持有它们。
