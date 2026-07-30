# 0018：窄重开 Phase 2A Controller lease 结算合同

- 状态：Accepted
- 日期：2026-07-30
- D0 基线：`d91490cce51008d85a6e97f292c05461d2469d01`
- 被窄重开的冻结决定：[ADR-0009](0009-phase-2a-freeze.md)
- 下游目标来源：[ADR-0017](0017-phase-4-ecs-automation-target.md)

## 状态与授权

本 ADR 接受 D0 的 docs-only 窄重开授权，只冻结 production Controller lease acquire、action admission、
neutralize/release completion 和 close settlement 的目标合同。ADR-0009 的实现基线从本决定起只在该窄表面保持
reopened，直至 D2 以独立 ADR 重新冻结；其余 Phase 2A 冻结面不变。

本提交不修改 production source、behavior、schema、fixture、conformance 或测试，不声明 D1 实现或回归已经完成，
不声明任何 Rust、Workspace、硬件或 CI 门禁通过，也不执行 D2 fixed-SHA review/refreeze。`Accepted` 表示下文设计
已经决定并授权后续 D1 实现，不表示代码已经符合该设计。

## 保留的 Phase 2A 边界

D1/D2 必须原样保留 ADR-0009 的以下合同：

- 依赖方向仍为 `easycon-serial -> easycon-controller -> easycon-runtime/easycon-model`。serial 是 concrete system
  leaf；Controller、Runtime 和 Model 不得反向依赖 serial、Win32 或具体 transport。
- Controller report 仍只有一个 lane writer。lease 仲裁、desired report mutation、action effect、neutralization、
  release 和 close 都在线程所有的同一 lane 中排序。
- contention 仍 fail-fast 为 `Controller/ResourceBusy`；acquire 只等待本次 lane 决议，不排队等待当前 owner 释放，
  不增加公平性、优先级或饥饿声明。
- lease generation 必须非零且不复用仍可观察的 generation；wrong-controller、stale generation、重复 release 或迟到
  Drop 不能影响当前或后续 owner。
- 所有逻辑 report，包括显式 release 与 resource close 的 neutral report，继续服从同一 Runtime clock 和最小 report
  interval；不得早发、绕过 pacing 或声称 transport acceptance 等于硬件执行。
- generation-aware ACK、迟到 reply 隔离、Amiibo 行为及既有 operation/stream settled-before-terminal 合同不变。
- 状态仍为 `Hardware Unverified`；O-01、O-02、O-04 保持开放。本决定不增加控制板、固件、VID/PID、baud、连续节拍、
  UART/USB/Switch 或物理中立化声明。
- 不新增或改变 public C ABI、语言 binding、`easycon-sdk`、ECS port 或任何 concrete Phase 5 adapter。

## Exact public-internal Rust 合同

D1 必须在 `easycon-controller` 导出以下 public-internal 形态；handle 的字段保持私有。两个 completion handle 均为
`Send` 的具名 `Future`，注册在方法返回前完成，因此仅创建但尚未 poll 的 handle 也已经受 close 和 Drop 规则监管。

```rust
pub enum AutomationLeaseAcquireOutcome {
    Granted(AutomationLease),
    Cancelled,
    Deadline,
    Closed,
    Failure(EasyConError),
}

pub struct AutomationLeaseAcquire { /* private */ }

// AutomationLeaseAcquire:
// Future<Output = AutomationLeaseAcquireOutcome> + Send

pub enum AutomationLeaseReleaseOutcome {
    NeutralAccepted,
    NeutralNotDeliveredStreamSettled(EasyConError),
}

pub struct AutomationLeaseRelease { /* private */ }

// AutomationLeaseRelease:
// Future<Output = Result<AutomationLeaseReleaseOutcome, EasyConError>> + Send

impl ControllerSession {
    pub fn acquire_automation_lease(
        &self,
        cancellation: &CancellationToken,
        deadline_ns: Option<u64>,
    ) -> AutomationLeaseAcquire;

    pub fn direct_with_lease(
        &self,
        lease: &AutomationLease,
        action: ControllerAction,
    ) -> Result<Operation, EasyConError>;
}

impl AutomationLease {
    pub fn neutralize_and_release(self) -> AutomationLeaseRelease;
}
```

`deadline_ns` 为 Controller 所属 Runtime `Clock` epoch 上的 absolute nanoseconds；`None` 表示没有 acquire deadline，
`Some(d)` 在 `clock.now_ns() >= d` 时已经到期。实现不得换算为 wall clock、相对 sleep 或另一 `Instant` epoch。
`AutomationLease::release(self)` 不再是 public-internal 正常路径；显式 owner 必须调用并 await
`neutralize_and_release`。现有 `direct_with_lease` 的 operation 模型和 transport-acceptance 成功边界保持不变。

## Acquire 决议与错误投影

每个 acquire 拥有一个共享、一次写入的 request record。其线性化点是该 record 从 `Pending` 首次转为上述五个 outcome
之一；channel send、future poll、waker 调用和调用方取得返回值都不是线性化点。取消 hook、Runtime-clock deadline、
Controller close 和 lane grant 只能竞争这一个 transition。

