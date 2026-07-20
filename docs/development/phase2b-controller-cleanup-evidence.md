# Phase 2B Controller close 中立化证据设计

- 状态：Implementation Target (`Hardware Unverified`)
- 日期：2026-07-20
- 设计基线：`92928b15ffee721b99ee4e0049dc8ebd2efc6fbd`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 相邻设计：[Phase 2B faults runner 所有权](phase2b-fault-runner.md)

## 问题与源码事实

当前 `tests/hardware` 的 `Harness::close` 先调用 `ControllerSession::close()`，再把
`Runtime::close()` 和最终 registry counts 投影成 cleanup JSON。这个顺序正确，但现有结果只证明 Runtime
完成资源回调和 registry 收敛，不能证明 Controller 的最终中立报告被 transport 接受。

production Controller 的冻结行为如下：

1. `ControllerSession::close()` 同步等待单写者 lane 退出，返回类型是 `()`。
2. lane 在关闭时先把 desired report 重置为中立；若关闭前处于 `Connected`，再发送一个
   `WriteKind::Neutralize` 报告。
3. 最终 close 中立写的 `WriteContext.operation_id` 为 `None`；operation cancel 等中立写仍绑定对应
   `OperationId`。
4. 中立写失败时 Controller 发布 `controller.neutralization.not_delivered` warning，随后仍关闭 transport、
   释放 lease 并进入 `Closed`。因此 `desired_report_neutral = true`、Controller `Closed`、Runtime
   `CloseOutcome::Closed` 和零 counts 可以同时成立，而物理中立报告并未送达。
5. Runtime event subscription 是有界、pull-based 的观察面，不是 durable history。只检查 Runtime close
   结果或 warning 的缺席都不能构成完整 transport acceptance 证据。
6. hardware CLI 已有私有 `ObservedTransport`，它逐次收到完整 `WriteContext` 和 transport 的原始
   `Result<usize, TransportError>`。该边界可以观察逻辑中立写的 partial progress 和最终错误，而无需改变
   production Controller、Serial 或 Runtime API。

所以旧实现存在确定性 P1 伪通过路径：连接成功后最终 close 中立写失败，Controller 和 Runtime 仍正常关闭，
`cleanup_succeeded` 仍返回 true。`handshake`、`smoke`、`home-wake`、faults 中实际 Connected 的 session、
hotplug 的 reconnect session、每个 lifecycle cycle、`sequence` 和实际写入的 `amiibo` 都受影响。`discover`
不创建 Harness，未授权而未执行写入的 Amiibo 也不受此特定缺陷影响。该缺口只影响资格证据，不重新打开
Phase 2A 的 production 语义。

## 范围

本任务只完成以下内容：

- hardware-only transport decorator 对逻辑中立写的结构化观察；
- 最终 Controller close 与 Runtime close 的组合 cleanup projection；
- connected、disconnected、完整接受、partial failure 和矛盾证据的无硬件回归；
- faults runner 和其他 Harness owner 共同消费同一 cleanup contract。

本任务不实现 durable ledger、stable identity guard、Ctrl+C、`OperatorPort`、Amiibo journal、manifest、
checkpoint 或物理复测，也不修改 Controller 的 close 返回类型、事件契约、中立化顺序或 serial error mapping。

## 组件与所有权

### `ObservedTransport`

`ObservedTransport` 继续只存在于 `tests/hardware`，并成为中立写 transport evidence 的唯一 owner。它在调用
inner transport 前取得 `WriteContext` 和剩余长度，在 inner 返回后原样返回结果，同时更新共享 `Telemetry`。
观察器不得重试、取消、吞掉或改写 inner 结果。

为允许 deterministic synthetic test，inner 边界可从具体 `SerialControllerTransport` 收窄为
`Box<dyn ControllerTransport>`；production 构造仍只包装同一个 `SerialControllerTransport`。这不是新的
运行时抽象，也不进入 release crate。

### `NeutralizationAttempt`

Telemetry 按 `WriteContext.sequence` 聚合一个逻辑中立写，至少保存：

- `sequence`、可选 `operation_id`、`total_bytes`；
- 首次进入时的 Runtime-clock timestamp；
- 已接受的连续 prefix 字节数；
- `accepted` 或 `failed` 终态；
- failure 的稳定 `TransportErrorKind` 和非空诊断 message。

同一 sequence 的 partial calls 必须保持 context、total length 和 prefix 连续。整数溢出、重复终态、越界写入、
context 改变或 close 返回后仍非终态都标记为 observer contradiction。观察器仍返回 inner 的原结果，但本次
cleanup 证据必须 fail closed。

