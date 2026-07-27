# GitHub CI 运维边界

本目录只建立可审计、可逐步启用的工程门禁，不改变 Phase 3/4 的冻结结论、平台支持状态或发布范围。

## Workflow 分层

| Workflow | 触发 | 稳定 check 名 | Ruleset 边界 |
| --- | --- | --- | --- |
| `Required CI` | pull request、merge queue、`main` push | `Required / Policy`、`Required / Windows Workspace` | 两项都在 GitHub-hosted runner 首次实际通过后，才可作为 `main` required checks |
| `Native Quality (Non-Required)` | 每周 schedule、手动 dispatch | `Windows Native / <preset>`、`Linux Phase 3 Candidate (Non-Required)` | 重型证据任务，不设为 required |

`Required / Policy` 在 Windows Server 2022 的 MSVC x64 developer environment 中校验规范、Rust exact
conformance、Markdown 链接、repository guards、变更行和 clean tree。选择 Windows runner 是为了让
`easycon-serial` 的 `#[cfg(windows)]` tests 与其余测试在同一 Policy job 中真实编译并执行；Ubuntu runner 既不能
链接仓库默认的 MSVC target，改用 Linux target 又会排除这些测试。

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

- Rust 由 `rust-toolchain.toml` 精确固定为 `1.97.1`、`x86_64-pc-windows-msvc`、rustfmt 和 clippy。
- vcpkg scripts 与 registry baseline 都固定为
  `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`。vcpkg tool 固定为 release `2026-07-13`、commit
  `bf04c909169fdbb30821c02c6eb01f1cd1295d05`；Windows asset 固定 6,749,536 bytes 和 SHA-256
  `67958c6a13a35130ff8035bef33097ffe3376a6708577a826cfa41fa592db611`。
- `scripts/vcpkg-tools.json` 的审计 SHA-256 固定为
  `7757067afc4839dd982eee8cfb68cb500c4255a64c37a7c150ee9244342595e6`，并逐字段核对 CMake `4.3.3`、
  Ninja `1.13.2`、7-Zip `26.01` 与其 `7zr` bootstrap 的 URL、archive、executable 和 SHA-512。7-Zip 最终
  `7z.exe` 另有固定 SHA-256；Setup 以空 PATH 和 `VCPKG_FORCE_DOWNLOADED_BINARIES=1` 调用 vcpkg fetch，只接受
  清单派生的唯一绝对 x64 `26.01` 路径，不接受宿主 PATH 中的任意 `7z.exe`/`7zr.exe`。
- 直接 native pins 为 OpenCV `4.12.0#5`、Tesseract `5.5.2#0` 与 Leptonica `1.87.0#0`；Setup 同时核对
  registry baseline 版本与 versions database git tree。传递依赖由同一固定 baseline 解析。
- OCR 只调用仓库 provisioner；manifest、来源、大小、SHA-256 和许可证在 Setup 与日常 Verify 中核验。
- Workflow 权限只有 `contents: read`，checkout 不持久化凭据，不读取本地 `EasyCon/`，也不使用 repository secret。
- 下载包、工具二进制、native install tree、OCR 模型和构建输出只位于 runner temp 或本机受控环境根，不进入 Git。

`tools/windows_build_environment.json` 是 Windows 环境审计清单。其自身与 `rust-toolchain.toml`、vcpkg manifest/
configuration、triplet、CMake preset、OCR manifest/provisioner 共同组成 fingerprint；任一输入变化都会选择新的环境
目录并要求重新 Setup。清单为每项 fingerprint 输入显式声明 `text` 或 `binary`：text 必须是严格 UTF-8，并在 hash
前把 CRLF 和独立 CR 规范化为 LF；binary 始终按原始 bytes 计算。repository guard 将该清单与 Required CI、vcpkg
配置及独立 native-quality pins 交叉核对。PowerShell Setup parser 在 provision 前强制 exact schema 键、固定
path/kind/顺序、无 `.`/`..` component 的规范相对路径和 Windows 大小写不敏感 path identity；任一偏差直接拒绝，
不能等到后置 repository gate 才发现。Python guard 独立执行同一严格合同，防止清单与 Setup parser 漂移。

## Windows Setup、Verify 与 Workspace

普通 PowerShell 不需要预先进入 VS Developer Shell。新电脑或固定清单变化后，先运行一次在线、幂等的 Setup：

```powershell
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Setup
```

