# 0017：提议冻结 Phase 4 ECS 与 Automation 目标

- 状态：Proposed / Not Effective
- 提议日期：2026-07-24
- 治理起点：[ADR-0016](0016-phase-3-downstream-reopen-boundary.md) 的 `Refrozen Governance Boundary`
- G0a governance predecessor：`38ef0dc301a2cdadafe91846b4a1b80918b54297`，tree
  `e0766c62277e16763fd6af79c7622a2f29a93e70`，parent
  `e3f0df9865119877b95732d2a6100ef15ed7ab9b`
- 初始 G0b proposal / REWORK 起点：`6468da576471975a05458767f14a55a31926e169`，tree
  `4b7f641090bd261acf39bf5570840c8b0776475e`，parent
  `38ef0dc301a2cdadafe91846b4a1b80918b54297`
- proposal chain 来源：`origin/main@bc24f0bfe65a34ba54ffacad62dd41905c77f952`
- legacy 只读证据：`EasyCon@11c4b992b9bce0ff977e9c587a6c0bb0d302853e`
- 编号说明：ADR-0015 保留给 Phase 2B；落盘前扫描全部本地 refs 与 worktrees，ADR-0016 已占用，0017 无冲突

## 状态、生效条件与审查链

本文件只是 Phase 4 target proposal，不是已接受决定，也不授权实现。必须依次完成以下步骤：

1. 将本文件所在的固定 proposal Git 对象交给新的独立 design reviewer；
2. 清零全部可复现、可行动且 in-scope 的 P0/P1/P2 finding；
3. 由后续单独的 docs-only acceptance/freeze commit 把状态推进为已接受。

在第三步完成前，不得启动 R0、W0、S0、D0 或任何 Phase 4/5 实现。本 proposal 的提交 SHA/tree 只由提交后的
Git 对象、固定 ref 与外部结构化 `TASK_REPORT` 记录；未来 acceptance commit 也不得在 tracked 文件中记录或预言
自身 SHA/tree。任一步发生语义修订，都必须固定新的 proposal SHA 并重新接受完整独立审查。

初始 proposal `6468da5` 的独立 design review 任务 `019f922d-192f-7e70-b32a-6b76eeb3ee8b` 结论为
`REWORK`（P0=0、P1=3、P2=2）。本次 docs-only 修订只关闭该轮五项 finding，不是 acceptance/freeze，状态仍为
`Proposed / Not Effective`。修订提交必须作为新的 fixed-SHA proposal 接受一次完整独立 re-review；不得把对
`6468da5` 的审查结论沿用为对修订后对象的批准。

本提议吸收了 architecture input 任务 `019f8e7d-67ee-7920-9e0c-c05b803c7415`、独立 `REWORK` review
任务 `019f8e9a-6c58-71e0-8f2b-8a45c36c1526` 的 `0 P0 / 7 P1 / 3 P2` 修订，以及 product decision
evidence 任务 `019f8eba-de1d-7042-aa83-c6c989de5e54`。产品方已对 Q1-Q3 全部选择推荐默认；这些选择在下文
明确标成 v1 产品合同，而不是 legacy 或现有 CI 已证明的事实。ADR-0016 已由任务
`019f9205-97e0-7b91-9796-b5a4be507441` 最终重新冻结，因此 Phase 4 的纯下游 target proposal 不再自动重开
Phase 3；实际修改 Phase 3 冻结面时仍须按 ADR-0016 重新打开。

## 所有权、依赖与阶段边界

Phase 4 新增 `easycon-ecs`，它拥有：

- ECS lexer、parser、binder、lowerer、compiler 与 evaluator；
- `SourceBundle`、受限 loader、不可变 `Program`、`ProgramHash` 与 compile diagnostics；
- `AutomationRun`、不可变 `RunCompletion`、领域 `RunFailure` 与 logical dependency summary；
- 抽象 `ControllerPort`、`VisionPort`、`OutputPort` 及其确定性 `RecordingPorts`；
- ECS fixture、schema、conformance mapping、golden vector 与 Phase 4 测试。

`easycon-ecs` 可以依赖 `easycon-model`、`easycon-runtime` 和经审计的纯算法库，但不得依赖
`easycon-controller`、`easycon-vision`、`easycon-native-sys` 或任何 concrete device adapter。内部 crate
dependency whitelist 在 W0 一次性登记，后续改变依赖方向必须重新审查。Runtime 继续只拥有通用 operation、resource、
task、clock、cancellation、terminal transaction 与 close；它不知道 ECS syntax、Program、Controller、Vision、label、
PRINT 或其他设备领域。

Phase 5 的 `easycon-sdk` 才拥有：

- `ControllerPort`/`VisionPort`/`OutputPort` 的 concrete adapters；
- 每个 Runtime 的 single-Automation-run gate；
- logical dependency 到 concrete `ResourceId` 的 admission、注册和关闭顺序；
- 面向 public C ABI/四语言的一条统一 Automation event 投影。

因此 Phase 4 不创建 `easycon-sdk`，不绑定 concrete resource ID，也不改变 public ABI。`RecordingPorts` 必须足以在
Phase 4 内验证全部编译、执行、背压、取消、cleanup 与 terminal race 合同。只消费现有 Phase 3 public-neutral Vision
合同不修改 Phase 3 owned file；若实际 diff 改变 Capture/NativePool/Vision contract、fixture、toolchain、平台或支持
状态，则按 ADR-0016 重新打开 Phase 3。

## Runtime 的窄重开决定

[ADR-0006](0006-runtime-stabilization.md) 与 [ADR-0007](0007-phase-1-freeze.md) 已冻结 terminal event、registry
unlink、waiter notification 及 deterministic close 的共同事务，但当前 primitive 不能同时表达 awaited、fallible cleanup，
且 cancel 路径不能复用 cleanup callback。Phase 4 保留“终态前 cleanup 已 settled”和五路共同事务，因此后续 R0
必须窄重开 ADR-0007；不得以 Run 外部先 cleanup、再调用现有 finish/cancel 的竞态序列替代。

