# 0026：Controller D1 settlement 实现重新冻结候选

- 状态：Refreeze Candidate / Pending Stage Review (Hardware Unverified)
- 日期：2026-08-13
- production implementation SHA：`f99333f1e4d359af4588a659e2890bcfd58483de`
- production implementation parent：`be4c2cdfe9ffcdfe9de35cdda032502d2a63e89c`
- production implementation tree：`cc9a6040d000be9c070f586b04bb20d2be75a685`
- production implementation 主题：`fix:线性化控制器动作派发结算`
- approved H validation input：`df13db4cb78c14602e05a31636a3e3a8f277f873`
- H input parent：`8edc700a91dc02fbe58833126954881a2ab0de22`
- H input tree：`a3819e469c9dc629876b84835fccbabcc73ccc8e`
- H input 主题：`build:分层Windows候选验证门禁`
- R document candidate 直接 parent 约束：`df13db4cb78c14602e05a31636a3e3a8f277f873`
- 上位合同：[ADR-0018](0018-phase-2a-controller-lease-reopen.md)、
  [ADR-0020](0020-controller-settlement-runtime-prerequisites.md)、
  [ADR-0024](0024-runtime-r0-v2-refreeze.md)、
  [ADR-0025](0025-stage1-working-implementation-base.md) 和
  [ADR-0028](0028-windows-candidate-gate-layering.md)

## 背景与候选对象

ADR-0018 和 ADR-0020 窄重开了 Controller acquire/action/release/close settlement，并要求 Runtime 前置、D1
实现、D2 独立审查和单独的 Controller refreeze。ADR-0024 已独立重新冻结 Runtime R0-v2；ADR-0025 随后只把
`3f1481480b3ee4206aa75a248a001213f960fcb6` 记录为 working implementation base，不是 Controller 或 Stage 1
冻结决定。

working base 的后续提交 `be4c2cdfe9ffcdfe9de35cdda032502d2a63e89c` 完成 D1 predecessor；production
implementation `f99333f1e4d359af4588a659e2890bcfd58483de` 以它为唯一直接 parent，闭合后续 Stage review
发现的两个竞争：generation seal 不得消费 backend `Outstanding` settlement owner，并发 admission 回填必须与
backend dispatch 通过同一 record 顺序决议。production tree 把 backend final-byte acceptance、Controller bookkeeping
和 Runtime terminal publication 分为 reservation、claim、finish 阶段，完成 Controller D1 的 acquire/action/release/
close settlement。

本 ADR 提议重新冻结且只重新冻结上述 production SHA/tree。任何其他提交、tree、未提交 diff、H validation tree 或
普通 Workspace summary 都不能替代 production implementation object。承载本文的 R document candidate 以 approved H
input `df13db4cb78c14602e05a31636a3e3a8f277f873` 为唯一直接 parent；它仍须取得独立 Stage reviewer `APPROVE`，
未经用户授权也尚未集成 canonical `main`。

## Production 与 H validation lineage

validation lineage 精确为：

```text
f99333f1e4d359af4588a659e2890bcfd58483de
  <- 8edc700a91dc02fbe58833126954881a2ab0de22
  <- df13db4cb78c14602e05a31636a3e3a8f277f873
```

`8edc700a91dc02fbe58833126954881a2ab0de22` 增加 debug-only deterministic rendezvous 及相关测试，稳定
Controller lease 与 CH32 simulator 的竞争复验。`df13db4cb78c14602e05a31636a3e3a8f277f873` 在其上实现
ADR-0028 的 A/B/C/D gate layering，并加固 classifier、runner、repository guards、policy 和 CI。

`df13db4c` 没有改变 production identity，也没有修改 Cargo、product behavior、schema、fixture、conformance、
dependency lock、native/OCR manifest 或 license fixed input。独立 Task reviewer 对固定 `df13db4c` SHA/tree 给出
`APPROVE`，P0/P1/P2/P3=`0/0/0/0`。因此该 H object 是本 R candidate 使用的 approved validation/
build-infrastructure input；它不把 H SHA/tree 冒充 production implementation SHA/tree，也不扩大重新冻结范围。

## Approved H input 与 evidence boundary

approved H input 的唯一 builder staged A credential 为 schema v2、status `passed`，精确绑定：

- base/head：`8edc700a91dc02fbe58833126954881a2ab0de22`；
- tree：`a3819e469c9dc629876b84835fccbabcc73ccc8e`；
- environment fingerprint：`a3b01b70ef0aee4ac7258e2bbf40aa6e953c64d40078b2fabd4adfe420bff9be`；
- gate policy：`a7c6abd2ede1e9ad6a4c8c36843c1e4270b22459d268e5094f9f0945d7c64500`；
- 11 条 ordered gate records，全部 `passed`。

