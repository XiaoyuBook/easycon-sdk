# 0020：冻结 Controller 结算的 Runtime 前置合同

- 状态：Accepted / Effective
- 提议日期：2026-07-30
- 接受与生效日期：2026-07-30
- proposal 基线：`main@2fe7eb2ef50f9ec49fe4e186b36605b6502aebb1`
- proposal 基线 tree：`1281e677ae4e26bfc7a85b19dd6a392d9736eff0`
- 初始 proposal：`c09c323476a5f0ed972aec6b7f2ee2acbeb139c0`，tree
  `c5b63720f0b4f7db86c92d2439d9489d9cf23425`，parent
  `2fe7eb2ef50f9ec49fe4e186b36605b6502aebb1`
- 初始 fixed-SHA review：`REQUEST CHANGES`，P0/P1/P2=`0/4/0`；后续修订逐项处理四项 P1
- 第一轮修订：`362df95b5fdc03c9e2f285de4b456a61f8b6a09f`，tree
  `b1af657a2ed03c94a8ea580a0ed8062239d03aee`，parent
  `c09c323476a5f0ed972aec6b7f2ee2acbeb139c0`
- 第一轮修订 fixed-SHA review：`REQUEST CHANGES`，P0/P1/P2=`0/1/0`；固定接受候选只处理该轮发现的
  ownership-loss waiter settlement 例外与 ADR-0018 冲突
- 固定接受候选 / 第二轮修订：`a617e0841b76051192a4d6f6069877b340d5ec33`，tree
  `4a149e4096aa2de33f232dc4d40608d25ff89bfd`，parent
  `362df95b5fdc03c9e2f285de4b456a61f8b6a09f`
- 最终 fixed-SHA 独立 review：`APPROVE`
- 上位冻结决定：[ADR-0004](0004-operations-events-shutdown.md)、
  [ADR-0006](0006-runtime-stabilization.md)、[ADR-0007](0007-phase-1-freeze.md)、
  [ADR-0009](0009-phase-2a-freeze.md)、[ADR-0017](0017-phase-4-ecs-automation-target.md) 与
  [ADR-0018](0018-phase-2a-controller-lease-reopen.md)
- 只读实现证据：归档 R0 候选 `c6e01b65aa8455f5d031f2e9a293c18984467177`，tree
  `c5449698d462cd15e912195cdb1e0bea48bcd728`，parent
  `d7f0178aa08e476abef9f2f45ec96588a43d9874`
- 编号说明：已扫描当前 refs、snapshot refs 与 worktrees；ADR-0020 在本 proposal 前未占用

## 状态、接受决定与授权边界

固定接受候选 `a617e0841b76051192a4d6f6069877b340d5ec33` 已完成 fixed-SHA 独立 review 并获得 `APPROVE`。
本 ADR 现接受并生效，下文规范性合同从本次 docs-only acceptance/refreeze 起成为后续实现与审查的冻结依据。本次
acceptance 只改变状态、治理链与索引，不修改固定候选已经审查通过的 deadline、terminal/effect arbitration、I/O
interruption、partial-write/stream settlement、owner-loss 例外或 close 顺序合同。

本 acceptance 仅解锁 Runtime-only R0-v2 按冻结 DAG 形成独立 RED/models、implementation candidate、完整 Phase 1
门禁、fixed-SHA review 与单独 refreeze；它不接受归档 R0 候选，也不冻结或声明任何实现完成。除下文明确要求的 R0-v2
Runtime-only source、RED/models、测试与 conformance 外，本 acceptance 不授权提前修改 production Controller、
`ControllerTransport`、system serial、Controller fake 及其 behavior/schema/fixture/conformance/test，也不改变 public ABI。
本 acceptance 自身的 SHA/tree 不在 tracked 文档中预言，只由提交后的 Git 对象与外部验收记录固定。

当前 D1 必须暂停在已有 RED 证据，不得把 bounded teardown fallback、单个测试进程退出或未提交 worktree 当作通过。
ADR-0018 的历史原文和 Accepted 状态保持不变；本文不通过修改该原文来倒写历史。从本文生效起，它只取代 ADR-0018
中“D1 与 R0 相互独立”“D1 只拥有 production Controller 且不触及 serial transport 内部边界”两项
实施假设，并且只在“Controller lane ownership 已丢失、没有可验证 transferable transport/settlement owner、completion 与
stream settlement 均不可证明”三个条件同时成立的极窄分支，取代 ADR-0018 对 action/release waiter 无条件 settlement
以及 close join 前 release record 必须终结的要求。该三条件分支之外，ADR-0018 的 waiter/record 完成要求、lease API、
五态 acquire outcome、同点优先级、generation、pacing、release outcome、硬件未验证和 D2 独立 refreeze 要求继续有效。

## 问题与证据

ADR-0018 已经要求完整 report 在 transport acceptance 处线性化，要求 generation seal 取消尚未 effect-linearize 的
action，要求 Controller close 接管 release 并在 join 前结算所有 waiter，也要求 SystemClock 与 VirtualClock 使用同一
Runtime clock epoch。当前冻结实现却没有足以实施这些要求的共同机制：

1. `ControllerTransport::write` 完整返回后，lane 才重新读取 Operation 状态。若 Runtime/Controller close 已把 Operation
   改为 cancelling，已经完成 transport acceptance 的 action 仍会被终结为 `Cancelled`；真实 effect 边界与终态仲裁
   因而不是同一个线性化动作。
