# 0016：澄清 Phase 3 候选的下游阶段重新打开边界

- 状态：Refrozen Governance Boundary
- 提议日期：2026-07-23
- 重新冻结日期：2026-07-24
- 治理基线：`origin/main@bc24f0bfe65a34ba54ffacad62dd41905c77f952`
- Phase 3 implementation：`27444f16d0625a7ab7e4e1543736c7c3c225ce8a`
- implementation tree：`334a3fb21bf4d6ca55e0bcbe3e2daa6e675b3194`
- implementation parent：`76436de98836dc2fff9a43c4560e7dc7f3fea780`
- 固定 proposal：`e3f0df9865119877b95732d2a6100ef15ed7ab9b`
- 独立治理复审：任务 `019f91fe-0f5a-7d71-a216-46312ecf721e`，结论 `APPROVE`，P0/P1/P2 均为空
- 被澄清记录：[ADR-0014](0014-phase-3-native-pool-admission-refreeze.md)
- 编号说明：ADR-0015 已保留给 Phase 2B refreeze；本提议落盘前已扫描全部本地 refs 与 worktrees，后续可用编号为 0016

## 状态与生效条件

ADR-0014 的现行“重新打开规则”明确把“增加 public ABI、Phase 4/5/6、binding、package、support row 或发布声明”
列为任一即触发的条件；该文本没有纯下游消费豁免。因此，准备增加 Phase 4 target 按现行字面规则确实重新打开
Phase 3 候选，不能追溯声称 ADR-0014 原本允许该例外。

固定 proposal 已完成独立治理复审并清零可复现且 in-scope 的 P0/P1/P2。本次 tracked refreeze 提交闭合上述
Phase 3 governance reopen；从该提交之后，本 ADR 的 successor 重新打开规则生效：

- ADR-0014 的原文和历史重新冻结证据保持权威且不被改写；
- Phase 3 implementation 继续固定为 `27444f16`，本次只重新冻结治理边界；
- 本 ADR 不授权 Phase 4 target、workspace scaffold、Runtime 或 Controller 实现；
- 后续工作仍须以实际 diff 和合同审查证明“纯下游”，不能仅凭阶段名称获得豁免。

本次 tracked 提交只在本 ADR 中记录固定 proposal SHA、review task、精确门禁结果、最终状态与索引推进。该
refreeze commit/tree 只能在提交后由 Git 对象和外部结构化 `TASK_REPORT`/handoff 固定，不写回任何 tracked ADR。

## 已核验且不变的 Phase 3 基线

本次 refreeze 不产生新的 Phase 3 implementation。Git 对象核验确认：

| 固定项 | `27444f16` | `bc24f0b` | 结论 |
| --- | --- | --- | --- |
| `crates/easycon-vision` tree | `1ee30940dce54e043ea56e7e4327542f85735d9a` | 同左 | 未改变 |
| `crates/easycon-native-sys` tree | `f2f5006473aa3cb8a377d89d86b09c6c563927c1` | 同左 | 未改变 |
| `native/bridge` tree | `6347ab7708ce00fae8580784be92803d70c52e50` | 同左 | 未改变 |
| `spec/fixtures/vision` tree | `a1e2adbde665452fe8d4b45238c22cedac550204` | 同左 | 未改变 |

`Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`CMakeLists.txt`、`CMakePresets.json`、vcpkg manifests/triplets
及 Phase 3 fixture generators/provisioner 在 implementation 与治理基线之间也没有差异。当前基线相对
implementation 的提交只增加或修改后续 docs、CI/workflow 与 repository guard；Phase 3 production、fixture、
private native ABI、toolchain selection 和 executable contract 均未改变。

Phase 3 public-neutral contract 继续只包括已经冻结的 immutable `Image`/`Frame`/`Label`、Capture 五态与确定性
owner cleanup、`0.0..1.0` Vision score、private native boundary，以及 typed Vision result/error 向既有 generic
Runtime Operation 的单向投影。它不是 public C ABI，也不包含 ECS integer rounding、Automation Run、binding、
package 或 release API。

平台、支持和发布状态保持不变：

| 平台 | 软件/build 状态 | 硬件状态 | 发布状态 |
| --- | --- | --- | --- |
| Windows 10/11 x64 | Phase 3 Vision software/native `Passed` | `Hardware Unverified` | 不发布；无 capture hardware 支持行 |
| Linux x64 | Vision `Candidate / Build Unverified` | `Hardware Unverified` | 不发布；不是完整 Linux SDK |
| macOS Apple Silicon arm64 | `Experimental Source Candidate / Build Unverified` | `Hardware Unverified` | `Not Shipped`；无 Intel/universal 声明 |

本次 refreeze 不新增或晋级 backend、hardware、public ABI、binding、package、support row 或发布声明，也不关闭 O-03。
`EasyCon/` 继续只读、ignored，不是 tracked source、fixture、build、bundle 或发布输入。

## 重新冻结决定

从本次 tracked refreeze 提交之后，本 ADR 仅取代 ADR-0014 重新打开规则中把“增加 Phase 4/5/6”本身视为
无条件触发的部分。ADR-0014 的 implementation、历史 finding、门禁证据、平台分级和其余 reopen 条件全部保留。

未来 Phase 4/5/6 的纯下游增加，只有实际改变以下任一 Phase 3 冻结面时才重新打开 Phase 3：

- Phase 3 candidate scope、依赖方向、Rust/C++ ownership 或 public-neutral contract；
- Vision/native production source、private C layout/ownership、fixture/spec、executable contract 或 lifecycle；
- Capture、NativePool、OCR pool、cancel/deadline、panic/Drop、close/join、limits 或错误投影；
- Rust/CMake/vcpkg/OpenCV/Tesseract/Leptonica toolchain selection 或 Phase 3 质量门禁；
- 平台 backend、fail-closed 行为、build/hardware 证据等级、support row、package 或发布声明；
- 新的、可复现且可行动的 in-scope P0/P1/P2 finding。

以下纯下游工作本身不再自动重新打开 Phase 3，但必须遵守各自阶段的 ADR、测试和 review：

- 新增只消费既有 public-neutral contract 的 Phase 4/5/6 ADR、spec、fixture 或 production crate；
- 通过 port/adapter 投影既有 Vision 结果，不修改 Vision/native owned file、生命周期或错误权威；
- 仅为登记下游 workspace member 修改根 Cargo/guard，且不改变 Phase 3 member、feature、依赖、toolchain 或 build；
- 在下游 ABI/binding/package 中投影既有合同，且不把 Phase 3 的平台、硬件、支持或发布状态升级。

“纯下游”必须由实际 diff 和合同审查证明，不能只凭文件归属或阶段名称声明。只要下游工作需要修改任一上述
Phase 3 冻结面，就必须先按本 ADR 与 ADR-0012/0014 重新打开、运行受影响与完整门禁、固定新 SHA，并接受新的
独立 review。

## 明确排除

本 ADR 不决定或描述：

- Phase 4 ECS grammar、ProgramHash、source bundle、PRNG、TIME/WAIT、Unicode、rounding 或 `EcsLimitsV1`；
- Automation `RunFailure`、OutputPort、RunCompletion、cleanup precedence 或具体 Runtime terminal API；
- Controller lease acquire、neutralize/release completion 或 ADR-0009 支线；
- `easycon-ecs`/`easycon-sdk` scaffold、Cargo/workflow/CI/ruleset、production/test code 或 fixture；
- public C ABI、四语言 binding、package、硬件资格或支持矩阵晋级。

这些事项分别属于后续 G0b、R0、D0 或实现节点，不能混入本次 Phase 3 治理 successor。

## Proposal、独立复审与本次 refreeze 证据

固定 proposal 的证据链在 proposal 阶段实际运行了 ADR-0012/0014 要求的完整软件矩阵，最终结果为：

| Proposal gate | 实际结果 |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --workspace --all-targets` | Passed，59.28 秒 |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 修正环境后的离线重跑 Passed，37.2 秒 |
| `cargo test --workspace --all-features` | Passed，51.6 秒；全部测试及 2 个 compile-fail doctest 通过 |
| `python tools/run_runtime_models.py` | Passed，6/6 Loom models |
| `python tools/validate_specs.py` | Passed：5 schemas、1 behavior spec、3 controller fixtures、15 Vision binary fixtures、1 capture manifest、24 label corpus entries、9 scenarios、65 exact Rust tests |
| OCR provisioner/hash | Passed：`eng.traineddata` SHA-256 `7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2`；`LICENSE` SHA-256 `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30` |
| MSVC Debug / Release configure、build、CTest | Passed，各 2/2 |
| clang-cl ASan / UBSan trap configure、build、CTest | Passed，各 2/2，无 sanitizer violation |
| MSVC `/analyze /analyze:external- /WX` build、CTest | Passed，2/2 |
| clang-tidy warnings-as-errors | Passed，10 个 owned translation units 无 finding；CTest 3/3 |
| clang-cl libFuzzer tracked-corpus replay | Passed，CTest 3/3，包含 128-run replay |
| Markdown links / repository guards / diff checks | Passed，links 221 references / 43 files；guards、worktree/cached/post-commit diff checks 均通过 |

