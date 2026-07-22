# Phase 2B durable evidence transaction 设计

- 状态：Architecture Target (`Hardware Unverified`)
- 日期：2026-07-21
- 设计基线：`0ce3e45`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 前置设计：[run directory ownership](phase2b-run-directory-ownership.md)
- 相邻设计：[device admission/readiness](phase2b-device-admission-readiness.md)

## 问题与现状

当前 `tests/hardware` 已用 directory-wide create-new tombstone、lease-specific staging、no-replace hard link、
retained source/final handles 和 Windows `FILE_ID_INFO` 保护一次进程内发布。该边界能证明 owner 发布的 final link
与 retained staging 是同一文件对象，也能在 transaction 期间拒绝污染或替换，但它还不是 durable qualification
transaction：

- 命令执行期间只有内存 JSON；崩溃、强制终止或 artifact commit 失败不会留下动作前缀和 cleanup 进度；
- final JSON 与 CSV 没有由磁盘 bytes 重算的外部 SHA-256 manifest，也没有 completion marker；
- reservation、staging 或 final 存在不能区分 completed、incomplete 和 polluted；
- binary 没有内嵌 commit/tree/dirty/lock provenance，运行时也没有 executable hash；
- 旧 binary 在新 checkout 中运行时，当前目录的 Git 状态可能被错误当成本次 build 身份。

本设计补齐 ADR-0010 的软件证据事务，不改变 production Controller、Runtime 或 Serial 行为，不运行物理命令，
也不把软件 transaction 完成升级成硬件资格结论。

## 文件集合与状态机

每个 run directory 继续只允许一个 `ArtifactReservation` owner。固定文件角色如下：

| 角色 | relative name | 创建/发布方式 | manifest 成员 |
| --- | --- | --- | --- |
| reservation tombstone | `.easycon-hardware-run.reservation.json` | `create_new`，全程 retained | 是 |
| durable journal | `evidence.journal.jsonl` | `create_new`，单 owner append | 是 |
| primary staging | lease-specific hidden name | `create_new`，全程 retained | 是 |
| primary final | `<command>.json` | retained staging 的 no-replace hard link | 是 |
| auxiliary staging/final | lease-specific / 固定 CSV 名 | 同 primary | 是（若 planned） |
| manifest staging | lease-specific hidden name | `create_new`，全程 retained | 否 |
| manifest final | `manifest.json` | no-replace hard link | 否，避免自引用 |
| completion staging/final | lease-specific / `completion.json` | manifest sync 后 create-new staging，再 no-replace hard link | 否 |

状态机固定为：

```text
Vacant
  -> Reserved
  -> JournalStarted
  -> Running
  -> Finalizing
  -> EvidencePublished
  -> ManifestPublished
  -> Completed
```

任一步错误都保留已有文件和 handles 已确认的 bytes，不回滚、不删除、不覆盖。没有 completion marker 的目录永远
不是 completed；final JSON 或 manifest 单独存在也不能升级状态。tombstone 成功后出现非 owner entry，或完成后
manifest/hash/allowlist 不一致，分类为 polluted。合法前缀缺少后续对象分类为 incomplete。

## Append-only journal 与 projection

`EvidenceJournal` 由 `ArtifactReservation` 唯一拥有。journal handle 使用与现有 owned staging 相同的
`create_new + retained handle + FILE_SHARE_READ` 边界；外部进程可读但不能在 owner 活跃时取得写共享。owner 不暴露
裸 `File`、path-based reopen 或 truncate API。

每行是一个完整、紧凑的 JSON object，并以 `\n` 结束：

```json
{
  "schema_version": 1,
  "sequence": 7,
  "event": "operation_terminal",
  "elapsed_ns": 123456,
  "payload": {}
}
```

- `sequence` 从 1 开始严格连续；`event` 是稳定枚举字符串；`elapsed_ns` 来自本 run 的 monotonic origin。
- append 先序列化完整 line，再一次 `write_all` 到 retained handle；成功 `flush`/`sync_all` 后才更新内存 projection。
- projection 只由已 durable 的 event fold 得到，不能先改内存再把失败 append 当成功。
- parser 要求每行 object 完整、sequence 连续、schema/event/elapsed 类型正确；尾部 partial line、重复/逆序 sequence、
  completion 后追加或未知结构均使目录 incomplete/polluted，不能静默截断。
