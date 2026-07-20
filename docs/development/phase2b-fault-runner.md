# Phase 2B faults runner 所有权与失败收口设计

- 设计基线：`173d8f2e8ddc92908a1a98e096a56cd6685902a8`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 状态：实现前设计；不表示硬件资格通过

## 目的与边界

本设计只收口 `tests/hardware` 中 `faults` 命令的资源所有权和部分失败证据。当前 runner 复用 primary 作为
port occupier 与 cancel session，并在 Harness 已创建后仍有 `?` 提前返回；这些路径只留下字符串错误，显式 close
和已取得的 operation、native open attempt、snapshot 证据会丢失。两项行为都违反 ADR-0010。

本次不改变 `easycon-controller`、`easycon-serial` 或 `easycon-runtime` 的冻结语义，不改变 faults 三个场景的机器
判据，也不运行物理动作。durable journal、stable identity guard、Ctrl+C、OperatorPort、Amiibo write-ahead 和
manifest 是 ADR-0010 的后续独立任务，不混入本次生命周期改造。

## 冻结约束

1. port-occupied 场景使用一个 `occupier` Harness 持有端口，并用独立 `occupied_probe` Harness 验证第二次打开；
   operation cancel 再创建全新的 `cancel` Harness，不能复用 occupier。immediate deadline 使用第四个 Harness。
   deadline 只能在前三个 Harness 都显式 close 且 cleanup 通过后创建。
2. wait timeout 只是 runner failure，不得改写 operation 的 error、state 或 cancellation reason。
3. 已创建的 Harness 必须恰好显式 close 一次；未创建的角色不得生成伪 cleanup evidence。
4. 首个 stage failure 是权威 execution error。后续 cancel、wait 或 close failure 追加为 recovery/cleanup evidence，
   不覆盖原错误。
5. 任一 execution 或 cleanup failure 固定得到 `failed/failed/1`。只有三个场景与四份真实 cleanup 都通过时才可
   进入 faults qualification。
6. 取消场景仍要求一次已接受的非中立 report、`Applied` 的首次取消请求、`Requested/Cancelled` 终态、后续中立
   report 和 lease release。Home prelude 不进入该流程。

## 组件与依赖

### `FaultRun<H>`

CLI 内部的唯一 owner，持有 `occupier`、`occupied_probe`、`cancel`、`deadline: Option<H>`、当前 operation 和
内存 projection。创建成功后立即把 Harness 移入对应 slot；只有 owner 的 close helper 可以 `take()`。因此正常、
失败和恢复路径共享同一关闭实现，类型状态保证一个 slot 最多 close 一次。

owner 不实现 `Drop` 资格结论。意外 unwind 时现有底层 Drop 仍是最后防线，但 projection 只有显式 finish 才能记录
可用于资格判断的 cleanup。

### `FaultHarness`

仅在 qualification crate 内部使用的窄 trait。production adapter 包装现有 `Harness`，测试 fake 记录调用顺序和
close 次数。接口只暴露 faults 所需能力：connect admission、sequence admission、Controller snapshot、当前时钟、
native open attempts 和 consuming close。`close(self)` 必须由 adapter 在消费自身前后完成采样，并返回
`FaultCloseEvidence { runtime_cleanup, post_controller_snapshot }`；owner 外不得保留 Harness 或 Controller clone
来绕过唯一 owner。该 trait 不进入根 workspace 或公共 API。

### `FaultHarnessFactory`

按 `Occupier`、`OccupiedProbe`、`Cancel`、`Deadline` 角色创建 Harness。production factory 仍调用
`Harness::new`；fake factory 可在精确角色注入 create failure，并记录后续角色是否被错误创建。port 与未来
expected identity 参数由 runner 显式传入，factory 不缓存全局设备选择。

### `FaultWaiter`

封装 bounded terminal wait 和 cancel-readiness wait。production waiter 复用当前 10 秒边界；fake waiter 在
`OccupierConnectWait`、`OccupiedProbeWait`、`CancelConnectWait`、`CancelReady`、`CancelWait`、
`DeadlineWait` 注入确定性结果，不使用随机 sleep。waiter 只能观察 operation，不能把 runner timeout 伪造成
operation cancel。

