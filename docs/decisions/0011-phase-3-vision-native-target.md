# 0011：冻结 Phase 3 Vision 与私有 native bridge 开发目标

- 状态：Frozen Target (`Hardware Unverified`)
- 日期：2026-07-21
- 开发起点：`9944dba50adc34484b65206e07ea0a444103f656`
- 实现状态：未完成；本 ADR 冻结目标，不冻结实现、公共 C ABI 或发布包

## 背景

Phase 1 Runtime 已按 [ADR-0007](0007-phase-1-freeze.md) 冻结，Phase 2A Controller/Serial Candidate 已按
[ADR-0009](0009-phase-2a-freeze.md) 冻结为 `Hardware Unverified`。Phase 3 需要在不改变这些冻结语义、
不进入 ECS 或公共 C ABI 的前提下，实现 Vision 与 OpenCV/Tesseract/capture 的私有原生边界。

本目标以只读 EasyCon 源码事实为兼容依据，但 `EasyCon/` 仍受
[ADR-0001](0001-source-boundary.md) 约束，不成为构建、测试、fixture、submodule 或下载输入。完整实现设计见
[Phase 3 Vision 与私有 native bridge 设计](../development/phase3-vision-native-design.md)。

### 已核对的源码事实

1. `EasyCon.Capture/Capture.cs` 在 Windows 只枚举 DirectShow friendly name 和顺序 index；没有稳定设备身份。
   `CVCapture.cs` 同步调用 `VideoCapture::Read`，忽略返回值，以空 `Mat` 表示失败；没有 reader thread、
   latest slot、首帧 deadline、取消、sequence、timestamp 或 fault 状态。
2. 活跃搜索集合是 `SqDiffNormed`、`CCorrNormed`、`CCoeffNormed`、XY edge、Laplacian edge 和 OCR。
   Canny、非 normalized template 和旧像素算法没有进入活动分支，不是 v1 兼容能力。
3. normalized template 的位置与 score 是：SqDiff 取 min location 并用 `1 - min`；CCorr 取 max；
   CCoeff 取 max 并用 `(max + 1) / 2`。`.IL` 搜索随后乘 100；CLI/Avalonia 截断而 WinForms 向上取整，
   所以旧 ECS 整数转换本身不一致。
4. XY edge 是 BGR to Gray、Scharr X/Y、`0.5/0.5` 加权和、饱和转 8-bit，负梯度不取绝对值。
   Laplacian edge 是 Gray、Gaussian `5x5 sigma=1.5`、Laplacian `CV_16S ksize=3`、absolute scale、
   binary threshold `30/255`。两者再使用 normalized CCoeff。
5. OCR 每次新建 Tesseract engine，固定 cwd 相对 `./Tessdata`、`chi_sim`、Default engine、SingleLine PSM；
   结果 `Trim` 后使用大小写敏感 Levenshtein similarity 乘 mean confidence。源码没有 engine cache、取消、
   whitelist/blacklist 或模型来源记录。
6. `.IL` 是 UTF-8 JSON。name 来自文件名；字段是 numeric `searchMethod`、误命名的 `ImgBase64` 以及绝对
   Range/Target 矩形。实际样本使用 24-bit BMP，当前 save 路径写 PNG；未知 legacy 字段会被忽略。
   原 loader 不排序、重复名首个胜出、非法文件只打印并跳过。
7. `.ILX` 没有调用点或实际文件，读写尾部规则不对称且主要字段不可从外部构造，不能形成稳定契约。
8. `HSVColor` 只有尺度不一致的转换 helper；没有接通 HSV ROI 检测。Phase 3 color detection 是已决定的
   新低级能力，不冒充 source-exact 行为。
9. `EasyCon.Capture/Tessdata` 中两个模型没有可审计来源、上游版本或单独许可证。它们只能作为存在事实，
   不得复制到 SDK、fixture 或包，也不能关闭 O-03。

## 决策

### 1. 范围和依赖方向

Phase 3 固定创建并只实现：

