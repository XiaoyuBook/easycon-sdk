# Phase 3 Vision 与私有 native bridge 开发设计

## 1. 目的和状态

本文把 [ADR-0011](../decisions/0011-phase-3-vision-native-target.md) 的冻结目标细化为可逐节点实现、验证和
审查的内部设计。起点固定为 `9944dba50adc34484b65206e07ea0a444103f656`。完成状态只能是
`Hardware Unverified Vision Candidate`，不等于公共 C ABI、binding、发布包、capture hardware 或 OCR model
release candidate。

本文中的类型和函数名是 Phase 3 Rust/private bridge 内部契约，可在 Phase 3 review 中调整；它们不是
`easycon_v1_*` 公共 ABI 承诺。

## 2. 证据到实现的映射

| 源码事实 | Phase 3 保留 | Phase 3 修正或新增 |
| --- | --- | --- |
| `.IL` JSON、文件名 label、numeric method、Range/Target、BMP target | 字段含义、活动 method、BMP/PNG decode | strict limits、稳定排序、duplicate/invalid diagnostic、immutable Label |
| SqDiff/CCorr/CCoeff normalized score | min/max location 和 score 映射 | clamp 到 `0.0..1.0`，ECS integer conversion 延后 |
| XY/Laplacian preprocessing | exact OpenCV steps | fixture 显式容差和 invalid error |
| OCR chi_sim/SingleLine、text similarity * confidence | 默认兼容配置和 score 公式 | explicit model root、engine cache、missing model、UTF-8 scalar distance |
| synchronous mutable Mat/VideoCapture | OpenCV low-level capability | Rust Frame ownership、one reader、latest slot、interrupt/join、fault state |
| decode/encode 错误吞成空值 | BMP/PNG/conversion capability | explicit status/error、limits、allocation counters |
| HSV helper only | 无 legacy detection claim | bounded HSV ROI count/ratio/bbox |
| `.ILX` isolated/inconsistent | 无 | 完全排除入口 |

OCR legacy 使用 .NET UTF-16 char edit distance；Phase 3 Rust 不复制 surrogate-specific 行为。v1 expected text
输入和 output 都是 strict UTF-8，score 使用 Unicode scalar values。ASCII 和常用 BMP 中文与源码一致；任何
non-BMP 差异要作为 corrected fixture 明示，未来 ECS 不得重新实现另一套距离函数。

## 3. 组件和构建图

```text
Cargo workspace
  easycon-vision
    -> easycon-runtime
    -> easycon-model
    -> easycon-native-sys
         -> build.rs / CMake private static bridge

Root CMake
  easycon_native_bridge (STATIC, private include)
    -> OpenCV core/imgproc/imgcodecs/videoio
    -> Tesseract
    -> Leptonica transitively
  easycon_native_bridge_tests
  fuzz seeds / sanitizer component targets
```

根 `CMakeLists.txt` 只编排 private bridge/component tests，不生成 public SDK library 或 install header。
`native/bridge/include/internal/easycon_native_bridge.h` 只被 bridge、component tests 和 generated Rust binding
declaration审计使用；没有 `install()`。symbol visibility 默认为 hidden，Windows static library 不导出 DLL symbol。

`easycon-native-sys/build.rs` 只消费 repository-relative source 和环境提供的 toolchain。它不得硬编码 VS、
vcpkg、OpenCV、Tesseract 或 worktree 绝对路径。Cargo rerun inputs覆盖 bridge header/source/CMake/vcpkg files。

## 4. vcpkg 和 preset

tracked inputs：

- `vcpkg.json`：manifest，OpenCV defaults off，精确 feature，Tesseract；
- `vcpkg-configuration.json`：builtin registry baseline
  `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3`；
- `cmake/triplets/x64-windows-static-md.cmake`：x64、static library、dynamic CRT；
- `CMakePresets.json`：MSVC Debug/Release、clang-cl ASan/UBSan、clang-tidy、MSVC `/analyze` 和 fuzz
  configure/build/test presets。

