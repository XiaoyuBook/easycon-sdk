# Phase 2B Amiibo 资格写入安全设计

- 状态：Architecture Target (`Hardware Unverified`)
- 日期：2026-07-21
- 设计基线：`5fe4d61`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 前置设计：[durable evidence transaction](phase2b-durable-evidence-transaction.md)
- 相邻设计：[操作员、取消与观察](phase2b-operator-cancellation-observation.md)

## 问题与边界

当前 qualification CLI 只用 `--authorize-write` 和一个不绑定槽位的确认开关接纳写入。它读取 payload 后没有
校验调用者预期 SHA-256，也没有保存 limits 的外部来源。`write_attempted` 只在内存中置位，Controller 内部按
20-byte chunk 写入时，CLI 既不能在每个 chunk 前同步 durable intent，也不能在 chunk ACK、save、select 和
cleanup 各阶段保存最后已知进度。进程异常终止后，现有 journal 无法回答哪些破坏性动作已经开始。

本设计只补齐 `tests/hardware` 的 destructive-write admission、journal instrumentation 和 synthetic 回归。它不
改变 Phase 2A Controller/Runtime/Serial 公共契约，不执行真实设备写入，不关闭 O-02，不把调用者声明的容量升级为
设备观测能力，也不进入 Phase 3。

## 固定命令契约

未提供 `--authorize-write` 时，`amiibo` 必须在 discovery、Harness 和 native open 前返回
`completed/not_run/2`。授权分支必须在任何 Harness 创建前一次性解析并满足全部条件：

1. `--authorize-write` 恰好出现一次；解析得到的 token 只可 move 到一次 save/select runner，不能 clone 或复用。
2. `--expected-identity` 和 `--port` 形成已有 typed `DeviceTargetRequest`，后续仍经过 initial admission 以及每次
   native open 的 pre/post identity guard。
3. `--slot N` 与 `--disposable-slot N` 都恰好出现一次且数值完全相同。旧的 blanket
   `--confirm-disposable` 不能单独或共同形成授权。
4. `--slot-count`、`--maximum-data-len` 和 `--limits-source` 都恰好出现一次。source 是非空、无控制字符、不含
   absolute、rooted 或 Windows drive-prefix 本机路径的外部证明引用；结果明确标记这些 limits 为
   `declared_external`，不能标记为 measured 或 discovered。normalized journal 始终脱敏 raw source，只有通过校验
   的引用进入结构化授权证据。
5. `--data` 指向的文件先完整读入内存；artifact 和 normalized arguments 不保存该机器路径。
6. `--expected-sha256` 必须是恰好 64 位十六进制。工具对实际 bytes 重算大写 SHA-256；expected、recomputed、
   actual length、slot、declared limits 和 source 全部一致后才构造 authorization。
7. `AmiiboLimits` 必须接纳 slot 和实际 payload length；空 payload、越界 slot、超出 declared/protocol maximum、
   hash mismatch、缺参、重复参数或未知的旧授权开关都在 discovery/open/write 前失败。

authorization 绑定本 run lease ID、expected stable identity、slot、payload length/hash 和 limits source。它不是
capability，也不能跨 run、设备、slot 或 payload 使用。保存结果只记录 `payload_source.kind = local_file` 和
`path_recorded = false`，不记录路径。

## Retained journal 的共享 writer

chunk write 发生在 Controller 的单 writer lane，而 `ArtifactReservation` 由命令线程拥有。为在真实写字节前形成
durability boundary，`EvidenceJournal` 的 retained file、expected bytes、projection、sequence、seal/poison 状态和
monotonic origin 组成一个 `Arc<Mutex<JournalState>>`。`ArtifactReservation` 保留 owner；它可发放窄的、可 clone
`JournalWriter`，writer 只暴露 `append(kind, object)`，不能 seal、readback、发布 artifact 或取得路径。

每次 append 在同一锁内按固定顺序执行 serialize -> retained handle write -> flush -> `sync_all` -> 更新 expected
bytes/projection。sync 成功前不得调用下层 destructive transport。任一步失败会 poison journal，当前动作返回失败，
后续 append 也失败；磁盘由 checkpoint 分类为 incomplete，不伪造 completed artifact。锁中不调用 Controller、
Runtime、operator 或用户 callback。

命令线程在请求 operation cancel 或等待 operation terminal 时不持有 journal 锁。controller lane 也只在一次 append
期间持锁，append 返回后才调用 inner transport。因此 interrupt journal、chunk journal 和 cleanup journal 可以按
真实线性顺序串行化，不形成 journal-lock/operation-wait 环。最终 seal 只在 Controller 和 Runtime 已显式 close、
transport writer 已释放后发生；seal 后任何迟到 writer 都失败。

## Qualification-only transport 状态机

在现有 `ObservedTransport` 与冻结的 serial transport 之间增加 qualification-only Amiibo evidence decorator。它
只观察 `ControllerTransport::{write, wait_for_ack, close}` 的 source-exact command bytes、stable write sequence 和
operation ID，不改变 production enum 或 Controller chunk 算法。

状态机按 operation 隔离：

```text
Idle
  -> ChunkHeaderIntent(slot, offset, length, attempt, write_sequence)
  -> HeaderAccepted -> HeaderAcked
  -> PayloadAccepted -> ChunkTerminal(acked)
  -> Idle / next chunk

Idle -> SelectIntent(slot, write_sequence) -> SelectAccepted -> SelectTerminal(acked) -> Idle
Any failed/cancelled exchange
  -> CleanupResetIntent(write_sequence) -> CleanupResetTerminal -> Idle/Failed
```