Proposal 阶段首次 strict clippy 的 fresh native configure 因共享 registry 元数据损坏且网络不可达而失败；该次失败
没有被计为 Passed。随后使用同一固定 vcpkg checkout 的 ports 和只读 binary cache 恢复相同依赖版本/ABI，离线重跑
通过。OCR 首次因输出路径不符合 ignored cache 边界被拒绝，随后下载超时；从已核验冻结副本填充合法 ignored cache
后，provisioner 按 manifest 重新验 hash 并通过。上述软件、native 与 synthetic/fixture 证据没有运行硬件、串口或
qualification CLI。

固定 proposal `e3f0df9865119877b95732d2a6100ef15ed7ab9b` 是移除 tracked refreeze SHA 自指的 docs-only 修订；该修订
实际运行并通过 links 221/43、repository guards、提交前 diff check，以及 parent/range 提交后 diff checks。它没有
重跑 Rust、Loom、spec、OCR 或 native 矩阵，也不把前述 proposal 矩阵描述为该三行修订刚刚执行。

独立治理复审任务 `019f91fe-0f5a-7d71-a216-46312ecf721e` 固定并核验 proposal SHA/tree/parent、父链、完整三文件
docs diff、Phase 3 owned trees、工具链/fixture generator、无 public ABI 状态和三平台分级；独立运行 links 221/43、
repository guards、parent/baseline/worktree diff checks 和 Git object/fsck 检查均通过，最终 P0/P1/P2 为空。该复审
按 docs/governance 范围没有重复 native 矩阵，并明确继续引用 proposal 阶段的完整实际证据。

本次 refreeze 提交仅修改 ADR 与索引，不影响可执行行为；本次实际运行结果：

- `python tools/check_markdown_links.py`：Passed，221 references / 43 files；
- `python tools/check_repository_guards.py`：Passed；
- `git diff --check`：Passed。

## 关联

- [ADR-0012：Phase 3 Vision 与跨平台私有 native bridge 开发目标](0012-phase-3-vision-native-target.md)
- [ADR-0013：第一次 Phase 3 Vision 跨平台源码候选冻结](0013-phase-3-vision-cross-platform-source-candidate-freeze.md)
- [ADR-0014：Phase 3 NativePool admission 修复后重新冻结](0014-phase-3-native-pool-admission-refreeze.md)
- [Phase 3 Vision native 设计](../development/phase3-vision-native-design.md)
- [Phase 3 Vision implementation log](../development/phase3-vision-implementation-log.md)
- [Phase 3 Linux/macOS 验证 handoff](../development/phase3-cross-platform-validation-handoff.md)
- [测试策略](../architecture/testing-strategy.md)
- [实施路线](../architecture/repository-roadmap.md)
