# 0012：冻结 Phase 3 Vision 与跨平台私有 native bridge 开发目标

- 状态：Frozen Target (`Hardware Unverified`)
- 日期：2026-07-21
- 开发起点：`9944dba50adc34484b65206e07ea0a444103f656`
- 跨平台接管起点：`9a1f6a57cf5b17c606b3c594b24cf25331aad302`
- 集成基线：`main@41c5f0c2b19165769d4aa8e4512ad46280a1415b`
- 实现状态：未完成；本 ADR 冻结目标，不冻结实现、公共 C ABI 或发布包

## 背景

Phase 1 Runtime 已按 [ADR-0007](0007-phase-1-freeze.md) 冻结，Phase 2A Controller/Serial Candidate 已按
[ADR-0009](0009-phase-2a-freeze.md) 冻结为 `Hardware Unverified`。Phase 3 需要在不改变这些冻结语义、
不进入 ECS 或公共 C ABI 的前提下，实现 Vision 与 OpenCV/Tesseract/capture 的私有原生边界。

本目标以只读 EasyCon 源码事实为兼容依据，但 `EasyCon/` 仍受
[ADR-0001](0001-source-boundary.md) 约束，不成为构建、测试、fixture、submodule 或下载输入。完整实现设计见
[Phase 3 Vision 与私有 native bridge 设计](../development/phase3-vision-native-design.md) 与
[Phase 3 跨平台边界设计](../development/phase3-cross-platform-design.md)。本 ADR 的编号由 Phase 3 原先的
`0011` 调整为 `0012`，因为 `main` 的 `0011` 已冻结 Phase 2B Qualification Software Candidate；本决定不覆盖、
替换或重写 Phase 2B ADR。

## 跨平台目标增补

Phase 3 在 Node F checkpoint 后增加跨平台收口，但不扩大到公共 C ABI、语言 binding、完整 SDK 发布或 Phase 4。
平台状态固定如下：

| 平台 | Phase 3 目标状态 | 允许声明 | 明确禁止 |
| --- | --- | --- | --- |
| Windows 10/11 x64 | v1 Tier 1 正式目标，Vision `Hardware Unverified` | 已执行的 MSVC/native/synthetic/file fixture 软件证据 | 未测试 capture card、profile、FPS、close SLO 或完整 SDK GA |
| Linux x64 | v1 正式目标方向，Vision Build Candidate；按实际证据标记 Passed 或 Build Unverified，硬件始终单列 | 实际 Linux build/native fixture/synthetic 与预验证 file capture 的结果 | 用 Windows 结果替代 Linux、把 Vision 候选写成 serial/四语言/package 已支持 |
| macOS Apple Silicon arm64 | `Experimental Source Candidate / Build Unverified / Hardware Unverified / Not Shipped` | fail-closed 架构选择点、最小 unavailable adapter、未来验证 handoff | AVFoundation 空成功、未编译的大段平台代码、binary/package、Intel 或 universal 支持声明 |

Windows 仍是 v1.0 GA 的唯一 Tier 1 发布目标。Linux 是 v1 正式目标方向，但必须在 Vision build 之外继续完成
Controller serial、硬件矩阵、四语言包和发布工程才可晋级完整 SDK 支持。macOS 只形成 Apple Silicon arm64
实验源码候选；至少一次真实 macOS 编译通过是实现完整 AVFoundation backend 或合并较大 macOS 专属生产代码的
前置条件，云端 Mac 软件门禁不能替代实体摄像头与 USB/CH32 资格。

跨平台收口不得改变以下平台中立语义：Frame/Image/Label、Capture 五态、operation、latest slot、pool、取消、
deadline、fault、close、唯一 worker handoff/join 和资源所有权。Windows 类型、HRESULT、HANDLE、COM、DirectShow、
Media Foundation 与平台库只能出现在 `platform/windows` 实现或私有 detail；Rust 公共语义和未来公共 C ABI 不得
依赖它们。

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
- platform-selected OpenCV capture adapter；Windows 只在 `platform/windows` 提供 DirectShow/Media
  Foundation，Linux 只预留 V4L2 admission 边界，macOS 本轮只提供显式 unavailable；