R0 只扩展一个内部共同 terminal permit/cleanup primitive，并固定以下顺序：

1. 一个 terminal caller 获得 permit，seal 新 child admission，并保留 first terminal/cancellation reason；
2. 向既有 children 传播 cancellation；此后所有新 child admission 稳定失败；
3. 在 Runtime state lock 外 await owner cleanup，得到成功、typed failure 或隔离后的 panic；
4. cleanup settled 后，由 permit owner 一次提交 terminal state、唯一 terminal event、registry unlink 与 waiter notify；
5. 重复或竞争的 terminal caller 只观察同一 completion，不得再次 cleanup、发事件或 unlink。

同一 primitive 必须覆盖 Success、Failure、Cancel、Deadline 和 ParentClose：

- 原执行 Success 且 cleanup 成功，generic operation `Succeeded(Unit)`；
- 原执行 Success 且 cleanup 返回错误，generic operation 改为 `Failed`；
- 原执行 Failed、Cancelled、Deadline 或 ParentClose 时，原 primary/first reason 保持权威，cleanup 错误只作为
  immutable `RunCompletion` 的 secondary diagnostic；包括 ParentClose 在内，secondary 不进入 `CloseReport`；
- cleanup panic 由 panic boundary 转为 `Internal`，不得 unwind 穿过 Runtime 或留下半终态；若已有 primary
  failure/cancellation，其领域记录保留 primary，并把 panic 记录为 secondary；
- generic result 仍是 `OperationValue::Unit`。

这是 R0 的明确所有权边界，而不是遗漏的 error projection。Runtime close 继续完全遵守 ADR-0006 的中立
`CloseOutcome`/`CloseReport` 规则：`CloseReport` 只记录 Runtime 自身 close start、resource、task、event、operation
finalization 或 registry close failure，不读取、不存储 ECS 类型，也不设置 Automation diagnostic collector。ParentClose 下的
Automation cleanup secondary 不会单独把 `CloseOutcome::Closed` 改为 `Failed`，不会替换已经保存的 Runtime failure，
也不会在 operation 已 terminal 后回写不可变 report；即使 Runtime 因较早的中立 close failure 已保存 report、而
Automation owner cleanup 此后才 settled，该 secondary 仍只在 owner 冻结的 `RunCompletion` 中可见。R0 因此不扩大
`CloseReport` schema、settle protocol、failure priority 或多错误模型。

R0 不扩 `ErrorDomain`、`ErrorCode`、Runtime `Event` 或 `OperationValue`，也不把 ECS payload 写入 `Event.detail`、
`Bytes` 或 JSON。R0 完成其 RED/model、完整 Phase 1 门禁、独立 review 与 refreeze 之前只阻断 E2；它不阻断 W0、
C1、C2、C3 或 E1。

## 领域错误、诊断与 Run 权威

`easycon-ecs::RunFailure` 与 immutable `RunCompletion` 是 Automation 领域终态的唯一 authority。现有
`EasyConError` 只是从领域结果到 generic Runtime operation 的单向、lossy projection；调用方不得从 generic code/detail
反解析领域失败，也不得用两份可独立修改的错误记录建立双权威。Compile error 发生在 run admission 前，由
`CompileReport`/`CompileDiagnostic` 表达，不创建部分 `Program` 或 `AutomationRun`。

`CompileDiagnostic` 归 `easycon-ecs`，至少包含 stable ECS code、severity、phase、`source_id`、权威 UTF-8
half-open `byte_span` 以及由源码派生的 display line/column。诊断稳定排序为 main、sorted-lib ordinal、byte start、
phase rank、severity、stable code、emission ordinal。此决定显式取代
[source capability map](../architecture/source-capability-map.md) 中把 ECS diagnostic 残留归到 `model::diagnostic`
的旧映射；本 proposal 不修改该历史输入，也不向 `easycon-model` 增加 ECS 类型。

`AutomationRun` 的不可变 start metadata 至少记录 `program_hash`、`seed`、logical dependencies 和与
`RunStarted` 共用的 monotonic clock sample。logical dependencies 只能是稳定的领域项，例如 controller required、
sorted label names、output capability；不包含 concrete `ResourceId`。`RunCompletion` 至少包含 authoritative outcome、
可选 `RunFailure`、first cancellation reason 与有序 secondary cleanup diagnostics。Phase 4 的 Runtime projection 仍只产生
generic lifecycle event；Phase 5 才把 run record、concrete dependencies 与 public event 统一投影。E2 必须在同一个
terminal permit 内先冻结 owner-owned `RunCompletion`，再让 generic terminal/event/unlink/notify 可见；observer 不得看到
已 terminal 但缺失或仍可变的 completion，Runtime 也不读取或存储 ECS 类型。

## ControllerPort 抽象合同

Phase 4 只冻结 port，不修改 production Controller：

- acquire 接收 cancellation token 与 Runtime-clock absolute deadline；contention 保持 fail-fast `ResourceBusy`，
  不引入排队公平性语义；
- acquire 必须在线性化点唯一决定 Granted、Cancelled(first reason)、Deadline、Closed 或 Failure；cancel/deadline
  已胜出后不得出现 late grant，receiver 消失也不得留下 orphan lease；
- 获得 lease 后，action、neutralize 与 release 都通过同一抽象 owner；stale generation 或重复 release 不得影响新 lease；
- `neutralize_and_release` 一旦开始，不再由已经触发的 run cancellation 中断；它必须返回 settled outcome，证明 neutral
  已被 transport 接受，或证明未交付且 stream 已 settled，或返回明确 cleanup failure；
- Drop 只能幂等触发非阻塞 cleanup，不能作为 terminal 前已经 settled 的证据；port close 必须结算 pending acquire、
  action 与 release waiter；
- `RecordingControllerPort` 必须确定性模拟 grant/cancel/deadline/close、effect linearization 和 settled cleanup。

