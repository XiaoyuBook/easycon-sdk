# 四语言绑定设计

本章落实 [ADR-0023](../decisions/0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md)：四种 SDK 是 Runtime、Controller 和
Vision 的宿主语言适配层，不是另一套业务核心，也不提供 ECS/Automation API。

## 1. 共同语义

- 一个 `Runtime` 是资源根，支持显式 close/dispose。
- discover、connect、Controller direct call、`ActionSequence`、snapshot、template、OCR 和 color 使用宿主语言的异步惯用形态。
- 调用方用普通函数、协程、Task 或 Promise 组合业务流程。普通业务计时由宿主语言负责；精确 press/release/delay 只交给
  核心校验并调度的 `ActionSequence`。
- v1 不冻结公共 `wait()` API。异步对象的完成、取消、错误和事件观察由已冻结 C ABI 与各语言惯用机制表达。
- Error 由 stable domain/code 映射，保留 message、native code、operation/resource ID 和 cause。
- Event 包含同一 sequence/kind/payload；语言层不得制造或吞掉核心状态事件。
- Frame、Image、Label 和结果值是不可变对象；TypeScript 只支持 Node.js，不提供浏览器降级实现。

| 核心概念 | C++ | .NET | Python | Node/TypeScript |
| --- | --- | --- | --- | --- |
| Runtime close | RAII + `close` | `IAsyncDisposable` | context manager / async context manager | `dispose()` / `Symbol.asyncDispose` |
| Operation | coroutine/`Operation<T>` | `Task<T>` + internal SafeHandle | awaitable `Operation[T]` | `Promise<T>` + internal native op |
| Cancel | `std::stop_token` / `cancel` | `CancellationToken` | `cancel()` / task cancellation | `AbortSignal` |
| Error | typed `std::system_error`-style exception | typed `EasyConException` | typed exceptions | typed `EasyConError` |
| Events | subscription/range | `IAsyncEnumerable<Event>` | async iterator | `AsyncIterable<Event>` |

## 2. 生成与手写边界

一份 tracked ABI manifest 描述 symbol、handle、enum、struct、ownership、blocking 和 result kind。由它生成：

- C declarations 和 symbol allowlist；
- C++ low-level calls；
- .NET `LibraryImport` declarations；
- Python `ctypes` declarations；
- Node addon C ABI declaration table；
- enum/error/event conformance vectors。

生成层不包含用户 API。每种语言在其上手写小型惯用 wrapper、文档和类型转换。业务校验只存在于 Rust；binding 可以做快速
参数检查，但核心仍重复验证。所有 binding 只装载同一 canonical native bundle。

## 3. C++ SDK

C++ 是第三阶段的优先实现语言，但其可用候选不等于四语言 GA。

- C++20、MSVC v143；namespace `easycon`。
- `EasyCon::SDK` CMake target 提供 public C header、RAII wrapper 和 import library；不链接私有 C++ native bridge。
- handle wrapper move-only；共享 immutable 值显式 `clone()`。

示意 API：

```cpp
easycon::Runtime runtime(options);
auto devices = co_await runtime.controller().discover();
auto controller = runtime.controller().create();
co_await controller.connect(devices.front().id);

easycon::ActionSequence sequence;
sequence.press(0ms, easycon::Button::A)
        .release(80ms, easycon::Button::A);
co_await controller.execute(sequence);

auto frame = co_await runtime.vision().snapshot();
auto match = co_await runtime.vision().match_template(frame, label);
```

- 正常路径是显式 close + release；析构不抛、不阻塞、不启动 finalizer。
- `std::stop_token` overload 只注册取消请求；token 销毁不释放 native operation。
- `EventSubscription` 是 move-only input range；明确标注的 `next(timeout)` 只读取事件，不定义通用 operation `wait()`。

## 4. .NET SDK

- 包目标 `net8.0`，使用 source-generated `LibraryImport`，禁用隐式字符串 marshalling。
- 每种 native handle 对应 sealed `SafeHandle`；public root 是 `EasyConRuntime : IAsyncDisposable, IDisposable`。
- 每 Runtime 的 managed event pump 只完成 Task/事件映射，使用 `RunContinuationsAsynchronously`，不执行用户 continuation。

```csharp
await using var runtime = await EasyConRuntime.CreateAsync(options, cancellationToken);
var devices = await runtime.Controllers.DiscoverAsync(cancellationToken);
await using var controller = runtime.Controllers.Create();
await controller.ConnectAsync(devices[0], cancellationToken);

var sequence = ActionSequence.Create()
    .Press(TimeSpan.Zero, Button.A)
    .Release(TimeSpan.FromMilliseconds(80), Button.A);
await controller.ExecuteAsync(sequence, cancellationToken);

var frame = await runtime.Vision.SnapshotAsync(cancellationToken);
var result = await runtime.Vision.MatchTemplateAsync(frame, label, cancellationToken);
```

