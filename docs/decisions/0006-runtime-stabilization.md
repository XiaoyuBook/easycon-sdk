# 0006：Runtime 所有权、终态事务与确定性关闭

- 状态：Accepted
- 日期：2026-07-18

## 背景

第一版 Runtime vertical slice 已经具备 operation registry、取消树、资源 callback、任务计数和同步
`close`，但仍把多个不同语义压在 `Closing` 与 panic 上：最后一个 Rust owning handle 的 `Drop`
会创建 finalizer 线程并最终发布 `RuntimeClosed`；任务由调用方手工注册并绑定线程；关闭失败以
`Closing + close_failed` 表示；operation 终态提交中的 event、registry 或锁故障可以跳过 waiter
通知。这些行为无法为后续 C ABI 和四种 binding 提供可验证的所有权边界。

本 ADR 只稳定 Phase 1 Runtime。它不冻结正式 C ABI，也不引入 serial backend、Vision、ECS、
binding、固件或 UI。

## 决策

### 显式关闭与 Drop

1. `Runtime::close` 是唯一确定性关闭路径。只有它可以返回保存的 `Closed` 或 `CloseFailed`
   outcome，并承诺资源 cleanup、operation 收尾、受监管任务 join、registry 收敛和最终事件。
2. 最后一个 owning `Runtime` handle 的 `Drop` 只原子拒绝后续 admission 并请求根取消。它不等待，
   不执行 `ManagedResource::close`，不 join task，不创建 finalizer 线程，不关闭事件生产端，也不
   承诺 `RuntimeClosed`。
3. 后续正式 binding 的 Runtime `release` 必须只释放已经显式 close 完成的对象。GC/finalizer、
   RAII destructor 和进程退出 hook 只能报告遗漏或执行非阻塞 release，不能替代 close。

### Runtime 受监管任务

1. 长期任务只能通过 `Runtime::spawn_supervised` 或等价的 Runtime-owned API 创建。
2. Runtime 在任务代码执行前完成 admission 和 task registry 登记；wrapper 自动绑定 owner thread，
   保存完成通知与 `JoinHandle`，捕获 task panic，并在退出/join 后自动注销。
3. `TaskRegistration`、`bind_to_current_thread` 和“调用方必须在正确线程 drop guard”的协议不再是
   public correctness primitive。
4. 受监管 task 内同步调用同一 Runtime 的 `close` 必须在 Runtime 状态变化和关闭副作用之前返回
   明确的 caller rejection；外部 owner 随后仍可正常 close。

### CloseOutcome 与失败终态

1. Runtime 状态固定为 `Active`、`Closing`、`Closed`、`CloseFailed`。
2. 第一个外部 close caller 原子取得 close ownership 并从 `Active` 进入 `Closing`。从这一刻起，
   admission 永久拒绝；并发 caller 等待同一次 close。
3. 成功关闭保存 `CloseOutcome::Closed`。任一不可恢复的 resource、task、event 或 registry 故障保存
   `CloseOutcome::Failed(CloseReport)`，进入 `CloseFailed`，发布 `runtime.close_failed` 作为最终事件，
   关闭事件生产端并唤醒全部 close waiter。
4. `CloseReport` 至少保留失败阶段、稳定诊断、相关 resource/task ID（若可用）和当时 registry
   counts。重复 close 返回同一个已保存 outcome，不重复取消、callback、join 或最终事件。
5. `Closed` 只能在 resource cleanup、owner task 退出、全部 task join、operation 收尾和 registry
   检查完成后提交。`RuntimeClosed` 只能在这些条件已经满足后发布。
6. 若 resource cleanup 或 owner task 未完成，失败路径不得为了清零计数而伪造 operation
   `Cancelled`/`Failed`。operation 维持 `Cancelling`，直到其真实 owner 完成 cleanup 并提交终态。

### Operation 终态事务与取消

每次成功、失败或取消终态按一个不可中断事务线性化：

1. 在 parent cancellation node 上禁止新的 child admission。
2. 封闭并取消已经 admission 的 cancellation subtree；每个 hook panic 单独隔离，其他 hook 和 child
   仍继续传播。
3. 执行 owner cleanup；cleanup panic 被转换成可诊断的 internal failure，不得遗留半提交终态。
4. 提交唯一、不可变的 state/result/error，并尝试发布唯一 terminal event。
5. 从 Runtime operation registry 注销。
6. 无条件唤醒全部 operation waiter。

event delivery、registry 清理或 hook 的局部故障不得跳过步骤 5 或 6。Runtime 同步路径中的 poisoned
lock 必须恢复 guard 或使用不传播 poison 的原语，不能把先前 panic 扩大为永久等待。

## 结果

- 忘记显式 close 不再产生一个看似成功但不可等待、不可报告的后台关闭流程。
- task owner 与 self-wait 判断由 Runtime 创建路径建立，不依赖调用方记住额外绑定步骤。
- close caller 可以区分成功、全局关闭失败和受监管 task 的 caller misuse；失败诊断可重复查询。
- operation terminal event 仍是观察面；snapshot/result/error 和 registry 顺序保持权威，waiter 不依赖
  event queue 正常工作。
- Controller vertical slice 只需最小适配：其 lane 由 Runtime spawn，并在 resource cleanup 中请求退出；
  Controller 协议、调度和 transport 行为不改变。

## 被否决的方案

- `Drop` 同步 close：可能在受监管 worker 或 unwind 中 self-wait、执行用户资源代码并二次 panic。
- `Drop` 启动 detached finalizer：无法向已经释放 owning handle 的调用方交付关闭结果，也让
  `RuntimeClosed` 的含义依赖线程创建是否成功。
- 保留 public task guard：允许未绑定、过早 drop 或跨线程移动，Runtime 无法证明 join ownership。
- `Closing + bool + panic`：不能表达保存的失败终态，并发 waiter 也没有稳定结果。
- close failure 时强制终结全部 operation：会在真实 owner cleanup 前伪造完成。

## 关联

- [ADR-0004：操作句柄、拉取事件与确定性关闭](0004-operations-events-shutdown.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