2. generation seal 可以封住新的 lane admission，却不能从 caller/close 线程中断 lane 内 acceptance 前的阻塞 action。
   seal command 若排在该 write 后面，neutral 永远没有机会开始。
3. release neutral 故意不观察 generation/run cancellation，但也没有只供 Controller close 使用的上级 I/O interrupt。
   close command 若排在阻塞 neutral 后面，就不能接管 release、settle stream 或 join lane。
4. 当前 deadline worker 只扫描真实 Operation。acquire request 不是 Operation；`SystemClock::on_change` 又不会主动产生
   clock-change 通知，因此 lane 无进展时 acquire 的 absolute deadline 没有独立结算来源。

归档 R0 候选提供了五路 terminal permit、fallible cleanup、owner settlement、Loom 与 conformance 方向，因此证明
Runtime 内部 terminal authority 是所需基础；但它只在调用 `complete_after_cleanup` 时 claim permit，没有把更早的
transport acceptance 接入同一 arbiter，也没有为非 Operation waiter 提供真实 deadline registration。该候选因此是
`necessary but insufficient` 的只读设计/测试证据，不是已接受实现，不得直接以其 SHA 代替新的 Runtime-only R0-v2
实施、review 或 Phase 1 refreeze。

D1 只读 worktree 中下列 teardown-safe 回归必须原样保留为 RED。它们不属于 R0-v2 acceptance；即使 Runtime primitive
使其中某条具备前置能力，在 Phase 1 refreeze 后也仍按 D1 RED 管理，直至恢复 D1 并完成 Controller/serial 集成：

- `system_clock_acquire_deadline_settles_without_lane_progress`
- `release_interrupts_an_uneffected_generation_action_before_neutral`
- `close_settles_pending_acquire_actions_and_release_before_join`
- `close_interrupts_release_neutral_before_acceptance_and_joins`

这些测试共同覆盖完整 report acceptance 后 close 不得改写 action、release seal 的 action interrupt、close 对 pending
acquire/release 的接管，以及 SystemClock deadline 的独立进展；它们当前都不是通过证据。

## 已接受决定概览

本文接受的最小治理变更由三个架构上不可缺少、实施时严格分阶段的部分组成：

1. R0-v2 只在 Runtime 增加真实、一次性、非 Operation 的 deadline registration。
2. R0-v2 只在 Runtime 增加“settlement 决议后 claim、再 cleanup/commit”的通用两阶段 terminal/effect-claim primitive，
   并使用 Runtime fake/model 验证 outstanding work 的 cancellation intent 与真实 completion 由同一个 settlement arbiter
   决定，intent 本身不能抢先取得 terminal winner。
3. Phase 1 独立 refreeze 后，D1 才在 `ControllerTransport` 的完整 logical report acceptance 内部边界接入该 generic
   effect-claim hook，并给 generation seal 与 Controller close 分配范围不同的 I/O interruption authority。

R0-v2 不得修改 production `easycon-controller`、`ControllerTransport`、system serial adapter 或 Controller fake，不得实现
partial-I/O、generation seal、release neutral 或 close takeover。这些全部属于 Phase 1 refreeze 之后的 D1 ownership。

只实施其中任一部分都不能闭合四条 RED：deadline registration 不解决 accepted effect，terminal permit 不产生真正的
transport 线性化点，transport interrupt 也不能为 acquire deadline 或 Operation event/registry 提供唯一 authority。

## Runtime deadline registration

### 注册对象与状态

Runtime 提供一个 public-internal、一次性的 `DeadlineRegistration`/`DeadlineSignal` 机制；最终 Rust 命名可在 R0-v2
实现 review 中机械调整，但以下语义不得改变：

- 输入只能是所属 Runtime `Clock` epoch 的 absolute `target_ns`。`clock.now_ns() >= target_ns` 时已到期；不得换算成
  wall clock、另一 `Instant` epoch 或 Controller 自建相对 timer。
- 每个 registration 在返回给调用方前已经登记，拥有 Runtime 单调分配且不复用仍可观察值的 registration id，并且只会
  从 `Armed` 一次转为 `Fired`、`Disarmed` 或 `RuntimeClosed` 之一。
- `Fired` 只设置通用 one-shot signal 并唤醒已登记 waker；deadline worker 不执行 Controller callback，不创建 lease，
  不提交 Operation terminal，也不接触 Controller lane state。
- resolve、显式 disarm、handle/drop cleanup 与 Runtime close 必须恰好一次从 scheduler queue 移除或消费 entry；迟到 heap
  entry 由 registration id/generation 判陈旧，不能再次 wake 或作用于后来 registration。
- registration 不是 Operation、resource 或 supervised domain task。它不分配 OperationId，不发布 operation terminal
  event，不写 operation/resource/task registry，也不改变这些 registry 的对外计数；唯一新增状态是 Runtime deadline
  scheduler 自身的 queue entry 与 one-shot signal。

Controller acquire 继续拥有 ADR-0018 的单个 request record。deadline signal、caller cancellation、Controller close 与
lane grant 仍只竞争该 record 的一次 transition，且保留
`Cancelled > Deadline > Closed > Failure > Granted` 的同一观察点优先级。deadline worker 触发 signal 后，acquire future
无需 Controller lane 进展即可提交/观察 `Deadline`。lane 在尝试 `Granted` 前必须检查 signal，并重新检查同一 Runtime
clock 的 `now_ns()`；已经满足 deadline predicate 时必须先提交 `Deadline`，不能因 worker 尚未获得调度而 late grant。

