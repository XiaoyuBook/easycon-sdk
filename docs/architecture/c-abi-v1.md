# C ABI v1 设计

## 1. 范围与原则

本文件定义 v1 ABI 的规则和接口形态，不是正式头文件，也不分配最终函数清单。实现阶段必须从一份机器可读 ABI manifest 生成 C 头、语言声明和 symbol allowlist，再按本文件冻结。

**[已决定]** C ABI 是四语言唯一底层边界。ABI 只表达稳定标量、版本化结构体和不透明句柄，不暴露 Rust layout、C++ class、STL、OpenCV `Mat`、Tesseract 对象或平台 handle。

核心规则：

1. 每个函数都有明确 ownership、线程安全和阻塞属性。
2. 每个耗时动作返回 operation；只有显式 wait/read/close 可以阻塞。
3. 没有用户 callback。日志、状态和完成通知全部从 subscription 拉取。
4. 所有文本是带长度 UTF-8；不把 NUL 终止当成边界。
5. 所有跨边界分配都由分配它的一侧释放。
6. Rust panic 和 C++ exception 都转换成错误，绝不跨 ABI unwind。

## 2. 命名、调用约定与可见性

- public symbol 前缀固定为 `easycon_v1_`，例如 `easycon_v1_runtime_create`。
- public type 前缀固定为 `easycon_`，例如 `easycon_runtime_t`、`easycon_status_t`。
- Windows 调用约定固定 `cdecl`；头文件用 `EASYCON_API` 和 `EASYCON_CALL` 宏表达导入/导出与调用约定。
- 只导出 allowlist 中的 `easycon_v1_*`；Rust、C++、OpenCV、Tesseract 和 CRT 符号默认隐藏。
- C 头兼容 C11；用 C++ 编译时放在 `extern "C"`，不改变类型或重载。
- public symbol 不使用名称修饰，不以 feature flag 删除已经发布的 v1 符号。

`v1` 是 ABI major，不是 SDK patch 版本。SDK 1.x 可增加 v1 符号和结构体尾字段；破坏性变化必须新增 `easycon_v2_*`，不能复用旧名。

## 3. 基础标量与结构体

### 标量

- 整数只用 `<stdint.h>` 固定宽度类型。
- 长度、容量、时间统一 `uint64_t`；避免 `size_t` 造成 32/64 位布局差异。
- 布尔输入/输出使用 `uint8_t`，只接受 0 或 1。
- stable enum/flags 的 ABI 表示为 `uint32_t` typedef + 常量宏，不使用编译器大小可变的 C enum。
- resource ID、operation ID、event sequence 使用 `uint64_t`；0 保留为“无”。
- v1 Windows x64 使用 little-endian；持久化/网络格式若以后出现，必须另行定义字节序，不能直接 dump 结构体。

### 版本化结构体

每个 public options/result value struct 都以以下逻辑前缀开始：

```c
uint32_t size;
uint32_t version;
```

规则：

1. 调用方先清零整个结构体，再设置 `size = sizeof(struct)`、`version = 1`。
2. 输入结构体必须至少覆盖该函数要求的最小前缀；未覆盖的尾字段使用文档默认值。
3. callee 只读取 `min(size, known_size)`，并要求所有已覆盖 reserved 字段为 0。
4. 输出结构体只写调用方声明的容量；未知尾部保持 0。
5. v1 内只追加尾字段，`version` 保持 1。若字段意义、顺序或对齐不能兼容，定义新结构体名或 ABI v2。
6. 禁止 `#pragma pack`。生成头对每个 target 写入 `sizeof`/`offsetof` static assertions。
7. public struct 不内嵌可变长数组、平台指针 union、C bitfield 或语言 runtime 对象。

## 4. 不透明句柄

计划中的 typed handle：

| 句柄 | 所有权 |
| --- | --- |
| `easycon_runtime_t` | Runtime 根资源 |
| `easycon_controller_t` | 一个 Controller session |
| `easycon_capture_t` | 一个 Capture session |
| `easycon_program_t` | 不可变 ECS Program |
| `easycon_operation_t` | 异步 operation 观察句柄 |
| `easycon_event_subscription_t` | 独立事件队列 |
| `easycon_event_t` | 一条不可变事件 |
| `easycon_frame_t` / `easycon_image_t` | 不可变像素资源 |
| `easycon_label_t` | 不可变图像标签 |
| `easycon_buffer_t` | 不可变 owned bytes/text |
| `easycon_error_t` | 不可变错误链 |
| typed list/result handles | 设备、诊断、匹配等复合结果 |

