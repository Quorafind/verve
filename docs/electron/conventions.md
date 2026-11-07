# 实现约定（给每个工作包的实现者）

## 工作方式

- **一个包一个分支一个 PR**：分支名 `electron/pXX-短名`（如 `electron/p12-ipc`），从最新 `main` 切。
  不要在一个 PR 里顺手改别的包的范围；发现别的包需要的改动，在 PR 描述的"给其他包的备注"里写出来。
- 动手前先读：本目录 `README.md`、`P02` 的契约、你的包文档、包文档里列出的现有文件。
- 包文档里的"接口契约"是**约定**，不是建议。确实需要改契约时，先在 PR 里单独提交一个只改
  `docs/electron/` 的 commit 说明原因，再改代码。
- 每个包完成时，更新 `docs/electron/packages/` 里自己那份文档末尾的「状态」一节：做了什么、
  没做什么、已知问题、体积增量。

## 代码

**完整的风格规则见 [`style.md`](style.md)，合并前按 [`review.md`](review.md) 审查。** 下面是要点。

- Rust edition 2024，toolchain 与仓库一致（Rust 1.95）。`cargo clippy --workspace --all-targets` 无警告，
  `cargo fmt` 过。
- **不写 JS 垫片**。所有 JS 可见的东西用 `src/native.rs` 的工具（`class`、`function`、`closure`、
  `object`、`get`、`hidden`、`ClassSpec`…）做成原生函数。要继承 `EventEmitter` 的类，照
  `src/sockets.rs` / `src/network/tcp.rs` 的写法（`crate::events::emit`），并保证
  `class X extends BrowserWindow` 和 `util.inherits` 两种继承都成立。
- 注释风格照仓库现有：说"它是什么、为什么"，用完整的句子，不写"TODO: maybe"式自言自语。
  公开项都有文档注释。
- 错误：与 Electron/Node 一致的类型、`message` 和 `code`。参数校验错误用 `TypeError` 加
  `ERR_INVALID_ARG_TYPE`。
- 不支持的功能：对象存在，调用时抛 `Error("<api> is not supported by Verve")`，`code: 'ERR_NOT_SUPPORTED'`。
- 线程：遵守 P02 的单向同步规则。GPUI 线程上不跑 JS；JS 线程上不调用 GPUI。
- `boa/` 是 Boa 子树。确实要改引擎时单独 commit，并在 `boa/SU.md` 记一条；尽量不改。
- `src/`（verve 根 crate）只允许包文档里明确写了"修改 src/…"的包去动。
- 新依赖：`Cargo.toml` 里每个依赖上方一行注释写用途（仓库现有风格）；关掉不需要的 default features；
  PR 描述里写 release 体积增量（`scripts/size-report`，P01 提供）。

## 精炼

代码要短而清楚。评审时以下任一条不满足即退回：

- **不重复**：第二次写出相似的代码就抽出来。Electron 的几十个 EventEmitter 类、几百个"参数校验 + 发消息"的方法，
  必须用共享的辅助（如一个声明式的方法表/宏）生成，不允许每个方法手写一遍同样的样板。P02 要先给出这套辅助，后续包照用。
- **函数短**：单个函数超过 ~60 行、嵌套超过 3 层，就拆。一个文件超过 ~1500 行，按职责拆模块。
- **没有死代码**：不留注释掉的代码、未用的参数、"以后可能用到"的抽象和 feature；不加没人调用的公开 API。
- **类型表达约束**：能用 enum/newtype 表达的状态不用 bool 组合和字符串比较；不用 `unwrap()` 糊过真实可能出错的路径。
- **注释写为什么**：不复述代码；一个公开项一段文档注释就够，不写长篇。
- **依赖克制**：标准库或已有依赖能做的不加新 crate（同时是体积要求）。
- **PR 小**：一个 PR 只做一件事，建议净增 ≤ 1500 行（不含测试与夹具）；更大的拆成多个 PR 顺序合并。

