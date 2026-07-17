# 四语言绑定设计

## 1. 共同语义

四种 SDK 名称可以符合语言习惯，但必须共享下列行为：

- 一个 `Runtime` 是资源根，支持显式 close/dispose。
- discover/connect/sequence/compile/run/snapshot/match/OCR/color 返回语言异步对象。
- wait timeout 不取消底层 operation；取消令牌只请求 cancel，异步对象在核心完成清理后才进入取消终态。
- Error 由 stable domain/code 映射，保留 message、native code、operation/resource ID 和 cause。
- Event 包含同一 sequence/kind/payload；语言层不得制造或吞掉核心状态事件。
- Program、Frame、Image、Label 是不可变对象。
- 同一 Runtime 同时最多一个 Automation run；run 期间直接 Controller 写入得到 busy error。
- TypeScript 只支持 Node.js，不提供浏览器构建或降级实现。

共同概念映射：

| 核心概念 | C++ | .NET | Python | Node/TypeScript |
| --- | --- | --- | --- | --- |
| Runtime close | RAII + `close` | `IAsyncDisposable` | context manager / async context manager | `dispose()` / `Symbol.asyncDispose` |
| Operation | `Operation<T>` | `Task<T>` + internal SafeHandle | awaitable `Operation[T]` | `Promise<T>` + internal native op |
| Cancel | `std::stop_token` / `cancel` | `CancellationToken` | `cancel()` / task cancellation | `AbortSignal` |
| Error | typed `std::system_error`-style exception | typed `EasyConException` | typed exceptions | typed `EasyConError` |
| Events | blocking iterator / subscription | `IAsyncEnumerable<Event>` | async iterator | `AsyncIterable<Event>` |

## 2. 生成与手写边界

一份 tracked ABI manifest 描述 symbol、handle、enum、struct、ownership、blocking 和 result kind。由它生成：

- C declarations 和 symbol allowlist；
- C++ low-level calls；
- .NET `LibraryImport` declarations；
- Python `ctypes` declarations；
- Node addon C ABI declaration table；
- enum/error/event conformance vectors。

生成层不包含用户 API。每种语言在生成层之上手写小型惯用 wrapper、文档和类型转换。业务校验只存在于 Rust；binding 可以做快速参数检查，但核心仍重复验证。

## 3. C++ SDK

### 工具与形态

- C++20、MSVC v143；namespace `easycon`。
- `EasyCon::SDK` CMake target 同时提供 public C header、RAII wrapper 和 import library。
- wrapper 只含轻量头/源，不链接私有 C++ native bridge。
- handle wrapper move-only；共享 immutable 值显式 `clone()`。

示意 API：

```cpp
easycon::Runtime runtime(options);
auto devices = runtime.controller().discover().get();
auto controller = runtime.controller().create();
controller.connect(devices.front().id).get();

easycon::ActionSequence sequence;
sequence.press(0ms, easycon::Button::A)
        .release(80ms, easycon::Button::A);
controller.execute(sequence).get();

auto program = runtime.automation().compile(bundle).get();
auto run = runtime.automation().run(program);
run.wait();
```

### 生命周期

- `Runtime`、`Controller`、`Capture` 析构时执行 no-throw close + release；显式 `close(timeout)` 可报告错误。
- 析构不得抛。失败记录到 Runtime cleanup event；如果对象仍有后台任务，析构等待确定性关闭。
- child wrapper 共享一个 internal Runtime control block，避免 Runtime wrapper 提前析构导致悬空。
- `Operation<T>::get()` 抛 typed exception；`wait_for()` 只返回 ready/not-ready；`cancel()` 幂等。
- 接受 `std::stop_token` 的 overload 注册取消请求，token 销毁不释放 native operation。

### 错误与事件

- `easycon::ErrorCode` 对 stable code 一一映射，category/domain 可与 `std::error_code` 集成。
- 默认高层 API 抛 `easycon::Exception` 子类；另提供 `Result<T>`/non-throwing overload 只在不会形成第二套语义时实现。
- `EventSubscription` 是 move-only input range；阻塞 `next(timeout)` 明确标注。C++ v1 不创建隐藏 callback thread。

## 4. .NET SDK

### .NET 目标与底层

