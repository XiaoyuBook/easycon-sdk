# 0010：冻结 Phase 2B 硬件资格工具与证据目标

- 状态：Accepted Target (`Hardware Unverified`)
- 日期：2026-07-20
- 审计基线：`15bd6c762323c34c73956daa2be096442d7724b7`
- 原始代码基线：`202b2610ffc836f21951e18f69fa98b46d6efc90`
- 实现状态：软件候选已按 [ADR-0011](0011-phase-2b-qualification-software-candidate-freeze.md) 冻结；硬件资格与
  完整 Phase 2 仍未完成。本 ADR 只冻结修复目标，不冻结硬件结论

## 背景

Phase 2A 已按 [ADR-0009](0009-phase-2a-freeze.md) 冻结为 `Hardware Unverified` Candidate。
Phase 2B 首轮交接在其上增加了独立、`publish = false` 且不属于根 workspace 的
`tests/hardware` CLI。6 个提交只建立资格工具，没有改变 production Controller/Serial 语义。

交接包和 14 项清单哈希均已复核，bundle 精确恢复到上述审计基线。现有证据只支持以下窄事实：

- COM8 的稳定 identity 为 `USB\VID_1A86&PID_FE0C\8&2D69411B&0&7`，VID/PID 为
  `1A86:FE0C`；板型和固件仍未知。
- COM8 三次发现一致，115200 首次握手成功；后续新会话出现过 hello mismatch/timeout，物理重插后恢复。
- 普通 A 和左摇杆 prelude 后的 A 没有用户可见反应；同一会话发送 Home、完成中立、等待配置的 3 秒，
  再保持 A 500 ms 后，用户确认 A 生效。该流程只是一次设备限定观察。
- COM7 是 identity `USB\VID_1A86&PID_8040\20191234` 的另一设备，只提供诊断观察，不能归入 COM8。
- 10,000 样本软件延迟只覆盖 memory transport。它不测 UART、CH32 firmware、USB HID 或 Switch。

审计确认旧 CLI 会把未满足判据或待操作员确认的结果写成 `passed`，失败时丢失设备身份和部分进度，
忽略 `CloseOutcome::Failed`，只按 COM 名选择设备，并允许覆盖原始证据。Amiibo 和 partial-write 计时也缺少
发布资格所需的证据边界。Ctrl+C 尚未进入取消、中立化和关闭协议。

O-01、O-02、O-04 全部保持开放。现有原始文件和交接叙述不得因本 ADR 重写或升级为新结论。

## 决策

### 1. 工具边界

`tests/hardware` 继续是独立、不可发布的 qualification workspace。它可以依赖冻结的 production crates，
但不得进入根 workspace、release feature、native bundle 或语言 SDK。production Controller 不感知 Home prelude、
操作员提示、测试 run ID 或硬件矩阵。

工具只执行调用者明确选择的本地设备测试。它不烧录固件、不推断板型/固件、不建立 Host、服务进程、
JSON-RPC、WebSocket 或网络控制面，也不修改 `EasyCon/`。

### 2. 两套终态与退出码

每次运行必须分别保存：

1. `execution_status`：`completed`、`failed` 或 `cancelled`，表示工具和协议是否完成。
2. `qualification_status`：`passed`、`failed`、`unverified` 或 `not_run`，表示该命令声明的资格判据。

状态规则固定如下：

| 条件 | execution | qualification | 退出码 |
| --- | --- | --- | ---: |
| 全部机器判据和必需操作员观察通过 | `completed` | `passed` | 0 |
| 测试完整执行，但机器判据或明确操作员观察未通过 | `completed` | `failed` | 1 |
| runner、协议、artifact 或 cleanup 无法完成 | `failed` | `failed` | 1 |
| 初始 expected identity 不存在或与 port hint 不匹配，且未打开设备 | `completed` | `not_run` | 2 |
| 动作已执行但缺少必需人工/仪器证据 | `completed` | `unverified` | 2 |
| 因授权、能力或外部前置条件未执行 | `completed` | `not_run` | 2 |
| 操作员协作中断且 cleanup 已结算 | `cancelled` | `unverified` | 130 |

`passed` 只适用于当前命令和精确设备，不表示板卡受支持或 O 项关闭。布尔字段不能只作为旁路信息；
`faults`、`hotplug`、`lifecycle`、`sequence` 的任一必需检查为 false 时，资格必须失败。`smoke`、
`home-wake` 没有记录必需操作员结果时必须为 `unverified`。未授权的 Amiibo 必须为 `not_run`。