[ADR-0009](0009-phase-2a-freeze.md) 的 production Controller D0-D2 是 Phase 5 concrete adapter 的独立前置：D0
提出窄 reopen，D1 以回归实现 cancellable acquire 与 neutralize/release completion，D2 完整门禁、独立 review、refreeze。
它不是 G0b、W0、S0、C1-C3、E1、E2、Q0 或 Phase 4 Q1 的前置。

## SourceBundle、语言与 Unicode

compiler 的权威入口是纯内存 `SourceBundle`：exactly one Main，加 0..63 个 Lib；每项为 role、`source_id` 与原始
UTF-8 bytes。compiler 不读取 cwd、环境变量或文件系统。可选 restricted loader 只接受调用方显式 root 与 main，读取
main 和直属 `lib/*.ecs`，不递归、不因 IMPORT 额外加载；只接收 regular file，拒绝 symlink/junction/reparse、root escape、
invalid UTF-8 和 partial bundle，并在 read/copy/decode 前完成枚举、路径及 limits preflight。

语言 v1 固定以下 corrected 选择：

- `FOR lower TO upper` 的 bounds 各求值一次、upper inclusive；当 upper 为 `i32::MAX`，执行 upper 对应 iteration 后
  先比较相等并退出，不再做 `+1`；
- statement builtin 名按 ASCII 大小写不敏感匹配；expression builtin 名精确大小写匹配；user symbols 始终
  case-sensitive；
- `TRUE`/`FALSE` 必须生成并绑定 boolean literal，不再保留 lexer-only token 缺陷；
- LF、CRLF、CR 均形成一个 newline；CRLF 只计一次；
- 权威 source span 是剥离一个 leading BOM 后、exact UTF-8 source bytes 中的 half-open byte range；display
  line/column 为 1-based Unicode scalar，tab 计 1，不做 Unicode normalization；
- string `LEN`、index 与 slice 统一按 Unicode scalar 计数；negative/out-of-range 产生 stable typed failure，
  不泄漏 host panic。

语法错误、未知/未闭合 token、除零、错误 closer、重复声明、unsupported trailing syntax 和所有 limits violation 都必须
形成稳定 diagnostic 或 `RunFailure`，parser、binder、lowerer、evaluator 不得 panic。没有 error 时才发布深不可变
`Program`；它包含 semantics version、ProgramHash、sorted logical dependencies 和执行所需 IR，内部 serde/Debug/arena
布局不属于兼容合同。

## ProgramHash v1

`ProgramHash` 为 SHA-256，hash 输入不是 AST、serde 或 Debug。所有整数大端、无 padding、无字段名；除 domain
固定的最后一个 NUL 外没有终止符。`u32` 用于 version/count，`u64` 用于 byte length，role/布尔标志用 `u8`。
输入按以下顺序直接拼接：

1. 27 bytes ASCII domain `easycon-sdk:ecs-program:v1\0`，其中最后一个 byte 是 `00`；
2. `u32 hash_format_version = 1`；
3. `u32 ecs_semantics_version = 1`；
4. 下表顺序的完整 `EcsLimitsV1`；
5. `u32 source_count`；
6. main record，然后是按 `source_id` 原始 UTF-8 bytes 升序排列的 lib records。

每个 source record 精确编码为 `u8 role || u64 id_len || id_bytes || u64 source_len || source_bytes`；role `00`
为 Main、`01` 为 Lib。每项只剥离一个 leading UTF-8 BOM `EF BB BF`，hash 其后的 exact bytes；其他位置或第二个
BOM 不被归一化。CRLF/LF、空白和注释差异都会改变 hash。

`source_id` 必须是非空 slash-relative UTF-8：拒绝 NUL、absolute/drive/UNC、反斜杠，以及空、`.`、`..` segment。
拒绝 exact duplicate 和逐 byte ASCII-case-fold collision；非 ASCII byte-exact，不做 Unicode normalization。hash 包含
role/id、BOM 处理后的 source bytes、semantics version 与完整 limits profile；明确不包含 cwd/root、mtime、输入/枚举
顺序、build SHA、run seed、time/deadline 或 logical/concrete dependency ID。

`EcsLimitsV1` 的 canonical field 顺序同时是 hash framing 合同：

| 顺序 | 字段 | 编码 | v1 值 |
| ---: | --- | --- | ---: |
| 1 | `profile_version` | u32 | 1 |
| 2 | `source_units` | u32 | 64 |
| 3 | `per_source_bytes` | u64 | 262144 |
| 4 | `bundle_bytes` | u64 | 1048576 |
| 5 | `source_id_bytes` | u64 | 256 |
| 6 | `identifier_bytes` | u64 | 128 |
| 7 | `parameters` | u32 | 32 |
| 8 | `arguments` | u32 | 32 |
| 9 | `syntax_nesting` | u32 | 64 |
| 10 | `functions` | u32 | 256 |
| 11 | `symbols` | u32 | 4096 |
| 12 | `tokens` | u32 | 262144 |
| 13 | `ast_nodes` | u32 | 131072 |
| 14 | `bound_nodes` | u32 | 262144 |
| 15 | `lowered_nodes` | u32 | 262144 |
| 16 | `instructions` | u32 | 262144 |
| 17 | `diagnostics_per_source` | u32 | 64 |
| 18 | `diagnostics_total` | u32 | 512 |
| 19 | `reserved_limit_diagnostics` | u32 | 1 |
| 20 | `call_depth` | u32 | 128 |
| 21 | `array_cells` | u32 | 16384 |
| 22 | `string_bytes` | u64 | 262144 |
| 23 | `live_logical_heap_bytes` | u64 | 33554432 |
| 24 | `output_fragment_bytes` | u64 | 32768 |
| 25 | `output_queue_pending` | u32 | 32 |
| 26 | `output_payload_bytes` | u64 | 1048576 |
| 27 | `production_instruction_fuel_present` | u8 | 0 |
| 28 | `production_output_count_present` | u8 | 0 |