binary dirs 使用 `cmake-build-${presetName}`，vcpkg installed tree 位于 binary dir。`VCPKG_ROOT` 只从环境读取。
本机官方 bootstrap 记录为 vcpkg tool `2026-07-13-bf04c909169fdbb30821c02c6eb01f1cd1295d05`；工具版本
记录在验证文档但不把用户目录写入 tracked file。registry baseline 才是 ports/dependency lock。

## 5. Private bridge ABI

### 5.1 基础约定

header 仅使用 C-compatible declarations：

```c
typedef int32_t easycon_native_status;

typedef struct easycon_native_error {
    int32_t code;
    char* data;
    uint64_t length;
} easycon_native_error;

typedef struct easycon_native_buffer {
    uint8_t* data;
    uint64_t length;
} easycon_native_buffer;

typedef struct easycon_native_image_view {
    const uint8_t* data;
    uint64_t length;
    uint32_t width;
    uint32_t height;
    uint64_t stride;
    uint32_t pixel_format;
} easycon_native_image_view;
```

最终 header 会为每个 struct 加 C++ `static_assert` 验证 x64 size/alignment/offset。status 是固定 `int32_t`，
format/mode 是固定 `uint32_t` 常量，不用 C enum layout。所有 function 形态：

```c
easycon_native_status EASYCON_NATIVE_CALL easycon_native_xxx(
    /* borrowed input */, /* zeroed out */, easycon_native_error* out_error) noexcept;
```

返回 OK 时 error/data 必须为空；失败时所有其他 out 保持零值。`length == 0` 才允许 input data NULL。
UTF-8 是 pointer + uint64 length，不依赖 NUL；路径和 language 拒绝 embedded NUL。uint64 到 `size_t` 转换先
检查 `<= SIZE_MAX`。x64 `cdecl` 宏在 Windows 明示 `__cdecl`。

### 5.2 status 分类

private status 至少包含：OK、INVALID_ARGUMENT、OUT_OF_RANGE、OVERFLOW、RESOURCE_EXHAUSTED、NOT_FOUND、
MODEL_NOT_FOUND、INVALID_IMAGE、NO_FRAME、CANCELLED、BACKEND_ERROR、CV_EXCEPTION、STD_EXCEPTION、
UNKNOWN_EXCEPTION、ALLOCATION_FAILED、INTERNAL。

这些数值只在 Phase 3 private boundary 使用，不能复制成未来 public ABI code。`easycon-native-sys` 立即映射为
`NativeErrorKind` 并复制 message。Rust 不解析 message 做控制流。

### 5.3 owned memory

`easycon_native_buffer_release` 和 `easycon_native_error_release` 都接受零值并幂等清零调用者 struct。bridge
使用 `new[]`/`delete[]` 在同一模块配对，live allocation counter 在成功分配后递增、释放后递减。Rust safe
wrapper先校验 returned pointer/length invariant，再复制到 `Vec<u8>`/`String`，最后通过 guard 无条件 release；
UTF-8 output 非法视为 INTERNAL 且仍释放。

### 5.4 exception trampoline

所有 entry 实现调用统一 `guard(out_error, lambda)`：

1. 验证 out_error 可写并清零；
2. 执行 lambda；
3. 捕获 `cv::Exception` -> CV_EXCEPTION；
4. 捕获 `std::bad_alloc` -> ALLOCATION_FAILED；
5. 捕获其他 `std::exception` -> STD_EXCEPTION；
6. 捕获 `...` -> UNKNOWN_EXCEPTION；
7. error message 分配失败时仍返回原 status，error 保持空，不二次抛出。

component tests 通过 test-only `easycon_native_test_raise(kind)` 触发三类异常。该 symbol 只在 test library
定义，production static target不含 failpoint。

## 6. `easycon-native-sys`

crate root使用 `#![deny(unsafe_code)]`；只有 `#[allow(unsafe_code)] mod ffi` 和一个私有 `call` module 可以包含
unsafe。FFI declarations 与 header 常量由人工双向 layout test 保持一致，本阶段不引入 bindgen/LLVM runtime
build依赖。

safe surface按能力拆分：

```text
codec::{decode, encode_png, convert, crop}
matching::{match_template, match_edge}
color::detect_hsv
ocr::{OcrEngine, OcrConfig, OcrOutput}
capture::{discover, CaptureHandle, CaptureInterrupt, NativeFrame}
debug::native_counts (cfg(test) or internal diagnostics)
```