### SystemClock 与 VirtualClock

同一个 Runtime deadline scheduler 必须支持两种 clock，不能为 Controller 或每个 registration 新建 timer thread：

- `SystemClock`：deadline worker 按 queue 中最早 `(target_ns, registration_id)` 调用现有 `real_wait_duration` 计算真实等待；
  新的更早 registration、disarm 或 Runtime close 必须唤醒 worker 重算。即使 Controller lane、调用 future 或 transport
  没有产生进展，worker 也会在目标时间独立 fire signal。
- `VirtualClock`：worker 不读取 wall clock，也不真实 sleep 到 virtual target；只由现有 `on_change` 通知唤醒并扫描到期
  registration。一次 advance 中相同 `target_ns` 的 entry 按 registration id 确定性 fire。
- 注册时已经到期的 target 必须同步标为 `Fired` 或安排等价的立即 wake，不能先入 lane 再等待下一次 clock change。

Runtime close 必须先 seal 新 registration，但不能因此提前 drain 或停止 deadline worker。该 internal worker 必须在
resource close、普通 supervised/external owner task join 与 Operation owner settlement/fallback 的全过程继续服务已有
registration。只有所有可能持有 registration 的 external owner task 已经 join，且其 Operation 已由合法 owner settle 或
进入下文保守 `CloseFailed` 分支后，Runtime 才把剩余 entry drain 为 `RuntimeClosed`、唤醒 waiter、停止并 join deadline
internal worker，最后执行 registry 检查。精确阶段顺序与无 join-cycle 约束见“task、thread 与 close ownership”。

## 两阶段 terminal 与唯一仲裁

### 两阶段 primitive

R0-v2 必须让每个真实 Operation 持有一个唯一、domain-neutral 的 terminal arbiter。generic primitive 明确区分
`intent`、`claim` 与 `commit`：intent 只记录请求，不能决定 terminal；真实 owner 取得 settlement evidence 后才原子 claim
唯一 winner；owner 完成既有 bookkeeping、settlement 与 fallible cleanup 后，才 commit 可观察终态。边界如下：

- first cancellation reason 仍由现有 Operation authority 一次写入并保持 immutable。`Requested`、`Deadline` 或
  `ParentClose` intent 可以请求 owner 停止工作，但只记录 reason、signal/wake，不等于 terminal/effect claim；后来的 reason
  不能覆盖第一个 reason。
- 对没有 outstanding effect 的 generic work，owner 可以在证明 effect 不可能发生后按现有优先级 claim cancellation、
  deadline、close 或 failure。对存在 outstanding effect 的 work，任何线程都不得仅凭 intent 抢先 claim；必须等待该 domain
  的唯一 settlement owner 产出 mutually exclusive 的 accepted-effect 或 not-delivered/failure evidence。
- settlement owner 在消费证据的同一临界区 claim winner。一旦 claim 成功，任何较晚的 cancel、deadline、close、failure
  或 success candidate 都只能观察 winner，不能改写它或发布第二个 terminal。
- claim 本身不发布 terminal event、不从 Operation registry unlink、不唤醒 terminal waiter，也不伪造新的 Operation。
- commit 继续严格使用 ADR-0006/0007 已冻结的顺序：完成 owner cleanup 后提交唯一终态与 immutable error/value，发布
  唯一 terminal event，执行唯一 registry unlink，最后通知 waiter。具体 event schema、OperationValue 与 registry identity
  不变。
- cancellation evidence 胜出后才可按既有状态机进入 `Cancelling`，完成 settlement 后提交 `Cancelled` 或既有 deadline/
  close 投影；outstanding I/O 尚未 settlement 时仅有 cancellation intent，Operation 保持原非终态，不能预先进入一条排除
  `Succeeded` 的路径。effect-accepted winner 在 cleanup 成功时提交 `Succeeded`。effect acceptance 后发生的 cleanup
  failure 仍按 ADR-0006 的 first-error/cleanup 规则结算，但较晚 cancellation 永远不能把它改写为 `Cancelled` 或抹去
  已接受 effect 的错误上下文。
- permit handoff 不是默认 panic fallback。只有 supervisor 在工作开始前已经持有下文定义的 transferable settlement owner，
  并在原 owner 已 join 后能够证明 cleanup/effect evidence 所有权完整转移时，才允许显式、一次性 handoff；否则 Runtime
  close 只能等待 owner settlement 或返回 `CloseFailed`，不得绕过 permit 直接清零 registry。

Operation 对外可见的六态状态机、OperationId、wait/cancel/query API、cancellation first-reason authority、ErrorDomain、
OperationValue 与 event schema 都不增加枚举值。本文只窄重开状态机内部“谁可以取得 terminal 结算权、取得后何时发布”
的实现合同。

### effect acceptance 与 cancellation

对 Controller logical report，唯一真实线性化点是 backend settlement gate 唯一消费真实 completion，确认累计接受完整
report 的最后一个 byte，并在同一临界区调用 acceptance hook 成功 claim 对应 arbiter 的瞬间。原始 cancellation request
与 `CancelIoEx` 返回都不是 settlement。下列位置也不是 effect 线性化点：

