# Phase 2B run directory ownership 设计

- 状态：Implementation Target (`Hardware Unverified`)
- 日期：2026-07-20
- 设计基线：`818fdf64d0cae0fd318602f68d0bffbd1dea6a98`
- 修订依据：`ba3fe410e7e46d281c109d8d90010237b542198f` 后的 Windows hard-link alias failpoint
- 上位契约：[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md)
- 相邻设计：[Controller close 中立化证据](phase2b-controller-cleanup-evidence.md)

## 问题与源码事实

当前 `ArtifactReservation` 用 command 组成三条路径：`<command>.json`、`<command>.json.tmp` 和
`.<command>.in-progress.json`。marker 只排斥同一 command；`handshake` 与 `sequence` 等不同 command 可以在
同一 output directory 同时取得 reservation，然后分别进入 dispatch、设备打开和物理动作。`sequence` 还写入
固定 `sequence-timings.csv`，所以目录内容不能解释成单一 artifact transaction。

默认 output directory 只含 Unix epoch 毫秒。两个进程在同一毫秒启动时可能自然选择同一目录；时间戳不是
互斥原语，也不能替代 `create_new`。

现有 marker 在写入并同步后关闭句柄。validation failure 和成功 commit 都按路径删除 marker；若另一个主体在
此期间删除并替换该路径，runner 会删除不属于自己的文件。现有回归只保护 raced temp/final，没有保护 marker
replacement，也只测试 `unknown` 对 `unknown`，没有覆盖跨 command。

[ADR-0010](../decisions/0010-phase-2b-qualification-evidence.md) 要求每次运行使用唯一 run directory，任何设备
动作前建立不可覆盖的 ownership record，并禁止并发 runner 共享目录。因此本设计把一个 CLI invocation 定义为
一个 run；同一目录既不能并发共享，也不能在完成后顺序复用给另一 command。多个资格 command 由 checkpoint
聚合多个 run directories，不在一个目录中拼接。

## 范围

本任务只实现：

- directory-wide、runner 永不删除的 reservation tombstone；
- command-specific final、lease-specific retained staging 与 reservation-owned auxiliary；
- empty-directory admission、retained-handle object ownership、exact-byte verification 和 no-replace publication；
- 跨 command、完成后复用、stale/replaced reservation 和 artifact race 的无硬件回归。

本任务不实现 append-only journal、completion marker、manifest、build provenance、stable identity guard、
device-wide lock、Ctrl+C、checkpoint 或 stale run 恢复工具。reservation 是后续 ledger 的 ownership seed，不冒充
durable journal 或完成证明。

## 固定路径与记录

每个 run directory 只允许一个固定 ownership tombstone：

```text
.easycon-hardware-run.reservation.json
```

它不使用 `in-progress` 名称，因为当前任务没有完整 crash-recovery/completion 协议。文件至少保存 planned
relative names：

```json
{
  "schema_version": 1,
  "kind": "run_directory_reservation",
  "lease_id": "00001234-0000000000000000185f4c2a12345678",
  "command": "sequence",
  "process_id": 4660,
  "created_unix_ns": 1784563200000000000,
  "primary_artifact": {
    "final": "sequence.json",
    "staging": ".sequence.00001234-0000000000000000185f4c2a12345678.json.staged"
  },
  "auxiliary_artifacts": [
    {
      "kind": "sequence_timings_csv",
      "final": "sequence-timings.csv",
      "staging": ".sequence.00001234-0000000000000000185f4c2a12345678.sequence-timings.csv.staged"
    }
  ]
}
```

`lease_id` 由本进程 ID 与高分辨率 Unix time 组成，只用于本地 ownership 和文件关联，不宣称随机性、安全性或
跨机器全局唯一。互斥权威始终是固定 tombstone 路径上的原子 `create_new`。记录只含 relative artifact names，
不写机器绝对路径。

