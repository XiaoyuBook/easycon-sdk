# Phase 2B 普通命令 runner 所有权与失败证据设计

- 状态：Implementation Target (`Hardware Unverified`)
- 日期：2026-07-21
- 设计基线：`d48c04b28cb0b7d050e06515197d7c22d4e3c29a`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 相邻设计：[faults runner ownership](phase2b-fault-runner.md)、
  [Controller close evidence](phase2b-controller-cleanup-evidence.md)

## 问题与复现

`faults` 已使用单一 `FaultRun` owner 收口失败，但其他拥有 `Harness` 的命令仍在创建 Harness 后使用 `?`
传播 connect、action、wait、discovery、stdout 或 process-metrics 错误。该提前返回绕过 `Harness::close`，只留下
顶层字符串错误，并丢失已经取得的 descriptor、handshake/native-open attempts、operation 终态、Controller
中立化与 Runtime close evidence。

该缺口可用 synthetic transport 稳定复现：让 connect 或第一个 direct action 失败，旧路径返回 `Err(String)`，
`finalize_result` 生成不含 `result` 与 cleanup 的 artifact。它违反 ADR-0010 的单一 completion path，不依赖
物理设备，也不是 durable journal 才能解决的 crash 场景。

受影响命令是 `handshake`、`smoke`、`home-wake`、`hotplug`、`lifecycle`、`sequence` 和已授权
`amiibo`。`discover` 不创建 Harness；未授权 Amiibo 在创建 Harness 前安全返回；`faults` 继续使用已经审查的
专用 owner。

## 范围

本任务只实现：

- Harness 一旦创建就立即安装到 run-owned role slot；
- 所有正常、operation failure、wait timeout、辅助步骤失败和 cleanup failure 共享一个 completion path；
- 保存首个 execution failure、失败 operation 的关闭前后快照、共同 Harness evidence 和真实 cleanup；
- `hotplug`、`lifecycle` 的动态 partial-cleanup contract；
- 使用 synthetic Harness/factory/waiter 的确定性失败回归。

本任务不实现 stable identity selector/open guard、append-only journal、manifest/provenance、Ctrl+C、
`OperatorPort`、Amiibo write-ahead、telemetry 新时间边界或 checkpoint。后续任务复用这里的 owner，不重新引入
提前返回。

## 失败类型

runner 内部使用结构化失败：

```text
CommandFailure {
  stage,
  message,
  operation?,
}
```

`stage` 是稳定枚举式字符串；`message` 只作诊断。operation 已经 admitted 后发生 terminal failure 或 wait
timeout 时，失败对象继续拥有该 operation handle，直到 completion path 结算。admission 前失败没有伪 operation。

首个 execution failure 是权威错误。后续 recovery/close failure 进入 `cleanup_errors`，不能覆盖首错；若此前
没有 execution failure，cleanup failure 自身成为权威失败。所有这些路径固定为 `failed/failed/1`。

## Owner 与 role

资格 workspace 定义内部 `CommandRun<H>`，其唯一职责是拥有 projection、Harness role slots 和 active
operation。role 固定如下：

| 命令 | role |
| --- | --- |
| handshake/smoke/home-wake/sequence/amiibo | `primary` |
| hotplug | `initial`、`reconnected` |
| lifecycle | 每个 `cycle-N`，任一时刻至多一个 active slot |

Harness factory 成功后必须先把 Harness 移入 slot、写入 `resources.<role>.created = true`，才能开始 connect 或
其他动作。只有 owner 的 close helper 可以 `take()` slot；每个已创建 role 恰好显式 close 一次，未创建 role
不得生成 cleanup。

`CommandHarness` 是 qualification-only 窄 trait。production adapter 包装当前 `Harness`；fake adapter 记录
admit/cancel/close 次数和调用顺序。trait 不进入 production crate，也不枚举或打开真实串口。

## 单一 completion path

每个命令固定采用：

```text
parse and pre-Harness validation
  -> create Harness
  -> install role in CommandRun
  -> execute(&mut run)
  -> run.finish(execution_result)
  -> finalize_result
```

`execute` 可以用 `?` 返回 `CommandFailure`，但返回目标必须是仍拥有所有 role 的 `CommandRun`。不得从持有裸
Harness 的作用域直接返回 `Err(String)`。

`finish` 顺序固定为：

1. 停止创建后续 role 和接纳后续 action；
2. 保存 active operation 的 failure-time snapshot；
3. 若 operation 非终态，请求一次 recovery cancel，并有界等待；wait timeout 不改写 operation 原因；
4. 按命令逆序显式 close 已创建 Harness；Controller close 先于 Runtime close；
5. 再保存 operation post-cleanup snapshot、descriptor、handshake/native-open attempts、Controller snapshot；
6. 保存每个真实 cleanup 及 cleanup failure；
7. 最后写入 `execution_error`、resources/scenario prefix 和 final projection。

Controller close 会结算仍活动的 operation，但 owner 仍显式记录 recovery request/outcome，不能把 Drop 当作正常
路径。future Ctrl+C owner 将复用同一结算顺序，只改变 failure/cancellation 来源。

