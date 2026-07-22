# 测试策略与共同验收

## 1. 目标

测试体系要证明三件事：

1. Rust 共享核心对协议、ECS 和 Vision 的行为正确且资源可控。
2. C ABI 在旧/新调用方、异常、并发和错误条件下仍兼容。
3. C++、.NET、Python、Node.js/TypeScript 只是同一语义的惯用表达，不产生语言分叉。

测试按“纯逻辑 → 模拟资源 → native → ABI → 语言 → 包 → 物理硬件”分层。低层失败阻止更高层发布，物理硬件结果不能替代可重复的 fake/virtual-clock 测试。

## 2. 测试资产

tracked 规范资产位于目标结构的 `spec/`：

```text
spec/
├── abi/                    # machine-readable manifest 与 symbol/layout golden
├── behavior/               # defaults、状态、错误、事件和单位
├── conformance/            # 四语言共同场景
├── fixtures/
│   ├── controller/         # report/协议字节和时序
│   ├── ecs/                # source bundle、诊断、输出 trace
│   └── vision/             # 无版权争议的图像、标签和期望结果
└── schemas/                # fixture/schema 版本
```

fixture 必须自包含、可审阅且注明来源。`EasyCon/` 只用于本地形成候选差分结果，不能成为 CI input。

## 3. 确定性测试后端

### VirtualClock

- 手动推进单调时间，不依赖 `sleep`。
- 记录 deadline registration、wake order 和实际 dispatch。
- 可同时触发 cancel/deadline/I/O completion，覆盖竞态。
- precise sequence 在 virtual clock 下要求逐纳秒匹配目标规则，无累计漂移。

### FakeControllerTransport

可编程行为：

- 握手在指定 baud 成功/超时/错误；
- 部分读写、迟到 ACK、错误 ACK、重复字节、噪声和断线；
- write backpressure 和关闭唤醒；
- 记录每个 report bytes、operation ID 和 virtual timestamp。

### FakeNativeBridge

使用合成 capture 和纯内存图像，不加载 OpenCV/Tesseract：

- 固定或脚本化 frame sequence；
- open/read/close timeout 和 exception；
- 确定性的 match/OCR/color result；
- owned handle 计数与故意失败点。

### RecordingPorts

ECS 的 ControllerPort、VisionPort、OutputPort 记录按顺序的 typed call，可注入 cancel 和 runtime error。这样 parser/evaluator 测试不需要硬件或 native 库。

所有 fake 都只能存在于 test/support crate，不进入 release features。

## 4. Rust 单元与模型测试

### `easycon-model`

- stable enum/error/event code 唯一性与 golden 数值；
- UTF-8、ID、坐标、ROI、checked arithmetic 和 limit；
- serialization 只用于 fixture，不把 Rust layout 当 ABI。

### `easycon-runtime`

- Operation 每条合法/非法状态转换；终态单次提交；cancel 与 success 竞态；
- wait timeout 与 operation deadline 分离；
- parent cancellation tree；task supervisor 无脱管任务；
- event filter、sequence、reserved capacity、gap 合并和 drain；
- Runtime explicit close 的逐步顺序、保存的 Closed/CloseFailed outcome、幂等和并发 waiter；
- final owning Drop 只拒绝 admission/请求根取消，不执行 callback、join、finalizer 或最终事件；
- supervised spawn 自动 owner binding、共享完成/JoinHandle ownership、真实 thread join/unregister，以及
  task 内 close 的 pre-state rejection；
- operation terminal transaction 在 cleanup/event/registry 故障下仍注销并通知全部 waiter；
- cancellation hook 调用、未触发 hook capture 析构和 panic payload 析构逐项隔离；capture 析构重入
  operation 查询不持有 operation/hooks/children 锁，poisoned synchronization state 可恢复；
- 用 Loom 模型覆盖 parent terminal/child admission、hook panic/child propagation、supervised task
  self-wait、hook capture 析构重入和 terminal commit/registry unlink/waiter notify。模型命令是
  Phase 1 正式门禁。

