# 构建、发布与合规

## 1. 支持矩阵

### v1.0 GA

| 项目 | 承诺 |
| --- | --- |
| OS | Windows 10 22H2、Windows 11；CI 额外覆盖 Windows Server 2022/2025 runner |
| 架构 | x64 only |
| Rust target | `x86_64-pc-windows-msvc` |
| C/C++ ABI | MSVC-compatible x64 `cdecl` |
| .NET | `net8.0`，x64 process |
| Python | CPython 3.10+，x64；wheel 不依赖 CPython C ABI |
| Node.js | Node 22、24 x64，Node-API 9 baseline |
| 浏览器 | 不支持 |

**[已决定]** ARM64、x86、Linux 和 macOS 不属于 v1.0 支持面。代码必须保留 target module 和 native bridge 边界，但不得在没有硬件、打包、ABI 和四语言一致性测试时发布对应 binary。

### Phase 3 平台候选

| 平台 | 方向 | 当前允许的最高状态 | 发布边界 |
| --- | --- | --- | --- |
| Windows 10/11 x64 | v1 Tier 1 | Vision software/native `Passed` | `Hardware Unverified`；完整 Phase 3、ABI、四语言与 package 门禁前不发布 |
| Linux x64 | v1 正式目标方向 | Vision Build Candidate；按实际 build 标记 Passed/Build Unverified | serial、硬件、四语言与 package 未完成，不是完整 Linux SDK |
| macOS Apple Silicon arm64 | experimental source | Build/Hardware Unverified、Not Shipped | 无 binary/package；不承诺 Intel 或 universal |

该表的目标由 [ADR-0012](../decisions/0012-phase-3-vision-native-target.md) 管理，第一次冻结历史见
[ADR-0013](../decisions/0013-phase-3-vision-cross-platform-source-candidate-freeze.md)，当前证据状态由 NativePool admission
修复后的 [ADR-0014](../decisions/0014-phase-3-native-pool-admission-refreeze.md) 重新冻结，不改变 v1.0 只有
Windows Tier 1 的发布承诺。macOS 至少一次真实 arm64 compile/software gate 通过前，不实现完整 AVFoundation
backend 或合并大段平台专属生产代码。

### 平台扩展条件

新增 target 必须同时具备：serial/capture backend、OpenCV/Tesseract 依赖构建、四语言包（适用时）、硬件矩阵、关闭可中断性和 conformance。只“能够编译”不等于受支持。Vision source/build candidate 只能证明该域的
对应门禁，不能越级形成 SDK 支持行。

## 2. 工具链基线

| 层 | 基线 |
| --- | --- |
| Rust | `rust-toolchain.toml` 精确固定 `1.97.1` 与 `x86_64-pc-windows-msvc`、rustfmt、clippy；发布分支不跟随 channel 漂移 |
| C++ | Visual Studio Build Tools 2022、MSVC `14.44.35207`、C++20、Windows SDK `10.0.26100.0` |
| Build | 受控 CMake `4.3.3`、Ninja `1.13.2`、Cargo；统一由 CMake preset/xtask 编排，不维护四套 native build |
| Native deps | vcpkg scripts/registry `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`、tool `2026-07-13`；OpenCV `4.12.0#5`、Tesseract `5.5.2#0`、Leptonica `1.87.0#0` 及传递依赖由同一 registry baseline 解析 |
| .NET | .NET 8 SDK 最新 servicing；`dotnet pack` |
| Python | Python 3.10-3.14 test matrix、build 1.x、twine；wheel repair/inspection 工具 |
| Node | Node 22/24、npm、TypeScript、node-gyp/CMake.js 中选定一个 addon 构建入口 |
| Quality | rustfmt/clippy、clang-format/clang-tidy、cargo-deny、SBOM 和 license scanner |

`tools/windows_build_environment.json` 是当前 Windows 开发构建环境的结构化固定清单，记录 vcpkg
scripts/tool/registry、CMake、Ninja、7-Zip、直接 native ports 的版本、来源和 hash 证据；Rust、triplet、preset 与
OCR 分别由 fingerprint 中列出的 tracked 文件共同约束。升级任一固定输入都必须经过显式变更并重新运行 Setup。

