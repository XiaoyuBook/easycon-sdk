# EasyCon SDK

EasyCon SDK 是基于 EasyCon 源码进行二次开发的多语言 SDK 项目，目标是让 C++、.NET、Python 和 Node.js/TypeScript 直接使用 EasyCon 的核心能力。

## 当前状态

仓库目前处于架构设计阶段，尚未发布稳定 API、ABI 或可用 SDK 实现。

此前的实验性共享运行基线已经移除，不再作为本项目的产品架构或兼容性约束。共享核心、C ABI 和各语言绑定将在架构与行为规范固定后重新实现。

完整架构已经固定在 [docs/README.md](docs/README.md)，包括源码能力映射、Rust/C++/C ABI 边界、生命周期与并发、四语言绑定、构建发布、测试和实施路线。当前提交只包含设计，不包含功能项目或业务实现。

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
