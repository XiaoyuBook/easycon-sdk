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
8. [目标仓库与实施路线](architecture/repository-roadmap.md)：四阶段产品路线、完成门槛、C++ 优先与四语言任务拆分。
9. [Phase 2B faults runner 所有权设计](development/phase2b-fault-runner.md)：资格 CLI 的单 owner、失败收口、
   动态 cleanup contract 和无硬件 failpoint 测试边界。
10. [Phase 2B Controller close 中立化证据设计](development/phase2b-controller-cleanup-evidence.md)：
    最终中立写的 transport acceptance、Controller/Runtime 组合 cleanup 和 synthetic failure 回归边界。
11. [Phase 2B run directory ownership 设计](development/phase2b-run-directory-ownership.md)：目录级永久
    reservation、retained staging、owned auxiliary 和 no-replace publish。
12. [Phase 2B 设备身份接纳与诊断 readiness 证据设计](development/phase2b-device-admission-readiness.md)：
    expected stable identity、open 前后复核、hotplug 重绑定和 diagnostic prelude 的非 capability 边界。
13. [Phase 2B 普通命令 runner 所有权与失败证据设计](development/phase2b-command-runner-ownership.md)：
    非 faults 命令的单 owner、operation 失败收口和动态 partial-cleanup contract。
14. [Phase 2B durable evidence transaction 设计](development/phase2b-durable-evidence-transaction.md)：
    build/runtime provenance、append-only journal、manifest、completion marker 和磁盘分类。
15. [Phase 2B 操作员、取消与观察设计](development/phase2b-operator-cancellation-observation.md)：
    单 owner OperatorPort、Ctrl+C cooperative cancellation、逐动作 observation 和终态顺序。
16. [Phase 2B Amiibo 资格写入安全设计](development/phase2b-amiibo-qualification-safety.md)：
    一次性写入授权、外部 limits 来源、payload hash、chunk write-ahead journal 和 synthetic failpoint 边界。
17. [Phase 2B telemetry 与资格投影设计](development/phase2b-telemetry-qualification-projection.md)：
    logical report partial 聚合、native I/O error、checked timing、物理未验证边界和唯一终态三元组。
18. [Phase 2B checkpoint 软件收口设计](development/phase2b-checkpoint-software-closeout.md)：
    磁盘 evidence 重验、五类 checkpoint、handoff attestation 边界和 Hardware Unverified transaction。
19. [Phase 3 Vision 跨平台边界设计](development/phase3-cross-platform-design.md)：Windows Tier 1、Linux
    candidate、macOS arm64 experimental source 的 ownership、构建、证据和晋级边界。
20. [Phase 3 Linux/macOS 验证 handoff](development/phase3-cross-platform-validation-handoff.md)：固定实现
    SHA、依赖版本、外部 build root、Linux 软件门禁与 macOS arm64 分阶段资格矩阵。
21. [Phase 2B faults 协议证据修订设计](development/phase2b-fault-protocol-evidence-remediation.md)：每个 faults
    role 的 baud/handshake ledger、qualification-only reply byte correlation、legacy 分类与 candidate refreeze 门槛。
22. [Stage 1 GPL/source/dependency license 初审](development/stage1-license-initial-review.md)：固定当前 Rust、native
    与 OCR 测试输入的版本、来源、许可证选择和 future release gate，不替代 SBOM/notices 或法律意见。

> **历史计划编号说明：** 上述 `Phase 2`、`Phase 2A`、`Phase 2B` 和 `Phase 3` 开发记录及历史 ADR 标题保留原名，
> 只表示 ADR-0023 前的开发/证据语境；它们不表示 ADR-0023 所定义的当前四个产品阶段或其完成状态。

## 决策记录

