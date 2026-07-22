# Phase 2B 操作员、取消与观察设计

- 状态：Architecture Target (`Hardware Unverified`)
- 日期：2026-07-21
- 设计基线：`1e6391a`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 前置设计：[durable evidence transaction](phase2b-durable-evidence-transaction.md)
- 相邻设计：[普通命令 runner 所有权](phase2b-command-runner-ownership.md)

## 问题与边界

当前资格 CLI 会一次性执行 smoke/home-wake 动作，并把 Switch 可见结果固定为待人工确认。operation wait、
home-wake interval、diagnostic delay 和 smoke hold 也没有共同观察进程中断。因而现状不能证明：

- 每个需要人眼判断的动作都取得了绑定当前 run、设备和 action 的明确回答；
- `no`、EOF/timeout 和 Ctrl+C 分别产生 ADR-0010 的不同状态；
- Ctrl+C 停止接纳后续动作，并让当前 operation 经过唯一取消终态后再中立化和关闭；
- stdin 没有遗留 detached reader，operator response 在下一动作前已经进入 durable journal。

本设计只补齐资格工具的软件交互、取消与观察边界。它不执行或声称任何物理观察，不改变 Runtime/Controller
公共契约，不关闭 O-01/O-02/O-04，也不进入 Phase 3。所有验证使用 scripted `OperatorPort`、synthetic transport、
可控 token、barrier/channel 和已有 fake clock，不调用系统串口 discovery/open。

## 固定类型与单 owner

进程为一次命令创建一个 `InterruptToken`。token 内部只有共享原子布尔值，初值为 false，置位幂等且不可复位。
Windows console handler 的回调只执行原子置位；不得写日志、分配、等待、join、调用 Controller 或访问 artifact。
handler 安装失败发生在设备 discovery 前，留下明确 execution failure，不能继续外部动作。

一次 run 只创建一个可变 `OperatorPort` owner。接口固定为两个同步、有界操作：

1. `announce(marker)` 在动作发送前输出稳定 marker；失败停止动作 admission。
2. `observe(request, interrupt)` 在 response deadline 内返回一个 typed outcome。

outcome 仅为 `yes`、`no`、`eof`、`timeout`、`ambiguous` 或 `interrupted`。实现不能从自由文本、环境变量或
blanket `--yes` 推断 yes。production console port 只接受交互终端的单次 `Y`/`N` 键；非交互 stdin 直接返回
EOF，不创建 reader thread。终端事件使用有界 poll/read；每个 poll slice 前后都检查同一 interrupt token。
因此 OperatorPort 不拥有后台任务，不需要依赖无法唤醒的 blocking `read_line`，run 结束时也没有待 join 的输入 owner。

`--operator-timeout-seconds` 是每次回答的显式 deadline，范围 `1..=300`，默认 30 秒。
`--observation-window-ms` 是 active action 保持可见的显式有界窗口，范围 `100..=5000`，默认 1000 毫秒。
两者进入 normalized arguments、final JSON 和 journal；`--yes` 一律作为不支持的 blanket authorization 拒绝。

## RunControl 与 durability boundary

`RunControl` 在命令执行期间唯一借用 `ArtifactReservation`、`InterruptToken` 和 `OperatorPort`。设备 admission、
operator response、interrupt request 和 cancellation terminal 都通过同一个 journal writer 同步，不建立第二套日志。
每个 observation 保存：

- journal sequence 和稳定 observation sequence；
- reservation lease/run ID、expected/observed stable identity；
- command、稳定 action ID/label 和 marker；
- configured observation window、response deadline、typed outcome；
- 对应 active/release/neutral operation 的 stable ID 与终态。

`yes` 只有在完整 action 已释放并中立、回答结构有效且 `operator_observation` append 已 flush/sync 后才允许接纳下一
动作。`no`、EOF、timeout、ambiguous 和 interrupted 同样先保存能够保存的 response，再停止接纳后续动作。
journal append 失败时，run 不再接纳动作；若已有 Harness，仍由 Harness owner 完成 operation settle 和 cleanup，
但 poisoned/incomplete journal 不能发布 passed artifact。

## 可中断等待与取消状态机

所有资格层 operation wait 和长 delay 使用短的有界 poll slice，不用单次长 `Operation::wait` 或不可观察的 sleep。
wait timeout 仍只表示 caller wait 失败，绝不调用 `cancel()`。只有检测到 operator interrupt 才走以下状态机：

```text
Running / BetweenActions / AwaitingObservation
  -> InterruptObserved
  -> InterruptJournalDurable
  -> StopAdmission
  -> CurrentOperationCancelRequested (若存在非终态 operation)
  -> OneOperationTerminal
  -> ActionReleaseOrControllerNeutralization
  -> ControllerClose
  -> RuntimeClose
  -> CleanupJournalDurable
  -> CancelledArtifact
```

interrupt event 在调用当前 operation `cancel()` 前同步，payload 保存 operation ID（若存在）、原状态和请求来源
`operator_interrupt`。请求结果只允许 `Applied`、`Unchanged` 或与竞态一致的 `AlreadyTerminal`；随后继续拥有 operation
直到观察唯一终态。operation 在 bounded settle deadline 内不能终态、终态为不一致状态或 cleanup 失败时，execution
为 failed，不伪造 cancelled。成功取消保存 `cancellation_terminal`，stable reason 必须为 `Requested`。