### 6.1 RAII 与 thread traits

`OcrEngine` 包含 `NonNull<opaque>`，Drop 调 destroy。它允许移交给另一个 worker，因此有经过注释证明的
`unsafe impl Send`；不实现 Sync。所有 process method 需要 `&mut self`。

`CaptureHandle` 由 read worker 独占且是 Send/not Sync。`CaptureInterrupt` 持有 bridge-defined ref-counted
interrupt control，只有 atomic request/wake function，因此是 Send+Sync。handle destroy 前必须从 join path
取得独占 owner；safe API不提供 clone handle。若 bridge 无法证明 interrupt 与 read 并发安全，该 backend
open 必须返回 unsupported，不能以 mutex 把 close 永久阻塞在 read 后面。

每个 unsafe impl 的同模块测试用 compile-time trait assertion固定允许和禁止集合。

## 7. Frame、Image 和 Buffer

```rust
pub enum PixelFormat { Bgr8, Bgra8, Gray8 }

pub struct Image {
    pixels: Arc<[u8]>,
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
}

pub struct Frame {
    image: Image,
    sequence: u64,
    timestamp_ns: u64,
}
```

constructors验证：width/height非零；channels固定；`row_bytes = width * channels` checked；
`stride >= row_bytes`；`required = stride * height` checked；buffer length至少 required且不超过 limits。
codec/native output统一复制为 tight stride，capture backend可以提供有 padding 的 stride；Frame 仍完整拥有 bytes。

公开 pixels access返回 immutable `&[u8]` 和 row iterator，不返回 `&mut`。`Image::crop` 复制每一行的有效
row bytes到 tight buffer，checked coordinate/offset。`Frame::crop`返回 Image，不继承 sequence/timestamp。

native view只在同步 call栈中创建；safe wrapper持有 `&Image` 到 call返回。异步 job在提交时 clone Image/Frame
的 Arc owner，worker完成前 owner不释放。

## 8. Codec

decode流程：

1. Rust拒绝 empty/encoded length limit；
2. native先识别BMP/PNG并做header preflight：BMP检查file/DIB header、signed dimensions、planes、bpp和
   compression；PNG检查signature、首个IHDR length/type、width/height、bit depth和color type；
3. preflight用checked arithmetic按最多4 channels计算width/height/pixels/decoded bytes，超过ceiling时在
   OpenCV分配前失败；
4. native用 `cv::imdecode(IMREAD_UNCHANGED)`；
5. 拒绝 empty、depth非 `CV_8U`、channels非 1/3/4，并对实际dimension/pixel/byte再次复验；
6. clone到连续 owned Mat，复制进 bridge buffer，返回 format/width/height/stride；
7. Rust再次验证 metadata和 length后接管为 Image。

BMP legacy和PNG current都必须有 synthetic fixture。invalid header、truncated BMP/PNG、oversized header、
decompression limit、unsupported depth/channels和zero dimension都是显式 error。任何情况不返回空成功。

encode只支持 PNG candidate。调用 `cv::imencode(".png")` 前，bridge按 `height * (row_bytes + 1)`、zlib
`compressBound` 和保守PNG chunk overhead做checked upper-bound；upper-bound超过output ceiling时拒绝。返回后
再检查实际bytes。round-trip断言 metadata/pixels；有损格式不进入 Phase 3。conversion覆盖 BGR<->BGRA、
BGR/BGRA->Gray、Gray->BGR/BGRA，alpha新增时固定255。

## 9. Template 和 edge

native输入是两个 image view和 method。Rust先保证格式相同或执行明确 conversion，target不得大于 search。
native调用 `cv::matchTemplate`，返回 raw min/max value和对应 location；Rust根据 method选择并归一化。

edge fixture需要同时保存 preprocess expected hash/pixels和final match expected，避免只测“同一个错误算法匹配
自身”。XY严格使用 `cv::Sobel(..., CV_16S, 1,0,-1)` / Y和 `addWeighted`; Laplacian参数固定。edge转换
output是 Gray8；最终 `TM_CCOEFF_NORMED`。constant image导致NaN/Inf时返回 deterministic vision error，
不把非有限值 clamp成成功。