先发生并成功提交的 transition 永远胜出。若多个条件在同一个尚未决议的观察点同时可见，唯一优先级固定为：

```text
Cancelled > Deadline > Closed > Failure > Granted
```

其中 `Failure` 只在更高优先级未胜出后检查 connection、contention 和内部 admission failure。lane 只能对仍为
`Pending` 的 record 设置 `lease_owner` 并提交 `Granted`；cancel、deadline 或 close 已提交后，任何迟到 lane command
都只能丢弃，禁止 late grant。

`Granted` transition 必须同时把真实 `AutomationLease` ownership 放入 completion record，调用方 poll 后才转移；不得先
写裸 generation、再尝试发送一个可能失败的通知。acquire future 在决议前消失会把 record 标成 abandoned 并阻止 grant；
在 grant 后消失则由 record 中 lease 的同代 Drop fallback 接管。因此 receiver/future 消失不能留下 orphan lease，也不
构成第六种可观察 outcome。

`Cancelled` 表示 caller token 胜出，但故意不复制 cancellation reason。现有 first-reason authority 是 caller
`OperationSnapshot::cancellation_reason`，以后是 ADR-0017 的 immutable `RunCompletion`；`CancellationToken` 本身只表达
已取消。adapter 必须保留已冻结的 `Requested`、`Deadline` 或 `ParentClose`，不得由 Controller 推断、覆盖或建立第二份
reason authority。这就是 `Cancelled(first reason)`：lease outcome 为 `Cancelled`，caller 的 first reason 原值不变。
没有 owner Operation 的内部调用按 `Runtime/Cancelled`、语义 reason `Requested` 投影。

| Outcome | 现有错误模型投影 |
| --- | --- |
| `Granted(lease)` | 成功，无 `EasyConError` |
| `Cancelled` | `Runtime/Cancelled`；owner Operation/RunCompletion 保留其 first reason |
| `Deadline` | `Runtime/DeadlineExceeded`；若投影到 Operation，则请求 `CancellationReason::Deadline` |
| `Closed` | `Controller/DeviceDisconnected`，message 明确 lane/resource 已关闭 |
| `Failure(error)` | 原样保留 immutable `EasyConError`；不得从 message 反解析类别 |

在没有更高优先级 signal 时，`Disconnected` 或 `Connecting` 返回
`Failure(Controller/DeviceDisconnected)`，不创建 lease；`Disconnecting`、close admission seal 已设置或 `Closed` 返回
`Closed`。已有 lease、sequence owner、waiting sequence 或未结算 report 返回
`Failure(Controller/ResourceBusy)`。意外 worker/admission invariant failure 返回 `Failure(Internal/Internal)`，不能伪装成
`Closed` 或 `ResourceBusy`。

## Action seal 与 generation 顺序

`AutomationLease` 的 generation admission gate 是 Controller owner state，不只是 caller handle 上的布尔值。
`direct_with_lease` 与 `neutralize_and_release` 必须在同一 gate 上形成唯一顺序：

1. `neutralize_and_release(self)` 在返回 future 前原子 seal 该 generation 的新 action admission，并把唯一 cleanup record
   入 lane；future 未 poll 或随后被丢弃都不撤销 cleanup。
2. seal 前已 admission 的 action 继续由 Controller 监管。effect 已在完整 report transport acceptance 处线性化的 action
   保留成功；尚未 effect-linearize 的 action 请求 `CancellationReason::ParentClose`，不得在 neutral report 之后再生效。
3. partial/in-flight write 必须先完成一个已接受 effect，或以 cancellation/failure 结算并在必要时 settle stream。每个已接纳
   action waiter 都先取得唯一终态，然后才能开始该 generation 的 neutral dispatch；这些 action 的 cancellation cleanup
   必须合并到同一 generation cleanup record，不得各自额外调度 neutral report。
4. neutral outcome 已决定后才清除匹配 generation、发布 lease released state 并完成 release future。stale/重复 release
   只观察既有 completion，不能清除新 generation。

seal 后使用该 lease action 返回 `Controller/InvalidArgument`；wrong-controller 和 stale generation 使用同一稳定投影。
若 Controller close 已先 seal 整个 lane，则使用既有 `Controller/DeviceDisconnected` closed-lane 投影。普通无 lease direct
在 Automation generation 存活期间仍 fail-fast `Controller/ResourceBusy`。

## Neutralize-and-release settled 合同

显式 release 的 neutralization intent 是无条件的：只要 stream 仍 connected 且可写，lane 必须调度恰好一个逻辑
`SwitchReport::NEUTRAL`，即使 desired report 已经中立、该 lease 从未提交 action 或前一 report 也是 neutral，也不得优化
掉。dispatch target 为 `max(clock.now_ns(), last_acceptance_ns + minimum_report_interval_ns)`；desired snapshot 在 dispatch
前设为 neutral。完整八字节被 transport 接受只产生 `NeutralAccepted`，不声明设备物理执行。

