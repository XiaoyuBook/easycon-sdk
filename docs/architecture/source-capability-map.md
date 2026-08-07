# 源码能力映射

## 1. 审计边界与方法

**[源码事实]** 本次只读审计覆盖本地 `EasyCon/src/EasyCon.Device`、`EasyCon/src/EasyCon.Capture`、`EasyCon/src/EasyCon.Script`、`EasyCon/src/EasyCon.Core`，并追踪到 `EasyCon2.CLI`、两套 Avalonia 服务、WinForms 服务、测试和固件服务中的实际调用链。四个核心项目分别有 17、16、67、30 个文件，现有 `test/` 只有 9 个文件。

除另有说明外，证据路径相对 `EasyCon/src/`，格式为 `相对路径:行号（符号）`。行号只定位当前本地快照；设计以符号和行为为准，不将该快照变成外层仓库的依赖锁。

迁移结论使用四种动作：

- **保留**：用户可见语义或协议可作为 v1 兼容基线。
- **迁移**：在 Rust 中重新实现，不复制原有线程、资源或错误处理方式。
- **桥接**：由私有 C++ 层承载原生库调用，Rust 仍拥有状态和生命周期。
- **延后**：保留为源码事实或 dormant workspace maintenance 的证据，不迁移为 v1 产品、公共 ABI 或语言 SDK。
- **排除**：不进入 v1 产品、公共 ABI 或官方绑定。

## 2. 源码项目关系

```mermaid
flowchart LR
    Device["EasyCon.Device\n串口与控制器协议"]
    Capture["EasyCon.Capture\n采集与识别"]
    Script["EasyCon.Script\nECS 编译与解释"]
    Core["EasyCon.Core\n应用编排"]
    Apps["CLI / Avalonia / WinForms"]

    Core --> Device
    Core --> Capture
    Core --> Script
    Apps --> Core
    Apps --> Device
    Apps --> Capture
    Apps --> Script
```

**[源码事实]** `EasyCon.Core.csproj` 同时引用 Device、Capture、Script，并额外引入 Python 和 Lua 运行时；这些应用层依赖不能原样成为共享核心。`EasyCon.Capture.csproj` 引入 OpenCvSharp、FlashCap、ImageSharp 和 Tesseract；`EasyCon.Device.csproj` 只直接引入串口包；Script 项目无外部包引用。

## 3. Controller 映射

| 源码组件与证据 | 已验证行为 | v1 归属 | 动作与约束 |
| --- | --- | --- | --- |
| `EasyCon.Device/ECDevice.cs:5-7` (`ECDevice.GetPortNames`) | 发现能力只是列出系统串口名，没有设备身份探测或稳定 ID | `easycon-serial::discovery` | 迁移。返回结构化描述符；端口名保留，VID/PID 等信息只在系统可提供时填充 |
| `JoyStickDevice.cs:39-68` (`TryConnect`) | 先用 115200，失败后等待 100 ms 再用 9600；成功后启动报告循环 | `easycon-controller::session` + `easycon-serial` | 保留握手顺序，迁移为异步 operation；不得阻塞调用线程 |
| `NintendoSwitchCmd.cs:16-64` (`_TryConnect`) | 创建连接后等待 1 秒的 Connected/Error 信号 | Controller 连接状态机 | 保留超时含义；使用单调时钟和显式 deadline |
| `TTLSerialClient.cs:83-236` | 当前 `NintendoSwitch` 实际实例化旧 `TTLSerialClient`；单线程忙轮询收发，取消只发信号，不等待关闭 | `easycon-serial` | 只保留字节协议，不迁移循环、锁或销毁方式 |
| `TTLSerialClient.cs:8-81` 与 `SerialPortClient.cs` | 新连接实现含心跳代码，但未被当前路径采用；重连调用被注释；`TTLv2SerialClient.Connect` 在 Open 后仍会抛出“连接失败” | 无公共能力 | 排除“自动重连已存在”的假设。v1 默认不自动重连，断线发事件 |
| `SwitchReport.cs:3-52` | 状态为 `Button + HAT + LX/LY/RX/RY`，按 7 位数据块编码并在末块置高位结束标志 | `easycon-controller::protocol` | 保留为协议黄金向量，Rust 迁移 |
| `SwitchCommand.cs:12-42` | 14 个按钮、8 向 HAT、中心值；摇杆范围 0..255、中心 128 | `easycon-model` + Controller | 保留数值语义；公共 API 使用稳定枚举和显式坐标 |
| `ECKeyUtil.cs:19-57` | 按钮/HAT/双摇杆最终都修改一份共享 `SwitchReport` | Controller desired-state | 迁移为单写者状态机；禁止跨线程直接修改报告 |
| `JoyStickDevice.cs:81-148` | Down/Up、组合方向和 HAT 操作进入 `_keystrokes` 字典 | Controller command lane | 保留用户语义；实现改为有序命令，不使用对象锁或覆盖式字典 |
| `NintendoSwitchPriv.cs:5-86` | 最小发送间隔固定 30 ms；报告循环按时间处理动作 | `easycon-runtime::scheduler` | 保留 30 ms 默认下限，改用单调时钟和绝对目标时间 |
| `ECKey.cs:12-19` + `NintendoSwitchPriv.cs:68-73` | `KeyStroke` 构造器接收 `time`，但字段总是 `DateTime.Now`，因此未来释放时间未保存 | 不作为兼容行为 | 修复。差分测试不能把此缺陷固化；序列时序以文档契约为准 |
| `NintendoSwitchCmd.cs:83-107` | 同步命令通过临时接收处理器等待谓词匹配，默认 100 ms | Controller request/response lane | 迁移为每连接唯一的请求匹配器，禁止多个临时监听器争用相同字节流 |
| `NintendoSwitchCmd.cs:232-271` | Amiibo 数据按 20 字节分包，ACK 失败时握手后重试；可切换索引 | Controller Amiibo operations | 保留分包/ACK 协议；长度、槽位由已验证设备能力约束 |
| `GamePadAdapter.cs:13-68` | ECS 按键/摇杆动作映射到 `NintendoSwitch`；延时后再释放 | ECS future evidence | 延后。v1 以直接 Controller 调用和 `ActionSequence` 替代，不形成跨 ABI 回调 |
| `OperationRecords.cs` | 手工操作可转成 ECS 文本 | 无 | 排除 v1；不是 Controller 首批能力 |
| `NintendoSwitchCmd.cs:130-230` 中固件、远端脚本、LED、配对和改色命令 | 与首批 Controller 能力无关 | 无 | 固件写入和远端脚本明确排除；其余不进入 v1 稳定 API |

