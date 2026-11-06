# 代码风格（Verve 全仓库）

> 目的：几十个包、几个实现者写出来的代码，读起来像一个人写的。本文的规则**都来自 Verve 现有代码的主流写法**，
> 不是新发明；拿不准时，照 `src/path.rs`（模块文档、分层）和 `src/native.rs`（工具箱）的样子写。
> 能机器检查的由 `scripts/style-check.sh` 和 clippy 检查（见文末），其余由合并前的风格审查（[`review.md`](review.md)）把关。

## 1. 一个工具箱

**所有 crate 共用 `verve::native` 一套原生函数工具**（P03 把 `src/native.rs` 公开为 `verve::native`）。

- 禁止在别的 crate 或模块里再写一份 `arg`、`js_text`、`text_of`、`type_error`、`coded_error`、`object`、`function`、
  `closure`、`get`、`array`、`list_of`、`class`/`ClassSpec`/`plain_class`、`constructor`、`hidden`、`exports`… 缺什么就**加进 `src/native.rs`**，
  其他人一起用。`style-check` 会拦下这些名字的新定义。
- 同理：base64、data: URL、percent-decode 只在 `src/encoding.rs`（R1 公开为 `verve::encoding`）；EventEmitter 子类只用
  `verve::events` 的辅助；结构化克隆只用 `verve::clone`。
- verve-dom 的 `bind/mod.rs` 曾有一套同名不同签名的副本（`text`、`list`、`string`、`arg_string`、`plain_object`…），R1 已迁到
  `verve::native` 并删除；`style-check` 拦下 verve-dom 里这些名字的重新定义，`ci/style/toolkit-copies.txt` 已清空。verve-dom 自己的
  `webidl_class`（登记在文档的类表里、方法可枚举、带 `toStringTag`）不是 `native::class`，名字也不同；`struct ClassSpec` 只在
  `src/native.rs`。
- 切换 Realm 只用 `verve::in_world` / `verve::in_realm`（`src/world.rs`，返回时放回原来的 Realm）；别处手写 `enter_realm` 会被
  `style-check` 拦下。
- Electron 的 API 定义辅助（P02 的 `api/define.rs`）**建立在 `verve::native` 之上**，只加 Electron 特有的部分
  （句柄映射、已销毁检查、参数提取），不另起一套。

## 2. 命名

一个概念一个名字，全仓库一致：

| 概念 | 用这个 | 不用 |
|---|---|---|
| 函数参数里的 Boa 上下文 | `context: &mut Context` | `ctx`、`cx`（函数签名里） |
| 原生闭包的参数 | `\|this, args, cx\|`（不用的写 `_`） | `\|_, a, c\|` |
| 构造一个内置模块的函数 | `fn make_<module>(context, name)` 或 `<module>_module` 二选一，**同一 crate 内统一** | 混用 |
| 布尔查询 | `is_*` / `has_*` | `check_*` 返回 bool |
| 取值 | 名词：`fn bare_name(..)`、`fn node_of(..)` | `get_*` 前缀（Rust 惯例不用） |
| 转换 | `to_*`（复制）/ `as_*`（借用）/ `into_*`（消耗） | `convert_*` |
| ID 类型 | newtype 或明确的类型别名：`WindowId`、`ContentsId`、`NodeId` | 裸 `u32` 到处传 |

- 不造缩写，除了已通行的：`cx`（仅闭包）、`id`、`fd`、`url`、`ipc`、`dom`、`css`、`js`。
- 模块名是名词（`hub`、`asar`、`ipc`），不是动词。
- JS 可见名字与 Electron/Node/Web 规范**逐字一致**（`webContents`、`getAllWindows`），Rust 侧用 snake_case 对应。

## 3. 文件结构

```rust
//! `<模块>`：它是什么、照谁的行为（Node 的 `lib/path.js`、Electron 39 文档、WHATWG 某规范）。
//! 一两段说清楚设计上的取舍（为什么这样而不那样）。不写历史、不写"本模块包含……"清单。

use …;                        // std → 外部 crate → crate 内，各一组

const …;                      // 常量
pub(crate) struct / enum …;   // 类型

pub fn install / make_…       // 对外入口在前
fn helper …                   // 辅助函数自上而下，按调用顺序

#[cfg(test)] mod tests { … }  // 纯函数的小单测；行为测试放 tests/
```

- 可见性用最小的：默认私有，`pub(crate)` 次之，`pub` 只给嵌入方 API。
- 一个文件一个职责。新文件 ≤ 1500 行；已有超长文件不许再变长（`style-check` 的棘轮），在重构关卡拆分。