ABI 声明使用 incomplete struct pointer。内部 wrapper header 含 ABI/type magic、generation、runtime ID 和一个 Rust `Arc`，但这些字段不公开。

### 句柄规则

- create/take-result 成功时通过 `T** out` 交付一个独立 owning wrapper。
- `*_clone_handle(source, T** out)` 在需要共享时创建另一个 owning wrapper；不是返回同一个 raw pointer。
- `*_release(NULL)` 成功且无操作；release 消耗该 wrapper。
- 同一非 NULL raw pointer 只能 release 一次。释放后使用或二次释放属于 C 调用方未定义行为，binding 必须把字段原子置 NULL。
- 有效但类型错误、ABI magic 错误或跨 Runtime 的 handle 返回稳定错误；不能靠 C cast 绕过。
- 关闭 Runtime 会使主动子资源停止，但 immutable Frame/Image/Program/Event/Error/Buffer 可读到其各自最后引用释放；它们不能再启动新 operation。
- handle 存活不会允许动态库被 binding 主动卸载。官方 binding 在进程期内固定装载核心。

## 5. UTF-8 与内存

### 输入文本

逻辑形态为：

```c
typedef struct easycon_utf8_view_t {
    const char* data;
    uint64_t length;
} easycon_utf8_view_t;
```

- `length == 0` 时 `data` 可为 NULL。
- 输入只在函数返回前借用；需要异步使用的文本在创建 operation 时复制进核心。
- 必须是严格 UTF-8。路径、端口名、label 名和标识符额外拒绝内嵌 NUL。
- 文件路径按 Windows UTF-16 系统调用转换，错误位置仍以输入 UTF-8 byte offset 表达。
- 不执行隐式本地代码页转换和 Unicode 大小写猜测；需要大小写不敏感的 ECS token 按语言规范处理。

### 输出内存

- 不返回需要调用方 `free` 的裸指针。
- 变长输出使用 typed list handle 或 `easycon_buffer_t`。
- buffer data 是只读 borrowed pointer，在 buffer release 前有效；binding 默认复制到自身 `string`/`bytes`/`Buffer`。
- buffer 由 `easycon_v1_buffer_release` 释放，error/event/result 由各自 release 释放。
- 调用方传入的数组/缓冲只在函数期间借用；异步 operation 创建时完成深拷贝或取得明确的 immutable handle clone。
- C++/Rust CRT allocator 从不跨边界配对。

截图和图像编码返回 Buffer；零拷贝 frame 访问只提供只读 data/stride view，且 frame handle 必须在访问期间存活。Node/Python/.NET 的默认高层 API复制数据，显式 unsafe/advanced view 不进入 v1。

## 6. 状态与错误模型

每个 fallible C 函数返回 `easycon_status_t`。返回值只表示“本次 ABI 调用是否成功受理/读取”，不代表异步 operation 最终成功。

稳定 code 分组：

| domain | 代表 code |
| --- | --- |
| ABI | INVALID_ARGUMENT、INVALID_UTF8、ABI_MISMATCH、INVALID_HANDLE、WRONG_HANDLE_TYPE、WRONG_RUNTIME |
| Runtime | RUNTIME_CLOSING、INVALID_STATE、RESOURCE_BUSY、RESOURCE_EXHAUSTED、UNSUPPORTED |
| Wait/cancel | WAIT_TIMEOUT、CANCELLED、DEADLINE_EXCEEDED、QUEUE_CLOSED |
| I/O/Controller | NOT_FOUND、ACCESS_DENIED、IO_ERROR、DEVICE_DISCONNECTED、PROTOCOL_ERROR、ACK_TIMEOUT |
| Automation | COMPILE_FAILED、SCRIPT_RUNTIME_ERROR、MISSING_DEPENDENCY |
| Vision | NO_FRAME、INVALID_IMAGE、MODEL_NOT_FOUND、VISION_ERROR |
| Isolation | NATIVE_EXCEPTION、PANIC、INTERNAL |