- `easycon-native-sys`：私有 C-compatible bridge 的窄 FFI 声明、错误转换和 native handle RAII；
- `easycon-vision`：Frame/Image/Label、capture、latest slot、`.IL`、score、native 调度和 OCR cache/pool；
- `native/bridge`：OpenCV、Tesseract 和 capture 的低级 C++20 适配；
- `spec/fixtures/vision`：自包含 synthetic 图像、label、expected result、来源、许可证和容差；
- 这些成员必需的根 Cargo、CMake、preset、vcpkg manifest/registry lock 和测试配置。

依赖固定为：

```text
easycon-vision -> easycon-native-sys -> private native bridge
easycon-vision -> easycon-runtime/easycon-model
```

`easycon-runtime` 不反向依赖 Vision；`easycon-native-sys` 不知道 Runtime、operation、event、Label 或 ECS；
C++ bridge 不知道 Rust Runtime。Phase 3 不创建 `easycon-sdk`、`easycon-capi`、ECS 或语言 binding。

### 2. Rust 与 C++ 职责

Rust 是以下内容的唯一 owner：

- capture 状态机、一个 read thread、latest frame slot、首帧等待、deadline、fault 和关闭顺序；
- immutable Frame/Image/Label、sequence、monotonic timestamp、ROI 和 score 归一化；
- native job admission、FIFO 有界并发、取消、OCR engine cache/pool 和 poisoned engine 丢弃；
- `.IL` parser、limits、diagnostics、稳定 registry 和 duplicate policy；
- operation/result/error 终态、panic 隔离以及 Runtime resource/task 注册。

C++ 只实现：

- OpenCV image decode/encode/format conversion/ROI、template/edge、HSV mask/statistics；
- Tesseract engine create/process/release；
- Windows/OpenCV capture discovery/open/read/interrupt/close 和实际 profile 查询；
- bridge-owned buffer/error/opaque handle 的创建与释放。

C++ 禁止实现 operation、event、deadline、retry、label parser、score policy、业务状态机、长期线程、
用户 callback 或 Rust 回调。

### 3. 私有 C-compatible 边界

bridge 只暴露内部前缀 `easycon_native_*`，使用 Windows x64 `cdecl`、固定宽度标量、显式长度 UTF-8、
opaque handle、status、owned error 和 owned buffer。不得出现 public `easycon_v1_*` symbol 或安装 header。

每个 entry point 必须 `noexcept`，并按顺序捕获 `cv::Exception`、`std::exception` 和未知异常。所有 out
parameter 在进入工作前清零；参数、length、stride、乘法、offset 和 output size 在 native 调用前校验。
bridge 分配的 error/buffer 只由 bridge release；Rust 从不使用自己的 allocator free C++ 内存。

Rust `unsafe` 只允许存在于 `easycon-native-sys::ffi` 和紧邻调用的私有转换函数。crate 其余区域 deny
unsafe；每个 unsafe block 记录 pointer、length、alignment、aliasing 和 lifetime 前置条件。safe API 不暴露
raw pointer、native status、OpenCV/Tesseract 类型或 bridge handle。

### 4. Send/Sync 与 native handle

每个 handle 单独证明并测试：

- image/codec/template/color 没有长期 handle，只借用 immutable bytes 到同步调用返回；
- OCR engine 是 `Send`、不是 `Sync`，一次只被一个 Rust pool lease 独占；正常返回归还，native exception
  或 engine-invalid status 标记 poisoned 并立即销毁；
- capture owner handle 是 `Send`，read/open/profile/close 只在一个 Rust read thread；单独的 interrupt token
  是 `Send + Sync`，只设置 bridge 原子 stop/请求 backend wake；destroy 必须在 read thread join 且全部
  interrupt token 释放后发生；
- bridge-owned buffer/error 不跨线程长期存活，safe wrapper 在同一次调用中复制后释放。

除上述证明外不增加 `unsafe impl Send/Sync`。native debug counters 记录 live handles 和 owned allocations；
所有 success/failure/cancel/timeout/fault/close 测试结束必须回到调用前基线。

### 5. Frame、Image、ROI 与限制

Frame 和 Image 都不可变，像素由 Rust `Arc<[u8]>` 拥有。元数据固定包含 width、height、stride、format；
Frame 另含严格递增 sequence 和 Runtime clock 的 monotonic `timestamp_ns`。格式只允许 BGR8、BGRA8、Gray8。