final 保持 `<command>.json`。primary staging 使用 `.<command>.<lease_id>.json.staged`；sequence auxiliary
staging 使用上例中的 lease-specific 名称。staging 在成功或失败后都永久保留，runner 不执行 unlink；成功发布时
staging 与 final 是同一对象的两个 hard links。这样不需要在关闭排他句柄后按路径删除一个可能已被替换的文件。

tombstone 中的 planned name 只限定路径，不单独证明文件属于当前 run。`ArtifactReservation` 内部保存不可克隆的
`OwnedAuxiliary`；它包含当前 lease、kind、relative names、exact expected bytes 和从 `create_new` 起一直持有的
`File`。只有 reservation 接受 sequence renderer 产生的 bytes 并成功写入、同步和 readback 后才能构造该 token。
调用方不能用一个路径或文件名伪造 ownership。非 sequence command 的 auxiliary 列表固定为空。

“永久保留”只表示 qualification runner 从不删除或回收这些名字，不是 ACL、数字签名或永久防篡改承诺。
`commit` 返回、owner handles 关闭后，拥有目录写权限的外部主体仍可修改或删除文件；后续 manifest/checkpoint
必须从磁盘重算 hash，交接包再以外层 SHA-256 固定证据。

## Admission 与状态机

### 状态

```text
Unowned
  -> Reserving

Reserving
  -> Reserved
  -> Incomplete

Reserved
  -> Staged
  -> Incomplete

Staged
  -> Publishing
  -> Incomplete

Publishing
  -> Published
  -> Incomplete
```

- `Unowned`：固定 tombstone 不存在；不代表目录内容安全。
- `Reserving`：固定 tombstone 的 `create_new` 已成功并成为 admission 线性化点，但 canonical write、sync、readback
  或 lease 内目录复查尚未全部完成；任一步失败都保留 tombstone 并进入 `Incomplete`。
- `Reserved`：本 invocation 成功 `create_new` tombstone、写入 exact bytes、flush/sync，并确认目录 admission；
  tombstone handle 仍由 owner 持有。
- `Staged`：primary 和所有实际产生的 auxiliary 都由 retained handle 固定，已同步且经同一 handle readback。
- `Publishing`：至少一个 hard link 已创建，但其 final guard、source/final identity 或全部 final 尚未验证完成；
  任一步失败都保留已有 link 并进入 `Incomplete`。
- `Published`：auxiliary final 先发布、primary final 最后发布；每个 final 都持有 guard、绑定 source identity，
  随后通过 post-publication 验证。tombstone 和 staging 都保留。
- `Incomplete`：reservation 写入、目录验证、staging、publish 或 post-publication 验证任一步失败；tombstone 和已知文件
  保留，不自动回到 `Unowned`。

`Published` 和 `Incomplete` 都是终态 ownership。CLI 不删除 tombstone，因此不存在成功结束与下一 runner
admission 之间的复用窗口，也不会删除被替换的 marker。

### Admission 顺序

`real_main` 在 command dispatch 和任何设备枚举/打开前执行：

1. 验证 command 只能含 ASCII 字母数字或 `-`，并计算 relative final/staging names。
2. 在不修改目录内容的前提下枚举目录；已有任何 entry 时拒绝，保留全部原始 bytes且不创建 tombstone。
3. 生成 lease metadata 和 canonical reservation bytes；在固定 tombstone 路径以 read/write、`create_new` 以及
   Windows `share_mode(FILE_SHARE_READ)` 一次打开。该 handle 允许诊断读取，但拒绝其他 write/delete sharing。
4. 通过 retained handle 写入 canonical bytes，`flush`、`sync_all`，再通过同一 handle 从 offset 0 readback 并确认
   exact length、bytes 和 EOF；handle 存入 `ArtifactReservation`，在 publish 后验证完成前不关闭。
5. 再次枚举目录。除刚创建的 tombstone 外存在任何 entry，都将本 run 标为 contaminated/incomplete并返回错误。
   tombstone 保留，既有 entry 不修改、不删除。
