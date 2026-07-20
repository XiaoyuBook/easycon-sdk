# Runtime + Controller/Serial Phase 2A Candidate

## 里程碑状态

本文实现中的 Phase 1 Runtime 已按
[ADR-0007](../decisions/0007-phase-1-freeze.md) 冻结在 `4261925dc4e84b36e8491c0a97c17048d3eacd84`。
基于 [ADR-0008](../decisions/0008-phase-2-controller-target.md)，Windows serial、ControllerTransport、
Amiibo、故障注入、10,000-step fake 和软件热路径测量已经收敛为 Phase 2A Candidate，可提交独立 review。

当前状态严格为 **Hardware Unverified**。没有物理 CH32/控制板参与开发或验收；O-01、O-02、O-04
保持开放，完整 Phase 2 仍未完成。本文不声称任何具体 VID/PID、固件、baud、Amiibo 容量、UART/USB HID/
Switch 执行时序或物理中立化已经验证，也不创建完整 Phase 2 冻结 ADR。实现冻结由后续独立 review 完成。

## 交付范围

首个开发里程碑在冻结架构下实现且只实现以下 Rust 组件：

- `easycon-model`：Runtime/operation/resource/task ID、稳定错误域、按钮、HAT 和摇杆值。
- `easycon-runtime`：operation 状态机、取消树、deadline coordinator、VirtualClock、严格有序事件
  subscription、registry 和幂等确定性关闭。
- `easycon-controller`：源码精确 Switch report、ControllerTransport、连接状态机、单写者 command lane、
  direct/reset、精确序列、Automation lease、ACK generation matcher，以及 Amiibo save/select operation。
- `easycon-serial`：Windows 10/11 x64 结构化发现、稳定端口身份、Win32 overlapped byte I/O 和
  `SerialControllerTransport`。所有串口系统调用、HANDLE/event/SetupAPI/registry RAII 都封装在此系统叶子。
- `easycon-test-support`：只供测试使用的 FakeControllerTransport、可注入 byte I/O、字节级 CH32 模拟器、
  close barrier、latency recorder 和 release measurement harness；不进入发布 feature。

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
  query 始终是权威状态；同一个 subscription 只接纳一个 active reader，并发 reader 返回
  `RESOURCE_BUSY`；并发发布和 gap 均保持严格递增 sequence；最终 `runtime.closed` 不受 subscription
  severity/log filter 影响。
- 每个 Controller 只有一个 writer thread。所有 report（包括取消和关闭中立报告）的默认最小间隔是
  30 ms；write 必须响应 operation/resource cancellation 或绝对 I/O deadline。
- Windows discovery 返回系统提供的稳定 device-instance identity 与可选属性；COM 名称只用于打开端口，
  不用于猜测 VID/PID 或支持设备。Win32 open/read/write/close 只存在于 `easycon-serial`。
- serial partial write 使用同一个 logical `WriteContext` 连续推进；半帧错误或两次 partial call 间取消会
  关闭流，禁止后续 command 拼接到损坏帧。零进展、busy/access denied、deadline、close wake 和热拔插
  归一化为稳定错误。
- Win32 `HANDLE`、event、SetupAPI list 和 registry key 均由窄 RAII owner 释放；pending `OVERLAPPED` 和
  调用方 buffer 在 completion、cancel、wait failure 以及可注入 `Clock` panic 路径上都先经
  `CancelIoEx`/`GetOverlappedResult` 同步结算，再允许释放或继续 unwind。
- precise sequence 的 offset 相对 lane 获权时刻且始终为绝对目标；同 offset 按输入顺序合并
  成一个 report，不从上次 dispatch 累加目标。
- sequence 取消或可恢复失败时，先让 transport 接受 neutral report，再释放 lease 并提交终态。
  断线导致 neutral 无法送达时发布 warning，绝不声称硬件已经中立。
