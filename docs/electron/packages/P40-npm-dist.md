# P40 npm 包与发行布局

**依赖**：P10（启动器能按 Electron 的规则找到应用）  **规模**：M  **修改**：新建 `crates/verve-electron-npm/`（npm 包模板与脚本，非 Rust）、`crates/verve-gpui/build.rs`（或 bin 所在 crate 的 build.rs：Windows 资源与 fuse 段）、`.github/workflows/electron-release.yml`（需新增）

## 目标

让 Verve 的 `electron` 能通过三条路径进入一个现有 Electron 项目，**应用代码和打包配置尽量不改**：

| 路径 | 用户做什么 | 适用 |
|---|---|---|
| A. npm 别名 | `npm i -D electron@npm:@verve/electron` | 开发期 `npx electron .`、electron-vite、forge start |
| B. 下载镜像 | 保留官方 `electron` 包，设 `ELECTRON_MIRROR` / `ELECTRON_CUSTOM_DIR` 指向 Verve 的 GitHub Release | 不想改 package.json；forge/packager/builder 下载 zip 的流程 |
| C. 直接替换 | 把已安装应用目录里的 `<App>.exe` 换成 Verve 的 `electron.exe`（改名） | 试用、对比、兼容排查 |

发行目录（`dist/`）与官方 Electron 的布局一致，使 electron-builder、electron-forge、`@electron/packager`
把它当成一个 Electron 发行包处理。

## 背景（先读）

- 官方 npm 包 `electron@39.2.0` 的源码（`npm pack electron@39.2.0` 解开看）：`index.js`、`cli.js`、
  `install.js`、`electron.d.ts`、`checksums.json`、`package.json`。本包逐项对照它实现。
- P10 文档：启动器找应用的顺序（`resources/app.asar` → `resources/app/` → `resources/default_app.asar`、
  命令行给出的路径）。路径 C 完全依赖它。
- `ci/macos/allow-jit.entitlements`、`ci/macos/jit-probe.sh`：macOS 上 JIT 需要的签名授权与验证方法。
- 仓库 README「Installed compilation targets」：已验证过的交叉编译目标与 cargo-zigbuild 用法（只到 `cargo check`，
  链接 GPUI 需要在各平台原生 runner 上构建）。
- P01：release 配置与体积脚本；本包的 CI 产物体积由它的脚本报告。

## 接口契约

### 1. npm 包 `@verve/electron`（`crates/verve-electron-npm/electron/`，需新增）

文件与官方同名同义：

| 文件 | 行为（与官方一致之处 / 差异） |
|---|---|
| `package.json` | `"name": "@verve/electron"`，`"main": "index.js"`，`"types": "electron.d.ts"`，`"bin": { "electron": "cli.js" }`，`"scripts": { "postinstall": "node install.js" }`；`optionalDependencies` 列全部平台包（见 2），版本精确锁定；**不依赖** `@electron/get` / `extract-zip`。保留 `@types/node` 依赖（官方有）。 |
| `index.js` | 读同目录 `path.txt`，返回 `path.join(__dirname, 'dist', <path.txt 内容>)`；若设了 `ELECTRON_OVERRIDE_DIST_PATH`，返回 `path.join(ELECTRON_OVERRIDE_DIST_PATH, <path.txt 内容 或 平台默认名>)`。`path.txt` 缺失时抛官方同样的错误文本：`Electron failed to install correctly, please delete node_modules/electron and try installing again`。 |
| `cli.js` | `#!/usr/bin/env node`；`spawn(require('./'), process.argv.slice(2), { stdio: 'inherit', windowsHide: false })`；转发 `SIGINT`、`SIGTERM` 给子进程；子进程 `close` 时：`code === null` → 打印 `${electron} exited with signal ${signal}` 并 `process.exit(1)`，否则 `process.exit(code)`。与官方 cli.js 逐行对照。 |
| `install.js` | 不下载。按 `process.platform`/`process.arch`（尊重 `npm_config_platform`、`npm_config_arch`，官方也认）找已安装的平台包 `@verve/electron-<platform>-<arch>`，把它的 `dist/` **硬链接或复制**到本包 `dist/`（esbuild 式：优先直接引用平台包路径，`path.txt` 写相对平台包的路径，可免复制——两种方案选一，写回本文），写 `path.txt`。`ELECTRON_SKIP_BINARY_DOWNLOAD=1` 时什么也不做（官方语义）。找不到平台包时报错并给出 `npm i --include=optional` 提示。 |
| `electron.d.ts` | **原样**取官方 `electron@39.2.0` 的类型定义（MIT），文件头加一行注释说明来源与版本。不手写类型。 |
| `checksums.json` | 平台包内二进制的 SHA-256，install.js 校验。 |
| `LICENSE` | Verve 许可 + 官方 electron.d.ts 的 MIT 声明。 |