Phase 1 的正式模型入口是 `python tools/run_runtime_models.py`。它以 test-only `runtime-model`
feature 执行独立的 `loom_runtime` target 并强制单 harness thread。模型直接调用 production 使用的
child admission、cancellation tree、panic isolation、task owner rejection、task lifecycle 和
unlink-before-notify 并发内核，而不是复制测试私有状态机。task 模型只能通过与 production 相同的
owner/handle 安装、body outcome、真实 thread join、panic diagnostic 持久化和 registry unlink 转换推进；
Loom 枚举内核同步交错，不使用随机 sleep。`runtime-model` 默认关闭，不进入普通 release API。任何 Runtime
Rust、Cargo、behavior 或 conformance 提交都必须在普通 workspace tests 之后单独运行该入口。

### `easycon-controller`

- `SwitchReport` 全零、单按钮、多按钮、八向 HAT 和摇杆边界的 7 位编码 golden；
- 115200→9600 握手顺序、每次 timeout 和取消；
- ACK matcher generation，迟到 ACK 不完成下一 operation；
- 20 字节 Amiibo 分包、重试、断线和 cancel；
- direct state、reset、组合方向、冲突输入和中立报告；
- precise sequence 同 offset 合并、最小 30 ms、无早发、取消中立化；
- Automation lease 和 direct command 的 busy 规则。

### `easycon-ecs`

- lexer/parser/binder/lowerer/evaluator 分层测试；
- CRLF/LF、UTF-8 中文、空文件、坏 token、span/行列；
- lib 独立作用域、函数可见性、全局语句、`IMPORT` NOP 兼容；
- 类型、数组/切片、函数/返回、循环和 BREAK/CONTINUE；
- 按键、摇杆、WAIT、AMIIBO、label getter 和全部内建函数；
- deterministic RAND seed、monotonic TIME、输出顺序；
- 每个可阻塞点 cancel，终态 cleanup；
- parser/evaluator fuzz 和有界资源测试。

### `easycon-vision`

- `.IL` legacy JSON 的合法/非法/未知字段/重复名/超限 corpus；
- ROI 边界、target 大于 range、分数 0.0..1.0 和 ECS 0..100 转换；
- Frame immutable/reference 生命周期；latest slot 并发替换；
- snapshot 首帧等待、deadline、capture fault；
- HSV hue wrap、空 ROI、阈值、ratio/count/bounding box；
- native pool 限流、公平性、取消前后 handle 计数；oversized、pre-cancelled、closed、full-queue decode在稳定错误
  返回前不构造输入等长owned copy、不调用native，且reservation的close/panic/可重入Drop路径保持计数收敛。

### `easycon-sdk`

- 跨域依赖检查和资源 admission；
- 每 Runtime 单 run；Program dependency validation；
- run success/failure/cancel 三条路径都先中立化再终态；
- Runtime close 按 Runtime-local `ResourceId` 顺序执行 resource callback，再 join task、收尾 operation、
  检查 registry 并发布最终事件；跨域中立化和 lease 释放属于各 owner 的 cleanup，不依赖硬编码类型顺序。

单元覆盖门槛：共享 Rust 业务 crates 行覆盖 ≥85%、分支覆盖 ≥80%；关键状态转换和 unsafe wrapper 要求 100% 语义分支覆盖。覆盖率只是下限，不替代竞态/故障测试。

## 5. C++ bridge 测试

### component tests

- capture device enumeration 的错误归一化；
- synthetic video/file backend 的 open/read/close；
- BGR/BGRA/Gray 解码、stride、ROI 和编码 round-trip；
- 三个归一化模板算法、XY/Laplacian 预处理的 fixture 结果；
- Tesseract model missing、engine create/process/reuse/reject；
- HSV mask/result；
- 每个 entry 捕获 `cv::Exception`、`std::exception` 和未知异常；
- handle create/destroy 计数，无异常穿越。

### native quality

- clang-cl ASan/UBSan 构建运行 bridge 和 C ABI corpus；
- 静态分析检查 noexcept、所有权、整数截断和 Windows handle；
- libFuzzer/AFL-compatible harness fuzz image decode、`.IL` target bytes 和 internal bridge parameter validation；
- OpenCV/Tesseract 升级必须重跑视觉 golden 并人工审查允许的浮点差异。

