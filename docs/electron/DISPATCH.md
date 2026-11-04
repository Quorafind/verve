# 派发模板

把下面这段发给实现者（DeepSeek），只替换 `<PXX>` 和包文件名。

---

你在 Verve 仓库（`F:\dev\sandbox-v2\verve`，Rust，Boa 引擎上的 JS 运行时）里实现一个工作包。
项目目标：让 Verve 能直接替换 Electron——一切 Electron/Node/Web API 都是 Rust 原生函数，DOM 由
`crates/verve-dom` 持有，GPUI 负责布局绘制；最终二进制约 32 MB（含 JIT，硬上限 34 MB）。

请按顺序阅读：
1. `docs/electron/README.md`（总体方案、原则、架构、里程碑）
2. `docs/electron/conventions.md`（分支、代码、测试、commit 约定——必须遵守）
   和 `docs/electron/style.md`（代码风格：一个工具箱、命名、文件结构、注释、规模上限——必须遵守）
3. `docs/electron/packages/P02-skeleton-and-contracts.md`（线程与消息契约）
4. `docs/electron/packages/<PXX-文件名>.md`（你的任务书）
5. 任务书「背景（先读）」里列出的每个源文件

然后：
- 从最新 `main` 切分支 `electron/<pxx-短名>`。
- 严格按任务书的「接口契约」实现；契约确需修改，先单独提交一个只改 `docs/electron/` 的 commit 并说明原因。
- 只改任务书「修改」一栏允许的范围。
- 完成「验收」里的每一条，贴出命令和输出；贴 `scripts/size-report` 的体积增量（P01 合并后）。
- 在任务书末尾「状态」一节写：完成了什么、没完成什么、已知问题、给其他包的备注。
- 代码必须精炼且与全仓库风格一致：遵守 `style.md` 与 `conventions.md`「精炼」；只用 `verve::native` 这一套工具，缺的加进去，不写副本。
- 结束前自查：`cargo fmt --all --check`、`bash scripts/style-check.sh`、`cargo clippy -p <crate> --all-targets -- -D warnings` 全部通过。合并前总控按 `review.md` 审查，不过的会退回。
- 不确定 Electron 的真实行为时，以 Electron 39 官方文档为准，并在 PR 里注明依据；拿不准就把问题列出来，不要猜着实现。

---

## 派发批次建议

| 批次 | 可同时派发 |
|---|---|
| 1 | P02、P01、P03、P22、P23、P25 |
| 2（P02 合并后） | P00、P10、P11、P12、P14、P17、P20、P34（P30 的 spike 已完成：可行） |
| 性能 A（与批次 2 同时） | P60、P61、P62、P63 |
| — | 小重构：批次 2 合并约 3 个后插入（如 P10+P12+P13 之后收拢 API 样板） |
| **R1** | M0+M1 重构关卡（通过后再派发批次 3 的渲染器部分） |
| 3 | P13、P21、P30、P24、P43 |
| 性能 B（P60、P61 合并后，与批次 3 同时） | P64、P65、P66（P66 的调用帧部分在 P65 合并后再合）；P67 在 P62 合并后即可派发 |
| 性能 C（P64–P67 合并后） | P68、P69、P70、P71、P72，之后 P73、P74、P75（P69 与 P70 都碰 `vm/call_frame/` 与 `vm/jit/`，P72 也碰 `vm/jit/`：按文件分开，冲突时后合并者 rebase） |
| **R2** | M2 重构关卡 |
| 4 | P31、P32、P33、P35、P15、P16 |
| **R3** | M3 重构关卡 |
| 5 | P40、P42、P41（持续）|
| **RP** | 性能复盘：P41 跑起 L2 应用且 P64–P66 合并后（见 README「性能线」） |
| **R4** | 发布前全仓库重构 |
| 之后 | P50 |

同一批里改同一文件的包：P10/P13 都会碰 `crates/verve-electron/src/api/`，按模块分文件即可，冲突时后合并者 rebase。
P61、P64、P65、P66 都改 `boa/core`：范围已在各自任务书里按目录分开；都要追加 `boa/SU.md`，冲突时后合并者 rebase。
性能线的包要做 release 测量，同时派发时错开测量时段，避免互相干扰（见 `conventions.md`「构建纪律」）。

## 重构 PR 的派发模板

你在 Verve 仓库做一次**只重构、不加功能**的整理。范围：<区域/包列表>。先读 `docs/electron/conventions.md` 的「精炼」「重构节奏」。
要求：行为不变（现有测试不改，只可新增）；找出这些包之间的重复样板、不一致命名、过长函数和文件、死代码并收拢；
PR 描述给出删减行数、合并了哪些重复、`cargo test --workspace` 结果和 `scripts/size-report` 前后对比。分支 `electron/refactor-<区域>`。