Setup 使用 `vswhere.exe` 加载并核对 VS 2022 x64、MSVC `14.44.35207` 与 Windows SDK `10.0.26100.0`，安装固定
Rust toolchain、受控 CMake/Ninja、vcpkg scripts/tool/native dependencies、7-Zip 和 OCR 模型。所有下载先按清单
hash 验证；完成后写入 `environment-stamp.json`，记录 fingerprint、workspace key、工具显式路径与 SHA-256、精确
版本、Cargo vendor/native tree hash 和资产路径。Cargo crates 由 `Cargo.lock` 与全部 workspace manifests 驱动
`cargo vendor --locked` 安装到该环境；下载缓存只用于加速生成 vendor tree。重复 Setup 先验证现有 stamp；环境完整时
返回 `already-ready`，损坏时只重建该 fingerprint/worktree 的受控环境。vcpkg scripts checkout 只从同卷完整
staging 目录做原子 rename；Windows sharing violation 使用有限 publish/cleanup 重试。若外部句柄阻止删除，原始
publish failure 始终是主错误，诊断同时记录 cleanup failure 并把残留 destination 标记为不可用；释放句柄后，下一次
Setup 在独占 lease 下删除旧环境树并恢复。脚本不把无法删除的 partial destination 伪装成有效 checkout。
通用工具、vcpkg.exe 与 stamp 的 `.download-*`/`.write-*` 临时文件也使用有界 cleanup；外部句柄阻止删除时，诊断以
原始 download/hash/publish failure 为主并附带 cleanup 与 residual 状态，临时文件永不作为固定 asset 或 stamp 接受。
句柄释放后，后续下载可以使用新的完整 temporary 恢复，下一次 Setup 也会在独占 lease 下清理旧环境树。

环境根的 `caches/` 是独立于 `e/<fingerprint>/<worktree>` 的共享资产层，不保存 stamp、ready 状态或 prepared
environment。CMake/Ninja、vcpkg.exe、7-Zip/7zr 与 OCR 原始文件进入 `assets-v1/blobs/<algorithm>/<hash>`；每次命中
重新核对 hash，带大小 pin 的资产同时核对 bytes。损坏 blob 在逐资产独占锁内原子移入 quarantine，下载写入唯一同卷
temporary，完整验证后才原子发布。不同 hash 使用不同锁，同一 hash 跨 identity 互斥，错误 hash 永不成为最终 blob。
已验证的 CMake/Ninja archive 还会原子物化到按 vcpkg scripts commit 隔离的 downloads root；vcpkg 使用前再次核对该
目标副本，损坏时从 blob 修复，因此不为同一 internal tool 发起第二次公网下载。
vcpkg scripts 按 commit 保存，每次复核 HEAD、cleanliness、tools manifest 与 registry pins，再以无 hardlink 的本地 clone
物化到当前环境；source downloads 按 scripts commit 持久复用并继续由 vcpkg 自身 hash 合同核验，install 最多做三次有界
尝试并复用同一 downloads/buildtrees/binary cache。安装成功后删除 transient buildtrees/packages，包括 port source 中的
合法 reparse/symlink；删除只移除链接本身且不跟随其目标，这些 transient 目录不进入 prepared environment 或 stamp。
OCR 先由原有 Python
provisioner 精确校验冻结 manifest，再从 SHA-256 blob 物化。`RUSTUP_HOME` 持久复用已安装 components，但由于 Rust
分发内容没有仓库内 hash pin，每次实际 Setup 仍执行 rustup install/check；install 最多做三次有界尝试并复用 rustup
保留的 partial，成功后再次核对 release、host、target 与 components。

日常只验证或运行完整门禁：

```powershell
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Verify
pwsh -NoProfile -File tools/run_windows_workspace.ps1 -Mode Workspace
```

Verify 与 Workspace 都不 provision、install 或 download。Verify 重新计算 fingerprint，验证 stamp、所有显式工具
路径/hash、MSVC/SDK、Rust、vcpkg checkout/tool/audit pins、OCR 与完整 native install tree，并只为当前进程设置受控
PATH、Cargo source replacement 和构建变量。Workspace 必须先通过同一 Verify，再执行 Cargo、Loom、规范、链接、
repository contracts 与 diff 门禁。缺失、损坏、worktree 不匹配或 fingerprint 失配都在第一个 build gate 前失败，
并明确要求重新运行 Setup。