Task reviewer 未 replay A，也没有生成或要求 clean-tree credential；其职责是对 fixed SHA 做独立静态、binding 与产品资格
审查。被该 approved H 替代的旧 validation candidate、旧 policy credential、旧 runId 或任何历史 clean-tree evidence
都不是本 R candidate 的批准材料。

本 R builder 只在最终 staged R tree 上运行一次新的 A。后续 Stage reviewer 只核验该唯一 staged R A 的 schema/status/
base/HEAD/tree/fingerprint/policy/ordered gates binding，并独立做静态与产品资格审查；不得运行 Workspace、生成 clean-tree
credential 或 replay A。本 R 文档不改变 ADR-0028 的其余分层：B 是 bootstrap 加 12 个 Fast groups，只阻断
infrastructure candidate；C 是 4 个 Qualification groups，仅作 observation；D 没有执行结果。ADR-0028 只定义候选
验证分层，本身不关闭 Controller 或 Stage 1。

## 候选重新冻结范围

本候选只重新冻结 Controller D1 的软件 settlement：

- backend final-byte gate 的唯一 completion owner、logical-report reservation、Runtime owner claim 与 deferred finish；
- generation admission 回填与 backend dispatch 共享同一 settlement record 顺序；seal 胜出且仍为 `NotDispatched`
  时可立即以 `NotDelivered` 结算，dispatch 胜出成为 `Outstanding` 后只登记 immutable cancellation intent、请求
  interrupt，并保留 backend settlement owner 等待真实 full/partial/error/not-delivered completion；
- Windows `CancelIoEx` 的 `ERROR_NOT_FOUND` 只表示 interrupt 请求未找到 pending I/O，仍须消费
  `GetOverlappedResult`；完整 logical bytes 必须赢得 accepted reservation，使 action `Succeeded`；
- acquire、action、release 和 close takeover 的取消、deadline、中立化、partial-stream 与 fail-closed 边界；
- fake/system-serial parity、Windows completion ownership、`ActionSequence` 精确软件时间线及对应测试与 conformance；
- owner 可转移或 settlement evidence 可证明时的确定性终态，以及 owner-loss 不可证明时保留非终态 record/registry
  并返回 `CloseFailed` 的保守行为。

Runtime deferred claim/finish 只作为 Controller physical-gate prerequisite 的既有合同实现。它不倒写 ADR-0018、
ADR-0020 或 ADR-0024，也不把 ADR-0024 的 Runtime refreeze object 改为本 production SHA。Runtime、Controller 与
Vision 的 Stage 1 组合候选由 [ADR-0027](0027-stage-1-software-core-closeout.md) 单独记录。

## 硬件与产品边界

`Hardware Unverified` 是候选边界的一部分。本 ADR 不证明真实 Controller、CH32、UART、USB HID、Switch、firmware、
Amiibo 容量或物理时序；O-01、O-02 和 O-04 保持开放。真实 capture hardware、Linux/macOS promotion、public C ABI、
canonical native bundle、C++/.NET/Python/Node.js bindings、packages、SBOM、notices、source archive、signing、release、
ECS 与 Automation 也不在候选范围内。

## 重新打开规则

以下任一变化重新打开本 Controller D1 software settlement candidate，并要求新的 implementation object、受影响测试、
新的唯一 staged-candidate A 和独立 Stage review：

- Runtime/Controller/serial production、behavior、schema、fixture、conformance 或 public contract 变化；
- backend acceptance、reservation/claim/finish、lease、action、release、neutralization、stream settlement、close takeover、
  owner-loss、registry 或 event 顺序变化；
- approved H validation/build-infrastructure input 的 deterministic tests 或受控 candidate policy 失效；
- 新的可复现且可行动的 in-scope finding；
- 把本软件候选扩展为硬件、ABI、binding、package、跨平台、供应链、release 或 ECS 完成声明。

## 关联

- [Stage 1 Runtime/Controller/Vision 软件核心 closeout candidate](0027-stage-1-software-core-closeout.md)
- [Stage 1 GPL/source/dependency license 初审](../development/stage1-license-initial-review.md)
- [Runtime + Controller/Serial 实现矩阵](../development/runtime-controller-vertical-slice.md)
- [目标仓库与实施路线](../architecture/repository-roadmap.md)