- [ADR-0001：外层 SDK 与 EasyCon 源码分离](decisions/0001-source-boundary.md)
- [ADR-0002：用户进程内的 Rust 共享核心](decisions/0002-embedded-shared-core.md)
- [ADR-0003：Rust 核心、私有 C++ 桥接与公共 C ABI](decisions/0003-core-native-abi-boundary.md)
- [ADR-0004：操作句柄、拉取事件与确定性关闭](decisions/0004-operations-events-shutdown.md)
- [ADR-0005：Windows x64 首发与同源原生包](decisions/0005-v1-platform-packaging.md)
- [ADR-0006：Runtime 所有权、终态事务与确定性关闭](decisions/0006-runtime-stabilization.md)
- [ADR-0007：冻结 Phase 1 Runtime 基线](decisions/0007-phase-1-freeze.md)
- [ADR-0008：冻结 Phase 2 Controller/Serial 开发目标](decisions/0008-phase-2-controller-target.md)
- [ADR-0009：冻结 Phase 2A Controller/Serial Candidate 基线](decisions/0009-phase-2a-freeze.md)
- [ADR-0010：冻结 Phase 2B 硬件资格工具与证据目标](decisions/0010-phase-2b-qualification-evidence.md)
- [ADR-0011：冻结 Phase 2B Qualification Software Candidate](decisions/0011-phase-2b-qualification-software-candidate-freeze.md)
- [ADR-0012：冻结 Phase 3 Vision 与跨平台私有 native bridge 开发目标](decisions/0012-phase-3-vision-native-target.md)
- [ADR-0013：冻结 Phase 3 Vision 跨平台源码候选](decisions/0013-phase-3-vision-cross-platform-source-candidate-freeze.md)
- [ADR-0014：Phase 3 NativePool admission 修复后重新冻结](decisions/0014-phase-3-native-pool-admission-refreeze.md)
- [ADR-0016：重新冻结 Phase 3 候选的下游阶段重新打开边界](decisions/0016-phase-3-downstream-reopen-boundary.md)
- [ADR-0017：冻结 Phase 4 ECS 与 Automation 目标（Accepted / Frozen Target）](decisions/0017-phase-4-ecs-automation-target.md)
- [ADR-0018：窄重开 Phase 2A Controller lease 结算合同（Accepted）](decisions/0018-phase-2a-controller-lease-reopen.md)
- [ADR-0019：冻结 Phase 4 C1 lexer 合同（Accepted / Frozen）](decisions/0019-phase-4-c1-lexer-contract.md)
- [ADR-0020：冻结 Controller 结算的 Runtime 前置合同（Accepted / Effective）](decisions/0020-controller-settlement-runtime-prerequisites.md)
- [ADR-0021：冻结 Phase 4 C1 Windows high-resolution file-identity foundation 前置合同（Accepted / Effective）](decisions/0021-phase-4-c1-windows-loader-handle-identity-dependency.md)
- [ADR-0022：Windows Workspace policy identity 与 evidence v2 合同（Accepted / Effective）](decisions/0022-windows-workspace-policy-identity-and-evidence-v2.md)
- [ADR-0023：v1 宿主语言 SDK 路线与 ECS 延后（Accepted / Effective）](decisions/0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md)
- [ADR-0024：重新冻结 Runtime R0-v2 实现基线（Refrozen Runtime Implementation / Effective）](decisions/0024-runtime-r0-v2-refreeze.md)
- [ADR-0025：Stage 1 开发 working implementation base（Not a refreeze）](decisions/0025-stage1-working-implementation-base.md)
- [ADR-0026：Controller D1 settlement 重新冻结候选（Pending Stage Review, Hardware Unverified）](decisions/0026-controller-d1-settlement-refreeze.md)
- [ADR-0027：Stage 1 Runtime/Controller/Vision 软件核心收口候选（Pending Stage Review）](decisions/0027-stage-1-software-core-closeout.md)
- [ADR-0028：Windows candidate gate 分层（Accepted / Effective）](decisions/0028-windows-candidate-gate-layering.md)

## v1 固定范围

| 能力域 | v1 包含 | v1 明确排除 |
| --- | --- | --- |
| Controller | 设备发现/连接、按键、方向键、双摇杆、Amiibo、精确动作序列 | 固件字节码、烧录、远端启动/停止、操作录制、键鼠映射 |
| Vision | 采集、最新帧截图、`.IL` 图像标签、模板匹配、OCR、HSV 颜色检测 | UI 搜图控制台、未完成的 `.ILX` 契约、未接通的旧像素匹配算法 |
| Runtime 与产品外壳 | operation、事件、所有权、取消、deadline、close；C++、.NET、Python、Node.js/TypeScript SDK | ECS/Automation、浏览器、独立服务进程、UI、配置界面、远程助手 |

## 当前实施状态

v1 路线由 [ADR-0023](decisions/0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md) 固定为四个产品阶段：

