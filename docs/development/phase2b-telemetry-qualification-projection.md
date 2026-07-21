# Phase 2B telemetry 与资格投影设计

- 状态：Architecture Target (`Hardware Unverified`)
- 日期：2026-07-21
- 设计基线：`c889e3c`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 前置设计：[durable evidence transaction](phase2b-durable-evidence-transaction.md)
- 相邻设计：[操作员、取消与观察](phase2b-operator-cancellation-observation.md)

## 问题与边界

当前 `tests/hardware` 只在一个 report 的最后一次 partial write 成功时追加 `TimingSample`。记录中的
`write_entered_ns` 因而是最后一段而不是第一段；失败的 logical report 完全消失，operation ID 和分段数也没有进入
CSV。`latency_json` 又用 `saturating_sub` 计算阶段耗时，逆序时间会被压成 0 而不是显式失败。native open error 已在
`ByteIoFactory` 边界保留，但 Controller write 的 `SerialErrorKind` 和 OS code 会在 production transport 映射成
`TransportError` 前丢失。

最终 document 还同时保存可变的 `status` 别名、`execution_status` 和 `qualification_status`，退出码在提交前临时
推导。三处可以漂移，也没有把退出码作为被 manifest 哈希的 projection 保存。

本设计只修复 qualification CLI 的软件 telemetry、证据完整性和终态投影。它不改变 Phase 2A production
Controller/Serial 契约，不重写 Phase 2A memory-transport fixture，不执行硬件动作，也不把 OS write acceptance
冒充 UART、USB HID、Switch 总线或物理动作证据。O-01、O-02、O-04 保持开放。

## Logical report 状态机

`ObservedTransport` 继续是 qualification-only 的 logical-write observer。它只聚合 `WriteKind::Report`；command 与
neutralization 保持各自已有的协议和 cleanup evidence owner。每个 report 由完整 `WriteContext` 标识，并固定为：

```text
Absent
  -> Pending(first entry, context, accepted = 0, partial_count = 0)
  -> Pending(contiguous accepted prefix, partial_count += 1)
  -> Accepted(final acceptance timestamp)
  -> Failed(mapped transport error, optional native error)
  -> Contradiction(stable diagnostic)
```

同一 `WriteContext.sequence` 只能有一个 attempt。第一次进入 transport 时保存
`first_write_entered_ns`；每个成功返回的非零 segment 都令 `partial_count` 加一，所以 3+5 byte 写的
`partial_count = 2`。只有累计 accepted bytes 精确等于 `total_len` 时才保存 `transport_accepted_ns`。失败 attempt
保存最后已知 accepted prefix、返回时间和结构化 mapped transport error，不能伪造 final acceptance。

以下任一情况把 attempt 固定为 contradiction，并令相关资格失败：

- sequence 为零、report 没有 operation ID、total length 为零；
- 同一 sequence 的 context 改变、prefix 不连续、terminal 后恢复、重复 terminal；
- transport 返回零、超过 remaining、累计溢出或超过 total length；
- 新 logical report sequence 不严格递增；
- direct timing 不是 `command admitted <= lane wake <= dispatch <= first entry`；
- 任一 call 的 entry 早于前一 call return，或 return 早于本次 entry；
- final acceptance 早于 first entry。

checked arithmetic 失败和时间逆序都保存稳定 contradiction code；不能用 saturating arithmetic、排序或删样本
恢复。`ObservedTransport` 仍原样返回 inner result，observer 不能改变 production operation 结果。

## Native byte-I/O 观察

`ObservedByteIoFactory` 在 successful open 后返回一个 qualification-only `ObservedByteIo` decorator。decorator 保持
同一 inner `ByteIo` owner，并原样转发 read、discard、close。write 返回 `SerialError` 时，它在 error 被
`SerialControllerTransport` lossy mapping 前检查 `ByteIoOperation::ControllerWrite(context)`，把以下字段绑定到同一
logical attempt：

- stable `SerialErrorKind`；
- optional unsigned OS code；
- diagnostic message；
- resource、operation 和 write sequence。

一次 logical report 最多接受一个 terminal native error。context 无 matching pending attempt、error 错绑、重复或在
accepted terminal 后到达都成为 contradiction。由 production transport 在进入 ByteIo 前产生的 deadline/cancel/
disconnected error没有 native backend error，明确保存为 `native_error = null`；工具不得解析 mapped message 反推
native code。open attempts 继续使用现有独立结构，不与 logical reports 混合。

## JSON 与 CSV projection

内存 telemetry 保存全部 attempt，不丢弃 failed 或 contradiction。bounded smoke/home-wake final JSON 保存完整
logical report rows；sequence 的完整 rows 写入 `sequence-timings.csv`，final JSON 只保存计数、完整性和 auxiliary
引用，避免把 10,000 rows 再复制进 journal projection。

每个 row 至少包含：

- `resource_id`、`operation_id`、`write_sequence` 和 `total_bytes`；
- optional `command_admitted_ns`、`lane_wake_ns`，以及 `dispatch_ns`；
- `first_write_entered_ns`、optional `transport_accepted_ns`、`last_returned_ns`；
- `partial_count`、`accepted_bytes`、`outcome`；
- optional structured mapped transport error、native serial error和 contradiction。

CSV 使用 RFC 4180 writer，不手工拼接可能含逗号、引号或换行的 message。固定 header 与 tracked qualification
fixture 一致。sequence 无论 success、operation failure、cancel 或 cleanup failure，只要 reservation 已建立，都在
finalization 前从内存 attempt projection 生成并 stage CSV；空或部分 CSV 是有效的失败前缀证据，不能只在成功路径
产生。CSV render error 是 artifact execution failure。

