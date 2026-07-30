# 0021：冻结 Phase 4 C1 Windows high-resolution file-identity foundation 前置合同

- 状态：Accepted / Effective
- proposal/decision 日期：2026-07-30；这不是任何 probe 或 review 的执行时间
- 接受与生效日期：2026-07-30
- proposal 基线：`main@2fe7eb2ef50f9ec49fe4e186b36605b6502aebb1`
- proposal 基线 tree：`1281e677ae4e26bfc7a85b19dd6a392d9736eff0`
- 初始 fixed-SHA proposal：`7f35f68d5620a581f51d1db081881fff7f416f21`，tree
  `c743a2df2e2c1fb77283a18c779459356103491f`，parent
  `2fe7eb2ef50f9ec49fe4e186b36605b6502aebb1`
- 初始独立 fixed-SHA review：`REWORK`，P0/P1/P2=`0/2/0`
- 第一轮修订 fixed-SHA proposal：`3a7bf3a031dbf2bd57bcba9c63270d5bcc4b0676`，tree
  `d100348bc1674c31ae442dad779d9d544f33638f`，parent
  `7f35f68d5620a581f51d1db081881fff7f416f21`
- 第一轮修订独立 fixed-SHA review：`REWORK`，P0/P1/P2=`0/4/3`
- 固定接受候选 / 第二轮修订：`ff10956578c4cce4f405b13bfdffbbe134175d68`，tree
  `b39d9822eec129122ec944c6e62ed2fd534f4aa6`，parent
  `507ada447ccef7acd78b8bc0ca54238d2ccce7c1`
- 最终 fixed-SHA 独立 review：任务 `019fb4f5-362b-7003-8415-0eacd208341f`，`APPROVE`，
  P0/P1/P2=`0/0/0`
- 上位冻结决定：[ADR-0017](0017-phase-4-ecs-automation-target.md) 与
  [ADR-0019](0019-phase-4-c1-lexer-contract.md)
- 现有 system-leaf 证据：[qualification-private manifest](../../tests/hardware/file-id-handle/Cargo.toml)、
  [borrowed-handle FFI](../../tests/hardware/file-id-handle/src/lib.rs) 与
  [retained publication 设计](../development/phase2b-run-directory-ownership.md)
- 编号说明：`main@2fe7eb2` 尚未包含 ADR-0020，但本地并行 ref `c09c323` 已为另一项 proposal
  分配 ADR-0020；本提议使用 ADR-0021，不链接、修改或取代该并行 proposal 的合同

## 状态、接受决定与授权边界

固定接受候选 `ff10956578c4cce4f405b13bfdffbbe134175d68` 已完成 fixed-SHA 独立 review 并获得 `APPROVE`，
P0/P1/P2=`0/0/0`。本 ADR 现接受并生效，下文规范性合同从本次 docs-only acceptance/refreeze 起成为后续实现与
审查的冻结依据。本次 acceptance 只改变状态、治理链与索引，不修改固定候选已经审查通过的 foundation public
authority、唯一 unsafe leaf、dependency admission、Windows object-authority、stable assertions、两套门禁或实施 DAG。

candidate `7f35f68` 与 `3a7bf3a` 都没有通过各自的 fixed-SHA review，继续只作为被取代的历史对象；旧 verdict 不得
复用于固定接受候选。本文接受两项上位设计结论：

1. 放弃低分辨率 identity 方案，不把 C1 限定为 NTFS，也不增加无法可靠执行的 filesystem admission；改为一个
   先行、共享、high-resolution `FILE_ID_INFO` foundation；
2. 冻结从 lexical input、每层 ancestor、root/`lib` directory、candidate admission 到 pre-read binding 的完整
   object-authority 顺序，并把 share exclusion 与 identity mismatch 拆成独立 RED。

本 acceptance 仅解锁独立的 Windows high-resolution file-identity foundation 节点 F0：先建立 stable RED/assertions，
再完成 dependency admission、qualification-private helper 迁移、扩展后的 guards、root/hardware 两套完整门禁、一个
F0 implementation commit、fixed-SHA 独立 review 与单独 refreeze。F0 refreeze 完成前，ADR-0017 的 root workspace/W0
零依赖 guard 与 ADR-0019 的 C1 RED 顺序继续有效；不得给 `easycon-ecs` 增加依赖，也不得开始 C1 loader/lexer 实现。
本 acceptance 不声明 foundation、dependency admission、hardware migration、guard、F0 assertions、门禁或 C1 已完成，
也不接受任何 implementation candidate。本 acceptance 自身的 SHA/tree 不在 tracked 文档中预言，只由提交后的 Git 对象
与外部验收记录固定。

