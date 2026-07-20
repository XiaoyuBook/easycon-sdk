# Phase 2B 设备身份接纳与诊断 readiness 证据设计

- 状态：Architecture Target (`Hardware Unverified`)
- 日期：2026-07-20
- 设计基线：`b8abb37466cce215d3e9ce0b16dbec4b8e25749d`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 相邻设计：[run directory ownership](phase2b-run-directory-ownership.md)

## 问题与事实

**[源码事实]** EasyCon 的 `EasyCon.Device/ECDevice.cs` 只调用
`SerialPort.GetPortNames()`，应用再把一个 COM 名交给连接路径。该源码没有稳定设备身份、板型探测、固件版本
探测或 capability 协商，因此不能作为按 COM 名安全选择设备的依据。

**[源码事实]** SDK 的 `easycon-serial::SerialPortDescriptor` 已把 Windows device-instance ID 定义为独立于
当前 COM assignment 的 stable identity，并把 COM 名、friendly name、manufacturer 和系统提供的 VID/PID
分开保存。Windows discovery 按 stable identity 排序并去重；`WindowsByteIoFactory` 最终仍按 descriptor 中的
COM 名排他打开串口。

**[源码事实]** 当前资格 CLI 的 `Harness::new(port_name, ...)` 会重新按 COM 名发现 descriptor。所有动作命令
只要求 `--port`；`hotplug` 虽然等待原 stable identity 消失和返回，返回后仍按旧 COM 名调用 `Harness::new`。
因此目标设备换到新 COM、或另一设备占用旧 COM 时，旧实现可能选择错误设备。当前 `smoke --wake-home` 还在
代码中固定等待 3 秒，输出没有保存该配置，也没有区分“完成 diagnostic prelude”和通用设备 readiness。

已校验交接证据只说明：COM8 identity
`USB\VID_1A86&PID_FE0C\8&2D69411B&0&7` 曾三次发现一致并在 115200 首次握手成功；同一会话 Home、中立、
配置为 3 秒的等待和 500 ms A hold 后，用户确认 A 生效。COM7 是另一设备。板型、固件、重连稳定性、完整动作、
Amiibo 和物理时序都未知。该观察不能产生通用 Home prelude、readiness 或 capability。

## 目标与范围

本任务只在 `publish = false` 的 `tests/hardware` workspace 内实现：

- 每个可能打开串口的命令显式接收 expected stable identity；
- 初始 `port hint + expected identity` 接纳、每次 native open 前后复核和 hotplug identity 重绑定；
- 将 identity admission、protocol connection、diagnostic prelude、operator observation 和支持资格保持为不同层；
- 为这些状态保存结构化、机器可判定的本次运行证据；
- 把 `smoke` 的 Home 后等待改成显式、有界配置，并记录真实 prelude 顺序；
- 用可注入 discovery、byte-I/O 和 monotonic time 完成无硬件回归。

本任务不实现 durable journal/manifest/checkpoint、build provenance、Ctrl+C、`OperatorPort`、Amiibo
write-ahead、跨进程 device lock 或 stale run 恢复。它不修改 production Controller/Serial 的公共行为，不创建
支持矩阵行，也不运行物理命令。上述证据持久化和操作员任务必须继续复用这里的 typed admission 结果，不能重新
退化为按字符串或 COM 名推断。

## Readiness 分层与 capability 边界

资格工具不定义一个可向上升级的 `ready: bool`。以下五层彼此独立，前一层只允许尝试下一步，不证明下一层：

1. `IdentityAdmission`：系统当前同时观察到 expected stable identity 与调用者给定的 initial COM hint，允许尝试
   打开该 descriptor。
2. `OpenIdentityGuard`：native stream 打开前和排他 handle 建立后，系统仍观察到同一 identity/COM mapping；在
   post-open 复核完成前不发送任何 handshake 或 report bytes。
3. `ProtocolConnection`：本 session 的 source-exact handshake 成功，并记录实际 baud 和 attempts。
4. `DiagnosticObservation`：本 run 按显式顺序执行某个 device-specific prelude 或 action，并记录 transport 与
   操作员/仪器结果；它只约束该 action。