- bridge-owned buffer/error/opaque handle 的创建与释放。

C++ 禁止实现 operation、event、deadline、retry、label parser、score policy、业务状态机、长期线程、
用户 callback 或 Rust 回调。

### 3. 私有 C-compatible 边界

bridge 只暴露内部前缀 `easycon_native_*`，使用调用约定宏、固定宽度标量、显式长度 UTF-8、
opaque handle、status、owned error 和 owned buffer。不得出现 public `easycon_v1_*` symbol 或安装 header。
Windows x64 宏展开为 `__cdecl`；Linux/macOS 使用平台 C ABI。边界禁止 C++ STL、C enum layout、OpenCV enum、
`long`、`wchar_t`、HRESULT、HANDLE 或其他平台指针布局。

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
  是 `Send + Sync`，只设置 bridge 原子 stop/请求 backend wake，且不向 caller 暴露；close 撤销 cancellation/
  deadline hooks、从 worker exit payload 收回内部 clone，并证明全部 token 已释放，随后才 destroy capture handle；
- bridge-owned buffer/error 不跨线程长期存活，safe wrapper 在同一次调用中复制后释放。

除上述证明外不增加 `unsafe impl Send/Sync`。native debug counters 记录 live handles 和 owned allocations；
所有 success/failure/cancel/timeout/fault/close 测试结束必须回到调用前基线。

### 5. Frame、Image、ROI 与限制

Frame 和 Image 都不可变，像素由 Rust `Arc<[u8]>` 拥有。元数据固定包含 width、height、stride、format；
Frame 另含严格递增 sequence 和 Runtime clock 的 monotonic `timestamp_ns`。格式只允许 BGR8、BGRA8、Gray8。

硬上限由 `VisionLimits` 提供并有不可放宽的编译期 ceiling：encoded bytes、width、height、pixel count、
decoded bytes、stride、label count、JSON bytes、Base64 bytes、OCR text bytes 和 queued native jobs。所有
计算使用 checked arithmetic；stride 至少为 row bytes，`stride * height` 不得溢出或超过 buffer。

BMP/PNG decode 必须在 `cv::imdecode` 前结构化读取 header，按允许的 bit depth/color type 计算最坏 decoded
bytes 并应用 ceiling；decode 后再次复验。PNG encode 在 `cv::imencode` 前按 raw scanline、filter byte、zlib
`compressBound` 和 chunk overhead 计算保守 output ceiling，返回后再次复验。limit 不能只在 OpenCV 已完成
潜在大分配之后检查。

ROI 是半开区间 `(x, y, width, height)`，宽高必须非零且完全位于图像内。Phase 3 ROI 产生独立、紧密排列的
immutable Image，避免跨 ABI 冻结 strided subview lifetime；native 同步调用仍接受显式 stride 的 borrowed
input。空 ROI 明确失败，不隐式解释为全图。

### 6. Capture 状态机与关闭

状态固定为 `Opening`、`Streaming`、`Faulted`、`Stopping`、`Closed`。创建资源后由一个 Rust-owned、
Runtime-supervised read thread 执行 native open 和所有 read。只有首个有效 Frame 发布后才进入 Streaming；
首帧 deadline 到期、open/read error 或无效帧产生稳定 fault。

首帧 deadline 由 session-owned Runtime Operation 和 Runtime Clock 驱动，不创建 timer thread。deadline hook
原子把 Opening 转为 Faulted、请求 interrupt 并唤醒 waiter；worker 完成 backend cleanup 后才提交该内部
operation 终态。VirtualClock 必须能确定性触发此路径。

