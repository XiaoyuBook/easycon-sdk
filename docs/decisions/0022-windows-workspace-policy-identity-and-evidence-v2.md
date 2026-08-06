# 0022：Windows Workspace policy identity 与 evidence v2 合同

- 状态：Accepted / Effective
- proposal/decision 日期：2026-08-06；这不是任何 Setup、Verify、Workspace 或 review 的执行时间
- proposal 基线：`21871d733b0672092f91e3919d14c85df55846c9`

## 背景

Windows 受控入口同时承担两类彼此不同的身份：prepared environment 的可复用构建输入，以及一次
Workspace gate 执行的可审计 policy。前者不应因 runner 的调用层文本变化而无条件重建；后者必须能证明
实际使用的是固定的 gate 集、Cargo 并行度和 wrapper 语义。原有 candidate evidence 也只绑定 tree，不能
完整表达 policy、Verify 与逐 gate timing。

## 决定

### Prepared environment identity

`tools/windows_build_environment.json` 升级为 schema v5。其 fingerprint input 精确包含 workspace
manifests 中的 `crates/easycon-file-identity/Cargo.toml`，使该 crate 的依赖或构建合同变化选择新的 prepared
environment。`tools/run_windows_workspace.ps1` 不再是 prepared identity 的输入：它不物化 vendor、native tree
或受控工具；其 gate 语义改由本 ADR 定义的 policy identity 覆盖。

PowerShell parser 与 Python repository guard 都必须按 exact path、kind、顺序和 Windows path identity 验证
v5 清单。旧 schema stamp 不迁移为命中；Verify 发现 identity 不兼容时 fail closed，只有受控 Setup 可以发布新环境。

### Gate policy identity

`tools/windows_gate_policy.json` 是 version 1 的固定 gate 清单：gate 名称、顺序、tool、参数和
`cargoJobs=4` 都由私有 policy parser 逐项验证。所有适用的 Cargo build gate 自动使用 `--jobs 4`；Targeted
调用方不能通过 Cargo 参数覆盖该预算。公开模块仍只导出 Setup、Verify 与 Workspace，不接收外部 `GateInvoker`。

policy hash 使用 `tools/windows_gate_policy.json`、`tools/windows_gate_policy.ps1` 与
`tools/run_windows_workspace.ps1` 的 strict UTF-8、无 BOM、无换行归一化的 source-byte hash。policy script 在自身最早
执行阶段从已解析的完整 PowerShell AST 捕获文本和文件作用域，再由私有 helper 固化该 script snapshot；direct module import
同时捕获 runner 物理输入作为 fail-closed 默认值。runner 则在 import 前从其已解析 AST 捕获自身文本，并只通过 module private
scope 替换该默认快照。Workspace 解析 JSON、计算 policy hash 都只能使用同一组三输入快照；随后在进入 lifecycle 前立即重读
三个物理输入并比较 hash。捕获期间或此后任一次复核发现变化均 fail closed：capture-time 与 Verify 后边界启动零 gate，
最后一个 gate 后边界不发布 passed evidence 或 passed workspace record。
runner 的 `-Mode`、内部 `-GateMode` 均只接受精确大小写的允许值，不能以 PowerShell 的大小写宽容绕过该 policy。

### Candidate 与 evidence

candidate 仍只由 `-RequireCleanTree` 或 `-RequireStagedCandidate` 之一产生。gate 后重新绑定 HEAD、状态快照和
tree 后才能发布；`BaseSha` 在 gate 前解析为 immutable full commit。普通 Workspace 只输出
`credential=none` 的结构化 summary，不写可复用 tree credential。

candidate 成功时，evidence 使用 UTF-8 无 BOM schema v2，并发布到当前 worktree writable root 的：

```text
evidence/v2/workspace-<candidate-mode>-<tree>-<lowercase-run-id>.json
```

记录包括 environment fingerprint、gate policy hash、Verify duration、ordered passed gate timings、candidate/base/head/tree、
environment/workspace identity、UTC 时间和总 duration。发布使用同目录 `CreateNew` temporary 加 no-replace move；已有
destination 不得覆盖，失败不能遗留 temporary、passed JSON 或 passed `EASYCON_WORKSPACE` record。source tree 永不接收
evidence。

## 测试与门禁

Windows contract tests 必须覆盖 v5 清单缺少 file-identity manifest 的 provision 前拒绝、lowercase mode 拒绝、两个
clean fixed-SHA worktree 使用同一 v5 配置得到同一 prepared identity、JSON/policy script/runner 三个输入分别在快照
捕获时变化的零 Verify/零 gate fail-closed 行为、strict UTF-8 BOM 拒绝和 LF/CRLF byte identity，以及 policy hash 在
Verify/gate window 中变化的 fail-closed 行为，还要覆盖 v2 evidence 的 path、字段、no-replace collision 和失败零发布。
并发 fixture 使用 lease、真实 Git 状态和同步 marker；不以随机 sleep 或放宽 timeout 证明正确性。

这些定向回归不替代最终受控 Workspace。可执行候选仍必须在 staged tree 上经官方入口完成完整 workspace gate。

## 后果

- prepared cache 不因纯 runner 文本变化无条件失效，但 Workspace evidence 总能绑定实际 gate policy；
- schema v5 可能使已有 prepared environment 在 Verify 时要求一次 Setup；
- candidate evidence 不再可按 tree 覆盖，重复运行保留不同 run ID 的独立 records；
- 后续改变 gate 集、jobs、runner 参数或 policy parser 必须同时更新 JSON、guard、contract tests 与本 ADR 的相关说明。

## 关联

- [构建、发布与合规](../architecture/build-release.md)
- [测试策略](../architecture/testing-strategy.md)
- [Windows CI 合同](../../.github/CI.md)