- 普通 report operation 的 `Succeeded` 只表示完整字节被 transport 接受。事件 detail 明确记录
  `hardware_execution=false`；acceptance timestamp 在完整 write 返回后采样，close neutral report 也计入
  report acceptance 观测。
- ACK command 在同一 FIFO lane 中等待前序 direct report，只有独占 sequence/Automation lease 才
  返回 `RESOURCE_BUSY`；ACK 路径发现断线时重置 desired report、记录中立化 warning 并关闭 transport。
- Controller generation matcher 和 command 前 input purge 可隔离 fake 中的旧 generation 及已经排队的
  重复字节；CH32 wire ACK 本身没有 generation 字段，未来才到达的物理迟到 ACK 归属仍需 O-01/O-02
  实物验证，Phase 2A 不把软件 matcher 外推为硬件保证。
- Amiibo save 使用 `A5 off_lo off_hi len_lo len_hi slot 90` header 和最多 20 字节 payload，两段分别等待
  generation-matched `FF` ACK；select 使用 `A5 slot 91`。失败重试前发送三次 `A5 81` 并等待 `80`。
  retry 有界；重试耗尽的部分失败仍执行一次 bounded reset，reset 失败则先关闭 stream；部分完成数、
  取消、绝对 deadline、断线和 cleanup 都进入 operation 终态契约。
- Controller 默认没有 Amiibo slot/总长度 capability。只有显式 `AmiiboLimits` 才接纳请求；该 limit 本身
  不构成硬件支持证据，O-02 仍开放。
- direct report 的 `WriteContext` 记录 command admission 和 lane wake，原有 `timestamp_ns` 记录 dispatch；
  非阻塞内存 transport 在 trait entry 和完整 acceptance 记录后两段时间戳。五段使用同一单调 Runtime clock。
- 显式 Runtime close 会先取消根树，再关闭/中立化 Controller，等待 owner cleanup，join 全部
  Runtime-owned supervised task，兜底终结 owner 已退出后的遗留 operation，join deadline worker 并验证
  registry 为空，最后发布 `runtime.closed`、保存 Closed outcome。`runtime.closed` 之后拒绝 event publish。
- 最后一个 owning Runtime 句柄 Drop 只同步进入 `Closing`、拒绝 admission 并请求根取消；它不等待 task、
  不执行 resource callback、不创建 finalizer 线程、不关闭 subscription，也不承诺 `runtime.closed`。
- Controller lane 与 Runtime worker 只通过 Runtime supervised spawn 创建。Runtime 自动绑定 task owner、持有
  完成通知和 `JoinHandle` 并注销；不再要求 worker 手工 bind task registration。task 内同步 close 在状态
  变化前返回明确 rejection，外部 owner 仍可 close。
- 单个 `ManagedResource::close` panic 会被隔离，其他健康 resource 继续关闭；Runtime 保存带 resource/task
  诊断和 counts 的 CloseFailed outcome，以 `runtime.close_failed` 终止事件流并唤醒 concurrent/later close
  caller。owner cleanup 或 owner task 尚未退出的 operation 不会被强制伪造成 Cancelled/Failed。

## 本地验证

目标工具链固定为 Rust 1.97.1 与 `x86_64-pc-windows-msvc`。在能发现 MSVC linker 和 Windows
SDK libraries 的开发 shell 中运行：

```powershell
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python tools/run_runtime_models.py
python tools/validate_specs.py
python tools/check_markdown_links.py
python tools/check_repository_guards.py
git diff --check
```

端到端测试是 `tests/support/tests/vertical_slice.rs`，执行路径为 Runtime create -> fake connect ->
direct action -> precise sequence -> cancel/neutralize -> Runtime close，并核对 event/operation、report
bytes/timestamp、lease 顺序和 registry 计数。

Phase 2A 专项测试还包括：

- `tests/support/tests/serial_ch32.rs`：Windows-independent byte chain、partial/zero/block、ACK fault、deadline、
  cancel、close、hot unplug、半帧失败和 stream 计数；