### 3. 稳定设备身份

任何打开端口或发送字节的命令都必须同时取得调用者提供的 expected stable identity。COM 名只是本次枚举的
可变 hint。打开前必须从系统发现结果证明 `port + stable identity` 指向同一 descriptor，否则不得动作。

hotplug 重连必须按 stable identity 重新发现当前 COM assignment；旧 COM 被另一设备占用时不得打开它。
结果同时记录 expected identity、实际 identity、初始/重连 port 和系统提供的 VID/PID。VID/PID、friendly name
或端口名都不能替代 stable identity，也不能用于推断板型、固件或 capability。

初始 identity guard 拒绝动作属于安全的 `not_run`，不是设备测试失败。已经开始 hotplug 后，目标 identity 未按
约定回归属于 `completed/failed` 的机器判据；系统 discovery 自身报错或 cleanup 无法完成才属于 execution
failure。

### 4. 运行 ledger 与结构化失败

命令从开始即维护 append-only journal，并以它形成内存 projection。journal 在打开设备前使用 `create_new`
持久化 run/tool/identity 元数据。以下 durability boundary 必须 `flush`/`sync` 后才能继续外部动作：身份 guard
通过、Amiibo 授权与 payload intent、每个已知 Amiibo chunk 进度、operator response、取消请求和 cleanup
终态。普通动作可在每个 action group 后同步；高频 sequence telemetry 写入独立临时流，不能为每个 report
强制磁盘 sync 而改变时序，但崩溃后必须明确标为 incomplete。

ledger/projection 至少包含：

- run ID、UTC 开始/结束、单调 elapsed、工具版本和构建 provenance；
- 归一化参数，设备 expected/observed identity，握手 baud attempts；
- 每个 action/operation 的 stable ID、状态、结构化 error/cancellation、时间戳和已知进度；
- 操作员或仪器 observation，以及它绑定的 action/run/device；
- telemetry、机器判据、限制和未验证边界；
- Controller neutralization/close、Runtime `CloseOutcome` 和最终 registry counts；
- 本次辅助 raw payload 的相对名称、字节数和 SHA-256。

构建 provenance 由构建时元数据生成，至少嵌入完整 commit、source tree、tracked dirty state 和独立 workspace
`Cargo.lock` SHA-256；运行时再记录 executable SHA-256。运行时 checkout 的 `git rev-parse` 只可作诊断，
不能覆盖 build provenance。commit/tree 未知，或 tracked dirty 且没有可复核 tree digest 时，本次结果不得
为 `passed`。旧 binary 在新 checkout 中运行时仍以 binary 内嵌 provenance 为准。

错误不得把 ledger 压缩成一条字符串。operation error 至少保存 stable domain/code、message、native code 是否
存在和 cancellation reason。错误 message 只作诊断，不作为机器判据。

所有拥有 Harness 的命令必须使用单一 completion 路径：先结算命令结果，再显式 neutralize/close Controller，
再调用 Runtime close，最后固定证据。只有 `CloseOutcome::Closed` 且 operation/resource/task counts 全零才算
cleanup 通过。`CloseOutcome::Failed` 必须保存 phase、diagnostic、resource/task ID 和 counts，并令资格失败。
Drop 只作为 panic/意外返回的最后防线，不能替代可审计的显式 close 结果。

### 5. 取消、deadline 与 operator interrupt

operation deadline、protocol timeout、wait timeout 和 operator interrupt 保持不同来源。wait timeout 不得伪造
operation cancel。长 wait 和 sleep 必须分段观察一个进程级 interrupt token；console handler 只设置 token，
不执行 I/O、分配、join 或回调。

观察到 Ctrl+C 后，runner 停止接纳后续动作，请求当前 operation cancel，等待其唯一终态和中立化，再按正常
顺序 close Controller/Runtime 并写 cancelled artifact。强制终止、断电和设备自身失电不可能由进程内 cleanup
保证，必须明确列为排除项。

操作员输入由单 owner、可注入的 `OperatorPort` 管理。它提供有界 poll/read、response deadline 和同一个
interrupt token，不得创建 detached stdin reader；若平台实现使用辅助线程，该线程必须由 runner 所有并在
cleanup 前可唤醒、join。回答 `no` 是 `completed/failed/1`；EOF 或 response timeout 是
`completed/unverified/2`；等待回答时 Ctrl+C 是 `cancelled/unverified/130`。三条路径都先完成当前 action 的
release/neutral 和资源 close，再提交终态 artifact。