- 普通 action group、身份 admission/open guard、operation terminal 和 cleanup terminal 后同步。高频 sequence 样本仍写
  独立 CSV，不逐 report sync；journal 只同步 sequence start、terminal、CSV identity 和 cleanup。
- destructive Amiibo intent/chunk、operator response、interrupt request 和 cancellation terminal 由后续 D/C 节点调用
  同一 sync API；不得建立第二套 ledger。

初始 `run_started` 在任何 discovery/open 前同步，至少保存 run/lease ID、command、规范化参数、UTC start、build
provenance 和 runtime executable SHA-256。`identity_admission` 在构造 Harness 前同步。正常、机器判据失败、runner
failure、取消和 cleanup failure 都同步 `run_projection_finalized`。artifact commit 前再同步
`artifact_finalization_started` 并封闭 journal；此后 artifact 成败由目录状态和 completion marker 表示，不能修改已被
manifest 哈希的 journal。

## Build 与 runtime provenance

独立 hardware workspace 增加 `build.rs`，只使用本地 Git CLI 和 SHA-256，不访问网络。构建时嵌入：

- 完整 40-hex commit；
- `HEAD^{tree}` 的完整 40-hex Git tree；
- tracked dirty boolean；
- 对当前 worktree 全部 tracked path、类型和实际 bytes 的确定性 SHA-256；
- `tests/hardware/Cargo.lock` SHA-256；
- package version 和 provenance schema version。

tracked source digest 覆盖 staged、unstaged、删除和类型变化，算法按排序 path 加长度分隔与 bytes，不能只哈希
`git diff` 文本。build script 为参与 digest 的 tracked 文件、Git HEAD/index 和 Cargo.lock 发出 rerun 条件。Git、
commit/tree、tracked enumeration 或任一 read 失败时嵌入明确 `unknown` 和 `trusted = false`，不能用当前 runtime
checkout 补写。

进程启动后对 `current_exe()` 的实际 bytes 计算 SHA-256；失败同样令 provenance untrusted。final document 和
journal 都保存 build provenance 与 executable hash。qualification policy 在其他机器判据之后应用：只有原结果将为
`passed` 时才检查 provenance；untrusted provenance 将其降为 `unverified`、退出 2，并增加明确 check。原本的
failed/not_run/cancelled 不被 provenance 改写。tracked dirty 只有在 deterministic tracked-source digest 完整时才
可 trusted；它仍在证据中显式标记，checkpoint 可采用更严格策略。

runtime checkout 的 `git rev-parse` 最多作为单独 diagnostic，不参与 trust，也不能覆盖内嵌字段。测试用“旧 build
provenance + 不同 checkout diagnostic”证明 binary 身份保持不变。

## Final artifact transaction

finalization 顺序固定如下：

1. 同步 final projection 与 `artifact_finalization_started`，封闭 journal；
2. stage 全部 planned auxiliary，并从 retained handle 验证 exact bytes；
3. stage final JSON；JSON 只保存 auxiliary relative name、byte length 和 SHA-256，不保存绝对路径；
4. 复核 tombstone、journal、全部 staging 和目录 allowlist；
5. 以现有 retained-handle/no-follow/FILE_ID_INFO 协议发布 auxiliary 和 primary final links；
6. 从 retained handles 和 guarded final handles重新读取磁盘 bytes，计算 evidence member SHA-256；
7. 生成 manifest staging，sync 后 no-replace 发布 `manifest.json`，再从 final guard 读回并校验；
8. 再次验证目录 allowlist、每个 member 的 length/hash、staging/final high-resolution identity 关系；
9. `create_new` 写入 lease-specific completion staging 并 sync/readback，其中只保存 schema、lease/run ID、manifest
   relative name/length/hash；再以 retained-handle/no-replace 协议发布 `completion.json`；
10. 最后一次磁盘 classification 必须为 completed，才向 caller 返回 primary relative path。

manifest member 按 relative path 排序，字段固定为 role、relative path、bytes、uppercase SHA-256，以及 final link 的
`same_file_as` staging relative path。manifest 不信任 final JSON 中复制的 auxiliary hash；生成和验证都重读磁盘。
manifest 与 completion 不进入 manifest，避免 JSON/manifest 自引用。completion 只锚定 manifest，不复制结论。

