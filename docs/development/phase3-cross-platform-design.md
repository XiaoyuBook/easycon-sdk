# Phase 3 Vision 跨平台边界设计

## 1. 状态与范围

本文是 [ADR-0012](../decisions/0012-phase-3-vision-native-target.md) 的跨平台实现 Gate，接管基线固定为
`9a1f6a57cf5b17c606b3c594b24cf25331aad302`，集成基线固定为
`main@41c5f0c2b19165769d4aa8e4512ad46280a1415b`。在独立架构 review 清零前，不修改生产实现。

本轮只收口 `easycon-vision -> easycon-native-sys -> private native bridge`。不实现 Controller Linux/macOS
serial，不进入四语言 binding、public C ABI、package、ECS 或 Phase 4，不创建完整跨平台 SDK 支持声明。

| 平台 | 产品方向 | 本轮最高状态 | 证据要求 | 本轮发布 |
| --- | --- | --- | --- | --- |
| Windows 10/11 x64 | v1 Tier 1 | Vision `Hardware Unverified` | MSVC Debug/Release、native fixture、sanitizer、analysis、fuzz、Rust/spec 全门禁 | 不发布，仍无硬件支持行 |
| Linux x64 | v1 正式目标方向 | Vision Build Candidate，软件与硬件分级 | 真实 Linux configure/build/CTest、Cargo、fixture；无环境则 `Build Unverified` 并交付可审计 handoff | 不发布，不外推 serial/语言/package |
| macOS Apple Silicon arm64 | 实验源码方向 | `Experimental Source Candidate / Build Unverified / Hardware Unverified / Not Shipped` | fail-closed source、依赖方向测试、未来实体 Mac handoff | 不生成 binary/package，不承诺 Intel/universal |

`Passed`、`Build Unverified` 和 `Hardware Unverified` 必须逐平台、逐门禁记录。Windows 结果不能替代 Linux 或
macOS；synthetic/file 结果不能替代摄像头、USB/CH32 或 hot-plug 证据。

## 2. 平台中立所有权

以下语义只存在于 Rust common core，平台 adapter 无权改变：

- immutable `Image`、`Frame`、`Label`，以及 SDK 自有 pixel format、ROI、sequence、monotonic timestamp；
- `Opening -> Streaming -> Faulted -> Stopping -> Closed` Capture 状态机；
- session-owned startup Operation、latest `Option<Arc<Frame>>`、snapshot 优先级和 stale-frame fault policy；
- 一个 Runtime-supervised read worker、一个 backend owner、一个 external cleanup owner；
- resource cancellation、deadline、interrupt request、worker handoff、join、handle consumed acknowledgement；
- native/OCR pool admission、公平、queued/in-flight cancellation、close 和 owner lifetime；
- backend close error 与 cleanup-unproven 的区分，后者保留 registration/retention 并形成真实 `CloseFailed`。

公共语义只能表达“请求中断、唯一 worker 收口、join、释放 owner”。不得把 Windows event、COM apartment、
`VideoCapture::release` 竞态、V4L2 fd 或 AVFoundation session 写入状态机契约。

## 3. Rust 扩展边界

公开 `CaptureBackendKind` 和 Capture error kind 使用 `#[non_exhaustive]` 或等价可扩展机制。公开 backend 只表达
平台中立的 `Synthetic`、预验证 `File` 与 `SystemDevice`；不得出现 `DirectShow`、`MediaFoundation`、`V4l2` 或
`AvFoundation`。调用方遇到未来平台中立能力值不得以穷举业务分支崩溃。

DirectShow、Media Foundation 和 V4L2 只存在于 `easycon-native-sys` 私有 platform detail。公开 descriptor 内部
持有不可解释的 adapter token，必要时只暴露 bounded UTF-8 diagnostic 字符串；device open 消费同一 descriptor/
token，不让调用方按平台 enum 重建业务分支。本轮没有 AVFoundation token 或值。

`CaptureSourceDescriptor` 的 `source_id` 是平台提供的 opaque strict UTF-8 值：

- 不把 friendly/display name 当 identity；
- 不解析、拼接或跨平台比较内部格式；
- discovery/open 只保证同一平台 adapter 定义的 round-trip；
- 跨重插稳定性只有硬件矩阵验证后才可声明。

backend、profile、pixel format、timestamp 和 top-level error 都使用 SDK 自有模型。平台/native code 只允许作为
可选 diagnostic，不参与业务控制流。Rust safe API 不暴露 HRESULT、HANDLE、COM pointer、V4L2 struct、
Objective-C object、OpenCV/Tesseract enum 或 native handle。

内部 `CaptureBackend` trait 保持最小表面：`open/read/close/interrupt/finalize`。Synthetic、预验证 File 与 native
adapter 共用同一 Rust worker/state/latest/operation 实现，不复制 Vision 业务逻辑。compile-time guard 必须固定：

