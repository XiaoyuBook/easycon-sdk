# 0020：提议冻结 Controller 结算的 Runtime 前置合同

- 状态：Proposed / Not Effective
- 提议日期：2026-07-30
- proposal 基线：`main@2fe7eb2ef50f9ec49fe4e186b36605b6502aebb1`
- proposal 基线 tree：`1281e677ae4e26bfc7a85b19dd6a392d9736eff0`
- 上位冻结决定：[ADR-0004](0004-operations-events-shutdown.md)、
  [ADR-0006](0006-runtime-stabilization.md)、[ADR-0007](0007-phase-1-freeze.md)、
  [ADR-0009](0009-phase-2a-freeze.md)、[ADR-0017](0017-phase-4-ecs-automation-target.md) 与
  [ADR-0018](0018-phase-2a-controller-lease-reopen.md)
- 只读实现证据：归档 R0 候选 `c6e01b65aa8455f5d031f2e9a293c18984467177`，tree
  `c5449698d462cd15e912195cdb1e0bea48bcd728`，parent
  `d7f0178aa08e476abef9f2f45ec96588a43d9874`
- 编号说明：已扫描当前 refs、snapshot refs 与 worktrees；ADR-0020 在本 proposal 前未占用

## 状态、生效条件与授权边界

本文只是 docs-only 设计候选，当前不生效，也不授权修改 production Rust、serial backend、behavior、schema、fixture、
conformance、测试或 public ABI。它不接受归档 R0 候选，不冻结任何实现，不声明 D1 的现有 RED 已通过，也不取代任何
既有 Accepted/Frozen ADR。只有本文自身形成固定提交、由独立 reviewer 对该 fixed SHA 完成 full review，并由另一个
docs-only acceptance/refreeze 决定明确接受后，下文的规范性合同才生效。

当前 D1 必须暂停在已有 RED 证据，不得把 bounded teardown fallback、单个测试进程退出或未提交 worktree 当作通过。
proposal 期间，ADR-0018 的历史原文和 Accepted 状态保持不变；本文不通过修改该原文来倒写历史。若本文以后被接受，
它只取代 ADR-0018 中“D1 与 R0 相互独立”“D1 只拥有 production Controller 且不触及 serial transport 内部边界”两项
实施假设，并按下文窄重开冻结面。ADR-0018 的 lease API、五态 acquire outcome、同点优先级、generation、pacing、
release outcome、硬件未验证和 D2 独立 refreeze 要求继续有效。

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

## 决定候选概览

若本文被单独接受，最小治理变更由三个架构上不可缺少、实施时严格分阶段的部分组成：

1. R0-v2 只在 Runtime 增加真实、一次性、非 Operation 的 deadline registration。
2. R0-v2 只在 Runtime 增加“先 claim、后 cleanup/commit”的通用两阶段 terminal/effect-claim primitive，并使用
   Runtime fake/model 验证 effect candidate 与 cancellation 竞争同一个 arbiter。
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

Runtime close 必须先 seal 新 registration；仍被 resource/Operation close 使用的 deadline worker 在这些 owner 结算前保持
可运行。依赖 registration 全部 resolve/disarm 后，Runtime 才 drain 剩余 entry 为 `RuntimeClosed`、唤醒 waiter 并 join
该既有 infrastructure worker。不得让 Controller lane 反向 join Runtime deadline worker。

## 两阶段 terminal 与唯一仲裁

### 两阶段 primitive

R0-v2 必须让每个真实 Operation 持有一个唯一、domain-neutral 的 terminal arbiter。terminal/effect candidate 先通过
原子 claim 取得不可转让的结算权，owner 随后完成既有 bookkeeping、stream settlement 与 fallible cleanup，最后才
commit 可观察终态。claim 与 commit 的边界如下：

- claim 决定先到的 terminal/effect winner；一旦成功，任何较晚的 cancel、deadline、close、failure 或 success candidate
  都只能观察 winner，不能改写它或发布第二个 terminal。
