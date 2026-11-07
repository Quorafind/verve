# P01 体积预算与 release 配置

**依赖**：无  **规模**：S  **修改**：根 `Cargo.toml`（profile、features）、新增 `scripts/size-report.*`、`docs/electron/size.md`

## 目标

让 `electron.exe` 能落在 **≈ 20 MB（硬上限 24 MB）**（**已修订**：2026-11-02 定为硬上限 34 MB、JIT 默认开启，见 README「目标」）。先量出现状、找出大头，再用 release 配置和
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
6. CI：加一个检查，`electron` release 体积超过 **36 MB**（`--max-bytes 37748736`；2026-11-07 前是 34 MB / 35651584）就失败（先只在 Windows x64）。

## 验收

- `docs/electron/size.md` 有：现状数据、每个开关的增减表、profile 对比表（体积 + 速度）、最终推荐组合。
- `verve` bin 在推荐组合下 release 体积相对 35.8 MB 显著下降（目标 ≤ 14 MB，作为 GPUI 之前的核心预算）。
- 全量 `cargo test`（默认 features）不受影响。

## 不做

UPX 之类的可执行压缩（会触发杀软误报、拖慢启动）；改动任何运行时代码逻辑（除了 `cfg` 开关）。

## 状态

做了（测量过的以 size.md 为准）：

- `[profile.release]`：fat LTO、1 个 codegen unit、`strip = "symbols"`、`panic` 保持 unwind；
  根 crate 和依赖用 `opt-level = "s"`，Boa 各 crate、regress、wasmi、cranelift/regalloc2 用 3；
  `[profile.dist]` 继承 release。实测：全 3 为 20.76 MB，Boa 也用 "s" 为 18.50 MB 但慢 24%（拒绝），
  最终 17.77 MB、慢约 5%（解释器基准，限制 10%）。与简报给的写法不同：简报让根 crate 用 3，
  实测 "s" 省 2.6 MB，所以改了。
- feature：`typescript`、`wasm`、`dns-records`、`x509`、`brotli`（均在默认里，默认行为不变）；`npm`、`jit` 已有。
  关闭时 API 保留并报 `ERR_NOT_SUPPORTED` / `ENOSYS`（`wasm` 则没有 `WebAssembly` 全局）。
  已用 `cargo check --tests` 检查默认、`--no-default-features`、`wasm`、`jit,wasm,brotli`、`typescript`、
  `dns-records,x509,brotli,npm` 几种组合；关闭 feature 时对应测试被 `cfg` 掉。没有跑全量 `cargo test`（交给总控）。
- `scripts/size-report.ps1` / `.sh`、`docs/electron/size.md`（基线 35,812,352 字节）。
- 前一位实现者的 feature 门控沿用，只调整了 profile；`brotli` 只能挡住编码器（解码器是同一个 crate 之外的
  brotli-decompressor，fetch 需要）。`fetch.rs` 的一个单元测试把 brotli 编码出的字节改成冻结值，
  这样 `brotli` 关闭时也能测解码。

没做 / 与简报不一致：

- 核心（Electron 组合，无 GPUI、无 JIT）是 17.77 MB，没有达到 ≤ 14 MB 的目标，预算表（核心 ≤ 10 MB）需要在 P02 之后重估。
  剩余大头：boa_engine/boa_parser 约 5 MiB、`std` 2.4 MiB、`.rdata` 4.4 MiB（未归因）、wasmi 0.7 MiB。
- `jit` 在体积上 +5.9 MB（实测，当时 20.38 → 26.31 MB），默认是否开交给 P42。
- `intl-full` 没做：`src/intl/` 只有 81 KB，Boa 没开 `intl`，没有可关的大表。
- 步骤 5（GPUI 之后重量 `electron` bin）和步骤 6（CI 体积检查）没做：`electron` bin 还不存在。脚本已有
  `--max-bytes` / `-MaxBytes`，等 bin 出现后在 CI 里加一行（36 MB 上限 = 37748736，2026-11-07 起，见 README「目标」）即可。
- 没有逐个 feature 单独构建；每个开关的体积是默认构建的 cargo-bloat `.text` 估计值。
- 注意：目录名 `default.exe` 在这台机器的 Git Bash 下无法启动（退出码 127），与 Verve 无关。