本文只窄覆盖 ADR-0017/0019 的 root member/dependency whitelist 和 Safe DAG。SourceBundle、limits、ProgramHash
framing、restricted loader、UTF-8/BOM、lexer、diagnostic、其余 RED 与后续 C2-C3/E1-E2 全部保持不变。这是 system
boundary 的窄开，不是 loader 安全合同降级，也不接受任何实现或硬件结论。

## 问题与可复现工具链证据

ADR-0017 要求 restricted loader 拒绝 reparse/root escape，并在 read/copy/decode 前完成枚举、路径和 limits
preflight；ADR-0019 把 dependency guard、SourceBundle/limits、ProgramHash 和 restricted loader 排在 C1 前四个 RED。
在 Windows namespace 可被并发 retarget 时，path、`DirEntry`、length、timestamp 或 canonical string 都不能证明
实际读取对象；对象 authority 必须来自 nofollow/shared guarded `File` handle。

固定 `rustc 1.97.1 (8bab26f4f 2026-07-14)`、host `x86_64-pc-windows-msvc` 上，标准库 by-handle accessor
仍未稳定。以下 safe probe 定义必须针对每一个后来形成的 fixed-SHA candidate 单独复现的证据，不把
2026-07-30 decision date 声称为该 candidate 的执行时间：

```rust
#![forbid(unsafe_code)]

#[cfg(windows)]
pub fn identity(metadata: &std::fs::Metadata) -> (Option<u32>, Option<u64>) {
    use std::os::windows::fs::MetadataExt;
    (metadata.volume_serial_number(), metadata.file_index())
}
```

review evidence 必须在该 candidate 的只读 worktree 中，把上面 exact source 与 compiler output 都放入 repo-external、
task-private temporary directory，并记录、绑定以下三项原始输出；`EASYCON_IDENTITY_PROBE_TEMP` 必须先解析为该外部目录：

```text
git rev-parse HEAD
rustc -vV
rustc --crate-type lib --emit metadata --out-dir "$env:EASYCON_IDENTITY_PROBE_TEMP" "$env:EASYCON_IDENTITY_PROBE_TEMP\windows_by_handle_probe.rs"
```

第一项必须等于被审 SHA，第二项必须是上述固定 release/host，第三项必须让两个 accessor 都返回 `E0658` 并标记
unstable library feature `windows_by_handle`；未记录 candidate SHA 或只记录 decision date 不构成执行证据。即使这些
accessor 稳定，这组字段也不是本合同需要的 128-bit high-resolution file ID。
`easycon-ecs` 又必须继续 `#![forbid(unsafe_code)]`，因此 Win32 query 只能进入一个经审计、共享、safe facade 后的
内部 system leaf，不能在 C1 或 qualification workspace 各复制一份 unsafe shim。

## F0：共享 high-resolution foundation

### root crate 与唯一 public authority

F0 新增 root workspace internal crate：

```text
package = easycon-file-identity
path = crates/easycon-file-identity
license = GPL-3.0-only
publish = false
```

该 crate 只在 Windows 提供 safe borrowed-`File` API。v1 public surface 只能表达“验证一个仍存活的 file object 可取得
高分辨率 identity”和“在同一次调用内比较两个仍存活 file objects”，例如：

```rust
pub fn validate_file_object(file: &std::fs::File) -> std::io::Result<()>;
pub fn same_file_object(
    left: &std::fs::File,
    right: &std::fs::File,
) -> std::io::Result<bool>;
```

内部可以使用私有
`HighResolutionFileIdentity { volume_serial_number: u64, file_id: u128 }`，但该类型、字段或等价 tuple 不得公开、
序列化、缓存到调用方，也不得脱离 borrowed handle lifetime 成为 C1 authority。`same_file_object` 必须在一次调用中
分别 query `left` 与 `right`，两个 `File` 在两次 query 和比较完成前始终同时存活；不得因引用地址相同而跳过 query。
两个 query 都成功且通过质量检查时，完全相等返回 `Ok(true)`，不同返回 `Ok(false)`；任一 query 失败不返回 bool。

