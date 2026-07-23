# GitHub CI 运维边界

本目录只建立可审计、可逐步启用的工程门禁，不改变 Phase 3 的冻结结论、平台支持状态或发布范围。

## Workflow 分层

| Workflow | 触发 | 稳定 check 名 | Ruleset 边界 |
| --- | --- | --- | --- |
| `Required CI` | pull request、merge queue、`main` push | `Required / Policy`、`Required / Windows Workspace` | 两项都在 GitHub-hosted runner 首次实际通过后，才可作为 `main` required checks |
| `Native Quality (Non-Required)` | 每周 schedule、手动 dispatch | `Windows Native / <preset>`、`Linux Phase 3 Candidate (Non-Required)` | 重型证据任务，不设为 required |

`Required / Policy` 在 Windows Server 2022 的 MSVC x64 developer environment 中校验规范、65 个 exact Rust
conformance tests、Markdown 链接、repository guards、变更行和 clean tree。选择 Windows runner 是为了让
`easycon-serial` 中 6 个 `#[cfg(windows)]` exact tests 与其余测试在同一 Policy job 中真实编译并各执行一次；
Ubuntu runner 既不能链接仓库默认的 MSVC target，改用 Linux target 又会排除这 6 个测试。

`Required / Windows Workspace` 仍在独立的 MSVC x64 job 中执行冻结 Rust workspace、Loom runtime models、规范、
链接、repository guards 和 diff 全门禁，并保留完整 vcpkg、OCR、cache 与权限边界。Policy 使用 Windows runner
只证明该次 repository policy 与 exact conformance gate；它不产生新的 Windows 平台、serial 硬件、Phase 3 native
或发布支持结论，也不能替代 Windows Workspace 或 non-required native-quality 证据。

重型 Windows native matrix 与 Phase 3 文档保持一致：MSVC Debug/Release、clang-cl ASan、clang-cl UBSan trap、
MSVC analyze、clang-tidy warnings-as-errors 和 libFuzzer tracked corpus。它们只能由 schedule 或手动触发，普通
PR 上的 workspace 结果不能替代这些 native-quality 证据。

Linux job 是 non-required Vision build candidate。只有四个 Linux native 配置、完整 Cargo/Loom/fixture/repository
门禁在真实 GitHub Linux executable 上通过并保留 Actions 日志后，才产生该次 software evidence；workflow 文件本身
不把 Linux 标记为 Passed，也不代表 serial、硬件、四语言、package 或完整 Linux SDK 支持。

## 固定输入与输出

- Rust 固定为 `rust-toolchain.toml` 的 `1.97.1`，job 同时核对实际 `rustc`。
- vcpkg scripts 固定到 `microsoft/vcpkg` release `2026.06.24` 的 commit
  `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`；同一 commit 以独立角色继续作为 registry baseline。
- vcpkg tool 固定到 `microsoft/vcpkg-tool` release `2026-07-13` 的 commit
  `bf04c909169fdbb30821c02c6eb01f1cd1295d05`。Windows `vcpkg.exe` 固定为 6,749,536 bytes、SHA-256
  `67958c6a13a35130ff8035bef33097ffe3376a6708577a826cfa41fa592db611`；Linux `vcpkg-glibc` 固定为
  8,553,216 bytes、SHA-256 `9f68d6f2158c8a1ae4800260fad2972a21a48f2d43c02d40e79049650b5260c9`。
- OCR 只调用仓库 provisioner；manifest、来源、大小、SHA-256 和许可证在每次运行中重新核验。
- Workflow 权限只有 `contents: read`，checkout 不持久化凭据，不读取本地第三方源码目录，也不使用 repository secret。
- Cargo target、CMake build、vcpkg install/binary cache 和测试模型仅位于 runner temp 或 ignored `.tools/`；cache hit
  只缩短依赖准备时间，后续实际 gate 仍必须执行并成功。
- Cache 使用分离的 restore/save：pull request 只能读取，只有全部门禁成功后的可信 `main` 运行可以写入。vcpkg
  key 同时绑定 scripts、tool release/asset、registry、manifest、triplet、preset 与两份 workflow；tool 本身不缓存，
  因此每次运行都会在 cache restore 后重新核验来源、大小、SHA-256 和精确 `vcpkg version`。

上述拆分只修正获取来源与校验说明。冻结的 vcpkg tool/version、registry、依赖版本和 Phase 3 平台状态均未改变；
新增显式 scripts pin 复用既有 `2026.06.24` registry release commit，不构成新的软件或硬件通过证据。

首次启用 Ruleset 时，应从成功的 `Required CI` 运行中选择精确 check 名 `Required / Policy` 与
`Required / Windows Workspace`。
不要选择 native matrix 或 Linux candidate；本仓库不由 workflow 自动创建或修改 Ruleset。