`path.txt` 内容与官方一致：Windows `electron.exe`；macOS `Electron.app/Contents/MacOS/Electron`；Linux `electron`。

### 2. 平台包

`@verve/electron-win32-x64`、`-win32-arm64`、`-darwin-arm64`、`-darwin-x64`、`-linux-x64`、`-linux-arm64`。
每个包 `package.json` 带 `"os"`、`"cpu"` 字段（npm 据此只装匹配的），内容只有 `dist/`。

### 3. 版本策略

- npm 版本：`39.2.0-verve.N`（N 从 1 递增）。前缀 `39.2.0` 表示**对齐的 Electron API 版本**；不用 `+build`
  元数据（npm 视 `39.2.0+a` 与 `39.2.0+b` 为同一版本，无法重复发布）。
- `dist/version` 文件内容：`39.2.0`（与 `process.versions.electron` 一致，**不带** `-verve.N`），因为
  electron-builder / packager 会读它或 package.json 的版本去拼下载文件名、判断特性。
- 二进制内另有 `process.versions.verve = '<verve 版本>+<git 短哈希>'`（P10 加；本包只要求 CI 注入
  `VERVE_BUILD_COMMIT` 环境变量）。
- 文档里要求用户在 electron-builder 配置写 `electronVersion: "39.2.0"`（预发布版本号会让它误判）。

### 4. `dist/` 布局

**Windows**（`electron-v39.2.0-verve.N-win32-x64.zip` 解开即此）：

```
electron.exe            Verve 的 GUI 可执行文件，带 VERSIONINFO 与图标资源（见 6）
resources/
  default_app.asar      Verve 自带的默认应用（没有给应用路径时显示的欢迎页；P10 定义内容，可极简）
version                 "39.2.0"
LICENSE                 Verve 许可
LICENSES.chromium.html  空占位（builder 某些版本会复制它；内容写明"Verve 不含 Chromium"）
locales/en-US.pak       空占位（见 5 的 electronLanguages）
```

**不放** `ffmpeg.dll`、`libEGL.dll`、`icudtl.dat`、`*.pak`、`v8_context_snapshot.bin`、`snapshot_blob.bin` 等
Chromium 文件——除非 5 的核实结果证明某个工具硬性要求它，那时放 0 字节占位并在本文登记。

**macOS**：

```
Electron.app/Contents/
  Info.plist            CFBundleExecutable=Electron, CFBundleIdentifier=com.github.Electron,
                        CFBundleName=Electron, CFBundleShortVersionString=39.2.0, NSHighResolutionCapable 等
  MacOS/Electron        Verve 可执行文件
  Resources/default_app.asar, Resources/electron.icns
  Frameworks/           见 5：Helper 应用占位的取舍
version, LICENSE
```

**Linux**：`electron`、`resources/default_app.asar`、`version`、`LICENSE`、`locales/en-US.pak`（占位）。
`chrome-sandbox` 不提供（builder 的 deb 安装脚本对它是 `chmod … || true`，缺失不失败——需核实）。

### 5. 打包工具的硬性假设（必须逐条实测，结论写回本表）

用**锁定版本**实测：electron-builder 26.x、@electron/packager 18.x、electron-forge 7.x（实施时取最新稳定版并写明）。

| 工具行为 | 假设 | 应对 |
|---|---|---|
| builder `electronDist` 指向目录 | 直接复制目录，改 `electron.exe` 为 `<productName>.exe`，删 `resources/default_app.asar`，放入 `app.asar` | 无需额外处理；验证 |
| builder Windows：用 rcedit 写图标、`FileDescription`、`ProductVersion` 等 | exe 必须是带资源段的标准 PE；rcedit 对无 VERSIONINFO 的 exe 可能失败 | 6：构建时嵌入完整 VERSIONINFO + 图标组 |
| builder/packager `electronLanguages` / `--prune` 语言 | 遍历 `locales/` 删除不需要的 `.pak` | 提供 `locales/en-US.pak` 占位；验证目录缺失时是否报错 |
| builder `electronFuses` / forge `FusesPlugin`（`@electron/fuses`） | 在二进制中搜索 fuse 哨兵字符串 `dL7pKGdnNz796PbbjQWNKmHXBZaB9tsX`，后跟版本字节和 fuse 线格式，原地翻转；找不到即报错 | 7：在二进制里放同格式的 fuse 段，并让启动器**读取并遵守**这些 fuse |
| builder macOS：改 `Info.plist`、重命名 `Electron.app`、`MacOS/Electron`，处理 `Frameworks/Electron Helper*.app` 与 `Electron Framework.framework` | 若缺少 Helper 应用/Framework，重命名步骤可能报错 | 实测；若报错，提供**最小占位 Helper 应用**（只含 Info.plist 与一个退出码 0 的极小可执行文件），并在本文记录体积成本 |
| builder/forge ASAR 完整性（`ElectronAsarIntegrity`：macOS 写 Info.plist，Windows 写 `INTEGRITY/ELECTRONASAR` 资源） | 启用 `EnableEmbeddedAsarIntegrityValidation` fuse 时运行时应校验 | 与 P11 协作：P11 读取并校验；本包保证资源段可写入 |
| packager/forge 通过 `@electron/get` 下载 `electron-v{version}-{platform}-{arch}.zip` 并校验 `SHASUMS256.txt` | 文件名与校验文件格式必须与官方一致 | 8：Release 资产按官方命名，附 `SHASUMS256.txt` |
| forge `packagerConfig.electronZipDir` | 目录内放上述命名的 zip，跳过下载 | 文档给出示例 |
| `@electron/rebuild` 读取 Electron 版本下载 headers | 用 `39.2.0` 下载官方 headers | 无影响（v1 不支持原生模块，见 P50）；文档说明 |