- 包目标 `net8.0`；使用 source-generated `LibraryImport`，禁用 runtime marshalling 的隐式字符串转换。
- 每种 native handle 对应 sealed `SafeHandle`；release 函数 no-throw。
- public root 类型 `EasyConRuntime : IAsyncDisposable, IDisposable`。`DisposeAsync` 是正常路径；`Dispose` 阻塞等待关闭，适合非异步场景。
- public value 使用 records/readonly structs 和 .NET enums，不暴露 `IntPtr`。

示意 API：

```csharp
await using var runtime = await EasyConRuntime.CreateAsync(options, cancellationToken);
var devices = await runtime.Controllers.DiscoverAsync(cancellationToken);
await using var controller = runtime.Controllers.Create();
await controller.ConnectAsync(devices[0], cancellationToken);

var program = await runtime.Automation.CompileAsync(bundle, cancellationToken);
await using var run = await runtime.Automation.StartAsync(program, runOptions, cancellationToken);
await foreach (var evt in runtime.Events(cancellationToken))
{
    // typed event records
}
```

### Task 与取消

- binding 为每个 operation 保留 SafeHandle，直到 terminal state 和 result/error 已读取。
- 每 Runtime 一个 managed event pump 在后台阻塞读取 subscription，按 operation ID 完成 `TaskCompletionSource`；注册后立即查询 operation，消除完成竞态。
- `TaskCompletionSource` 使用 `RunContinuationsAsynchronously`，不在 event pump 上执行用户 continuation。
- `CancellationToken.Register` 只调用 native cancel；只有 native 状态为 Cancelled 才把 Task 置 canceled。
- token cancellation 与 operation success 同时发生时，以核心唯一终态为准。
- event pump 故障时退化为有限的 operation wait dispatcher，并报告 binding error；不能把活动 Task 永久挂起。

### 错误、Frame 与事件

- `EasyConException` 按 domain 派生 `ControllerException`、`AutomationException`、`VisionException` 等；`Code` 始终可用。
- compile errors 是 `CompilationResult.Diagnostics`，不是逐条 exception；对 not-runnable Program 调用 Run 才抛 `CompilationException`。
- Frame 默认 `CopyPixels()`/`EncodeAsync()`；不返回跨 await 存活的裸 span。
- Events 使用 `IAsyncEnumerable<EasyConEvent>`，每次枚举创建独立 subscription；取消枚举只关闭该订阅。

## 5. Python SDK

### Python 目标与底层

- Python 3.10+，首发 `py3-none-win_amd64` wheel。
- 低层使用标准库 `ctypes` 从生成声明装载 package-private `easycon_core.dll`，不要求本机编译 Python extension。
- public 包使用 type hints、`py.typed`、dataclass/enum 和泛型 operation。
- 同步与异步 API 共享同一对象，不复制 native 调用逻辑。

示意 API：

```python
async with easycon.Runtime(options) as runtime:
    devices = await runtime.controllers.discover()
    async with runtime.controllers.create() as controller:
        await controller.connect(devices[0])
        await controller.press(Button.A, duration=0.08)

    program = await runtime.automation.compile(bundle)
    run = runtime.automation.start(program)
    async for event in runtime.events():
        ...
    await run
```

### context manager 与 asyncio

- `Runtime`、`Controller`、`Capture` 同时实现 `__enter__/__exit__` 和 `__aenter__/__aexit__`。
- 同步 `close()` 阻塞；异步 `aclose()` 不阻塞 event loop。
- `Operation[T]` 实现 `__await__`、`done()`、`cancel()`、同步 `result(timeout=None)`。
- 每 Runtime 一个 Python daemon event-pump thread 只做 C ABI read 和 `loop.call_soon_threadsafe`；用户 handler 不在该线程运行。
- asyncio Task 被取消时请求 native cancel，然后用 shield 等待 native cleanup；只有核心终态为 Cancelled 才抛 `asyncio.CancelledError`，若 success/failure 已先提交则该唯一终态获胜。
- interpreter finalization 不可靠，不能作为资源正常释放路径。finalizer 只发 `ResourceWarning` 并 best-effort release。

### 错误与数据

- `EasyConError` 按 domain 派生；stable code 是 enum，未知值保留为整数。
- 编译诊断为 immutable dataclass list。
- Frame 默认 `bytes`/Pillow-compatible encoded bytes；v1 不要求 NumPy。可选 NumPy adapter 属于独立 extra 且只能读取复制数据。
- event async iterator 退出时关闭 subscription，队列 gap 映射成普通 `EventGap`。

## 6. Node.js / TypeScript SDK

### Node.js 目标与底层