### Windows 开发环境生命周期

Windows 构建流程分为两个职责：

```powershell
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Setup
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Verify
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Workspace
```

`Setup` 是可重跑的一次性在线准备步骤，安装并核验固定 Rust toolchain、受控 CMake/Ninja、vcpkg
scripts/tool/registry/native install tree、7-Zip 与测试 OCR 模型，然后写入包含 fingerprint、显式路径、版本、文件
hash、Cargo vendor tree hash 和 native tree hash 的 stamp。Setup 使用 `cargo vendor --locked` 把 lockfile 与全部
workspace manifests 对应的 crate sources 安装进环境。`Verify` 与 `Workspace` 不安装或下载；两者只接受当前
fingerprint 对应且未损坏的 stamp，缺失或失配时要求重新运行 Setup。`Workspace` 在 Verify 后运行完整仓库门禁。

同一 fingerprint/worktree identity 使用环境目录外的跨进程 reader/writer lease：Setup 独占 stamp 检查、旧树删除、
安装、stamp 发布与最终 Verify；普通 Verify 共享读取；Workspace 的共享 lease 从 Verify 连续覆盖到最后一个 gate。
所有异常路径释放 lease。vcpkg checkout 先在同卷不可见 staging 中完成并验证，再用原子目录 rename 发布；访问冲突
只做有限重试，任何失败都回滚 partial destination，使后续 Setup 能从干净目的路径恢复。

Verify 在执行任何 gate 前清除 ambient `RUSTC*`/wrapper/rustflags、Cargo target linker/profile/registry overrides、
cc-rs target compiler、`CL`/`_CL_`、MSVC Developer Shell 残留、CMake/package roots、vcpkg overrides 和 proxy，并用
stamp 核验后的工具目录构造 PATH，显式恢复 pinned Developer Shell 生成的 include/lib 路径并设置 MSVC linker、Cargo
home/target、vcpkg tree 与 OCR 路径。代理只属于在线 Setup，不进入 Verify/Workspace 子进程。

下载包、工具二进制、native install tree 与编译缓存只存在于用户的受控环境根或 CI 临时目录，不进入 Git。
Cargo/vcpkg cache 只提速，cache miss 不改变正确性合同。本职责拆分不是离线构建承诺，开发电脑与首次 CI Setup
允许联网。PowerShell、Git、Python、rustup、VS Installer/vswhere 与 VS Build Tools 是运行 Setup 所需的宿主启动条件；
Setup 会把实际解析到的可执行文件路径和 SHA-256 写入 stamp，日常 Verify 不会回退到另一个系统工具。

## 3. 单一原生构建

每个 release version + target 只生成一份 canonical runtime bundle：

```text
runtime/win-x64/
├── bin/
│   └── easycon_core.dll
├── lib/
│   └── easycon_core.lib
├── include/easycon/
│   └── easycon.h
├── share/easycon/
│   ├── easycon-native.json
│   ├── licenses/
│   └── tessdata/            # 仅放已核验可再分发模型
└── symbols/
    └── easycon_core.pdb     # 单独 symbol artifact，不默认进用户包
```

`easycon-native.json` 至少记录：SDK version、ABI major/minor、build ID、git source revision、target、compiler、features、public symbol hash、OpenCV/Tesseract/Leptonica/Rust dependency versions、各文件 SHA-256。

四语言 packaging job 只能消费签名/校验后的 canonical bundle，不允许重新运行 Cargo/CMake。这样相同版本的 NuGet、wheel、npm platform package 和 CMake archive 必然携带相同 `easycon_core.dll`。

## 4. 原生依赖布局

### 链接策略

- Rust crates、私有 C++ bridge、所需 OpenCV modules、Tesseract、Leptonica 和可静态链接的 codec 依赖默认静态并入 `easycon_core.dll`。
- Windows Media Foundation/DirectShow、UCRT 等系统组件保持系统依赖。
- MSVC C++ runtime 使用一致的 `/MD` 配置；若需要 app-local redistributable，按 Microsoft 许可单独列入 manifest。
- 不跨 ABI 传递 allocator 所有权，因此 Rust/C++/CRT allocator 不需要互相 free。
- 若某依赖无法安全静态链接，允许放在同一 `bin` 目录的 private DLL，但必须进入 manifest/hash/SBOM，且 loader 只从 package-private 路径解析。