Windows tests 必须把由 nofollow guarded open 得到的 volume/share root、普通 directory 与 regular file `File` 逐类传入
上述 API，覆盖同一 object 的两个同时存活 handles、distinct objects、query failure 与质量拒绝；root/目录不能只由
regular-file mock 代替，handle lifetime 不能只由复制出的 identity value 代替。

foundation 不提供 path overload、open/canonicalize/metadata/read/write/delete/close、raw `HANDLE`、ownership transfer、
low-resolution fallback 或 unsafe public API。调用方必须先自行建立 guarded `File`，foundation 只比较内核 file object。

### exact Windows leaf 与 unsafe containment

foundation 唯一 external dependency 必须是 Windows-only、normal、nonoptional、未 rename 的：

```toml
[target.'cfg(windows)'.dependencies]
windows-sys = { version = "=0.61.2", default-features = false, features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
] }
```

这里冻结的是 foundation manifest 自己声明的两个 features；其他 workspace package 的 feature unification 不授权
foundation import 其他 Win32 module。唯一 native query 是
`GetFileInformationByHandleEx(FileIdInfo, FILE_ID_INFO)`。private query leaf 初始化固定大小 writable buffer，从 borrowed
`File` 取得 raw handle，执行一次调用，检查返回值，再复制 `VolumeSerialNumber` 与 16-byte `Identifier`；API 不保留
任一 pointer 或 handle。

F0 implementation 必须在 tracked dependency-admission record 中对实际解析的 `windows-sys` 及全部 registry
transitive packages 记录 version、source、checksum、license expression 与声明/实测 MSRV，并在固定 Rust 1.97.1 上
通过 root 与 hardware build；未声明 `rust-version` 不能当作 MSRV 证明。root 与 hardware `Cargo.lock` 都必须固定同一
受审解析及 checksum，repository guard 必须拒绝缺项、额外版本或 source 漂移。本文不预先宣称任何外部 crate 的
checksum、许可证兼容性或 MSRV 审计结论。

root `[workspace.lints.rust] unsafe_code = "forbid"` 保持不变，所有现有 root crates 与 `easycon-ecs` 继续继承 forbid。
foundation 是唯一不得继承该不可下调 workspace lint 的 package；它自身必须同时使用 `deny(unsafe_code)` 与
`deny(unsafe_op_in_unsafe_fn)`，只在一个 private query function 上局部 `#[allow(unsafe_code)]`，且只有一个
`unsafe` block。该 block 只包围上述单次 Win32 call，并带逐参数、buffer size、borrowed-handle lifetime 与 pointer
不逃逸的精确 `SAFETY` 证明。repository guard 必须拒绝第二个 allow、第二个 unsafe block、public/raw-handle API 或
其他 root/tests source 中新增同型 FFI。

### fail-closed 与信任边界

以下任一情况返回 `io::Error`，不得返回相等、fallback 或“unsupported but allowed”：

- `GetFileInformationByHandleEx` 返回失败，包括 provider/volume 不支持 `FileIdInfo`；
- `VolumeSerialNumber == 0`；
- 16-byte `Identifier` 全零；
- buffer/result 无法按 exact `u64 + u128` 解释，或 test seam 注入低质量/不完整结果。

这一定义使“低质量”成为可执行条件；不得把 identifier 的高 64 bits 为零单独判失败，因为合同信任的是成功返回且
非零的完整 16-byte field，而不是推断 filesystem 类型。loader 不探测、allowlist 或宣称 NTFS/ReFS/redirector
支持；内核成功返回并通过上述质量检查的 `FILE_ID_INFO` 是 object identity 信任边界。内核或 filesystem 返回碰撞、
虚假但非零 identity 不在用户态合同可证明范围内；这不授权 length/timestamp/path fallback。

## 迁移现有 qualification-private leaf

当前 `tests/hardware/file-id-handle` 已有 `GPL-3.0-only`、`publish = false`、exact Win32 call、两个 required features、
crate-level deny 和单一 private unsafe leaf。它是实现输入，不是 production dependency：当前 public API 会返回可复制的
identity value，hardware code 也会在 handle drop 后保存和比较该 value，因此不能原样提升为 C1 authority。