latest slot 只保存 `Option<Arc<Frame>>`。替换不会修改旧 Frame；snapshot 在同一个锁/condvar 协议中返回当前
强引用。等待 snapshot 的 caller 可被 resource cancellation、operation deadline、fault 或 close 确定性唤醒，
不使用随机 sleep。

OpenCV device backend 只在 open/read timeout 参数由选定 backend 明确接纳并能把单次 open/read 限制在冻结
quantum 内时 admission；否则在 open 前返回 unsupported，不进入支持矩阵。interrupt 只设置独立原子 control
并唤醒 backend 已证明安全的 wait，不与 `VideoCapture::read` 并发调用 `release`。bounded read 返回后由唯一
worker 观察 stop。file backend 使用同一有界规则。

close 顺序固定为：

1. 在 admission gate 内进入 Stopping，拒绝新 snapshot/open work并取消 resource token；
2. 调用 bridge interrupt 或 synthetic backend unblock；
3. 唤醒首帧/snapshot waiter；
4. read worker 完成 backend close，把唯一 closed handle 通过 one-shot channel 交回 owner 并退出；
5. join 唯一 read thread；
6. 撤销 cancellation/deadline hooks，从 worker payload 收回内部 interrupt clone，seal control 并等待 active
   request guard 归零，证明全部 token 已释放后销毁 interrupt control；
7. destroy closed native handle并取得该 handle 的 consumed acknowledgement；
8. 清空 latest slot；隔离测试另确认 native handle/allocation counts 回到基线；注销 Runtime resource并进入 Closed；
9. 重复 close 返回同一结果且不重复 native side effect。

显式 `CaptureSession::close` 和 `ManagedResource::close` 才执行上述确定性协议。Capture 最后 owning Drop 只在
admission gate 内拒绝新工作、请求 resource cancellation/interrupt 并唤醒 waiter；不等待、不 join、不注销、
不执行 backend callback，也不声称 Closed。

Runtime registry 只持有 `Weak<dyn ManagedResource>`，因此 Capture 使用与 public session 外壳分离的
`CaptureResource`。登记成功后，它把自己的强 `Arc` 和 `ResourceRegistration` 放入 private retention cell；
public `CaptureSession` 在 Phase 3 不实现 Clone，caller 需要共享时可使用 `Arc<CaptureSession>`。最后 session
Drop 只触发上述 fallback，retention 继续使 Runtime 能 upgrade 并执行 `ManagedResource::close`。只有显式 close
已通过 per-handle consumed acknowledgement 证明 join 和 destroy 完成时才 unregister 并拆除 retention；清理
不可证明时必须保留二者，不能靠字段 Drop 自动注销。global debug counters 只作隔离测试证据，不参与并发
production session 的 cleanup 判定。未调用 session/Runtime close 的程序不得声称确定性清理或资源归零。

worker 的 one-shot send 若因 receiver 已被异常路径取走而失败，必须把 `SendError` 中的 closed handle 和 worker
interrupt token 移入 resource-owned fallback handoff slot；worker 线程绝不析构该 handle。后续 close 在 join 后
从 channel 或 fallback slot 取回并 destroy。两处都拿不到 owner、token clone 未归还或 destroy 无 consumed ack
属于 cleanup-unproven，保留 retention/registration 并失败。

worker closure 在 Runtime supervisor 外再设置内层 `catch_unwind`，唯一 handle 始终由外层 `WorkerOwnerGuard`
持有。run/open/read/publish panic 时，guard 仍在 worker 上执行 noexcept/backend close，把 handle、worker token 和
panic marker 送到 exit/fallback，disarm 后才 `resume_unwind` 让 supervisor 记录 panic。guard Drop 是最后防线：
从 poisoned mutex 恢复并把 payload 放入唯一 fallback slot，绝不在 worker 上执行 handle destroy；确定性 failpoint
覆盖 open 后、read 中、publish 和 close-to-send 间的 panic。

