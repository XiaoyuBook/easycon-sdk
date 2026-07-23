# Phase 2B faults 协议证据修订设计

- 状态：**Proposed / Not Implemented**
- 日期：2026-07-23
- 审查基线 HEAD：`ab39ea4f8e3a5f24fddf3bbe20d57e389efea445`
- 审查基线 tree：`262ff1c60140617c9fae127d01707a29b1e05184`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- candidate 边界：[ADR-0011](../decisions/0011-phase-2b-qualification-software-candidate-freeze.md)
- 相邻设计：[faults runner 所有权](phase2b-fault-runner.md)、
  [telemetry 与资格投影](phase2b-telemetry-qualification-projection.md)、
  [checkpoint 软件收口](phase2b-checkpoint-software-closeout.md)

## 范围、非目标与冻结边界

本设计只修订 `tests/hardware` 中 `faults` 命令的 handshake 协议证据。交付分为一项 mandatory contract
bugfix 和一项随本修复纳入的 qualification-only diagnostic；两者都只改变未来资格工具的内部观测和 artifact
投影，不改变 production 行为。

本设计不实现代码、不运行硬件，不修改 `easycon-serial`、`easycon-controller`、`easycon-runtime`、
`ControllerTransport`、`TransportError` 或任何公共 API；不修改 Cargo、根 schema/conformance、现有 ADR 决策正文、
workflow、roadmap 里程碑或支持矩阵。它也不授权串口枚举/open/write、独立 handshake、拔插、lifecycle、sequence、
push、PR 或 Ruleset 操作。

冻结事实如下：

1. ADR-0010 要求 ledger/projection 保存 handshake baud attempts，并要求 handshake 中途失败仍保留 attempts、partial
   ledger 和 cleanup；保持目标的普通 bugfix 可以不重开 ADR-0010，但实现后必须回归、完整门禁并以新基线独立审查。
2. ADR-0011 规定 `tests/hardware` executable、artifact schema 或 telemetry 变化会重开 software candidate。
3. 当前 `Telemetry` 已有 `actual_baud` 和 ordered `HandshakeAttempt`；`FaultCloseEvidence`、role close 以及
   `capture_partial_harness_evidence` 没有把它们投影到 faults 结果。
4. production serial transport 读取并验证一个 reply byte；mismatch 只返回通用 `Protocol` error，实际 byte 不保留。
5. 已复核的窄诊断事实是：一个 fresh occupier session 在 115200 成功并 clean close；第二个独立 cancel session
   经过 115200 到 9600 的尝试后以 handshake mismatch 结束。该事实只约束当时的设备、host 和 run，不能证明固件
   缺陷、根因、设备支持或可重复性。
6. 当前 checkpoint 仍为 `Hardware Unverified`，O-01、O-02、O-04 开放，支持矩阵为空。pairing 只是
   user-attested diagnostic provenance；USB identity 不证明 MCU、板型或固件。旧 immutable artifact 不得回填、
   重写或改变 hash，本地 raw artifact 也不得转成 tracked evidence。

因此本文不声明 Phase 2B 完成、任何设备受支持，也不把 mismatch 归因于固件、host、串口层或 session 模型。

## 两项交付

### A. Mandatory contract bugfix

每个 faults Harness role：`occupier`、`occupied_probe`、`cancel`、`deadline`，都必须在完整 close 和
partial-failure 路径投影：

- `<role>_actual_baud`；
- `<role>_handshake_attempts`，保持实际开始顺序且不丢弃失败 attempt。

这是 ADR-0010 已有 ledger/projection 和 partial evidence 要求的缺口修复，不改变 faults 的四个独立 Harness、
三项机器判据、cleanup 顺序或 qualification 状态规则。

### B. Included qualification-only diagnostic

每个 attempt 同时保存 `expected_reply_byte` 与 `observed_reply_byte`。expected byte 来自本次
`HandshakeRequest`；只有 qualification decorator 成功读取恰好一个 reply byte 时 observed 才是 `0..=255`，未成功
读取时必须为 `null`。

B 本轮必须与 A 一起纳入：若只补 baud/attempt，下一次同类 mismatch 仍只会得到通用 diagnostic error，无法区分
设备实际返回的 byte，硬件复测对 session、线路、协议或设备侧问题的诊断价值仍然不足。B 不得通过修改 production
transport、`TransportError`、`ControllerTransport` 或公共 API 实现，也不得把 byte 拼进 error message；byte 必须是
qualification artifact 中的独立 typed 字段。