普通 `WriteKind::Report` 的高频 timing 路径不增加此聚合；`WriteKind::Command` 也不进入中立化集合。

### `Harness`

`Harness` 仍拥有一个 Runtime、一个 Controller、一个 transport telemetry 和一个 descriptor。显式 close 的
唯一顺序固定为：

1. 保存 close 前 Controller snapshot 和当前中立写证据边界；
2. 保存当前 `ControllerSession::id()`，作为所有 attempt 的 Runtime-local resource anchor；
3. 同步调用 `ControllerSession::close()`；
4. 保存 close 后 snapshot，并固定本次 close 新增的中立写尝试；
5. 调用 `Runtime::close()`，再读取最终 counts；
6. 组合 Controller 与 Runtime 子证据，最后计算 outer cleanup 结果。

faults runner 的 consuming close 继续在同一步骤之后刷新 native open attempts 和 active operation 终态。
cleanup failure 进入现有 role-specific close stage；若已有更早 execution failure，只追加到
`cleanup_errors`，不得覆盖首错。

## 最终 close 识别规则

最终 close 中立写由以下全部条件识别：

- `kind == WriteKind::Neutralize`；
- `operation_id == None`；
- sequence 位于本次 Harness close 固定的证据范围内。

operation cancel、disconnect recovery 或 sequence cleanup 的 `operation_id = Some(...)` 中立写必须保留为
普通 telemetry，但不能冒充最终 close 成功。不能通过 error message、desired snapshot 或 accepted report
总计数推断 close 中立化。

Controller close 前状态为 `Connected` 时，中立化是 `required`。通过要求恰好一个逻辑最终 close attempt，
其 context 连续、`total_bytes == 8`、`accepted_bytes == total_bytes`、终态为 `accepted` 且没有结构化错误。

close 前状态不是 `Connected` 时，中立化标记为 `not_required`，原因保存精确 pre-close state，并要求没有
最终 close attempt。典型 hotplug 断线属于该分支；它不能被描述成“中立报告已送达”。若状态与实际 attempt
矛盾，证据失败而不是猜测设备状态。

无论是否需要写入，post-close Controller state 都必须为 `Closed`、lease 必须为 `Available`、desired report
必须为中立。它们证明内存和 owner 收口，但不替代 transport acceptance。

## Cleanup projection

每个 Harness cleanup 使用一个组合对象，不再把 Runtime 子结果伪装成全部 cleanup：

```json
{
  "kind": "harness_cleanup",
  "succeeded": true,
  "controller": {
    "kind": "controller_cleanup",
    "succeeded": true,
    "controller_resource_id": 17,
    "pre_close_state": "Connected",
    "post_close_state": "Closed",
    "post_close_desired_report_neutral": true,
    "post_close_lease": "Available",
    "neutralization": "accepted",
    "attempts": [
      {
        "resource_id": 17,
        "sequence": 9,
        "operation_id": null,
        "total_bytes": 8,
        "accepted_bytes": 8,
        "outcome": "accepted",
        "structured_error": null
      }
    ]
  },
  "runtime": {
    "kind": "runtime_cleanup",
    "succeeded": true,
    "outcome": "Closed",
    "counts": {
      "active_operations": 0,
      "active_resources": 0,
      "active_tasks": 0
    }
  }
}
```

`controller` 和 `runtime` 字段名故意不以 `_cleanup` 结尾：现有结果树的 cleanup slot 统计仍把一个 Harness
计算为一个 owner slot；nested `kind = runtime_cleanup` 仍让 Runtime cleanup 计数精确为一。

outer `succeeded` 只在两个子 validator 都通过时为 true。`cleanup_succeeded` 必须从 nested evidence
重新计算并核对 outer 值，不能信任单独布尔值。缺字段、旧版扁平 `runtime_cleanup`、多余/缺失 attempt、
错误 operation binding、partial prefix、observer contradiction、Runtime 非 `Closed` 或非零 counts 均失败。
`controller_resource_id` 必须存在、非零，并与每个 attempt 的 `resource_id` 精确相等；只要求 attempts 彼此
相同不足以证明它们属于当前 Harness。

JSON validator 对本版本 projection 使用 exact branch schema。声明为 nullable 的 `structured_error`、
`diagnostic` 等字段必须实际存在；缺失 key 不能利用 Serde 索引返回的隐式 `Null` 冒充显式 `null`。Runtime
`Closed` 成功分支只允许显式 null diagnostic，不能同时出现 `report` 或 `rejection`；`Failed` 和 `Rejected`
分支的互斥结构同理。outer、Controller、attempt、structured error、Runtime 和 counts 对象都拒绝缺失或
分支矛盾字段。