- write future/阻塞调用返回到 Controller lane；
- lane 再次读取 Operation 或 cancellation token；
- desired report、lease 或 sequence bookkeeping；
- terminal event 发布、registry unlink 或 waiter wake；
- UART/USB 对端、固件或 Switch 的物理执行。

`ControllerTransport::write` 及 serial/fake adapter 必须接收每次 logical report 专属的 settlement/acceptance handle，并在
dispatch 前建立唯一 backend settlement gate。该 gate 的内部状态机固定为：

```text
NotDispatched -> Outstanding
NotDispatched -> Settled(NotDeliveredStreamSettled)
Outstanding -> Settled(FullAccepted)
Outstanding -> SettlingNonFull -> Settled(NotDeliveredStreamSettled | PartialOrFailedStreamSettled)
```

只有 lane-owned backend completion consumer 能消费真实 completion 并取得 `SettlingNonFull` 的排他 owner。full completion 在
`Outstanding -> Settled(FullAccepted)` 的同一临界区 claim effect；non-full completion 先由该 owner 完成 interrupt/close 与
stream settlement，再在 `SettlingNonFull -> Settled(...)` 的同一临界区 claim cancellation/failure。`SettlingNonFull` 不是
terminal，不发布 event 或唤醒 waiter。action report 的 `FullAccepted` hook claim 真实 Operation arbiter；release/close
neutral 的 hook claim 对应唯一 cleanup record。重复、迟到、wrong-generation 或已经 settled 的 completion/hook 必须无效果。

当 gate 为 `Outstanding` 时，caller cancellation、Operation deadline、generation seal、Controller close 或 Runtime close
只能在现有 cancellation authority 登记 first reason，并请求相应 generation/controller-scoped I/O interrupt；它们不得先
claim cancellation/close winner，也不得仅因 token bit、lane command 或 waker 把 Operation 改到排除 success 的状态。
若 gate 仍为 `NotDispatched`，owner 可以在证明没有 I/O effect 后直接 settle 为 not-delivered 并 claim 相应 terminal；若 gate
已经 `Settled(FullAccepted)`，迟到 intent 只能观察 accepted winner。

唯一 backend settlement gate 消费真实 completion 后，必须按可观察结果进入上述唯一分支；只有 full acceptance 或已经
证明 stream settled 的 non-full outcome 才能在对应最终 transition 的同一临界区 claim：

| settlement evidence | 唯一 winner 与结果 |
| --- | --- |
| 累计 transferred bytes 精确达到完整 report 长度 | `FullAccepted` 必须胜出；正常 cleanup 后 action 为 `Succeeded`，已经登记或较晚到达的 cancel/deadline/close intent 都不能改写为 `Cancelled`，已登记 first reason 保持 immutable diagnostic 但不成为 terminal winner |
| 已确认 I/O aborted/not dispatched，完整 report 未接受，且存在 first cancellation reason | 先证明 stream settled，再按该 immutable first reason claim cancellation/deadline/close 的既有投影；禁止 late success |
| partial bytes 或 I/O failure，完整 report 未接受 | 按下节 settle/关闭 stream 后，以既有 first-reason/first-error 优先级 claim cancellation 或 failure；禁止伪造 acceptance |
| completion/byte count 仍不确定 | 没有合法 winner，合法 owner 必须继续 settlement；只有下文 ownership-loss 三条件同时成立时才进入 `CloseFailed`，不能提交 terminal、唤醒 terminal waiter 或清零 registry |

Windows overlapped adapter 必须把 `CancelIoEx` 只视为 interrupt request。`ERROR_NOT_FOUND` 不能投影为 cancelled，也不能推断
未发生 effect；adapter 必须继续以 `GetOverlappedResult` 或等价的唯一 completion consumer 取得最终 transferred byte count。
若结果是完整 report，`FullAccepted` 必须胜过此前登记但晚于真实 completion 的 cancel intent；若结果是 aborted、partial 或
failure，才按上表 settle。即使 cancellation thread 在 completion 尚未被用户态 gate 消费时先登记 intent，也不能覆盖已经
由 backend 完成的最后一个 byte。

## I/O interruption、partial write 与 stream settlement

### interruption authority

Controller 为每个 lane-owned in-flight write 建立一个只用于 interrupt 的 handle。handle 可以从 caller/close 线程发出
取消信号，但不能提交 report、修改 desired state/lease、关闭 registry 或成为第二个 writer；transport I/O 的 submit、
completion 解释、stream settlement 与最终 close 仍只由 Controller lane owner 执行。serial backend 必须保证 interrupt
能使阻塞 write 在既有 bounded transport timeout 内产生可消费的 completion；interrupt 返回、`CancelIoEx` 成功或 token
变为 cancelled 都不能自行决定 terminal。fake 必须实现同一 settlement contract，而不是依赖测试释放 barrier 才退出。

interruption scope 必须分层：

- **generation action scope**：lease generation 在 seal 时原子拒绝新 action。对该 generation 中 `NotDispatched` 的 action，
  owner 可证明无 effect 后 settle cancellation；对 `Outstanding` action，seal 只登记 immutable `ParentClose` first reason 并
  请求 generation-scoped I/O interrupt，必须等待 backend settlement gate 决定 full acceptance 或 cancellation。它不能取消
  当前或随后开始的 release neutral。
