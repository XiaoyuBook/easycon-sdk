# Phase 3 Linux/macOS 验证 handoff

## 1. 固定输入与状态

本 handoff 只验证 Phase 3 Vision 私有 native bridge 和共享 Rust 状态机，不是完整跨平台 SDK 资格证明。
待验证实现固定为：

| 项目 | 固定值 |
| --- | --- |
| integration base | `main@41c5f0c2b19165769d4aa8e4512ad46280a1415b` |
| implementation commit | `b1b3aee5f4a734e1df632c40b52edf8d70fe0d0c` |
| implementation tree | `5feab09398bfe38e9586ff7424ab01f1b4a72997` |
| vcpkg tool commit | `bf04c909169fdbb30821c02c6eb01f1cd1295d05` |
| vcpkg registry baseline | `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3` |
| OpenCV | `4.12.0#5` |
| Tesseract | `5.5.2` |
| Leptonica | `1.87.0` |
| Rust | `1.97.1` |
| CMake / Ninja | `4.3.3` / `1.13.2` |

验证方必须从精确 commit 或协调方提供的 Git bundle 建立干净 checkout，并先执行：

```bash
test "$(git rev-parse HEAD)" = "b1b3aee5f4a734e1df632c40b52edf8d70fe0d0c"
test "$(git rev-parse 'HEAD^{tree}')" = "5feab09398bfe38e9586ff7424ab01f1b4a72997"
test -z "$(git status --porcelain=v1)"
git fsck --strict
```

若使用 bundle，必须先用协调方伴随交付的 `SHA256SUMS` 执行 `sha256sum --check`。`EasyCon/`、`.tools/`、
build cache 和设备日志不得进入 bundle。所有 build、Cargo target、vcpkg install/cache 和临时输出必须位于
checkout 之外的本次验证独占目录；下列命令用 `EASYCON_BUILD_ROOT` 表示该目录。唯一例外是 provisioner 强制写入
ignored `.tools/vision-models` 的合法 OCR 测试模型；它不能成为 bundle 或 package 输入。

截至 2026-07-22，Windows x64 软件/native 门禁已在上述实现提交通过，capture hardware 仍为
`Hardware Unverified`。当前 Windows 主机没有可用 WSL、Docker 或 Podman，且未获授权使用远程 Linux 主机，所以
Linux 证据保持 `Candidate / Build Unverified`。没有 Xcode/Apple SDK 证据，macOS 保持 Apple Silicon arm64
`Experimental Source Candidate / Build Unverified / Hardware Unverified / Not Shipped`。

## 2. Linux x64 环境

使用原生 x86_64 Linux，不得用 Windows executable 或日志替代。记录发行版、kernel、glibc、CPU、可用内存和以下
工具的完整版本输出。主机需提供官方发行版来源的 GCC/G++、Clang、Git、curl、zip/unzip、tar、pkg-config、
autoconf、automake、autoconf-archive、libtool、Python 3、Linux headers 及 vcpkg bootstrap 所需基础工具。
依赖源码只由锁定的 vcpkg manifest/registry 解析；不得静默改用系统 OpenCV/Tesseract。

```bash
uname -a
getconf GNU_LIBC_VERSION
gcc --version
g++ --version
clang++ --version
cmake --version
ninja --version
python3 --version
rustc --version --verbose
cargo --version --verbose
git --version
```

在 checkout 之外取得并固定 vcpkg tool：

```bash
git clone https://github.com/microsoft/vcpkg.git "$HOME/.cache/easycon-vcpkg"
git -C "$HOME/.cache/easycon-vcpkg" checkout --detach bf04c909169fdbb30821c02c6eb01f1cd1295d05
"$HOME/.cache/easycon-vcpkg/bootstrap-vcpkg.sh" -disableMetrics
export VCPKG_ROOT="$HOME/.cache/easycon-vcpkg"
test "$(git -C "$VCPKG_ROOT" rev-parse HEAD)" = "bf04c909169fdbb30821c02c6eb01f1cd1295d05"
```

准备独占输出和合法 OCR 测试模型。模型只用于测试，不进入源码、bundle 或 package：