- `tests/support/tests/amiibo.rs`：8 条 save/select success/failure/cancel/deadline/disconnect/cleanup 路径；
- `tests/support/tests/phase2a_sequence.rs`：10,000 input steps、5,000 same-offset merged reports，逐个绝对
  target 验证无丢失、乱序、早发和漂移，并在 close 后验证三个 registry 为零；
- `tests/support/tests/phase2a_latency.rs`：五段时间戳单调性与不丢样本的确定性 contract。

当前完整 workspace 为 154 个非文档测试通过；Runtime Loom 模型 6/6；规范校验执行 5 schemas、1 behavior、
3 controller fixtures、9 scenarios 和 61 个 exact Rust tests。最终提交前仍以实际完整门禁输出为准。

## 软件路径延迟结果

正式命令和方法见[测试策略](../architecture/testing-strategy.md#phase-2a-无硬件延迟证据)，结构化结果见
[latency fixture](../../spec/fixtures/controller/phase2a-latency-result-v1.json)。环境为：

- `DESKTOP-IQM6HN5`，Intel Core i7-11800H，16,964,685,824 bytes RAM，x86_64；
- Microsoft Windows 11 Home China 25H2，build `10.0.26200.8875`；
- `GamePP 电源方案`；Rust/Cargo 1.97.1，`x86_64-pc-windows-msvc`；
- release build，1,000 warmup，10,000/10,000 eligible measured samples，1 ns measurement-only pacing，
  non-blocking memory transport；未过滤 outlier，未永久 busy wait。

| 单调时间段 | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: |
| admitted -> lane wake | 800 ns | 1,000 ns | 4,800 ns | 147,200 ns |
| lane wake -> dispatch | 700 ns | 800 ns | 1,000 ns | 8,500 ns |
| dispatch -> transport write entered | 200 ns | 300 ns | 400 ns | 6,000 ns |
| write entered -> transport accepted | 100 ns | 100 ns | 100 ns | 3,000 ns |
| admitted -> transport write entered | 1,600 ns | 1,900 ns | 5,800 ns | 148,500 ns |
| admitted -> transport accepted | 1,700 ns | 2,000 ns | 5,900 ns | 148,600 ns |

ADR-0008 的软件路径目标为主指标 p99 <= 1,000,000 ns、max <= 5,000,000 ns，本次结果通过。raw CSV
为 10,001 行（含 header），SHA-256 为
`8A7C803777E70D30F92898C23E3DF3821973C49D691DE4481E22066D45A0DCA6`。这些数值只代表本机进程内
软件路径，不得外推为 UART、CH32、USB HID、Switch 总线、固件或游戏画面延迟。

## 有意保留的差异

以下差异由本里程碑边界决定，不改变冻结架构：

1. `Runtime::close` 当前 Rust 内部接口同步等待保存的 Closed/CloseFailed outcome。带 caller wait timeout 的
   版本化公共形态留给正式 C ABI 阶段；binding 最终 release 必须先显式 close 并检查真实 outcome。
2. Windows system serial backend 已实现并在 Windows x64 编译/测试，但没有目标控制板；真实 VID/PID、
   固件、115200/9600 能力和热拔插行为留给 O-01/Phase 2B。
3. Amiibo public Rust API 和源码精确分包已实现；物理 slot 数、总长度和设备恢复行为留给 O-02/Phase 2B。
4. Automation 只实现独占 lease 的 acquire/authorized write/release，不包含 ECS 编译器、evaluator
   或跨域运行逻辑。
5. 软件延迟 harness 只覆盖进程内 memory transport；UART/CH32/USB/Switch 时序和连续节拍留给
   O-04/Phase 2B。
6. 没有正式 C ABI、C++ bridge、语言绑定或发布包；当前 Rust public items 仍是实现候选，不构成
   v1 ABI 承诺。