- **release scope**：显式/Drop release neutral 使用独立于 generation/run token 的 I/O token，因此 run cancellation、future
  drop 与 generation seal 不能中断它。
- **Controller close scope**：resource close 拥有上级 interrupt authority，可以请求中断 outstanding action，也可以请求
  中断并接管已经开始的 release neutral。action 在现有 Operation authority 登记 immutable first reason；release 在同一
  cleanup record 登记一次 close intent，不建立第二份 cancellation-reason authority。两者都由 backend settlement gate
  裁定，不能由 close 线程抢先 claim；release token 也不能被错误地改成 generation token。
- **close-owned neutral scope**：Controller close 自己新建的 neutral attempt 使用新的、未取消的 close token，只受
  Controller close、既有 bounded transport timeout 与不可恢复 transport failure 约束；不得复用刚刚取消的 action、run
  或 release token。

### partial write

partial prefix 永远不是 logical report effect acceptance。adapter 可以在同一次 write 内继续已知的 partial completion；若
唯一 settlement gate 最终确认累计 bytes 已达到完整 report，则仍必须选择 `FullAccepted`。只有 gate 确认 write 以
not-delivered、partial、aborted 或 failure 结束时，才先完成以下 settlement，之后才能结算 Operation/release waiter 或发
下一份 report：

1. 停止/撤销 outstanding I/O，并等待其 completion 已被唯一消费；
2. 若存在非零 prefix、迟到 byte 数不确定、framing 不确定或 backend 不能证明可复用，幂等关闭 transport 并等待 close
   完成，把 Controller 永久 seal；
3. 只有 backend 明确证明零 effect、无 outstanding completion 且 stream framing 可复用时，lane 才可继续同一 generation
   的 paced neutral；
4. 若存在 cancellation intent，保留其 immutable first reason；记录只能结算为既有 cancellation/failure 或
   `NeutralNotDeliveredStreamSettled`，不得伪造 success/acceptance，也不得在旧 stream 未 settled 时重试 neutral。

stream settled 表示不会再有该 write 的 byte、completion 或 acceptance hook 作用于当前或后续 generation；仅 future 被
drop、token 被设置或 lane command 被移除都不构成 settled。

## release 与 Controller close 接管

generation seal 必须在 `neutralize_and_release(self)` 返回 future 前发生，并从 caller 线程为未结算 action 登记 immutable
first reason；只有其 gate 为 `Outstanding` 时才直接请求上述 generation-scoped interrupt。seal 不能抢先 claim，也不能依赖
lane 先取到 seal command；lane 必须消费真实 completion。所有已 admission action 取得唯一 terminal 且其 stream settled
后，才按既有 Runtime clock/pacing 开始该 generation 的唯一 release neutral。

Controller close 对 release cleanup record 的接管必须按该 record 的 dispatch 状态保持单 record、单 completion、单次
neutral dispatch：

| close 观察到的 release 状态 | close ownership 与唯一结果 |
| --- | --- |
| `NeutralUndispatched` | close 继承同一 cleanup record，使用新的 close-owned token 执行这一个 paced neutral；完整 acceptance 为 `NeutralAccepted`，确认 not-delivered 且 stream settled 才为 `NeutralNotDeliveredStreamSettled` |
| `NeutralInFlight` | close 只在同一 record 登记 close intent 并请求 interrupt；backend settlement gate 若观察到完整 acceptance，则同一 record 为 `NeutralAccepted`；只有 non-full close-interrupt/transport-cancel evidence 胜出时才 settle 为 `NeutralNotDeliveredStreamSettled`，且不得重试第二个 neutral |
| `ReleaseSettled` | close 不得重开或改写旧 record；若 resource close 此后才独立开始，则按既有合同另行执行一次 final paced neutral，并使用新的 close-owned token |

因此“close 在 acceptance 前到达”本身不足以宣告 `NeutralNotDeliveredStreamSettled`：neutral 尚未 dispatch 时 close 必须执行
同一 record 的唯一 neutral；neutral 已 outstanding 时必须先消费真实 completion。

### cleanup failure 与 ownership-loss 例外优先级

以下两条路径互斥，合法 owner 与 settlement evidence 优先于 ownership-loss 例外：

1. **owner/evidence 可证明，cleanup failure**：合法 owner 已取得唯一 completion 与 stream-settled evidence 后，即使后续
   interrupt、transport close 或 bookkeeping cleanup 返回 failure，也不是 ownership-loss 例外。owner 必须按 ADR-0018 在
   同一 record 投影 `Err(cleanup_failure)`，为每个受影响 action/release waiter 写入确定结果并唤醒，按既有顺序恰好一次
   执行适用的 Operation terminal/event/registry unlink 与 cleanup-record notify，然后才能完成 lane join。Controller 永久
   seal 并保留真实 first error，但该 record 已完成，不能改写成非终态或普通 `CloseFailed` 残留。
2. **owner/evidence 均不可证明**：只有 Controller lane ownership 已丢失、没有可验证 transferable transport/settlement
   owner、且 completion 与 stream settlement 均不可证明三个条件同时成立，才进入本 ADR 的极窄例外。close caller/close
   waiter 得到 `CloseFailed`；对应 action/release waiter 保持非终态且不得被伪唤醒，record 与 registry identity 保留供诊断
   或后续合法 owner settlement。该路径不得称为普通 cleanup failure、不得发布 terminal/event、不得 unlink/notify，也不得
   声称 Controller close 已完成。

