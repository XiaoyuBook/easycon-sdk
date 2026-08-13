# 0027：Stage 1 Runtime/Controller/Vision 软件核心收口候选

- 状态：Stage 1 Software Core Closeout Candidate / Pending Stage Review
- 日期：2026-08-13
- production implementation input：`f99333f1e4d359af4588a659e2890bcfd58483de`
- production input parent：`be4c2cdfe9ffcdfe9de35cdda032502d2a63e89c`
- production input tree：`cc9a6040d000be9c070f586b04bb20d2be75a685`
- approved H validation input：`df13db4cb78c14602e05a31636a3e3a8f277f873`
- H input parent：`8edc700a91dc02fbe58833126954881a2ab0de22`
- H input tree：`a3819e469c9dc629876b84835fccbabcc73ccc8e`
- R document candidate 直接 parent 约束：`df13db4cb78c14602e05a31636a3e3a8f277f873`
- R formal base：`3f1481480b3ee4206aa75a248a001213f960fcb6`
- Controller 候选决议：[ADR-0026](0026-controller-d1-settlement-refreeze.md)
- Runtime 决议：[ADR-0024](0024-runtime-r0-v2-refreeze.md)
- Vision 既有冻结记录：[ADR-0013](0013-phase-3-vision-cross-platform-source-candidate-freeze.md)、
  [ADR-0014](0014-phase-3-native-pool-admission-refreeze.md) 和
  [ADR-0016](0016-phase-3-downstream-reopen-boundary.md)
- 候选验证分层：[ADR-0028](0028-windows-candidate-gate-layering.md)
- 许可证初审：[Stage 1 GPL/source/dependency license 初审](../development/stage1-license-initial-review.md)

## 决议候选与对象绑定

ADR-0023 将当前 v1 第一阶段定义为 Runtime、Controller 与 Vision 软件核心收口。Runtime R0-v2 已由 ADR-0024
独立重新冻结。Controller production implementation 固定为 `f99333f1e4d359af4588a659e2890bcfd58483de`；
validation lineage `f99333f1 <- 8edc700a <- df13db4c` 先增加 debug-only deterministic rendezvous 与相关测试，
再实现 ADR-0028 的 gate layering、classifier、runner 与 CI hardening。独立 Task reviewer 对最终 H input
`df13db4cb78c14602e05a31636a3e3a8f277f873` 给出 `APPROVE`，P0/P1/P2/P3=`0/0/0/0`。H SHA/tree
不替代 production implementation object。Vision private native、capture、image、OCR、template 与 color 软件路径保留
ADR-0014 的现行 implementation/refreeze 和 ADR-0016 的重开边界。

本文件不写尚未知晓的 R commit SHA/tree，不预写未来 Stage reviewer verdict，也不声称 canonical integration 已发生。
承载本决议候选的 R commit 必须同时满足：

1. 唯一直接 parent 是 `df13db4cb78c14602e05a31636a3e3a8f277f873`；
2. commit tree 与以 formal base `3f1481480b3ee4206aa75a248a001213f960fcb6` 运行的唯一 fresh staged-candidate
   A evidence tree 完全一致；
3. 独立 Stage reviewer 对同一固定 R SHA/tree、完整 `base..R` delta、八类退出矩阵、许可证选择和范围排除做静态、
   binding 与产品资格审查；reviewer 不运行 Workspace、不生成 clean-tree credential、不 replay A；
4. Stage reviewer `APPROVE` 是关闭 Stage 1 software core 的唯一指标；只有用户明确授权后才可集成 canonical `main`。

在第 4 项完成前，本对象只是 `Pending Stage Review` 的 R document candidate，不是 `Stage 1 Closed / Effective`，
不是 canonical `main` 已集成状态，也不授权自动开始、提交或集成 Stage 2 实现。

## Stage 1 退出矩阵