目录 transaction 继续保留 staging 和 tombstone，既不自动回收 stale run，也不把 hard link 当成 commit 返回后的
永久防篡改。checkpoint 每次消费前必须重新 classification 和 hash。

## Classification

`classify_run_directory` 是只读、fail-closed 的磁盘分类器：

- `Completed`：reservation/journal/final/manifest/completion schema 均有效，allowlist 精确，manifest 从磁盘重算
  完全一致，声明的 staging/final 是同一 high-resolution file identity；
- `Incomplete`：目录是合法 owner 前缀但没有 completion，或 journal 有 partial tail/未封闭，或发布只完成一部分；
- `Polluted`：存在未声明 entry、完成 marker 后缺失/新增/替换 member、hash/length/identity 不一致、manifest 或 marker
  schema 矛盾、reservation 无法建立唯一计划；
- `Vacant`：目录为空且没有 reservation；它不是 run evidence。

活动 owner 正在 append 时 classifier 可能看到 incomplete，这是预期；它不能等待、修复或删除。无法读取必要对象时
在 marker 不存在时保守归 incomplete，marker 已存在时归 polluted。classification detail 保存稳定 reason enum 和
相对路径，不把机器绝对路径写入 tracked checkpoint。

## 错误、取消与资源所有权

- journal append/sync 失败发生在外部动作前时停止 admission；发生在动作后时停止后续动作，并仍由 Harness owner
  完成 operation settle、neutralize、Controller close 和 Runtime close。journal 不可用意味着本 run 不可 passed。
- final JSON serialization、auxiliary stage、publish、manifest 或 marker 失败不删除已 durable journal；caller 返回
  artifact execution failure，目录由 classifier 标为 incomplete 或 polluted。
- journal/manifest/completion 的 retained handle 与现有 primary/auxiliary handles 同属一个 reservation；没有 detached
  writer、后台 flush thread 或 Drop 伪 completion。
- `completion.json` final link 只能由成功通过所有 readback/identity/hash/allowlist 检查的 owner 发布。Drop、进程退出和只存在
  final JSON 都不能生成 marker。

## 实现拆分与无硬件测试

实现分两个独立节点：

1. **Build provenance 与 durable journal**：增加 build script、tracked-source/Cargo.lock/executable SHA-256、
   qualification downgrade；实现 retained append-only journal、durable-before-projection、initial/admission/operation/
   cleanup/finalization events。failpoint 覆盖 create/write/flush/sync、partial tail、逆序 sequence、failure/cancel/
   cleanup projection 保留，以及 unknown/dirty-without-digest/old-binary policy。
2. **Manifest、completion 与 classifier**：扩展 reservation plan/allowlist，发布 final JSON/CSV 后从磁盘重算 manifest，
   最后 create-new completion；failpoint 覆盖每个 publish 边界、manifest collision/mutation、completion collision、
   member 替换、unexpected entry、staging/final identity mismatch和成功后的磁盘重算。

测试全部使用临时目录、synthetic document、fake observer/hasher/clock 或现有 FILE_ID_INFO helper；不得调用 discovery、
打开串口或执行设备动作。时间/并发使用 barrier/channel/failpoint，不用随机 sleep。

每个节点执行根 workspace 九项门禁与 `tests/hardware` 的 fmt/check/strict clippy/test。提交后以新 SHA 做独立 review；
发现直接相关可复现 finding 时先回归、修复、完整门禁和新基线复审。

## 排除项与重新打开规则

本设计不实现 OperatorPort/Ctrl+C、Amiibo destructive journal event、telemetry CSV 新字段、checkpoint 聚合、支持矩阵
或物理测试；后续节点必须复用本 journal/transaction，不得另建覆盖式日志。O-01/O-02/O-04 保持开放。

以下变化必须先重开本设计：

- 删除 tombstone/staging，或用覆盖 rename、truncate、path reopen 取代 retained-handle/no-replace 协议；
- 从内存复制 hash 而不重新读取磁盘，或让 manifest 自哈希/包含 completion；
- final/manifest 存在即视为 completed，或在 hash/identity/allowlist 不一致时仍创建 completion；
- journal append 失败后继续外部动作，允许 sequence 间断/尾部 partial line被静默接受，或由多个 writer 共享 journal；
- 用 runtime checkout 身份覆盖 binary build provenance，或允许 unknown/untrusted provenance 产生 `passed`；
- 自动删除、覆盖、修复或复用 incomplete/polluted run directory。