- Node.js 22 和 24，CommonJS/ESM 双入口，TypeScript 声明同源。
- package `exports` 只提供 `node` 条件；检测到浏览器或非 Node runtime 时立即抛清晰错误。
- 使用稳定 Node-API addon 调用 C ABI。addon 不包含 Controller/ECS/Vision 业务逻辑，也不链接私有 bridge API。
- addon 的 `napi_async_work`/专用等待线程执行阻塞 wait/read；JS 主线程不阻塞。

示意 API：

```ts
await using runtime = await EasyConRuntime.create(options);
const devices = await runtime.controllers.discover({ signal });
await using controller = runtime.controllers.create();
await controller.connect(devices[0], { signal });

const program = await runtime.automation.compile(bundle, { signal });
const run = runtime.automation.start(program, { signal });
for await (const event of runtime.events({ signal })) {
  // discriminated union
}
await run.result;
```

### Promise、AbortSignal 与 dispose

- 每个异步方法返回 Promise；内部对象持有 native operation 直到 terminal。
- `AbortSignal` 已 aborted 时不提交 operation；提交后 abort 只调用 native cancel。
- Promise 只在 native terminal 后 resolve/reject，竞争规则与其他语言一致。
- 主动资源提供幂等 `dispose(): Promise<void>`、`close()` alias 和 `[Symbol.asyncDispose]()`；`[Symbol.dispose]` 仅释放已经关闭的对象，否则抛明确状态错误，避免同步阻塞 event loop。
- addon cleanup hook 请求关闭所有 Runtime 并等待 worker 退出；这只是进程退出兜底，不替代显式 dispose。

### 类型和数据

- event 是 TypeScript discriminated union，`kind` 对应 stable event kind；未知 kind 变成 `UnknownEvent` 并保留 raw code。
- `EasyConError` 扩展 `Error`，有 `code`、`domain`、`nativeCode`、`operationId` 和 `cause`。
- Frame 编码返回 Node `Buffer` 的拷贝；native frame 不以 external ArrayBuffer 暴露，避免 GC/finalizer 跨线程生命周期。
- npm 包不得声明 browser field，也不得提供空实现。

## 7. API 命名对照

| 语义 | C++ | .NET | Python | TypeScript |
| --- | --- | --- | --- | --- |
| 发现设备 | `discover()` | `DiscoverAsync` | `discover` | `discover` |
| 连接 | `connect()` | `ConnectAsync` | `connect` | `connect` |
| 精确序列 | `execute(sequence)` | `ExecuteAsync` | `execute` | `execute` |
| 编译 | `compile(bundle)` | `CompileAsync` | `compile` | `compile` |
| 启动脚本 | `run(program)` | `StartAsync` | `start` | `start` |
| 停止 | `cancel/stop` | `StopAsync` / token | `cancel` | `AbortController` / `cancel` |
| 截图 | `snapshot()` | `SnapshotAsync` | `snapshot` | `snapshot` |
| 模板匹配 | `match_template` | `MatchTemplateAsync` | `match_template` | `matchTemplate` |
| OCR | `recognize_text` | `RecognizeTextAsync` | `recognize_text` | `recognizeText` |
| 颜色检测 | `detect_color` | `DetectColorAsync` | `detect_color` | `detectColor` |

命名不同不代表语义不同。请求 defaults、范围、错误、取消和结果字段由共同行为规范生成测试。

## 8. 绑定不得做的事

1. 自行重试连接、改变超时、吞掉 queue gap 或改写 error code。
2. 在语言层实现动作序列、ECS、标签解析、分数归一化或 Controller 状态。
3. 以 finalizer 作为正常关闭机制。
4. 在 callback/event pump 线程执行用户代码。
5. 暴露 raw native pointer 给普通用户。
6. 打包与其他语言包不同 build ID 的核心。
7. 在 Node 包中提供浏览器替代，在 Python 包中隐式下载 DLL，在 .NET 包中依赖系统 PATH。

## 9. 共同验收

四语言必须运行同一 conformance corpus，并在规范化后产生完全相同的：

- operation state 序列与 terminal code；
- event kind/sequence/source/operation 关系；
- ECS diagnostics code/span 和输出顺序；
- Controller report bytes 与 precise sequence timeline；
- Vision score、位置、文本、颜色统计；
- cancel、deadline、wait timeout 和 dispose 结果。

任何 binding 只能在全部共同场景通过后发布；“某语言暂时跳过”不满足 v1 完成门槛。