- claim 本身不发布 terminal event、不从 Operation registry unlink、不唤醒 terminal waiter，也不伪造新的 Operation。
- commit 继续严格使用 ADR-0006/0007 已冻结的顺序：完成 owner cleanup 后提交唯一终态与 immutable error/value，发布
  唯一 terminal event，执行唯一 registry unlink，最后通知 waiter。具体 event schema、OperationValue 与 registry identity
  不变。
- cancellation winner 可以按既有状态机进入 `Cancelling`，完成 settlement 后提交 `Cancelled`；effect-accepted winner
  在 cleanup 成功时提交 `Succeeded`。effect acceptance 后发生的 cleanup failure 仍按 ADR-0006 的 first-error/cleanup
  规则结算，但较晚 cancellation 永远不能把它改写为 `Cancelled` 或抹去已接受 effect 的错误上下文。
- permit 只能由登记的 Operation owner commit，或在 owner panic/close handoff 时通过显式、一次性的 supervised takeover
  转交。Runtime close 只能 request cancellation、等待 owner settlement 或执行该 takeover；不得绕过 permit 直接清零
  registry。

Operation 对外可见的六态状态机、OperationId、wait/cancel/query API、cancellation first-reason authority、ErrorDomain、
OperationValue 与 event schema 都不增加枚举值。本文只窄重开状态机内部“谁可以取得 terminal 结算权、取得后何时发布”
的实现合同。

### effect acceptance 与 cancellation

对 Controller logical report，唯一真实线性化点是 transport adapter 已累计接受完整 report 的最后一个 byte、在同一 write
settlement 临界区内调用 acceptance hook 并成功 claim 对应 arbiter 的瞬间。下列位置都不是 effect 线性化点：

- write future/阻塞调用返回到 Controller lane；
- lane 再次读取 Operation 或 cancellation token；
- desired report、lease 或 sequence bookkeeping；
- terminal event 发布、registry unlink 或 waiter wake；
- UART/USB 对端、固件或 Switch 的物理执行。

`ControllerTransport::write` 及 serial/fake adapter 必须接收每次 logical report 专属的 settlement/acceptance handle。adapter
只能在累计长度等于该 report 精确长度时调用 hook，且必须在向 lane 返回成功之前调用。action report 的 hook 竞争真实
Operation terminal arbiter；release/close neutral 的 hook 竞争对应的唯一 cleanup record。重复、迟到、wrong-generation
或在 stream settlement 后到达的 hook 必须无效果。

“cancellation 先到”是 cancellation candidate 先成功 claim 同一个 arbiter，不是仅设置 token bit、排入 lane command 或
调用 waker；“acceptance 先到”是上述 hook 先成功 claim。仲裁结果唯一：

| 首个成功 claim | 必须结果 |
| --- | --- |
| 完整 report acceptance | accepted effect 保留；正常 cleanup 后 action 为 `Succeeded`，较晚 generation seal、cancel 或 close 不得进入 `Cancelling` 或改成 `Cancelled` |
| cancellation/close | 禁止 late success；立即请求对应 I/O interruption，证明 write 已返回且 stream settled 后才提交 `Cancelled`/既有 close outcome |
| failure/deadline | 保留既有 first-error/first-reason；禁止后来 acceptance 或 cancellation 覆盖 |

transport 在 cancellation claim 后仍可能观察到底层迟到 completion；该 completion 只能用于证明 stream settlement，不能
重新 claim accepted effect。若底层无法证明取消前后 byte 边界，必须按下节关闭不确定 stream，不能从“write 最终返回了
完整长度”反推 late success。

## I/O interruption、partial write 与 stream settlement

### interruption authority

Controller 为每个 lane-owned in-flight write 建立一个只用于 interrupt 的 handle。handle 可以从 caller/close 线程发出
取消信号，但不能提交 report、修改 desired state/lease、关闭 registry 或成为第二个 writer；transport I/O 的 submit、
completion 解释、stream settlement 与最终 close 仍只由 Controller lane owner 执行。serial backend 必须保证 interrupt
能使阻塞 write 在既有 bounded transport timeout 内返回；fake 必须实现同一 contract，而不是依赖测试释放 barrier 才退出。