### Controller 兼容基线

**[已决定]** 协议编码、按钮/HAT/摇杆数值、115200→9600 握手顺序、30 ms 默认报告间隔和 Amiibo 20 字节分包是迁移基线。

**[推导]** 原源码的壁钟调度、忙轮询、临时事件竞争、取消不等待和计时字段缺陷都不是用户语义。新实现以单调时钟、单写者命令队列和确定性关闭替代。

### Phase 2A 实现状态

**[已决定]** Windows 10/11 x64 serial backend 现在是 `easycon-serial` 系统叶子；结构化发现只使用
Windows 提供的属性，稳定身份不从 COM 名称猜测。Controller 继续只依赖 `ControllerTransport`，不感知
Win32 或具体串口实现。

**[已决定]** Amiibo v1 save/select 已按上述源码事实实现 20 字节分包、`0xff` ACK、generation matcher
和有界 reset/retry。源码没有给出可信的槽位数和总长度，因此默认 capability 为空；显式 limit 只允许
无硬件测试和调用方保守接入，不能关闭 O-02 或形成设备支持声明。

**[待确认]** Phase 2A 状态为 `Hardware Unverified`。O-01 的控制板/固件/VID/PID/baud 支持矩阵、O-02 的
Amiibo 物理容量以及 O-04 的 UART/USB/Switch 时序必须在 Phase 2B 用实物关闭；软件 fake 和内存
transport 结果不得替代这些证据。

## 4. `EasyCon.Script` 源码事实与延后处置

`EasyCon.Script` 继续是本地只读源码事实：它包含 `.ecs` 的 source loading、lexer/parser/binder/evaluator、诊断、
控制器/标签 adapter 和内建函数。该事实不等于 v1 迁移承诺。

