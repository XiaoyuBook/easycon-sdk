# EasyCon SDK

EasyCon SDK 是基于 EasyCon 源码进行二次开发的多语言 SDK 项目，目标是让 C++、.NET、Python 和 Node.js/TypeScript 直接使用 EasyCon 的核心能力。

## 当前状态

仓库已经冻结 SDK v1 架构、[Phase 1 Runtime 基线](docs/decisions/0007-phase-1-freeze.md) 和
[Phase 2A Controller/Serial Candidate](docs/decisions/0009-phase-2a-freeze.md)。Phase 2A 包含 Windows serial
系统 backend、可注入 byte I/O、Controller 单写者 lane、Amiibo save/select、10,000-step fake 和软件路径
latency harness。

[Phase 2 Controller/Serial 开发目标](docs/decisions/0008-phase-2-controller-target.md) 已冻结；Phase 2A 仍为
`Hardware Unverified`：没有验证任何具体控制板、固件、VID/PID、Amiibo 容量或 UART/USB/Switch 时序，
O-01、O-02、O-04 保持开放。当前也未发布稳定公共 API/ABI；Phase 3 只重新冻结私有 Vision 跨平台源码候选，
不包含 ECS、语言绑定、固件或 UI；
完整 Phase 2 必须在 CH32 可用后通过 Phase 2B 硬件资格验证。

[Phase 2B Qualification Software Candidate](docs/decisions/0011-phase-2b-qualification-software-candidate-freeze.md)
已冻结并通过 fake/synthetic 门禁，但尚未在目标设备上执行资格命令，也没有创建支持矩阵或关闭任何硬件开放项。

Phase 3 私有 Vision 跨平台源码候选在旧候选因 NativePool admission P2 被重开后，已按
[ADR-0014](docs/decisions/0014-phase-3-native-pool-admission-refreeze.md) 以 implementation `27444f16` 重新冻结；
[ADR-0013](docs/decisions/0013-phase-3-vision-cross-platform-source-candidate-freeze.md) 仅保留第一次冻结的历史记录。Windows x64
软件/native 门禁通过但 capture hardware 未验证；Linux x64 是 v1 正式目标方向的 `Candidate / Build Unverified`；
macOS 只形成 Apple Silicon arm64 `Experimental Source Candidate / Build Unverified / Hardware Unverified /
Not Shipped`。三者均不等于完整 SDK 发布，且未冻结 public C ABI、Phase 4、四语言或 package。

[ADR-0016](docs/decisions/0016-phase-3-downstream-reopen-boundary.md) 已以 `Refrozen Governance Boundary` 接受：
准备增加 Phase 4 按 ADR-0014 的现行字面规则确实触发治理 reopen，本次 refreeze 已闭合该治理 reopen。从本次
refreeze 之后，纯下游 Phase 4/5/6 增加本身不再自动重开 Phase 3，只有实际改变其冻结面时才重开。Phase 3
implementation、public-neutral contract 和平台/支持状态均未改变；该治理决定本身不授权 Phase 4 target 或实现。

[ADR-0017](docs/decisions/0017-phase-4-ecs-automation-target.md) 现已作为
`Accepted / Frozen Phase 4 ECS/Automation Target` 生效，固定接受候选为 `fa265dff`。它冻结 Phase 4
`easycon-ecs` 的 compiler、不可变 Program、Automation Run、抽象 ports、确定性语义、limits、Runtime 窄重开边界
与安全实施 DAG。

治理链从 G0a/design base `38ef0dc` 依次经过初始 proposal `6468da5`、第一轮修订 `9983d42` 和第二轮修订
`fa265dff`；最终独立 full review 任务 `019f9348-1fb0-7130-86b6-57d69a0db31c` 对固定接受候选给出
`APPROVE`，P0/P1/P2=`0/0/0`。当前 `main` 基线 `87544d9` 已完成并合入 W0 与 S0：W0 只建立零依赖、可编译的
`easycon-ecs` workspace 骨架，S0 只建立 11 条自包含 provenance records、33 个 SDK-local artifacts 及静态
validator。R0 尚未完成独立 review/refreeze，C1 及其后节点也未启动；D0 仍是独立的 Phase 5 Controller 支线，
须另行授权。这些进展不表示 Phase 4 实现、硬件、支持或发布已经完成。

此前的实验性共享运行基线已经移除，不再作为本项目的产品架构或兼容性约束。后续 public C ABI 和各语言绑定
将在当前 Rust 共享核心之上按 Phase 5/6 的独立门禁实现。

完整架构固定在 [docs/README.md](docs/README.md)，包括源码能力映射、Rust/C++/C ABI
边界、生命周期与并发、四语言绑定、构建发布、测试和实施路线。首个里程碑的实现范围、
行为边界和验证命令见
[Runtime + Controller/Serial Phase 2A Candidate](docs/development/runtime-controller-vertical-slice.md)。

## SDK v1 目标

- **Controller**：设备发现与连接、按键、方向键、摇杆、Amiibo 和精确动作序列。
- **Automation**：ECS 脚本的编译、执行、停止、状态查询和运行事件。
- **Vision**：视频采集、截图、图像标签、模板匹配、OCR 和颜色检测。

首批官方 SDK：

- C++
- .NET
- Python
- Node.js/TypeScript

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