result：

```rust
pub struct MatchResult { pub x: u32, pub y: u32, pub score: f32 }
```

`x + target.width <= search.width` 和 y同理必须成立；否则是 INTERNAL/native contract error。

## 10. HSV color

Rust `HsvRange` 固定 OpenCV尺度。native先crop ROI，再按 BGR/BGRA/Gray明确转换到BGR后 `cvtColor(BGR2HSV)`。
normal hue执行一个 `inRange`；wrap执行 `[h_min,179] OR [0,h_max]`。`findNonZero`/boundingRect只在 count>0
调用。返回 count和ROI相对 bbox；Rust用checked ROI area算 ratio，并把bbox转换为输入 image绝对坐标。

空ROI、S/V反向、count>area、bbox越界或非有限 ratio失败。threshold是调用者对ratio的比较策略，native不拥有
业务成功/失败；Label Phase 3不新增color method。

## 11. OCR engine pool

`OcrConfig`包含 explicit canonical model root、language、engine mode、PSM和最大output bytes。canonicalization
只用于cache key和path containment；不存在的root/model在create前返回 ModelNotFound。禁止 fallback到cwd。

bridge使用 `tesseract::TessBaseAPI`，create时 `Init(root, language, mode)`，process时从 image view调用
`SetPageSegMode`、`SetImage`、`Recognize`、`GetUTF8Text`、`MeanTextConf`，并在每次复用前 `Clear`。engine mode
和PSM只接受bridge明确映射的固定值。`GetUTF8Text()` 立即进入 `std::unique_ptr<char[]>` guard；用有界
`strnlen(max_output_bytes + 1)` 检查NUL和上限，所有success/error/exception路径都 `delete[]`。UTF-8 text复制到
bridge-owned buffer，confidence转换到 `0.0..1.0`。bad image、recognize error和oversized output显式失败。

成功OCR component test使用与EasyCon无关的test-only模型：`tesseract-ocr/tessdata_fast` tag `4.1.0` 的
`eng.traineddata`，Apache-2.0，4,113,088 bytes，SHA-256
`7D4322BD2A7749724879683FC3912CB542F19906C83BCC1A52132556427170B2`。fixture同时跟踪upstream URL、LICENSE
hash和model hash；它不进入package/default lookup，也不关闭O-03的chi_sim来源与再分发问题。

`OcrPool` state：

```text
open: true/false
max_engines
created
creating
idle Vec<OcrEngine>
waiters FIFO ticket queue
borrowed count
Condvar changed
```

acquire只允许队首在 idle非空或 `created + creating < max` 时推进。cancel hook只notify condvar；wait loop读取
token并移除自己的ticket。engine create在锁外完成，但先增加creating；返回锁内后减少creating并重新检查
closed/cancelled，若已关闭或取消就立即destroy，否则增加created并发出lease。失败回滚并notify。lease Drop正常
归还；poison flag导致destroy并减少created。close标记closed、唤醒所有waiter，等待
`creating == 0 && borrowed == 0` 后destroy idle；重复close幂等。

OCR native exception、unknown exception、engine-invalid status都poison；普通 expected text mismatch不是poison。
missing model发生在engine create，不占用永久slot。fairness测试使用ticket/barrier，不用wall-clock sleep。

## 12. Native compute admission

模板、edge、color、codec和OCR共享 `NativePool` 的有界 admission。Phase 3固定为FIFO job queue和固定数量的
Runtime-supervised workers，不提供调用线程同步执行的第二种模式：

- total permits和queued waiters有硬上限；
- ticket顺序稳定；
- queued cancel不调用native；
- in-flight cancel保留所有borrowed owner，native返回后提交cancel；
- close拒绝新ticket、取消queued、等待in-flight归零并join全部worker；
- panic被Rust boundary捕获，permit guard仍归还，operation失败且其他job继续。

worker只通过 `Runtime::spawn_supervised` 创建，不能用 detached `thread::spawn`。pool本身作为
ManagedResource登记并在close中unregister。job closure/result slot保留所有Frame/Image/engine owner到worker返回。