fault 场景彼此隔离。port-occupied、operation cancel 和 immediate deadline 使用独立 session；deadline case
开始前必须释放占用者，并断言 deadline 对应的 stable cancellation reason，不能让 PortBusy 竞态冒充通过。

### 6. 操作员观察

transport acceptance 与 Switch 可见结果是两项证据。需要可见判断的动作必须在发送前显示稳定 action marker，
在释放并中立后取得绑定当前 run/device/action 的明确观察。EOF、无响应、模糊回答和 blanket `--yes` 均不能
成为通过。批量 full smoke 必须逐项记录按钮、HAT 和摇杆观察；不能在无人确认时快速发送后整体标记通过。

交接聊天中的观察可以作为 hash-protected handoff attestation 保存，但 checkpoint 必须标明其来源是
`handoff_attestation`，不能伪装成 raw JSON 内字段。未来运行应把操作员回答直接写入本次 ledger。

### 7. 实验 prelude 与通用语义隔离

Home prelude 只允许作为显式选择、绑定 stable identity 的 diagnostic step。等待时间必须由有界参数提供，输出
字段命名为 configured post-neutral delay；不能命名成测得的 Home-to-A latency。若随后还有 left-stick 或其他
report，ledger 按真实顺序记录，不能继续声称 A 在固定 3 秒后发送。

该观察只能进入 exact-device checkpoint 的 `Observed/Unverified` 区域。它不得成为默认 smoke、Controller
connect、通用 readiness state 或 capability，也不得改变 30 ms、单写者、lease、取消和中立化语义。

### 8. Amiibo 写入

O-02 关闭前默认不写。实机写入同时要求：

- expected stable identity；
- 本次 run 的显式 write authorization；
- 明确 slot 和调用者确认该 slot 可丢弃；
- 外部确认的 slot count、maximum length 及其 provenance；
- 实际 payload length 和调用者预期 SHA-256 与工具重算 SHA-256 一致。

durable journal 必须在首个写字节前写入并同步 `write_attempted`、slot、实际 length/hash 和授权条件。每个
save chunk 的 intent 在写前同步，已知 accepted/failure 进度在返回后再次同步；save/select/cleanup 也各自形成
durable record。这样异常进程终止至少留下可恢复的 destructive intent 和最后已知进度。每个 save chunk 的已知
进度、save operation、select operation、cleanup 和失败点都要保留。save 成功而 select 失败时仍明确记录已经
写入；调用者声明的 slot count/maximum length 只能标为 declared limit，不能由一次成功短写升级为观察容量。

### 9. Telemetry 边界

一个 logical report 的 `write_entered_ns` 是第一段 partial write 进入 transport 的时间；
`transport_accepted_ns` 是最后一段完整接受返回的时间。中间分段不得重置起点。CSV 保存 write sequence、
operation、完整阶段时间和 partial count；任何时间逆序必须成为显式失败，不能用 saturating subtraction 隐藏。

OS write acceptance 仍不是 UART complete frame。UART 理论值必须标记 `measured = false`；没有 analyzer 或
可审计 firmware trace 时，USB HID 和物理顺序继续为 `unverified`。本规则不改变已复核的 Phase 2A
memory-transport CSV。

native I/O detail 由 `tests/hardware` 自有的 ByteIo factory/I/O decorator 在 production transport 的 lossy
错误映射前观察并写入 ledger，保存 `SerialErrorKind` 和可选 OS code；operation 层另存冻结的 stable
domain/code。资格工具不得解析 error message 还原 native code，也不得为此修改 Phase 2A production 错误契约。

### 10. 不可覆盖的 artifact transaction

每次运行使用唯一 run directory。开始物理动作前以 `create_new` 建立 in-progress journal；若目标 run ID、最终
JSON、CSV 或 manifest 已存在，命令必须在打开设备前失败。完成时在同一目录写临时文件、flush/sync 后原子
rename，并以完成 marker 和 SHA-256 manifest 收口。并发 runner 不能共享 run directory。

final JSON 只内嵌辅助 payload 的 hash。外部 manifest 再哈希 final JSON、journal 和 CSV；manifest 本身与
completion marker 不进入该 manifest 的自哈希集合。checkpoint 或交接包可以在更外层对 manifest 再哈希。
测试必须从磁盘重算 manifest，而不是信任 JSON 内复制值。