### 最小 OpenCV 面

只构建 v1 需要的 core、imgproc、imgcodecs、videoio 模块和 Windows capture backend。禁用 DNN、highgui、Java、Python bindings、测试/示例和不使用的 codec/backend，降低攻击面和包体。

### OCR 数据

- Tesseract engine 不从当前工作目录查找模型。
- Runtime options 可指定受信任 `tessdata_root`；未指定时使用 binding 解析出的 package-private share 路径。
- 官方包只携带来源、版本、checksum 和许可证已核验的模型。
- 模型缺失返回 `MODEL_NOT_FOUND`，不联网下载。
- O-03 未关闭前，release candidate 不得悄悄复制 `EasyCon/` 中的 traineddata。

## 5. C++ / CMake 发布

发布两个 artifact：

1. `easycon-sdk-<version>-win-x64.zip`：canonical bundle + C++ RAII headers/source + examples/license。
2. CMake package metadata：`EasyConSDKConfig.cmake`、version file 和 imported target。

target 约定：

- `EasyCon::CoreC`：C ABI header + import library。
- `EasyCon::SDK`：C++ RAII wrapper，公开依赖 `EasyCon::CoreC`。
- 自动把 runtime DLL 复制到 consumer target 的 output 只作为 opt-in helper，不修改全局 install rules。

`find_package(EasyConSDK 1 CONFIG REQUIRED)` 检查 architecture 和 ABI。CMake package 不下载 `EasyCon/` 或任何上游源码。

## 6. NuGet 发布

包名暂定 `EasyCon.SDK`，布局：

```text
lib/net8.0/EasyCon.SDK.dll
runtimes/win-x64/native/easycon_core.dll
buildTransitive/EasyCon.SDK.targets       # 仅必要的 runtime copy/validation
contentFiles/any/any/easycon-native.json  # 或嵌入 managed assembly 后读取
LICENSE
THIRD_PARTY_NOTICES.md
```

- managed assembly 为 AnyCPU，但包在非 x64/Windows 初始化时抛 `PlatformNotSupportedException`。
- 使用 `NativeLibrary.SetDllImportResolver` 从 NuGet runtime asset 精确加载，不依赖 PATH 或当前目录。
- package version 与 native SDK 完全相同；不单独发布不同版本的 native 子包。
- symbols/source link 单独发布 `.snupkg`；本项目源码 tag 必须可访问。

## 7. PyPI 发布

包名暂定 `easycon-sdk`：

```text
easycon/
├── __init__.py
├── _abi.py
├── _native/
│   ├── easycon_core.dll
│   └── easycon-native.json
├── tessdata/              # 仅核验模型
└── py.typed
easycon_sdk-<version>.dist-info/
```

- wheel tag 为 `py3-none-win_amd64`；Python 代码不绑定 CPython C API。
- loader 用绝对 package path 和 Windows safe DLL directory API，不修改进程全局 PATH。
- wheel repair/inspection 确认所有非系统 DLL 在包内且无工作站绝对路径。
- sdist 包含 SDK 自有源和构建说明，但构建 native core 需要已声明的 native toolchain；普通用户优先 wheel。
- 安装/导入不联网，不查找本地 `EasyCon/`。

## 8. npm 发布

使用 core package + platform package：

```text
@easycon/sdk
├── dist/index.js
├── dist/index.cjs
├── dist/index.d.ts
└── package.json

@easycon/sdk-win32-x64
├── prebuilds/win32-x64/easycon_node.node
├── prebuilds/win32-x64/easycon_core.dll
├── prebuilds/win32-x64/easycon-native.json
└── package.json
```

- `@easycon/sdk` 用 optional dependency 选择精确同版本 platform package；找不到时抛平台安装错误。
- addon 使用 Node-API 9，避免每个 Node minor 重编译。
- `package.json` 设 `engines`、`os: ["win32"]`、`cpu: ["x64"]`，不设置 browser fallback。
- loader 从 addon 相邻目录加载 core DLL；不依赖 PATH。
- 两个 npm 包必须同步发布，平台包的 native manifest hash 写入主包 allowlist。