该例外不适用于 deadline、ordinary cancellation、已有合法/transferable owner、`NotDispatched`、完整 acceptance、partial
但已证明 stream settled，或任何已经取得 completion/settlement evidence 的路径；这些路径继续遵守 ADR-0018 的确定性
waiter completion 与 close-before-join 顺序。

Controller close 的精确 lane 顺序为：seal acquire/action admission；按 ADR-0018 优先级结算 pending acquire；对未接受且
`NotDispatched` 的 action 直接 settle cancellation，对 `Outstanding` action 只登记 first reason 并 interrupt；按上述三态
接管 active release record；由唯一 backend gate 消费所有相关 completion 并完成 stream settlement；再提交 action 与 release
的唯一 terminal/completion、清除匹配 generation 并唤醒 waiter；必要时执行 `ReleaseSettled` 后独立的 final neutral；最终
幂等关闭 transport 并 join lane。任何 waiter completion、Operation terminal 或 registry unlink 都不能先于其所属 stream
settlement；lane panic 时还必须服从下节的保守分支。

## task、thread 与 close ownership

本文不增加 Controller timer、arbiter 或 watchdog thread。职责固定如下：

| owner | 唯一职责 | 明确禁止 |
| --- | --- | --- |
| Runtime deadline worker | deadline queue、SystemClock real wait、VirtualClock change scan、signal fire 与 shutdown drain | Controller callback、lease mutation、假 Operation/event/registry |
| Runtime Operation owner | terminal arbiter、first reason/error、合法 settlement 后 claim/cleanup/commit、event 与 registry 顺序 | 把 cancellation intent 当作 winner，或在 domain evidence 外猜测 effect |
| Controller lane-owned backend settlement gate | 唯一 report writer、desired report、lease generation、release record、真实 I/O completion 消费与 transport close | 自建 deadline thread、让 caller/interrupt handle 成为第二 writer 或 settlement owner |
| thread-safe interrupt handle | 对指定 in-flight I/O 发出 interrupt request | 消费 completion、解释 byte count、写 report、close/接管 transport、提交 terminal 或修改 generation |
| caller/executor | poll handle、观察 wake、请求 cancel/release/close | 通过持续 poll 或测试 barrier 承担正确性进展 |

### 可转移 owner 与 lane panic

generic permit 只在以下条件全部成立时允许 supervisor handoff：工作开始前，Operation 已登记唯一 owner identity，且
supervisor 已经持有显式的 transferable cleanup/settlement owner；原 owner task 已经 join，能够证明不存在仍可 commit 的
live owner；全部 cleanup state 与 effect/settlement evidence 位于该 transferable owner 控制的共享 record，而不是原线程栈、
TLS 或已遗失的 domain object；handoff 本身通过同一 arbiter 恰好一次转移 owner identity。满足这些条件后，新 owner 才能
按既有 first-error 规则 cleanup 并 commit；仅检测到 panic、持有 interrupt handle 或拥有 Operation permit 都不满足条件。

当前 Controller lane 独占 `ControllerTransport`、backend completion consumption、desired report、lease generation 与 release
record；当前代码没有预先登记的 transferable transport settlement owner，也没有能够证明 byte/completion、framing 与
cleanup ownership 已从 panicked lane 完整转移的 Drop-settlement contract。因此 D1 不得声称或实现 lane panic takeover：

- lane panic 后，interrupt handle 可以请求 OS I/O 返回，但不能消费 completion、调用/解释 acceptance hook、接管或关闭
  lane-owned transport，也不能提交 action/release terminal；transport Drop 或 OS handle close 本身不证明 logical stream
  settled。
- 三条件 ownership-loss 例外成立时，Runtime/Controller close caller 与 close waiter 必须得到 ADR-0006 的 `CloseFailed`，
  保留 panic/owner identity/Controller identity 的可诊断 first error，并保留尚未合法 settlement 的 Operation、cleanup
  record 与 registry identity；对应 action/release waiter 不得被伪造为 `Cancelled`、`Failed`、`Closed` 或 completion，
  registry 也不得伪装为 zero。若 owner/evidence 可证明而只有 cleanup failure，则必须改走上节第一条并完成这些 waiter。
- 非终态与 registry 保留持续到合法 owner settlement；本 ADR 不授权构造该 owner。未来若要接管 Controller lane，
  必须另行完整冻结 owner identity、panic detection、transport ownership transfer、completion consumption、cleanup/error
  projection 与 exactly-once commit，并重新审查受影响冻结面。

### Runtime close 顺序

Runtime close 保留 ADR-0006/0007 的根 owner 和既有
`external task join -> Operation fallback -> internal worker join` 语义，精确顺序为：

1. seal 新工作与新 deadline registration，发出 root cancellation 并启动 resource/Controller close；deadline internal worker
   继续服务已有 registration。
2. 在该 worker 仍运行时完成 resource close/interrupt，并 join 全部普通 supervised/external owner task，包括 Controller
   lane；不得先 drain registration 来迫使 owner 退出。
3. external task join 后，执行既有 Operation owner settlement/fallback。只有持有上述 transferable owner 的 supervisor 才能
   handoff 并 commit；Controller lane 等不可转移 owner 的 panic 进入 `CloseFailed` 并保留 registry。