硬上限由 `VisionLimits` 提供并有不可放宽的编译期 ceiling：encoded bytes、width、height、pixel count、
decoded bytes、stride、label count、JSON bytes、Base64 bytes、OCR text bytes 和 queued native jobs。所有
计算使用 checked arithmetic；stride 至少为 row bytes，`stride * height` 不得溢出或超过 buffer。

ROI 是半开区间 `(x, y, width, height)`，宽高必须非零且完全位于图像内。Phase 3 ROI 产生独立、紧密排列的
immutable Image，避免跨 ABI 冻结 strided subview lifetime；native 同步调用仍接受显式 stride 的 borrowed
input。空 ROI 明确失败，不隐式解释为全图。

### 6. Capture 状态机与关闭

状态固定为 `Opening`、`Streaming`、`Faulted`、`Stopping`、`Closed`。创建资源后由一个 Rust-owned、
Runtime-supervised read thread 执行 native open 和所有 read。只有首个有效 Frame 发布后才进入 Streaming；
首帧 deadline 到期、open/read error 或无效帧产生稳定 fault。

latest slot 只保存 `Option<Arc<Frame>>`。替换不会修改旧 Frame；snapshot 在同一个锁/condvar 协议中返回当前
强引用。等待 snapshot 的 caller 可被 resource cancellation、operation deadline、fault 或 close 确定性唤醒，
不使用随机 sleep。

close 顺序固定为：

1. 在 admission gate 内进入 Stopping，拒绝新 snapshot/open work并取消 resource token；
2. 调用 bridge interrupt 或 synthetic backend unblock；
3. 唤醒首帧/snapshot waiter；
4. join 唯一 read thread；
5. read thread 完成 backend close，Rust 再 destroy native handle；
6. 清空 latest slot，注销 Runtime resource，进入 Closed；
7. 重复 close 返回同一结果且不重复 native side effect。

synthetic blocking backend 用 barrier/channel 证明 blocked read 可被打断并 join。OpenCV backend 必须实现
实际 discovery/open/read/interrupt/close，但在具体 capture card/backend/profile 完成物理验证前支持矩阵为空；
本阶段不得宣称 1080p、30/60 FPS、2 秒 close 或任一设备已通过。

### 7. Vision 算法

Phase 3 实现：

- BMP/PNG decode，PNG encode，BGR/BGRA/Gray conversion 和 ROI round-trip；
- `SqDiffNormed`、`CCorrNormed`、`CCoeffNormed`；
- source-exact XY/Laplacian preprocess 后的 normalized CCoeff match；
- OCR with explicit model root、language、engine mode/page segmentation；
- HSV range detection with hue wrap、count、ratio 和 optional bounding box。

template 位置对 SqDiff 取 min location，其他取 max location。Rust 将 raw normalized value 转为：

- SqDiff: `clamp(1 - min, 0, 1)`；
- CCorr: `clamp(max, 0, 1)`；
- CCoeff 和两种 edge: `clamp((max + 1) / 2, 0, 1)`。

位置、mode、error 必须精确；浮点只按每个 fixture 明示的绝对/相对容差比较。Canny、non-normalized
template、Strict/Random/Opacity/Similar/FindColor 旧算法不进入 API。

HSV 输入固定为 OpenCV hue `0..179`、saturation/value `0..255` 的 inclusive bounds。`h_min > h_max`
表示跨 179/0 wrap；S/V lower 大于 upper 是参数错误。ratio 是 `matched_count / ROI pixel count`。

### 8. `.IL`、Label 与 registry

parser 使用结构化 JSON API，不使用字符串切割。输入必须严格 UTF-8、无 comments/trailing comma；已知字段
类型严格。为兼容实际 legacy 文件，未知字段可被保留为 ignored diagnostic，`name`/`matchDegree` 不改变
Label；label name 只来自调用者给出的 source name/文件名。只接受活动 numeric methods `1,3,5,11,12,107`。

