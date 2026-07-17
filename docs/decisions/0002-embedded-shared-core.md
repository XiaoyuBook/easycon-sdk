# 0002：用户进程内的 Rust 共享核心

- 状态：Accepted
- 日期：2026-07-17

## 背景

SDK 需要让 C++、.NET、Python 和 Node.js/TypeScript 使用相同的 Controller、Automation 和 Vision 行为。此前试验过的额外服务进程方案已经从工作树和历史中清除，不再构成产品方向或兼容约束。

如果每种语言分别实现状态、调度和资源管理，会立即产生时序、错误和取消差异；如果要求用户先启动额外进程，则违背“用户进程内直接调用”的产品方向并增加部署、版本协商和故障面。

## 决策

1. SDK 核心作为 native dynamic library 直接加载到用户进程。
2. Rust 实现唯一的业务主核心：Runtime、状态机、调度、operation、事件和资源生命周期。
3. C++、.NET、Python、Node.js/TypeScript 绑定都通过同一 C ABI 调用该核心。
4. v1 不启动、安装或依赖额外服务进程，不定义进程间或网络控制协议。
5. Runtime 是实例对象；除版本/build 查询外不使用进程全局业务状态。

## 理由

- Rust 适合表达所有权、并发和 panic 隔离，可把四语言行为集中在一个实现。
- 进程内调用减少部署物、启动步骤、序列化和故障协商。
- 稳定 C ABI 能覆盖首批四种语言，同时避免把 Rust ABI 当公共契约。
- 多 Runtime 实例比应用级静态单例更适合嵌入测试、服务和桌面程序。

## 结果

正面结果：

- 四语言共享 Controller 时序、ECS、Vision、错误和关闭语义。
- 用户只安装对应语言包和原生 runtime bundle。
- fake backend 可在同一核心中驱动共同 conformance。

代价：

- native crash 会影响调用进程，因此 panic/exception、unsafe、原生依赖和故障注入门槛更高。
- 每种语言必须正确适配 native 生命周期，不能依赖进程隔离回收资源。
- 更新 native 核心需要严格 ABI 和包版本检查。

## 被否决的方案

- 每语言独立实现：无法合理保证精确时序和一致性。
- 以 .NET 原源码作为运行时并从其他语言嵌入 CLR：部署和语言 runtime 耦合过重，不能形成轻量稳定边界。
- 额外服务进程：与已决定的直接嵌入方向冲突。

## 关联

- [架构总览](../architecture/architecture-overview.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [ADR-0001](0001-source-boundary.md)