4. 所有可能持有 registration 的 external owner 已 join，且合法 owner settlement/fallback 已完成后，drain/disarm 剩余
   registration 为 `RuntimeClosed`，停止并 join deadline internal worker。
5. internal worker join 后才执行最终 operation/resource/task/deadline registry 检查；任何保守分支留下的非终态或 registry
   identity 必须使 close 返回 `CloseFailed`，不能被清零。

Runtime close coordinator 是唯一 join owner；deadline internal worker 从不等待或 join Controller/external owner，Controller
lane 与 external owner 也不得等待 worker shutdown，只能注册、观察或 disarm signal。close coordinator 在 join resource/
task 时不得持有 deadline queue 或 Operation arbiter lock。由此 deadline 进展与外部 owner settlement 保持可用，同时不存在
Runtime 等 Controller、Controller 反向等待 Runtime internal worker 的 join cycle。

## 方案比较

| 方案 | 能闭合的缺口 | 不能接受的代价或剩余缺口 | 结论 |
| --- | --- | --- | --- |
| A. 先扩展 Runtime deadline/two-phase terminal primitive，Phase 1 refreeze 后再由 D1 窄接 backend settlement/acceptance hook | 一个 Runtime clock authority；非 Operation deadline；intent 与 completion 由单 settlement arbiter 决议；沿用 Operation event/registry/close owner | 需要依次窄重开 Phase 1 内部 terminal/deadline 与 Phase 2A transport adapter/fake，不能合并实施或 refreeze | **选择**；这是同时满足四条 RED 的最小分阶段组合 |
| B. 新增 Controller supervised timer/arbiter | 可单独唤醒 acquire，也可保存 Controller 私有 winner | 复制 Runtime clock/terminal authority，增加 thread/task 和双向 close 顺序；仍不能决定真实 Operation accepted effect | 拒绝 |
| C. 把 Controller Operation 移出 Runtime 根 cancellation，或让 transport 直接提交 terminal | 可绕开 close 覆盖的局部症状 | 破坏 ADR-0004/0006/0007 的 Runtime 根 ownership；transport 获得 event/registry authority；仍没有 SystemClock acquire deadline | 拒绝单独采用；只接受无 event/registry authority 的窄 backend settlement/acceptance/interrupt hook |
| D. 为每个 acquire 创建隐藏 Operation | 可复用当前 Operation deadline scan | 伪造 OperationId、terminal event、registry count 与 telemetry，改变外部观察且把非 Operation request冒充 Operation | 拒绝 |

选择 A 不是偏好性抽象：当前代码已经由 Runtime 唯一拥有 Clock、Operation terminal/event/registry 与 root close，Controller
已经由单 lane 唯一拥有 transport I/O 和 lease mutation。R0-v2 把 generic deadline/terminal 留在 Runtime，D1 随后只把
真实 settlement/acceptance 与 interrupt 接入 ControllerTransport；这一顺序保留两条冻结 ownership，也避免用 Phase 1 refreeze
错误覆盖尚未审查的 Phase 2A production transport 变更。

## 窄重开与保持不变的冻结面

本文只有在后续 acceptance 生效时才窄重开：

- ADR-0006：Operation terminal/cancellation 的 intent/settlement/claim/commit 仲裁、仅在预持有 transferable owner 时的
  conditional handoff、不可转移 owner 的 `CloseFailed` 与 close 等待合同；
- ADR-0007：Runtime deadline scheduler、两阶段 terminal primitive、相应 Loom/spec/conformance 与 close drain，并要求新的
  Phase 1 implementation candidate 通过完整门禁后单独 refreeze；
- ADR-0009：仅授权未来 D1 修改 `ControllerTransport::write` backend settlement/acceptance/interrupt hook、system serial
  adapter、fake 与 partial-I/O/overlapped-completion conformance；R0-v2 不得修改这些 production 表面，单 writer、协议、
  pacing、ACK 与 Hardware Unverified 状态不变；
- ADR-0017：只以 Runtime-only R0-v2 取代旧 R0 窄范围，并把其 Phase 1 refreeze 设为 D1 的真实前置；
- ADR-0018：取代 D1/R0 独立性和“不触及 serial transport 内部边界”的实施假设，并把 success-after-acceptance 落到
  completion settlement gate；另仅在 Controller lane ownership 丢失、无可验证 transferable transport/settlement owner、
  completion 与 stream settlement 均不可证明的三条件分支，取代其无条件 action/release waiter settlement 与 close join 前
  release-record 终结要求。三条件之外，包括 owner/evidence 可证明但 cleanup failure 的路径，ADR-0018 的 waiter completion、
  五态 acquire 优先级、immutable first cancellation reason 与其余 D0 合同继续有效。

保持不变：ADR-0004 的根 Runtime/Operation/事件/确定性关闭原则；Operation 六个可见状态；ErrorDomain、OperationValue、
event schema 与 registry identity；public C ABI 与四语言；ADR-0019 C1 lexer 合同及其独立 implementation 授权；Phase 2A
protocol、硬件资格状态与 O-01/O-02/O-04；Phase 3、Vision、package 和发布范围。本文不授权任何硬件 claim。

## 新依赖 DAG 与验收

ADR-0020 fixed-SHA proposal、独立 design review 与本次单独 docs-only acceptance/refreeze 已完成。从本 acceptance 生效后，
后续唯一允许的顺序为：