图像 method 的 `ImgBase64` 必须是有界 Base64，并在 load 时 decode 为 immutable target Image；OCR method
把该字段解释为 expected UTF-8 text。Range/Target 都是 frame 绝对 ROI。图像 label 在 Range 中搜索 embedded
target；OCR label 对当前 Frame 的 Target ROI 识别。target/range 超界、target 大于 range、空目标、未知 mode、
duplicate name、limit 和 decode error 都产生稳定 diagnostic/error，不打印后跳过。

registry 先按 normalized source path/name 的 UTF-8 byte order稳定排序，再构造；同名是显式 duplicate error，
不采用枚举顺序中的首个。Label immutable。一次 `evaluate(label)` 在入口只取得一个 `Arc<Frame>`，所有 ROI、
template/OCR 和结果都借用该 Frame；同次 evaluate 期间即使 latest 更新也不换帧。结果 score 始终 `0.0..1.0`。
未来 ECS 的 `0..100` 转换属于 Phase 4，本阶段不冻结 rounding。

`.ILX` 没有 parser、loader、extension dispatch 或 API 入口。

### 9. Native pool 与 OCR cache

Rust native pool 有固定 worker/permit 上限、FIFO ticket admission、有限 queue、取消和 close。取消发生在排队
阶段时不执行 native call；执行中无法由第三方库安全中断的调用完成后才提交 Cancelled，不能提前释放借用
buffer/engine。close 拒绝新 admission、取消 queued job、等待 in-flight job 返回并 join Rust workers。

OCR cache key 至少包含 canonical explicit model root、language、engine mode 和 PSM。路径不从 cwd、PATH、
环境变量或 `EasyCon/` 推导；缺模型返回 `MODEL_NOT_FOUND` 等价错误，不联网下载。O-03 未关闭前不提交
traineddata，也不要求成功中文 OCR fixture。若使用成功 OCR 资产，必须单独记录来源 URL、版本、许可证和
SHA-256；否则冻结门槛只要求 missing-model、bad-image、reuse/release、exception 和 poisoned-discard。

### 10. 工具链、依赖和许可证

首发目标是 Windows 10/11 x64、MSVC v143、C++20、CMake/Ninja。vcpkg registry 锁固定为官方 release
`2026.06.24` 的 commit `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`，`x64-windows-static-md`，并解析：

- OpenCV `4.12.0#5`，关闭 default features，只启用 core/imgproc/imgcodecs/videoio 所需的
  `dshow`、`msmf`、`png`、`jpeg`、`intrinsics`、`thread`；
- Tesseract `5.5.2`；
- Leptonica `1.87.0`（Tesseract transitively required）。

自有 C++ target 使用 `/MD`、`/W4 /WX /permissive- /EHsc /Zc:__cplusplus`。第三方 include 标为 system。
MSVC Debug/Release、clang-cl ASan、clang-cl UBSan、clang-tidy、MSVC `/analyze` 和固定 fuzz seed 都是 native
门禁。sanitizer 若与某依赖配置不兼容，必须在实现前把精确替代门禁和理由写入本 ADR 的后续 review，
不能在失败后静默跳过。

依赖许可证至少记录 OpenCV Apache-2.0、Tesseract Apache-2.0、Leptonica BSD-style 及实际 transitive
notices；vcpkg port 中 `license: null` 不能当成已完成审核。SDK 自有代码保持 GPL-3.0-only。

## 实现节点和提交门槛

每个节点先加入在旧实现缺失时失败的最小 fixture/test，再做最小实现，并在全部适用门禁通过后独立提交：

1. A：build、private bridge status/error/buffer/handle、三类 exception isolation、safe native-sys；
2. B：Frame/Image/limits/ROI 与 actual OpenCV BMP/PNG codec/conversion；
3. C：三个 normalized template、XY/Laplacian、HSV wrap/count/ratio/bbox；
4. D：Tesseract create/process/reuse/release、missing model、poisoned engine、fair bounded pool；
5. E：legacy `.IL` corpus、immutable Label、registry、single-Frame evaluate、fuzz seeds；
6. F：synthetic/file/OpenCV capture、state/latest slot/first-frame wait/fault/interrupt/join；
7. G：Runtime resource/operation integration、完整 fault/resource evidence、最终 review 和冻结 ADR。

## Phase 3 退出门槛

只有以下条件全部满足，才能另建 ADR 冻结 `Hardware Unverified Vision Candidate`：

