# 0009：冻结 Phase 2A Controller/Serial Candidate 基线

- 状态：Frozen (`Hardware Unverified`)
- 日期：2026-07-20
- 目标基线：`7bfe2fe0411c6d467c9e953410469a35d8f59de2`
- 初始审查基线：`63157b86c6d77d556175984f2374ea359a0ff379`
- 实现冻结基线：`2e9743c5d4b205c7ecbd082f4265199ce1a6cc30`
- 审查范围：`7bfe2fe0411c6d467c9e953410469a35d8f59de2..2e9743c5d4b205c7ecbd082f4265199ce1a6cc30`

## 决策

Phase 2A Controller/Serial Candidate 冻结在上述完整实现 SHA。该基线满足
[ADR-0008](0008-phase-2-controller-target.md) 的无硬件退出门槛；在当前证据下，没有未解决、可复现且
可行动的 in-scope P0/P1/P2 finding。

本决定只冻结 Phase 2A。状态仍为 `Hardware Unverified`，不冻结完整 Phase 2，不关闭 O-01、O-02、O-04，
也不声明支持任何具体控制板、固件、VID/PID、baud、Amiibo 容量、连续报告节拍或 UART/USB/Switch 延迟。
包含本 ADR 的后续文档提交不改变或冒充上述实现冻结基线。

## 冻结范围

冻结内容包括：

1. `easycon-controller` 的单写者 lane、desired report、direct/reset、precise sequence、Automation lease、
   generation-aware ACK、Amiibo save/select、取消、中立化和确定性关闭行为。
2. `easycon-serial` 的 Windows 10/11 x64 SetupAPI discovery、稳定 identity、Win32 overlapped byte I/O、
   `SerialControllerTransport`、错误归一化、partial I/O、deadline、取消和关闭结算。
3. test-only byte I/O、CH32 模拟器、10,000-step VirtualClock 验收和 release 软件延迟 harness。
4. 对应 behavior、schema、fixture、conformance、测试策略和实现说明。

依赖方向保持 `easycon-serial -> easycon-controller -> easycon-runtime/easycon-model`；serial 只为共享时钟和
取消直接依赖 Runtime。Controller、Runtime 和 Model 没有 Win32 或 serial 反向依赖。Host、JSON-RPC、
WebSocket、服务进程、UI、Vision、ECS、C ABI、语言 binding、固件和发布包装均不在本冻结范围。

## 独立审查与修复

审查先把 `63157b8` 固定为只读候选，逐提交检查 `7bfe2fe..63157b8` 的 14 个提交及其组合 diff；每次修复后
都把新 HEAD 作为新基线复查相邻调用者、规范和回归面。最终范围共有 21 个提交，全部通过
`git show --check`。

| ID | 严重性 | 旧实现确定性复现与根因 | 回归测试 | 修复提交 |
| --- | --- | --- | --- | --- |
| F-01 | P2 | Windows discovery 把 `XVID_1234`、`XPID_5678` 等非独立字段当成 VID/PID，并用 lossy UTF-16 暴露畸形属性；根因是无 component 边界扫描和宽松解码。旧实现 2 条回归失败。 | `usb_ids_come_only_from_explicit_hardware_id_components`、`malformed_utf16_properties_are_not_exposed_as_system_values` | `315ba3738eeeeb93eeb98ec2ce8641459a0b328b` |
| F-02 | P1 | serial neutral report 首字节前发生 `Io` 时，operation 已终态而 stream close 尚被 barrier 阻塞；根因是 adapter 只在已接受 prefix 后关闭。 | `failed_cancel_neutral_closes_the_serial_stream_before_terminal` | `c660ffcbe4caa4fe6cb0bb7713fcdcccc2177282` |
| F-03 | P1 | resource close 唤醒 blocked ACK 后，operation 在 stream close 前暴露为 `Cancelled`；相邻 Amiibo 原始协议错误分支在 reset cleanup 被 close 取消时又绕过延后点。两条旧实现回归都观察到提前终态。 | `blocked_ack_obeys_deadline_and_controller_close_wakes_a_second_wait`、`resource_close_during_failed_save_cleanup_settles_stream_before_terminal` | `1d7d52d88ea24bae9372466f5caec89afb3bd6d9`、`6a262146203e07b058dd730b957f739f9a180dec` |
| F-04 | P2 | discovery 接受 65 字符的 `COM...` 注册表值，但同一 production open 边界立即以 `InvalidPort` 拒绝；根因是枚举与打开各自维护不一致的 port-name 条件。 | `only_system_com_port_values_are_openable_serial_names` | `99844d892e10365ef25e337092790d8d7e354582` |
| F-05 | P1 | `ERROR_DEVICE_REMOVED`、`ERROR_DEV_NOT_EXIST`、`ERROR_NO_SUCH_DEVICE` 和 `ERROR_BROKEN_PIPE` 被归为可恢复 `Io`，ACK 失败后可能保留虚假的 Connected 状态；根因是 Win32 明确移除码集合不完整。 | `native_open_and_disconnect_codes_have_stable_categories`，并复用端到端 hot-unplug 回归 | `07fd6a18c7a24896d29a80c1835e3c7ae6856d6f` |

性能证据在 `2e9743c5d4b205c7ecbd082f4265199ce1a6cc30` 提交。没有通过修改 ADR-0008、放宽期望、
随机 sleep、过滤 outlier 或忙等来关闭 finding。

## 验证证据

实现冻结基线按顺序通过：

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

- `cargo test`：157 个非文档测试通过，0 failed、0 ignored。构成为 Controller 5、Model 5、Runtime 68、
  Loom target 6、serial lib 13、byte-I/O contract 3、latency binary 1、test-support integration 56。
