# 0013：冻结 Phase 3 Vision 跨平台源码候选

- 状态：Frozen Source Candidate（分平台分级）
- 日期：2026-07-22
- 集成基线：`main@41c5f0c2b19165769d4aa8e4512ad46280a1415b`
- Node F checkpoint：`9a1f6a57cf5b17c606b3c594b24cf25331aad302`
- 跨平台实现：`b1b3aee5f4a734e1df632c40b52edf8d70fe0d0c`
- 实现 tree：`5feab09398bfe38e9586ff7424ab01f1b4a72997`
- 最终审查候选：`c4fef4ae8e8c46cd905636ef955b1275591f7282`
- 审查候选 tree：`82e47a06d2f2ce697d9b260d6db9cb76d3a26666`
- 完整审查范围：`41c5f0c2b19165769d4aa8e4512ad46280a1415b..c4fef4ae8e8c46cd905636ef955b1275591f7282`
- 最终候选 HEAD：包含本 ADR 的独立 `docs:冻结Phase 3跨平台源码候选` 提交；精确 SHA 由最终 bundle handoff 固定
- 上位目标：[ADR-0012](0012-phase-3-vision-native-target.md)

## 决策

将 `b1b3aee5f4a734e1df632c40b52edf8d70fe0d0c` 冻结为 Phase 3 Vision 跨平台私有 native bridge 的实现
checkpoint，并以通过最终独立审查的 `c4fef4ae8e8c46cd905636ef955b1275591f7282` 作为本 ADR 的说明基线。
冻结范围是共享 Rust Vision core、private `easycon-native-sys`、internal C boundary、common C++ 算法和分平台
source/build selection，不是 public C ABI、完整 SDK 或发布候选。

平台状态固定为：

| 平台 | 产品方向 | 软件/build 状态 | 硬件状态 | 发布状态 |
| --- | --- | --- | --- | --- |
| Windows 10/11 x64 | v1 Tier 1 | Phase 3 Vision software/native gates Passed | `Hardware Unverified` | 不发布；尚无 capture hardware 支持行 |
| Linux x64 | v1 正式目标方向 | Vision `Candidate / Build Unverified` | `Hardware Unverified` | 不发布；不是完整 Linux SDK |
| macOS Apple Silicon arm64 | experimental source | `Experimental Source Candidate / Build Unverified` | `Hardware Unverified` | `Not Shipped`；无 Intel/universal 声明 |

Windows 软件通过不能替代 capture hardware qualification；Windows executable/log 不能替代 Linux 或 macOS build。
Linux Vision candidate 不能外推为 Controller serial、四语言、package 或完整 SDK 支持。macOS 的 fail-closed source
不是 AVFoundation backend，也不构成 binary/package/support 声明。

## 冻结实现边界

本候选冻结以下职责和依赖方向：

- immutable `Image`/`Frame`/`Label`、SDK 自有 pixel/profile/timestamp/error 模型；
- Rust-owned `Opening -> Streaming -> Faulted -> Stopping -> Closed` Capture 状态机、startup Operation、latest slot、
  cancel/deadline、唯一 worker、interrupt、handoff/join、close/finalize 和 unresolved owner 保留；
- 平台中立 Synthetic backend，以及 session 创建前已取得并解码 `Image` 的 Rust-owned prevalidated File backend；
- `.IL` parser/evaluator、codec、template、edge、HSV、OCR 和 Rust-owned bounded native/OCR pool；
- 固定宽度整数、显式 UTF-8 pointer/length、opaque handles、owned buffer/error/release、calling-convention macro 和
  exception trampoline 组成的 private internal C boundary；
- `native/bridge/src/common` 与 `platform/windows|linux|macos` 的 source 分层，以及 target-specific CMake/vcpkg/Cargo
  选择；Windows MF/DShow/COM 库和类型只存在于 Windows platform detail；
- Linux V4L2 private adapter boundary 和 macOS arm64 unavailable adapter 的明确 fail-closed 行为。