每个 fingerprint/worktree identity 在环境根外有一个 ownership lock。Setup 从检查 stamp、删除旧树、安装到发布并
复验 stamp 全程持有独占 lease；Verify 持共享 lease；Workspace 的同一个共享 lease 覆盖 Verify 和全部 gates，因而
Setup 不能与同一环境的读取或构建竞争。共享资产层另有跨 identity reader/writer lease：Setup provision 期间独占，
Verify/Workspace 从验证到最后一个 gate 共享读取，防止另一 identity 修改 Rust/vcpkg 共享状态。异常路径总是释放 lease。
Verify 在启动 gate 前清除 ambient Rust wrapper/compiler、
Cargo target/linker/registry flags、cc-rs target compiler、`CL`/`_CL_`、MSVC Developer Shell 残留、CMake/package roots、
vcpkg override 与代理变量，再用 stamp 中核验过的工具绝对路径、pinned Developer Shell include/lib 路径和受控变量重建
当前进程环境；HTTP(S) proxy 只允许在线 Setup 使用。模块完整导出集合只有 Setup、Verify 与 Workspace；parser、下载、
安装、环境修改和无锁 gate core 等 helper 全部保持私有。所有公开 gate 执行都必须先 Verify，
并持有同一个 shared lease 到最后一个 gate。Setup/Verify/Workspace 返回时完整恢复调用进程原有环境（包括原先缺失、
空值和名称大小写），成功和异常路径都不遗留临时受控变量。native command 或 gate 的非零退出在 cwd cleanup 前登记为
主错误；若原 cwd 已被外部删除，location restore failure 只附加到诊断。子进程成功且 cwd 无法恢复时，恢复失败仍直接失败。

完整且 hash 匹配的直接资产 cache hit 不调用下载器；fingerprint/worktree 变化、旧环境损坏或前次 Setup 失败都只重建
prepared environment，并复用这些已验证 blob。vcpkg scripts 命中也不 fetch。Rust 仍执行 rustup 安全检查，vcpkg/Cargo
在其下载 cache 缺项时仍可联网，因此这一区分不是 air-gapped 或完全离线合同。PowerShell、Git、Python、rustup、
VS Installer/vswhere 与 VS Build Tools 是启动
Setup 的宿主前置条件；Setup 把实际使用的宿主可执行文件路径和 SHA-256 写入 stamp，日常 Verify 不静默回退到其他
系统工具，也不修改 user/machine PATH、持久环境变量或 Git 全局配置。
宿主 Python minimum 由固定清单设为 `3.8.0`，Setup 使用数值版本语义比较，因此 hosted runner 的 Python `3.12.x`
满足要求；Setup 与 Verify 都只接受恰好一行、完整 `Python X.Y.Z` 的版本输出。这项启动条件不改变 SDK Python
binding 的 `3.10+` 产品目标。

默认环境根为 `LocalApplicationData/EasyConSdk/be2`，其下按 fingerprint 与 canonical worktree
路径 hash 分隔。`-CacheRoot` 可选择另一个受控根。Required Windows job 在 checkout 后、cache restore 和 Setup 前，
用 step 可用的 `RUNNER_TEMP` 计算受控根并通过 `GITHUB_ENV` 设置 `EASYCON_BUILD_CACHE_ROOT`；job-level env 不引用该处
不可用的 `runner` expression context。restore/save 路径与 Setup/Workspace 都使用同一个 runner temp 根。短 Cargo
target 路径保持不同 worktree 的 source-bound CMake cache 隔离；脚本拒绝 reparse point、环境根逃逸、过长 object
path、外部 vcpkg overlay/chainload/install 输入和脏 scripts checkout。

Required CI 先单独执行 `-Mode Setup`，再执行 `-Mode Workspace -RequireCleanTree`。Actions restore 的 Setup 资产仅包含
`assets-v1`、vcpkg scripts/downloads 与 Rust home；`e/`、stamp、ready 状态和物化后的环境工具目录永不进入 Actions
cache。key 不包含 workflow/module 实现文件，避免无关 runner 编辑导致所有原始资产失效。pull request 先读同一 PR
namespace，再回退到 trusted-main；每个 run 使用新 primary key，并在 `always()` 下只写该 PR namespace，使 Setup 后的
代码门禁失败仍可供同一 PR 后续 run 复用。`main` 只读 trusted-main，且仅在完整成功后写 trusted-main，绝不读取 PR
namespace。所有 restore 都是不可信的提速输入：Setup 每次按上述 hash/git/rustup 安全模型复验，cache miss 不改变正确性。
Cargo downloads 与 vcpkg binary cache 保持独立 key，且同样不因 workflow/module 文本编辑全量失效。

`Native Quality (Non-Required)` 没有改用 workspace runner：其 clang-cl、clang-tidy、analyze、sanitizer、fuzz preset
矩阵不是 Required Windows Workspace 合同。它保留独立初始化，同时由 guard 防止 vcpkg pins 漂移。

首次启用 Ruleset 时，应从成功的 `Required CI` 运行中选择精确 check 名 `Required / Policy` 与
`Required / Windows Workspace`。
不要选择 native matrix 或 Linux candidate；本仓库不由 workflow 自动创建或修改 Ruleset。