## Faults protocol evidence v1

最终 faults result 的现有 document schema v2 保持不变；在 result 内增加一个独立子合同。字段沿用仓库已有的
snake_case 和 `<role>_...` 顶层命名，因为 faults 已使用 `occupier_cleanup`、
`occupied_probe_identity_open_attempts` 等角色前缀。不得另建与这些字段重复的嵌套 role 别名。

子合同精确包含：

```json
{
  "faults_protocol_evidence_schema_version": 1,
  "occupier_actual_baud": 115200,
  "occupier_handshake_attempts": [
    {
      "baud": 115200,
      "outcome": "succeeded",
      "error": null,
      "expected_reply_byte": 128,
      "observed_reply_byte": 128
    }
  ],
  "occupied_probe_actual_baud": null,
  "occupied_probe_handshake_attempts": [],
  "cancel_actual_baud": null,
  "cancel_handshake_attempts": [],
  "deadline_actual_baud": null,
  "deadline_handshake_attempts": []
}
```

示例只展示字段和类型，不是 passed fixture，也不表示某个 run 的真实结果。四个 role 的字段始终存在；其约束如下：

| 字段 | v1 约束 |
| --- | --- |
| `faults_protocol_evidence_schema_version` | 必须精确为整数 `1`。 |
| `<role>_actual_baud` | `null` 或 `115200`/`9600`；非空时必须等于该 role 唯一 `succeeded` attempt 的 baud。 |
| `<role>_handshake_attempts` | 数组，按 `ObservedTransport::handshake` 开始顺序追加，不按完成时间排序。 |

每个 attempt object 只允许以下精确字段：

| 字段 | 类型与含义 |
| --- | --- |
| `baud` | 整数 `115200` 或 `9600`。 |
| `outcome` | `succeeded`、`mismatch`、`timeout`、`cancelled`、`disconnected`、`io_error`、`protocol_error` 或 `contradiction`。 |
| `error` | 现有 `TransportError` diagnostic 的非空字符串，成功时为 `null`；机器判据不得解析此字段。contradiction 有 outer error 时保留它，否则使用不含 reply byte 的稳定 observer 诊断文本。 |
| `expected_reply_byte` | 必需的整数 `0..=255`；attempt 一开始即可从 `HandshakeRequest` 取得，不能为 `null`。 |
| `observed_reply_byte` | 整数 `0..=255` 或 `null`；只有一次成功的单字节 `HandshakeRead` 才可非空。 |

outcome 映射固定为：

- outer handshake 成功且 observed 等于 expected：`succeeded`；
- outer `Protocol` failure 且成功读取的 observed 与 expected 不同：`mismatch`；
- `TransportErrorKind::Timeout`、`Cancelled`、`Disconnected`、`Io` 分别为 `timeout`、`cancelled`、
  `disconnected`、`io_error`；
- `Protocol` failure 没有一份可关联的不同 reply byte：`protocol_error`；
- handshake 出现不可能的 error kind，或 outer result、attempt owner 与 byte observation 互相矛盾：
  `contradiction`，并使资格失败。

`succeeded` 必须满足 `error = null` 且 observed 等于 expected；`mismatch` 必须有非空 error，且 observed 与
expected 不同；timeout/cancel/disconnect/I/O/普通 protocol failure 没有成功 reply read 时 observed 必须为
`null`。任何 byte 都只按无符号十进制 JSON number 保存；不得生成十六进制字符串、字符解释或固件含义推断。

### Command-level 全出口 result 构造

九字段不能只由 `FaultRun::new` 初始化，因为 device admission、interrupt handler、command dispatch 和通用 `Err`
都可能在创建 `FaultRun` 前退出。未来实现必须在识别到 `faults` 命令、进入 admission 或 `FaultRun` 前创建唯一的
command-level `faults_result_base`。base 至少包含：

- `command = "faults"`；
- `faults_protocol_evidence_schema_version = 1`；
- 四个 `<role>_actual_baud = null` 和四个 `<role>_handshake_attempts = []`；
- `scenarios.port_occupied/cancel/deadline.status = "not_run"`；
- `resources.occupier/occupied_probe/cancel/deadline.created = false`。

