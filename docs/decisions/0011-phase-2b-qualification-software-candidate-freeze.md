# 0011：冻结 Phase 2B Qualification Software Candidate

- 状态：Frozen (`Hardware Unverified`)
- 日期：2026-07-21
- Phase 2A 基线：`202b2610ffc836f21951e18f69fa98b46d6efc90`
- Phase 2B 原始交接基线：`15bd6c762323c34c73956daa2be096442d7724b7`
- 本轮只读审查基线：`9944dba50adc34484b65206e07ea0a444103f656`
- 软件实现冻结基线：`eef1ed35c172fe31e805d634aa324170856e782f`
- 软件实现 tree：`e53d74f25566fa77da42982925462f8335d35ca6`
- 完整审查范围：`202b2610ffc836f21951e18f69fa98b46d6efc90..eef1ed35c172fe31e805d634aa324170856e782f`
- 上位目标：[ADR-0010](0010-phase-2b-qualification-evidence.md)

## 决策

将上述实现 SHA 冻结为 **Phase 2B Qualification Software Candidate**。该候选已经具备 ADR-0010 要求的
软件接纳、安全执行、持久 evidence、operator/cancellation、Amiibo 写前保护、telemetry 和 checkpoint 能力；
在当前证据下，没有未解决、可复现且可行动的 in-scope P0/P1/P2 软件 finding。

本决定只冻结资格软件候选，不冻结完整 Phase 2，不创建受支持设备行，不关闭 O-01、O-02、O-04，也不声明
任何控制板、固件、VID/PID、baud、Amiibo 容量、UART/USB/Switch 时序或物理中立化已经通过。包含本 ADR 的
后续纯文档提交不改变上述软件实现基线；硬件 evidence 必须同时记录实际 build commit 和本候选基线。

## 输入与边界

原始交接输入为 `easycon-sdk-phase2b-handoff-20260720T231200+0800-final.zip`，复核 SHA-256 为
`45379400BC65D9DD65F436DF8B9C40325114883385895707C1CCD28CFD8168F3`。它只作为历史交接输入，不升级为
本机物理复测或当前硬件结论。

资格工具继续位于独立、`publish = false` 的 `tests/hardware` workspace，不进入根 workspace、release feature、
native bundle 或公共 API。`EasyCon/` 未进入 tracked tree、workspace 或 build graph；外层许可证保持
`GPL-3.0-only`。本轮没有配置 remote、push、PR、历史改写、固件动作、串口 open/write 或真实 Amiibo 写入。

## 冻结的软件范围

| 能力 | 冻结内容 | 主要实现提交 |
| --- | --- | --- |
| runner 与 cleanup | 普通命令/faults 的单 owner、动态资源角色、唯一 completion path、Controller/Runtime 显式 close | `59a312d`、`92928b1`、`818fdf6` |
| device admission | typed expected stable identity、initial admission、每次 native open 的 pre/post guard、hotplug identity rebind、显式 diagnostic delay | `ab3593b`、`efab3c0`、`0ce3e45` |
| durable evidence | build/runtime provenance、append-only synced journal、final/CSV/manifest/completion 无覆盖事务、磁盘分类与 retained file identity | `07bb79e`、`5d160a7`、`1e6391a` |
| operator/cancellation | 有界单 owner `OperatorPort`、逐动作 observation、Ctrl+C token、operation cancel、neutralize/close/cancelled artifact 顺序 | `baa691a`、`5fe4d61` |
| Amiibo safety | 默认不写、一次性 slot-bound 授权、declared limits source、payload hash、chunk/save/select/cleanup durable progress | `ce0139d`、`c889e3c` |
| telemetry/status | logical report first-entry/final-acceptance、partial/native error、checked time、RFC CSV、唯一 outcome 三元组和物理未验证投影 | `594c7f3` |
| checkpoint | 磁盘 evidence 重验、Observed/Attested/Unverified/Failed/NotRun 五类、strict attestation、`Hardware Unverified` transaction | `eff496f` |

Home prelude 仍只是显式、identity-bound diagnostic step；UART 数值仍是 `measured=false` 的理论值；没有
analyzer 或可审计 firmware trace 时 USB HID 和 Switch physical order 始终为 `unverified`。Amiibo declared limits
不会因一次短写升级为 capability，checkpoint 的 `Observed` 也不等于 supported。

## 独立整体审查与修复

审查固定 `9944dba` 为只读起点，逐提交和组合检查 `202b261..9944dba`，随后每个软件节点在完整门禁后形成新
基线。最终再次审查 `202b261..eef1ed3` 的 60 个提交；60/60 均通过 `git show --check`。审查按可复现 bug、
硬件未知、架构决定和未来加固分类，O-01/O-02/O-04、缺少目标设备和已登记的 nominal-directory
hard-link/junction backlog 没有被误报为当前软件 bug。

| ID | 严重性 | 旧实现确定性复现与影响 | 回归与修复 |
| --- | --- | --- | --- |
| Q-01 | P2 | cancel fault 接受 `post_cancel accepted_report_count >= expected`，因此一个未解释的额外 report acceptance 仍可能被投影为合格。 | `faults_require_nonvacuous_cancel_evidence` 增加 extra-acceptance case；`d48c04b` 改为精确计数。 |
| Q-02 | P2 | journal 在参数校验前保存 normalized arguments；无效 absolute `--limits-source` 或游离绝对路径会进入失败 run，Windows drive-relative source 也能绕过 `Path::is_absolute`。两条旧实现回归稳定失败。 | `normalized_arguments_redact_rejected_and_unbound_machine_paths`、`limits_source_rejects_windows_drive_relative_paths`；`eef1ed3` 统一拒绝/脱敏 absolute、rooted 和 drive-prefix 路径。 |