- 独立 Loom：6/6，通过 parent/child admission、terminal unlink/wake、hook panic/capture drop、supervised task
  panic durability 和 self-close 模型。
- 规范映射：5 schemas、1 behavior、3 controller fixtures、9 scenarios、65 个 exact Rust tests。
- 10,000-step：实际创建 10,000 个 step，按 5,000 个同 offset group 生成 5,000 份 report；逐个绝对 target
  验证无丢失、乱序、早发或累计漂移，并在 close 后验证 operation/resource/task registry 收敛。
- Markdown：79 个引用、21 个文件；repository guard、最终 `git diff --check` 和 21/21 commit check 均通过。

## 软件延迟证据

release harness 的测量源为 `07fd6a18c7a24896d29a80c1835e3c7ae6856d6f`。最终实现冻结基线
`2e9743c5d4b205c7ecbd082f4265199ce1a6cc30` 相对该 SHA 只更新 latency fixture 与实现说明；Cargo、production、
test-support source 和 harness 均无差异，因此测量对应同一最终可执行树。

- 机器：`DESKTOP-IQM6HN5`，Intel Core i7-11800H，16,964,685,824 bytes RAM，x86_64。
- 系统：Microsoft Windows 11 家庭版 中文版 25H2，build `10.0.26200.8875`。
- 电源与工具链：`GamePP 电源方案`；Rust/Cargo 1.97.1；release MSVC build。
- 方法：1,000 warmup，10,000/10,000 eligible measured samples，1 ns measurement-only interval，
  non-blocking memory transport；真实 OS worker/channel lane 与 `SystemClock`，无 VirtualClock、同线程直调、
  预置 timestamp、样本过滤或永久 busy wait。
- 完整性：CSV 10,001 行，10,000 个唯一 operation，write sequence `1001..11000`；阶段单调、相邻 measured
  eligibility、sequence 和派生列违规均为 0。分位数按 nearest-rank 从 CSV 独立重算。
- 原始文件：`target\phase2a-controller-latency-review-final-07fd6a1-2026-07-20.csv`；SHA-256
  `33F63849AC51D718118AD54388CA1D7C4D8607D2A05E86713D8A4676C6013F00`；全部 outlier 保留。

| 单调时间段 | p50 | p95 | p99 | max |
| --- | ---: | ---: | ---: | ---: |
| command admitted -> lane wake | 700 ns | 2,000 ns | 13,000 ns | 91,800 ns |
| lane wake -> dispatch | 700 ns | 1,000 ns | 1,600 ns | 99,200 ns |
| dispatch -> transport write entered | 300 ns | 400 ns | 500 ns | 24,200 ns |
| transport write entered -> acceptance | 100 ns | 100 ns | 200 ns | 1,300 ns |
| command admitted -> transport write entered | 1,700 ns | 3,100 ns | 15,400 ns | 99,900 ns |
| command admitted -> transport acceptance | 1,800 ns | 3,200 ns | 15,500 ns | 99,900 ns |

ADR-0008 的主指标 p99 不超过 1,000,000 ns、max 不超过 5,000,000 ns，本次通过。该结果只测进程内
Controller 软件路径到 memory transport，不能外推为 UART、CH32、USB HID、Switch 总线、固件或画面延迟。

## 冻结边界证据

- Phase 1 的 `easycon-model` 和 `easycon-runtime` tree 与 ADR-0007 实现基线 `4261925` 字节相同；ADR-0007
  未改变。Phase 1 operation/runtime/event behavior 与 schema、原 6 个 conformance scenario、Controller
  原 report vectors、corrected behaviors 和 sequence traces 均结构相等，Phase 1 未重新打开。
- 只读 `EasyCon/` 快照为 1,223 文件、98,311,849 bytes；排序 manifest SHA-256 为
  `2C417B3BC7690033A930519BE0A647D11B0A03185D32CD2AC961E763CA8763CC`，前后无差异，tracked 数为 0。
- 最终组合 diff 有 44 个文件，禁止范围路径为 0；`easycon-test-support` 为 `publish = false`，production
  crate 不依赖 test fake。

## Phase 2B 开放项

- O-01：真实控制板、固件、VID/PID、稳定 serial identity、115200/9600 和热拔插支持矩阵。
- O-02：真实 Amiibo slot 数、最大长度、分包恢复和取消行为。
- O-04：UART 收齐、CH32 firmware、USB HID/Switch 总线时序、物理中立化和连续节拍。

这些硬件未知属于 Phase 2B，不是 Phase 2A 的伪失败。没有物理 CH32 参与本冻结；完整 Phase 2 仍未完成。

## 重新打开规则

以下任一情况重新打开 Phase 2A 实现基线，直到新的回归、完整门禁和独立 review 通过并由后续 ADR 推进 SHA：

- 修改 production serial、Controller、Amiibo、单写者/lease/中立化/关闭语义或依赖方向；
- 修改相关 behavior、schema、fixture、conformance、CH32 fake、10,000-step 或 latency harness/测量边界；
- 出现新的、可复现且可行动的 in-scope P0/P1/P2 finding；
- Phase 2B 发现当前软件声明不实或必须改变 Phase 2A 契约。

只补充不改变可执行行为的说明，或记录不改变 Phase 2A 契约的 Phase 2B 硬件结果，不自动重新打开本基线。

## 关联

- [ADR-0007：冻结 Phase 1 Runtime 基线](0007-phase-1-freeze.md)
- [ADR-0008：冻结 Phase 2 Controller/Serial 开发目标](0008-phase-2-controller-target.md)
- [实施路线](../architecture/repository-roadmap.md)
- [测试策略](../architecture/testing-strategy.md)
- [Phase 2A 实现与验证说明](../development/runtime-controller-vertical-slice.md)