admission evidence、`FaultRun` 和 cleanup 只能取得并单调填充同一 base，不能另建 result、删除字段、把
`created = true` 重置为 false，或清空已经开始的 attempt。`FaultRun` 应改为接收该 base 的 owner，而不是成为
九字段的唯一构造点。

faults dispatch 得到任何 `Ok`/`Err` 后，必须先经过 command-specific
`normalize_faults_execution(base, execution)`，再生成 operation/cleanup journal payload 并调用 `finalize_result`。
normalizer 的职责是：

1. 对 `Ok(result)` 合并 admission/FaultRun evidence，并 exact validate base、resources/scenarios 和协议子合同；
   只可为确认未创建的 role 保留空值，不能用默认空数组修补已创建 role 的缺失或矛盾 evidence。
2. 对通用 `Err(message)` 从原 base 生成结构化 faults result，保存原 message，并写入非空稳定
   `execution_error.stage`；随后仍以 `Ok(structured_result)` 进入通用 finalizer。因此 faults final document 的
   `result` 永远存在，不再出现 `result = None`。
3. 已有首个 execution error 不得被 protocol normalization 或 cleanup error 覆盖；后者追加为独立诊断。base/validator
   也不得把 execution/qualification/exit triplet 从原路径语义改成另一种结果。

全出口映射固定如下：

| 出口 | 稳定 stage / 终态 |
| --- | --- |
| `device_target_request` 参数失败 | `device_target_request`；`failed/failed/1`，保留原参数诊断，不伪装 `not_run`。 |
| admission `Rejected` | 无 execution error；保持 `completed/not_run/2` 和原 admission reason。 |
| admission `Ambiguous` | `identity_admission`；`failed/failed/1`。 |
| discovery error | `identity_admission_discovery`；`failed/failed/1`，保留 structured serial error。 |
| interrupt handler install failure | `interrupt_handler_install`；`failed/failed/1`。 |
| dispatch/admission/runner precheck interrupt | 保留 `command_dispatch_interrupt`、`identity_admission_interrupt` 或 `runner_interrupt`；`cancelled/unverified/130`。 |
| faults precheck interrupt | 保留 `faults_interrupt`；`cancelled/unverified/130`。 |
| admitted runner `Err` | `admitted_runner_failure`；`failed/failed/1`，保留 runner message。 |
| 其他 faults 通用 `Err` | fallback stage `faults_command_error`；`failed/failed/1`，不得产生 `result = None`。 |

以上所有 pre-Harness 出口都保持四个 role 未创建、actual baud 为 `null`、attempts 为空；这表示未开始，不是伪造
cleanup 或 handshake evidence。cleanup/qualification validator 必须把上表枚举的零 role 出口识别为精确的
no-resource layout：不要求伪 cleanup，也不能把 Rejected 或 interrupt 改成 execution failure；没有匹配 admission、
interrupt 或 execution-error 语义的未知零 role `Ok` 仍须 fail closed，不能因此通过 faults qualification。

### Role 与 fallback 规则

1. `resources.<role>.created = false` 时，该 role 必须是 `actual_baud = null` 和空 attempts；不得伪造 attempt。
2. role 已创建但在 outer handshake 前发生 admission、immediate deadline 或其他失败时，仍允许
   `null`/空数组，但 failure stage 必须证明尚未开始 handshake。
3. attempt 一旦开始必须恰好 finalization 一次。失败、取消、deadline、disconnect 和 close 都不得删除已开始
   attempt；未终结 attempt 在 final projection 中属于 `contradiction`，不能伪装成未发生。
4. attempts 只能是自动顺序 `[115200]` 或 `[115200, 9600]` 的前缀。9600 只可在 115200 已终结为非成功且
   Controller 仍允许 fallback 后开始；operation cancel 或总 deadline 已生效时不得追加 fallback。
5. 成功 attempt 最多一个，必须是数组最后一个；成功后不得追加 attempt。没有成功 attempt 时 actual baud 必须为
   `null`；有成功 attempt 时 actual baud 必须与它一致，即使后续 action 或 cleanup 失败也不得清除。