浮点结果比较由每个 fixture 声明容差；位置、错误和 event order 不使用模糊比较。

### Phase 3 平台证据分级

- Windows 10/11 x64 是 Tier 1：MSVC Debug/Release、clang-cl ASan/UBSan、clang-tidy、MSVC analyze、fuzz、
  Rust/spec 全门禁必须在最终源码执行。
- Linux x64 是 Vision build candidate：只有真实 Linux configure/build/CTest、Cargo 和 fixture log 可以标记
  software Passed；没有可用环境时保持 Build Unverified，并交付 exact bundle/dependencies/commands/SHA-256。
- macOS Apple Silicon arm64 是 Experimental Source Candidate：本轮只验证 fail-closed 结构和平台无关契约；
  没有 Xcode/Apple SDK log 时 build/hardware 均 Unverified，且 Not Shipped。云端 Mac 不能替代摄像头/USB 硬件。

common fixture 至少覆盖 codec、template、edge、HSV、OCR missing-model/合法独立资产、prevalidated finite File、
synthetic Capture、exception/ownership/resource count。平台 native adapter 未资格化时必须返回 Unsupported/
BackendUnavailable，不能以空 discovery/profile/frame success 通过测试。

平台结果的完整门槛和 macOS 两阶段 handoff 见
[Phase 3 跨平台边界设计](../development/phase3-cross-platform-design.md) 与
[Linux/macOS 验证 handoff](../development/phase3-cross-platform-validation-handoff.md)；当前证据状态由
[ADR-0013](../decisions/0013-phase-3-vision-cross-platform-source-candidate-freeze.md) 冻结。

## 6. C ABI 测试

### 编译与布局

- 用 MSVC C11、MSVC C++20、clang-cl C/C++ 编译 public header smoke client。
- golden 检查 exported symbol allowlist、calling convention、`sizeof/alignof/offsetof`。
- 检查 NULL、零长度、最小 size、较大 size、reserved 非零、未知 version。
- x64 Debug/Release 和动态分析 build 都执行。

### 兼容矩阵

| caller header | library | 期望 |
| --- | --- | --- |
| v1.0 | v1.0 | 全部通过 |
| v1.0 | 当前 v1.x | 旧 symbol/struct/默认行为不变 |
| 当前 v1.x | 支持的最老 v1 library | 通过 ABI info/capability 降级，不调用缺失 symbol |
| v1 | 错误 ABI major | 初始化明确失败 |

每个 release 保存 public header、manifest 和最小 smoke binary，用真实旧 artifact 验证，不只模拟 size。

### ownership/error/isolation

- 每种 handle create/clone/release/parent-close 路径；NULL release；类型/Runtime 混用；
- Buffer/Event/Error/Result 的 accessor 生命周期；
- Operation wait/cancel/result 竞态；多个 waiter；
- subscription 单 reader 约束、timeout、gap、closed；
- 强制 Rust panic 和 C++ exception，验证 status/error/event 与无跨边界 unwind；普通可恢复失败资源回基线，
  close callback 无法确认释放时提交保存的 `CloseFailed` outcome，并保留非零诊断注册直到 owner 实际
  释放；close waiter 不得在 `Closing` 永久等待；
- 调用方 buffer 不被越界写，异步 input 在提交时深拷贝；
- Windows Application Verifier、page heap 或等价 heap 工具检查跨 allocator/双释放。

use-after-release 和同一 raw handle 与 release 并发属于 C 调用方未定义行为，不尝试“通过测试支持”；官方 binding 必须从类型设计上阻止。

## 7. 源码差分策略

差分不是盲目逐字节复制。每个 case 标记三类之一：

### Exact compatibility

- report bytes、按钮/HAT/摇杆数值；
- ECS token/parse/bind/evaluate 的已支持语义；
- lib scope 和 `IMPORT` NOP；
- `.IL` 字段、ROI 和启用模板算法；
- ECS label 整数百分分数。

这些 case 的 normalized result 必须与 EasyCon 当前实现一致，除非先批准兼容变更 ADR。

### Corrected behavior