1. A-F 生产实现、fixture 和测试完整，不存在空壳、假的 native result 或测试专用 release 分支。
2. Runtime close 后 Vision resource、operation、task、native handle 和 bridge allocation 计数全部归零；
   success/failure/cancel/timeout/fault/repeated close 均覆盖。
3. synthetic blocked read 由可控 barrier/channel 打断并 join；没有随机 sleep 或 detached thread。
4. actual OpenCV/Tesseract component build/test 通过；missing model 与 O-03 边界准确保留。
5. MSVC Debug/Release、strict warnings、sanitizer/static analysis/fuzz seeds 以及根 Rust/spec 门禁实际执行并记录。
6. 固定最终实现 SHA 经新的独立 reviewer 审查；可复现 in-scope P0/P1/P2 finding 先回归后修复，并在新
   SHA 复审。理论未来加固进入 backlog，不无限循环。
7. 没有公共 `easycon_v1_*`、public C header、语言 binding、package、ECS、`.ILX`、traineddata、硬件支持行、
   开发机绝对路径、缓存或设备日志。
8. worktree/index 干净，`EasyCon/` 仍 ignored、tracked count 为零且未被修改。

## Hardware Unverified 边界

本阶段可以冻结 software/native candidate，但不关闭 O-03，也不声明：

- 任一 capture card、stable identity、backend/profile、分辨率、pixel format、FPS 或 close SLO 已支持；
- OCR 中文模型已获准再分发或识别质量已达标；
- 真实 30/60 FPS throughput、2 秒 hot-unplug close、长稳、工作集或 native leak threshold 已通过。

这些结论必须由后续固定硬件、模型、许可证和原始测量证据关闭。无硬件 synthetic/fixture 结果不能替代。

## 禁止项

Phase 3 不得：

- 修改、跟踪、构建、复制、下载或把 `EasyCon/` 加入任何 manifest/build/test；
- 读取、合并或修改并行 Phase 2B 的未提交状态和独占文件；
- 改变 Phase 1 Runtime/Operation/event/close 语义；确需改变时必须先按 ADR-0007 和 ADR-0006 设计 Gate；
- 实现 ECS、Automation 跨域运行、正式 C ABI、public header、四语言 binding、package、UI 或网络服务；
- 把业务状态、retry、deadline、事件或长期线程移入 C++；
- 复制 EasyCon traineddata、依赖 cwd/PATH 模型、下载模型或把 missing model 伪造成成功；
- 用 fake 代替 actual OpenCV/Tesseract bridge，或用 synthetic capture 宣称硬件通过。

## 重新打开规则

以下变化必须先修改并 review 本 ADR：

- 改变依赖方向、Rust/C++ ownership、private bridge ABI ownership 或 Send/Sync 证明；
- 改变 Frame metadata、score、`.IL` active method/ROI、capture state/close order、pool fairness 或 O-03 边界；
- 增加 `.ILX`、Canny、non-normalized template、公共 C ABI、ECS、语言 binding 或支持平台；
- 更换 vcpkg baseline、OpenCV/Tesseract/Leptonica major/minor 或 sanitizer 门禁。

保持本目标的局部实现选择和 bug fix 不重新打开目标，但行为变化必须同步 fixture、测试和设计说明。实现冻结
后，production Vision/native、相关 fixture/spec 或 lifecycle test 的变化会重新打开 Phase 3 Candidate，直到
完整门禁、独立 review 和后续冻结记录推进 SHA。

## 关联

- [ADR-0001：外层 SDK 与 EasyCon 源码分离](0001-source-boundary.md)
- [ADR-0003：Rust 核心、私有 C++ 桥接与公共 C ABI](0003-core-native-abi-boundary.md)
- [ADR-0006：Runtime 所有权、终态事务与确定性关闭](0006-runtime-stabilization.md)
- [ADR-0007：冻结 Phase 1 Runtime 基线](0007-phase-1-freeze.md)
- [架构总览](../architecture/architecture-overview.md)
- [生命周期与并发](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
- [构建、发布与合规](../architecture/build-release.md)
- [实施路线](../architecture/repository-roadmap.md)