`CancellationToken.Register` 只调用 native cancel；只有核心终态为 Cancelled 才将 Task 标为 canceled。Frame 默认通过
`CopyPixels()`/`EncodeAsync()` 交付复制数据，事件使用 `IAsyncEnumerable<EasyConEvent>`。

## 5. Python SDK

- Python 3.10+，首发 `py3-none-win_amd64` wheel。
- 标准库 `ctypes` 从生成声明装载 package-private `easycon_core.dll`，不要求用户编译扩展。
- 同步与异步 API 共享同一 native 调用逻辑，public 包提供 type hints、`py.typed`、dataclass/enum 和泛型 operation。

```python
async with easycon.Runtime(options) as runtime:
    devices = await runtime.controllers.discover()
    async with runtime.controllers.create() as controller:
        await controller.connect(devices[0])
        sequence = (
            easycon.ActionSequence()
            .press(0.0, easycon.Button.A)
            .release(0.08, easycon.Button.A)
        )
        await controller.execute(sequence)

    frame = await runtime.vision.snapshot()
    result = await runtime.vision.match_template(frame, label)
```

`asyncio` task 取消会请求 native cancel，再等待核心 cleanup；interpreter finalization 只报告遗漏，不是正常释放路径。普通
业务延迟使用应用自己的 asyncio/同步计时，不能伪装为 SDK precision contract。

## 6. Node.js / TypeScript SDK

- Node.js 22 和 24，CommonJS/ESM 双入口，TypeScript 声明同源。
- 稳定 Node-API addon 调用 C ABI；addon 不包含 Controller/Vision 业务逻辑，也不链接私有 bridge API。
- addon 的等待线程只执行已明确的 native event read/observation；JS 主线程不阻塞。

```ts
await using runtime = await EasyConRuntime.create(options);
const devices = await runtime.controllers.discover({ signal });
await using controller = runtime.controllers.create();
await controller.connect(devices[0], { signal });

const sequence = new ActionSequence()
  .press(0, Button.A)
  .release(80, Button.A);
await controller.execute(sequence, { signal });

const frame = await runtime.vision.snapshot({ signal });
const result = await runtime.vision.matchTemplate(frame, label, { signal });
```

每个异步方法返回 Promise；`AbortSignal` 在提交后只请求 native cancel。资源提供幂等 `dispose()`、`close()` 和
`[Symbol.asyncDispose]()`；addon cleanup hook 不得暗中执行同步关闭。

## 7. API 命名对照

| 语义 | C++ | .NET | Python | TypeScript |
| --- | --- | --- | --- | --- |
| 发现设备 | `discover()` | `DiscoverAsync` | `discover` | `discover` |
| 连接 | `connect()` | `ConnectAsync` | `connect` | `connect` |
| 直接 Controller 动作 | `press()` / `set_stick()` | `PressAsync` / `SetStickAsync` | `press` / `set_stick` | `press` / `setStick` |
| 精确序列 | `execute(sequence)` | `ExecuteAsync` | `execute` | `execute` |
| 截图 | `snapshot()` | `SnapshotAsync` | `snapshot` | `snapshot` |
| 模板匹配 | `match_template` | `MatchTemplateAsync` | `match_template` | `matchTemplate` |
| OCR | `recognize_text` | `RecognizeTextAsync` | `recognize_text` | `recognizeText` |
| 颜色检测 | `detect_color` | `DetectColorAsync` | `detect_color` | `detectColor` |

命名不同不代表语义不同。请求 defaults、范围、错误、取消、ActionSequence 时序和结果字段由共同行为规范生成测试。

## 8. 绑定不得做的事

1. 自行重试连接、改变 timeout、吞掉 queue gap 或改写 error code。
2. 在语言层实现 `ActionSequence`、Controller 状态、Vision 标签解析或分数归一化。
3. 以 finalizer 作为正常关闭机制，或在 callback/event pump 线程执行用户代码。
4. 暴露 raw native pointer 给普通用户，或打包与其他语言不同 build ID 的核心。
5. 提供 browser fallback、隐式下载 DLL、依赖系统 PATH、ECS/Automation API 或自定义 callback runner。

## 9. 共同验收

四语言对同一个 fake-enabled core 运行共同场景，并在规范化后产生相同的：

- Runtime/operation/event/close 状态序列与 terminal code；
- Controller direct actions、report bytes、`ActionSequence` timeline、取消和中立化结果；
- Vision snapshot、标签、模板、OCR、颜色结果；
- cancellation、deadline、event gap 和 dispose 结果；
- canonical native bundle 的 build ID、ABI major/minor 和 manifest hash。

ECS diagnostics、Program、compile/run trace 不属于此共同验收。四语言全部通过后才可进入同一 release train；C++ 候选只作为
第三阶段的先行里程碑。
