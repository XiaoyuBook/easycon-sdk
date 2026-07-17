# Runtime + Controller fake vertical slice

## 交付范围

首个开发里程碑在冻结架构下实现且只实现以下 Rust 组件：

- `easycon-model`：Runtime/operation/resource/task ID、稳定错误域、按钮、HAT 和摇杆值。
- `easycon-runtime`：operation 状态机、取消树、deadline coordinator、VirtualClock、严格有序事件
  subscription、registry 和幂等确定性关闭。
- `easycon-controller`：源码精确 Switch report、ControllerTransport、连接状态机、单写者
  command lane、direct/reset、精确序列、Automation lease 原语和 ACK generation matcher。
- `easycon-test-support`：只供测试使用的 FakeControllerTransport 与 vertical slice harness。

所有 fake 和验收测试都不读取、构建或下载 `EasyCon/`，也不需要物理硬件、C++ 或 OpenCV。
完整行为资产位于 [spec/README.md](../../spec/README.md)。

## 行为边界

- Operation 只允许 `Pending -> Running -> Succeeded/Failed`，或经 `Cancelling -> Cancelled`；
  result/error 终态只提交一次。
- Controller operation 是 Controller resource cancellation token 的子节点；父资源或 Runtime 取消会先
  推进 operation 到 `Cancelling`，父 operation 终结也会取消仍活动的后代；跨 Runtime 或已终结的
  parent token 会在 admission 时拒绝。
- wait timeout 只结束观察；operation deadline 由 Runtime worker 自动请求取消；握手/ACK protocol
  timeout 提交 Controller failure；report/command write 使用独立的 1 s 默认 I/O deadline，ACK reply
  timeout 从完整 command write 被 transport 接受后开始。
- 每个 subscription 有独立有界队列。普通事件溢出合并为 `EventGap`，operation/resource
  query 始终是权威状态；并发发布和 gap 均保持严格递增 sequence。
- 每个 Controller 只有一个 writer thread。普通 report 的默认最小间隔是 30 ms；write 必须响应
  operation/resource cancellation 或绝对 I/O deadline。
- precise sequence 的 offset 相对 lane 获权时刻且始终为绝对目标；同 offset 按输入顺序合并
  成一个 report，不从上次 dispatch 累加目标。
- sequence 取消或可恢复失败时，先让 transport 接受 neutral report，再释放 lease 并提交终态。
  断线导致 neutral 无法送达时发布 warning，绝不声称硬件已经中立。
- 普通 report operation 的 `Succeeded` 只表示完整字节被 transport 接受。事件 detail 明确记录
  `hardware_execution=false`；acceptance timestamp 在完整 write 返回后采样，close neutral report 也计入
  report acceptance 观测。
- ACK command 在同一 FIFO lane 中等待前序 direct report，只有独占 sequence/Automation lease 才
  返回 `RESOURCE_BUSY`；ACK 路径发现断线时重置 desired report、记录中立化 warning 并关闭 transport。
- Runtime close 先取消根树，再关闭/中立化并 join Controller，最后发布 `runtime.closed`；关闭后
  operation/resource/task 计数全部为零，`runtime.closed` 之后的事件发布会被拒绝。

## 本地验证

目标工具链固定为 Rust 1.97.1 与 `x86_64-pc-windows-msvc`。在能发现 MSVC linker 和 Windows
SDK libraries 的开发 shell 中运行：

```powershell
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python tools/validate_specs.py
python tools/check_markdown_links.py
python tools/check_repository_guards.py
git diff --check
```

端到端测试是 `tests/support/tests/vertical_slice.rs`，执行路径为 Runtime create -> fake connect ->
direct action -> precise sequence -> cancel/neutralize -> Runtime close，并核对 event/operation、report
bytes/timestamp、lease 顺序和 registry 计数。

## 有意保留的差异

以下差异由本里程碑边界决定，不改变冻结架构：

1. `Runtime::close` 当前 Rust 内部接口同步等待真实 `Closed`；带 caller wait timeout 的版本化公共
   形态留给正式 C ABI 阶段，核心关闭顺序和幂等语义已经实现。
2. 本阶段只有 FakeControllerTransport。Windows serial discovery/open/cancellable I/O 是后续系统
   leaf backend，不允许为 fake 测试引入硬件依赖。
3. ACK 以通用内部 command primitive 验证 generation、timeout 和 close wake；Amiibo public API、
   分包能力上限和物理设备数据留待相应产品/硬件里程碑。
4. Automation 只实现独占 lease 的 acquire/authorized write/release，不包含 ECS 编译器、evaluator
   或跨域运行逻辑。
5. 没有正式 C ABI、C++ bridge、语言绑定或发布包；当前 Rust public items 仍是实现候选，不构成
   v1 ABI 承诺。