### `FaultSequenceBuilder`

qualification crate 内部的窄 builder 负责构造固定 cancel sequence。production builder 仍调用
`PreciseSequence::new`；fake builder 可精确注入 `cancel_sequence_build` failure。builder 只返回序列或
`FaultFailure`，不拥有 Harness、operation 或线程。

### `FaultFailure` 与 projection

`FaultFailure` 至少保存稳定 stage 和原始 message。projection 从命令开始就包含三个场景的状态：

```json
{
  "scenarios": {
    "port_occupied": {"status": "not_run"},
    "cancel": {"status": "not_run"},
    "deadline": {"status": "not_run"}
  },
  "resources": {
    "occupier": {"created": false},
    "occupied_probe": {"created": false},
    "cancel": {"created": false},
    "deadline": {"created": false}
  }
}
```

场景状态只允许 `not_run -> running -> completed|failed`。创建 Harness 后先把对应 resource 的 `created` 固定为
true；stage 失败写入 `execution_error {stage,message}`，当前场景变为 `failed`，后续场景保持带原因的 `not_run`。
已有 operation、snapshot 或 native error 证据语义保持不变；旧的含糊 cleanup 字段由四个明确 role 字段替代，
不能让兼容别名制造重复 cleanup slot。

## 执行状态机

| 顺序 | stage | owner 变化 | 成功证据 | 失败后的动作 |
| ---: | --- | --- | --- | --- |
| 1 | `occupier_create/connect_admit/wait` | `occupier = Some` | holder connect 终态 | settle holder，close occupier |
| 2 | `occupied_probe_create/admit/wait` | `occupied_probe = Some` | operation、native attempts | settle probe，close probe 与 occupier |
| 3 | `occupied_probe_close` | `occupied_probe = None` | `occupied_probe_cleanup` | close occupier；不得创建 cancel/deadline |
| 4 | `occupier_close` | `occupier = None` | `occupier_cleanup` | 不得创建 cancel/deadline |
| 5 | `cancel_create/connect_admit/wait` | `cancel = Some` | 独立 session connect 终态 | settle operation，close cancel |
| 6 | `cancel_sequence_build/admit/ready/request/wait` | cancel 不变 | 三份 snapshot、两类 outcome | recovery cancel + bounded wait，close cancel |
| 7 | `cancel_close` | `cancel = None` | `cancel_cleanup`、post-cleanup snapshot | 不得创建 deadline |
| 8 | `deadline_create/admit/wait` | `deadline = Some` | operation、零 native open | settle deadline，close deadline |
| 9 | `deadline_close` | `deadline = None` | `deadline_cleanup` | 保留原 stage 或记录 cleanup failure |

sequence 构造属于 `cancel_sequence_build`，即使尚未 admission，也必须经 owner finish 关闭 cancel。admission 成功后
operation 立即进入 owner 的 active-operation slot；任何 wait failure 都先保存 operation snapshot，再按该 operation
真实状态决定是否请求 recovery cancel。恢复请求 outcome 和首次 qualification request outcome 分字段保存。

## 单一完成路径

`run_faults` 只负责参数解析、构造 owner 并调用内部执行函数。内部执行可用 `?` 返回 `FaultFailure`，但其返回值必须
回到仍持有 `FaultRun` 的外层：

```text
parse -> FaultRun::new -> execute(&mut run) -> run.finish(execution_result) -> Value
```

`finish` 先停止后续 admission，再结算 active operation，然后按
`deadline -> cancel -> occupied_probe -> occupier` 对仍存在的 slot 执行 close。每次 consuming close 返回
`FaultCloseEvidence`，owner 立刻把真实 cleanup 与 post-controller snapshot 写入对应固定字段。最后才把 stage
error、scenario status 和 cleanup 完整性固定进 projection。不存在从持有 `Some(H)` 的作用域直接返回
`Err(String)` 的路径。