## 重构节奏

重构是计划内的工作，不是"有空再说"：

- **每个 PR**：顺手清理自己碰到的代码（重命名、去重、删死代码），但不改无关模块。
- **每合并约 3 个功能 PR**：在同一区域安排一个**只重构不加功能**的 PR（行为不变、测试不改或只增），目标是把刚合进来的
  几个包之间的重复和不一致收拢。由总控在派发时插入（见 `DISPATCH.md` 的重构批次）。
- **每个里程碑结束**：一次重构关卡（`R1`–`R4`，见 `README.md`）。关卡不过，下一里程碑的包不派发。
- 重构 PR 的描述必须给出：删了多少行、合并了哪些重复、`cargo test --workspace` 与体积前后对比。

## 提交钩子

仓库的 `core.hooksPath` 指向 `scripts/hooks`：每次 commit 自动跑 `cargo fmt --all --check` 和 `scripts/style-check.sh`，
不过就提交不了。**不许用 `--no-verify` 绕过**；钩子拦下了就按提示修。

## 构建纪律（多个实现者共用一台机器）

- **先写，少编译**：一块完整功能写完再验证，不要每改几行就编译。
- **验证首选 `cargo check -p <crate>`**（加 `--tests` 检查测试代码），不用 `cargo build`。
- 不跑 release 构建、不跑全量 `cargo test --workspace`；测试在 check 通过后用 `-p <crate> --test <name>` 缩小范围跑。
- 全量测试与 release 体积测量由总控在合并前统一跑；「状态」里写明哪些验收是 check 过的、哪些测试实际跑过。
- `CARGO_BUILD_JOBS` 由总控设置，不要自行调大。
- **性能线例外**（P60–P67，以及 P42）：测量需要 release 构建，允许 `cargo build --release --bin verve`（或任务书写明的 bin）与 `--profile profiling`。
  一轮改动写完再构建一次、测完再改，不要每改一处就 release 构建；测量时机器上不要有自己的其他构建在跑，报告里写明测量时的负载。
- 含平台分支（`cfg(unix)`/`cfg(windows)`）的包，报告完成前跑 `bash scripts/linux-check.sh`（需要 `rustup target add x86_64-unknown-linux-gnu` 与 Zig ≥0.15）。

## 测试

- 单元/集成测试放在所在 crate 的 `tests/`。Electron 行为的端到端验收用 P00 的无头夹具：
  `cargo test -p verve-electron --test <name>`，不需要显示器、不需要 GPU。
- 每个对外 API 至少一个测试；行为以**真实 Electron 39** 为准。拿不准时在 PR 里附上用真实
  Electron 跑出的结果（脚本 + 输出），测试里把期望值冻结下来（参考 `tests/network.rs` 冻结
  Node 观测值的做法）。
- 环境变量 `VERVE_NO_SYSTEM_DIALOGS` 的意思是“Shell 不碰桌面”：不弹系统对话框，不建原生菜单、托盘图标和全局快捷键。
  跑真实 Shell 的测试（`verve-gpui` 的二进制测试）都要设它；碰桌面的测试标 `#[ignore = "touches the desktop"]`。
- 需要联网或 npm 的测试标 `#[ignore]`，并在包文档里写如何准备夹具。
- 完成前必须自查通过：`cargo fmt --all --check`、`bash scripts/style-check.sh`、`cargo clippy -p <你的 crate> --all-targets -- -D warnings`，
  以及你的包列出的验收命令；全量 `cargo test --workspace` 由总控在合并前跑。

## Commit

Conventional Commits，scope 用 crate 或模块：`feat(electron): …`、`fix(dom): …`、`feat(gpui): …`、
`build: …`、`docs(electron): …`。英文，祈使句，首字母大写（仓库现有风格，如
`fix(dom): Deduplicate the merged font shorthand test`）。