interruption scope 必须分层：

- **generation action scope**：lease generation 在 seal 时原子拒绝新 action，并直接对该 generation 中尚未 acceptance
  claim 的 action 发出 `ParentClose` cancellation claim 与 I/O interrupt。它不能取消当前或随后开始的 release neutral。
- **release scope**：显式/Drop release neutral 使用独立于 generation/run token 的 I/O token，因此 run cancellation、future
  drop 与 generation seal 不能中断它。
- **Controller close scope**：resource close 拥有上级 interrupt authority，可以中断尚未 acceptance 的 action，也可以
  中断并接管已经开始的 release neutral。它不把 release token 错误地改成 generation token。
- **close-owned neutral scope**：Controller close 自己新建的 neutral attempt 使用新的、未取消的 close token，只受
  Controller close、既有 bounded transport timeout 与不可恢复 transport failure 约束；不得复用刚刚取消的 action、run
  或 release token。

### partial write

partial prefix 永远不是 logical report effect acceptance。adapter 可以在同一次 write 内继续已知的 partial completion，
但若 cancellation、timeout 或 failure 使 logical report 在完整 acceptance claim 前终止，则必须先完成以下 settlement，
之后才能结算 Operation/release waiter 或发下一份 report：

1. 停止/撤销 outstanding I/O，并等待其 completion 已被唯一消费；
2. 若存在非零 prefix、迟到 byte 数不确定、framing 不确定或 backend 不能证明可复用，幂等关闭 transport 并等待 close
   完成，把 Controller 永久 seal；
3. 只有 backend 明确证明零 effect、无 outstanding completion 且 stream framing 可复用时，lane 才可继续同一 generation
   的 paced neutral；
4. 记录只能结算为既有 cancellation/failure 或 `NeutralNotDeliveredStreamSettled`，不得伪造 success/acceptance，也不得在
   旧 stream 未 settled 时重试 neutral。

stream settled 表示不会再有该 write 的 byte、completion 或 acceptance hook 作用于当前或后续 generation；仅 future 被
drop、token 被设置或 lane command 被移除都不构成 settled。

## release 与 Controller close 接管

generation seal 必须在 `neutralize_and_release(self)` 返回 future 前发生，并从 caller 线程直接触发未接受 action 的上述
interrupt；它不能依赖 lane 先取到 seal command。lane 等所有已 admission action 取得唯一 terminal 且其 stream settled
后，才按既有 Runtime clock/pacing 开始该 generation 的唯一 release neutral。

Controller close 对 release cleanup record 的接管必须保持单 record、单 completion、单 neutral attempt：

- neutral acceptance 已先 claim 时，close 复用该 record 的 `NeutralAccepted`，不得改成未交付或取消；
- close 在 neutral acceptance 前先 claim 时，close 发出 release I/O interrupt，等待 stream settled，并在同一 record 完成
  `NeutralNotDeliveredStreamSettled`；不得再发送第二个 neutral 来掩盖第一次未接受；
- 不可恢复的 interrupt/close/ownership failure 继续使用 ADR-0018 已有 `Err(cleanup_failure)`，永久 seal Controller，
  不得清零或伪造 `Closed`；
- 若显式 release 已经完全终结、generation 已清除，之后才开始独立的 resource close，则该 close 不再是对旧 record 的
  takeover；它按既有 Controller close 合同发送自己的一次 paced neutral，并使用新的 close-owned token。

Controller close 的精确 lane 顺序为：seal acquire/action admission；按 ADR-0018 优先级结算 pending acquire；对未接受
action claim cancellation 并 interrupt；接管 active release record；等待所有相关 write/stream settlement；提交 action 与
release 的唯一 terminal/completion；清除匹配 generation 并唤醒 waiter；必要时执行 close-owned neutral；最终幂等关闭
transport 并 join lane。任何 waiter completion、Operation terminal 或 registry unlink 都不能先于其所属 stream settlement。