## 13. `.IL` parser 和 Label

```rust
pub enum LabelMethod {
    SqDiffNormed,
    CCorrNormed,
    CCoeffNormed,
    EdgeXy,
    EdgeLaplacian,
    Ocr,
}

pub enum LabelTarget { Image(Image), Text(Arc<str>) }

pub struct Label {
    name: Arc<str>,
    source: Arc<str>,
    method: LabelMethod,
    range: Roi,
    target_roi: Roi,
    target: LabelTarget,
}
```

parser先检查JSON bytes limit和strict UTF-8，再用 `serde_json` typed raw struct。integer从JSON number checked到i64/
u32，拒绝fraction/negative/out-of-range。字段名按legacy精确大小写；缺字段使用源码default 0，但最终ROI验证会
拒绝zero size。未知字段记录stable warning diagnostic；duplicate JSON key必须拒绝，不能依赖last-wins。

由于serde_json默认不能直接报告duplicate key，使用custom Visitor逐key解析并维护seen set。Base64使用strict
standard alphabet/padding；decode前用encoded length推导上限。image method load时立即native decode，确保Label
发布后不会延迟暴露坏target。Target ROI必须完全包含于Range ROI；image target decoded width/height必须
精确等于TargetWidth/TargetHeight。OCR也保留Target containment检查，但只对当前Frame的Target ROI识别。

registry builder输入 `(source_name, bytes)`，先按source_name UTF-8 bytes稳定排序，逐项parse，收集全部有界
diagnostic；任何error则不发布partial registry。duplicate label name对每个冲突source给diagnostic。name normalization
只做strict UTF-8和非空/NUL/length验证，不做locale case folding。

parser corpus至少包括：BMP、PNG、OCR text、unknown legacy fields、missing/default、unknown method、string enum、
fraction/negative/overflow ROI、invalid UTF-8、duplicate JSON key、duplicate label name、bad/missing padding Base64、
decoded limit、embedded/Target dimension mismatch、Target不在Range、frame-out-of-bounds evaluate、`.ILX`
extension rejection。fuzz seeds来自这些自有文本，不复制 EasyCon fixture。

## 14. Label evaluate

`LabelEvaluator::evaluate(&Label, Arc<Frame>, ...)`在入口接收一个Frame owner：

- image label：validate frame Range，crop/borrow range，match embedded target，result location转换为frame绝对位置；
- OCR label：validate frame Target ROI，OCR一次，用rolling two-row算法计算Unicode scalar Levenshtein similarity
  和confidence product；expected/actual scalar count及checked `n * m` edit-cell count都有独立hard ceiling，
  超限在分配/循环前失败；
- result始终带输入 Frame sequence/timestamp，使caller可证明同一帧；
- score clamp到0..1，只对finite input；OCR output保留bounded UTF-8 text；
- evaluator不再次访问CaptureSession latest slot。

如果便捷API从CaptureSession evaluate，它先调用snapshot一次，再转给上述函数。测试在evaluate barrier期间发布
下一Frame，断言结果仍携带旧sequence且native借用结束前旧buffer未drop。

## 15. Capture backend interface

Rust内部trait拆分reader和interrupt：

```rust
trait CaptureBackend: Send + 'static {
    fn open(&mut self, request: &CaptureRequest) -> Result<CaptureProfile, VisionError>;
    fn read(&mut self, cancel: &CancellationToken) -> Result<CaptureRead, VisionError>;
    fn close(&mut self) -> Result<(), VisionError>;
    fn interrupt(&self) -> Arc<dyn CaptureInterrupt>;
}

trait CaptureInterrupt: Send + Sync {
    fn request_stop(&self);
}
```

实际实现可在构造时先取得interrupt control，read/open/close仍只在read thread。`CaptureRead`是Frame输入或
deterministic End/Fault；empty native Mat永远不是成功Frame。

OpenCV backend 不通过并发 `VideoCapture::release` 打断read。open request同时传入冻结的open/read timeout
quantum；只有选定backend明确接受且可读回/探测这两个capability时才允许open，单次call因此有确定上界。
unsupported DShow/MSMF实现必须在open前返回Unsupported；Hardware Unverified阶段支持矩阵可以为空，但不能
绕过close证明。interrupt token只持有与VideoCapture handle分离的atomic stop/control；bounded call返回后由
read worker观察并close。file backend也必须使用可证明的bounded call。