1. Runtime、Controller、Vision 核心收口。
2. 公共 C ABI 与 canonical native bundle。
3. C++、.NET、Python、Node.js/TypeScript SDK，其中 C++ 优先形成可用候选。
4. 打包、真实硬件、ABI、供应链与发布资格。

Controller D1 production implementation 固定为 `f99333f1e4d359af4588a659e2890bcfd58483de`；其后的 approved H
validation/build-infrastructure input `df13db4cb78c14602e05a31636a3e3a8f277f873`、tree
`a3819e469c9dc629876b84835fccbabcc73ccc8e` 已获独立 Task reviewer `APPROVE`，P0/P1/P2/P3 均为 0。
H SHA/tree 不替代 production implementation object。

本 R 文档候选由 [ADR-0026](decisions/0026-controller-d1-settlement-refreeze.md) 提议重新冻结 Controller D1
software settlement，并由 [ADR-0027](decisions/0027-stage-1-software-core-closeout.md) 汇总 Runtime R0-v2、
Controller D1、Vision 既有 software evidence 和
[Stage 1 license 初审](development/stage1-license-initial-review.md)，形成 Windows Runtime/Controller/Vision
software-core closeout candidate。fake/synthetic/native software 路径覆盖八类退出条件；真实硬件、O-01 至 O-06、
public C ABI、bindings、packages、SBOM/signing/release 仍开放。该 R candidate 仍为 `Pending Stage Review`，未经用户授权
尚未集成 canonical `main`，不授权开始或集成 Stage 2；C++ SDK 尚未进入实施，C++ 可用候选也不等于四语言 GA。

现有 ECS spec、fixture、conformance、validator、guards 和 `easycon-ecs` crate 继续是 dormant workspace maintenance
资产：现有健康门禁继续维护它们，但它们不属于 v1 public ABI、语言 SDK 的共同验收、硬件/soak 或发布资格。ADR-0017、
ADR-0019 和 ADR-0021 保留为历史/未来 ECS 合同；ADR-0018、ADR-0020 中通用 Runtime/Controller 正确性仍可服务第一阶段，
但其 ECS 专属 DAG 不定义 v1 产品路线。

## 首发基线

- **[已决定]** Windows 10/11 x64 是 v1 Tier 1，v1.0 GA 目标三元组为 `x86_64-pc-windows-msvc`。
- **[已决定]** Linux x64 是 v1 正式目标方向；当前只形成 Vision build candidate，serial、硬件、四语言包
  和发布门禁完成前保持 Candidate/Unverified。
- **[已决定]** macOS 只形成 Apple Silicon arm64 `Experimental Source Candidate / Build Unverified /
  Hardware Unverified / Not Shipped`；不承诺 Intel 或 universal binary。
- **[已决定]** 四种 SDK 必须装载同一份原生核心构建，不允许复制业务实现或形成语言特有语义。
- **[已决定]** 所有发布物统一采用 `GPL-3.0-only`，不存在专有链接例外。

## 待确认登记

以下事项不改变组件边界，可在实现期间通过硬件或发布输入收敛：

| 编号 | 事项 | 当前默认 | 最迟门槛 |
| --- | --- | --- | --- |
| O-01 | 首批受支持的控制板、固件版本和 USB/串口标识 | 以现有 115200/9600 握手协议实现，支持表为空 | Controller 硬件 Beta 前 |
| O-02 | Amiibo 槽位数量与允许的数据长度 | Controller 默认没有该 capability；只有调用方显式提供 Hardware Unverified limit 后才接纳 save/select | C ABI 冻结前 |
| O-03 | 官方包内置的 OCR 语言数据 | 默认接口语言为 `chi_sim`，模型只有在来源和许可证核验后才随包发布 | 首个发布候选版前 |
| O-04 | 物理硬件动作时序 SLO | 采用 ADR-0008 的分段暂定目标，以逻辑分析仪、firmware trace 和 Switch 可观察结果校准 | Controller 硬件 Beta 前 |
| O-05 | Linux x64 完整 SDK 晋级 | Vision build candidate；serial、硬件、四语言和 package 未完成 | Linux 支持声明前 |
| O-06 | macOS Apple Silicon arm64 晋级 | Experimental Source Candidate；build/hardware unverified，not shipped | 首次真实 build 后重开 |