公开 Vision backend 只表达 `Synthetic`、`File`、`SystemDevice`，并使用可扩展 enum/error。device descriptor 私有持有
opaque native adapter token并由 `open_device` 消费；公开业务层不出现 DirectShow、Media Foundation、V4L2、
AVFoundation、HRESULT、HANDLE、COM/OpenCV enum 或平台 pointer layout。

native path-backed File/任意视频流仍在 I/O 前 Unsupported。Linux V4L2 discovery/open 和 macOS system-device
capture 返回明确 Unsupported/BackendUnavailable，不返回空 discovery/profile/frame success。macOS source 不包含 Apple
framework、AVFoundation enum/token、Objective-C++ production backend、preset、install 或 package rule。

## Windows 软件证据

所有可执行实现门禁在 `b1b3aee` 提交前后的同一 production diff 上完成；后续 `adb9d15` 与 `c4fef4a` 仅修改
说明文档，不改变 executable/source contract。受磁盘协调影响，后续 Cargo/CMake 输出使用 checkout 外的任务
独占目录；机器路径没有进入 tracked tree。

| 门禁 | 结果 |
| --- | --- |
| MSVC Debug configure/build/CTest | Passed，2/2 native component tests |
| MSVC Release configure/build/CTest | Passed，2/2 |
| clang-cl ASan configure/build/CTest | Passed，2/2 |
| clang-cl UBSan trap configure/build/CTest | Passed，2/2 |
| MSVC `/analyze /analyze:external- /WX` build/CTest | Passed，2/2 |
| clang-tidy warnings-as-errors | Passed，owned translation units 0 finding |
| clang-cl libFuzzer seed replay | Passed，tracked ABI corpus 128 runs |
| OCR missing/legal model component | Passed，frozen `tessdata_fast 4.1.0` test-only asset |

Rust 与仓库门禁通过：

```powershell
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features -j 1
python tools/validate_specs.py
python tools/check_markdown_links.py
python tools/check_repository_guards.py
git diff --check
```

首次并行 workspace test 因 Windows pagefile error 1455 失败；未修改源码的完整 suite 随后用 `-j 1` 通过，因此只
记录确定性低并发门禁，不把第一次环境失败写成测试通过。CTest 覆盖 codec、template、edge、HSV、native File
fail-closed、Windows discovery、exception/ownership/resource count 和 OCR missing/legal model；Cargo 覆盖
prevalidated File、Synthetic 状态机、pool、cancel/deadline/close/join 与 safe native ownership。

## Linux 与 macOS 未验证证据

当前 Windows 主机没有可用 WSL、Docker 或 Podman，且没有授权使用其他远程 Linux 主机；因此未生成 Linux
executable/log，Linux 保持 `Candidate / Build Unverified / Hardware Unverified`。没有用 Windows 结果替代，也
没有声明 V4L2 discovery、identity、profile、pixel format、FPS、hot-plug 或 close SLO。

当前没有 Xcode/Apple SDK/Apple Silicon build log；macOS 保持 `Experimental Source Candidate / Build Unverified /
Hardware Unverified / Not Shipped`。仓库只接受显式 experimental arm64 source build，默认 configure fail closed，
不生成 macOS preset、binary 或 package，也不承诺 Intel 或 universal binary。

固定依赖、外部 build root、Linux 四配置门禁、macOS 软件阶段和后续实体硬件矩阵见
[Phase 3 Linux/macOS 验证 handoff](../development/phase3-cross-platform-validation-handoff.md)。至少一次真实 Apple
Silicon compile/software gates 通过前，不设计或合并完整 AVFoundation backend；云端 Mac 只能提供软件证据，不能
替代 camera、USB/CH32、hot-plug 或长期生命周期硬件门禁。

## 独立最终审查

一名未参与本轮架构、实现或既有审查的 reviewer 只读检查 `main@41c5f0c2..adb9d15`，覆盖 Rust 状态机和
descriptor/token、Synthetic/File/native backend、internal C layout/ownership、common/platform sources、
CMake/presets/vcpkg/build.rs、platform status 和验证 handoff。