## 命令细则

### 单 Harness 命令

connect、action admission、operation wait、sequence reset、Amiibo save/select 任一步失败都回到 `finish`。
projection 至少保留 command、已解析参数、descriptor、actual baud、handshake/native-open attempts、失败
operation、pre-cleanup snapshot 和 `cleanup`。

success projection 保持现有字段和资格判据。新增共同 evidence 不能替代命令专用机器判据。

### Hotplug

状态前缀为 `initial created -> initial closed -> reconnected created -> reconnected closed`。等待设备消失、stdout
flush 或初始 disconnect 操作失败时仍关闭 initial role；此时不得伪造 reconnected cleanup。initial 已关闭后，
等待返回、reconnected create/connect 失败必须保留 initial cleanup。两个 role 都创建时都必须关闭。

stable identity 重绑定由相邻 device-admission 任务完成；本任务暂不修正按旧 COM 选择，只确保任何当前路径都
不会绕过 owner。

### Lifecycle

每个 cycle 创建后立即安装。connect 或 metrics 失败时，当前 cycle 仍写一条带 `status = failed`、真实 cleanup
和失败 stage 的 record；此前 completed records 保留，后续 cycle 为 not run。create 自身失败时没有当前
cleanup，但此前 records 仍保留。qualification 只有精确完成请求 cycles 时才可能通过。

### Sequence auxiliary

timing CSV 只有 execution 完整产生 bytes 后才交给 reservation stage。operation/reset/timing render 失败时不
伪造 CSV token；主 JSON 仍通过 owner completion path保留。durable journal 任务会进一步记录 incomplete
telemetry stream。

## Cleanup contract

完整成功沿用现有固定布局。含 `execution_error` 的普通命令改为按 `resources` 和 records 验证动态布局：

- `created = true` 必须有且只有一份该 role cleanup；
- `created = false` 不得有该 role cleanup；
- cleanup slot 与 nested Runtime cleanup 数必须等于已创建 role 数；
- 每份 cleanup 必须通过 exact Controller/Runtime schema；
- role 创建必须符合命令前缀顺序，不能出现 reconnected 而 initial 未创建；
- cleanup failure 时整体失败，即使首个 execution error 的其他 cleanup 均成功；
- 不接受旧版只有字符串 error、没有 resources/result 的投影作为 cleanup 完成。

顶层 artifact 仍由 run-directory reservation no-replace commit。该 artifact 在 durable ledger 实现前不构成完整
发布证据。

## 并发、取消与 deadline

- runner 自身单线程推进；Controller lane 和 Runtime supervision 语义不变。
- operation wait 使用现有有界 waiter；测试 fake 由脚本推进，不使用随机 sleep。
- caller wait timeout 只成为 runner failure；recovery cancel 单列，不能把 timeout 伪装成 operation deadline。
- Harness close 同步 join Controller writer 后才固定 transport cleanup evidence。
- owner 没有 detached thread、callback 或 Drop 资格结论。

## 测试设计

先加入旧实现确定失败的最小回归：synthetic connect operation 失败后，结果必须仍包含 operation、一次真实
Harness cleanup、Controller final-neutral evidence 与零 Runtime counts。旧命令 helper 只返回字符串，回归失败。

随后覆盖：

1. handshake connect admit/terminal/wait failure；
2. smoke/home-wake 每个 action prefix failure，已完成 actions 保留；
3. sequence operation、reset、CSV render failure，不发布伪 CSV；
4. Amiibo save/select failure，save 已成功时不能被 select failure抹去；
5. hotplug initial wait/close、return wait、reconnected create/connect/close failpoint 的 role/cleanup prefix；
6. lifecycle create/connect/metrics/close failpoint 的 records prefix；
7. active operation recovery cancel、settle timeout、Controller close、Runtime close 的固定顺序；
8. 首错与 cleanup failure 同时存在时首错不被覆盖，cleanup error 仍使 contract fail closed；
9. 所有已创建 Harness 恰好 close 一次，未创建 role 零 close；
10. 所有测试只使用 synthetic/fake discovery、transport、metrics 和 waiter，不调用系统串口发现或 I/O。

## 后续兼容与重新打开规则

device admission 实现后 factory 参数从 raw port 变为 typed `AdmittedDevice`，但 install-before-action 和
completion path 不变。journal 实现把相同 transition 持久化，不能只在 final JSON 重建。operator interrupt
复用 active operation owner 和关闭顺序。

以下变化必须重开本设计：

- 允许已创建 Harness 依赖 Drop 完成正常 cleanup；
- execution failure 后跳过 Controller 或 Runtime close；
- 用伪 cleanup 填充未创建 role；
- cleanup failure 覆盖或删除首个 execution error；
- hotplug/lifecycle 中途失败时删除已完成前缀；
- 把 wait timeout 改写成 operation deadline/cancellation reason；
- 为复用 owner 而修改 production Controller/Runtime 的冻结公共语义。