### 6. Windows 资源（build.rs，需新增）

用 `winresource`（或 `embed-resource`，选体积更小、无运行时依赖的一个）在 build.rs 嵌入：
`VERSIONINFO`（`FileVersion`/`ProductVersion` = `39.2.0.0`，`FileDescription` = `Electron`，`ProductName` = `Electron`，
`CompanyName` = `Verve`，`OriginalFilename` = `electron.exe`，`InternalName` = `electron`，`LegalCopyright`）、
默认图标（`assets/electron.ico`，需新增，用 Verve 自己的图标）、应用清单（DPI 感知 PerMonitorV2、
`longPathAware`、Common Controls v6、`supportedOS` Win10）。子系统 `windows`（不弹控制台），但支持
`ELECTRON_ENABLE_LOGGING`/`--enable-logging` 时 `AttachConsole(ATTACH_PARENT_PROCESS)` 把输出接回父终端（与官方行为一致）。

### 7. Electron fuses

在可执行文件的只读数据段放与官方完全相同的 fuse 线格式（哨兵 + 版本 `1` + 长度 + 每个 fuse 一字节
`'0'`/`'1'`/`'r'`），默认值与 Electron 39 一致。启动器（P10）在 `main` 开头读自己的这段字节，遵守：

| fuse | Verve 的行为 |
|---|---|
| `RunAsNode` | 开：`ELECTRON_RUN_AS_NODE=1` 时等同 `verve run`（Node 模式）；关：忽略该变量 |
| `EnableCookieEncryption` | P16 的 cookie 落盘时用 safeStorage 加密 |
| `EnableNodeOptionsEnvironmentVariable` | 关时忽略 `NODE_OPTIONS` |
| `EnableNodeCliInspectArguments` | 关时 `--inspect*` 直接报错退出；开时按 P43 接受并警告 |
| `EnableEmbeddedAsarIntegrityValidation` | 交给 P11 校验 |
| `OnlyLoadAppFromAsar` | 开时只从 `app.asar` 加载应用，不看 `resources/app/` |
| `LoadBrowserProcessSpecificV8Snapshot`、`GrantFileProtocolExtraPrivileges` | 前者无意义，读取后忽略；后者交给 P16 的 `file://` 权限 |

必须防止 LTO 把这段数据优化掉（`#[used]` + `#[unsafe(link_section)]`，并用测试在 release 二进制里搜索哨兵）。

### 8. GitHub Release 资产

每个版本发布：6 个平台 zip（官方命名 `electron-v39.2.0-verve.N-<platform>-<arch>.zip`）、`SHASUMS256.txt`
（官方格式：`<sha256> *<filename>`）、`electron.d.ts`。路径 B 的用户配置：

```sh
ELECTRON_MIRROR=https://github.com/Quorafind/verve/releases/download/
ELECTRON_CUSTOM_DIR=electron-v{{ version }}
# 并把 package.json 里 electron 的版本写成 39.2.0-verve.N
```

需实测 `@electron/get` 对预发布版本号拼出的 URL 是否与上面一致，不一致就调整 tag 命名并写回本文。

### 9. 路径 C：替换已安装应用的使用说明（写进 `docs/electron/replace-installed-app.md`，需新增）