6. `FaultCloseEvidence` 是 close 后的权威 role snapshot；partial-failure capture 必须先保留已取得的 attempt，随后
   recovery/close 只能完成同一 attempt 或追加合法 fallback，不能重排、覆盖或跨 role 合并。

command-level base、`FaultRun` 和每个 Harness 内的 typed recorder 共同构成同一 run-owned projection；不得在
`FaultRun` 内重新初始化协议字段。同一 recorder 是既有 append-only journal transition 与 final result projection
的唯一来源：attempt start/finalization 作为结构化 transition 保存，final JSON 不得从 diagnostic message 或 raw
log 重建。已终结 attempt 必须在进入 fallback、下一 role 或 cleanup 前进入既有 action-group durability boundary；
不新增单独 raw protocol artifact。进程在 finalization 前异常终止时保留 in-progress journal，由 checkpoint 标为
incomplete，不能合成 v1 completed evidence。

## Run-local reply correlation

`ObservedTransport` 和 `ObservedByteIo` 继续共享每个 Harness 自己的 `Arc<Mutex<Telemetry>>`，不增加 global
recorder。实现应在 qualification workspace 内增加单调递增、run-local 的私有 attempt ID：

1. `ObservedTransport::handshake` 在调用 inner transport 前持锁创建 in-flight attempt，记录 ID、baud 和 expected
   byte，然后释放锁再执行 I/O。
2. `ObservedByteIoFactory::open` 以当前 baud 绑定唯一 in-flight attempt，并把 ID 存入本次返回的
   `ObservedByteIo`。fallback 会创建新 stream 和新 ID，旧 stream 的观察不能落到新 attempt。
3. `ObservedByteIo::read` 先调用 inner；仅当 operation 是 `HandshakeRead` 且返回 `Ok(1)` 时，才在返回后用绑定 ID
   写入 `buffer[0]`。`Err`、`Ok(0)` 或非单字节进展都不产生 observed byte。
4. outer handshake 返回后，`ObservedTransport` 以同一 ID 和 typed `TransportErrorKind` finalization；不得解析
   diagnostic message 来判断 mismatch、取消或 native cause。

上述时序保持现有可实现边界：`ObservedTransport::handshake` 先创建 attempt，随后 inner
`SerialControllerTransport::handshake` 才执行 factory open。因此 open failure 也属于已经开始且必须保留的 attempt，
不能因没有返回 `ObservedByteIo` 而退化为空数组。

mutex 只保护 attempt 状态转换；任何 open/read/close 或 inner transport 调用期间都不得持锁。Controller 的单写者
lane 保证一个 Harness 正常情况下只有一个 handshake in flight；recorder 仍必须 fail closed 检出无 active attempt、
baud/ID 不匹配、重复 reply、跨 attempt reply、重复 finalization、成功却缺少匹配 byte、或 mismatch 却没有不同
byte。上述情况输出 `contradiction`，不能修改 inner 返回值来“修正”生产行为。

取消、deadline 与 close 使用两阶段 recovery，而不是要求 operation 必须在 close 前先终态：

1. 若 active operation 尚未终态，先 request cancel 并 bounded settle；若已终态，再按正常路径 consuming close。
2. 若 bounded settle 失败或 operation 仍非终态，owner 必须继续执行 consuming Controller close 作为最后 recovery。
   resource cancellation 应唤醒被 handshake read/open 阻塞的 lane，close join worker；不得因 operation 未终态跳过 close。
3. worker 中的 inner handshake 返回后，`ObservedTransport` 必须把同一 attempt 恰好 finalization 一次。close 返回后再
   固定 post-close operation snapshot、protocol evidence、Controller/Runtime cleanup 和 registry counts。
4. 若 close 或 worker 无法确认收敛，结果 fail closed，并保留首个 execution error、settle error、已知 attempt 和
   cleanup diagnostic；不得无限等待、丢 evidence、伪造 operation 终态或重复 finalization。

immediate deadline 若在 outer handshake 前被 Controller 接受为终态，则 attempts 为空；in-flight deadline/cancel
必须 finalization 已开始的 attempt，observed 在没有成功 read 时为 `null`。cleanup failure 不覆盖已终结的 protocol
evidence。

所有竞态测试使用 scripted fake、barrier/channel、可控 cancellation 和 VirtualClock/脚本时钟；不得用随机 sleep
制造 read、cancel、deadline 或 close 顺序。