方案比较固定为：

| 方案 | 结果 | 决定 |
| --- | --- | --- |
| 在 root 新复制一个 shim，保留 `tests/hardware/file-id-handle` | 两份 unsafe authority、两套 quality/lifetime policy，后续可能漂移 | 拒绝 |
| production 直接 path-depend `tests/hardware/file-id-handle` | release ownership 反向依赖 qualification-private `tests/`，且继续暴露 detached tuple | 拒绝 |
| 把现有 leaf 迁移为 root `easycon-file-identity`，hardware 通过 package alias/path 消费 | 单一 unsafe authority；root 与 qualification 共用 borrowed-handle equality；可独立审查/refreeze | **选择** |
| 只在 C1 用 path metadata、length/timestamp、share lock 或 filesystem admission | 无法绑定实际读取对象，或无法可靠执行 admission | 拒绝 |

F0 必须迁移而不是复制：创建 `crates/easycon-file-identity` 后删除旧
`tests/hardware/file-id-handle/{Cargo.toml,src/lib.rs}` 和 hardware workspace member；production 不得依赖任何 `tests/`
path。hardware manifest 可用以下 alias 保持 qualification import ownership：

```toml
[target.'cfg(windows)'.dependencies]
easycon-hardware-file-id = { package = "easycon-file-identity", path = "../../crates/easycon-file-identity" }
```

hardware 的 source/final publication、manifest hard-link 验证和 checkpoint double-read 必须改为 borrowed two-handle
comparison。需要跨步骤比较的 `GuardedDiskFile` 必须继续拥有原 `File`，直到与另一 guarded `File` 调用
`same_file_object` 完成；不得在 hardware adapter 中重新构造 public tuple。既有错误/failpoint provider 改成返回
`io::Result<bool>` 的 safe comparison seam，不能成为第二个 identity 算法。

这会修改 `tests/hardware` executable、dependency 和 lock，按 [ADR-0011](0011-phase-2b-qualification-software-candidate-freeze.md)
明确重新打开 Phase 2B Qualification Software Candidate。F0 的 implementation review/refreeze 必须同时证明 migration
保持 no-replace publication、retained handles、artifact/checkpoint classification 与 `Hardware Unverified` 边界；不能用
“只是移动 helper”跳过 hardware workspace 全部门禁或独立 refreeze。

## C1 exact dependency boundary

只有 F0 单独 refreeze 后，C1 才能把 `easycon-ecs` 从 W0 zero-dependency 形状一次性改为：

```toml
[dependencies]
sha2 = { version = "=0.10.9", default-features = false }

[target.'cfg(windows)'.dependencies]
easycon-file-identity = { path = "../easycon-file-identity" }
```

`sha2` 是唯一跨平台 direct algorithm dependency，必须是 normal、nonoptional、未 rename、unconditional、exact
`=0.10.9` 且 `default-features = false`。`easycon-file-identity` 必须是 Windows-only、normal、nonoptional、未 rename
的 direct path dependency。`easycon-ecs` 不得直接依赖 `windows-sys`，不得出现 unsafe、raw handle 或另一 identity
crate；non-Windows C1 build graph 不消费 foundation API。

C1 implementation 必须在同一原子节点更新 root `Cargo.lock`、tracked dependency-admission record 与 repository guard。
对 exact `sha2 0.10.9` 和实际解析的每个 registry transitive package，admission record 必须逐项绑定 package/version、
source、checksum、license expression、声明的 `rust-version` 或明确的未声明状态，以及固定 Rust 1.97.1 的实际
build/test 结果；未声明 `rust-version` 不能当作 MSRV 证明。guard 必须把 record 与 manifest/lock closure 交叉验证，
精确验证两个 direct dependencies 的 kind、optional、rename、target、version/path 与 default-features 形状，并拒绝
未审 license、缺失或漂移的 source/checksum、未在 Rust 1.97.1 实测、额外版本、第二个跨平台 algorithm dependency
或第二条 identity path。上述每个字段都必须有 mutation regression；本 proposal 不预先宣称 `sha2` 或其 transitive
packages 的许可证兼容性、MSRV 或 checksum 审计已经通过，admission 未完整通过时 C1 implementation 继续被阻断。