以下 golden vectors 以本表和上述 framing 为准，digest 使用 lowercase hex：

| Vector | 输入 | framed bytes | SHA-256 |
| --- | --- | ---: | --- |
| A | main id `main.ecs`，empty source，无 lib | 202 | `14194633f5b81dc0bfe9c674990c0126126a01c39ef3142c482bc1b178a8e10a` |
| B | main id `main.ecs`、source `EFBBBF5052494E5420224F4B220D0A`；输入 libs 为 `lib/z.ecs:5A3D310A`、`lib/a.ecs:413D320A` | 274 | `aa86e95ac3def7b3022f4a415cf2312169562f3455a291bc9c9e6b2d4ce2ee12` |
| C | main id `main.ecs`、source `EFBBBF5052494E5420224F4B220A`；输入 libs 已按 a、z 排列，内容同 B | 273 | `c192953c0745bbb04b555d8ee6e4b4657d3b014ae443cac6c68ae59af4fd1916` |

B 与以下每个 isolation case 必须各自形成独立 golden assertion，不得只由组合测试间接覆盖。除明确指出的单一
mutation 外，mutation cases 的全部输入都与 B 相同；它们只验证 framing/hash 敏感度，不把
`profile_version=2` 宣告为受支持 profile：

| Assertion | 与 B 的唯一差异 | framed bytes | SHA-256 |
| --- | --- | ---: | --- |
| BOM equivalence | main source 删除开头 `EFBBBF` | 274 | `aa86e95ac3def7b3022f4a415cf2312169562f3455a291bc9c9e6b2d4ce2ee12` |
| Lib input-order invariance | 输入 libs 从 z、a 交换为 a、z | 274 | `aa86e95ac3def7b3022f4a415cf2312169562f3455a291bc9c9e6b2d4ce2ee12` |
| Source-id sensitivity | main `source_id` 从 `main.ecs` 改为 `Main.ecs` | 274 | `9e47433943992be6596b53feb7697a8218d3bdbb680ce15bf676d052351c9760` |
| Profile-field sensitivity | 只把 `profile_version` 从 1 编码为 2 | 274 | `e7deac9052f78b742269ad3fcf8544126589a9f5f1436db055ab4e532880fbf2` |
| Source-byte sensitivity | main source 最后一个 byte 从 `0A` 改为 `0B` | 274 | `92e3250d93c87fddedaef09dad5c5df8c18cbf5fbe5554dff13f36d4e2b70642` |

因此 B without BOM 与 B with swapped input libs 都必须 byte-for-byte 形成与 B 相同的 canonical frame 和 digest；
B 与 C 必须不同。Vector A 的完整 framed bytes 为：

```text
65617379636F6E2D73646B3A6563732D70726F6772616D3A7631000000000100000001000000010000004000000000000400000000000000100000000000000000010000000000000000800000002000000020000000400000010000001000000400000002000000040000000400000004000000000040000002000000000100000080000040000000000000040000000000000200000000000000000080000000002000000000001000000000000000010000000000000000086D61696E2E6563730000000000000000
```

任何 framing、semantics 或 profile 字段调整都必须使用新 version/profile，并新增独立 golden vectors；不得静默改变 v1。

## Determinism、RAND、TIME 与 WAIT

每个 Phase 4 run 必须接收具体 `u64 seed` 并写入 start metadata；seed 不属于 ProgramHash。PRNG 固定为
PCG XSH-RR 64/32：

- multiplier `0x5851F42D4C957F2D`；
- fixed `initseq = 0x0000000000000036`，因此 odd increment
  `inc = (initseq << 1) | 1 = 0x000000000000006D`；
- run seed 是 `initstate`；所有 state arithmetic 都 modulo `2^64`；
- seeding 精确为 `state=0`、advance once、wrapping-add run seed、advance once；
- 每次输出先保存 `oldstate`，更新
  `state = oldstate * 0x5851F42D4C957F2D + inc`，再令
  `xorshifted = u32(((oldstate >> 18) ^ oldstate) >> 27)`、`rot = oldstate >> 59`，输出
  `rotr32(xorshifted, rot)`。

raw `u32` golden vectors（每行前 8 个输出）为：

| initstate | 输出 |
| --- | --- |
| `0000000000000000` | `47C28B93 B98F6A27 7D3DCB1E F0761116 9CC33F5B BE0E744D 5752C556 43369132` |
| `0000000000000001` | `9B6BDDA9 0C31FC48 CE97F8EF 822C03D4 943E8CF2 1B75DDE3 AFDE6F30 C9748B79` |
| `000000000000002A` | `A15C02B7 7B47F409 BA1D3330 83D2F293 BFA4784B CBED606E BFC6A3AD 812FFF6D` |
| `FFFFFFFFFFFFFFFF` | `11526277 E6D82672 AF1798BA D0751021 A734CDB8 AD4F9760 2D5EB982 5FE3FD7C` |

`RAND()` 等于 `RAND(100)`。当 `max <= 0` 时返回 0 且不 advance；当 `max > 0` 时把 max 转为 `u32 bound`，
令 `threshold = (2^32 - bound) mod bound`，反复取 raw sample，拒绝 `sample < threshold`，否则返回
`sample mod bound`。因此 `max=1` 必须走正值路径、消耗一个 sample 并返回 0。独立 sampler trace：seed 0 上
`RAND(0)` 后下一个 raw 仍是 `47C28B93`；seed 0 上 `RAND(1)` 消耗 `47C28B93`，下一个 raw 是
`B98F6A27`；seed `FFFFFFFFFFFFFFFF`、bound `0x40000001` 时 threshold 为 `3FFFFFFD`，先拒绝
`11526277`，再接受 `E6D82672`，结果 `26D8266F`。这禁止仅做 modulo 的有偏实现。