## 4. 注释与文档

Verve 的注释是**陈述句，说"是什么、为什么"**：

```rust
/// The name Node's built-in module answers to, as `require` was given it: `node:` and a
/// trailing `/` taken off, and Electron's `original-fs` read as `fs`.
pub(crate) fn bare_name(name: &str) -> &str {
```

- 公开项都有文档注释；一段就够。私有函数名字说清楚了就不写。
- 写清行为依据：Node/Electron/规范里对应的名字或章节。
- 不写：复述代码的注释、`// TODO`（要写就写 `// P31: …` 指明哪个包负责）、被注释掉的代码、修改历史、"Note: …"式自言自语。
- 注释语言：代码注释与文档注释用**英文**（与现有代码一致）；`docs/electron/` 下的任务书用中文。

## 5. JS 可见 API 怎么写

- 类：`native::class(context, ClassSpec { … })`；方法表用 `&[(name, length, fn)]` 的数组（照 `exports`/`methods`）。
  一张表看得到一个类的全部方法，不要散落的 `set` 调用。
- 参数：`arg(args, i)` 取值，统一的提取辅助做校验；错误是 Node 的形态：`coded_error(context, msg, "ERR_INVALID_ARG_TYPE")`。
- 继承 EventEmitter：照 `src/sockets.rs`；两种继承（`class X extends`、`util.inherits`）都要成立。
- 不支持的功能：对象存在，调用抛 `coded_error(context, "<api> is not supported by Verve", "ERR_NOT_SUPPORTED")`。
- 每个方法的 Rust 实现只做"取参数 → 调一个有类型的 Rust 函数 → 包装结果"。业务逻辑写在不碰 `JsValue` 的普通 Rust 函数里，
  这样能单测、能复用。

## 6. 错误与 panic

- 非测试代码不用 `unwrap()`；确实不可能失败的用 `expect("为什么不可能")`。
- 错误在边界转换一次：内部用 `Result<T, 具体错误类型>` 或 `io::Error`，到 JS 边界才变 `JsError`。
- 不吞错误：`let _ = …` 只用于"失败也无所谓"的场合，并在旁边说明为什么无所谓。
- isolate 依赖 `catch_unwind`，但 panic 仍是 bug，不是控制流。

## 7. 状态、线程与所有权

- 每个 realm 的状态挂在 `crate::shared(context)` 那样的单一入口上，不用全局 `static mut`、不用到处传 `Rc<RefCell<…>>`。
- 跨线程只走 P02 的消息类型；不共享 `JsValue`、不在 GPUI 线程碰 JS、不在 JS 线程碰 GPUI。
- 能用 enum 表达的状态不用几个 bool 组合；能用类型保证的不靠注释约定。

## 8. 测试

- 行为测试在 crate 的 `tests/<主题>.rs`；测试名是一句描述行为的话：`hex_and_base64_read_as_node_reads_them`。
- 期望值来自真实 Node/Electron/浏览器时，冻结在测试里并注明来源和版本。
- 测试也守本文的规则（不重复：公共夹具放 `tests/common` 或 `tests/support`）。

## 9. 规模上限（机器检查）

| 项 | 上限 | 检查 |
|---|---|---|
| 函数行数 | 80 | clippy `too_many_lines`（新 crate 开启） |
| 参数个数 | 7 | clippy `too_many_arguments` |
| 文件行数 | 1500（新文件）；旧超长文件不得增长 | `scripts/style-check.sh` |
| 工具箱副本（含 R1 退役的名字） | 0 | `scripts/style-check.sh` |
| 手写 `enter_realm` | 只在 `src/world.rs` | `scripts/style-check.sh` |
| 嵌套层级 | 3 | 审查 |
| PR 净增（不含测试与夹具） | ~1500 行 | 审查 |

## 10. 机器检查

合并前必须全部通过（CI：`.github/workflows/style.yml`）：

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
scripts/style-check.sh            # 文件长度棘轮、工具箱副本、禁用模式、Realm 切换
```

新 crate（`verve-electron`、`verve-gpui`、`verve-layout`）在 `Cargo.toml` 写 `[lints] workspace = true`，
使用根 `Cargo.toml` 的 `[workspace.lints]`（更严格：`unwrap_used`、`too_many_lines`、`dbg_macro`、`todo` 等）。
老 crate（`verve`、`verve-dom`）在 R2/R4 关卡清理后再接入。集成测试文件（`tests/*.rs`）开头可写
`#![allow(clippy::unwrap_used)]`——测试里 `unwrap` 就是断言。