6. 再通过 retained handle 验证 tombstone bytes；只有一致时返回 `ArtifactReservation` owner。

前置空目录检查避免调用者误传已有数据目录时被工具写入文件；它不承担互斥。真正线性化仍由随后的
`create_new` 完成。若两个 runner 同时看到空目录，至多一个能创建 tombstone；若其他 entry 在前置检查后竞态
出现，lease 内复查会失败并永久保留 tombstone，不能 rollback 后让另一 command 接管污染目录。unsafe command
在所有目录写入前拒绝，不能借路径逃逸在目录外创建文件。

Windows share mode 与 retained handle 固定的是 owner handle 持有期间的 tombstone 对象：标准文件 API 无法在 handle
关闭前写入、删除或同名替换它，即使 replacement bytes 恰好相同也不能获得路径。它不锁整个目录，不能阻止
外部主体创建其他文件；该边界由 commit 的复查和后续 checkpoint 检测。

## Commit transaction

sequence runner 不再直接接收 output path。它把 CSV 渲染成 bytes，并由 `real_main` 交给当前 reservation 的窄
`stage_auxiliary(sequence_timings_csv, bytes)` API。该 API 只接受 command 允许的 kind，拒绝重复 token，以
read/write、`create_new` 和 `share_mode(FILE_SHARE_READ)` 创建 planned lease staging，通过 retained handle
write/flush/sync/readback 后才在 owner 内登记。写入失败使 reservation poisoned，不会把半写文件登记为 owned。

`ArtifactReservation::commit` 消费 owner，固定顺序为：

1. 通过 retained tombstone handle 验证 exact bytes、length 和 EOF。
2. 枚举目录并校验 owned-entry allowlist：只允许 tombstone 和 owner 内已有 `OwnedAuxiliary` 对应的 staging；
   planned 但没有 token 的路径、任意 final、其他 staging 或 sentinel 都失败且不修改。
3. 先序列化 primary pretty JSON exact bytes，再以与 tombstone 相同的 share mode 和 `create_new` 创建 primary
   staging；通过 retained handle write/flush/sync/readback。任一失败保留 tombstone 和已创建 staging。
4. 再次通过 retained handles 验证 tombstone、primary 和每个 auxiliary 的 exact bytes，并枚举 owned-entry
   allowlist。此时仍不允许任何 final 或未登记路径。
5. 按固定顺序发布已登记 auxiliary，最后发布 primary JSON。每个 artifact 的 publication 必须依次完成：
   `hard_link(staging, final)`；以 read-only desired access、`FILE_FLAG_OPEN_REPARSE_POINT` no-follow flag 和
   `share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)` 打开并保留 final guard（明确不共享 DELETE）；通过 guard
   metadata 拒绝 `FILE_ATTRIBUTE_REPARSE_POINT`；再通过
   `file-id 0.2.3` 的 Windows high-resolution `FILE_ID_INFO` 分别读取 retained staging 与 guarded final 的
   volume/file ID 并要求完全相同；最后通过 guard readback exact bytes。只有四步全部成功，该 final 才进入
   published set。任一 final 已存在、guard 打不开、identity 不同、high-resolution ID 不可用或 filesystem 不支持
   hard link 都保守失败，不覆盖、不删除、不 rollback 已发布的 earlier link。
6. final guard 必须固定 final directory entry 而不是跟随 symlink/junction 锁住 target。guard no-follow open
   failure 或任何 reparse attribute 都直接失败；只有 guard 已持有且证明 entry 是普通文件后，`file-id` 才允许
   通过该稳定路径查询 identity。source handle 的 `FILE_SHARE_READ` 负责拒绝同一 file object 的新 writer；final guard 必须共享既有 source
   writer 才能打开，因此保留 `FILE_SHARE_WRITE`，同时通过不共享 DELETE 固定 final directory entry。单独保留
   source handle 不足以阻止删除新增 hard-link alias。hard-link 创建到 guard 打开之间若发生 delete/replacement，
   open failure 或 source/final identity mismatch 必须使状态成为 `Incomplete`；相同 bytes 不能绕过 identity。
