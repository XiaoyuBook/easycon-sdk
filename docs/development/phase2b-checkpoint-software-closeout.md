# Phase 2B checkpoint 软件收口设计

- 状态：Architecture Target (`Hardware Unverified`)
- 日期：2026-07-21
- 设计基线：`594c7f3`
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 前置设计：[durable evidence transaction](phase2b-durable-evidence-transaction.md)
- 相邻设计：[telemetry 与资格投影](phase2b-telemetry-qualification-projection.md)

## 问题与边界

现有 `classify_run_directory` 已能从磁盘重新验证 reservation、journal、final、manifest、completion、allowlist、hash
和 high-resolution file identity，并给出 `Vacant`、`Incomplete`、`Polluted` 或 `Completed`。它还不能生成 ADR-0010
要求的 checkpoint：没有固定输入布局、evidence class 映射、completed document 验证、handoff attestation 边界或
checkpoint 自身的不可覆盖 transaction。

本设计只增加一个只读 evidence consumer 和一个 ignored checkpoint artifact。它不枚举/打开串口，不发送 report，
不创建 `hardware/matrix.yaml`，不把旧 handoff 描述伪装成当前物理观察，也不关闭 O-01、O-02、O-04。即使输入中
存在 exact-command `passed` run，输出仍是 `Hardware Unverified`，不是支持设备行、完整 Phase 2 冻结或 Phase 3
入口。

## CLI 与输入所有权

资格 CLI 增加纯软件命令：

```text
easycon-hardware-qualification checkpoint \
  --runs-root PATH \
  [--attestations-root PATH] \
  --output-dir PATH
```

`--runs-root` 必需并把 immediate child directories 当作 run；`--attestations-root` 可选并把 immediate regular
`.json` files 当作 attestation。两者的其他 immediate entry 不能静默忽略，而是形成 `Failed` input record。扫描不递归、
不跟随目录 symlink/junction，按稳定相对名称排序，单类最多 4096 个 entry。output
directory 必须与两个 input root 分离，不能相同、位于 input 内或包含 input；preflight 在 reservation、journal 或
任何 output 文件创建前完成。

所有 path option 在 normalized journal arguments 中写为 `<redacted-path>`。checkpoint 只保存 input entry 的安全单
component relative ID、内容 hash 和结构化分类，不保存 input root、绝对路径、canonical path、用户名或任意污染
文件名。relative ID 必须是 UTF-8、非空、非 `.`/`..`，且只含 ASCII 字母、数字、点、下划线和连字符；不合规则
entry 只通过排序 snapshot hash 参与目录完整性，不把原名复制到 checkpoint。

checkpoint command 创建自己的 `ArtifactReservation`，但不创建 Harness、DeviceDiscovery 或 Operator observation。
Ctrl+C 只中断有界磁盘扫描，得到 `cancelled/unverified/130`；没有 Controller/Runtime cleanup claim。

## Run directory 只读 snapshot

每个 run child 先调用现有 `classify_run_directory`。checkpoint reader 与 classifier 位于同一 artifact ownership
模块，复用 no-follow guarded open、reparse rejection、retained final identity规则，不另写宽松的 `fs::read` parser。

每次消费同时生成 `run_snapshot_sha256`：按排序 entry 对 name bytes、entry kind、可安全读取的 byte length 和 SHA-256
进行 domain-separated hash。无法 no-follow 读取、非 regular file、非 UTF-8 name 或 metadata 矛盾令
`snapshot_complete = false`，但仍得到覆盖已观察安全元数据的 hash。checkpoint 不输出原始 entry names；completed
run 必须 `snapshot_complete = true`。

`Completed` run 再从 guarded final handles 读取：

- reservation 的 lease ID、command 和 primary relative name；
- final document bytes/hash；
- journal、manifest、completion bytes/hash；
- manifest member list及 auxiliary relative name/length/hash。

checkpoint 不信任 final JSON 内复制的 auxiliary hash，仍以 classifier 从磁盘重算的 manifest 为准。completed final
document 必须是 schema v2，且 `execution_status`、`qualification_status`、`exit_code` 是 telemetry 设计固定的合法
三元组；top-level legacy `status` 不参与判据。未知 schema、非法三元组、缺失 provenance 或 manifest/document
command/lease 矛盾都把该 entry 归为 `Failed`，不能升级为 Observed。

## Evidence class 映射

checkpoint 固定且总是输出五个数组：`Observed`、`Attested`、`Unverified`、`Failed`、`NotRun`。磁盘 run 映射如下：

| 磁盘/终态 | evidence class | 说明 |
| --- | --- | --- |
| `Completed` + `completed/passed/0` + trusted provenance | `Observed` | 仅表示该 exact command/run 的原始观察通过 |
| `Completed` + `completed/unverified/2` 或 `cancelled/unverified/130` | `Unverified` | 已有动作或证据，但不足以形成资格通过 |
| `Completed` + `completed/failed/1` 或 `failed/failed/1` | `Failed` | 机器判据、runner、artifact 或 cleanup 失败 |
| `Completed` + `completed/not_run/2` | `NotRun` | 身份、授权、能力或外部前置条件阻止执行 |
| `Incomplete` | `Unverified` | 合法 transaction 前缀，没有可验证 completion |
| `Polluted` | `Failed` | allowlist、hash、identity、schema 或 completed evidence 被污染 |
| `Vacant` | `NotRun` | 显式 child directory 内没有 run evidence |

