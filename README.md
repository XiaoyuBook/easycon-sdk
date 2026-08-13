# EasyCon SDK

EasyCon SDK 是基于 EasyCon 源码进行二次开发的多语言 SDK 项目，目标是让 C++、.NET、Python 和 Node.js/TypeScript 直接使用 EasyCon 的核心能力。

## 当前状态

[ADR-0023](docs/decisions/0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md) 将 SDK v1 重新冻结为四个产品阶段：
Runtime、Controller、Vision 核心收口；公共 C ABI 与 canonical native bundle；各语言 SDK；以及打包、真实硬件、ABI、
供应链与发布资格。

Controller D1 production implementation 已固定；其上的 validation/build-infrastructure hardening H input
`df13db4cb78c14602e05a31636a3e3a8f277f873`、tree
`a3819e469c9dc629876b84835fccbabcc73ccc8e` 已获独立 Task reviewer `APPROVE`。本 R 文档候选通过
[ADR-0026](docs/decisions/0026-controller-d1-settlement-refreeze.md) 提议重新冻结 Controller D1 software settlement，
并由 [ADR-0027](docs/decisions/0027-stage-1-software-core-closeout.md) 提议关闭 Runtime、Controller 与 Vision 的
Stage 1 Windows software core；[许可证初审](docs/development/stage1-license-initial-review.md) 覆盖当前 GPL/source、
41 个 Rust registry package、17 个 native runtime dependency 和 1 个 OCR 测试模型及其许可证文本。该候选仍为
`Pending Stage Review`，未经用户授权尚未集成 canonical `main`，不授权开始或集成 Stage 2。

上述候选只依据 fake/synthetic/native software evidence，状态保持 `Hardware Unverified`；尚无稳定 public C ABI、
canonical native bundle、任一语言 SDK、package、SBOM 或 release candidate。C++ 会在第一、第二阶段完成后优先进入
第三阶段；其可用候选也不等于四语言 v1 GA。

现有 `easycon-ecs`、ECS spec、fixture、conformance、validator 和 guards 保留为 dormant workspace maintenance 资产。
它们继续参加仓库健康门禁，但不是 v1 public ABI、四语言共同验收、真实硬件、soak 或发布的前置。ADR-0017、ADR-0019
和 ADR-0021 保留为历史/未来 ECS 合同，不自动恢复 v1 ECS 产品范围。

完整架构固定在 [docs/README.md](docs/README.md)，包括源码能力映射、Rust/C++/C ABI 边界、生命周期与并发、四语言绑定、
构建发布、测试和四阶段路线。Controller 与 Vision 的现有软件候选边界仍可参考
[Controller/Serial 现有软件候选](docs/development/runtime-controller-vertical-slice.md)。

## SDK v1 目标

- **Runtime 基础**：operation、事件、资源所有权、取消、deadline 与确定性 close。
- **Controller**：设备发现与连接、按键、方向键、摇杆、Amiibo 和精确 `ActionSequence`。
- **Vision**：视频采集、截图、图像标签、模板匹配、OCR 和颜色检测。

首批官方 SDK 为 C++、.NET、Python 和 Node.js/TypeScript。调用方直接用宿主语言的函数、协程、Task 或 Promise 组合
Controller 与 Vision；普通业务计时由宿主语言负责，精确 press/release/delay 交给 `ActionSequence`。v1 不提供独立的、用于
编排业务流程的公共 `wait()`/delay/sleep API；第二阶段冻结的 C ABI 保留通用 `operation_wait`，它只观察既有 operation，
超时返回 `WAIT_TIMEOUT` 并不取消该 operation。v1 也不包含 Automation。

## 仓库边界

- 外层目录是 EasyCon SDK 自有仓库。
- `EasyCon/` 是本地携带的第三方源码目录，由外层 `.gitignore` 完整忽略。
- 外层仓库不记录 EasyCon 的远程地址、分支、提交锁或下载脚本。
- 未经明确安排，不直接修改 `EasyCon/` 中的源码。
- 跨电脑开发只传输 Git tracked archive/bundle 与单独列明的可审计依赖；ignored `EasyCon/`、`.tools/`、
  build cache 和设备日志不得进入 SDK handoff bundle。

详细边界见 [源码边界决策](docs/decisions/0001-source-boundary.md)。

## Windows 构建环境

新电脑、共享 prepared environment 缺失或损坏，或 `tools/windows_build_environment.json` 所列 fingerprint、环境 schema、
host/target identity 变化后，运行一次在线 Setup：

```powershell
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Setup
```

日常可单独 Verify，或在 Verify 后运行完整 workspace gates：

```powershell
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Verify
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Workspace
```