7. 全部 source/final handles 保持打开，再次枚举 post-publication allowlist，并通过每个 final guard 读取 exact
   bytes；必须恰好包含 tombstone、全部 retained staging 和已经发布的 final。发现污染、缺失、identity 或 byte
   mismatch 时返回 artifact failure，不删除已经发布的文件。
8. 验证成功后返回 primary final relative path，随后 owner drop 才关闭 source/final handles。tombstone、staging
   和 final 都永久保留且 reservation bytes 不改变。

Windows `std::fs::rename` 会替换既有 destination，`exists` check 加 rename 仍有 TOCTOU。本任务保留当前同目录
hard-link publication，因为 destination 已存在时它失败而不是替换。若 filesystem 不支持 hard link，运行保守
失败；不得降级为可覆盖 rename。未来若引入平台原生 rename-no-replace，必须另做设计和故障测试。

final identity 不能用 canonical path、length、mtime 或相同 bytes 代替。内部 identity provider 只返回无其他
variant 的 `HighResolutionFileIdentity { volume_serial_number: u64, file_id: u128 }`；production provider 必须调用
`file-id::get_high_res_file_id` 并只接纳 `FileId::HighRes`，不因 API/volume 不支持而回退到 low-resolution ID。
依赖只封装平台查询，不改变 no-follow guard、allowlist、share mode、publication 顺序或 error policy；workspace
自身仍保持 `unsafe_code = "forbid"`。

begin、stage 或 commit 不扫描并删除 stale file。当前 lease 以 `create_new` 建立的 staging 也不删除；create_new
失败时绝不删除同名文件。final publish 后出现的失败不删除 final。

最后一次目录枚举与下一次外部目录写入无法由 `std` 文件 API 原子化。tombstone 线性化的是遵守协议的 runners；
对拥有目录写权限的非协作主体，本任务只保证检测 post-publication scan 已观察到的污染并 fail closed，不能承诺
scan 返回后不会再出现文件。manifest/checkpoint 必须以扫描当时的磁盘 bytes 为最终证据边界，不能信任内存
allowlist 或 tombstone 中的 planned names。

## 并发、取消与 deadline

- 固定 tombstone 的 `create_new` 是 directory admission 唯一线性化点。不同 command 和相同 command 使用同一
  路径，因此至多一个 runner进入 dispatch。
- retained handles 只活到 commit 返回；它们防止 transaction 期间 owned object replacement，不是跨进程结束的
  永久锁或 tamper seal。
- reservation 没有 background thread、timer、callback 或锁文件轮询；不影响 Controller 单写者、operation
  cancellation、deadline 或 Runtime close。
- Ctrl+C、process crash、强制终止和断电都不触发 tombstone回收。留下的 reservation/staging/final 由未来
  checkpoint 归类为 incomplete 或 published-without-completion。
- PID、mtime 和“进程当前不存在”只可作为诊断，不能授权自动删除或接管。重试必须使用新的空目录。
- directory ownership 不等于 device ownership。不同 output directories 仍可能指向同一设备；stable identity
  guard 和 device readiness/locking 是后续独立任务。

## 错误与所有权

reservation `create_new` 失败在 dispatch 前返回 artifact error，不生成当前 command final，不运行设备动作。
错误消息保存目标 relative/diagnostic path，但 artifact 内容不写开发机绝对路径。

如果 tombstone 创建成功后任一步失败，CLI 不删除它。这样既修复 marker replacement 删除风险，也保留 crash/
污染边界。transaction 存活期间，staging/tombstone replacement 由 source share-mode 拒绝；final alias 由 source
writer exclusion、final delete exclusion 和 high-resolution identity 联合固定。任一验证失败时 runner fail closed，
保留磁盘内容且不尝试“修复”或删除路径。`commit` 返回、owner handles 关闭后的 replacement 由
manifest/checkpoint 识别，本任务不把 exact-byte path read 冒充对象身份。