reviewer 未发现直接相关、可复现的 P0/P1/P2，确认八项架构 Gate 通过。两个非阻断 P3 文档 finding 为：通用构建
文档的 CMake 下限落后于根工程，以及 handoff 把 Rust-owned prevalidated File 错归到 native CTest。`c4fef4a`
分别把下限更新为 3.30+，并准确区分 native path-backed File fail-closed 与 Cargo `native_capture_contract`。
同一 reviewer 在新 SHA 定向复审，两个 finding 均 Closed，未发现新增 P0-P3。

reviewer 自己的 Capture Rust contract 复跑因其 shell 未加载 MSVC 开发环境、`cl.exe` 不在 PATH 而在 CMake
configure 前停止；该尝试没有被写成通过，也不是代码 finding。独立 review 的 Python spec/link/guard 和 final diff
checks 通过；完整 Windows executable 门禁以实现阶段的上述实际结果为准。

## 明确不冻结

本 ADR 不冻结或声明完成：

- public C ABI、public header/symbol/layout、`easycon-sdk`/`easycon-capi` 或任何稳定公共 API；
- Phase 4 ECS/Automation、Phase 5 ABI candidate、四语言 binding 或发布工程；
- Windows capture card、camera、profile/FPS/hot-plug/close SLO 和 24h soak；
- Linux native build、V4L2 hardware、Controller serial、四语言 package 或 clean-machine install；
- macOS AVFoundation、Controller serial、camera/USB/CH32、Intel/universal binary 或任意 package；
- OCR O-03、官方语言模型集合、SBOM/notices、NuGet/PyPI/npm/CMake archive 或完整跨平台 SDK 支持。

`EasyCon/` 继续只读、ignored，未进入 tracked tree、workspace、build、fixture、bundle 或下载输入。SDK 自有代码保持
`GPL-3.0-only`；OpenCV/Tesseract/Leptonica 和 test-only OCR model 的许可证边界不因本冻结自动满足发布门禁。

## 重新打开规则

以下变化必须重新打开本候选，运行受影响与完整门禁，并以新 SHA 接受独立 review：

- 修改 Vision/native production source、Cargo/CMake/vcpkg、private C layout、fixture/spec 或 lifecycle contract；
- 改变 Capture 状态、latest、cancel/deadline、worker ownership、interrupt/join、close/finalize 或 File prevalidation；
- 增加/晋级平台 backend，加入 AVFoundation/V4L2 真实实现，或改变 Linux/macOS fail-closed 行为；
- 更换 Rust、CMake、vcpkg registry/triplet、OpenCV/Tesseract/Leptonica 或 sanitizer 门禁；
- 将任一平台从 Unverified 晋级，新增 binary/package/support row，或进入 public ABI、Phase 4/5、四语言与发布；
- 出现新的、可复现且可行动的 in-scope P0/P1/P2 finding。

纯说明修正不改变 `b1b3aee` 实现 checkpoint，但仍需文档门禁和准确记录实际 build commit。Linux/macOS 后续证据
只能晋级相应平台/门禁，不能自动升级完整 SDK 或其他平台。

## 关联

- [ADR-0001：外层 SDK 与 EasyCon 源码分离](0001-source-boundary.md)
- [ADR-0003：Rust 核心、私有 C++ 桥接与公共 C ABI](0003-core-native-abi-boundary.md)
- [ADR-0006：Runtime 所有权、终态事务与确定性关闭](0006-runtime-stabilization.md)
- [ADR-0011：Phase 2B Qualification Software Candidate](0011-phase-2b-qualification-software-candidate-freeze.md)
- [ADR-0012：Phase 3 Vision 与跨平台私有 native bridge 开发目标](0012-phase-3-vision-native-target.md)
- [Phase 3 跨平台边界设计](../development/phase3-cross-platform-design.md)
- [Phase 3 Vision implementation log](../development/phase3-vision-implementation-log.md)
- [Phase 3 Linux/macOS 验证 handoff](../development/phase3-cross-platform-validation-handoff.md)