中断恰好与 operation 成功竞态时，若 terminal 已先提交，保留该唯一 terminal，并把 run 标为 operator-cancelled；
runner 仍停止后续 admission 并完成中立化/关闭。重复 Ctrl+C 只保持 token，不产生第二个 cancel owner 或第二个
run terminal。final projection 固定后才到达的 Ctrl+C 不改写已封闭 journal；强制终止仍属于排除项。

## 动作与 observation 单元

可见资格动作不是单个 transport report，而是一个明确的 observation unit：

1. 输出绑定 stable action ID 的 marker；
2. 接纳 active action operation 并等待成功；
3. 在可中断 observation window 内保持该状态；
4. 接纳对应 release/center，再接纳 neutral reset，并等待各自唯一终态；
5. 在 neutral snapshot 后调用 `OperatorPort::observe`；
6. durable 记录回答，之后才决定是否接纳下一 unit。

`smoke` 的 A-only 模式包含 `button.A` 一个 unit。`smoke --full` 对每个 `Button::ALL`、每个非 Center HAT、
左右摇杆的四个边界各建独立 unit；release/center/neutral 是该 unit 的机器步骤，不伪装成额外人工观察。wake Home/
left-stick diagnostic prelude 仍只记录真实步骤，不产生 capability observation。`home-wake` 每个尝试各有 marker、
Home press/release/neutral 和独立 observation；尝试编号是 stable action ID 的一部分。

final JSON 的 `operator_observations` 必须按 sequence 连续，并与本次 expected/observed identity 和已完成 action unit
一一对应。full smoke 不能通过一个回答覆盖多个 unit，也不能从缺失、重复、unknown label 或未绑定回答得到 passed。

## 状态与退出码

cleanup contract 先于下表。任何 Controller/Runtime close failure、operation 无法结算或 journal/artifact failure 都是
`failed/failed/1`。

| operator outcome | execution | qualification | exit |
| --- | --- | --- | ---: |
| 每个必需 observation 均为 yes，机器判据和 provenance 通过 | `completed` | `passed` | 0 |
| 任一 observation 为 no，cleanup 成功 | `completed` | `failed` | 1 |
| EOF、timeout、ambiguous 或缺少必需 observation，cleanup 成功 | `completed` | `unverified` | 2 |
| Ctrl+C，operation terminal 和 cleanup 均完成 | `cancelled` | `unverified` | 130 |

明确 no 结束当前命令，不再执行其余 observation unit；未执行 unit 记录为 not_run，不改变 prescribed
`completed/failed/1`。EOF/timeout/ambiguous 同样停止后续动作以避免无观察地继续发送。Ctrl+C 在 discovery 前发生时
不打开设备，仍形成 cancelled ledger；在 Harness 存在时必须走完整 cleanup。

## 实现拆分与测试

实现分两个可独立验证的节点：

1. **OperatorPort 与 observation projection**：增加 typed port、console bounded poll、scripted fake、参数验证和
   smoke/home-wake observation unit；journal durable response；qualification validator 要求逐项 exact binding。
2. **Interrupt 与 cooperative cancellation**：安装 atomic-only console handler；operation wait/delay 分片；把
   interrupt/cancel/terminal/neutralize/Controller close/Runtime close/artifact 顺序接入共同 completion path。

最小回归必须覆盖：

- scripted yes/no/EOF/timeout/ambiguous 得到精确状态和退出码，且每次 response 在下一 action admission 前 durable；
- full smoke 每个稳定 action ID 恰有一个回答，blanket `--yes`、缺失、重复和错绑 observation 不得 passed；
- interrupt 在 action 非中立、operation wait、observation wait 和 unit 间隙触发时均停止后续 admission；
- barrier/channel 固定 cancel 与 terminal 竞态，断言一次 cancel request、一个 operation terminal 和一个 run terminal；
- trace 精确为 interrupt journal -> cancel -> terminal -> neutralize -> Controller close -> Runtime close -> cleanup journal
  -> cancelled projection，cleanup failure 降为 failed；
- fake port 无 thread，production 非交互 stdin 返回 EOF，不以随机 sleep 证明 deadline 或取消。

每个节点执行根 workspace 九项门禁以及 `tests/hardware` 独立 workspace 的 fmt/check/strict clippy/test。测试不得
调用真实 discovery、打开串口、发送物理动作或把 scripted yes 描述成物理观察。

## 排除项与重新打开规则

本设计不实现 Amiibo destructive authorization、telemetry 新字段、checkpoint 聚合、硬件 attestation 或支持矩阵。
以下变化必须先重开设计：

- console handler 执行 token store 之外的工作，或引入不可唤醒、不可 join 的 stdin reader；
- 用 blanket flag、默认值、缺失输入或一个回答通过多个 observation unit；
- wait timeout 自动请求 operation cancel，或 Ctrl+C 直接跳过唯一终态、中立化、Controller/Runtime close；
- operator response 未 durable 就接纳下一动作，或取消/cleanup 使用覆盖式第二日志；
- 把 scripted/fake observation、Switch 未观察或无物理设备描述成硬件通过。