## C1 object-authority 顺序

lexical path/limit 检查只决定允许的 prefix、component、name、count 与 byte budget，不携带 object identity，也不能把
`Path`、canonical string、`DirEntry` 或 path `Metadata` 升级为 authority。Windows loader 必须按以下唯一顺序推进：

本节所有首次 admission 与 pre-read binding reopen 必须消费同一个 exact read-only `GuardedOpenSpec`：desired access
精确为 `GENERIC_READ`，creation disposition 精确为 `OPEN_EXISTING`，share mode 精确为 `FILE_SHARE_READ`；directory
custom flags 精确为 `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS`，regular-file custom flags 精确为
`FILE_FLAG_OPEN_REPARSE_POINT`。对应 `OpenOptions` 形状只能是 read=true，write/append/create/create_new/truncate=false。
禁止 `GENERIC_WRITE`、`GENERIC_ALL`、`DELETE` 或任何 write/append/delete-child access，禁止 create/truncate disposition，
禁止 `FILE_FLAG_DELETE_ON_CLOSE`，也禁止额外会修改、截断、创建或删除对象的 access/flag；不得让普通 path open、默认
share mode 或 follow-reparse fallback 混入 authority 链。

C1 implementation guard 与 non-ignored opener spy assertion `C1-WIN-EXACT-OPEN-SPEC` 必须逐字段验证上述 spec。
mutation regression 必须分别改变 desired access、`OPEN_EXISTING`、share mode、directory/file required flags，分别打开
write/append/create/create_new/truncate，并分别加入 delete/delete-on-close；每个单字段 mutation 都必须稳定失败，不能用
A 的运行时 sharing failure 代替 exact spec admission。

1. **Lexical plan**：在 I/O 前分解显式 root/main，拒绝 escape、`.`/`..`、非法 prefix/component/name 和 limits，
   形成从 volume/share root 到显式 root、可选直属 `lib`、main 与候选 lib names 的 lexical plan。
2. **Ancestor admission**：从 volume/share root 开始，按 component 顺序对每层 ancestor、显式 root 与可选 `lib`
   directory 做 guarded nofollow/shared directory open 并 retain。每个 handle 的 metadata 才是 directory/type/reparse
   authority；每个 handle 立即通过 C1 private provider admission，production provider 必须委托 `validate_file_object`。
   任一 opaque error 全局 fail closed。
3. **Guard before enumeration**：只有 `lib` directory guard 已建立、验证并 retained 后才能 `read_dir`。enumeration
   只产生直属、受限、排序后的 names；不得读取或信任 `DirEntry::metadata`、path metadata、canonical path 或递归结果。
4. **Immediate candidate admission**：每产生一个允许的 main/lib logical name，立即按同一 guarded nofollow/shared
   policy 打开并 retain regular `File`。该 handle 的 metadata 是 regular-file、reparse 与 size-limit authority；
   production provider 委托 `validate_file_object` 的成功是 high-resolution identity admission。失败时不跳过 source、
   不形成 partial bundle。
5. **Open all before read**：所有 ancestor/root/directory/candidate retained handles 的 type、reparse、checked size 和
   high-resolution admission 全部成功之前，禁止任何 candidate read/copy/decode；任一失败时 read count 必须为 0。
6. **Pre-read binding**：在仍未读取任何 candidate 时，对 lexical plan 中每个 ancestor、root、`lib` directory、main
   与 lib logical path 再执行相同 guarded nofollow/shared open，通过 C1 private provider 比较 retained/current；
   production provider 必须委托 `same_file_object(&retained, &current)`。两个 `File` 在调用期间同时存活；`Ok(false)`
   或任一 opaque error 都使整个 load fail closed、read count 保持 0。不得以先前 metadata 或 share success 替代 equality。
7. **Retained read only**：全部 binding 成功后，只从步骤 2-4 的原 retained candidate `File` 读取 exact bounded bytes。
   read/copy/decode 开始后禁止任何 path open、metadata、canonicalize、enumerate 或 identity lookup。current binding
   handles 可以在其比较完成后释放；ancestor/directory retained guards 与 candidate share policy 继续存活到整个 load
   成功或失败，以排除比较后的 namespace retarget/mutation。

