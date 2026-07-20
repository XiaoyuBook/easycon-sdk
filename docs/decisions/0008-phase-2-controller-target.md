# 0008：冻结 Phase 2 Controller/Serial 开发目标

- 状态：Frozen Target
- 日期：2026-07-20
- 开发基线：`ece2f737b0bdcd693dc125796155480f8c51c9f5`
- 实现状态：未完成；本 ADR 冻结目标，不冻结实现

## 背景

Phase 1 Runtime 已按 [ADR-0007](0007-phase-1-freeze.md) 冻结。仓库还提前实现了 Controller
report/protocol、FakeTransport、连接状态机、单写者 lane、direct action、精确序列、Automation lease、
通用 ACK、取消和中立化，但 Windows serial、Amiibo、10,000-step 验收及物理设备能力数据尚未完成。

当前没有可用于开发和验收的 CH32 控制板。缺少硬件不阻止建立可注入的 Windows serial backend、字节级
设备模拟器和 Controller 完整无硬件链路，但阻止验证真实 VID/PID、固件能力、热拔插、UART/USB 时序、
Amiibo 容量和 Switch 可观察的端到端延迟。

### 源码与现有实现事实

1. **[源码事实]** `EasyCon/src/EasyCon.Device/NintendoSwitchPriv.cs:5-86` 把相邻报告的最小间隔固定为
   30 ms；空闲时发送线程等待事件，收到首个动作后立即被唤醒，只有仍早于上一报告形成的
   `_nextSendTime` 时才等待。30 ms 不是每次空闲后首个动作的固定前置延迟。
2. **[源码事实]** `EasyCon/src/EasyCon.Device/Utils/SwitchReport.cs:22-52` 把 7 个状态字节编码成 8 个
   串口字节。按常见 8N1 计算，完整报告在线路上的理论时间约为 0.694 ms（115200）或 8.333 ms
   （9600）；这不包含操作系统调度、驱动缓冲、固件处理、USB HID 轮询或 Switch 消费时间。
3. **[源码事实]** 当前参考快照只携带 CH32 firmware hex，没有可审计的固件时序实现或端到端测量数据。
   PC 侧 `Write` 返回也不能证明 Switch 已经物理执行输入。
4. **[已决定]** 当前候选实现的兼容默认值仍是 30 ms，且成功边界仍是完整报告被 transport 接受，见
   [behavior spec](../../spec/behavior/runtime-controller-v1.json)。该默认值不能被描述为已验证的硬件下限，
   transport acceptance 也不能被描述为硬件执行。

## 决策

### 阶段拆分

Phase 2 固定拆为两个连续门槛：

1. **Phase 2A：Controller/Serial Candidate（无硬件）**。完成 Windows x64 serial 系统叶子、
   ControllerTransport 接入、Amiibo、可重复模拟与故障测试、10,000-step fake 以及软件热路径测量。
2. **Phase 2B：Controller Hardware Qualification（有硬件）**。在实物可用后关闭 O-01、O-02、O-04，
   验证真实连接、断线、时序、Amiibo 和稳定性，并形成首批支持矩阵。

Phase 2A 可以独立完成、review 和冻结为候选基线，但不能据此把完整 Phase 2 标为完成，也不能宣称支持
任何具体控制板、固件或端到端延迟。没有硬件是 Phase 2B 的外部前置条件，不是 Phase 2A 的阻塞项。

### Phase 2A 交付范围

1. 创建 `easycon-serial`，只承担 Windows 10/11 x64 串口发现、打开、字节 I/O、取消、deadline 和关闭。
   发现结果返回稳定端口身份以及系统确实提供的 VID/PID 等可选属性；不得通过名称猜测受支持设备。
2. `easycon-serial` 通过既有 `ControllerTransport` 契约接入 `easycon-controller`。依赖方向固定为
   `easycon-serial -> easycon-controller -> easycon-runtime/easycon-model`；serial 只可为共享取消、时钟、
   ID 或错误类型按既有架构图直接依赖 Runtime/Model，任何依赖都不得反向。Controller 业务 crate 不依赖
   Win32、具体串口库或硬件枚举实现。
3. serial I/O 必须处理 partial read/write、零进展、绝对 deadline、operation/resource cancellation、端口占用、
   access denied、协议超时、关闭唤醒和热拔插错误归一化。取消或关闭不得留下后台线程、端口 handle 或
   无 owner 的 operation。
4. 建立不依赖物理 COM 口的可注入 byte-I/O adapter 和字节级 CH32 协议模拟器。模拟器必须覆盖握手、
   report framing、分段 I/O、迟到/重复/错误 ACK、断线和可控阻塞，且不能进入发布 feature。
5. 实现 v1 Amiibo save/select Controller API 和 transport primitive。保存按源码事实使用 20 字节分包，
   generation-matched ACK、取消、deadline、部分失败及断线恢复均进入共同测试；未验证的槽位数和总长度
   由显式 capability/limit 拒绝，不能猜测硬件支持。
6. 以 VirtualClock 和模拟器运行 10,000-step 精确序列，证明无丢失、无乱序、无早发、同 offset 合并、
   绝对目标无累计漂移，以及 success/failure/cancel/timeout/disconnect 后的唯一终态、lease 释放和中立化。
7. 增加可重复的软件延迟测量 harness，记录 command admission、lane wake/dispatch、write start、完整
   transport acceptance 的单调时间戳。测量资产不得把 fake acceptance 外推为 UART、CH32 或 Switch 执行。
8. 同步 behavior、fixture、conformance、开发文档和故障注入场景。Phase 2A 不导出正式 C ABI，不创建
   四语言 binding 或发布包。

### 低延迟目标

低延迟是 Phase 2 的显式验收目标，不以“Rust 理论上更快”代替测量：