只有完整匹配 source-exact save header、select 或 reset command 才进入相应状态；payload 只在已 ACK 的 header state
中按 header 声明长度接纳，因此任意 payload 前缀不能冒充命令。每个 logical write 的 partial calls 必须保持相同
resource/operation/sequence/total length 和连续 offset。矛盾、未知 command ordering、跨 operation 交叉或 invalid
partial progress 都返回 protocol failure并保存结构化 terminal evidence。

在首次 header byte 交给 inner transport 前，decorator 同步 `amiibo_chunk_intent`，包含 run/device/slot、offset、
length、attempt、operation ID 和 write sequence。header/payload 每次返回后更新内存 trace；payload 的匹配 ACK 后
同步 `amiibo_chunk_terminal(status=acked)`。write/ACK failure 或 cancel 同步 terminal，保存 stage、已知 accepted
prefix、结构化 transport kind 和 attempt。retry reset 以及下一次 header 都有独立 intent/terminal，不覆盖前一次。

select write 前同步 intent，ACK/failure 后同步 terminal。save operation 和 select operation 的唯一终态由命令线程
同步到 journal，包含 stable operation ID、state、structured error 和 cancellation reason。Controller close 的最终
neutralization、Controller state、Runtime close outcome/counts 继续由统一 cleanup terminal 同步；Amiibo decorator
另保存协议 cleanup reset 的 intent/terminal。这样 save 成功而 select 失败时，已 ACK chunks 和 save terminal 不会
丢失。

顶层 `amiibo_write_intent` 必须在 `save_amiibo` admission 前同步，完整保存 authorization predicate，但不保存 raw
payload 或路径。若该 append 失败，Controller 不得接纳 save。`write_attempted` 只在该 durable intent 成功后为 true；
`write_performed` 只表示 save operation succeeded，select 另有独立字段，不能用一个布尔值覆盖两阶段事实。

## 投影与资格语义

final projection 至少保存：

- authorization 的 lease/device/slot binding 和所有 predicate；
- external limits 的 declared values、source reference 和 `measured = false`；
- actual payload length、expected/recomputed SHA-256 和 exact match；
- 每个 chunk attempt 的 offset/length、logical sequences、accepted prefix、header/payload ACK 和 terminal；
- save/select operation 的完整结构化终态；
- protocol cleanup reset、Controller neutralization/close 和 Runtime close；
- `capability_inference = none`、`o_02_status = open` 和 raw payload/path omitted。

未授权仍为 `not_run`。任何 admission、journal、save、select 或 cleanup failure 都不能 `passed`。即使全部软件步骤和
操作员授权完成，外部 limits 仍只是 attested/declared，O-02 保持开放，因此当前 command 最多为
`completed/unverified/2`。Ctrl+C 继续服从 ADR-0010：停止后续 admission、取消当前 operation、保存已知 chunk
进度、完成 cleanup，并在 cleanup 成功时形成 `cancelled/unverified/130`；cleanup failure 覆盖为
`failed/failed/1`。

## Failpoint 与无硬件验证

测试只用 fake discovery、synthetic ControllerTransport、可控 ACK/write 结果和临时 retained journal。不得枚举或
打开本机串口。至少覆盖：

1. 未授权、重复授权、旧 blanket 确认、disposable slot mismatch、缺失 limits source、越界 limits、空 payload、
   malformed/mismatched SHA-256 均不 discovery、不创建 Harness、不写 transport。
2. 顶层 write intent 的 write/flush/sync failpoint 发生在任何 save header 前；journal poisoned 后 run 分类为
   incomplete，不能提交伪终态。
3. 第一和后续 chunk 的 header write、header ACK、payload partial write、payload ACK 各失败点都保留 intent、精确
   accepted prefix、operation error 和已完成 chunk 前缀，并停止未接纳的后续 chunk。
4. retry reset 的 write/ACK 失败与成功重试均按 attempt 保留；同一 chunk intent 不被覆盖。
5. save 成功后 select intent/write/ACK 各失败点仍保留全部 chunk 和 save terminal；select 成功有独立 terminal。
6. Ctrl+C 在 save 和 select 中分别得到 requested cancellation、唯一 operation terminal、protocol cleanup（若发生）、
   Controller neutralization、Controller close、Runtime close、cancelled artifact 的确定顺序。
7. final neutralization/Controller close/Runtime close failpoint 使结果 `failed/failed/1`，同时保留此前 destructive
   progress。
8. payload 恰好跨多个 20-byte chunk、末 chunk 非整长、transport partial write 和 data bytes 恰似 command prefix
   时，状态机仍按 operation/context 产生准确事件。
9. journal parser/manifest 从磁盘重算，final JSON 不含 raw payload、本机 data path 或未验证 capacity claim。

每个实现节点先加入旧实现稳定失败的最小回归，再修改根因；完成后执行根 workspace 全部门禁和
`tests/hardware` 独立 fmt/check/strict clippy/test。任何真实硬件结论、支持矩阵行或 O-02 关闭均不属于本任务。

## 重新打开规则

以下变化必须先重开并 review 本设计：

- 允许只凭 `--authorize-write`、blanket confirmation 或未绑定 slot/hash/identity 的输入写入；
- 在 chunk intent `sync_all` 前调用 inner transport，或把事后 Runtime event 冒充 write-ahead；
- 解析错误 message 推断 chunk/native progress，或修改 Phase 2A public transport/Controller 契约提供测试 hook；
- 把 declared external limits、一次短写成功或 known VID/PID 升级为 observed capacity/capability；
- 在 O-02 开放时把成功写入投影为 `passed`，或丢弃失败、取消及 cleanup 的已知 destructive progress。