若 cleanup 开始时设备已断开，或 neutral write 失败/被 partial I/O 中断，lane 必须先调用幂等 transport close 并确认
stream 已 settled，再返回 `NeutralNotDeliveredStreamSettled(cause)`。该 variant 明确表示“未送达但不会再有该 stream 的
迟到 effect”，不是 neutral success；adapter 将 cause 记录为 cleanup warning/secondary diagnostic。

只有既不能证明完整 neutral transport acceptance，也不能证明 stream settled 时，release future 才返回
`Err(cleanup_failure)`。有效 `ControllerTransport::close` 合同下该分支仅用于被隔离的 close panic、内部 ownership 破坏或
等价不可恢复故障，使用现有 `Io/Transport` 或 `Internal/Internal` first error。此时 Controller 永久 seal admission，不能
继续写或重新 acquire；Runtime/resource close 必须保留真实 failure，不能清零 registry 或伪造 Closed。

cleanup record 一旦由 `neutralize_and_release` 创建，就不再观察 run cancellation token，也没有 caller deadline。它只受
Controller resource close 接管和既有 bounded transport write timeout 约束。release future 被 cancel/drop 不取消 cleanup；
run terminal 只能在 await 得到上述三类 settled completion 且 lease generation 已清除之后提交。

Controller close 与已开始的 release 竞争时只允许一个 lane cleanup owner；close 接管同一 record，并可把自己的 close
neutral write 作为该 release 的唯一 neutral attempt。release 完成后才开始的 resource close 仍按 ADR-0009 独立发送其
close neutral report，两次 report 均服从 pacing。

## Drop、close 与 waiter settlement

`AutomationLease::drop` 只能原子 seal generation 并非阻塞、幂等地触发与显式路径相同的 cleanup record；不得直接发送
裸 `ReleaseAutomationLease` 绕过 neutralization，不得等待、join 或声称 cleanup settled。若 lane 已由 close seal，Drop
只登记/合并 fallback，close owner 负责实际结算。只有 await 显式 `AutomationLeaseRelease` 的结果可作为 run terminal 前
的 settled proof。

Controller close 在 join writer 前必须：

1. seal 全部 acquire 和 action admission；
2. 按上述优先级结算所有 pending acquire，close 胜出者得到 `Closed`；
3. 结算所有已接纳 action，保留已线性化 effect，取消未线性化 effect，并先 settle 任何受损 stream；
4. 接管并完成所有显式或 Drop 触发的 release record，写入 accepted/not-delivered/failure 的唯一 completion；
5. 清除匹配 lease generation，唤醒全部 acquire/action/release waiter，再完成 transport close 和 lane join。

因此 port close 不能留下 blocked receiver/future、非终态 action Operation、悬空 lease 或等待 Drop 才完成的 correctness。
cleanup failure 可以使既有 Runtime deterministic close 进入保存的 failure outcome，但任何 waiter 都必须得到确定结果，
不得无限等待或观察虚假成功。

## D0、D1、D2 与下游 DAG

```text
D0: 本 Accepted docs-only narrow reopen
  -> D1: production Controller + behavior/schema/conformance/tests
  -> D2: full Workspace gates -> fixed-SHA independent review
         -> separate ADR refreezes ADR-0009

Phase 4: R0 / C1 / C2 / C3 / E1 / E2 / Q0 / Q1

Phase 5 concrete adapters require Q1 + D2.
```

D0/D1/D2 与 R0、C1、C2、C3、E1、E2、Q0、Q1 相互独立；任一 Controller 节点都不是 Phase 4 节点的前置，
Phase 4 Q1 也不等待 D2。D1 可以在本 ADR 后独立实现，但只拥有 production Controller 和相应 behavior/schema/
conformance/tests；不得加入 ECS、SDK、ABI 或 concrete adapter。D1 必须以确定性 barrier/VirtualClock 回归覆盖五态 acquire、
同点优先级、future abandonment、generation、action seal、三类 release outcome、pacing、Drop 和 close 全 waiter settlement。

D2 才运行完整 Windows Workspace 及 ADR-0009 受影响门禁，固定 candidate SHA，交给独立 reviewer 对完整 D0..D1 delta
审查，并以另一份 ADR 记录 refreeze SHA 和证据。D0 的三项 docs-only 门禁、D1 的局部或完整测试以及 reviewer 之外的
自查都不能替代 D2。任何 Phase 5 concrete adapter 必须同时等待 Phase 4 Q1 与 Controller D2，不能由本 ADR 提前设计。

## D0 验证边界

D0 只新增本 ADR，按 docs-only 规则运行：

```powershell
python tools/check_markdown_links.py
python tools/check_repository_guards.py
git diff --check
```

这些门禁只证明文档引用、仓库边界与 diff hygiene，不证明 Rust 编译、Controller 行为、并发竞态、serial/hardware、
Workspace、D1 或 D2 已通过。

## 关联

- [ADR-0009：冻结 Phase 2A Controller/Serial Candidate 基线](0009-phase-2a-freeze.md)
- [ADR-0017：冻结 Phase 4 ECS 与 Automation 目标](0017-phase-4-ecs-automation-target.md)
- [架构总览](../architecture/architecture-overview.md)
- [Runtime 生命周期](../architecture/runtime-lifecycle.md)