`Observed` 不等于 supported、Attested 或 O 项关闭。若 `passed` document 的 provenance 不 trusted、缺失，或
snapshot/manifest 无法重新验证，则归 `Failed`，不能降格后继续称为原始 observed pass。

每个 run entry 保存 class、source kind `run_directory`、safe source ID、disk status/reason、snapshot hash/complete、
command（仅 completed）、document outcome 以及 final/journal/manifest/completion hashes。`Incomplete/Polluted/Vacant`
不伪造 command 或 execution outcome。

## Handoff attestation

可选 attestation file 只接受严格 schema：

```json
{
  "schema_version": 1,
  "kind": "easycon_hardware_handoff_attestation",
  "attestation_id": "handoff-com8-20260720",
  "handoff_sha256": "64 UPPERCASE HEX",
  "claims": ["stable safe claim ID"]
}
```

文件最大 1 MiB；object key 必须精确，attestation/claim ID 只允许安全 ASCII token，每个 claim 唯一且最多 256 个。
checkpoint 从磁盘 bytes 重算 `attestation_sha256`，保存 handoff hash、attestation hash 和 claim IDs，不保存自由文本、
聊天内容、设备推断或路径。合法文件进入 `Attested`；畸形、重复 ID、hash 格式错误、非 regular/reparse file进入
`Failed`，并只保存 safe source ID、snapshot hash 和稳定 reason，不复制不可信 payload。

Attested 永远不升级为 Observed。它只能说明某个 hash-protected handoff 曾声明这些 claim ID；checkpoint 的
`source_kind = handoff_attestation` 明确其来源。

## Checkpoint document 与 transaction

command result 内固定保存：

```json
{
  "checkpoint": {
    "schema_version": 1,
    "kind": "easycon_phase2b_hardware_unverified_checkpoint",
    "status": "Hardware Unverified",
    "open_items": ["O-01", "O-02", "O-04"],
    "support_matrix_rows_created": false,
    "evidence": {
      "Observed": [],
      "Attested": [],
      "Unverified": [],
      "Failed": [],
      "NotRun": []
    },
    "summary": {}
  }
}
```

每个数组按 `source_kind + source_id` 排序；同一 source ID、run lease ID 或 attestation ID 重复时 command execution
失败，不能任意选一个。summary 保存五类 count、总 input count、completed/incomplete/polluted/vacant disk count和
attestation count，并从数组/entry 重算，不信任输入。

checkpoint 自身走现有 append-only journal、final JSON、manifest、completion transaction。它没有 auxiliary；外层
manifest 从磁盘重新哈希 reservation、journal、checkpoint final/staging。`completion.json` 仍只锚定 manifest。
最终 document 使用 schema v2 的 `completed/unverified/2`：checkpoint 软件执行完成，但 O 项开放，所以永远不能
`passed/0`。结构矛盾、scan/read/hash/serialization 或自身 artifact failure 为 `failed/failed/1`。

## 验证与 failpoint

全部测试使用临时目录和 synthetic transaction，不调用 serial discovery/open：

1. completed passed/unverified/failed/not-run/cancelled document 映射到精确 evidence class；
2. incomplete、polluted、vacant 目录分别映射 Unverified、Failed、NotRun，且原始文件不被修改；
3. manifest member/hash、completion、primary schema/outcome/provenance 任一矛盾 fail closed；
4. 合法 attestation 只进入 Attested，畸形/重复/超限进入 Failed，payload 和绝对路径不泄漏；
5. input/output overlap、symlink/junction、非 UTF-8/unsafe ID、超过 entry limit 在 output reservation 前拒绝；
6. arrays/summary 稳定排序，运行两次相同输入生成的 checkpoint payload除 run/UTC/provenance 外一致；
7. checkpoint final/manifest/completion 可由现有 classifier 从磁盘重算为 Completed；
8. output 始终 `Hardware Unverified`、`completed/unverified/2`，且仓库不出现 `hardware/matrix.yaml`。

实现提交前执行根 workspace 完整门禁、Runtime models、Python validators，以及 `tests/hardware` 独立 workspace 的
fmt/check/strict clippy/test。实现 SHA 固定后重新整体 review；直接相关 finding 先回归、修复、门禁和新基线复审。

## 排除项与重新打开规则

本设计不生成支持矩阵、不消费远程 URL、不上传 artifact、不签名、不压缩 handoff，也不运行物理测试。最终 Git
bundle/handoff 是软件候选冻结后的独立交付步骤。

以下变化必须先重开本设计或 ADR-0010：

- 递归/跟随 reparse 扫描，保存 input 绝对路径或污染文件名，或在 input validation 前创建 output；
- 未重新运行 classifier/hash 就信任 final JSON、manifest 或 completion 中复制值；
- 把 incomplete/polluted、untrusted passed、handoff attestation 或空目录升级为 Observed；
- 省略五类中的任一类、合并 Attested/Observed，或让 summary 与数组独立维护；
- checkpoint 输出 `passed/0`、创建 `hardware/matrix.yaml`、关闭 O 项或冻结完整 Phase 2。