## Schema、fixture 与 legacy 策略

现有 `tests/hardware/fixtures/phase2b-qualification-projection-v1.json` 必须保持 byte-for-byte 不变，不原地增加
faults 字段或改版本。实现应新增独立 fixture：
`tests/hardware/fixtures/phase2b-fault-protocol-evidence-v1.json`。

新 fixture 只含 synthetic role/attempt case、outcome 映射、fallback、optional byte、contradiction 和 legacy
分类，不含真实 identity、host、reply 观察或硬件结论。hardware workspace conformance 必须把 fixture case 送入
真实 recorder/serializer 和 exact validator，不能复制一套测试专用映射。

exact validator 必须同时检查：九个子合同字段、attempt exact keys、role created 状态、actual/success 一致性、
attempt 顺序、成功后无追加、outcome/error/byte 组合和 partial-failure layout。每一个未来 faults final result，无论
passed、failed、cancelled、unverified 或 not-run，都必须携带可验证的 v1 子合同；合同缺失或 contradiction 不能通过
资格判据。

### Checkpoint faults-only proposed supersession

当前 [checkpoint 软件收口设计](phase2b-checkpoint-software-closeout.md) 和现有 binary 仍把 trusted
`completed/passed/0` 映射为 `Observed`。本文状态是 `Proposed / Not Implemented`；在新 faults v1 software
candidate 完成实现、独立 review 和 refreeze 前，该既有映射仍然有效，本文不声称旧 passed faults 已经降级。

新 faults v1 合同随新 candidate/refreeze 生效时，本文对 checkpoint 设计提出一项仅限 `command = "faults"` 的
supersession：

- 由 refreeze provenance fence 明确认定为 pre-v1 的旧 trusted passed faults，缺少 v1 子合同时投影到
  `Unverified`，并保存稳定机器 token `legacy_protocol_evidence_missing`；未知或 v1 生效后的 provenance 缺失/畸形
  子合同必须是 `Failed`，不能借 legacy 分支降级逃逸。
- 旧 failed faults 继续投影为 `Failed`；缺少新字段不能改写其失败事实或升级分类。
- 其他非 faults 的 trusted passed run 完全不受影响，继续按 checkpoint 设计投影为 `Observed`。
- new v1 faults passed 只有在子合同 exact valid、provenance trusted、磁盘/manifest/snapshot 和其余既有条件全部通过时，
  才能投影为 `Observed`。

新 checkpoint record 必须保留旧 artifact 的原始 `document_outcome` 三元组、hash 和完整 provenance；只改变新
checkpoint binary 生成的 `evidence_class`，并以 `validation_reason = "legacy_protocol_evidence_missing"` 解释该
faults-only projection。不得改写原 final、journal、manifest、completion、provenance 或 hash，也不得从聊天、本地
raw artifact 或后来的硬件观察回填旧 run。

checkpoint exact projection validator 与 integration regression 必须共同覆盖：旧 trusted passed faults 的
`Unverified` + token、旧 failed faults 的 `Failed`、非 faults passed 的不变映射，以及 new v1 passed 在其余条件
满足后的 `Observed`；token 出现在其他 command、未知/new provenance 或已有 v1 合同上都必须 fail closed。

该 proposed supersession 是 ADR-0010 既有 faults evidence 完整性要求内的 bugfix，不要求修改 ADR-0010 或根
behavior/schema/conformance；但 executable、artifact/checkpoint projection 和 telemetry 会变化，因此 ADR-0011
software candidate 仍必须重开、独立 review 并 refreeze。

## 预计实现清单

以下只是未来实现范围，不表示这些文件已修改：

