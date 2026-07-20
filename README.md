# EasyCon SDK

EasyCon SDK 是基于 EasyCon 源码进行二次开发的多语言 SDK 项目，目标是让 C++、.NET、Python 和 Node.js/TypeScript 直接使用 EasyCon 的核心能力。

## 当前状态

仓库已经冻结 SDK v1 架构和 [Phase 1 Runtime 基线](docs/decisions/0007-phase-1-freeze.md)，并实现
Controller 协议/单写者调度、FakeControllerTransport 和无硬件 vertical slice。Controller 部分只是
Phase 2 的提前 slice；当前仍未发布稳定公共 API 或 ABI，也不包含物理串口 backend、Amiibo、Vision、
ECS、语言绑定、固件或 UI。

[Phase 2 Controller/Serial 开发目标](docs/decisions/0008-phase-2-controller-target.md) 已冻结：先完成不依赖
实物的 Phase 2A Candidate，再在 CH32 可用后执行 Phase 2B 硬件资格验证。冻结目标不表示实现或硬件支持
已经完成。

此前的实验性共享运行基线已经移除，不再作为本项目的产品架构或兼容性约束。共享核心、C ABI 和各语言绑定将在架构与行为规范固定后重新实现。

完整架构固定在 [docs/README.md](docs/README.md)，包括源码能力映射、Rust/C++/C ABI
边界、生命周期与并发、四语言绑定、构建发布、测试和实施路线。首个里程碑的实现范围、
行为边界和验证命令见
[Runtime + Controller fake vertical slice](docs/development/runtime-controller-vertical-slice.md)。

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
- 跨电脑开发时可以打包完整目录，以同时携带外层 SDK 仓库和本地 EasyCon 源码。

详细边界见 [源码边界决策](docs/decisions/0001-source-boundary.md)。

## 许可证

EasyCon SDK 自有代码采用 GNU General Public License v3.0（`GPL-3.0-only`）。完整条款见 [LICENSE](LICENSE)。

`EasyCon/` 中的第三方源码保留其自身的许可证和版权声明。