cleanup failure 仍产生 `execution_status = failed`、`qualification_status = failed` 和退出码 1。完整结构化
attempt 与 Runtime report 保留在结果中；机器判据不解析诊断 message。

## 状态、并发、取消与 deadline

- transport write 仍只有 Controller lane 单写者。Telemetry mutex 只保护短小的观察记录，不跨 inner I/O、
  operation wait、Controller close 或 Runtime close 持有。
- Harness close 是单 owner、单次显式调用；faults 使用 consuming close，其他现有命令维持唯一显式 close。
- operation cancel/deadline 的终态和中立写保持原 owner。最终 close attempt 的 `operation_id = None` 不得
  反向写入任何 operation 结果。
- protocol timeout、operation deadline 和 caller wait timeout 不因本任务合并。observer 自身没有 timer、
  thread、callback 或 detached task。
- Controller close 返回后 writer 已 join，因此随后固定 telemetry 不需要 sleep 或经验性 settle。

## 错误与资源关闭

结构化 transport error 保存稳定 kind 和 message；message 只用于诊断。native OS code 仍由后续 ByteIo ledger
任务完成，不能从 message 反向解析。

Controller 中立写失败不能阻止 Runtime close。Harness 必须继续完成 Runtime close 和 counts 采样，以同时
保留物理安全失败与资源释放结果。Runtime close failure也不能删除 Controller attempt。两项同时失败时都进入
同一个 cleanup object，outer failure 不选择性隐藏任何子证据。

Drop 仍只是 panic/意外返回防线，不产生资格 cleanup evidence。没有任何新析构、后台 worker 或跨平台句柄。

## 平台、打包与性能边界

实现只修改 `publish = false` 的 `tests/hardware` workspace，不进入根 workspace dependency graph、native bundle、
C ABI、binding 或 package。system serial 仍只在 Windows production Harness 构造中出现；synthetic tests 不打开
串口。

聚合只处理低频 `Neutralize` writes，空间与锁成本随本次 Harness 的中立化次数线性增长。高频 sequence report
和 Phase 2A latency 路径不增加记录，因此本任务不产生新的性能声明，也不关闭 O-04。

## 测试设计

实现前先加入旧基线确定失败的最小回归：使用真实 `ControllerSession` 和 synthetic transport，完成 connect 后
直接 close；只让 `WriteKind::Neutralize + operation_id = None` 返回稳定 transport error。旧实现会给出 Runtime
`Closed`、零 counts 和 cleanup passed；新契约必须保存 failed attempt 并令 cleanup、document 和退出码失败。
partial-then-failure 作为紧邻的第二个 case 覆盖 accepted prefix，不靠 sleep 或普通 report 制造条件。

随后至少覆盖：

1. connected close 完整接受恰好一个 `operation_id = null` 的 8-byte logical attempt；
2. first-call failure 和 partial-then-failure 均保存 accepted prefix、稳定 error 并失败；
3. disconnected close 为 `not_required`，保存精确 pre-state，不能声称 delivered；
4. operation-bound neutralization 不能替代缺失的 final close attempt；
5. post-close state、lease 或 desired snapshot 矛盾时 fail closed；
6. Runtime `CloseOutcome::Failed` 与 Controller failure 同时保留；
7. faults 四角色 success/failpoint projection 迁移后，cleanup slot 与 nested Runtime count 仍精确；
8. malformed/legacy cleanup JSON、缺失 nullable key、互斥 Runtime 分支、outer/inner succeeded 矛盾、
   attempt 数量、operation ID 或 foreign resource ID 畸形均非零退出；
9. production Harness adapter 证明 active operation 被 cancel、最终 close neutral 被接受、lease 释放、Controller
   `Closed`、Runtime counts 归零；
10. 测试只使用 synthetic transport，不枚举或打开当前机器串口。

每次实现提交执行根 workspace 九项门禁和 `tests/hardware` 的 fmt/check/strict clippy/test。固定实现 SHA 后做
新的独立 review；任何修复形成新基线并重新 review。

## 开放项与重新打开规则

本设计不声称目标 CH32 实际接收中立报告。真实设备、UART 与 Switch 可见中立化仍为 `Hardware Unverified`，
O-01/O-04 保持开放。

以下变化必须先重新审查本设计和 ADR-0010：

- 用 snapshot、event absence 或 error message 代替 transport acceptance；
- 把 operation-bound neutralization 当作最终 close attempt；
- 中立写失败后跳过 Runtime close；
- 修改 production Controller close/neutralization/error 语义；
- 把 hardware telemetry 或设备限定行为加入 release crate。