失败、取消和 cleanup failure 也必须提交 ledger。意外进程终止留下 in-progress journal，由后续 checkpoint
标为 incomplete，不能静默删除或覆盖。raw artifact 继续 ignored；tracked checkpoint 只引用相对 artifact
名称、hash、证据分类和结论，不写开发机绝对路径。

### 11. Checkpoint 与支持矩阵

在 O-01/O-02/O-04 关闭前，只创建 `Hardware Unverified` checkpoint，不创建受支持设备行。checkpoint 必须
分别列出 `Observed`、`Attested`、`Unverified`、`Failed` 和 `NotRun`，并引用原始 SHA-256 或 handoff hash。

只有板型、固件、identity、baud、全动作、fault/hotplug、100-cycle、物理 10,000-step、Amiibo 和规定的
UART/USB 时序证据全部满足 ADR-0008，且固定实现和证据经独立 review 后，才能把 exact device/firmware
提升到 `hardware/matrix.yaml` 支持行并冻结完整 Phase 2。

## 测试与门禁

实现前先增加能在旧基线确定失败的无硬件回归，至少覆盖：

1. false qualification predicate、待操作员确认和未授权写入不能成为 `passed`。
2. handshake/action 中途失败仍保留 identity、attempts、partial ledger 和 cleanup outcome。
3. Runtime `CloseOutcome::Failed` 使运行失败且结构化 report 完整。
4. 目标回到新 COM、另一设备占旧 COM 时，只能选择 expected stable identity。
5. operator interrupt 在非中立动作后触发 cancel、neutral、close、cancelled artifact 的顺序。
6. operator response 的 no、EOF/timeout、Ctrl+C 分别得到规定状态，且输入 owner 无 detached task。
7. Amiibo 各 write-ahead/chunk/save/select failpoint 后，durable journal 仍可恢复 intent、实际 length/hash 和进度。
8. 3+5 byte partial write 保留第一次 entry 和最后一次 acceptance，并捕获注入的 native OS code。
9. 复用 run directory 不覆盖 sentinel；manifest 可从磁盘重算且不存在 JSON/manifest 自引用。
10. 旧 binary 在新 checkout、unknown/dirty provenance 时不能产生 `passed`。
11. deadline case 不受 port-occupied session 污染。
12. Home diagnostic delay 是显式配置，输出不冒充测得 latency 或通用 capability。

每个独立修复提交前执行根 workspace 九项门禁和 `tests/hardware` 的 fmt/check/strict clippy/test。任何修复都会
形成新审查基线，必须重新做独立 review。物理命令只在 exact expected identity 和操作员/仪器可用时运行；
无硬件测试通过不能关闭 O 项。

## 排除项

- 不改变 production Controller/Runtime/Serial 的冻结公共语义来适配一次硬件观察。
- 不实现固件、烧录、字节码、Host、服务进程、网络控制、UI 或远程操作员。
- 不把 COM7 观察、COM8 Home prelude、memory transport 或理论 UART 时间泛化为支持能力。
- 不在本机缺少目标设备时运行物理动作，也不把历史日志描述为当前机器复测。

## 重新打开规则

以下变化必须先修改并 review 本 ADR：

- 合并 execution/qualification 状态或允许未验证结果退出 0；
- 移除 stable identity 写前校验、Amiibo 写前授权/hash 或不可覆盖 artifact；
- 让 Ctrl+C 绕过 cooperative cleanup，或把 Drop 结果当作资格 close evidence；
- 改变 telemetry 的 first-entry/final-acceptance 边界；
- 把设备限定 diagnostic prelude 上移到 production Controller/capability；
- 在 O-01/O-02/O-04 未关闭时创建支持矩阵行或冻结完整 Phase 2。

保持本目标的普通实现选择和 bug fix 不重新打开 ADR，但必须通过规定回归、完整门禁和新基线独立 review。

## 关联

- [ADR-0008：冻结 Phase 2 Controller/Serial 开发目标](0008-phase-2-controller-target.md)
- [ADR-0009：冻结 Phase 2A Controller/Serial Candidate 基线](0009-phase-2a-freeze.md)
- [测试策略](../architecture/testing-strategy.md)
- [实施路线](../architecture/repository-roadmap.md)