| # | 退出条件 | 直接 tracked evidence | 候选结论 |
| --- | --- | --- | --- |
| 1 | Runtime、Controller、Vision 状态机、错误和关闭顺序由实现测试覆盖 | [Runtime lifecycle](../architecture/runtime-lifecycle.md)、[Runtime 实现测试](../../crates/easycon-runtime/src/runtime.rs)、[Controller lease tests](../../tests/support/tests/controller_lease.rs)、[Vision capture contract](../../crates/easycon-vision/tests/capture_contract.rs) | 满足 software candidate；覆盖 operation/resource/task ownership、settlement、错误与 close 顺序。 |
| 2 | fake serial/native 可运行核心共同场景 | [vertical slice tests](../../tests/support/tests/vertical_slice.rs)、[CH32 simulator tests](../../tests/support/tests/serial_ch32.rs)、[native capture contract](../../crates/easycon-vision/tests/native_capture_contract.rs) | 满足 software candidate；不依赖物理硬件或 `EasyCon/`。 |
| 3 | Controller report、`ActionSequence` 与 Vision fixture 分类明确 | [Controller reports](../../spec/fixtures/controller/reports-v1.json)、[sequence traces](../../spec/fixtures/controller/sequence-traces-v1.json)、[Vision native design](../development/phase3-vision-native-design.md) | 满足；source-exact、corrected、new 与 excluded 边界保持显式。 |
| 4 | lease、取消、中立化、stream settlement 与精确时序成功/失败边界覆盖 | [Runtime/Controller conformance](../../spec/conformance/runtime-controller-v1.json)、[Controller settlement fixture](../../spec/fixtures/controller/lease-settlement-v1.json)、[Controller lease tests](../../tests/support/tests/controller_lease.rs)、[sequence ACK tests](../../tests/support/tests/sequence_ack.rs)、[Windows completion exact](../../crates/easycon-serial/src/windows/io.rs) | 满足 software candidate；40 项 D1 obligation、9 项 Controller settlement scenario assertion 和 absolute monotonic timeline 均映射到 exact tests；generation release 与 admission/dispatch 竞争覆盖 `Outstanding` + `ERROR_NOT_FOUND` + full completion，保留 accepted winner 且 release/close 各只中立化一次。 |
| 5 | Capture read 可中断；正常 Runtime close 后无活动 task/resource/native handle | [capture contract](../../crates/easycon-vision/tests/capture_contract.rs)、[OCR pool contract](../../crates/easycon-vision/tests/ocr_pool_contract.rs)、[vertical slice counts](../../tests/support/tests/vertical_slice.rs) | 满足成功 `Closed` 路径。owner-loss 无 transferable owner 且 settlement 不可证明时，保留非终态 record 和非零 registry 并返回 `CloseFailed` 是正确 fail-closed 行为，不能虚写为所有失败均清零。 |
| 6 | C++ exception 与 Rust panic 隔离，无跨 ABI unwind | [native guard](../../native/bridge/src/common/bridge_internal.hpp)、[bridge component tests](../../native/bridge/tests/bridge_component_tests.cpp)、[Runtime panic tests](../../crates/easycon-runtime/src/runtime.rs)、[capture panic tests](../../crates/easycon-vision/tests/capture_contract.rs) | 满足 private native/Rust software boundary；不等于 public C ABI 已实现或冻结。 |
| 7 | operation/status/error/event/struct/ownership/blocking 文档不依赖 binding 猜测 | [Runtime lifecycle](../architecture/runtime-lifecycle.md)、[C ABI v1 design](../architecture/c-abi-v1.md) | 满足；`operation_wait` 只观察既有 operation，独立 workflow `wait()`/delay/sleep 继续排除。 |
| 8 | GPL 来源、版权与 dependency license 完成初审 | [Stage 1 license initial review](../development/stage1-license-initial-review.md)、[GPL-3.0-only](../../LICENSE)、[source boundary](0001-source-boundary.md) | 满足 Stage 1 initial-review candidate：workspace license 9/9、Rust registry 41/41、native 17/17、OCR 2/2 双向闭包；不是 SBOM、notices、release scan、source archive 或法律意见。 |

2026-08-12 的直接产品与 validator 结果实际来自 `8edc700a91dc02fbe58833126954881a2ab0de22`：Runtime Loom
12/12；冻结 validator 为 8 schemas、1 behavior spec、1 Runtime fixture、4 controller fixtures、15 Vision binary fixtures、
1 capture manifest、24 label corpus entries、11 dormant ECS provenance records（33 SDK-local artifacts）、10 conformance
scenarios 和 129 exact Rust tests；Controller `controller_lease`、`serial_ch32`、`sequence_ack` 三个受控相关 targets
均通过。

`df13db4c` 没有修改 product source/tests、`spec/**`、`tools/validate_specs.py`、`tools/run_runtime_models.py`、
`tools/check_markdown_links.py`、Cargo/native/license fixed inputs，因此上述 provenance 继续适用于 approved H input；本文
不声称在 `df13db4c` 上重新运行了这些直接检查。repository guards、policy、runner 与 CI 的迁移由 H `APPROVE`
覆盖，并由本 R tree 的唯一 staged A 实际覆盖。Stage reviewer 只核验该 A binding 并做静态/产品资格审查，不 replay A。

## 候选关闭范围

本候选提议关闭且只关闭：

- Windows x64 上 Runtime、Controller、Vision 的 software core；
- fake serial、virtual clock、synthetic/file native backend、private C++ bridge 与对应 Rust/native software tests；
- Controller D1 software settlement、`ActionSequence` software timeline，以及 Vision capture/image/OCR/template/color
  software lifecycle；
- Stage 1 所需的 GPL/source/dependency license 初审记录。

本候选不改变 Stage 2 的实现状态。public C ABI、generated header、symbol/layout golden、canonical native bundle 和
四语言低层表达检查仍未形成候选。

## 保持开放的资格与产品范围

- O-01：受支持控制板、固件和 USB/serial identity；
- O-02：真实 Amiibo slot 与长度 capability；
- O-03：官方 package 内 OCR language model 及其发行资格；
- O-04：真实物理动作时序 SLO；
- O-05、O-06：Linux x64 与 macOS arm64 promotion；
- 真实 Controller/capture hardware、hardware soak、public C ABI、bindings、packages、ABI compatibility；
- per-package notices、SBOM、source archive、checksum、signing、release candidate、tag 与 release；
- ECS 与 Automation 产品范围。

现有 ECS crate/spec/fixture/conformance/validator/guards 继续作为 dormant workspace maintenance 资产参加仓库健康门禁，
但不因 Stage 1 candidate 恢复为 v1 产品能力。`EasyCon/` 继续只作为 ignored、只读 reference source，不进入 workspace、
build、candidate 或 release input。

## 重新打开规则

Runtime、Controller 或 Vision production/contract/frozen asset 的变化，approved H validation input 失效，Stage 1 exit 1--8
任一 evidence 失效，license source/compatibility conclusion 变化，新的 in-scope P0/P1/P2 finding，或任何扩大上述候选
范围的 support statement，都会重新打开受影响的 Stage 1 candidate，并要求新的唯一 staged-candidate A 与独立 Stage
review。纯索引修正不得改变 production SHA/tree、approved H input、R credential 或开放资格边界。

## 关联

- [ADR-0023：v1 宿主语言 SDK 路线与 ECS 延后](0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md)
- [Runtime + Controller/Serial 实现矩阵](../development/runtime-controller-vertical-slice.md)
- [目标仓库与实施路线](../architecture/repository-roadmap.md)
- [Spec index](../../spec/README.md)
