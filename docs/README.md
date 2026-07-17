# EasyCon SDK v1 架构文档

本目录固定 EasyCon SDK v1 的嵌入式共享核心架构。SDK 直接加载到用户进程，Rust 实现唯一的业务核心，C++ 只隔离 OpenCV、Tesseract、视频采集和无法合理由 Rust 承担的系统调用，对外只承诺稳定 C ABI。

## 结论标记

文档使用以下标记区分信息来源，未带标记的规范性语句视为“已决定”。

- **[源码事实]**：可由本地 `EasyCon/` 快照中的实现直接验证，并附路径、符号或行号。
- **[已决定]**：本阶段固定的 v1 产品或架构约束，实施不得自行偏离。
- **[推导]**：为满足已决定事项、修复源码中已确认的问题或形成可测试契约而得出的设计结论。
- **[待确认]**：源码无法回答且需要产品、硬件或合规输入的窄问题；不得用它扩大 v1 范围。

`EasyCon/` 的证据引用只用于审计，不构成远程地址、分支、提交锁或构建依赖。外层仓库仍遵守 [ADR-0001](decisions/0001-source-boundary.md)。

## 文档导航

1. [源码能力映射](architecture/source-capability-map.md)：源码组件到 v1 能力和新核心模块的逐项归属。
2. [架构总览与模块依赖](architecture/architecture-overview.md)：组件、crate、C++ 桥接、绑定边界和禁止依赖。
3. [生命周期与并发](architecture/runtime-lifecycle.md)：资源所有权、状态机、调度、取消、超时、事件和销毁顺序。
4. [C ABI v1 设计](architecture/c-abi-v1.md)：句柄、结构体、字符串、错误、异步操作、线程安全和兼容规则。
5. [语言绑定](architecture/language-bindings.md)：C++、.NET、Python、Node.js/TypeScript 的惯用接口与底层绑定。
6. [构建、发布与合规](architecture/build-release.md)：首发平台、工具链、动态库布局、四类包和 GPL 边界。
7. [测试策略](architecture/testing-strategy.md)：单元、ABI、一致性、差分、故障注入和硬件验收。
8. [目标仓库与实施路线](architecture/repository-roadmap.md)：固定目录、阶段顺序、完成门槛和四语言任务拆分。

## 决策记录

- [ADR-0001：外层 SDK 与 EasyCon 源码分离](decisions/0001-source-boundary.md)
- [ADR-0002：用户进程内的 Rust 共享核心](decisions/0002-embedded-shared-core.md)
- [ADR-0003：Rust 核心、私有 C++ 桥接与公共 C ABI](decisions/0003-core-native-abi-boundary.md)
- [ADR-0004：操作句柄、拉取事件与确定性关闭](decisions/0004-operations-events-shutdown.md)
- [ADR-0005：Windows x64 首发与同源原生包](decisions/0005-v1-platform-packaging.md)

## v1 固定范围

| 能力域 | v1 包含 | v1 明确排除 |
| --- | --- | --- |
| Controller | 设备发现/连接、按键、方向键、双摇杆、Amiibo、精确动作序列 | 固件字节码、烧录、远端启动/停止、操作录制、键鼠映射 |
| Automation | ECS 编译、不可变程序、运行、停止、状态、诊断、日志和事件 | Python/Lua Runner、自定义语言回调、工程编辑器、推送副作用 |
| Vision | 采集、最新帧截图、`.IL` 图像标签、模板匹配、OCR、HSV 颜色检测 | UI 搜图控制台、未完成的 `.ILX` 契约、未接通的旧像素匹配算法 |
| 产品外壳 | C++、.NET、Python、Node.js/TypeScript SDK | 浏览器、独立服务进程、UI、配置界面、远程助手 |

## 当前实施状态

架构冻结后的首个开发里程碑已经实现 Rust Runtime + Controller fake vertical slice。它覆盖
operation、取消、事件溢出、确定性关闭、源码精确 report、单写者调度、精确序列、底层
Automation lease 与 generation-aware ACK；不代表正式 C ABI、系统串口 backend 或物理硬件
支持已经完成。实现说明和本地验收命令见
[Runtime + Controller fake vertical slice](development/runtime-controller-vertical-slice.md)。

## 首发基线

- **[已决定]** v1.0 GA 只承诺 Windows 10/11 x64，目标三元组为 `x86_64-pc-windows-msvc`。
- **[已决定]** Linux x64 与 macOS arm64 只要求代码边界可移植；在各自硬件、打包和一致性门槛通过前不得出现在支持矩阵中。
- **[已决定]** 四种 SDK 必须装载同一份原生核心构建，不允许复制业务实现或形成语言特有语义。
- **[已决定]** 所有发布物统一采用 `GPL-3.0-only`，不存在专有链接例外。

## 待确认登记

以下事项不改变组件边界，可在实现期间通过硬件或发布输入收敛：

| 编号 | 事项 | 当前默认 | 最迟门槛 |
| --- | --- | --- | --- |
| O-01 | 首批受支持的控制板、固件版本和 USB/串口标识 | 以现有 115200/9600 握手协议实现，支持表为空 | Controller 硬件 Beta 前 |
| O-02 | Amiibo 槽位数量与允许的数据长度 | ECS 暴露 0..9；底层按字节缓冲并依据已验证设备能力拒绝 | C ABI 冻结前 |
| O-03 | 官方包内置的 OCR 语言数据 | 默认接口语言为 `chi_sim`，模型只有在来源和许可证核验后才随包发布 | 首个发布候选版前 |
| O-04 | 物理硬件动作时序 SLO | 先采用测试策略中的暂定阈值，以逻辑分析仪结果校准 | Controller 硬件 Beta 前 |
| O-05 | v1.1 是否增加 Linux x64 或 macOS arm64 | 不承诺 | v1.0 发布后规划 |