```bash
export EASYCON_SOURCE="$PWD"
export EASYCON_BUILD_ROOT="${TMPDIR:-/tmp}/easycon-phase3-$USER-$(date +%Y%m%d%H%M%S)"
export CARGO_TARGET_DIR="$EASYCON_BUILD_ROOT/cargo-target"
export EASYCON_VISION_TEST_TESSDATA="$EASYCON_SOURCE/.tools/vision-models/tessdata_fast-4.1.0"
mkdir -p "$CARGO_TARGET_DIR"
python3 tools/provision_vision_test_model.py \
  --manifest spec/fixtures/vision/ocr-model.json \
  --output "$EASYCON_VISION_TEST_TESSDATA"
```

## 3. Linux x64 软件门禁

四个 native 配置必须分别 configure、build 和运行其 Linux executable。`-B` 覆盖 preset 的默认 binary dir，保证
输出不会写入 checkout。任何 sanitizer 被环境跳过都必须使该项保持 Unverified，不能以普通 Debug/Release 代替。

```bash
for preset in linux-x64-debug linux-x64-release linux-x64-asan linux-x64-ubsan; do
  binary="$EASYCON_BUILD_ROOT/cmake-$preset"
  cmake --preset "$preset" -B "$binary"
  cmake --build "$binary" --parallel
  ctest --test-dir "$binary" --output-on-failure --no-tests=error
done
```

`easycon_native_bridge_components` 必须实际覆盖 codec round-trip/invalid input、template、edge、HSV、native
path-backed File fail-closed admission、Linux V4L2 fail-closed admission、exception trampoline、owned
handle/buffer/error 和 resource-count 归零。`easycon_native_ocr_components` 必须同时覆盖 missing-model 和
provisioned English model 成功路径、复用、exception 与 release。仅看到 CTest 总数不够；回传日志必须包含两个
测试名及成功结果。Rust-owned prevalidated File 生命周期由下述 Cargo `native_capture_contract` 覆盖。

随后用同一 checkout、vcpkg 和 OCR model 运行 Rust 与仓库门禁：

```bash
cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 tools/run_runtime_models.py
python3 tools/validate_specs.py
python3 tools/check_markdown_links.py
python3 tools/check_repository_guards.py
git diff --check
test -z "$(git status --porcelain=v1)"
```

完整 Cargo test 必须包含 `matching_color_contract` 的 template/edge/HSV fixture、`image_contract` 的 codec/ownership、
`ocr_contract` 与 `ocr_pool_contract` 的 missing/legal model 和资源归零、`native_capture_contract` 的 prevalidated
File，以及 `capture_contract` 的 synthetic cancel/deadline/close/join 状态机。测试不得增加随机 sleep，也不得访问
摄像头、串口或 `EasyCon/`。

只有上述命令均在 Linux x64 原生 executable 上通过，Linux Vision 软件状态才可从 `Build Unverified` 晋级为
`Build Verified Candidate`。本 handoff 不实现或验证 Linux serial、V4L2 discovery/open/profile/FPS/hot-plug、
四语言 package 或 clean-machine install；这些项目完成前不得宣称完整 Linux SDK 支持。

## 4. macOS arm64 软件阶段

第一阶段只允许在 Apple Silicon arm64 Mac 上验证 common 路径和 fail-closed adapter。记录 `uname -m`、
`sw_vers`、`xcodebuild -version`、`xcrun --sdk macosx --show-sdk-version`、Apple Clang、CMake、Ninja、Python、
Rust 和 vcpkg 精确版本。`uname -m` 必须为 `arm64`；Intel 和 universal binary 不在本候选范围。

仓库故意没有 macOS preset。验证方必须显式选择 experimental source boundary：