5. `SupportQualification`：只有 checkpoint 汇总板型、固件和 ADR-0008 全部门槛后才能建立；本任务永远不产生。

`IdentityAdmission` 和 `ProtocolConnection` 都不推断板型、固件、按钮、Amiibo、最小节拍或时序 capability。
friendly name、manufacturer、VID/PID、baud 成功和 Home 观察只作为证据字段。结果继续显式保存
`capability_inference = "none"`；调用方声明的 `AmiiboLimits` 仍是 `Hardware Unverified` declared limits，不能
由 identity 接纳升级为观察能力。

## 组件与所有权

### `DeviceTargetRequest`

每个动作命令在创建 run-directory reservation 后、构造 `Harness` 前规范化：

```text
DeviceTargetRequest {
  expected_stable_id,
  initial_port_hint,
}
```

`--expected-identity` 必须非空并按 discovery 输出原样提供；identity 使用逐字节精确比较，不做大小写折叠、Unicode
规范化、VID/PID 提取或前缀匹配。`--port` 只按 ASCII case-insensitive 比较合法 COM 名。严格 identity 比较可能
保守拒绝大小写不同的人工输入，但不会扩大被接纳设备集合。

`discover` 不打开设备，因此不要求 expected identity。未授权的 `amiibo` 在任何设备动作前返回 `not_run`，也不
强迫调用者提供 identity；一旦请求写入授权，identity、port、slot、limits 和 payload hash 都是必需参数。
其余动作命令缺少 identity 或 port 是 CLI contract error：提交 `failed/failed` artifact、退出 1，且不发现或打开
设备。

### `DeviceDiscovery`

资格 workspace 定义窄的可注入 discovery owner，production adapter 只委托
`WindowsSerialDiscovery::discover`。一次调用返回不可变、排序的 snapshot；selector、hotplug poll 和 open guard
都消费 snapshot，不读取全局缓存。fake 以预定 snapshot/error 序列驱动测试，不使用 sleep 或真实串口。

### `IdentityAdmission`

selector 对一次 snapshot 产生以下 typed decision：

- `Admitted(AdmittedDevice)`：恰有一个 descriptor 的 stable identity 精确匹配 expected，且它的 COM 名匹配
  initial hint。
- `ExpectedAbsent`：snapshot 不含 expected identity。
- `ExpectedAtDifferentPort`：expected identity 存在，但当前映射不是 initial hint。
- `HintOwnedByDifferentIdentity`：initial hint 存在，但它属于其他 identity；若 expected 同时在其他 COM，仍使用更
  具体的 `ExpectedAtDifferentPort`，并把 hint descriptor 作为冲突证据。
- `AmbiguousSnapshot`：fake/异常 backend 给出重复 stable identity 或重复可打开 COM mapping；保守视为 discovery
  execution failure，不任选其一。

`AdmittedDevice` 保存 request、被接纳 descriptor 和 admission snapshot evidence。其字段私有，只能由 selector
构造；`Harness`、fault session 和 lifecycle cycle 不再接收裸 port string。类型边界不是权限系统，但能让新命令在
编译期不能绕过 identity selector。

初始三个非 admitted 结果都发生在打开前，映射为 `completed/not_run/2`，并写入结构化 admission evidence。
discovery API error 或 ambiguous snapshot 表示 runner 无法建立安全事实，映射为 `failed/failed/1`。缺少参数是
contract error，不冒充设备不存在。

### `IdentityGuardedByteIoFactory`

`Harness` 用 `AdmittedDevice` 中的 descriptor 构造 serial transport，并把 production
`WindowsByteIoFactory` 包在 qualification-only guard 内。每次 115200/9600 native open attempt 固定执行：

1. 先检查 `ByteIoRequest::interruption`；取消或 deadline 已到时不 discovery、不 open。
2. discovery 一次，要求 expected identity 仍映射到即将交给 inner factory 的 descriptor COM；失败时不调用
   inner factory。