share mode 始终只是 retained interval 的 mutation exclusion，不是 identity credential。guarded open 成功不能替代
`FILE_ID_INFO` equality；identity 相等也不能替代 nofollow/type/reparse/size 检查。两者失败都统一阻止 bundle publish。

## 独立 deterministic RED

C1 必须使用可控 barrier/fault injection，禁止随机 sleep、概率性 filesystem race 或一个组合测试冒充多项证明。A-F
各自需要 exactly-one stable assertion ID、non-ignored test 和 production marker，并明确使旧 WIP Windows snapshot
稳定失败：

foundation 的 query/quality injection 只存在于 `easycon-file-identity` crate-private unit tests，不进入 public API，
也不能被 `easycon-ecs` 的 `cfg(test)` 看见。C1 另有自己的 private identity provider seam；其调用方只观察与 production
safe API 同形的 opaque `io::Result<()>` admission 与 `io::Result<bool>` comparison，不观察 `FILE_ID_INFO`、identity tuple、
raw handle 或 foundation failure subtype。production provider 只能委托 `validate_file_object`/`same_file_object`，test
provider 只能注入 opaque success/error/equal/not-equal result，不能实现第二套 identity 算法。

| RED | 确定性安排与新实现唯一结果 | 旧 WIP 稳定失败原因 |
| --- | --- | --- |
| A. real share exclusion (`C1-WIN-SHARE-EXCLUSION`) | 在真实 Windows 上用 exact open spec 分别持有 ancestor/directory/file guarded handle，barrier 内尝试 write/delete/rename；持有期必须得到 sharing failure，drop 后 control mutation 成功 | 旧 WIP 未把完整 guard lifetime 保留到 load 结束，持有期 mutation control 会成功；本项不执行 identity mismatch，不能充当 B/D |
| B. high-resolution mismatch (`C1-WIN-HIGHRES-MISMATCH`) | 预先打开并同时保留两个真实 distinct regular-file handles，令内容 length 与 timestamp 完全相同；C1 private opener 为 main/lib 的 current binding 返回 distinct handle，通过 production safe provider 调用 `same_file_object` 必须为 false，loader 全局失败且 read count=0，不注入 raw ID | 旧 WIP snapshot 只比较 length/timestamp，会稳定错误接受；本项不尝试 namespace replacement/share failure，必须独立证明 file identity |
| C. enumeration-to-open authority (`C1-WIN-OPEN-AUTHORITY`) | enumeration 只返回允许 name；barrier 让 stale path/`DirEntry` metadata 报 ordinary file，但 guarded-open handle metadata 分别注入 root/`lib`/main/lib reparse 或 wrong type；新实现全部在 read 前拒绝 | 旧 WIP 把 enumeration/path metadata 当 authority，稳定越过 barrier 并读取错误对象 |
| D. injected reopen-to-distinct-object (`C1-WIN-REBIND-DISTINCT`) | 不执行 junction/rename；C1 private opener 在 pre-read binding 时，分别为中间 ancestor、显式 root 与 `lib` 的 current reopen 返回预先打开、同 type/non-reparse 但 object identity 不同的 directory `File`，retained handle 不变。production safe comparison 必须返回 false，全局失败且 read count=0 | 旧 WIP 不对每层 retained/current handles 做 pre-read identity comparison，会稳定越过 injected reopen 并读取；本项只证明 comparison branch，不声称真实 namespace mutation 穿过 share exclusion |
| E. opaque identity failure propagation (`C1-WIN-OPAQUE-IDENTITY-FAILURE`) | 只用 C1 private provider 分别向 ancestor/root/directory/main/lib 的 admission 注入 opaque `Err(io::Error)`，并向 binding 注入 `Err` 与 `Ok(false)`；每项都必须全局失败、零 partial bundle、read count=0，不访问 foundation private seam | 旧 WIP 不调用该 admission/comparison provider；在其余输入完全合法的 fixture 上，每项注入都被稳定绕过并错误成功 |
| F. open-all-before-read (`C1-WIN-OPEN-ALL-BEFORE-READ`) | 记录每个 open/metadata/identity/read event，让最后一个 lib open 或 opaque identity admission 失败；所有先前 candidate 的 read count 必须仍为 0，handles 恰好释放 | 旧 WIP 逐文件 eager read，在最后一项失败前已稳定读取 earlier candidate |