staging 和 final 的 `create_new`/hard-link ownership继续遵循现有规则：不覆盖、不删除任何磁盘文件。写失败或
publish 失败保留当前 lease staging，供 checkpoint 识别；成功 publish 也保留 staging。sequence final 只有
owner 内存在当前 lease 的 `OwnedAuxiliary` 才能发布，固定文件名或 planned record 本身不授予权限。

## 平台、打包与性能

实现只修改 `publish = false` 的 `tests/hardware` workspace。reservation 不进入 root workspace、native bundle、
C ABI、binding 或 package。CLI 已在 `real_main` 拒绝非 Windows 平台；Windows 实现使用安全的
`std::os::windows::fs::OpenOptionsExt::share_mode`、`windows-sys 0.61.2` 常量和 `file-id 0.2.3` safe API，
不在 workspace 代码中引入 `unsafe`。两个新增依赖固定在 Windows target，均不进入 release bundle；`file-id`
为 MIT OR Apache-2.0。若未来需要非 Windows hardware runner，必须先设计等价的 retained-object/no-replace/
identity primitive；不得静默使用只按路径复查的弱化 fallback。测试只运行 unknown command、未授权 Amiibo 或
直接调用 reservation API，不枚举/打开串口。

每次 invocation 增加一个小 tombstone write+flush+sync、primary staging write+flush+sync、retained-handle
readback，以及成功路径固定五次非递归目录枚举：admission 两次，commit 在 primary staging 前、publish 前和
publish 后各一次。sequence 另增加一个有界 CSV staging sync/readback；CSV 先在内存生成，大小受现有 step 上限
约束。每个 published artifact 另保留一个 read-only final guard 并执行两次 high-resolution file-ID lookup；artifact
数量当前最多两个，成本有固定上界。这些不进入 sequence timing 或 latency sample。目录规模按设计接近常数；
发现额外 entry 立即失败。

## 测试设计

实现前先加入旧基线确定失败的最小回归：在空目录中持有
`ArtifactReservation::begin("unknown", ...)`，再调用 `begin("amiibo", ...)`；第二次必须失败。旧实现因 marker
按 command 命名会返回成功。

随后至少覆盖：

1. 同 command 与不同 command 都只能有一个 directory owner；失败发生在 dispatch 前。
2. 第一个 owner成功 publish 后，tombstone bytes 保持不变，另一 command 仍不能复用该目录。
3. 预置 reservation sentinel 时，unknown 与未授权 Amiibo 都失败、sentinel bytes 不变，且不产生 final、staging、
   CSV 或设备动作。
4. failpoint 在 begin readback 后、primary/auxiliary sync 后和 publish 前 allowlist 后分别尝试删除、改写或同名替换
   retained tombstone/staging；Windows 必须拒绝 replacement，commit 仍只发布 owner bytes。该测试必须证明旧版
   关闭 handle 后的实现会接受至少一个 replacement。
5. begin 后注入固定 `sequence-timings.csv`、planned auxiliary staging 或任意同名文件都不能构造
   `OwnedAuxiliary`，不能发布 injected bytes；既有 bytes 不变。
6. sequence CSV 只有当前 lease 的 private token 才进入 allowlist；覆盖正常发布、CSV staging replacement 拒绝、
   auxiliary hard-link failure 和 primary publish failure 后已发布 auxiliary 不 rollback。
7. 目录已有 final、staging、CSV 或任意 sentinel 时，begin 在写入前失败且不创建 tombstone；既有 bytes 不变。
8. 前置空目录检查后发生竞态污染时，lease 内检查失败并保留 tombstone；staging/final 在 reservation 后竞态出现时
   不覆盖、不删除。publish 前和 publish 后注入 sentinel 都必须返回 artifact failure；post-publication 污染允许
   final 已存在，但状态不是 `Published`。