1. 备份应用目录。
2. 用 Verve 的 `electron.exe` 覆盖 `<App>.exe`（保留原名）；可选用 `rcedit` 把原 exe 的图标/版本信息拷过来。
3. 保留 `resources/app.asar` 和 `resources/app.asar.unpacked/`；Chromium 的 dll/pak 可以留着（Verve 不读），也可删。
4. 已知限制：`app.asar.unpacked` 里的 `.node` 原生模块在 v1 不可用（P50）；启用了 ASAR 完整性 fuse 的应用需要
   Verve 读取原 exe 的完整性资源（P11），否则用 `--verve-skip-asar-integrity` 调试开关（P10 加，打印警告）。
5. 回滚：恢复备份。

### 10. macOS 签名

- 内置 JIT 时（P01/P42 决定是否默认开启 `jit` feature），硬化运行时必须带 `com.apple.security.cs.allow-jit`
  （仓库 `ci/macos/allow-jit.entitlements`）。electron-builder 默认的 `entitlements.mac.plist` 已含该项，文档里提醒
  自定义 entitlements 的用户保留它。
- CI 用 ad-hoc 签名跑 `ci/macos/jit-probe.sh` 同款验证：签名后的 `Electron.app` 启动并编译热函数。
- 公证（notarization）由用户自己的打包流程负责；我们的 Release 资产只做 ad-hoc 签名（与官方 zip 一致，官方 zip 是签名过的——
  记录这一差异，评估是否需要 Developer ID 签名）。

### 11. CI 构建矩阵（`.github/workflows/electron-release.yml`，需新增）

| 目标 | runner | 说明 |
|---|---|---|
| `x86_64-pc-windows-msvc` | windows-latest | 主平台，跑全部无头测试 + 体积门禁 |
| `aarch64-pc-windows-msvc` | windows-latest 交叉（或 windows-11-arm） | 只构建 + 冒烟 |
| `aarch64-apple-darwin` / `x86_64-apple-darwin` | macos-14 / macos-13 | 构建、签名、jit 探针；可选再 `lipo` 出 universal |
| `x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu` | ubuntu-22.04 / ubuntu-22.04-arm | glibc 基线写明（以 runner 为准）；GPUI 依赖的 X11/Wayland 开发包在 workflow 里装 |

每个 job：`cargo build --release -p verve-gpui --bin electron` → 组装 `dist/` → 打 zip → 计算 SHA-256 →
无头冒烟（`electron tests/electron/apps/hello` 退出码 0，P00 的 hello 夹具）→ 体积报告（P01 脚本，超过硬上限即失败）。
汇总 job：生成 `SHASUMS256.txt`、组装 npm 主包和 6 个平台包、`npm publish --dry-run`（真正发布手动触发）。

## 步骤

1. 解开官方 `electron@39.2.0`，逐文件写出对照表，实现 `@verve/electron` 主包与平台包模板，`scripts/assemble-dist`（需新增，
   用 Rust 小工具或 `verve run` 脚本写，不依赖 Node 运行时之外的东西；构建机上用 Node 也可以，但要说明）。
2. build.rs：Windows 资源、清单、fuse 段；测试在 release 产物中找到哨兵与 VERSIONINFO（Windows 上读 PE 资源）。
3. 组装三平台 `dist/`；`default_app.asar` 由 P10 提供内容，本包负责打包（asar 写入器：可复用 P11 的格式代码）。
4. 第 5 节逐条实测，结论写回本文表格；按结论补占位文件或 Helper 应用。
5. 写 `replace-installed-app.md`；CI workflow；Release 资产命名与路径 B 实测。

## 验收

- 在一个 electron-quick-start 副本里：`npm i -D electron@npm:@verve/electron@<本地 tgz>` 后 `npx electron .` 打开窗口；
  `require('electron')` 在 Node 里返回 Verve 的可执行文件路径；Ctrl+C 结束 `npx electron .` 时子进程也结束且退出码正确。
- 同一项目用 electron-builder（`electronDist` 指向 Verve 的 `dist/`、`electronVersion: "39.2.0"`）产出 Windows NSIS 安装包与
  `win-unpacked/`，安装后应用启动；exe 的属性页显示应用自己的图标和版本（rcedit 生效）。配置了 `electronFuses` 时打包成功且
  运行时行为符合翻转后的 fuse。
- electron-forge（`packagerConfig.electronZipDir` 或路径 B 镜像）`make` 成功，产物可启动。
- 路径 C：把一个用 electron-builder 打包的示例应用（P41 L1 的应用）的 exe 换成 Verve 的，应用启动。
- CI 矩阵全绿；Windows x64 的 `electron.exe` 体积在 P01 的上限内。

## 不做

原生模块（P50）；自动更新服务端（`autoUpdater` 见 README 第 7 节）；Developer ID 签名与公证的证书管理；
Snap/Flatpak/MSIX 专门适配（只保证 builder 的通用流程能走通，失败项登记到 P41）。

## 状态

（实现者完成后填写）
