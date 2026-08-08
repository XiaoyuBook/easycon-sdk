# 0024：重新冻结 Runtime R0-v2 实现基线

- 状态：Refrozen Runtime Implementation / Effective
- 日期：2026-08-08
- 重新冻结实现 SHA：`809866c36362128ffb3d7556187708fc1790b2b8`
- implementation parent：`baa67d44d00b9c95c7cbe668256de47b43cb71bf`
- implementation tree：`54d1c3259fd8d3cca8bf39d78e3ecb39bfbdf460`
- 上位冻结与窄重开：[ADR-0007](0007-phase-1-freeze.md)、[ADR-0020](0020-controller-settlement-runtime-prerequisites.md)
- 产品路线关系：[ADR-0023](0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md)

## 背景与冻结对象

[ADR-0007](0007-phase-1-freeze.md) 原先冻结的 Runtime Phase 1 基线，因
[ADR-0020](0020-controller-settlement-runtime-prerequisites.md) 对 deadline 与 terminal settlement 的窄重开而处于
reopened 状态。ADR-0020 明确要求 Runtime-only R0-v2 完成实现、完整门禁和 fixed-SHA 独立审查后，还必须进行一次
separate Phase 1 refreeze，D1 Controller 才能恢复。

本记录不倒写 ADR-0007 或 ADR-0020 的历史原文。唯一重新冻结、可集成的 Runtime 实现对象是
`809866c36362128ffb3d7556187708fc1790b2b8` 及其上述 tree；它是以
`baa67d44d00b9c95c7cbe668256de47b43cb71bf` 为唯一直接 parent 的单一 H 级提交。早期
`8671dd5b03120c4da10a6dc37f80fa1638505b8e`、`51a291ac42600f1a14b575d80e458692e1c7621e` 与
`7782e023d243b643522278a7eb435c9ef035c560` 只保留为 R0-v2 的审计历史来源，不是最终可集成对象，也不能替代本记录
冻结的 SHA、parent 或 tree。

## R0-v2 范围

本次重新冻结仅覆盖下列 Runtime 范围：

- 非 Operation 的 `DeadlineRegistration` 与其一次性 signal、注册、disarm、worker 调度语义；
- intent、claim、cleanup、commit 分离的 terminal/effect arbitration；
- owner、预持有 transferable handoff，以及不可证明 owner/evidence 时的 ownership-loss `CloseFailed`；
- deadline worker 在 Runtime close 期间继续服务既有 registration、随后 drain/join 的关闭顺序；
- `Clock` public reentry、`dispatch`/`now` panic 隔离、same-target 与 pending deadline 的确定性顺序修复。

该范围不引入 Controller transport、system serial 或 Controller fake 的 production 变更。R0-v2 的通用 Runtime
机制只提供 D1 所需前置，不替代 D1 的 transport settlement、acceptance、interrupt、partial-I/O 或 close-takeover 实现。

## 固定审查与门禁证据

Terra Ultra 对该 H 级 fixed-SHA 候选给出 `APPROVE`，P0/P1/P2/P3 均为 `0`。builder 的 staged credential 与 reviewer 的
clean credential 都绑定到 tree `54d1c3259fd8d3cca8bf39d78e3ecb39bfbdf460`、environment fingerprint
`a3b01b70ef0aee4ac7258e2bbf40aa6e953c64d40078b2fabd4adfe420bff9be` 和 gate policy
`c6e4f33fe5b85183ee746ee383355882a8305bbc8641b8ef836d5563f23778d1`。

| 证据 | 结果 |
| --- | --- |
| builder staged candidate | 14/14 gates passed；credential 为 tree-bound staged candidate |
| reviewer clean candidate | 13/13 gates passed；credential 为 tree-bound clean candidate |
| canonical `main` 集成后 Verify | Passed；fingerprint 与上述一致 |
| canonical `main` 普通 integration Workspace | Passed，551.4s；这是普通 Workspace，不作为 staged 或 clean candidate credential |

关键定向证据包括 public `Clock` reentry、same-target/pending order、`dispatch`/`now` panic 路径、deadline integration
13/13、terminal arbitration 9/9、Runtime unit 77、Loom 11、spec/guards，以及完整 Workspace。它们共同证明本次
Runtime-only 候选的已审查范围；普通 integration Workspace 不被表述为 staged 或 clean evidence。

## 重新冻结决定与产品边界

从本记录生效起，`809866c36362128ffb3d7556187708fc1790b2b8` 重新冻结为 Runtime R0-v2 实现基线。此决定仅关闭
Runtime R0-v2 prerequisite，并满足 ADR-0020 所要求的 separate Phase 1 refreeze；因此 D1 可以恢复，但任何既有 D1
RED 都不会自动变绿，仍须按 ADR-0020 完成完整 D1/D2 实现、验证、独立审查和 Controller refreeze。

本决定没有关闭 ADR-0023 的当前产品第一阶段：Controller settlement 仍未完成，Vision capture hardware 仍为
unverified。它也不冻结或声明 public C ABI、bindings、package、hardware、release 或 ECS 完成。`operation_wait` 保持为
观察既有 operation 的通用 API；独立 workflow delay/sleep API 继续排除。四个产品阶段、C++ first、ECS dormant
workspace maintenance、`ActionSequence` 的成功/失败边界、GPL 与平台/发布边界均不因本次重新冻结改变。

## 重新打开规则

以下任一变化重新打开本 Runtime R0-v2 基线，并要求受影响回归、完整门禁、固定新 SHA 与独立审查：

- Runtime production、behavior/spec、schema、fixture、conformance 或 models 的变化；
- deadline、terminal settlement、effect arbitration、close 或 `Clock` 语义、顺序、panic/reentry 或 worker ownership 的变化；
- owner/transferable handoff、ownership-loss `CloseFailed`、registry、event 或 Runtime API 的变化；
- 新的可复现且可行动的 in-scope finding，或任何把本记录的 Runtime 前置扩展为 Controller、ABI、bindings、package、
  hardware、release 或 ECS 完成声明的变化。

纯说明性修正不改变已冻结实现 tree 时，仍须保持 SHA、parent、tree、审查证据与产品边界准确；它不授权倒写历史 ADR 或
跳过上述重新打开条件。

## 关联

- [ADR-0007：冻结 Phase 1 Runtime 基线](0007-phase-1-freeze.md)
- [ADR-0020：冻结 Controller 结算的 Runtime 前置合同](0020-controller-settlement-runtime-prerequisites.md)
- [ADR-0023：v1 宿主语言 SDK 路线与 ECS 延后](0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md)
- [目标仓库与实施路线](../architecture/repository-roadmap.md)