| 源码组件与证据 | 已验证行为 | 当前处置 |
| --- | --- | --- |
| `EasyCon.Script/Compilation.cs:22-60`、`Syntax/*`、`Binding/*` | 加载 source 与 `lib/*.ecs`，编译、诊断并解释执行 | 延后为 `easycon-ecs` 及其现有 spec/fixture/conformance 的未来合同；不进入 v1 ABI 或语言 SDK |
| `Binder.cs:44-119`、`Binder.cs:253-287` | lib/main 可见性和 `IMPORT` NOP | 延后为未来 ECS 语义证据，不迁移为 v1 行为 |
| `Diagnostic.cs:6-29`、`Text/*` | 文件、span、行列与 error/warning | 延后为未来诊断合同；v1 不导出 ECS diagnostics |
| `Evaluator.cs`、`GamePadAdapter.cs`、`CustomSleep.cs` | 解释器驱动控制器、标签、等待和取消清理 | 延后。v1 Controller 的中立化/取消和 `ActionSequence` 另由 Runtime/Controller 合同定义 |
| `BuiltInFuncs.cs`、`BuiltinCallable.cs` | WAIT、PRINT、ALERT、RAND、TIME、AMIIBO、BEEP、LEN、APPEND | 仅作源码事实；不把该语言、其事件或等待模型公开为 v1 |
| `Scripter.cs`、`EasyRunner.cs`、Avalonia `ScriptService.cs` | 应用层编译/执行封装和 stop/reset 路径 | 延后；`easycon-sdk` v1 只聚合 Runtime、Controller、Vision |
| `Runner/PyRunner.cs`、`Runner/LuaRunner.cs`、`Assembly/`、`ForeignFunction.cs` | 语言 runner、字节码、固件和自定义回调 | 排除 v1 |

现有 ECS maintenance 资产继续参加仓库健康门禁，但不作为 v1 public ABI、共同语言 conformance、硬件/soak 或发布证据。
未来恢复必须按 ADR-0023 重新决定产品范围、API/ABI、兼容性、资源/安全与测试成本。

## 5. Vision 映射

| 源码组件与证据 | 已验证行为 | v1 归属 | 动作与约束 |
| --- | --- | --- | --- |
| `EasyCon.Capture/Capture.cs:10-107` | Windows 用 DirectShow 名称枚举，其他系统尝试 FlashCap，失败后探测前 10 个索引；采集 API 枚举直接暴露 OpenCV 枚举 | `easycon-vision::discovery` + 私有桥接 | Windows 首发由 C++ 桥接枚举；公共 API 返回自有稳定 backend 枚举，不暴露 OpenCV 数值 |
| `CVCapture.cs:6-72` | `OpenCVCapture` 包装 `VideoCapture`，设置分辨率、MJPG/30 FPS，读取新 `Mat` 并可 Dispose | `easycon-vision::capture` + 私有桥接 | 桥接。Rust 拥有状态、采集线程和帧生命周期；C++ 只拥有原生 capture 对象 |
| Avalonia `CaptureService.cs:94-112` | 通过锁串行读取并返回克隆帧 | Vision 最新帧槽 | 迁移为单采集线程 + 不可变最新帧，避免每个调用方直接读设备 |
| `Search.cs:11-21` | 实际启用 SqDiffNormed、CCorrNormed、CCoeffNormed、EdgeDetectXY、EdgeDetectLaplacian、OCR | Vision template/OCR | 这五种图像匹配模式和 OCR 作为 v1 兼容集合 |
| `MatchFacts.cs:11-56` | 使用 OpenCV MatchTemplate；SqDiff 取 min，其余取 max；再换算匹配分 | `easycon-vision::match` + 私有桥接 | 桥接算法，Rust 统一把分数规范化到 0.0..1.0 |
| `CVSearch.cs:21-88` | XY Sobel 平均和 Laplacian 预处理；Canny 有实现但没有进入启用列表及 Search 分支 | 私有桥接 | 迁移前两种；Canny 不进入 v1 稳定枚举 |
| `OCRDetect.cs:5-30` | 默认 `chi_sim`、SingleLine；每次调用创建 TesseractEngine；数据路径固定为进程相对 `./Tessdata` | `easycon-vision::ocr` + 私有桥接 | 桥接；模型路径由 Runtime 配置，按语言缓存/池化引擎，不依赖当前工作目录 |
| `ImgLabel.cs:10-129` | `.IL` 为 JSON，含算法、Base64 目标、搜索区和目标区；name/path 来自文件系统 | `easycon-vision::label` | 在 Rust 用结构化解析器迁移；标签不可变，名称和来源独立于当前目录 |
| `ImgLabel.cs:159-202` | 标签搜索裁剪 ROI，图像模板匹配或 OCR，最终把分数乘 100 | Vision label evaluate | 公共 Vision 结果统一为 0.0..1.0；0..100 的 ECS adapter 仅保留为未来合同证据 |
| `ECCore.cs:17-50` | 从多个 `ImgLabel` 目录加载 `.IL`，按名称去重并跳过后出现者 | label registry | 改为稳定排序和显式 duplicate diagnostic；默认重复名是编译/加载错误 |
| `ImgLabelX.cs` | 对外只有名称可设置；核心字段私有；写入把 0xFF 当结束符，读取却先把下一字节当扩展标记 | 无 | `.ILX` 不具备可验证的稳定契约，v1 排除 |
| `MatExtensions.cs` | 使用 ImageSharp 在 BGR/BGRA/Gray 与 PNG 间转换，失败时吞异常并返回空对象 | Vision image codec | OpenCV 编解码由桥接承担；错误必须显式传播，不允许空结果掩盖失败 |
| `HSVColor.cs` | 只提供 RGB↔HSV 换算，没有接通的区域颜色检测 API | `easycon-vision::color` | 为满足已定产品能力新增 HSV 区域检测：匹配像素数、比例、可选包围框 |
| `Search.cs:106-362` | Strict/Random/Opacity/Similar/FindColor 属于未接通旧代码，且含 32 位指针截断写法 | 无 | 排除，不作为兼容或桥接依据 |

