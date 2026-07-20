# 0007：冻结 Phase 1 Runtime 基线

- 状态：Frozen
- 日期：2026-07-20
- 实现基线：`4261925dc4e84b36e8491c0a97c17048d3eacd84`

## 背景

Phase 1 的目标是建立 `easycon-model`、`easycon-runtime`、测试支撑和 VirtualClock，并固定
operation、取消树、事件 subscription、resource/task registry、panic isolation 与确定性关闭语义。
[ADR-0006](0006-runtime-stabilization.md) 已冻结 Runtime 的所有权和终态协议；后续实现与 review 又关闭了
事件 subscription 并发 reader、取消 hook 析构故障和 hook capture 析构重入等缺口。

仓库同时包含一段无硬件 Controller fake vertical slice。它复用了 Phase 1 Runtime，但 Windows serial、
Amiibo、10,000-step fake、硬件 characterization 和 O-01/O-02 数据尚未完成，因此不能把这段提前实现
表述为完整 Phase 2。

## 决策

1. Phase 1 的可执行实现冻结在
   `4261925dc4e84b36e8491c0a97c17048d3eacd84`。包含本 ADR 的后续提交只记录冻结决定，不改变该可执行
   基线。
2. 冻结范围包括：
   - `easycon-model` 的 Runtime/operation/resource/task ID、稳定错误模型和 Controller 基础值类型；
   - `easycon-runtime` 的 operation 状态机、取消树、deadline/clock、受监管任务、资源 registry、事件队列、
     显式 close、限定 Drop 语义和 panic/poison 隔离；
   - Phase 1 对应的 behavior、conformance、Loom 模型、fault tests 和测试工具。
3. 冻结不代表 Rust public item、正式 C ABI 或任何语言 SDK 已获得兼容承诺。正式 ABI 只能在 Phase 5
   按 ABI manifest、header、symbol 和 smoke 门槛冻结。
4. 当前 `easycon-controller` 的 report/protocol、FakeTransport、单写者 lane、精确序列、lease 和通用 ACK
   是 Phase 2 的提前 slice，不属于本次冻结完成声明。
5. Phase 2 及后续实现必须复用该 Runtime 语义，不得旁路 operation、取消、事件、task/resource ownership
   或确定性 close，也不得恢复额外服务进程架构。

## 冻结证据

固定基线上的独立 review 复核了 operation 终态、取消传播、hook/capture destructor、child admission、
task join、CloseFailed、事件顺序、conformance 映射和 Loom 生产共享原语。在该范围和下列验证下，没有
发现可复现且可行动的 in-scope P0/P1/P2 finding：

- `cargo fmt --all --check`；
- `cargo check --workspace --all-targets`；
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`；
- `cargo test --workspace --all-features`：118 个非文档测试通过，其中 Runtime 68 个、Loom 6 个；
- `python tools/run_runtime_models.py`：6/6 个独立 Loom 模型通过；
- `python tools/validate_specs.py`：6 个 conformance 场景映射的 39 个精确 Rust 测试通过；
- Markdown 链接、repository guard 和 `git diff --check` 通过；
- 外层工作树干净，`EasyCon/` 保持 ignored 且参考源码无改动。

验证不能证明不存在未知缺陷。未进入 Phase 1 范围的物理串口、硬件时序、Amiibo、C ABI、sanitizer、
fuzz 和长稳测试仍由后续阶段各自关闭。

## 重新打开规则

- 普通文档澄清且不改变可执行行为，不重新打开 Phase 1。
- Phase 1 生产代码、行为规范、schema、fixture、conformance 或并发模型发生变化时，Phase 1 自动视为
  reopened，直到回归测试、完整门禁和新基线独立 review 通过，并由后续冻结记录推进实现基线。
- 保持既有语义的 bug fix 不要求重写架构来迁就实现；改变 Runtime/Operation/事件/关闭模型仍必须新增 ADR。
- 任何冻结后修复都必须遵守“回归测试先于根因修复”，不得用静态检查或一次测试通过替代竞态证据。

本阶段是内部实现里程碑，不创建 release/ABI tag；完整 Git SHA 是唯一冻结基线标识。

## 关联

- [ADR-0004：操作句柄、拉取事件与确定性关闭](0004-operations-events-shutdown.md)
- [ADR-0006：Runtime 所有权、终态事务与确定性关闭](0006-runtime-stabilization.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
- [Runtime + Controller fake vertical slice](../development/runtime-controller-vertical-slice.md)
- [实施路线](../architecture/repository-roadmap.md)