## 9. 版本策略

### SDK SemVer

- C++、NuGet、PyPI、npm 使用同一个 `MAJOR.MINOR.PATCH`。
- 所有官方包同一 release train；缺任一语言验收则整个 release 不发布。
- prerelease 使用 `-alpha.N`、`-beta.N`、`-rc.N`，四种 registry 做各自等价映射。

### ABI version

- SDK 1.x 的 public ABI major 固定 1。
- ABI minor 只随向后兼容新增而递增；patch 修复不能改变 layout/symbol 集合语义。
- SDK major 变化不必然改变 ABI major，但任何破坏性 C 变化必须新增 v2 symbols。
- v2 出现时，核心至少一个 SDK major 同时导出 v1/v2，或提供明确的并存 library；不得让旧 binding 静默装载错误 ABI。

### 行为版本

ECS grammar、Controller protocol normalization、Vision score 和 event schema 都有 conformance version。修复源码实现缺陷但改变可观察结果时，在 changelog 标注 compatibility note，并增加 fixture；不能只靠 SemVer 猜测。

## 10. 可重复构建与供应链

release pipeline：

1. 校验 clean tree、签名 tag、toolchain/dependency lock。
2. 构建一次 canonical native bundle。
3. 执行 unit/ABI/sanitizer/fault tests 和硬件 release suite。
4. 生成 SPDX 或 CycloneDX SBOM、third-party notices、source archive、checksums。
5. 用 canonical bundle 打四种包并在 clean VM 安装测试。
6. 比较每个包内 native manifest/hash 完全一致。
7. 签名 Windows DLL/ZIP 和 registry artifact；签名不替代 checksum。
8. 先推 staging/preview，执行下载后 smoke，再 promote 同一字节到正式渠道。

dependency 更新必须经过 license、CVE、ABI、行为和包体差异审查。不得在 release job 从未锁定分支或“latest”下载依赖。

## 11. GPL-3.0-only 合规边界

**[源码事实]** EasyCon 本地源码与外层仓库都携带 GNU GPL v3 文本；外层项目已明确采用 `GPL-3.0-only`。

**[已决定]** SDK 自有 Rust/C++/C/.NET/Python/Node 代码和官方包装均为 `GPL-3.0-only`。稳定 C ABI 只是工程边界，不构成专有链接例外。

发布必须包含：

- `LICENSE` 与准确 package metadata；
- 所有迁移/翻译代码的原作者版权和来源说明；
- 第三方许可证、notice、版本、修改说明；
- 与 binary release 精确对应的完整 SDK 源码 tag/archive；
- 构建、安装和生成 binary 所需脚本/lock/配置；
- 若适用，GPL 所要求的安装信息和提供源代码方式。

调用方分发链接或嵌入该 SDK 的应用时，需要自行满足 GPLv3 对组合程序的义务。官方文档不得暗示动态链接、C ABI 或换语言可以绕开 GPL。此处是项目工程政策，不替代针对具体分发方式的法律意见。

OpenCV、Tesseract、Leptonica、Rust crates、Node addon 工具等必须逐项做许可证兼容扫描；不能仅凭常见许可证印象放行。Apache-2.0/MIT/BSD 类依赖也必须保留 notices。

## 12. EasyCon 源码边界

- canonical build、CI、package 和 source archive 都不读取 `EasyCon/`。
- 外层仓库不新增其远程、分支、提交锁、submodule 或下载脚本。
- 差分审计只能是本地可选流程；release gate 使用已经审阅并跟踪的黄金 fixture。
- 任何迁移实现进入外层仓库时，必须保留必要版权，不通过复制未追踪目录来完成构建。

## 13. 发布阻断项

以下任一项成立不得发布：

- 四语言 package 的 build ID/hash 不同；
- OCR 模型来源/许可证未核验却被打包；
- private native dependency 从 PATH 或工作目录解析；
- SBOM/license/source archive 缺失；
- ABI symbol/layout 与 golden 未解释地变化；
- Windows clean VM 或目标硬件 release suite 未通过；
- 包含 `EasyCon/` 内容、引用或获取机制；
- package metadata 未标 `GPL-3.0-only` 或暗示专有使用例外。
