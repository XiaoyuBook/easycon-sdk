# 0023：v1 宿主语言 SDK 路线与 ECS 延后

- 状态：Accepted / Effective
- 日期：2026-08-07
- 适用范围：EasyCon SDK v1 产品范围、实施顺序与发布资格
- 历史关联：[ADR-0017](0017-phase-4-ecs-automation-target.md)、[ADR-0018](0018-phase-2a-controller-lease-reopen.md)、[ADR-0019](0019-phase-4-c1-lexer-contract.md)、[ADR-0020](0020-controller-settlement-runtime-prerequisites.md)、[ADR-0021](0021-phase-4-c1-windows-loader-handle-identity-dependency.md)

## 背景

此前的路线把 ECS/Automation 作为 public C ABI、语言绑定和发布前的产品前置。这使尚未完成的语言、loader、fixture 和跨域实现阻断了一个更直接的 SDK 使用模型：宿主语言调用 Controller 与 Vision，并用自己的普通控制流组织业务逻辑。

`easycon-ecs`、其规范、fixture、conformance、validator 和 guards 已是受控 workspace 资产。它们不能因产品路线调整被删除、降级为不健康状态，或被误写为已经发布的 SDK 能力。

## 决定

### v1 产品模型

v1 聚合 Runtime、Controller 与 Vision。调用方在 C++、.NET、Python 或 Node.js/TypeScript 中直接调用这些能力，以普通函数、协程、Task 或 Promise 组合自己的业务流程。

- 普通长流程的等待、轮询和业务节拍由宿主语言负责；v1 不提供独立的工作流 `wait()`/delay/sleep API。第二阶段冻结的
  通用 C ABI 保留 `operation_wait`，它只观察既有 operation；超时返回 `WAIT_TIMEOUT` 并不取消 operation。语言 binding
  可以在内部把 `operation_wait`、`operation_status` 和 event 投影为 Task、Promise 或协程完成。
- 精确的 press、release 和 delay 使用由核心校验并由 Controller lane 调度的 `ActionSequence`。它保持绝对时间线、取消、lease、中立化和 transport-acceptance 边界。
- v1 不把 `.ecs` 文件、Program、Compilation、AutomationRun、Automation port 或 ECS diagnostic/source-limit 域暴露为产品能力。

### 四个产品阶段

v1 只按下列四个面向产品的阶段推进：

1. **Runtime、Controller、Vision 核心收口。** 收敛通用 Runtime 生命周期、Controller settlement 与 ActionSequence，以及 Vision 的资源和 native 边界；软件正确性先由确定性测试证明。
2. **公共 C ABI 与 canonical native bundle。** 冻结 Runtime、Controller、Vision 的 ABI 形态，生成头文件、manifest、symbol/layout golden，并构建所有语言共用的 native bundle。
3. **各语言 SDK。** 先完成 C++ 可用候选，再依次完成 .NET、Python、Node.js/TypeScript。它们只适配同一 ABI 和 bundle，不复制业务逻辑。
4. **打包、真实硬件、ABI、供应链与发布资格。** 完成包安装、真实 Controller/capture 硬件、ABI 兼容、SBOM/许可证、供应链和长稳资格。四语言 GA 属于同一 release train。

C++ 可用候选是第三阶段的优先里程碑，不代表四语言 v1 GA。任何阶段的通过都不能跳过后续阶段的门槛。

### ECS 的保留与非 v1 边界

`easycon-ecs` 以及现有 ECS spec、fixture、conformance、validator 和 guards 保留为 **dormant workspace maintenance** 资产：它们继续参加现有仓库健康门禁，继续可被维护和审查，但不是 v1 产品完成、公开 ABI、语言绑定共同验收、真实硬件、soak 或发布资格的前置。

因此：

- `easycon-sdk` 的 v1 聚合路径不连接 ECS ports；
- v1 C ABI 不包含 Program、compile/run、Automation handles、Automation errors/events、ECS diagnostics 或 ECS source/resource limits；
- v1 的四语言 SDK 不实现 Automation API，也不以 ECS trace 作为共同 conformance；
- 现有 ECS 资产不构成已发布、已支持或已硬件验证的声明。

### 历史 ADR 的关系

ADR-0017、ADR-0019 和 ADR-0021 保持原文及其已审查的历史/未来 ECS 合同。ADR-0018 和 ADR-0020 中通用 Runtime、Controller 所有权、取消、deadline、settlement 和 close 正确性仍可服务第一阶段。

本 ADR 只取代这些历史决定作为 **v1 产品前置和实施顺序** 的效力；它不倒写历史、不撤销其未来 ECS 合同，也不把其中的内部 DAG 名称当作当前产品进度。历史 ADR 中的 ECS 专属 API、ABI、fixture 和安全合同不会自动回流到 v1。

## 当前状态

当前处于第一阶段的软件核心收口，前三个产品阶段尚未全部完成：

- Runtime 的通用修复候选仍需要独立的固定 SHA 审查；
- Controller settlement 尚未完成；
- Vision 已有源码候选，但真实硬件仍未验证；
- C++ SDK 要在第一、第二阶段完成后才进入第三阶段的优先实现。

这些状态不构成 public C ABI、语言 SDK、硬件支持或发布完成的声明。

## 后果

- 路线、组件图、ABI、绑定、测试、包和发布文档必须以四阶段和 Runtime/Controller/Vision 为主线。
- 现有 Workspace 仍可运行 ECS 相关维护检查；这些检查只证明仓库资产没有退化，不转换成 v1 product/release evidence。
- 未来有意实现 ECS 时，必须另建 ADR，重新决定产品范围、公共 API/ABI、兼容性、资源/安全边界、fixture/conformance 和测试成本；不得仅凭 ADR-0017、ADR-0019 或 ADR-0021 自动恢复。

## 明确非目标

- 不在 v1 提供 ECS source loader、compiler、evaluator、Program 或 AutomationRun。
- 不把 Python/Lua runner、字节码、固件、UI、工程编辑器、远程服务或自定义回调纳入 v1。
- 不改变 `EasyCon/` 的只读参考源码边界，也不以它作为构建、CI 或发布输入。

## 关联

- [目标仓库与实施路线](../architecture/repository-roadmap.md)
- [架构总览](../architecture/architecture-overview.md)
- [C ABI v1](../architecture/c-abi-v1.md)
- [语言绑定](../architecture/language-bindings.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
- [构建、发布与合规](../architecture/build-release.md)