`TIME` 从与 `RunStarted` 相同的 Runtime monotonic clock sample 起算，返回 elapsed whole milliseconds；只取整、
不读 wall clock，超过 `i32::MAX` 时饱和为 `i32::MAX`。`WAIT()` 等于 `WAIT(50)` milliseconds；显式参数范围
`0..i32::MAX`，negative 产生 typed failure。实现用 checked nanosecond conversion 和 absolute monotonic target，
不做相对 sleep 累积；`WAIT(0)` 不推进 fake clock、不 sleep/spin，但必须执行 cancel/deadline checkpoint。所有
lowered step，以及 Controller/Vision/Output effect 的前后都检查 first cancellation/deadline；已经在线性化点完成的 effect
或 port failure 不被更晚 cancellation 覆盖。

## VisionPort 与 OutputPort

每次 label evaluation 从 `VisionPort` 取得一个 immutable snapshot。score 非 finite 时返回 typed failure；否则 clamp
到 `0.0..1.0`，计算 `floor(score * 100)`，并把精确端点固定为 0 和 100。此规则只做 ECS integer projection，
不改变 Phase 3 normalized score contract。

`OutputPort` 只接受以下三种领域项：

- `PrintFragment { text, starts_new_line }`；
- `Alert { text }`；
- `Beep { frequency_hz, duration_ms }`。

首个 PrintFragment 的 `starts_new_line=true`。PRINT 文本尾部一个反斜杠被剥离，并使下一条 PrintFragment 的
`starts_new_line=false`；没有尾反斜杠则下一条恢复为 true。Alert 不改变 PRINT continuation state。Beep 只形成 port
effect，不允许 evaluator 直接调用 UI、network 或 system beep。

OutputPort 必须 fallible、bounded、支持 backpressure，且等待可由 run cancel/deadline 取消；fragment 不超过 32 KiB，
pending queue 不超过 32，总 payload 不超过 1 MiB。它不得承载 RunStarted/terminal lifecycle、cleanup warning、
`RunFailure` 或 secondary diagnostic，避免 cleanup 失败通过正在 cleanup 的 port 递归发布。

`output_payload_bytes = 1048576` 的唯一 v1 含义是 OutputPort **当前 pending queue** 中 payload 的 aggregate
UTF-8 byte count，不是 run 从开始至今累计产生或交付的 bytes。`output_queue_pending` 同样只计当前 pending items；
一个已取得 slot、正在执行 owned copy、尚未对 consumer ready 的 reservation 也属于 pending ledger，以保证并发实现
不能越过上限。字段计费固定如下：

- `PrintFragment.text` 的 exact UTF-8 bytes 计入 aggregate，`starts_new_line` 不计 bytes；
- `Alert.text` 的 exact UTF-8 bytes 计入 aggregate；
- `Beep.frequency_hz` 与 `Beep.duration_ms` 都不计 payload bytes，但该 Beep 仍占一个 pending item；
- `output_fragment_bytes` 同时约束单个 `PrintFragment.text` 或 `Alert.text`；Beep 的 payload length 为 0。

enqueue ledger 固定为 reserve-before-copy：先 checked 计算 item bytes，并同时取得一个 pending slot 与对应 byte
reservation，再进行任何 OutputPort-owned copy，最后才把 item 标成 consumer-ready。算术 overflow、单个 text 超过
`output_fragment_bytes`，或 item 即使在空队列中也不能容纳，立即返回稳定 output limit failure，且不等待、不复制、
不产生可见 item。若 item 本身可容纳、只是当前 pending count 或 aggregate bytes 暂时没有余量，则这是 backpressure，
不是 limit failure；producer 等待 dequeue，直到 enqueue、first cancel、deadline 或 port close 的既有线性化点之一胜出。

owned copy、enqueue 或其故障注入失败时，slot 与 bytes 全部 rollback，不能留下 partial item。consumer 在 dequeue
线性化点移除 item 时立即释放其 slot/bytes，即使 consumer 随后仍持有自己的值；cleanup、run cancel 或 port close
若移除/drop queued item，也在同一移除点恰好释放一次。尚未取得 reservation 的 backpressure waiter 被取消时不释放
虚构额度；已取得但尚未 ready 的 reservation 被取消时完整 rollback。正常 delivery 不得静默 drop。因而 production
没有累计 output-count 或 output-bytes ceiling：只要 consumer 持续 dequeue，合法 run 可以无限地产生输出，最终仍由
deadline/cancel 终止。

## EcsLimitsV1 与证据门槛

上表数值是用户选择的显式、保守 product profile，不是现有 corpus 或 CI 已 evidence-backed 的 ceiling。当前 SDK
没有自有 ECS fixture；只读 legacy 的 9 个磁盘脚本最大 source/bundle 仅 3922 bytes，且现有 CI 没有固定 RAM
预算。这些观察不能证明生产上限。

所有 limits 都遵守：

- source/bundle/path/count/UTF-8 在 read、decode 和 owned copy 前 charge；per-source 与 bundle bytes 对 raw input
  计数并包含 BOM；token、node、diagnostic、string、array、heap、output 在 allocation/copy 前 charge；
- 所有累加和 `u64 -> usize` 转换 checked；overflow 与 N+1 使用稳定 limit code，拒绝路径完整 rollback reservation；
- bound nodes、lowered nodes、instructions 各自独立计数；parameters/arguments 各自独立计数；
- diagnostic 正常项最多 64/source、511 total；下一项将越界时停止该 phase，并用预留槽写一个 bundle-level
  `ECS_DIAGNOSTIC_LIMIT`，最终不超过 512，且不再产生不稳定的尾部诊断；
- single string 以 UTF-8 bytes 计，single array 以 direct cells 计；live logical heap 使用 semantics-v1 的稳定领域
  ledger 而不是 allocator RSS：每个 live logical string allocation 按 exact UTF-8 bytes、每个 live logical array
  allocation 按 `8 * direct cell length` charge，nested logical allocations 另计；
- production 不设置 total instruction fuel 或 output-count ceiling；deadline/cancel 负责终止合法无限 run，fuzz/model
  可以使用不会进入 production profile/hash 的 test-only fuel；