Q-02 修复后以 `eef1ed3` 为新基线复审 identity、授权 binding、journal、artifact、checkpoint 和文档投影，没有
发现新的 in-scope P0/P1/P2。理论攻击、风格建议和不改变当前契约真实性/资源安全的加固没有扩大本冻结范围。

## 验证证据

最终实现按顺序通过根 workspace：

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

并在 `tests/hardware` 独立 workspace 通过：

```powershell
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

- 根 workspace：157 个非文档测试通过，0 failed、0 ignored；独立 Runtime Loom 6/6 通过。
- hardware workspace：file-id 1、library 2、CLI/unit 153、artifact integration 4、checkpoint integration 2、
  qualification-status integration 1，共 163 个测试通过，0 failed、0 ignored。
- 规范：5 schemas、1 behavior、3 controller fixtures、9 scenarios、65 个 exact Rust tests。
- 实现基线 Markdown：136 个引用、33 个文件；包含本冻结 ADR 后的文档门禁为 146 个引用、34 个文件；
  repository guards 和最终 diff check 通过。
- `tests/hardware/Cargo.lock` SHA-256：
  `F4AF3D9A0122DDDB0EB815404CE2468D7EC03725BE1313026FFB7AC3EC1869E1`。

干净 `eef1ed3` 上另运行一次只含空 synthetic runs root 的 `checkpoint`。它没有调用 discovery、Harness 或
operator observation，输出固定为 `completed/unverified/2` 和 `Hardware Unverified`，且 build provenance 为：

- commit `eef1ed35c172fe31e805d634aa324170856e782f`；
- tree `e53d74f25566fa77da42982925462f8335d35ca6`；
- `tracked_dirty=false`、`untracked_present=false`、`trusted=true`；
- tracked source SHA-256 `9D2FE8568BD951E30FB726CA2466D05A3EA1C9AFCAF3F816FD8F0CCB3B20CAF0`；
- executable SHA-256 `7DE82C2CC561F3FDD317B8DF068298642A86EF0B84B3551D0D5DC669C0F3331D`。

这些值只证明本次软件构建来源和 checkpoint 路径，不是硬件 evidence。资格 CLI 的 `discover`、`handshake`、
`smoke`、`home-wake`、`faults`、`hotplug`、`lifecycle`、`sequence` 和授权 `amiibo` 命令均未用于形成当前冻结结论。

## 未完成的硬件项

- **O-01**：目标板型和固件版本、stable identity/VID/PID、115200/9600、真实 discovery/handshake、fault、
  hotplug、100-cycle、关闭和重新接入行为。
- **O-02**：外部可审计的 slot count/maximum length、一次性 disposable slot 授权后的真实 save/select、各失败点
  恢复和设备侧容量结论。在 O-02 关闭前仍默认不写。
- **O-04**：逻辑分析仪或可审计 firmware trace 下的 UART frame、CH32、USB HID、Switch physical order、
  连续节拍、物理 10,000-step 和端到端延迟。
- 每个要求可见判断的 action 仍需真实 operator observation；历史 handoff 只能作为 `Attested`，不能伪装成
  当前 run 的 `Observed`。

硬件电脑必须保存 exact build provenance、expected stable identity、原始 journal/final/CSV/manifest/completion
以及五类 checkpoint。上述证据全部满足 ADR-0008/ADR-0010 且经新的独立 review 前，不得创建
`hardware/matrix.yaml`、受支持设备行或完整 Phase 2 冻结 ADR。

## 重新打开规则

以下任一情况重新打开本软件候选，直到最小回归、两套完整门禁、新实现 SHA 和独立 review 再次完成：

- 修改 `tests/hardware` 可执行行为、依赖、Cargo lock、状态三元组、CLI 参数或 artifact schema；
- 放宽 stable identity 写前接纳/pre-post guard、single owner cleanup、Ctrl+C cooperative close、Amiibo
  slot/hash/authorization 或无覆盖 evidence transaction；
- 改变 telemetry first-entry/final-acceptance、native error、checked time 或 physical-unverified 边界；
- checkpoint 不再从磁盘重算、合并五类、允许 untrusted pass、关闭 O 项或创建支持矩阵；
- production Controller/Runtime/Serial 变化影响 qualification adapter，或硬件结果证明当前软件声明不实；
- 出现新的、可复现且可行动的 in-scope P0/P1/P2 finding。

只修改不影响 executable/source contract 的说明，或追加不改变软件声明的硬件原始 evidence，不自动重新打开
`eef1ed3`；但实际运行必须记录其真实 build commit。任何 executable-affecting descendant 都不能继续冒充本候选。

## 关联

- [ADR-0008：冻结 Phase 2 Controller/Serial 开发目标](0008-phase-2-controller-target.md)
- [ADR-0009：冻结 Phase 2A Controller/Serial Candidate 基线](0009-phase-2a-freeze.md)
- [ADR-0010：冻结 Phase 2B 硬件资格工具与证据目标](0010-phase-2b-qualification-evidence.md)
- [Phase 2B checkpoint 软件收口设计](../development/phase2b-checkpoint-software-closeout.md)
- [实施路线](../architecture/repository-roadmap.md)
- [测试策略](../architecture/testing-strategy.md)