```text
easycon-runtime !-> easycon-vision
easycon-native-sys !-> easycon-runtime/easycon-vision
easycon-vision -> easycon-native-sys
platform source !-> Rust state model
common native source !-> platform headers
```

## 4. Internal C boundary

私有 header 继续只使用：

- `<stdint.h>` 固定宽度整数；
- pointer + `uint64_t` length 的 strict UTF-8；
- opaque handles；
- owned buffer/error 及同模块 release；
- calling-convention macro，Windows x64 为 `__cdecl`，其他平台使用平台 C ABI；
- `noexcept` entry、zeroed outputs 和完整 exception trampoline。

禁止出现 C++ STL、C enum layout、OpenCV enum、`long`、`wchar_t`、HRESULT、HANDLE、GUID、V4L2 layout、
Objective-C pointer 或平台 allocator ownership。x64 layout assertion 必须在 Windows/Linux 都成立；macOS arm64
首次 build 时必须重新验证，不能由文档推定。

未知 backend/status raw value 在 private safe wrapper 中成为稳定 contract error；未来 public C ABI 会重新分配
自己的数值，不复制本 private ABI。

## 5. Native source 分层

目标布局固定为：

```text
native/bridge/src/
  common/
    bridge.cpp
    bridge_internal.hpp
    image_codec.cpp
    ocr.cpp
    vision_ops.cpp
    capture_common.cpp
    capture_platform.hpp
  platform/windows/
    capture.cpp
  platform/linux/
    capture.cpp
  platform/macos/
    capture_unavailable.cpp
```

`common` 只包含 OpenCV/Tesseract/Leptonica、固定 C 数据模型、exception/ownership、codec/template/OCR/color、
预验证 File 与 adapter-neutral capture glue。它不得 include Win32、DirectShow、Media Foundation、V4L2 或 Apple
framework header。

`platform/windows` 独占 COM、DirectShow、Media Foundation 与相关 link libraries。Windows Node F 行为必须零
回归：真实 discovery 可保留，但 DShow/MSMF open 在未资格化时仍于 device access 前返回 Unsupported。

`platform/linux` 只建立 V4L2 adapter boundary。本轮允许编译真实 platform selection 和明确 Unsupported；没有
真实设备时不声明 discovery、stable identity、profile、pixel format、FPS、hot-plug 或 close SLO。不得以空列表
success 或空 frame success 冒充 backend。

`platform/macos/capture_unavailable.cpp` 是小型 fail-closed adapter，不 include 或模拟 AVFoundation。discover/open
返回明确 BackendUnavailable/Unsupported；null release/close 只维持 C ownership 的幂等清零，不返回假设备、
profile 或 frame。较大 Objective-C++/AVFoundation 生产代码必须等至少一次真实 Apple Silicon build 通过后另行
设计、review 和实现。

## 6. 构建选择

CMake 只允许 x64 Windows/Linux 和显式 experimental arm64 Apple source candidate：

- `WIN32`: MSVC-compatible ABI，选择 `platform/windows`，只在这里链接
  `mf`, `mfplat`, `mfuuid`, `ole32`, `strmiids`；
- `UNIX AND NOT APPLE`: x86_64，选择 `platform/linux`，不得看到 Windows library/header；
- `APPLE`: 默认 configure 失败；只有 arm64 且调用方显式设置 experimental source flag 才选择
  `platform/macos/capture_unavailable.cpp`；
- 其他 target: configure 失败，不生成空成功 target。

presets、Cargo build script、toolchain 和 vcpkg triplet 按 target 选择，禁止硬编码盘符、反斜杠、cwd 或开发机
PATH。Windows triplet 为 `x64-windows-static-md`，Linux 为固定的 x64 Linux triplet，macOS 未来 handoff 为
arm64-osx。三者共享 registry baseline `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3` 与 OpenCV
`4.12.0#5`、Tesseract `5.5.2`、Leptonica `1.87.0`。

Windows manifest 可以启用 `dshow/msmf`；Linux/macOS manifest 不得请求这些 feature。macOS 本轮不增加未经
执行的 preset，也不产生 package/install rule。

owned warnings：Windows 保持 `/W4 /WX /permissive- /EHsc /Zc:__cplusplus`；Linux 使用
`-Wall -Wextra -Wpedantic -Werror`。sanitizer 选项必须按 compiler/platform 分支，不能把 clang-cl runtime 名称
传给 Linux/Apple Clang。

## 7. Synthetic 与 File capture

Synthetic backend 是平台中立状态机的正式可复现测试输入，继续使用 barrier/channel/failpoint/VirtualClock，
不得依赖随机 sleep。

任意路径的 `cv::VideoCapture` file open、filesystem metadata/read 或同步 `imdecode` 都不能在 capture worker 内
仅靠事后 elapsed check 宣称可中断。common File candidate 固定为：