construction guard 覆盖 startup Operation、interrupt control、backend owner、blocked worker、registration、retention
和 handoff。构造先在 private `Constructing` 状态持有 lifecycle gate，创建 Operation并spawn worker；worker停在
Run/Abort barrier，不能提前open。`register_resource` 是最后一个可失败步骤和唯一 commit linearization point：成功后
在仍持 gate 时以不失败的步骤安装 registration/self-retention、把 barrier设为Run并进入Opening；失败则设Abort，
guard join/handoff/destroy、终结Operation且回滚。并发Runtime close在register前会令register失败；在register成功
后可upgrade但必须等待同一gate，随后只看到完整Opening或已回滚Aborted，cleanup owner不会重叠。每个构造barrier
都注入并发Runtime close并验证Runtime/native counts回基线。

backend close 返回错误但 per-handle destroy consumed ack 已确认时，session 保存 diagnostic、返回显式 close
error 并可注销 resource；Runtime 仍可真实 Closed。若 worker join、内部 token 回收或 handle destroy无法确认，
retention/registration 必须保留，使现有 Runtime registry convergence 或 supervised task panic 形成 CloseFailed；
禁止通过 panic 模拟普通错误或忽略失败。若该通道不足，必须先按 ADR-0007 重开 Runtime，不能在 Phase 3 偷改
callback 签名。

explicit destroy 返回 `Consumed`（pointer已清零，可同时带diagnostic）或
`Unconsumed { handle, diagnostic }`，Rust 不从status猜ownership。Unconsumed/no-ack时 close 在返回前把仍 armed 的
唯一 owner 原子放回 resource `unresolved` slot，不允许 stack Drop 重试副作用；retention/registration 保留，后续
close可显式重试。测试分别注入 destroy-before-consume、consume-with-error 和 protocol no-ack。

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
target；OCR label 对当前 Frame 的 Target ROI 识别。Target 必须完全包含于 Range；图像 label 的 embedded
width/height 必须精确等于 Target width/height。target/range 超界、空目标、未知 mode、
duplicate name、limit 和 decode error 都产生稳定 diagnostic/error，不打印后跳过。

registry 先按 normalized source path/name 的 UTF-8 byte order稳定排序，再构造；同名是显式 duplicate error，
不采用枚举顺序中的首个。Label immutable。一次 `evaluate(label)` 在入口只取得一个 `Arc<Frame>`，所有 ROI、
template/OCR 和结果都借用该 Frame；同次 evaluate 期间即使 latest 更新也不换帧。结果 score 始终 `0.0..1.0`。
未来 ECS 的 `0..100` 转换属于 Phase 4，本阶段不冻结 rounding。

OCR score 只对 Tesseract actual text 两端执行 Unicode scalar `is_whitespace` trim，expected label text 不 trim。
两者按 Unicode scalar 计算 Levenshtein distance：双空 similarity 为 1、单空为 0，否则
`1 - distance / max(actual_len, expected_len)`；先把 finite similarity 与 mean confidence 分别 clamp 到 `0..1`，
再相乘并最终 clamp。非 finite confidence 是 backend error。返回的 OCR text 保留 bounded raw UTF-8，fixture
固定尾随 CR/LF、Unicode whitespace、双空、单空、完全匹配和替换路径。

`.ILX` 没有 parser、loader、extension dispatch 或 API 入口。

### 9. Native pool 与 OCR cache

Rust native pool 有固定 worker/permit 上限、FIFO ticket admission、有限 queue、取消和 close。取消发生在排队
阶段时不执行 native call；执行中无法由第三方库安全中断的调用完成后才提交 Cancelled，不能提前释放借用
buffer/engine。close 拒绝新 admission、取消 queued job、等待 in-flight job 返回并 join Rust workers。

pool 固定使用 `Runtime::spawn_supervised` 创建 Rust workers；不允许以调用线程同步执行作为另一种实现，也不
允许 detached `thread::spawn`。OCR cache 的 `creating` 与 `borrowed` 分别计数；engine 在锁外 create 时先
增加 creating，close 必须等待两者均为零。create 返回后重新检查 close/cancel，必要时立即 destroy 并回滚。