3. 调用 inner factory，以排他 Windows handle 打开并配置 descriptor；此步骤不发送协议 bytes。
4. handle 仍打开时再次 discovery，并再次要求相同 identity/COM mapping。
5. post-open discovery error 或 mismatch 时先显式 `ByteIo::close`，再返回结构化 `SerialError`；不得把该 stream
   返回给 serial transport，因此 handshake bytes 尚未发送。
6. 只有 post-open 通过才返回 stream。现有 `ObservedByteIoFactory` 位于 guard 外层，继续记录每个 baud attempt；
   guard 另向 run-owned evidence recorder 记录 pre/post snapshot、decision 和 inner open outcome。

115200 失败后若 Controller 尝试 9600，第二次 open 必须重新走完整 guard，不能复用第一次 snapshot。
optional label 或 VID/PID 变化只记录，不替代 identity/COM predicate。exclusive handle 建立后的拔线由冻结的 serial
disconnect 语义处理；具有系统级设备伪装权限的主体不在本地资格工具威胁模型内。

pre-open race、post-open mismatch 或 post-open discovery error 使 execution 失败，因为 runner 已进入 native-open
transaction，不能再声称单纯的 initial `not_run`。inner open 的 `AccessDenied`、`PortBusy`、`NotFound` 和 native
code 保持原分类。guard 不解析 error message。

## 命令行为

### 普通动作命令

`handshake`、`smoke`、`home-wake`、`faults`、`sequence` 和已授权 `amiibo` 各自只取得一个 initial
`AdmittedDevice`。所有 Harness/session 都从该 token 创建。任何命令在 admission rejected 后立即形成 `not_run`
result，不创建 Runtime、Controller 或 byte-I/O factory，也不发送设备动作。

`lifecycle` 对每个 cycle 用同一个 immutable request 重新 admission，并记录 cycle-specific descriptor；它不自动
追随 COM 变化。任一 cycle identity/port 不再匹配时停止后续 cycle，保存已完成前缀，并按是否已执行动作映射为
qualification failure 或 execution failure；不得切换到 hint 上的其他设备。

fault runner 的 occupied/cancel/deadline session 都接收同一 admitted target 或重新执行相同 request 的 selector。
PortBusy 场景可以有意让一个受 identity guard 的 owner 占用目标，但不能用任意 raw COM occupier 冒充目标。

### Hotplug

hotplug 的状态机为：

```text
Requested
  -> InitialAdmitted
  -> InitialConnected
  -> AwaitingExpectedAbsent
  -> InitialClosed
  -> AwaitingExpectedReturn
  -> ReboundAdmitted
  -> Reconnected
  -> Closed
```

任一步 runner/discovery/cleanup error 可进入 `Failed`；initial admission rejected 直接进入 `NotRun`。已经提示并
开始 hotplug 后，expected identity 未在 timeout 内消失或返回属于已执行机器判据失败，映射
`completed/failed/1`，前提是 cleanup 成功；discovery API error 或 cleanup failure 是 `failed/failed/1`。

等待 absent 只查 expected identity；旧 COM 被另一 identity 占用不算目标仍 present。等待 return 按 expected
identity 查找当前 descriptor，不要求它回到 initial COM。返回后以新 descriptor 构造 `AdmittedDevice`，并在
native open 前后再次 guard。旧 COM 上的另一设备只写入 conflict evidence，永远不打开。poll 使用 monotonic
deadline 和现有有界 interval；本任务不增加后台线程或自动无限重连。

### Diagnostic prelude

`smoke --wake-home` 必须同时提供显式 `--post-home-neutral-delay-ms`，范围固定为 `0..=60000`；未选择 Home 时
提供该参数属于 contract error。工具在 Home release 和 neutral 已终态后才开始该 configured delay。字段必须命名
`configured_post_home_neutral_delay_ms`，禁止出现 measured latency、recommended delay 或 device readiness 等
含义。

结果保存有序 `diagnostic_prelude_steps`，至少包含 Home down/up/neutral、configured wait，以及随后真实执行的
left-stick 或其他 report。若 Home 后还有 left-stick，不能声称 A 恰好在 Home 后该延迟发送。`--wake-home` 仍是
本次 run 的显式诊断选择，不成为 default smoke、connect hook、Controller capability 或 support matrix 字段。