- `KeyStroke` 未来时间丢失；
- cancel 跳过按键 release；
- 自动重连的文档/实际路径不一致；
- ACK listener 竞争；
- 图像错误被吞成空结果；
- `.ILX` 读写不自洽和未接通旧像素算法。
- ALERT/BEEP 的应用副作用在 SDK 中改为 typed event；网络推送和 UI/系统提示不属于 v1。

这些 case 保存“源码观察结果 + v1 规范结果 + 修正理由”，新核心只断言规范结果。

### Excluded behavior

Python/Lua Runner、字节码/固件、远端控制、配置/推送、UI/键鼠、工程编辑不建立差分 harness。

本地可选参考流程可在忽略的 `.tools/reference-easycon/` 使用 EasyCon 源码生成候选 trace 到 `artifacts/`；人工核对后只把最小 fixture/expected trace 纳入 `spec/`。CI 和 release 不读取 `.tools/` 或 `EasyCon/`。

## 8. 四语言 conformance

### 共同场景

每种 SDK 必须对同一个 fake-enabled core 运行：

1. Runtime create/capability/event subscription/close。
2. 发现两个 fake device，连接 fallback、断线、重连由调用方发起。
3. direct button/HAT/stick/reset 和 precise sequence trace。
4. Amiibo save/select 的 success、ACK timeout、cancel。
5. ECS valid/invalid compile，完整 diagnostics；run success/failure/stop、logs/events。
6. synthetic capture、snapshot、Frame metadata/encode。
7. `.IL` load、template、OCR、color 的共同 result。
8. wait timeout 不取消；语言 cancel 映射到 native Cancelled。
9. queue overflow gap 和状态恢复查询。
10. 显式 dispose/context exit 后资源/task/handle 计数为零。

### trace 比较

每个 runner 输出规范化 trace：

- 删除墙钟和语言 stack；保留 event sequence 相对顺序、operation/resource 关系；
- error 只比较 stable domain/code/native code presence，message 做 UTF-8/非空检查；
- 浮点按 fixture tolerance；
- 集合顺序有规范时逐项比较，无规范时显式排序；
- unknown event/status case 也必须保留 raw 数值。

四份 trace 必须与 canonical expected 完全相符，且彼此相同。不得为某语言维护独立 expected 文件。

### 语言专项

- C++：move/destructor、stop_token、exception/no-throw close。
- .NET：SafeHandle finalization、Task continuation 线程、CancellationToken race、trim/AOT compatibility 评估。
- Python：sync/async context、asyncio task cancel、interpreter shutdown warning、wheel 多 Python 版本。
- Node：Promise/AbortSignal race、addon cleanup hook、ESM/CJS、Symbol async dispose、event loop 不被阻塞。

## 9. 故障注入矩阵

| 层 | 故障 | 必须观察到 |
| --- | --- | --- |
| Runtime | executor admission failure、queue full、close/Drop race、resource/task panic | 保存的 CloseFailed 或 stable error/gap，无脱管 task、无伪造 operation 终态 |
| Serial | access denied、partial write、read timeout、hot unplug | operation Failed/Cancelled，Controller 明确状态 |
| Protocol | wrong hello、busy、late/duplicate ACK | 不串请求，重试有界，正确 error |
| Scheduler | cancel 与 deadline 同 tick、clock jump fake | 单一终态、单调规则不破坏 |
| Automation | parser limit、infinite loop cancel、Vision error | diagnostics/运行错误，最终中立化 |
| Capture | no first frame、corrupt frame、read stuck、close | NO_FRAME/fault，可中断并 join |
| Vision | invalid ROI、huge image、missing OCR model、native exception | 参数/模型/native 错误，无空成功 |
| Events | 消费者停读、容量 1、terminal burst | gap + 可查询终态，生产者不阻塞 |
| ABI | bad size/version/type/runtime、panic | 明确错误，无越界/unwind |
| Binding | event pump 退出、GC/finalizer、process shutdown | Task 不永久悬挂，遗漏显式 close 可诊断且 release 不伪造关闭 |

故障点使用命名 failpoint，release binary 默认移除或关闭；测试不能依赖随机睡眠制造竞态。