```text
R0-v2 Runtime-only RED/models
  -> generic deadline registration + two-phase terminal/effect-claim implementation
  -> Phase 1 full gates
  -> fixed-SHA independent implementation review
  -> separate Phase 1 refreeze
  -> resume D1 with the existing four RED unchanged
  -> D1 ControllerTransport/system serial/fake backend settlement,
     acceptance, interrupt,
     partial-I/O and close-takeover implementation
  -> all affected Phase 2A and Runtime gates
  -> D2 fixed-SHA independent review and separate Controller refreeze
```

R0-v2 至少必须只用 Runtime fake/model 与 deterministic/Loom/conformance 证据覆盖：SystemClock 在任意 consumer 无进展时
deadline signal 仍可触发、VirtualClock 只随 advance 且同点排序稳定、registration resolve/drop/Runtime close 恰好一次、
outstanding generic work 只登记 cancellation intent 且不能抢先 claim、settlement evidence 与 claim 的原子性、accepted claim
与 terminal commit 间的 late cancellation、预持有 transferable owner 的合法 handoff、无 transferable owner 时的
`CloseFailed`/registry 保留、event/registry/waiter 唯一顺序，以及 external task join、Operation fallback、deadline internal
worker join 与最终 registry 检查顺序。R0-v2 的 source/test diff 不得包含 production ControllerTransport、system serial 或
Controller fake。归档 `c6e01b65` 可作为设计输入或测试来源，但新的 Runtime-only implementation candidate 必须以自己的
fixed SHA 接受审查。

Phase 1 refreeze 不改变四条现有 D1 RED 的治理状态。D1 恢复后必须先按原样运行它们，不能删除、放宽 bounded deadline、
以 sleep 拉长掩盖竞态或预先改写为“已通过”；随后才实现 ControllerTransport/system serial/fake backend settlement/
acceptance hook、分层 interrupt、partial-I/O settlement 与 close takeover。D1 exact conformance 至少还必须新增：

- last-byte completion 与 cancellation intent 的双向 deterministic barrier；完整 completion 即使尚未被 lane 消费也必须
  胜过迟到 cancel，confirmed abort/not-delivered 才按 first reason 取消；
- Windows `CancelIoEx == ERROR_NOT_FOUND` 后 `GetOverlappedResult` 返回完整 transferred bytes，必须投影
  `FullAccepted`/`Succeeded`，不得投影 `Cancelled`；
- release `NeutralUndispatched`、`NeutralInFlight` 与 `ReleaseSettled` 三态 close takeover，分别证明唯一 close-owned
  neutral、in-flight settlement 且不重试、以及旧 record 终结后的独立 final neutral；
- partial prefix/late completion、system serial/fake parity，以及 Controller lane panic 无 transferable owner 时
  `CloseFailed`、非终态/registry 保留且 interrupt handle 不冒充 transport owner。

D1 还必须用 exact fault injection 固定下列 acceptance matrix，不能只断言 close 返回了错误：

| 注入场景 | close 与 domain waiter | record/registry |
| --- | --- | --- |
| 合法 owner 已取得 completion 与 stream-settled evidence，随后 cleanup 返回 failure | action/release waiter 得到同一 `Err(cleanup_failure)` 并恰好唤醒一次；close 保留真实 failure，完成必要 join | record 完成；适用的 Operation terminal/event/registry unlink 与 cleanup-record notify 各恰好一次，不保留假非终态 |
| Controller lane ownership 丢失，无可验证 transferable owner，completion 与 stream settlement 均不可证明 | close caller/close waiter 得到 `CloseFailed`；对应 action/release waiter 保持非终态且不唤醒 | record 与 registry identity 保留；无 terminal/event、unlink、notify，close 不得声明完成 |

该矩阵是新增 D1 acceptance obligation，不把四条既有 D1 teardown-safe RED 改成通过；它们仍须按原名、原 bounded teardown
约束保留为 RED，直至 Phase 1 refreeze 后恢复 D1 并形成独立实现证据。

D1 必须运行所有受影响 Phase 2A 与 Runtime 门禁；D2 再固定完整 D1 candidate SHA，完成独立 review 与单独 Controller
refreeze。R0-v2 refreeze、单条 D1 测试通过或 D1 自报完整门禁都不能代替 D2。

## 本 acceptance 的 docs-only 门禁

本 acceptance 提交前只运行：

```text
python -B tools/check_markdown_links.py
python -B tools/check_repository_guards.py
git diff --check
git diff --cached --check
```

这些命令只证明 acceptance 文档引用、仓库边界和 diff hygiene，不证明 Runtime/Controller/serial implementation、Rust/Loom、
Workspace、hardware、R0-v2、D1 或 D2 已经通过。

## 关联

- [操作句柄、拉取事件与确定性关闭](0004-operations-events-shutdown.md)
- [Runtime 所有权、终态事务与确定性关闭](0006-runtime-stabilization.md)
- [Phase 1 Runtime 冻结](0007-phase-1-freeze.md)
- [Phase 2A Controller/Serial Candidate 冻结](0009-phase-2a-freeze.md)
- [Phase 4 ECS 与 Automation 目标](0017-phase-4-ecs-automation-target.md)
- [Phase 2A Controller lease 结算合同](0018-phase-2a-controller-lease-reopen.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