```bash
test "$(uname -m)" = "arm64"
export EASYCON_EXPERIMENTAL_MACOS=1
export EASYCON_BUILD_ROOT="${TMPDIR:-/tmp}/easycon-phase3-$USER-$(date +%Y%m%d%H%M%S)"
export CARGO_TARGET_DIR="$EASYCON_BUILD_ROOT/cargo-target"
export EASYCON_VISION_TEST_TESSDATA="$PWD/.tools/vision-models/tessdata_fast-4.1.0"
mkdir -p "$CARGO_TARGET_DIR"
python3 tools/provision_vision_test_model.py \
  --manifest spec/fixtures/vision/ocr-model.json \
  --output "$EASYCON_VISION_TEST_TESSDATA"

cmake -S . -B "$EASYCON_BUILD_ROOT/cmake-macos-arm64-debug" -G Ninja \
  -DCMAKE_BUILD_TYPE=Debug \
  -DCMAKE_CXX_COMPILER=clang++ \
  -DCMAKE_OSX_ARCHITECTURES=arm64 \
  -DCMAKE_TOOLCHAIN_FILE="$VCPKG_ROOT/scripts/buildsystems/vcpkg.cmake" \
  -DVCPKG_TARGET_TRIPLET=arm64-osx \
  -DEASYCON_EXPERIMENTAL_MACOS=ON \
  -DEASYCON_BUILD_COMPONENT_TESTS=ON
cmake --build "$EASYCON_BUILD_ROOT/cmake-macos-arm64-debug" --parallel
ctest --test-dir "$EASYCON_BUILD_ROOT/cmake-macos-arm64-debug" \
  --output-on-failure --no-tests=error

cargo fmt --all --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python3 tools/run_runtime_models.py
python3 tools/validate_specs.py
python3 tools/check_markdown_links.py
python3 tools/check_repository_guards.py
git diff --check
test -z "$(git status --porcelain=v1)"
```

测试必须证明 codec/template/edge/HSV/OCR/File/synthetic/ownership 路径可执行，且未实现 system-device capture
明确返回 `BackendUnavailable`/`Unsupported`，不返回空成功。云端 Apple Silicon Mac 只可提供本软件阶段证据。
至少一次真实 macOS compile/software gates 全部通过之前，不得设计或合并大段 AVFoundation Objective-C++ 生产实现。

## 5. macOS 后续实体硬件与发布矩阵

完成软件阶段后必须重开 [ADR-0012](../decisions/0012-phase-3-vision-native-target.md)，由独立 reviewer 审查
AVFoundation 设计，再在可接 USB 和摄像头的实体 Apple Silicon Mac 上执行：

| 门禁 | 必需证据 |
| --- | --- |
| AVFoundation camera | discovery、opaque stable identity round-trip、open、实际 profile/pixel format/FPS/frame |
| 生命周期 | hot-plug、blocked read interrupt、cancel/deadline、唯一 worker join、close/retry、长期资源计数 |
| Controller | USB/CH32 serial discovery/open/read/write/interrupt/close 与真实硬件矩阵 |
| 语言 | C++、.NET、Python、Node.js/TypeScript 同一 canonical native core 的四语言 conformance |
| package | CMake archive、NuGet、PyPI、npm、许可证/notices、clean-machine install/uninstall |

云端 Mac 不能替代 camera、USB/CH32 或 hot-plug 证据。全部门禁完成并重新冻结 ADR 前，macOS 始终是
`Experimental Source Candidate / Build Unverified 或按实际软件证据分级 / Hardware Unverified / Not Shipped`，
不生成 binary/package，不加入支持列表，也不暗示 Intel/universal binary 支持。

## 6. 回传证据与判定

每个平台回传一个只读 evidence directory，至少包含：

- source commit/tree 和输入 bundle SHA-256；
- OS、CPU、SDK、compiler、CMake、Ninja、Rust、Python、vcpkg tool/registry 和 resolved package versions；
- 每条 configure/build/CTest/Cargo/Python/git command、exit code、UTC start/end 和完整 stdout/stderr；
- OCR model manifest/hash、CTest test list、Cargo test list和每个输出文件的 SHA-256；
- 明确的 `Passed`、`Failed`、`Build Unverified`、`Hardware Unverified` 分项状态，不合并平台证据。

任何命令未运行、失败、日志缺失、版本漂移、source/tree 不符或工作树被修改，都不得写成 Passed。修复源码后 SHA
已经变化，必须生成新 handoff 并重跑受影响及完整门禁；旧日志不能证明新 SHA。
