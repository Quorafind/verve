# 合并前审查清单

每个工作包 PR 开出之前，总控按这份清单审查；不过的退回实现者修改。目的只有一个：**代码库不变成屎山**——
每次合并之后，全仓库仍然像一个人写的。

## 1. 机器检查（必须全绿）

```sh
cargo fmt --all --check
bash scripts/style-check.sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p <改动的 crate>          # 总控合并前再跑全量
```

分支先 rebase 到最新的基线（`docs/electron-plan`，合并后是 `main`），再跑。

**每个提交都要单独通过**格式与风格检查（提交钩子本应保证；rebase、`reset --soft` 重组时会绕过它）：

```sh
git rebase --exec 'cargo fmt --all --check && bash scripts/style-check.sh' <基线>
```

**改了 `boa/` 的包**：`boa/` 不是 workspace 成员，上面的命令管不到它。逐提交检查加上 `&& (cd boa && cargo fmt --all --check)`，
并在 `boa/` 里跑 `cargo clippy -p <改动的 boa crate> --all-targets -- -D warnings`。

**带 feature 开关的代码**（`jit`、`wasm` 等）：CI 的 clippy 只跑默认 feature，再用 `--features <开关>` 跑一次 clippy 与测试。

**含平台分支（`cfg(windows)`/`cfg(unix)`）的包**：本地只能编一个平台，合并前必须等 PR 上 CI 的 Linux `style`（clippy）任务通过——只在一个平台用到的字段、变体会在另一个平台被判为死代码。 推送前先在本机跑 `bash scripts/linux-check.sh`（Zig 做 C 编译器，在 Windows/macOS 上也能做 Linux 的 clippy），不必等 CI。

历史要成组、可读：每个提交一件事、能编译；"修上一个提交"的提交折叠进去；同一主题不拆成两个同名提交。

## 2. 一个工具箱

- [ ] 没有新的 `arg`/`js_text`/`type_error`/`get`/`array`… 副本（`style-check` 已拦），也没有**换了名字的副本**
      （例如 `fn str_arg`、`fn make_obj`）——人工看新增的小辅助函数，凡是和 `verve::native` 已有功能重叠的都退回。
- [ ] 没有自己的 base64、URL/percent 解码、结构化克隆、EventEmitter 实现。
- [ ] 两个以上包需要的辅助，放进共享位置（`verve::native`、`verve-electron/src/api/define.rs`），不各写一份。

## 3. 一致性

- [ ] 命名符合 [`style.md`](style.md) §2（`context`/`cx`、`is_*`、无 `get_*`、ID 用类型）。
- [ ] 模块以 `//!` 文档开头，说清是什么、照谁的行为；公开项有文档注释；注释是英文陈述句。
- [ ] JS 可见 API 用方法表定义，实现函数只做"取参 → 调有类型的 Rust 函数 → 包装结果"。
- [ ] 错误是 Node/Electron 的形态（`code`、消息）；不支持的 API 抛 `ERR_NOT_SUPPORTED`。
- [ ] 跨线程只用 P02 的消息类型。

## 4. 精炼

- [ ] 新文件 ≤ 1500 行，函数 ≤ 80 行，嵌套 ≤ 3 层；老的超长文件没有变长。
- [ ] 无死代码、无注释掉的代码、无 `#[allow(dead_code)]` 掩盖、无"以后用"的参数和泛型。
- [ ] 同一段逻辑没有出现两次（含测试代码：重复的夹具抽到 `tests/support`）。
- [ ] 新依赖有用途注释、关了不需要的 default features，体积增量写在 PR 里。

## 5. 范围与契约

- [ ] 只改了任务书「修改」允许的文件；越界的改动单独 commit 并说明理由（如 P23 改 `src/dom_events.rs`）。
- [ ] 契约变更有单独的 docs commit，并同步到 `P02` 汇总表或相关包文档。
- [ ] 「状态」一节写清：完成/未完成、哪些只 check 过、给其他包的备注。

## 6. 审查方式

总控对每个包派一个**只读审查子代理**（与实现者不同的模型优先），输入：本清单、`style.md`、任务书、
`git diff <基线>..<分支>`。它只报告问题（文件:行、违反哪条、建议改法），不改代码；问题汇总后发回实现者修改，
改完复查一次。审查结论写进 PR 描述。