| 文件/类型 | 预计变更 |
| --- | --- |
| `tests/hardware/src/main.rs` | 在 admission 前建立 faults command-level base，对全部 Ok/Err 出口做 monotonic normalizer；扩展内部 `HandshakeAttempt`/`Telemetry` 和两层 observer correlation；新增 faults serializer 与 exact validator，同时保留非 faults projection。 |
| `tests/hardware/src/faults.rs` | 让 `FaultRun` 接收 command base、`FaultHarness` 提供 protocol snapshot；扩展 `FaultCloseEvidence`、partial capture、`close_role`，并实现 bounded settle 后可由 consuming close 驱动的 recovery 与确定性测试。 |
| `tests/hardware/fixtures/phase2b-fault-protocol-evidence-v1.json` | 新增独立 v1 synthetic fixture。 |
| `tests/hardware/src/checkpoint.rs`、`tests/hardware/tests/checkpoint.rs` | 实现 refreeze provenance fence 与 faults-only supersession；保留原 outcome/hash/provenance，覆盖旧/new faults 和非 faults checkpoint。 |
| `tests/hardware/tests/qualification_status.rs` | 固定缺失/矛盾 faults 子合同不能得到 `passed/0`。 |
| `docs/development/phase2b-checkpoint-software-closeout.md` | 实现/refreeze 时同步 faults-only supersession、稳定 token 和生效边界；本次文档返工不修改该文件。 |
| `docs/architecture/testing-strategy.md` | 实现时补充新 fixture、exact validator 和物理未验证边界；不得改 milestone 状态。 |

不应修改 `crates/easycon-serial/src/transport.rs`、`crates/easycon-serial/src/io.rs`、
`crates/easycon-controller/src/transport.rs`、production serial/Controller/Runtime、公共 API、根 behavior/schema/
conformance、现有 qualification projection v1 fixture、Cargo/Cargo.lock、workflow、roadmap 或现有 ADR 决策正文。

## Regression-first 确定性测试矩阵

每项先证明当前缺口，再实现修复；全部使用 fake/synthetic I/O，不枚举或打开物理串口。

| Case | 脚本与必须断言 |
| --- | --- |
| 115200 success | 一个 `Ok(1)` reply 等于 expected；仅一条 `succeeded`，actual 为 115200，成功后无追加。 |
| 115200 failure -> 9600 success | 第一条 scripted `io_error`，第二条 reply 匹配；顺序精确，actual 为 9600。 |
| 双 mismatch | 两次各返回一个不同于 expected 的 byte；两条均为 `mismatch`，各自 byte 不串位，actual 为 `null`。 |
| timeout fallback | 115200 protocol timeout、未读 byte，随后 9600 success；第一条 observed 为 `null`。 |
| cancel before/during read | handshake 开始后、read 前取消，以及 read barrier in-flight 后取消；均只 finalization 一次、无 fallback、observed 为 `null`。 |
| immediate/in-flight deadline | immediate deadline 在 outer handshake 前终态时数组为空；in-flight deadline 保留一条 `timeout`，无 open-after-deadline 或 fallback。 |
| open failure | open 返回 PortBusy/AccessDenied/Disconnected synthetic error；保留 started attempt、typed outcome 和 diagnostic，observed 为 `null`。 |
| partial-failure | 在各 role create/admit/wait/action failpoint 失败；已创建 role 保留当时 attempts，未创建 role 固定 `null`/空数组，首错不被覆盖。 |
| cleanup failure | Controller/Runtime close failpoint 后仍保留 close 前已终结 attempts 与 actual baud，最终状态保持 fail closed。 |
| close 驱动 recovery | handshake read 由 barrier 阻塞，operation cancel 后 bounded settle 失败，只有 resource-close cancellation 才通过 channel 释放；零 `sleep`，断言 close 解锁并 join、attempt 只 finalization 一次、counts/owner 收敛、无泄漏，首错与 cleanup error 均保留。 |
| 跨 role 隔离 | 四个 Harness 使用不同 scripted reply；attempt ID 从各自 run-local recorder 绑定，任何 role 都不能看到其他 role 的 byte/baud。 |
| reply correlation contradiction | 注入无 active read、错误 attempt ID/baud、重复 reply/finalization、success-without-byte 与 mismatch-with-equal-byte；均为 `contradiction` 且不能 `passed/0`。 |
| legacy checkpoint | provenance-fenced 旧 trusted passed faults -> `Unverified` + `legacy_protocol_evidence_missing`；旧 failed -> `Failed`；非 faults passed 不变；new v1 passed 仅在其余条件通过时为 `Observed`，输入 outcome/hash/provenance 不变。 |

command-level 全出口另以表驱动 fake 固定：