另有 mandatory no-post-read assertion：path backend 在 first retained read 后把任何 open/metadata/canonicalize/enumerate/
identity 调用变成确定性 panic/error；合法 bundle 必须仍完成且 post-read call count 为 0。真实 Windows share/rename
exclusion 只由 A 的 real test 证明，不能由 fake 替代；D 明确是 injected opener test，不是 junction/rename 实证，也不能
被 A 的 sharing failure 代替。B/D/E 的 identity/comparison assertions 均不得以“replacement 被 share 拒绝”冒充通过。

## F0 文件、guards、门禁与 refreeze

F0 implementation node 必须先建立以下 non-ignored stable RED/assertions，再在同一最终 implementation candidate 中
使其通过。foundation query injection 只能位于 crate-private unit-test module，不能成为 public item、feature、dev-only
API 或可由 `easycon-ecs`/hardware dependency 访问的 `cfg(test)` seam：

| Stable assertion ID | F0 必须证明 | 当前 `tests/hardware/file-id-handle` 的确定性 RED 原因 |
| --- | --- | --- |
| `F0-NO-DETACHED-PUBLIC-AUTHORITY` | public API/source guard 只允许 borrowed-`File` validation/comparison；mutation 暴露 public identity struct/tuple/field/return value 时稳定失败 | 旧 helper 公开 `HighResolutionFileIdentity` 及返回该 copyable value 的函数，guard 对当前 source 必须直接失败 |
| `F0-QUERY-QUALITY-FAIL-CLOSED` | crate-private query seam 分别注入 Win32 error、unsupported、zero volume、all-zero ID；public safe APIs 每项返回 `io::Error`，并有逐字段 mutation regression | 旧 helper 只检查 Win32 return value，成功返回的 zero volume/all-zero ID 会稳定成为 `Ok(detached_identity)`，因此 assertion 失败 |
| `F0-TWO-LIVE-HANDLE-DROP-TRACE` | caller-side event/drop recorder 包住两个 borrowed `File`，固定 `left-query -> right-query -> compare -> return -> drop-current -> drop-retained`；API 返回前两者均存活，相同 object 为 true、distinct object 为 false | 旧 helper 只有 single-handle query，现有测试在每次调用后立即 drop 临时 `File` 再比较 values，无法产生 two-live-handle trace |
| `F0-ROOT-DIR-FILE-BORROWED-HANDLES` | 真实 Windows volume/share root、普通 directory、regular file 都以 nofollow retained handles 覆盖 same/distinct/error，不用 regular-file mock 代替 | 旧 helper 只测试 path-opened regular files 并返回 detached values，缺少 root/directory retained-handle assertions |
| `F0-HARDWARE-OWNERSHIP-MIGRATION` | old workspace member 已删除，hardware alias/path 指向唯一 root crate；publication、manifest、checkpoint paths 的 owner 都保留两个 `File` 到 shared comparison 完成，source/guard mutation 回归禁止缓存 tuple | 当前 member 仍存在，`GuardedDiskFile` 只保存 bytes + detached identity，publication 也先取两个 values 再比较，source/manifest assertion 稳定失败 |

这些 RED 的失败必须来自表中指定合同，不得以“新 crate 尚不存在”的 compile failure 冒充。尤其 query-quality RED 与
C1 opaque propagation RED E 属于不同 crate、不同 seam 和不同 assertion ID，不能交叉代替。

F0 是一个独立 implementation node，至少拥有：

- `crates/easycon-file-identity/{Cargo.toml,src/lib.rs}` 及其 Windows unit/integration tests；
- root `Cargo.toml`、`Cargo.lock` 和 `tools/check_repository_guards.py` 的同一原子 workspace/guard 更新；
- 删除 `tests/hardware/file-id-handle`，更新 `tests/hardware/{Cargo.toml,Cargo.lock}` 的 alias/path；
- 更新 `tests/hardware/src/artifact.rs` 及相关 unit/integration tests，使全部 publication、manifest hard-link、checkpoint
  double-read equality 在两个 retained handles 同时存活时调用共享 safe API。