## 10. 物理硬件体系

### Phase 2A 无硬件延迟证据

Phase 2A 先按 [ADR-0008](../decisions/0008-phase-2-controller-target.md) 用非阻塞内存 transport 测量
`command admitted -> transport write entered`，至少记录 10,000 次 lane 已空闲且 pacing 已满足的 eligible
direct report 的 p50/p95/p99/max 和固定测试环境。该 harness 验证 PC 核心没有人为前置等待及明显调度
回归，不代表 UART、CH32、USB HID 或 Switch 端到端延迟；普通 CI 继续使用 VirtualClock 证明确定性规则，
不以易抖动的墙钟阈值代替 correctness。

当前 Candidate 的确定性资产为：

- `tests/support/tests/phase2a_sequence.rs`：10,000 个输入 step、5,000 个同 offset 合并 report，逐目标
  推进 VirtualClock 并核对无丢失/乱序/早发/漂移、唯一终态、lease、中立化和 registry 收敛；
- `tests/support/tests/phase2a_latency.rs`：不设墙钟阈值，只验证 admission、lane wake、dispatch、transport
  entry、transport acceptance 五段时间戳单调且 recorder 不过滤样本；
- `tests/support/tools/controller_latency.rs`：显式 release harness。它先执行指定 warmup，再要求至少
  10,000 个 measured sample 全部满足 pacing eligibility；任一非单调、非 eligible 或缺失样本直接失败。

固定环境测量命令：

```powershell
cargo run --release -p easycon-test-support --bin controller_latency -- --samples 10000 --warmup 1000 --minimum-report-interval-ns 1 --machine DESKTOP-IQM6HN5 --windows-build 10.0.26200.8875 --power-plan "GamePP 电源方案" --output target\phase2a-controller-latency-2026-07-20.csv
```

`1 ns` 配置使顺序提交且上一 operation 已终态的每个 direct report 都满足 pacing，而不是把 30 ms 等待计入
`admitted -> write entered`；harness 仍逐样本验证 eligibility，不删除 outlier，也不使用永久 busy wait。
环境、六段完整分位数、目标判断和 raw CSV 哈希见
[Phase 2A latency fixture](../../spec/fixtures/controller/phase2a-latency-result-v1.json)。

### Phase 2B qualification telemetry 软件证据

Phase 2B 的不可发布 hardware CLI 按
[telemetry 与资格投影设计](../development/phase2b-telemetry-qualification-projection.md) 聚合每个 logical report。
`write_entered_ns` 固定为第一次 partial transport entry，`transport_accepted_ns` 只在最后一段完整接受后存在；
failed row、partial count、operation/write sequence、mapped transport error 和 lossy mapping 前的 native serial
kind/OS code 全部保留。任何 context、prefix 或时间逆序都成为显式 qualification failure，不能以饱和算术隐藏。

sequence 的成功、失败、取消和 cleanup failure 都从同一内存 projection 生成 `sequence-timings.csv`。该 CSV 和
最终 JSON 的字段、六种合法 execution/qualification/exit 三元组由
`tests/hardware/fixtures/phase2b-qualification-projection-v1.json` 固定，并由独立 hardware workspace 的 synthetic
conformance test 读取。fixture 不改变 Phase 2A memory-transport measurement，也不包含物理硬件结论。

OS write acceptance 不是 UART complete frame。UART 值始终标记为 theoretical 且 `measured = false`；没有 analyzer
或可审计 firmware trace 时，USB HID 和 Switch physical order 明确为 `unverified`。这些软件证据不能关闭 O-04。

### 矩阵登记

O-01 关闭时建立 `hardware/matrix.yaml`（只记录 SDK 自有测试配置，不记录 EasyCon 源码锁）：

- 控制板型号、固件版本、VID/PID/串口类型、支持 baud；
- Amiibo slot/长度能力；
- Windows 版本、USB chipset；
- capture card 型号、backend、分辨率/FPS/像素格式；
- OCR 模型版本和测试画面来源。

### Controller 硬件测试