| Case | 必须断言 |
| --- | --- |
| device target request failure | final `result` 存在，stage 为 `device_target_request`，九字段与初始 resources/scenarios 存在，`failed/failed/1`。 |
| admission Rejected | final `result` 存在且 `completed/not_run/2`；四 role 未创建、无 attempt，原 rejection reason 保留。 |
| admission Ambiguous / discovery error | 分别保留 `identity_admission` / `identity_admission_discovery` 与原 structured diagnostic；均为 `failed/failed/1`。 |
| handler install / command dispatch interrupt | handler failure 为 `interrupt_handler_install` 和 `failed/failed/1`；dispatch interrupt 保留 `command_dispatch_interrupt` 和 `cancelled/unverified/130`。 |
| admission/runner/faults precheck interrupt | 分别保留 `identity_admission_interrupt`、`runner_interrupt`、`faults_interrupt`；均有 result、零 attempt 和 `cancelled/unverified/130`。 |
| admitted runner failure | 保留 `admitted_runner_failure` 与原 runner message；result 存在且 `failed/failed/1`。 |
| generic faults Err | 生成 stage `faults_command_error` 的结构化 result，不再是 `result = None`，保持 `failed/failed/1`。 |

每一行都必须使用 deterministic fake，不调用真实 discovery/I/O，并断言九字段存在、没有伪造 attempt、resources/
scenarios 与实际创建前缀一致、原始 execution/qualification/exit triplet 不被 normalizer 改写。

## Candidate 重开、验证与 refreeze

未来实现会改变 `tests/hardware` executable、artifact schema 和 telemetry，因此按 ADR-0011 必须重开当前 software
candidate。实现完成后至少需要：

1. 上述最小 regression-first tests 全部通过。
2. 根仓库按协作规则和冻结测试策略执行完整门禁：

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

3. `tests/hardware` 独立 workspace 执行 fmt、check、strict clippy 和全部 tests。
4. 固定新的 implementation HEAD/tree、`tests/hardware/Cargo.lock` SHA-256 和 executable SHA-256；不得沿用
   ADR-0011 的旧 executable provenance。
5. 由未参与实现的 reviewer 对新基线、完整差异、fixture/validator 与 legacy 分类进行独立 review；finding 修复后
   以再次变化的新基线复审。
6. 在上述证据全部完成后创建新的 refreeze record；不得改写 ADR-0011 原文，也不得在本文预先宣称 refreeze、
   merge readiness 或门禁已经完成。

## 后续硬件门槛

硬件 next gate 只能发生在实现和 refreeze 之后。对同一 user-attested pairing 使用全新 evidence root，授权范围只含：

1. 一次 minimum identity discovery；
2. 一次 `faults` run。

不得先跑 standalone handshake，不得重试失败 run。只有完整 faults、四 role cleanup 和 `Hardware Unverified`
checkpoint 经独立复审通过后，才可能另行授权 lifecycle 或 sequence。该 gate 仍不能单凭 USB identity、baud 或一次
faults 结果确认 MCU/固件、关闭 O 项、创建支持矩阵或冻结完整 Phase 2。

## 排除 contract option C

contract option C，即放宽当前多独立 session 要求，不属于本 bugfix。本文保持 faults 的 mandatory reconnect 与四个
独立 Harness 目标不变，不用 evidence 缺口掩盖产品决策。

若未来考虑 option C，必须另开 ADR，并在以下三个互斥产品方向中明确选择一个：继续保留 mandatory reconnect、
定义 persistent-session-only profile、或排除该 pairing。该 ADR 必须重新评估 faults 判据、支持矩阵含义和硬件
qualification 门槛；不能由本次 diagnostic result 或实现细节默认决定。

## 未决风险

- 当前 mismatch 没有 observed byte，根因仍未知；本设计只提高下一次授权复测的证据分辨率。
- qualification 两层 observer 的 correlation 若实现错误会制造伪 reply 归属，因此 contradiction 必须 fail closed，
  且跨 role/取消/deadline 回归是 candidate 阻断项。
- faults-only legacy supersession 只在新 candidate/refreeze 后生效；当前 checkpoint 映射不变。未来实现必须可靠区分
  provenance-fenced legacy 与新/未知 artifact，否则可能错误降级无效 pass。
- 在新实现、完整门禁、独立 review、refreeze 和受控硬件 next gate 完成前，Phase 2B、设备支持、固件身份和根因均
  保持未完成或未确定。