- profile 的全部字段进入 ProgramHash 和 run metadata；任何数值或 charging semantics 调整都必须新建 profile/version。

### v1 live logical heap ledger

array direct-cell charge 与 host representation 无关：Vec/List capacity、allocator size class、pointer width、header、refcount、
small-object optimization、persistent-tree node 和预留但未成为元素的 capacity 一律计 0。长度为 `N` 的每个 live
logical array allocation 精确 charge checked `N * 8` bytes；cell 引用的 nested string/array allocation 再按自身 ledger
计费。不同实现即使采用不同增长策略或结构共享，同一 ProgramHash/profile 也必须在相同 semantic checkpoint 得到
相同 `live_logical_heap_bytes` 与终态。

array reservation ownership 固定如下：

- array literal 创建一个长度等于元素数的新 logical allocation；assignment、parameter binding、argument passing 和
  return 只 alias/transfer 同一个不可变 logical allocation 与 reservation，不按 alias 数重复 charge；
- array slice、array concat/`+` 与 `APPEND` 都是 semantic copy，创建长度分别为 slice length、左右长度之和、原长度
  加一的新 logical allocation；即使 backing storage 被共享，也必须取得完整新 direct-cell reservation，源 allocation
  在仍 live 时继续全额计费；
- string assignment/argument/return 使用同一 alias/transfer 规则；string slice/concat 产生按结果 exact UTF-8 bytes
  计费的新 logical allocation；实现层 copy-on-write 或 interning 不得改变 ledger；
- expression temporary 从创建到被 binding/return 接管或在该 expression/statement 结束后销毁期间都算 live；binding
  replacement 必须先完成 RHS 及其全部新 reservations，再原子替换 binding，最后只在旧值最后一个 logical alias
  消失时释放旧 reservation；self-assignment 不创建 semantic copy；
- scope exit、function-frame unwind、temporary discard、binding replacement、run success/failure/cancel cleanup 都在最后
  logical alias 不再可达的点释放一次；nested alias 只延长其已有 reservation 的 lifetime，不重复 charge。

所有 literal、slice、concat、`APPEND` 和 string copy 都先 checked 计算 result length、`array_cells`/`string_bytes`、
`delta` 及 `live + delta`，在任何 allocation/copy 前原子 reserve。reservation 失败时不 allocation、不复制、不改 binding；
reservation 后的 allocation/copy/fault failure 必须撤销该 expression 新取得的全部 reservations，并保持原数组、binding、
temporaries 和 ledger 可观察状态不变。concat/`APPEND` 的 result 计算和 replacement 因此会在提交前同时计入仍 live 的
source 与新 result，这是规范峰值，不允许先释放 source 来规避上限。

`array_cells` 的 N/N+1 证据按单个结果的 direct cell length 计；`live_logical_heap_bytes` 的 N/N+1 证据按上述 canonical
ledger 在同一 semantic checkpoint 计。恰好 `limit` 可在其他条件满足时成功，`limit + 1` 必须在 allocation/copy 前以
稳定 limit code 失败并证明 rollback；测试、runner telemetry 与 ProgramHash profile 全部使用这一口径，不得用 capacity、
RSS 或 allocator-reported bytes 替代。

Phase 4 最终 source freeze 的必要证据包括：逐层 instrumentation 得到 source/token/AST/bound/lowered/instruction/
diagnostic/symbol、call/string/array/logical-heap/output maxima；每个 limit 的 N 与 N+1；charge-before-copy/allocation 与
rollback fault injection；在 pinned Required runner 上记录 dense/adversarial case 的 peak memory 和 elapsed time。只有这些
证据通过后才能称该 profile evidence-backed。若证据不支持已选数值，必须修订 profile/version、golden vectors 与 ADR，
不得静默调参或降低门禁。

## Legacy provenance 与 fixture 分类

S0 必须把最小、可审计 fixture 自包含地收进 SDK。每项 manifest 记录 provenance class、legacy
`EasyCon@11c4b992b9bce0ff977e9c587a6c0bb0d302853e`、仓库内相对 path、input/legacy-observed/v1-expected 的
SHA-256 和修订理由；CI 只读 SDK fixture，绝不读取 ignored `EasyCon/`。

三类 provenance 不得混用：