OCR cache key 至少包含 canonical explicit model root、language、engine mode 和 PSM。路径不从 cwd、PATH、
环境变量或 `EasyCon/` 推导；缺模型返回 `MODEL_NOT_FOUND` 等价错误，不联网下载。O-03 未关闭前不提交
任何 EasyCon traineddata，也不要求成功中文 OCR fixture。实际 create/process/reuse/release 门禁使用独立的
test-only `tessdata_fast` English model：upstream tag `4.1.0`、Apache-2.0、4,113,088 bytes、SHA-256
`7D4322BD2A7749724879683FC3912CB542F19906C83BCC1A52132556427170B2`。该资产必须带来源/许可证/hash manifest，
只供 component test，不进入 package default 或关闭 chi_sim O-03。model bytes 和 upstream LICENSE 都不 tracked、
不打包，也不由 Runtime 下载；tracked manifest 固定两个 HTTPS source URL、size/hash 和 LICENSE 的 11,358 bytes、
SHA-256 `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30`。显式 test provisioning tool 只写 ignored
`.tools/vision-models`，逐项验 hash 后把路径显式传给 component test；未 provision、hash/license 不匹配或成功
OCR test 被 skip 都使完整门禁失败。缺模型、bad-image、exception和poison仍是必测路径。

### 10. 工具链、依赖和许可证

Windows Tier 1 目标是 Windows 10/11 x64、MSVC v143、C++20、CMake/Ninja。Linux x64 candidate 使用同一
C++20、OpenCV/Tesseract/Leptonica 版本和 registry baseline；macOS arm64 只保留 experimental source 选择点，
没有 Xcode/Apple SDK 证据时不得产生通过状态或 binary。vcpkg registry 锁固定为官方 release
`2026.06.24` 的 commit `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`，`x64-windows-static-md`，并解析：

- OpenCV `4.12.0#5`，关闭 default features，只启用 core/imgproc/imgcodecs/videoio 所需的
  `dshow`、`msmf`、`png`、`jpeg`、`intrinsics`、`thread`；
- Tesseract `5.5.2`；
- Leptonica `1.87.0`（Tesseract transitively required）。

Windows 自有 C++ target 使用 `/MD`、`/W4 /WX /permissive- /EHsc /Zc:__cplusplus`；Linux owned target 使用
Clang/GCC 的 `-Wall -Wextra -Wpedantic -Werror`，两者均将第三方 include 标为 system。
MSVC Debug/Release、clang-cl ASan、clang-cl UBSan、clang-tidy、MSVC `/analyze` 和固定 fuzz seed 都是 native
门禁。ASan/UBSan preset 或 runtime 不兼容会阻断冻结；任何替代门禁都必须先修改并独立 review 本 ADR，
不能在实现后静默跳过或用 counters 冒充 sanitizer coverage。

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
- 复制 EasyCon traineddata、依赖 cwd/PATH 模型、让 Runtime/library 下载模型或把 missing model 伪造成成功；
- 用 fake 代替 actual OpenCV/Tesseract bridge，或用 synthetic capture 宣称硬件通过；
- 用返回成功的空 native stub 冒充 Linux/macOS backend，或在没有真实 build/package/hardware 证据时作平台支持声明。

## 重新打开规则

以下变化必须先修改并 review 本 ADR：

- 改变依赖方向、Rust/C++ ownership、private bridge ABI ownership 或 Send/Sync 证明；
- 改变 Frame metadata、score、`.IL` active method/ROI、capture state/close order、pool fairness 或 O-03 边界；
- 增加 `.ILX`、Canny、non-normalized template、公共 C ABI、ECS、语言 binding，或把任何 Candidate/Experimental
  平台晋级为 shipped/supported；
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
- [Phase 3 跨平台边界设计](../development/phase3-cross-platform-design.md)