- 100 次发现/连接/断开，无 port/thread/handle 增长；
- 115200/9600 正反例、占用端口、拔插和电源循环；
- 所有按钮、八向 HAT、摇杆边界和组合；
- Amiibo slot/分包/错误恢复；
- 逻辑分析仪或 firmware trace 验证 10,000-step sequence 顺序零错误；
- 暂定调度 SLO：间隔 ≥30 ms 时不早发，软件 dispatch lateness p99 ≤5 ms、max ≤15 ms；cancel 后已连接设备在 100 ms 内收到中立报告；
- 115200 低延迟档暂定 SLO：空闲 direct action 到 CH32 收齐报告 p95 <5 ms、p99 <10 ms，对应 USB HID report 出现在 Switch 侧总线的中位数 <10 ms，并记录 p95/p99/max；
- 9600 只作为兼容档；低于 30 ms 的连续节拍必须在对应控制板和固件上通过 10,000-report 无丢包/乱序验证。

SLO 在专用、固定电源策略的测试机测量；O-04 依据首轮数据冻结最终测量方法和支持阈值。未达暂定目标时
必须保留原始结果并通过 ADR 调整目标或缩小支持范围，不能在失败后静默放宽。

### Vision 硬件测试

- 每个受支持 capture profile 连续 30 分钟采集，frame sequence 单调、无 native handle 增长；
- 1920x1080/30 FPS 仅在设备协商成功时宣称；实际格式写入 open result；
- 拔卡后在协议期限内进入 Faulted，close 在 2 秒内完成；
- 截图与已知帧色彩/尺寸一致；模板/OCR/颜色 fixture 通过；
- capture + OCR + Automation 并发压力下 Controller 时序仍满足 SLO。

### 长稳

- 24 小时 ECS + capture + periodic Vision run；
- 10,000 次 Runtime/Controller/Capture create-close soak；
- queue overflow、日志风暴和反复 cancel；
- working set、handle、thread、native allocation 回归阈值由首个 Beta 建立，后续不得无解释上升。

## 11. 包与安装测试

每个候选包在全新 Windows VM 中：

- 不安装 EasyCon 源码、不设置开发机 PATH；
- 安装对应 package manager artifact；
- 验证 native manifest/build ID 与 canonical bundle；
- 运行无硬件 fake smoke 和有硬件 discover/open smoke；
- 卸载后不遗留全局 DLL、环境变量、服务或配置目录；
- 检查 NuGet/wheel/npm/ZIP 中 license、notices、SBOM/source URL。

## 12. 发布门槛

### Core API Candidate

- 全部 Rust unit/property/fuzz seed 通过；
- fake controller/native end-to-end 通过；
- corrected/exact 差分分类完成；
- task/handle/native allocation 计数归零；
- C ABI panic/exception/ownership tests 通过。

### Binding Candidate

- ABI header/symbol/layout golden 冻结；
- 四语言共同场景 trace 零差异；
- 各语言专项 cancel/dispose 测试通过；
- public API 文档没有绕过核心的语言特例。

### Release Candidate

- 旧/新 ABI compatibility matrix 通过；
- sanitizer/static/license/CVE/SBOM 通过；
- 全部 package clean-VM smoke 通过且 native hash 相同；
- O-01 至 O-04 在其规定门槛关闭；
- 物理硬件、时序、capture 和 24h soak 通过；
- GPL source/notice/build artifact 齐全。

## 13. 阶段适用门禁

仓库已经包含 Runtime、Controller 和 test support 功能项目，不再处于只验证架构文档的阶段。验证范围
以当前仓库协作规则为准：Rust 源码、Cargo、behavior、schema、fixture 或 conformance
变更必须执行完整 workspace、Loom 模型、规范、链接、repository guard 和 diff 门禁；当前完整命令清单见
[Runtime + Controller fake vertical slice](../development/runtime-controller-vertical-slice.md#本地验证)。

仅修改说明性文档且确认不影响可执行行为时，至少执行：

- Markdown 相对链接检查；
- repository guard；
- `git diff --check`。

无论变更类型，都必须确认 `EasyCon/` 仍被根 `.gitignore` 忽略、第三方参考源码没有改动，且外层 tracked
文件没有引入 `EasyCon/` 内容或项目依赖。