### Vision 兼容基线

**[已决定]** `.IL`、三个归一化模板算法、XY/Laplacian 边缘模板、默认简体中文单行 OCR 和公共 Vision 的 0.0..1.0 归一化分数是 v1 基线。

**[推导]** 公共 Vision API 不暴露 `Mat`、`Bitmap` 或 Tesseract 类型。帧、图像和标签都是核心拥有的不透明资源，跨 ABI 只给稳定元数据和显式复制/编码结果。

**[已决定]** 上表的 DirectShow/Windows 枚举只是 EasyCon 源码事实，不是跨平台公共语义。Vision 平台候选与证据保持
Frame/Image/Label/Capture 状态机平台中立，Windows、Linux V4L2 和未来 AVFoundation 都只能作为 leaf adapter。此前的
“Phase 3” 是 ADR-0023 前的历史 Vision 平台标签，不是当前第三阶段的语言 SDK。
Linux 在真实 build/native fixture 后仍是 Vision candidate，macOS 本轮只允许 Apple Silicon arm64
fail-closed experimental source；两者都不得从现有 Windows 源码推导硬件或完整 SDK 支持。

## 6. Core 与相关项目归属

| 源码区域 | 事实 | v1 处理 |
| --- | --- | --- |
| `EasyCon.Core/ECCore.cs` | 静态便捷入口聚合设备、采集、算法和标签 | 由实例化 `Runtime` 取代，禁止进程全局可变状态 |
| `EasyCon.Core/Scripter.cs` | 保存 runner 与外部 getter，未定义并发访问 | 延后为 ECS 未来证据；v1 不创建 Program 或 Run operation |
| `EasyCon.Core/ProjectManager.cs` | ZIP 工程编辑和文件写入职责 | 排除；不进入 v1 产品 |
| `EasyCon.Core/Config/*` | AppData 配置和网络推送 | 排除；SDK 只接受调用方传入的 options |
| `EasyCon.Core/Assist/` (`AssistClient`) | 连接固定远程地址并发送消息 | 远程助手排除，不进入依赖图 |
| `EasyCon2*` UI、WinInput | UI 状态、键盘映射、虚拟面板和对话框 | 全部排除；只能作为调用模式证据 |
| `EasyCon2.CLI` | 无 UI 组合运行能力，但资源释放并不完整 | 不迁移 CLI；用其证明共享核心无需 UI |
| `fw/` 与 FirmwareService | 固件生成、写入和板型列表 | 全部排除 |

## 7. 不得继承的源码行为

以下是已经从源码直接确认的实现问题，而非待猜测事项：

1. 动作未来时间未写入 `KeyStroke.Time`，不能作为时序兼容基线。
2. 取消按键点击可能绕过释放，必须由运行终态清理兜底。
3. 当前连接路径没有已工作的自动重连；v1 不宣称该能力。
4. 多个同步 ACK 等待器可同时监听同一接收流；新核心必须串行化 request/response。
5. capture、frame、event 和后台任务在多个应用服务中由 UI 习惯性清理，不能成为库生命周期。
6. 图像转换吞异常、标签加载打印后跳过、CLI 中的帧未处处释放；新 ABI 必须传播错误并明确所有权。
7. `.ILX` 和未接通的像素算法不是完成能力。
8. Python/Lua、固件、远程助手、配置/推送和 UI 都不因存在源码而自动进入 SDK。

这些结论直接驱动 [生命周期规范](runtime-lifecycle.md)、[C ABI 错误模型](c-abi-v1.md) 和 [差分测试分类](testing-strategy.md)。