proposal baseline 的 `tools/check_repository_guards.py` 只提供现有 root workspace/W0 等冻结项的基线保护；它没有验证
F0 crate、hardware manifest/lock alias、旧 helper removal、唯一 unsafe leaf、borrowed public surface 或上述 stable
assertions。当前 guard 通过不能作为这些未来合同的证据。F0 必须先扩展 guard 和 guard 自身 mutation regressions，之后
future F0 candidate 才允许用该扩展后的通过结果作为 evidence。

扩展后的 F0 guard 必须验证：root expected member 精确增加 foundation；package 是 `GPL-3.0-only`、`publish=false`；
foundation manifest 的 `windows-sys = "=0.61.2"` target/kind/optional/rename/default-features/two-feature 集合精确；F0
dependency-admission record 与两份 lock 的 direct/transitive version/source/checksum/license/MSRV evidence 精确匹配；
唯一 private unsafe leaf、allow/block 与 SAFETY comment；无 raw/public identity value；hardware alias 精确指向 root
package；旧 private leaf/第二个 FFI 不存在；F0 stable assertion IDs 全部存在且 non-ignored；F0 阶段 `easycon-ecs`
依赖仍为空。每个字段都要有 mutation regression。C1 后续再把 guard 原子升级为上述 exact `sha2` admission + Windows
path dependency + exact opener spec，不能在 F0 提前放开。

F0 implementation commit 前必须通过受控 Windows Workspace 入口执行 root 全部门禁：

```text
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python tools/validate_specs.py
python tools/check_markdown_links.py
python tools/check_repository_guards.py
git diff --check
```

随后还必须在 `tests/hardware` 工作目录依次通过：

```text
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

随后固定 F0 implementation SHA/tree/parent，由独立 reviewer 对 root foundation、unsafe containment、hardware migration、
两套 locks/guards/tests 做完整只读 review；任何修复产生新 SHA 并重跑两套门禁。最后必须用单独 refreeze 同时冻结
foundation source candidate 和被 F0 executable/dependency 变更重新打开的 Phase 2B software candidate。只有该 refreeze
完成，C1 dependency/loader/RED 节点才可启动。

## 唯一实施 DAG

```text
F0 stable RED/assertions + dependency admission + migration implementation
  -> full root Workspace + hardware workspace gates
  -> one F0 implementation commit
  -> fixed-SHA independent implementation review
  -> separate foundation + Phase 2B software refreeze
  -> C1 exact dependency/opener guard RED + sha2 admission
  -> C1 manifest/lock + retained loader + A-F RED + Workspace gates
  -> one C1 implementation commit
  -> fixed-SHA independent C1 implementation review
```

不得把本 acceptance、F0 implementation、F0 refreeze 或 C1 implementation 合并为同一提交，也不得让 C1
临时复制 FFI 等待 foundation。F0 不实现 lexer/loader；C1 不修改 foundation unsafe leaf 或 hardware qualification。

## 本 acceptance 的 docs-only 门禁

本 acceptance 只修改 ADR-0021 与原三处状态索引，并运行：

```text
python -B tools/check_markdown_links.py
python -B tools/check_repository_guards.py
git diff --check
git diff --cached --check
```

这些命令只证明 acceptance 文档引用、现有 repository guard 基线仍通过和 diff hygiene。当前 guard 不验证
`tests/hardware` manifest/lock/helper identity 合同，也不存在 F0 crate、expanded guard 或本 ADR 冻结的 stable
assertions；因此本次 guard 结果不能证明任何 F0/hardware migration 项。它也不证明 foundation/`sha2` dependency
admission、unsafe correctness、MSRV compile、loader behavior、RED、Rust、Workspace、conformance、CI、hardware 或发布
已经通过。

## 关联

- [Phase 4 ECS 与 Automation 目标](0017-phase-4-ecs-automation-target.md)
- [Phase 4 C1 lexer 合同](0019-phase-4-c1-lexer-contract.md)
- [Phase 2B Qualification Software Candidate](0011-phase-2b-qualification-software-candidate-freeze.md)
- [Phase 2B run directory ownership](../development/phase2b-run-directory-ownership.md)
- [目标仓库与实施路线](../architecture/repository-roadmap.md)