新建或切换 Git worktree 本身不是 Setup 理由，应先运行 Verify。同一 fingerprint、环境 schema 和 host/target identity
的 worktree 共享一个 immutable prepared environment；Setup 安装并记录固定工具、Cargo vendor、native dependencies 与 OCR
资产，Verify/Workspace 不准备或修复该树。环境缺失、损坏或 shared identity 不匹配时会明确要求重新运行 Setup。下载包、工具二进制和缓存都位于受控的
Git 外目录；cache 只用于提速。环境目录之外的共享缓存按内容 hash 保存直接资产，并持久复用已复验的 vcpkg
scripts/downloads、Cargo downloads 与 Rust home；任何环境 stamp 或 ready 状态都不进入共享缓存。命中直接资产时每次
重新核对 hash/大小，损坏项先隔离再重新获取，发布使用逐资产锁和同卷原子 rename。Rust 因没有仓库内分发 hash，仍由
rustup 在每次实际 Setup 中执行其安装校验；短暂失败最多重试三次并复用已保留的 partial，成功后再核对
release、host、target 和 components。Cargo 不从 ambient PATH 独立选择；在首次 Cargo 执行（包括 vendor）前，Setup
只接受 rustup 为固定 toolchain 返回且位于受控 Rust home 精确 toolchain 目录中的 `cargo.exe`，并核对其 release 与 host。
已验证的 CMake/Ninja archive 同时预填到按 vcpkg scripts commit 隔离的共享 downloads root；vcpkg 不再为同一内部工具
二次联网，目标副本损坏时从内容寻址 blob 重新物化。vcpkg install 的短暂失败最多重试三次，并复用同一
downloads/buildtrees/binary cache；最终成功或失败都安全清理 transient buildtrees/packages。若清理受外部句柄阻止，
install 首错保持为主错误并附带 cleanup/residual tree 诊断。句柄释放后的 Setup 会先清理固定布局可推导的
buildtrees/packages，再执行通用严格环境树删除；只删除这些 transient tree 内的 link 本身，不跟随外部目标，
installed tree 不属于专用清理范围，其他位置的 reparse point 仍 fail closed。

prepared environment 的 identity 只包含 fingerprint、环境 schema 和固定 host/target，不包含 canonical worktree path。
canonical worktree key 只隔离 `CARGO_TARGET_DIR`、CMake cache、Cargo home/config、vcpkg wrapper/downloads 与 `TEMP`/`TMP`
等可写或 source-bound 输出；`TMPDIR`、Python pycache prefix 和 bytecode policy 也固定到同一 `w/<workspace-key>`，
不会写回源码 checkout。Setup 对同一共享 identity 独占重建，Setup 的 ready/final Verify、普通 Verify 与 Workspace
在访问 `w/` 前都取得同一 worktree writable lease，统一按 environment、workspace、shared-cache 的顺序持锁；Workspace
从 Verify 到最后一个 gate 持续受保护；
共享资产另有跨 identity reader/writer lease，避免 Setup 修改 Rust/vcpkg 共享状态时与 Verify 或 gates 竞争。受控 7-Zip
获取使用空 PATH 和 downloaded-binaries-only 模式，不接受宿主 PATH 中的任意 `7z.exe`/`7zr.exe`。模块只公开 Setup、
Verify 与 Workspace 三个命令；所有 helper 保持模块私有。
Setup 对现有 ready 环境的预检必须先取得共享资产 reader lease；writer busy/timeout 原样失败并保留 stamp、环境与 marker，
不把 lease acquisition failure 当作 Verify failure，也不启动下载或 provision。
所有 gate 只继承核验后的编译环境，命令返回后
恢复调用 shell 原有环境。text fingerprint 对严格 UTF-8 内容规范化 CRLF/CR 为 LF，binary 输入按原始 bytes 计算，
因此同一文本在不同 checkout 行尾下保持同一环境 identity；Setup 在任何下载前拒绝固定输入的乱序、非规范 path 或
Windows 大小写 alias。stamp v2 逐层拒绝 duplicate key、错误 JSON 类型、非十进制整数、缺失或未知 nested field；工具
记录不能位于源码 repository 或整个 `CacheRoot/w`。prepared JSON 会结构化解码 escaped path，并与其他 text artifact
一起拒绝源码及任意 worktree writable-root 引用。Verify 对 prepared vcpkg Git 只执行无 optional lock 的读取，保持共享
`e/` 及其 `.git/index` 不变。锁定的 download/stamp temporary 无法立即删除时保持无效，错误同时保留首错与 cleanup 状态，
句柄释放后的后续下载或 Setup 可恢复。完整、未损坏的直接固定资产 cache hit 不重新启动下载；Rust/vcpkg/Cargo
仍按各自安全模型检查或补齐缺失内容，因此这一职责拆分不承诺完全离线或零网络请求。完整固定清单与 CI 边界见
[GitHub CI 运维边界](.github/CI.md) 和 [构建、发布与合规](docs/architecture/build-release.md)。
`vswhere.exe` 成功退出但没有非空匹配时，Setup 明确报告未找到带 x64 C++ toolchain 的 Visual Studio 2022，
不会暴露内部数组索引异常。

## 许可证

EasyCon SDK 自有代码采用 GNU General Public License v3.0（`GPL-3.0-only`）。完整条款见 [LICENSE](LICENSE)。

`EasyCon/` 中的第三方源码保留其自身的许可证和版权声明。