1. 在创建 CaptureSession 前，通过有界输入或 fixture API 取得并验证 bytes；
2. 使用 common codec 路径解码为 immutable owned `Image`/finite Frame sequence；
3. worker read 只从预验证、内存持有的 finite sequence 取 Frame，不执行文件系统 I/O 或 decode；
4. interrupt 只唤醒 sequence wait，close 可以确定性 handoff/join；
5. arbitrary path/video streaming 继续 Unsupported，直到有独立 bounded-I/O 设计和回归。

因此 File fixture 可以证明 common codec + Capture 状态机组合，但不能宣称任意视频文件、容器、seek、实时 FPS
或硬件 capture 支持。

## 8. Linux candidate 验证

Linux 环境存在时至少执行：

```bash
python3 tools/provision_vision_test_model.py \
  --manifest spec/fixtures/vision/ocr-model.json \
  --output .tools/vision-models/tessdata_fast-4.1.0
export EASYCON_VISION_TEST_TESSDATA="$(realpath .tools/vision-models/tessdata_fast-4.1.0)"
cmake --preset linux-debug
cmake --build --preset linux-debug --parallel
ctest --preset linux-debug --no-tests=error
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 tools/run_runtime_models.py
python3 tools/validate_specs.py
python3 tools/check_markdown_links.py
python3 tools/check_repository_guards.py
git diff --check
```

native CTest 必须实际覆盖 codec、template、edge、HSV、OCR missing-model 和 provisioned legal English model、
prevalidated File、exception/ownership/resource count。Linux fixture 结果必须来自 Linux executable，不能复制
Windows log。无可用 Linux 环境时不安装需重启的系统组件、不借用未授权远程主机；生成精确 bundle、依赖、
命令和 SHA-256，状态保持 `Build Unverified`。

## 9. macOS experimental handoff

本轮 macOS 只接受以下证据：Windows/Linux 可运行的依赖方向、C layout source assertion、平台选择和 fail-closed
contract tests，以及 future handoff。没有 Xcode/Apple SDK build log 时，所有 macOS build/hardware 项为 Unverified。

未来第一阶段必须在 Apple Silicon arm64 Mac 上固定并记录：

- exact source bundle SHA-256 与 git commit/tree；
- Xcode、Apple Clang、macOS SDK、CMake 4.3.3、Ninja、Rust 1.97.1；
- locked vcpkg tool/registry 和 OpenCV/Tesseract/Leptonica 解析版本；
- common codec/template/edge/HSV/OCR、synthetic、prevalidated File、exception/ownership/resource-count 门禁；
- header layout/calling convention 与 Rust FFI smoke。

只有第一阶段真实 compile/software gates 通过，才允许设计和合并完整 AVFoundation Objective-C++ backend。第二
阶段必须在可接 USB/摄像头的实体 Apple Silicon Mac 上验证 camera discovery/open/profile/frame、热插拔、取消、
deadline、close/join、USB/CH32 serial、长期资源计数。云端 Mac 只能提供第一阶段证据。

最后才执行 C++/.NET/Python/Node 四语言、canonical binary、CMake archive、NuGet/PyPI/npm layout 和 clean-machine
install。完成前状态保持 `Not Shipped`，不生成 Intel x64 或 universal binary。

## 10. 许可证与包边界

SDK 自有代码保持 `GPL-3.0-only`。OpenCV Apache-2.0、Tesseract Apache-2.0、Leptonica BSD-style 及 transitive
notices 按实际 target 重验。测试用 `tessdata_fast` English model 仍只在 ignored cache，不能打包，也不关闭
O-03。`EasyCon/` 继续只读、ignored，不成为 build、fixture、bundle 或下载输入。

本轮没有 public header、public C ABI、四语言 package 或平台 binary 发布。macOS unavailable source 不是 backend
capability；Linux Vision candidate 不是完整 Linux SDK；Windows software gates 不是 capture hardware qualification。

## 11. Review 与重新打开

独立架构 reviewer 必须专门回答：

1. common Rust/C++ 是否完全没有 Windows/Linux/macOS 业务语义或平台类型；
2. C boundary 是否仍为固定宽度、UTF-8 length、opaque/owned/calling macro；
3. backend/device/profile/error 是否可扩展且没有伪造 AVFoundation；
4. File candidate 是否在 worker 前完成 I/O/decode，close 是否仍可证明；
5. Linux 无硬件时是否避免 discovery/profile/FPS 声明；
6. macOS source candidate 是否 fail closed、足够小、无死代码堆积和虚假支持；
7. Windows source/library/preset 是否零回归，Linux 是否完全看不到 Windows dependencies；
8. 状态矩阵、许可证、包和后续 serial/四语言/hardware 门槛是否准确。

以下变化必须重新打开 ADR-0012 并完成新 review/gates：平台晋级、增加 AVFoundation、放开 arbitrary file path、
改变状态机/close/ownership、改变 private C layout、依赖版本、triplet、sanitizer 替代、公共 ABI、package 或语言
binding。普通局部 bug fix 仍需回归和适用门禁，但不自动扩大平台支持状态。