worker close后不在worker thread destroy capture handle。它把唯一已closed handle放入one-shot exit channel，
然后退出；external owner join成功，释放自己的interrupt token，再destroy handle。其他interrupt clone只持有
独立control且没有handle pointer，所以其迟到Drop无法访问已destroy handle。

backends：

- `SyntheticCapture` test/support：scripted frames/fault/end，blocking read由barrier/channel控制；
- `FileCapture` component/integration：tracked synthetic image/video输入，no hardware；
- `OpenCvCapture` production：Windows discovery/open/read/profile/interrupt/close through bridge。

production discovery返回private稳定descriptor候选字段：opaque source id、display name、backend；不从friendly name
猜stable identity。直到硬件qualification，source id只保证本次discovery/open round-trip，不形成跨重插稳定承诺。

## 16. CaptureSession 与 Runtime

`CaptureSession::new(runtime, backend, options)`：

1. validate options；创建resource cancellation token；
2. 构造 `Arc<CaptureInner>`，状态Opening；
3. `runtime.register_resource`，保存ResourceRegistration；
4. 创建session-owned startup Operation并设置first-frame deadline；deadline hook只锁state、把Opening原子转
   Faulted、请求interrupt并notify，不执行native I/O；
5. `runtime.spawn_supervised`启动唯一read worker，start gate确保worker handle已保存后才发布session；
6. worker open、循环bounded read、用Runtime Clock产生timestamp、checked increment sequence并发布latest；
7. first frame赢得deadline race时状态Streaming并完成startup Operation；deadline/fault赢时worker cleanup后
   完成对应terminal；
8. close遵守ADR顺序，通过exit channel取回closed handle，join、destroy、核对counts后清latest/registration。

`CaptureInner`有 `close_gate`、`admission_gate`、`Mutex<CaptureStateData>`、Condvar、worker start barrier、
one-shot exit channel、interrupt、registration和resource token。worker不能持有最后一个`Arc<CaptureInner>`；
它只持有独立shared state/clock/token和backend owner，避免在线程退出时触发self-join。

显式 `CaptureSession::close` 和 `ManagedResource::close` 调用同一幂等确定性 `close_internal`。CaptureInner Drop
只在admission gate内标记closing、请求resource cancellation/interrupt并notify；不等待、不join、不调用backend
close、不take registration，也不声称Closed。正式验收始终显式session close后Runtime close。

close返回错误分两类：若worker已join、handle已destroy且native counts归零，保存session diagnostic、注销resource，
显式close返回error但Runtime仍可真实Closed；若join/destroy/count无法确认，保留ResourceRegistration和Stopping/
fault diagnostic，使现有Runtime registry convergence失败，或由supervised task panic记录CloseFailed。不得把普通
backend error变成panic。若未来需要更丰富fallible ManagedResource返回，必须先按ADR-0007重开Phase 1。

snapshot可以有同步internal API和Runtime Operation wrapper。Operation必须是resource token child；wait timeout不
取消，operation deadline由Runtime取消。snapshot先按state判定，再读取latest，优先级固定为：

- Streaming且latest存在：Succeeded，result在Vision side typed storage中；
- Opening：按poll/wait/deadline规则等待首帧；
- deadline/caller/resource cancel：先退出waiter并释放owner，再Cancelled；
- Faulted：即使latest仍保存旧帧也Failed，保留Vision fault；故障前已经取得的Arc<Frame>继续可读，新snapshot
  不返回未标记陈旧帧；
- Stopping/Closed：Failed或ParentClose Cancelled，原因稳定且不伪称NoFrame；
- Opening下poll且无frame：NO_FRAME equivalent，不改变capture state。

Phase 3不修改 `OperationValue` 枚举来塞Frame；内部 `VisionOperation<T>`组合Runtime Operation和typed result slot。
这避免触碰Phase 1 frozen model。未来Phase 5由easycon-sdk/capi决定public result handle。

## 17. Error mapping

