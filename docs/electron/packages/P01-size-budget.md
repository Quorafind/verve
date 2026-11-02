# P01 体积预算与 release 配置

**依赖**：无  **规模**：S  **修改**：根 `Cargo.toml`（profile、features）、新增 `scripts/size-report.*`、`docs/electron/size.md`

## 目标

让 `electron.exe` 能落在 **≈ 20 MB（硬上限 24 MB）**。先量出现状、找出大头，再用 release 配置和
feature 开关把不需要的东西拿掉，并给之后每个包提供一个"体积增量"脚本。

现状：`E:\cargo-target\verve\release\verve.exe` = **35.8 MB**（2026-10-25 构建，未开 LTO、未 strip，不含 GPUI）。
根 `Cargo.toml` 里**没有 `[profile.release]`**。

## 背景（先读）

- 根 `Cargo.toml`：依赖清单与 features（`npm`、`jit`、`code-cache-env`）。
- README「Same-input build and binary observations」一节：测量写法，照着记录。
- `src/isolate.rs`：isolate 用 `catch_unwind` 让 panic 只结束自己 → **不能用 `panic = "abort"`**。

## 步骤

1. **量现状**：`cargo install cargo-bloat`；对 `verve` bin 跑
   `cargo bloat --release --crates -n 40` 和 `cargo bloat --release -n 60`，结果写进 `docs/electron/size.md`。
2. **release profile**（根 `Cargo.toml`）：
   ```toml
   [profile.release]
   lto = "fat"
   codegen-units = 1
   strip = "symbols"
   opt-level = 3          # 解释器热路径要 3
   [profile.release.package."*"]
   opt-level = "s"        # 其余依赖按体积优化
   [profile.release.package.boa_engine]
   opt-level = 3
   # boa_parser / boa_gc / boa_string / regress / wasmi 同样 3（与 dev 段一致）
   ```
   另设 `[profile.dist]`（inherits release）供 P40 出包用；不同组合都量一遍，表格记录：
   `opt-level` 3 vs "s" 对 Boa 的体积与速度（用 `examples/jit_probe.rs` 和一个 JS 基准，速度回退 >10% 不接受）。
3. **feature 开关**（默认 features 保持现状，Electron 构建选择性关闭）：
   | feature | 包住什么 | Electron 默认 |
   |---|---|---|
   | `npm`（已有） | ureq、tar、base64 | 关 |
   | `typescript`（新） | oxc_*（`module.stripTypeScriptTypes`）；关闭时该函数抛 `ERR_NOT_SUPPORTED` | 关 |
   | `wasm`（新） | wasmi；关闭时 `WebAssembly` 不存在 | 开（量一下大小再定） |
   | `dns-records`（新） | hickory-resolver（`resolve4/6/Txt/Srv`）；关闭时只留 `lookup` | 关 |
   | `x509`（新） | x509-parser（`getPeerCertificate` 详情）；关闭时返回最小信息 | 关 |
   | `brotli`（新） | brotli 编码器；解码器保留（fetch 要 `br`） | 量了再定 |
   | `intl-full`（新） | `src/intl/` 的大表（若有） | 量了再定 |
   | `jit`（已有） | Cranelift | 量了再定（性能与体积权衡，交给 P42 决定默认） |
   每关一个都要保证 `cargo test --no-default-features --features <electron 组合>` 能编译、相关测试被 `cfg` 掉而不是失败。
4. **脚本** `scripts/size-report.ps1` 和 `scripts/size-report.sh`：构建指定 bin + features 的 release，打印
   字节数、与 `docs/electron/size.md` 里记录的基线之差、`cargo bloat --crates` 前 20。之后每个 PR 贴它的输出。
5. GPUI 引入（P02 合并）后，再量一次 `electron` bin，更新预算表：

   | 部分 | 预算 |
   |---|---|
   | Boa + Verve 核心（Node/web API、网络、TLS） | ≤ 10 MB |
   | verve-dom | ≤ 2 MB |
   | GPUI + 平台层 | ≤ 6 MB |
   | Electron API + 系统集成 crate（rfd/muda/tray-icon/arboard/image…） | ≤ 2 MB |
   | **合计** | **≈ 20 MB** |

   初始预算是估计值；实测后在 `size.md` 修订，并标出超支项的处理方案（换更小的 crate、按需加载、feature 关闭）。
6. CI：加一个检查，`electron` release 体积超过 24 MB 就失败（先只在 Windows x64）。

## 验收

- `docs/electron/size.md` 有：现状数据、每个开关的增减表、profile 对比表（体积 + 速度）、最终推荐组合。
- `verve` bin 在推荐组合下 release 体积相对 35.8 MB 显著下降（目标 ≤ 14 MB，作为 GPUI 之前的核心预算）。
- 全量 `cargo test`（默认 features）不受影响。

## 不做

UPX 之类的可执行压缩（会触发杀软误报、拖慢启动）；改动任何运行时代码逻辑（除了 `cfg` 开关）。

## 状态

（实现者完成后填写）