## task、thread 与 close ownership

本文不增加 Controller timer、arbiter 或 watchdog thread。职责固定如下：

| owner | 唯一职责 | 明确禁止 |
| --- | --- | --- |
| Runtime deadline worker | deadline queue、SystemClock real wait、VirtualClock change scan、signal fire 与 shutdown drain | Controller callback、lease mutation、假 Operation/event/registry |
| Runtime Operation owner | terminal arbiter、first reason/error、cleanup 后 commit、event 与 registry 顺序 | 在 transport acceptance 外猜测 effect |
| Controller lane | 唯一 report writer、desired report、lease generation、release record、I/O completion 与 transport close | 自建 deadline thread、让 caller 成为第二 writer |
| thread-safe interrupt handle | 使指定 in-flight I/O 返回 | 写 report、提交 terminal、修改 generation 或复用为 cancellation reason authority |
| caller/executor | poll handle、观察 wake、请求 cancel/release/close | 通过持续 poll 或测试 barrier 承担正确性进展 |

Runtime close 保留 ADR-0006/0007 的根 owner：先 seal 新工作并发出 root cancellation；在 deadline infrastructure 仍可服务
既有 registration 时关闭并 join resources/Controller；等待 Operation owner cleanup、terminal commit 与 registry unlink；
再 drain/disarm 剩余 deadline registration 并 join deadline worker；最后完成既有 supervised task/thread 与 Runtime close
检查。实现可以把这些步骤映射到现有内部 phase 名称，但不得形成 Runtime 等 Controller、Controller 反向等 Runtime
worker 的 join cycle。

## 方案比较

| 方案 | 能闭合的缺口 | 不能接受的代价或剩余缺口 | 结论 |
| --- | --- | --- | --- |
| A. 先扩展 Runtime deadline/two-phase terminal primitive，Phase 1 refreeze 后再由 D1 窄接 transport acceptance hook | 一个 Runtime clock authority；非 Operation deadline；effect/cancel 单 arbiter；沿用 Operation event/registry/close owner | 需要依次窄重开 Phase 1 内部 terminal/deadline 与 Phase 2A transport adapter/fake，不能合并实施或 refreeze | **选择**；这是同时满足四条 RED 的最小分阶段组合 |
| B. 新增 Controller supervised timer/arbiter | 可单独唤醒 acquire，也可保存 Controller 私有 winner | 复制 Runtime clock/terminal authority，增加 thread/task 和双向 close 顺序；仍不能决定真实 Operation accepted effect | 拒绝 |
| C. 把 Controller Operation 移出 Runtime 根 cancellation，或让 transport 直接提交 terminal | 可绕开 close 覆盖的局部症状 | 破坏 ADR-0004/0006/0007 的 Runtime 根 ownership；transport 获得 event/registry authority；仍没有 SystemClock acquire deadline | 拒绝单独采用；只接受无 terminal authority 的窄 acceptance/interrupt hook |
| D. 为每个 acquire 创建隐藏 Operation | 可复用当前 Operation deadline scan | 伪造 OperationId、terminal event、registry count 与 telemetry，改变外部观察且把非 Operation request冒充 Operation | 拒绝 |

选择 A 不是偏好性抽象：当前代码已经由 Runtime 唯一拥有 Clock、Operation terminal/event/registry 与 root close，Controller
已经由单 lane 唯一拥有 transport I/O 和 lease mutation。R0-v2 把 generic deadline/terminal 留在 Runtime，D1 随后只把
真实 acceptance 与 interrupt 接入 ControllerTransport；这一顺序保留两条冻结 ownership，也避免用 Phase 1 refreeze
错误覆盖尚未审查的 Phase 2A production transport 变更。

## 窄重开与保持不变的冻结面

本文只有在后续 acceptance 生效时才窄重开：