`VisionError`在easycon-vision定义，至少包含Validation、Limit、NoFrame、Closed、Faulted、Cancelled、Deadline、
InvalidImage、ModelNotFound、Native(kind)、PoolClosed、Internal。它保留native kind/message但不暴露handle。

需要提交Runtime Operation时，Phase 3只映射到现有稳定 `EasyConError`：validation/limit -> Validation/
InvalidArgument；capture disconnect/fault -> Io/DeviceDisconnected或Transport；deadline/cancel沿Runtime既有路径；
native exception/model/image等临时映射Internal/Transport并在typed Vision error slot保留精确原因。不得为Phase 3
修改Phase 1 `easycon-model` error enum。正式Vision public domain/code分配留Phase 5 ABI设计。

C++ exception不会panic；Rust native-sys invariant violation返回NativeError::Internal；Rust worker panic由Runtime
supervisor记录，Runtime close返回CloseFailed。每个可恢复错误只失败当前job/session，不污染其他engine/capture。

## 18. 测试资产和差分分类

`spec/fixtures/vision`只放项目自有synthetic资产：

- ASCII PPM/PGM或由明确脚本生成的BMP/PNG，注明generator、license `GPL-3.0-only`和SHA-256；
- template scene/target和三个mode expected location/raw/score tolerance；
- XY/Laplacian preprocess expected pixels/hash与match；
- HSV wrap/non-wrap/empty/no-match/full-match expected count/ratio/bbox；
- `.IL` corpus和expected diagnostic；
- OCR missing model config，以及独立Apache-2.0 `tessdata_fast` English test model及其source/license/hash manifest；
- capture frame sequence/profile/fault script。

classification：

- exact：active `.IL`字段、BMP/PNG、normalized template、XY/Laplacian、OCR default/score formula；
- corrected：strict errors、stable duplicate diagnostic、limits、immutable frame、explicit model root、resource close；
- new：HSV ROI statistics、native pool；
- excluded：`.ILX`、Canny、non-normalized/old pixel、UI、EasyCon/default package traineddata。

浮点fixture逐项声明tolerance；location/error/order/count必须exact。fixture validation脚本检查license/provenance/hash、
unique IDs和bounds，且不读取EasyCon。

## 19. 确定性并发测试

禁止用随机sleep制造竞态。测试primitive：

- `BlockingCaptureBackend` read-entered/read-release/interrupt-observed channels；
- first-frame waiter barrier；
- latest replacement drop observer；
- pool FIFO ticket gate和engine borrowed barrier；
- operation cancel/deadline使用VirtualClock；
- native exception test entry；
- live handle/allocation baselines；
- panic payload/worker barrier沿用Runtime supervision。

必须覆盖：open success/failure/cancel/deadline；no first frame；read fault/hot-unplug equivalent；snapshot poll/wait；
close during blocked read；close racing frame publish；fault racing close；repeated/concurrent close；Frame borrow during latest
replace；queued/in-flight cancel；OCR poison discard；pool close with waiters；Runtime close registry convergence。

## 20. Native component tests与质量门禁

component tests直接通过internal header验证：

- status/error/buffer zeroing和release；
- cv/std/unknown exception；
- BMP/PNG invalid/truncated/oversized/decode/encode/conversion/ROI；
- normalized template和edge fixture；
- HSV；
- Tesseract missing model、bad image、create/process/reuse/release；
- file/synthetic input和OpenCV capture invalid source/open/read/close；
- handle/allocation count回零。

native commands固定记录为：

```powershell
cmake --preset msvc-debug
cmake --build --preset msvc-debug --parallel
ctest --preset msvc-debug --no-tests=error
cmake --preset msvc-release
cmake --build --preset msvc-release --parallel
ctest --preset msvc-release --no-tests=error
cmake --preset clang-asan
cmake --build --preset clang-asan --parallel
ctest --preset clang-asan --no-tests=error
cmake --preset clang-ubsan
cmake --build --preset clang-ubsan --parallel
ctest --preset clang-ubsan --no-tests=error
cmake --preset clang-tidy
cmake --build --preset clang-tidy --target easycon_native_clang_tidy --parallel
cmake --preset msvc-analyze
cmake --build --preset msvc-analyze --target easycon_native_bridge easycon_native_bridge_tests --parallel
cmake --preset clang-fuzz
cmake --build --preset clang-fuzz --target easycon_native_fuzzers --parallel
ctest --preset clang-fuzz --no-tests=error -L fuzz-seed-replay
```