## 证据投影

每个动作结果至少保存以下结构，名称在实现 review 前通过 fixture 固定：

```json
{
  "device_target": {
    "expected_stable_id": "USB\\VID_...",
    "initial_port_hint": "COM8"
  },
  "identity_admission": {
    "status": "admitted",
    "observed_expected": {},
    "observed_hint": {},
    "conflict": null
  },
  "identity_open_attempts": [
    {
      "baud": 115200,
      "pre_open": {"status": "matched", "descriptor": {}},
      "inner_open": "opened",
      "post_open": {"status": "matched", "descriptor": {}},
      "stream_returned": true
    }
  ],
  "capability_inference": "none"
}
```

rejected admission 同样保存 expected、hint、observed expected/hint descriptor 和 stable reason enum，不只保存
message。open guard 保存 `SerialErrorKind`、可选 native code 和诊断 message；机器判据只读取 enum/boolean。
descriptor JSON 继续区分 stable identity、COM、friendly name、manufacturer、VID 和 PID。

本任务的内存 projection/final JSON 不是 durable ledger。物理测试在 journal/manifest 任务完成前仍不得宣称形成
新的发布资格证据。后续 ledger 必须逐条持久化相同 admission/open/hotplug/prelude transition，而不是从 final
message 反向解析；checkpoint 再从磁盘 hash 和 evidence class 区分 `Observed`、`Attested`、`Unverified`、
`Failed` 与 `NotRun`。

## 并发、取消、deadline 与资源关闭

- selector 和 open guard 无全局状态。run-directory lease 只保护证据目录；Windows exclusive serial open 是当前
  device contention 的实际边界。本任务不宣称跨进程 device lease。
- discovery snapshot 是一次观察，不缓存为永久事实。pre/post-open 两次 guard 和 exclusive handle 共同缩小
  COM reassignment race；无法把系统级恶意设备替换变成密码学身份。
- `ByteIoRequest` cancellation/deadline 在每次 guard 前检查，inner factory 仍执行冻结检查。post-open 失败必须
  close 已取得的 stream；不得依赖 Drop 作为正常路径。
- hotplug poll 只使用 monotonic timeout，没有 detached task。Ctrl+C cooperative cleanup 仍由后续独立设计完成；
  强制终止、断电和 SetupAPI 调用自身无限阻塞不由本任务保证。
- `Harness`、Controller、Runtime 的显式 close 和 neutralization 仍服从已审查的 owner/cleanup 设计。identity
  failure 不覆盖更早 operation error，cleanup failure 仍使 qualification fail closed。

## 错误与退出状态

| 情况 | execution | qualification | exit |
| --- | --- | --- | ---: |
| initial identity + hint admitted，命令其余判据通过 | `completed` | 按命令判据 | 0/1/2 |
| expected absent / mapped elsewhere / hint 是其他 identity，未 open | `completed` | `not_run` | 2 |
| 缺少或空 identity/port 参数 | `failed` | `failed` | 1 |
| discovery API error 或 ambiguous snapshot | `failed` | `failed` | 1 |
| pre/post-open guard race 或 post-open discovery error | `failed` | `failed` | 1 |
| hotplug 开始后 expected 未按时消失/返回，cleanup 成功 | `completed` | `failed` | 1 |
| hotplug/fault/lifecycle cleanup 失败 | `failed` | `failed` | 1 |

initial rejected path 是 typed result，不通过 error message 匹配生成 `not_run`。所有路径仍提交 run-directory
artifact；artifact transaction 自身失败继续覆盖命令 exit，因为没有可信 final result。

## 平台、依赖、打包与性能

实现只使用 `easycon-serial` 已公开的 `SerialDiscovery`、`WindowsSerialDiscovery`、`SerialPortDescriptor`、
`ByteIoFactory` 和 `SerialError`。预计无需新增 crate。若实现发现 production API 缺口，先证明 qualification-only
adapter 无法表达，再单独审查 production 变更；不能把硬件工具类型上移公共 SDK。