| 分类 | 冻结内容 |
| --- | --- |
| Legacy Exact | `\` 为 double quotient 后 midpoint-away-from-zero 的整数商（如 `5\2=3`、`-5\2=-3`）；`^` 为 integer XOR；`and/or` short-circuit；普通 i32 wrap/shift；IMPORT bind NOP、无 IMPORT 仍加载 bundle libs、shared lib scope、lib/main visibility 与 lib globals 先执行；可达的 array/string/control-flow 成功路径 |
| Corrected | `FOR i32::MAX` stop-after-upper；可执行 TRUE/FALSE；libs raw-UTF-8 排序；LF/CRLF/CR；typed diagnostic/failure 取代 host panic；UTF-8 byte span 与 Unicode-scalar string；PRINT continuation、label floor；effect 前后 cancellation 与五路 cleanup settled-before-terminal |
| v1-native | SourceBundle/restricted loader、BOM 与 ProgramHash、PCG/replay、monotonic TIME/absolute WAIT、EcsLimits、immutable Program/RunCompletion、typed ports/RecordingPorts、generic error projection、Runtime five-way terminal race |

Corrected provenance 不能只引用 evaluator test mock。S0 必须为下表每一行创建独立、SDK-local、自包含的 manifest
record 与 input/legacy-observed/v1-expected artifacts；相同 observed value 也不能合并 production oracle。Q0 必须逐行建立
v1 executable assertion。这里仅冻结未来 fixture 合同，不在 G0b proposal/rework 中创建 fixture：

| Fixture obligation | Oracle kind | `EasyCon@11c4b992b9bce0ff977e9c587a6c0bb0d302853e` legacy path/symbol | 必须保留的冲突与 v1 expected |
| --- | --- | --- | --- |
| `corrected.print.cli` | Production CLI | `src/EasyCon.Script/Binding/BuiltinCallable.cs::ImplPrint`；`src/EasyCon2.CLI/ConsoleOutAdapter.cs::Print` | legacy 把 bool 当作写入前换行并加入 timestamp/ANSI；保留其 token trace，v1 为下述四个 `PrintFragment` |
| `corrected.print.winforms` | Production WinForms | `src/EasyCon.Script/Binding/BuiltinCallable.cs::ImplPrint`；`src/EasyCon2/App/EasyConForm.cs::Print`；`src/EasyCon2/Controls/RichLogBox.cs::Print` | legacy 把 bool 当作写入前 `Environment.NewLine` 并加入 timestamp；保留 UI queue token trace，v1 为同一四个 fragments |
| `corrected.print.avalonia` | Production Avalonia | `src/EasyCon.Script/Binding/BuiltinCallable.cs::ImplPrint`；`src/EasyCon2.Avalonia/Services/LogService.cs::Print`；`src/EasyCon2.Avalonia.Core/Services/LogService.cs::Print` | legacy 把 bool 当作 payload 后 LF 并在 true 分支加入 timestamp；两个 production implementation 都须保留，v1 为同一四个 fragments |
| `corrected.print.test-mock` | Test oracle，非 production | `test/EasyCon.Tests/EvaluatorTests.cs::MockOutputAdapter.Print` | 单独记录 legacy mock 的 trailing-LF list；不得用它替代或证明任一 production row，v1 expected 仍是同一 fragments |
| `corrected.label.cli` | Production CLI | `src/EasyCon.Capture/ImgLabel.cs::Search`；`src/EasyCon2.CLI/Program.cs` external getter | legacy `md *= 100` 后 `(int)md` truncation；canonical normalized score `0.4225` observed 42，v1 `floor(clamp(score,0,1)*100)` 为 42 |
| `corrected.label.avalonia` | Production Avalonia | `src/EasyCon.Capture/ImgLabel.cs::Search`；`src/EasyCon2.Avalonia/Services/ScriptService.cs` external getter | legacy `md *= 100` 后 `(int)md` truncation；同一 canonical input observed 42，v1 expected 42；仍与 CLI 分开留证 |
| `corrected.label.winforms` | Production WinForms | `src/EasyCon.Capture/ImgLabel.cs::Search`；`src/EasyCon2/Services/CaptureService.cs::BuildExternalGetters` | legacy `md *= 100` 后 `Math.Ceiling(md)` observed 43；同一 canonical input 的 v1 expected 为 42 |

PRINT canonical probe 是 UTF-8、无 BOM、LF 分隔且末尾有 LF 的以下 exact source；legacy capture 必须保留四次 adapter
调用及 frontend rendering token，不得通过删除 timestamp/newline 差异把三个 production oracle 归一成同一 observed：

```ecs
PRINT "A\"
PRINT "B\"
PRINT "C"
PRINT "D"
```

v1 expected 是 exact ordered trace
`[("A", true), ("B", false), ("C", false), ("D", true)]`，其中 tuple 对应
`PrintFragment { text, starts_new_line }`，不含 timestamp、ANSI 或 host newline。动态 timestamp 必须在 legacy observed
artifact 中编码为明确的 `Timestamp` token；ANSI、LF、`Environment.NewLine`、payload 与 UI enqueue 必须使用有类型 token，
normalization schema/version 写入 manifest，不能把 token 丢掉后只 hash 拼接文本。

每个 record 至少固定：`provenance_class=Corrected`、稳定 fixture ID、`oracle_kind`、完整 legacy commit、上述每个
legacy repo-relative path 与 symbol、SDK-local exact input path + lowercase SHA-256、SDK-local legacy observed path +
lowercase SHA-256、SDK-local v1 expected path + lowercase SHA-256、capture/normalization schema version，以及逐项
`revision_reason`。label record 的 input 必须明确区分 normalized `0.0..1.0` score 与 legacy `md *= 100` 后的值；PRINT
record 的 observed hash 必须覆盖 ordered typed tokens。S0 的 static validator 验证 schema、路径、hash、obligation 完整性、
duplicate/unknown-field/tamper fail-closed；Q0 只从 SDK-local artifacts 建立 executable assertions。两者都不得读取、构建或
运行 ignored `EasyCon/`，legacy commit/path 只是不可变 provenance metadata。

Exact fixture 同时保存 legacy observed 与 v1 expected；Corrected fixture 同时保存旧观察、v1 expected 和修订理由；
v1-native 只验证本 ADR 合同，不伪称 differential legacy evidence。Python/Lua/FFI、bytecode/firmware、selective
IMPORT/AS、FOR STEP 语义、float/hex/exponent、STRUCT/FOR-IN、dynamic label、`.ILX`、UI/network/editor 不在 v1。

## Safe DAG 与节点验收

治理与实现 DAG 固定为：

```text
G0b fixed-SHA proposal (含 REWORK 修订)
  -> independent design review，P0/P1/P2 清零
  -> separate G0b target acceptance/freeze
       -> R0: ADR-0007 narrow reopen -> RED/models -> minimal terminal API
              -> Phase 1 full gates -> independent review/refreeze
       -> W0: one compilable easycon-ecs workspace-registration node
       -> S0: static ECS schemas/provenance/hash/classification validator

W0 + S0 -> C1 source/bundle/loader/hash/lexer/diagnostics
C1 -> C2 parser/recovery
C2 -> C3 binder/lowerer/immutable Program/logical dependencies
C3 -> E1 evaluator/PCG/clock/limits/ports/RecordingPorts
E1 + completed R0 -> E2 AutomationRun/RunCompletion/generic projection/five-way races
E2 -> Q0 complete mapping/golden/replay/fuzz/limits/coverage/full gates
Q0 -> Q1 fixed-SHA independent implementation review -> separate Phase 4 source freeze