`clang-tidy` preset生成 `compile_commands.json`，`easycon_native_clang_tidy` 对全部自有bridge和component test
translation units运行仓库固定check集并把warning视为error。`msvc-analyze` preset在自有target上固定
`/analyze /WX`。`clang-fuzz`只构建仓库列举的parser/header fuzz targets；`easycon_native_fuzzers`聚合这些target，
`fuzz-seed-replay` label逐个重放tracked corpus，并由CTest timeout提供总上界。不得扫描或生成未跟踪corpus。

Windows ASan不作为leak唯一证据，必须同时核对native counters。Debug/Release需要fresh configure，不能复用
跨compiler cache。`clang-asan` 或 `clang-ubsan` 任一 configure/build/CTest失败、timeout或未执行都阻断Phase 3
冻结；只有先修改ADR-0011说明精确替代门禁并在新SHA完成独立复审后才能改变该要求。

## 21. Rust/spec完整门禁

每个Rust/Cargo/spec/fixture节点按顺序执行：

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

native变化还要执行上一节适用的CMake/CTest/sanitizer/analysis/fuzz门禁。每次提交前复核status、owned diff和
staged diff，只stage Phase 3文件；commit message使用 `英文类型:中文说明`。

## 22. 节点验收

### A. Build与bridge skeleton

- private header/layout/status/error/buffer/handle counters；
- three exception tests；
- safe native-sys smoke；
- MSVC Debug/Release和strict warnings；
- 无installed header/public symbol。

### B. Frame/Image/codec

- checked constructors/ROI/lifetime；
- actual BMP/PNG OpenCV decode/PNG encode/conversion；
- invalid/truncated/oversized；
- native allocation归零。

### C. Template/edge/color

- 三mode位置/score；
- XY/Laplacian preprocess和match；
- HSV wrap/count/ratio/bbox；
- fixture tolerance/hash。

### D. OCR/pool

- explicit path/config；
- missing model/bad image/exception/poison；
- bounded FIFO acquire/cancel/close；
- engine reuse/release count；
- O-03保持开放。

### E. `.IL`/Label

- complete corpus、duplicate key/name、limits、UTF-8/Base64；
- immutable registry和single Frame evaluate；
- BMP/PNG target；
- fuzz seeds；
- `.ILX`无入口。

### F. Capture/latest

- synthetic/file/native implementations；
- Opening/Streaming/Faulted/Stopping/Closed；
- first-frame wait/deadline/fault；
- blocked read interrupt/join；
- repeated/race close；
- Runtime counters归零；
- hardware support matrix仍空。

### G. Integration/review/freeze

- Runtime resource/task/operation不变语义接入；
- all component/Rust/fixture/fault gates；
- fixed SHA independent review和必要修复复审；
- freeze ADR记录精确SHA、依赖、命令结果、O-03/capture hardware边界和reopen rule。

## 23. Review checklist

独立reviewer必须逐项回答：

1. EasyCon源码引用是否与实际字段、score、edge、OCR、loader和调用链一致；
2. 是否有Runtime反向依赖、C++业务状态、额外长期线程或public ABI泄漏；
3. pointer/buffer/error/allocator/exception ABI是否每条路径有owner；
4. 每个unsafe和Send/Sync是否有具体证明；
5. capture close是否真实interrupt、join后destroy且latest borrow安全；
6. pool/OCR cancel/poison/close是否会提前terminal或泄漏；
7. limits、ROI、stride、Base64、JSON duplicate和UTF-8是否checked；
8. vcpkg版本/modules/CRT/static策略和许可证是否准确；
9. synthetic evidence是否被错误外推为hardware/OCR model claim；
10. Phase 4/5、`.ILX`、traineddata、shared Phase 2B files是否保持排除。

可复现且in-scope finding必须先建立回归再修；只描述未来public ABI、硬件或模型release加固的建议进入backlog，
不无上限循环。