`latency` projection 只从 outcome 为 accepted 且时间完整、单调的 rows 计算分布；它同时保存 total/accepted/failed/
contradiction counts。存在任何 contradiction 时 integrity 为 failed，分布不能令资格通过。failed rows 保留但不进入
成功延迟分布，也不被描述成 filtered outlier。

## 物理边界

资格 projection 固定保存三层边界：

- `uart_complete_frame` 是按 baud、8 bytes、8N1 计算的理论值，`measured = false`；
- `usb_hid` 为 `unverified`，`measured = false`，原因是没有 analyzer 或可审计 firmware trace；
- `switch_physical_order` 为 `unverified`，`measured = false`，原因是没有 analyzer/firmware trace 与逐动作物理观察的
  共同顺序证据。

OS acceptance 只命名为 `transport_accepted_ns` 或 `os_write_acceptance`。任何字段、check 或文档都不得称其为 UART
完成、USB 到达、Switch 到达或端到端 latency。sequence 即使软件 rows 全部有效，缺少物理顺序证据仍为
`unverified`。

## 唯一终态投影

实现增加一个 typed `RunOutcome`，它是 execution、qualification 和 exit code 的唯一 owner：

| outcome | execution | qualification | exit |
| --- | --- | --- | ---: |
| `Passed` | `completed` | `passed` | 0 |
| `QualificationFailed` | `completed` | `failed` | 1 |
| `ExecutionFailed` | `failed` | `failed` | 1 |
| `NotRun` | `completed` | `not_run` | 2 |
| `Unverified` | `completed` | `unverified` | 2 |
| `Cancelled` | `cancelled` | `unverified` | 130 |

final document schema 升为 v2，只保存 `execution_status`、`qualification_status` 和 `exit_code`；删除没有 ADR 语义且
可漂移的 top-level `status` 别名。command 内部的 scenario/action `status` 不受影响。退出前重新验证 document 三元组
与 typed outcome 精确一致；未知、缺失或矛盾组合 fail closed 为 1，不能让 `cancelled/failed` 等非法组合得到 130。

cleanup/execution failure 优先于 operator cancellation；operator cancellation 优先于 qualification decision；build
provenance 只可把 `Passed` 降为 `Unverified`。任何机器判据、operator no 或 telemetry integrity failure使用
`QualificationFailed`，not-run/unverified 不携带伪 failure。final JSON、journal projection 和 process exit 都消费同一
typed outcome。

## Fixture 与 conformance

Phase 2B 尚未冻结为 root workspace/public behavior，因此本任务不修改 Phase 2A 的
`spec/fixtures/controller/phase2a-latency-result-v1.json` 或其 schema。新增的 tracked hardware qualification fixture
位于 `tests/hardware/fixtures/phase2b-qualification-projection-v1.json`，由独立 workspace test 直接读取，固定：

- 六个合法 outcome 三元组及全部非法组合 fail-closed 规则；
- logical report JSON 字段和 CSV header；
- single 8-byte、3+5 byte、3-byte then native failure、时间逆序四个 synthetic case；
- UART/USB/Switch 的 measured/unverified 边界。

conformance test 必须把 fixture 的每个 case 经真实 projection code 执行，不复制一套测试专用映射。native failure
使用 fake `ByteIo` 注入 `SerialErrorKind` 和 OS code；时间使用 scripted `Clock` 或直接 observer boundary，不使用随机
sleep。fixture 只描述 synthetic 软件结果，不含真实 identity、COM、机器路径或物理通过结论。

## 实现与验证

实现按一个行为节点提交，先增加旧实现稳定失败的最小回归，再修改根因：

1. 3+5 byte 保留 first entry、final acceptance、operation/sequence 和 `partial_count = 2`；
2. partial then native failure 保留 prefix、mapped error、`SerialErrorKind` 和 OS code；
3. context/prefix/time reversal 与 terminal-after-terminal 都产生 contradiction 并使资格失败；
4. sequence failure/cancel/cleanup failure 仍渲染完整或部分 CSV；
5. 六个合法终态、非法组合、provenance downgrade 和退出码与 fixture 一致；
6. UART 理论值以及 USB/Switch 未验证字段不能被软件 acceptance 升级。

提交前依次执行根 workspace 完整门禁、Runtime models、Python validators，以及 `tests/hardware` 独立 workspace 的
fmt/check/strict clippy/test。实现 SHA 固定后重新做整体 review；直接相关可复现 finding 必须先回归、修复、门禁和
新基线复审。

## 排除项与重新打开规则

本设计不实现 checkpoint 聚合、支持矩阵、硬件 attestation、物理 measurement 或 Phase 3。以下变化必须先重开
设计或 ADR-0010：

- 以最后 partial entry 代替 first entry，丢弃 failed row，或用 saturating arithmetic 隐藏逆序；
- 从 mapped error message 解析 native kind/code，或为没有 backend error 的路径伪造 OS code；
- 只在 sequence 成功时发布 CSV，或让 CSV 与 final JSON 使用不同 telemetry owner；
- 把 UART 理论值标为 measured，或把 OS acceptance 升级为 USB/Switch/物理顺序证据；
- 恢复第三套 top-level status、允许非法状态组合，或让 manifest 中 document 的 `exit_code` 与进程退出不一致；
- 修改 Phase 2A memory-transport latency fixture来迎合 Phase 2B hardware qualification 实现。