separate G0b target acceptance/freeze
  -> D0 ADR-0009 narrow reopen
  -> D1 production Controller acquire/neutralize-release completion
  -> D2 full gates + independent review/refreeze

Phase 5 concrete adapters require Phase 4 Q1 + D2.
Phase 4 Q1 does not require D2.
```

R0、W0、S0 只有在 G0b target acceptance 后才可并行。W0 必须在同一可编译提交中创建最小
`crates/easycon-ecs`，同步 root `Cargo.toml`、`Cargo.lock` 和 repository guard：从 forbidden 移除
`easycon-ecs`，加入 expected member，固定 internal dependency whitelist；`easycon-sdk`/`easycon-capi` 继续 forbidden。
root Cargo/lock/guard 是 W0 的同一 single owner，不能拆出一个 guard 失败或无法 workspace build 的中间节点。

S0 只映射静态 schema、provenance/hash、三类 classification、tamper/duplicate/unknown-field；不得预先引用尚不存在的
Rust tests。C1/C2/C3/E1/E2 各自在同一节点加入实现、test、conformance assertion、source marker 与 incremental mapping；
Q0 才要求 target requirements 与 executable assertions 完整双向覆盖。E2 依赖完成并重新冻结的 R0，但 Phase 4
任一节点都不依赖 D0-D2。每个节点必须是可独立审查、可编译、可测试的语义提交，不把 proposal、实现、review 与
freeze 混成一个提交。

## 门禁分层

以下是当前 baseline 已存在且可执行的 repository gates；它们不等于 Phase 4 专项证据：

| 类型 | 当前命令/检查 |
| --- | --- |
| Rust workspace | `cargo fmt --all --check`；`cargo check --workspace --all-targets`；`cargo clippy --workspace --all-targets --all-features -- -D warnings`；`cargo test --workspace --all-features` |
| Runtime/spec | `python tools/run_runtime_models.py`；`python tools/validate_specs.py` |
| Repository/docs | `python tools/check_markdown_links.py`；`python tools/check_repository_guards.py`；`git diff --check` |
| Required CI | 现有 Required / Policy 与 Required / Windows Workspace；它们目前没有 ECS coverage 或 Rust fuzz job |

以下能力当前不存在；只有相应 pinned tool/script/job 落盘、可复现并通过后，才能在 Q0/Q1 称为 Phase 4 gate：

| Phase 4 必增门禁 | 最低合同 |
| --- | --- |
| Static fixture validator | self-contained manifest、provenance/hash、classification、tamper/duplicate/unknown-field fail closed |
| Incremental mapping | C1-C3/E1/E2 每节点 exactly-one passing assertion、zero ignored；Q0 完整双向覆盖 |
| Canonical golden/replay | ProgramHash 与 PCG hex vectors、source permutation、same Program/seed/fake-clock trace replay |
| Deterministic fuzz | pinned parser/evaluator seed corpus、固定资源/时间上界、never panic；失败 seed 可直接 replay |
| Limits/runner evidence | 所有 N/N+1、charge-before-copy/allocation、rollback、diagnostic saturation、dense peak memory/time |
| Coverage | pinned coverage tool/version/exclusion，业务 crate line `>=85%`、branch `>=80%`；关键 terminal/limit state 语义分支全覆盖 |

以下是建议的模型/性质测试，不得在其 executable harness 落盘前称为已有 gate：terminal permit 与 cancel/deadline/close
竞争、child admission seal、cleanup Err/panic、terminal/event/unlink/notify 次序；port effect-vs-cancel、backpressure wake、
grant-vs-cancel 与 stale release；source permutation/hash、scope alpha-renaming、小循环 reference model；parser/evaluator
never-panic 与 same-seed/time replay；logical dependency 两种注册顺序与最终 operation/resource/task/lease count 收敛。

Q0/Q1 运行当前完整 repository gates 和全部已落盘 Phase 4 gates。若实际修改 Phase 1/2A/3 冻结面，还必须运行对应
ADR 的完整门禁并接受新的独立 review；纯 Phase 4 docs/code 不把 hardware/COM 或尚不存在的 coverage/fuzz 命令伪称为
现有证据。

## Proposal chain 的验证边界

初始 proposal `6468da5` 只新增本 ADR 并更新两处索引；其提交前实测结果为 Markdown links Passed（239
references / 44 files）、repository guards Passed、working/new-file diff checks Passed。本次 REWORK 仍只允许修改本
ADR 与同两处索引，用于关闭固定 review 的五项 finding；不修改 code、Cargo、workflow、Ruleset、fixture、support
matrix 或 architecture/source map，不运行 hardware/COM，也不执行 R0/W0/S0/D0。

REWORK 修订只运行 AGENTS.md 要求的 docs-only links、repository guards 与 diff checks，精确结果由修订后的 Git
对象和外部结构化 `TASK_REPORT` 固定。proposal chain 没有运行 Rust、Loom、spec、native、coverage 或 fuzz，不能把
未来 gate 描述成当前已通过，也不能把 docs-only 通过描述为 design acceptance。

## 关联

- [ADR-0006：Runtime 所有权、终态事务与确定性关闭](0006-runtime-stabilization.md)
- [ADR-0007：Phase 1 Runtime 基线](0007-phase-1-freeze.md)
- [ADR-0009：Phase 2A Controller/Serial Candidate](0009-phase-2a-freeze.md)
- [ADR-0014：Phase 3 NativePool admission 修复后重新冻结](0014-phase-3-native-pool-admission-refreeze.md)
- [ADR-0016：Phase 3 下游阶段重新打开边界](0016-phase-3-downstream-reopen-boundary.md)
- [架构总览](../architecture/architecture-overview.md)
- [Runtime 生命周期](../architecture/runtime-lifecycle.md)
- [测试策略](../architecture/testing-strategy.md)
- [实施路线](../architecture/repository-roadmap.md)
- [源码能力映射](../architecture/source-capability-map.md)