数值一旦发布不复用。binding 按 code/domain 映射类型，绝不解析本地化 message。

### 错误对象

所有会产生细节的同步函数接受可选 `easycon_error_t** out_error`：

- 成功时写 NULL。
- 失败时可返回 owned error，包含 code/domain/message/platform code/resource ID/operation ID/cause。
- 调用方可传 NULL，仍得到 status。
- 不提供 thread-local last-error，避免异步和嵌套调用覆盖。

异步失败存入 operation：先等待终态，再用 `operation_error_clone` 获取 error。`Succeeded` 无 error；`Failed` 必须有；`Cancelled` 可有 cancellation reason error；result 与 error 不能同时存在。

诊断不是 exception：ECS compile operation 可成功产生 Program + diagnostic list；若有 error diagnostics，Program 标记 not-runnable，直接 run 返回 `COMPILE_FAILED`。这保留一次获取全部诊断的能力。

## 7. Operation handle

### 创建与观察

耗时 API 采用形态：同步验证 + 创建 operation + 返回 handle。operation 类型在内部固定，result accessor 会验证 kind。

代表性形态（仅设计示例）：

```c
status controller_connect(controller, options, &operation, &error);
status operation_wait(operation, timeout_ms, &state, &error);
status operation_cancel(operation, &error);
status controller_connect_result(operation, &info, &error);
```

`operation_wait` 的 ABI 调用成功表示得到了一个状态。若 timeout 到期，返回 `WAIT_TIMEOUT`；operation 本身没有被取消。`operation_cancel` 幂等，只发请求；必须 wait 到 terminal 才完成语言级取消。

### 状态

public state 是 `PENDING`、`RUNNING`、`CANCELLING`、`SUCCEEDED`、`FAILED`、`CANCELLED`。同时暴露：

- operation ID；
- operation kind；
- progress（可选、0.0..1.0）；
- created/started/completed monotonic timestamp；
- cancel reason；
- result kind。

### 结果

- 小型固定结果通过版本化 output struct 复制。
- 设备列表、诊断列表、Frame、Buffer 等以新 owning handle 取出。
- `take_result` 只能成功一次时，函数名必须包含 `take`；可重复读取的结果使用 `clone_result`。
- accessor 在 operation 未成功终态时返回 `INVALID_STATE`，不能隐式 wait。
- release operation 不取消；语言 Task/Promise 仍由 binding 的内部 registry 保留观察引用直到完成。

## 8. 事件读取

代表性 C ABI 流程：

1. `event_subscribe(runtime, options, &subscription, &error)`。
2. `event_read(subscription, timeout_ms, &event, &error)`。
3. 用 common/kind-specific accessor 读取 event。
4. `event_release(event)`。
5. `event_subscription_close/release`。

事件本身是不透明 handle，避免在 v1 结构体中冻结庞大 union。common accessor 返回 sequence、timestamp、kind、severity、resource ID、operation ID、stable code；kind-specific accessor 返回版本化 payload struct 或 Buffer。

`event_read`：

- 有事件：返回 OK 和 owned event。
- 等待超时：返回 `WAIT_TIMEOUT`，event 为 NULL。
- 队列 drain 完且关闭：返回 `QUEUE_CLOSED`。
- 不把 gap/overflow 当成 ABI 失败；它是一种正常 event kind。

Operation terminal event 便于 binding 完成 Task/Promise，但 operation query 是最终真值。binding 在注册 terminal listener 后立即查询一次状态，避免“先完成、后登记”竞态。

## 9. 能力 API 形态

本阶段只冻结类别，不冻结完整函数名：

### Runtime

- ABI/library/build info 查询；
- create、state、close、release；
- capability query；
- event subscribe。

### Controller

- discover operation → device list；
- create session、connect/disconnect/state；
- button/HAT/stick/reset operation；
- validated precise sequence operation；
- Amiibo save/select operation。

### Automation

- compile source bundle/directory operation → Program + diagnostics；
- Program metadata/required labels/hash；
- run Program operation；
- operation cancel 作为 stop；
- run state/log/state events。

### Vision

- discover/open/close capture；
- snapshot operation → Frame；
- image decode/encode；
- `.IL` load/save → Label/Buffer；
- template/OCR/color operation → typed result。