本能力只在 Windows x64 hardware workspace 编译和运行，不进入根 workspace、C ABI、native bundle 或语言包。
`EasyCon/` 继续只读且不成为构建输入。

每个 native open 增加两次同步 SetupAPI discovery；hardware qualification 不是 runtime hot path，该成本可接受，
且不得计入 Controller software latency sample。hotplug polling 保持 250 ms interval 和调用方有界 timeout。任何
未来为性能删除 post-open guard 的改动都必须重开本设计。

## 实现拆分与测试

实现按三个可独立提交的任务推进，每项都先加入旧实现确定失败的最小回归：

1. **Typed selector 与 CLI admission**：fake snapshot 中 COM8 属于 `DEVICE\\OTHER`、expected 在 COM11；旧
   `find_port("COM8")` 会接纳错误 descriptor，新 selector 必须 `ExpectedAtDifferentPort` 且不构造 Harness。
   覆盖 expected absent、hint conflict、重复 mapping、缺参、discover 无需 identity、未授权 Amiibo 不打开和
   rejected result 的 `not_run/2`。
2. **Open-time guard 与 Harness ownership**：fake inner factory 记录 open/write/close。覆盖 pre-open mismatch 时
   inner open count 为零、post-open mismatch/error 时 open 后立即 close 且 write count 为零、success 返回 stream、
   115200/9600 每次重查、cancel/deadline 不 open、native error kind/code 不丢失。改造所有普通/fault/lifecycle
   Harness 路径，使编译期不存在动作命令从 raw port 构造 Harness 的入口。
3. **Hotplug rebind 与 diagnostic prelude**：snapshot 序列让 expected 从 COM8 消失、另一设备占 COM8、expected
   从 COM11 返回；新实现只能打开 COM11。覆盖 absent/return timeout、discovery error、cleanup failure、真实顺序
   evidence。旧 `smoke --wake-home` 缺少配置仍固定 sleep；新实现必须拒绝缺少 delay，fixture 验证边界值和字段
   不含 measured/readiness/capability 声明。

组合测试还必须证明：

- friendly name、VID/PID 或相同 COM 不能替代 stable identity；identity 大小写变化保守拒绝；
- initial rejected 路径不创建 Runtime/Controller、不开串口、不发送报告；
- hotplug 新 COM 被采用，旧 COM 上的另一设备不进入任何 open trace；
- identity evidence 在 handshake/action failure 后仍进入 final projection；
- Home、neutral、configured wait、left-stick 和 A 的记录顺序与真实调用顺序一致；
- 所有测试只使用 fake/synthetic transport 或未授权命令，不枚举/打开当前机器串口。

每个实现任务执行根 workspace 九项门禁与 `tests/hardware` 的 fmt/check/strict clippy/test，随后固定实现 SHA 做
独立 review；finding 修复形成新 SHA 后必须重新 review。

## 排除项、开放问题与重新打开规则

本设计明确不解决：板型/固件发现、支持 capability、Amiibo 容量、UART/USB/Switch 时序、操作员输入、真实
Home 行为、device-wide lock、journal/manifest/checkpoint 或 O-01/O-02/O-04。COM8 和 COM7 的历史观察保持原
分类，当前机器没有因此完成任何复测。

后续窄问题不阻塞本地实现：Windows 是否能提供更强的 opened-handle 到 device-instance 原子绑定，可在获得可
审计 API 和 failpoint 后另行加固；当前 pre/post discovery + exclusive handle 是不扩大支持面的保守边界。

以下变化必须先重开并 review 本设计：

- 允许只凭 COM、VID/PID、friendly name、manufacturer 或 baud 成功接纳设备；
- expected identity 不存在时自动选择“看起来像 CH32”的设备；
- hotplug 返回后继续打开旧 COM，而不是按 expected identity 的新 descriptor；
- 删除任一次 open 的 pre/post identity guard，或在 post-open guard 前发送 bytes；
- 把 Home prelude、配置 delay 或一次 action observation 变成 default connect/readiness/capability；
- 把本任务 final JSON 冒充 durable journal、support checkpoint 或物理资格结论。