1. lane 没有上一份已接受报告时，direct action 的目标时刻必须是当前单调时间；Runtime 和 serial 层不得
   增加固定 sleep、轮询周期或批处理等待。30 ms 兼容节拍只约束相邻完整报告。
2. Phase 2A 在固定电源策略、空闲的 Windows 10/11 x64 测试机上，以非阻塞内存 transport 至少采样
   10,000 次“lane 空闲且上一报告 pacing 已满足”的 eligible direct report。
   `command admitted -> transport write entered` 的 p99 目标不超过 1 ms、最大值不超过 5 ms。结果必须记录
   机器、Windows build、电源策略、样本量和 p50/p95/p99/max；普通 CI 不把墙钟阈值当作确定性
   correctness test，也不得用永久忙等换取结果。
3. 低延迟硬件档只适用于通过 Phase 2B 的 115200 设备。9600 下单帧理论线路时间已约 8.333 ms，不能
   承诺个位数的完整端到端延迟。
4. Phase 2B 的暂定目标是：空闲 direct action 从 command admission 到 CH32 收齐完整 8 字节报告，
   p95 小于 5 ms、p99 小于 10 ms；从 command admission 到对应 USB HID report 出现在 Switch 侧总线的
   中位数小于 10 ms，同时完整记录 p95/p99/max。USB protocol analyzer 或可审计 firmware trace 才能作为
   该边界的证据；包含游戏逻辑和屏幕刷新的摄影结果另行记录，不能混入此阈值。测量方法和最终支持阈值
   由 O-04 依据首轮实测冻结；未达目标不得静默放宽，必须保留原始结果并通过后续 ADR 调整目标或支持范围。
5. 小于 30 ms 的连续报告节拍只有在对应控制板、固件和 baud 通过无丢包/乱序的 10,000-report 实测后，
   才能进入该设备 capability。未经验证时保持 30 ms 兼容默认；优化不得改变取消、中立化、唯一终态、
   report 顺序或 Automation lease 语义。

### Phase 2A 退出门槛

Phase 2A 只有同时满足以下条件才能创建实现冻结 ADR：

1. 上述交付全部落地，Windows x64 production backend 可编译，所有串口系统调用均在 `easycon-serial` 内。
2. serial、Controller、Amiibo 和模拟器测试覆盖 success、failure、cancel、deadline、partial I/O、disconnect、
   close 及相关确定性竞态；不依赖随机 sleep 证明正确性。
3. 10,000-step fake、软件延迟测量和资源收敛验收通过并记录环境与结果。
4. 根 `AGENTS.md` 要求的完整门禁通过；每个独立任务均在通过门禁后形成语义完整的独立提交。
5. 固定实现提交经过独立 review，不存在未解决且可复现的 in-scope P0/P1/P2 finding。
6. 文档明确标注 `Hardware Unverified`，O-01、O-02、O-04 保持开放，完整 Phase 2 保持未完成。

### Phase 2B 退出门槛

1. 形成包含控制板、固件、VID/PID、串口身份和 baud 的 O-01 支持矩阵。
2. 完成 100 次发现/连接/断开、端口占用、热拔插、电源循环、全部 direct action 和中立化延迟验收。
3. 关闭 O-02 的 Amiibo slot/长度能力，并覆盖分包、错误恢复和取消。
4. 物理 10,000-report/step 顺序零错误；满足该设备声明的连续节拍与 O-04 延迟阈值。
5. 固定硬件、Windows build、USB controller、电源策略和测量方法，保存原始分布而不是只报告最佳值。
6. 固定实现与硬件证据经过独立 review 后，另建 ADR 冻结完整 Phase 2 基线。

## 禁止项

Phase 2A/2B 不得：

- 修改、跟踪、构建或下载 `EasyCon/`，也不得加入 firmware、烧录或字节码能力；
- 修改 Phase 1 Runtime 语义来迁就 serial backend；若确有必要，必须按 ADR-0007 重新打开 Phase 1；
- 引入 Host、JSON-RPC、WebSocket、服务进程、UI、远程助手或网络控制面；
- 提前实现 Vision、ECS、C ABI、C++/.NET/Python/Node binding 或发布包装；
- 以 COM 名称、参考源码注释、fake 或理论计算宣称硬件支持和端到端性能。

## 重新打开规则

- 修改 Phase 2A 范围、依赖方向、ControllerTransport 所有权、单写者/lease/中立化语义、性能测量边界或
  退出门槛，必须先修改本 ADR 并 review，不能在实现后补写理由。
- 保持冻结目标的普通实现选择和 bug fix 不重新打开目标，但行为变化必须同步 spec、fixture、conformance
  和文档。
- Phase 2A 实现冻结后，production serial/Controller/Amiibo、相关 behavior/schema/fixture/conformance 或
  性能 harness 的变化会重新打开 Phase 2A 实现基线，直到完整门禁和独立 review 再次通过。

## 结果

- 无硬件开发有明确完成点，不会把硬件缺口伪装成软件阻塞或测试通过。
- 低延迟从语言选择和主观体验转成分段时间戳、分位数和设备能力证据。
- 30 ms 源码兼容默认与空闲首动作延迟被明确区分；低于 30 ms 的连续节拍必须由具体硬件证明。
- 系统串口保持叶子依赖，Controller 核心、Runtime 和后续语言 SDK 不绑定 Windows I/O 细节。

## 关联

- [ADR-0007：冻结 Phase 1 Runtime 基线](0007-phase-1-freeze.md)
- [源码能力映射](../architecture/source-capability-map.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
- [实施路线](../architecture/repository-roadmap.md)
- [Runtime + Controller fake vertical slice](../development/runtime-controller-vertical-slice.md)