9. hard link 不受 retained source handle 支持时保守失败；staging、tombstone 和 earlier links 全部保留且不 fallback。
10. 在 hard-link 后、final guard 前分别注入缺失 replacement 与 exact-byte replacement；前者必须使 guard open
    失败，后者必须由 high-resolution identity mismatch 拒绝。guard 建立后对 final 的 delete/write 都必须被拒绝；
    source/final handles 全部关闭后的 control mutation 必须成功，证明 failpoint 不是 ACL 或只读目录伪通过。
11. production guard opener 只接受结构化 `FinalGuardOpenSpec`，并直接把其中的 desired access、share mode 与
    custom flags 交给 `OpenOptions`。不可跳过的 spy test 必须证明 commit 传入 desired READ、精确
    `FILE_SHARE_READ | FILE_SHARE_WRITE`，且 custom flags 包含 `FILE_FLAG_OPEN_REPARSE_POINT`；另一个不可跳过的
    guard metadata failpoint 强制 `FILE_ATTRIBUTE_REPARSE_POINT` 并验证拒绝。具备 symlink 权限的 Windows 环境
    还应运行 hard-link 后换成指向 staging 的真实 file symlink 测试，但它不替代前两项，也不能成为唯一门禁。
12. identity provider 分别在 auxiliary 和 primary high-resolution lookup 注入错误，且 provider 类型不能表达 low-res
    成功。断言当前/earlier link 与 staging 保留、后续 final 不创建、primary relative path 不返回且结果为
    `Incomplete`。
13. auxiliary staging 创建失败后即使攻击文件随后被外部移除、目录 allowlist 再次有效，poisoned reservation 仍不得
    创建 primary staging 或发布 final。
14. unsafe command 不能在目录内外创建 tombstone、final 或 staging。
15. 默认目录时间碰撞时，directory-wide create_new 仍只允许一个进程进入 dispatch。
16. reservation JSON 只含 relative names；lease-specific staging、auxiliary planning 和 envelope字段完整、可从磁盘
    重读。`commit` 返回且 owner handles 关闭后篡改文件时，后续磁盘 hash 检查必须可报告 mismatch，不宣称
    reservation 自身防篡改。

每个实现提交执行根 workspace 九项门禁和 `tests/hardware` 的 fmt/check/strict clippy/test。固定实现 SHA 后做
新的独立 review；任何修复形成新基线并重新 review。

## 后续边界与重新打开规则

durable ledger 后续复用 `lease_id`，在 tombstone之后建立 append-only journal；manifest 从磁盘哈希 final、
staging、journal 和 CSV，且必须识别同一 final/staging object 的预期 hard-link 关系；completion marker 只能在
manifest sync 后创建。引入这些文件时必须更新目录 admission/owned-entry规则，但不得让 tombstone自动消失或
允许目录复用。checkpoint 还必须把本任务因 post-publication pollution 返回的 artifact failure 归为 incomplete，
不能只因 primary final 存在就升级为 completed。

以下变化必须先重新审查本设计和 ADR-0010：

- 把 directory ownership重新降为 command-specific marker；
- 成功后删除 tombstone或允许顺序复用目录；
- 按 PID、mtime 或 operator convenience 自动回收 stale reservation；
- 用 `exists + rename` 或可覆盖 API 替代 no-replace publish；
- 关闭 retained handle 后再按 staging path 发布，允许 marker/staging 被替换后继续发布，或删除任何 evidence file；
- 仅凭固定 auxiliary 文件名或 planned record 授予 sequence CSV ownership；
- 不持有 no-follow final guard、允许 final reparse point、用 path bytes 代替 source/final high-resolution identity，
  或回退 low-resolution ID；
- 把 transaction 期间的 share-mode object lock 描述成 `commit` 返回后的永久 tamper protection；
- 把 directory lease误称为 device identity/capability 或硬件资格证据。
