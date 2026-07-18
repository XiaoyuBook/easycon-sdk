# 0004：操作句柄、拉取事件与确定性关闭

- 状态：Accepted
- 日期：2026-07-17

## 背景

设备连接、动作序列、ECS 运行、capture 和 OCR 都可能阻塞、超时或取消。四种语言拥有不同的异步模型，跨 C ABI callback 又会引入线程重入、语言 runtime 生命周期和异常回传问题。

原源码的多个后台任务由 UI 服务清理，取消可能跳过按键释放，连接/capture 关闭也没有统一 join 约束。这些行为不能成为嵌入式库的生命周期。

## 决策

1. 所有耗时 API 返回 opaque operation handle。
2. Operation 使用 Pending/Running/Cancelling/Succeeded/Failed/Cancelled 状态，终态只提交一次。
3. wait timeout 只终止等待；operation deadline 和协议 timeout 分别建模。
4. cancel 是幂等请求，语言异步对象只在核心清理完成并进入终态后结束。
5. 日志、状态、错误和完成通知通过每订阅者独立的有界拉取队列提供，不调用用户 callback。
6. Runtime 是根取消/资源 owner，按 Automation、Controller、Capture、native pool、event/executor 的顺序关闭。
7. Automation 任何终态之前必须中立化 Controller 并释放写 lease。
8. Event 只用于观察；Operation/resource query 是权威状态，队列 overflow 不得破坏正确性。

## 理由

- Operation 可自然映射 C++ Operation、.NET Task、Python awaitable 和 Node Promise。
- 拉取事件避免核心线程重入语言 runtime，也允许每语言自行调度 continuation。
- 显式 Cancelling 状态避免把“已请求停止”误报为“资源已经释放”。
- 根所有权和 task supervisor 可以验证 Runtime Closed 时没有脱管线程。

## 结果

- C ABI 增加 operation wait/cancel/status/result/error 和 event subscription/read/release。
- binding 必须保留 operation handle 到终态，不能因 Future/Task 对象被丢弃就释放监管。
- event queue 为 terminal/error/state 保留容量，日志溢出产生 gap event。
- 主动资源需要显式 close；RAII/SafeHandle/GC finalizer 只能报告遗漏并释放已关闭 wrapper，不能
  执行确定性 close。详细边界由 [ADR-0006](0006-runtime-stabilization.md) 收紧。
- 所有 capture/serial backend 必须提供可中断关闭；不能 detach 卡住的线程后宣称 Closed。

## 被否决的方案

- C ABI user callback：线程、重入和异常规则过于脆弱。
- 每种语言自行实现 timeout/retry：会产生行为差异。
- release 自动等同 cancel：观察者释放不应改变共享 operation。
- 仅靠析构/GC：无法给出确定性硬件中立化和 task join。

## 关联

- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [C ABI v1](../architecture/c-abi-v1.md)
- [语言绑定](../architecture/language-bindings.md)