API 不暴露 Python/Lua、固件写入、远端脚本、远程助手、UI 或推送函数。capability query 对未知 capability 返回 false，不用“函数存在但总是 unsupported”扩充表面。

## 10. 线程安全与重入

| 对象/调用 | 线程安全约束 |
| --- | --- |
| Runtime query/submit | 可从任意线程并发 |
| Operation status/wait/cancel | 可并发；多个 wait 均可观察同一终态 |
| Event subscription read | 每个 subscription 允许一个并发 reader；多个 reader 返回 `RESOURCE_BUSY` |
| Controller submit/query | 可并发，内部序列化；release 需调用方同步 |
| Capture/Frame/Vision | query/submit 可并发；配置通过 capture lane 串行 |
| Program/Label/Frame/Buffer/Error/Event accessor | immutable，可并发读取 |
| close | 幂等，可与 operation 并发；close 获胜后新提交被拒绝 |
| release | 不可与同一个 raw wrapper 的其他调用并发 |

核心不执行用户 callback，因此不存在从核心线程重入语言代码。C ABI function 也不持有内部全局锁执行 native I/O、等待或分配语言对象。

## 11. panic/exception 隔离

### Rust export

- 每个 public export 经过统一 trampoline 和 `catch_unwind`。
- FFI release 构建使用可捕获 unwind；panic payload 被净化为 `PANIC` error，不暴露敏感 backtrace。
- 若能识别 Runtime，额外发布 fatal error event。
- OOM、栈溢出和进程破坏不承诺恢复；文档不能把它们伪装成普通 status。

### C++ bridge

- 所有内部 entry point 为 `noexcept`。
- 捕获 `cv::Exception`、`std::exception` 和未知异常，生成 bridge-owned error。
- Rust wrapper 在同一次调用中复制/接管 error 并按桥接释放函数释放。
- C++ 析构也不得抛异常；析构错误转为 cleanup warning。

### 语言层

- C++ wrapper 把 error 转为 `easycon::Error` / typed exception。
- .NET/Python/Node wrapper 只从 status + error 构造 exception。
- language exception 永远不会被传回核心线程；binding event pump 捕获并隔离消费者错误。

## 12. 兼容与协商

核心提供无 Runtime 的只读 ABI info：

- ABI major/minor；
- SDK semantic version；
- build ID；
- target triple；
- feature bitset；
- native dependency manifest hash。

binding 初始化时检查：

1. ABI major 必须等于 1。
2. library minor 至少满足 binding 所需的最小 minor。
3. 必需 symbol/capability 存在。
4. 官方包中的 expected build ID/manifest hash 与实际核心一致。

同 ABI major 的 patch/minor 升级允许：新增 symbol、status、event kind、可选 capability、结构体尾字段。调用方遇到未知 status/event 必须保留数值并映射为 Unknown，而不是崩溃。

不允许：改变既有数值、缩小结构体、改变字段意义/单位、改变 blocking/ownership、删除 symbol、让此前线程安全的调用变成不安全。

## 13. 输入限制与防御

实现前为以下内容定义 Runtime 可配置且有硬上限的 limit：

- source 文件数、单文件字节、总 source bytes、诊断数；
- action sequence step 数和总跨度；
- image encoded bytes、解码像素数、width/height/stride；
- label 数、名称长度、Base64 target bytes；
- OCR 文本长度和 model 数；
- event queue capacity、log payload 和 operation 数。

所有乘法/offset 使用 checked arithmetic。ROI 必须在 Frame 边界内；图像解码在 native compute pool 受限执行；路径加载只允许显式 root，拒绝跳出 root 的规范化路径。

## 14. ABI 冻结门槛

正式 `easycon.h` 只有在以下条件同时满足后才能标记 v1：

- 资源、operation、错误和事件模型已有 Rust reference implementation；
- C 与 C++ smoke client 覆盖所有 ownership 路径；
- 旧头/新库和新头/旧库兼容矩阵通过；
- 四语言原型从同一 manifest 生成并通过一致性场景；
- symbol/layout golden 已建立；
- sanitizer/fuzz 未发现跨边界泄漏、越界或 unwind；
- O-01/O-02 的硬件能力不会迫使改变现有字段，只需 capability/options 尾字段。