- ADR-0006：Operation terminal/cancellation 的内部 claim/commit 仲裁、owner takeover 与 close 等待合同；
- ADR-0007：Runtime deadline scheduler、两阶段 terminal primitive、相应 Loom/spec/conformance 与 close drain，并要求新的
  Phase 1 implementation candidate 通过完整门禁后单独 refreeze；
- ADR-0009：仅授权未来 D1 修改 `ControllerTransport::write` acceptance/interrupt/stream settlement hook、system serial
  adapter、fake 与 partial-I/O conformance；R0-v2 不得修改这些 production 表面，单 writer、协议、pacing、ACK 与
  Hardware Unverified 状态不变；
- ADR-0017：只以 Runtime-only R0-v2 取代旧 R0 窄范围，并把其 Phase 1 refreeze 设为 D1 的真实前置；
- ADR-0018：只取代 D1/R0 独立性和“不触及 serial transport 内部边界”的实施假设；其余 D0 合同继续有效。

保持不变：ADR-0004 的根 Runtime/Operation/事件/确定性关闭原则；Operation 六个可见状态；ErrorDomain、OperationValue、
event schema 与 registry identity；public C ABI 与四语言；ADR-0019 C1 lexer 合同及其独立 implementation 授权；Phase 2A
protocol、硬件资格状态与 O-01/O-02/O-04；Phase 3、Vision、package 和发布范围。本文不授权任何硬件 claim。

## 新依赖 DAG 与验收

若本文被接受，后续唯一允许的顺序为：

```text
ADR-0020 fixed-SHA proposal
  -> independent design review
  -> separate docs-only acceptance/freeze
  -> R0-v2 Runtime-only RED/models
  -> generic deadline registration + two-phase terminal/effect-claim implementation
  -> Phase 1 full gates
  -> fixed-SHA independent implementation review
  -> separate Phase 1 refreeze
  -> resume D1 with the existing four RED unchanged
  -> D1 ControllerTransport/system serial/fake acceptance, interrupt,
     partial-I/O and close-takeover implementation
  -> all affected Phase 2A and Runtime gates
  -> D2 fixed-SHA independent review and separate Controller refreeze
```

R0-v2 至少必须只用 Runtime fake/model 与 deterministic/Loom/conformance 证据覆盖：SystemClock 在任意 consumer 无进展时
deadline signal 仍可触发、VirtualClock 只随 advance 且同点排序稳定、registration resolve/drop/Runtime close 恰好一次、
generic effect/cancel/close claim 竞态、accepted claim 与 terminal commit 间的 late cancellation、owner panic/takeover、
event/registry/waiter 唯一顺序，以及 deadline/Operation/task/resource registry 最终无泄漏。R0-v2 的 source/test diff 不得
包含 production ControllerTransport、system serial 或 Controller fake。归档 `c6e01b65` 可作为设计输入或测试来源，但新的
Runtime-only implementation candidate 必须以自己的 fixed SHA 接受审查。

Phase 1 refreeze 不改变四条现有 D1 RED 的治理状态。D1 恢复后必须先按原样运行它们，不能删除、放宽 bounded deadline、
以 sleep 拉长掩盖竞态或预先改写为“已通过”；随后才实现 ControllerTransport/system serial/fake acceptance hook、分层
interrupt、partial-I/O settlement 与 close takeover，并增加 full acceptance/close 确定性 barrier、cancel-before-acceptance、
partial prefix/late completion、release takeover 和 system serial/fake parity。D1 必须运行所有受影响 Phase 2A 与 Runtime
门禁；D2 再固定完整 D1 candidate SHA，完成独立 review 与单独 Controller refreeze。R0-v2 refreeze、单条 D1 测试通过或
D1 自报完整门禁都不能代替 D2。

## 本 proposal 的 docs-only 门禁

本 proposal 提交前只运行：

```text
python -B tools/check_markdown_links.py
python -B tools/check_repository_guards.py
git diff --check
```

这些命令只证明 proposal 文档引用、仓库边界和 diff hygiene，不证明 Runtime/Controller/serial implementation、Rust/Loom、
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