正常流程允许在阶段边界提前 consuming close，例如 occupied probe 与 occupier 必须在创建 cancel 前释放；该 close
仍通过 owner helper 完成并把 slot 置空。finish 只处理尚未关闭的 slot，不重复 close。

## Cleanup contract

完整成功精确要求 `occupier_cleanup`、`occupied_probe_cleanup`、`cancel_cleanup`、`deadline_cleanup` 四份
`Closed + succeeded + zero counts`。部分 execution failure 改为按 `resources.*.created` 和 owner role 计算精确布局：

- `created = true` 必须有且只有该角色的一份 runtime cleanup；
- `created = false` 不得出现该角色 cleanup；
- cleanup slot 总数必须等于已创建角色数，且每个 slot 都是 runtime cleanup；
- 任一已创建角色缺 cleanup、重复 cleanup、错位 cleanup 或 failed cleanup 都使 execution failed；
- 动态布局只适用于已经存在 `execution_error` 的 faults partial result，不能放宽完整 qualification 的四份要求。

这使 occupier create failure 可以只有 stage error，而 deadline admission failure 必须保留前三份已完成 cleanup 和
一份 deadline cleanup。projection 的自述状态不能单独证明 cleanup；validator 同时核对固定 role 字段、slot 总数
和 runtime cleanup 结构。

## 并发、取消、deadline 与资源关闭

- runner 本身单线程推进；Controller/Runtime 内部并发仍由冻结实现所有。
- 所有 wait 有固定 deadline，fake 以显式脚本推进，不使用 wall-clock race。
- cancel readiness failure时没有 qualification cancel request，`cancel_request_outcome = null`；owner recovery 请求单列。
- cancel terminal wait failure保留首次 outcome，再进行幂等 recovery request；两者不得互相覆盖。
- deadline operation必须使用 admission 时的 `now_ns()`，并精确验证 `Deadline/DeadlineExceeded`。其 native open
  attempts 必须为空；PortBusy 不能冒充 deadline。
- close failure不触发新 Harness 创建。owner 继续尝试关闭其余已持有角色，并保存每个真实 close 结果。
- occupied probe 与 occupier 都 clean close 后才允许创建 cancel；cancel clean close 后才允许创建 deadline，避免
  PortBusy 或前一 session 状态污染后续场景。

## 错误与测试

实现按小任务拆分：

1. 先增加动态 partial cleanup contract、scenario/resource transition 和 fail-closed finalization 回归。
2. 再增加 fake factory/harness/waiter/sequence builder 与表驱动 failpoint，证明每个
   create/admission/wait/build/close failure 的
   create/close 次数、stage error、后续 `not_run` 和 partial evidence。
3. 最后把 production `run_faults` 迁移到 owner；删除被 owner 替代的专用提前返回 helper。

每一行 failpoint 至少断言：所有已创建 Harness 恰好 close 一次、未到达角色创建零次、首个错误不被 cleanup 覆盖、
已取得 operation/native attempt/snapshot 保留、未创建场景无伪 cleanup，以及最终 `failed/failed/1`。另保留完整
success projection，证明四份 cleanup 和既有精确 fault 判据没有放宽。

## 排除项与重新设计条件

本设计不实现通用 command runner、跨命令资源框架或公共错误类型；只有后续命令出现相同且已经证实的 owner 需求时
才考虑提取。它不改变 JSON schema version，不承诺硬件 capability，也不关闭 O-01、O-02、O-04。

若实现需要改变 production operation 终态、Controller neutralization/lease、serial error mapping、faults 三个
场景的独立 session 条件，或允许 cleanup failure 后继续 deadline admission，必须先重新审查 ADR-0008/0010，
不能以内部重构名义落地。

## 验证门禁

每个实现提交必须先有旧实现确定性失败的无硬件回归，随后通过根 workspace 九项门禁和独立
`tests/hardware` workspace 的 fmt/check/strict clippy/test。实现 SHA 固定后必须做新的独立 review；修复产生
新 SHA 时重新 review。所有测试均使用 fake 或 memory transport，不把通过结果描述为 CH32 复测。
