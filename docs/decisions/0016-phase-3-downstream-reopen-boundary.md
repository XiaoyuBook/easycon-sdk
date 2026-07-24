# 0016：澄清 Phase 3 候选的下游阶段重新打开边界

- 状态：Proposed Refreeze（待独立治理审查）
- 日期：2026-07-23
- 治理基线：`origin/main@bc24f0bfe65a34ba54ffacad62dd41905c77f952`
- Phase 3 implementation：`27444f16d0625a7ab7e4e1543736c7c3c225ce8a`
- implementation tree：`334a3fb21bf4d6ca55e0bcbe3e2daa6e675b3194`
- implementation parent：`76436de98836dc2fff9a43c4560e7dc7f3fea780`
- 被澄清记录：[ADR-0014](0014-phase-3-native-pool-admission-refreeze.md)
- 编号说明：ADR-0015 已保留给 Phase 2B refreeze；本提议落盘前已扫描全部本地 refs 与 worktrees，后续可用编号为 0016

## 状态与生效条件

本 ADR 是语义完整的 successor/refreeze **提议**，不是已经完成的重新冻结。ADR-0014 的现行“重新打开规则”
明确把“增加 public ABI、Phase 4/5/6、binding、package、support row 或发布声明”列为任一即触发的条件；该文本
没有纯下游消费豁免。因此，准备增加 Phase 4 target 按现行字面规则确实重新打开 Phase 3 候选，不能追溯声称
ADR-0014 原本允许该例外。

在固定本提议提交完成独立治理审查、清零可复现且 in-scope 的 finding，并由后续提交记录 refreeze 证据之前：

- ADR-0014 的原文和历史重新冻结证据保持权威且不被改写；
- Phase 3 implementation 继续固定为 `27444f16`，但本次治理 reopen 尚未闭合；
- 本提议不授权 Phase 4 target、workspace scaffold、Runtime 或 Controller 实现；
- 后续提交不得把本文件的 `Proposed Refreeze` 状态描述为最终 freeze/refreeze。

独立审查通过后的后续 tracked 提交才可把本 ADR 更新为最终 refreeze 状态；该 tracked 提交只在本 ADR 中记录
固定的 proposal SHA、review task、精确门禁结果、最终状态与索引推进。该 refreeze commit/tree 只能在提交后由
Git 对象和外部结构化 `TASK_REPORT`/handoff 固定，不写回任何 tracked ADR。

## 已核验且不变的 Phase 3 基线

本提议不产生新的 Phase 3 implementation。Git 对象核验确认：

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

本提议不新增或晋级 backend、hardware、public ABI、binding、package、support row 或发布声明，也不关闭 O-03。
`EasyCon/` 继续只读、ignored，不是 tracked source、fixture、build、bundle 或发布输入。

## 提议决定

独立审查和最终 refreeze 完成后，仅取代 ADR-0014 重新打开规则中把“增加 Phase 4/5/6”本身视为无条件触发的
部分。ADR-0014 的 implementation、历史 finding、门禁证据、平台分级和其余 reopen 条件全部保留。

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

## 本提议的验证与最终 refreeze gate

本地 proposal 提交前必须在包含本文件及索引差异的工作树上通过 ADR-0012/0014 要求的完整软件门禁：

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

同时必须执行 provisioned OCR model、MSVC Debug/Release、clang-cl ASan/UBSan trap、MSVC analyze、clang-tidy
warnings-as-errors 和 clang-cl libFuzzer tracked-corpus replay。全部命令只验证 software/native 与 synthetic/fixture
路径，不运行串口命令、物理硬件或资格 CLI。

提交后仍有以下独立 gate，当前线程不得代替或启动：

1. reviewer 固定本 proposal commit SHA/tree/parent 并审查完整 `bc24f0b..proposal` docs diff；
2. reviewer 核验本节门禁证据、Phase 3 owned trees、public-neutral contract 与平台状态没有变化；
3. reviewer 清零直接相关、可复现且 in-scope 的 P0/P1/P2；有 finding 则在新 proposal SHA 重审；
4. 单独后续提交记录 review/refreeze 证据并更新本 ADR 状态；完成前不得开始 G0b 或声称治理 reopen 已关闭。

## 关联

- [ADR-0012：Phase 3 Vision 与跨平台私有 native bridge 开发目标](0012-phase-3-vision-native-target.md)
- [ADR-0013：第一次 Phase 3 Vision 跨平台源码候选冻结](0013-phase-3-vision-cross-platform-source-candidate-freeze.md)
- [ADR-0014：Phase 3 NativePool admission 修复后重新冻结](0014-phase-3-native-pool-admission-refreeze.md)
- [Phase 3 Vision native 设计](../development/phase3-vision-native-design.md)
- [Phase 3 Vision implementation log](../development/phase3-vision-implementation-log.md)
- [Phase 3 Linux/macOS 验证 handoff](../development/phase3-cross-platform-validation-handoff.md)
- [测试策略](../architecture/testing-strategy.md)
- [实施路线](../architecture/repository-roadmap.md)
