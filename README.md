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

[ADR-0016](docs/decisions/0016-phase-3-downstream-reopen-boundary.md) 以 `Proposed Refreeze` 状态承认：准备增加
Phase 4 按 ADR-0014 的现行字面规则确实触发治理 reopen。Phase 3 implementation、public-neutral contract 和
平台/支持状态均未改变；独立审查与后续 refreeze 提交完成前，该提议不生效，也不授权 Phase 4 target 或实现。

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

## 许可证

EasyCon SDK 自有代码采用 GNU General Public License v3.0（`GPL-3.0-only`）。完整条款见 [LICENSE](LICENSE)。

`EasyCon/` 中的第三方源码保留其自身的许可证和版权声明。
