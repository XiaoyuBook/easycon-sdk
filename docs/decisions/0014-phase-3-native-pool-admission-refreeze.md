# 0014：Phase 3 NativePool admission 修复后重新冻结

- 状态：Refrozen Source Candidate（分平台分级）
- 日期：2026-07-22
- 集成基线：`main@41c5f0c2b19165769d4aa8e4512ad46280a1415b`
- 历史冻结候选：`76436de98836dc2fff9a43c4560e7dc7f3fea780`（不可合入）
- 修复 implementation：`27444f16d0625a7ab7e4e1543736c7c3c225ce8a`
- implementation tree：`334a3fb21bf4d6ca55e0bcbe3e2daa6e675b3194`
- implementation parent：`76436de98836dc2fff9a43c4560e7dc7f3fea780`
- 完整实现审查范围：`41c5f0c2b19165769d4aa8e4512ad46280a1415b..27444f16d0625a7ab7e4e1543736c7c3c225ce8a`
- 上位目标：[ADR-0012](0012-phase-3-vision-native-target.md)
- 历史冻结：[ADR-0013](0013-phase-3-vision-cross-platform-source-candidate-freeze.md)

## 背景与取代关系

ADR-0013 和候选 `76436de98836dc2fff9a43c4560e7dc7f3fea780` 保留为第一次 Phase 3 冻结的
历史审计记录。后续独立 review 发现一个阻断合入的 P2：`NativePool::decode` 在检查调用前 cancellation、
pool lifecycle、queue capacity 和 encoded-byte limit 之前，就把完整 encoded slice 复制为 owned input。
因此本应稳定拒绝的 oversized、pre-cancelled、closed 或 full-queue 请求仍会先分配并复制大输入，极端情况下
可能在返回既有错误前耗尽内存。该 finding 重新打开 Phase 3，旧候选及其旧 bundle/handoff 不再具备合入资格。

修复提交 `27444f16d0625a7ab7e4e1543736c7c3c225ce8a` 是旧候选的单一直接后代，tree 为
`334a3fb21bf4d6ca55e0bcbe3e2daa6e675b3194`。本 ADR 在不改写 ADR-0013 原文、SHA 或当时证据的前提下，
取代 ADR-0013 对候选“可合入”的结论；ADR-0013 仍用于解释旧冻结为何存在及为何失效。

## 修复与 RED 到 GREEN 证据

四条确定性回归直接观察 owned-copy builder 和 native decode 边界：

- `oversized_decode_rejects_before_input_owned_copy_or_native_call`；
- `pre_cancelled_decode_rejects_before_input_owned_copy_or_native_call`；
- `closed_pool_decode_rejects_before_input_owned_copy_or_native_call`；
- `full_queue_decode_rejects_before_input_owned_copy_or_native_call`。

在未修改的旧树上，每条路径都先得到既有稳定错误、零次 native decode 和收敛的 pool/Runtime/native 计数，
随后只因观测到一次等长、内容相同但存储独立的 owned copy 而 RED。修复树上四条均 GREEN。全新 reviewer 还以
仓库外 allocator harness 独立对照 pre-cancelled 公共调用：旧树观测复制计数 1，新树为 0，错误与 pool 计数一致；
这排除了仅由测试 hook 自证或编译器优化造成的假阳性。

修复后的 admission 顺序固定为 cancellation、pool lifecycle、queue capacity、FIFO ticket/slot reservation，随后
才在 state mutex 外执行 encoded-size 零复制 preflight、owned job 构造和 commit。后续 admission 不能越过尚未
提交的 reservation。`close` 先进入 Closing 并拒绝新 reservation，等待已经线性化的 reservation 提交或回滚，
再取消 queued job、等待 in-flight 归零、join worker 并完成 Runtime registry 收敛。

reservation Drop 只幂等回滚容量并通知 waiter；builder panic、panic payload、拒绝路径 capture 和其他可能
可重入的析构均在 state mutex 外发生。定向测试覆盖 reservation FIFO、等待中的 cancellation wake、close handoff、
builder panic rollback、queued/in-flight cancellation 和可重入 rejected-capture Drop。其他 Image 输入仍只增加
`Arc` 引用，没有扩大修复范围，也没有改变 4096 job、64 MiB 单项 ceiling 或公共错误/API。

## 独立复审与软件门禁

全新只读 reviewer 在任务 `019f8905-33c7-7de2-bbf3-c48d22b25674` 中固定审查 implementation、tree、parent、
main merge-base 和 `main..implementation` 完整差异。reviewer 独立复现上述 RED 到 GREEN，审查 reservation、
cancellation、close、panic/Drop、FIFO、capacity、错误优先级、private ABI、平台边界和依赖方向，结论为：
当前证据下未发现未解决、可复现且可行动的 in-scope finding；该 implementation 有资格进入重新冻结步骤。
这是一轮 production code 复审，不由本次后续文档审查替代或重复声明。

reviewer 实际运行并通过：

| 门禁 | 结果 |
| --- | --- |
| 四条 decode 拒绝回归与 `pool::tests` | Passed，4/4；pool 16/16 |
| `easycon-vision` / `easycon-native-sys` / compile-fail doctest | Passed，71 / 11 / 2 |
| Runtime Loom models | Passed，6/6 |
| 根 Rust 与仓库门禁 | `fmt`、workspace check、strict clippy、workspace all-features test、spec、links、guards、diff check 全部 Passed |
| MSVC Debug / Release configure、build、CTest | Passed，各 2/2 |
| clang-cl ASan / UBSan trap configure、build、CTest | Passed，各 2/2，无 sanitizer violation |
| MSVC `/analyze /analyze:external- /WX` | Passed，build 与 CTest 2/2 |
| clang-tidy warnings-as-errors | Passed，10 个 owned translation units，零 finding |
| clang-cl libFuzzer tracked corpus | Passed，128 runs |

修复任务 `019f88ce-9a62-72a0-ba04-c2941e41ed16` 的 RED、实现与完整门禁记录，以及上述全新 reviewer 的
独立复跑，共同构成本次重新冻结证据。Windows 软件证据不能替代硬件、Linux 或 macOS 证据。

## 重新冻结决定与平台状态

将 `27444f16d0625a7ab7e4e1543736c7c3c225ce8a`、tree
`334a3fb21bf4d6ca55e0bcbe3e2daa6e675b3194` 重新冻结为 Phase 3 Vision 跨平台私有 native bridge 的
executable source implementation。包含本 ADR 与状态同步的最终 docs freeze commit 在提交后才确定，由本任务
外部 `FINAL-HANDOFF.md` 和 Git bundle 固定，不在 tracked 文档中递归自指。

| 平台 | 产品方向 | 软件/build 状态 | 硬件状态 | 发布状态 |
| --- | --- | --- | --- | --- |
| Windows 10/11 x64 | v1 Tier 1 | Phase 3 Vision software/native `Passed` | `Hardware Unverified` | 不发布；尚无 capture hardware 支持行 |
| Linux x64 | v1 正式目标方向 | Vision `Candidate / Build Unverified` | `Hardware Unverified` | 不发布；不是完整 Linux SDK |
| macOS Apple Silicon arm64 | experimental source | `Experimental Source Candidate / Build Unverified` | `Hardware Unverified` | `Not Shipped`；无 Intel/universal 声明 |

本 ADR 只重新冻结 Phase 3 源码候选，不冻结 public C ABI、public header/symbol/layout、Phase 4 ECS/Automation、
Phase 5/6、Controller serial、四语言 binding、package、发布或完整跨平台 SDK。Windows capture card、camera、
profile/FPS/hot-plug/close SLO、Linux native build/V4L2 hardware、macOS AVFoundation 和所有实体硬件仍未验证。
`EasyCon/` 继续只读且 ignored，不是 tracked source、fixture、build、bundle 或发布输入。

ADR-0013 的旧 handoff/bundle 固定的是已拒绝的 `76436de`，现已过时，不得用其 hash 或验证说明交付新候选。
本次最终交付必须由包含 ADR-0014 的 docs freeze commit 新建 bundle，并同时保留
`main@41c5f0c2b19165769d4aa8e4512ad46280a1415b` ref；外部 SHA-256 清单必须覆盖 bundle、最终 handoff 和
tracked 跨平台验证 handoff 的精确副本。

## 重新打开规则

以下任一变化必须重新打开本候选，运行受影响与完整门禁并由新的独立 reviewer 审查新 SHA：

- 修改 Vision/native production source、Cargo/CMake/vcpkg、private C layout、fixture/spec 或 executable contract；
- 改变 NativePool reservation、FIFO/capacity、cancel、panic/Drop、close/join 或 encoded/image ceiling；
- 改变 Capture 状态、latest slot、deadline、worker ownership、interrupt/handoff/finalize 或 File prevalidation；
- 新增或晋级平台 backend，改变 Linux/macOS fail-closed 行为，或将任一 Unverified 状态晋级；
- 更换 Rust、CMake、vcpkg registry/triplet、OpenCV/Tesseract/Leptonica 或质量门禁；
- 增加 public ABI、Phase 4/5/6、binding、package、support row 或发布声明；
- 出现新的、可复现且可行动的 in-scope P0/P1/P2 finding。

纯说明修正仍须保持 implementation SHA/tree、审查证据和平台分级准确，并通过文档与仓库门禁。Linux/macOS
后续证据只能晋级对应平台和门禁，不能自动升级完整 SDK 或其他平台。

## 关联

- [ADR-0012：Phase 3 Vision 与跨平台私有 native bridge 开发目标](0012-phase-3-vision-native-target.md)
- [ADR-0013：第一次 Phase 3 Vision 跨平台源码候选冻结](0013-phase-3-vision-cross-platform-source-candidate-freeze.md)
- [Phase 3 Vision native 设计](../development/phase3-vision-native-design.md)
- [Phase 3 Vision implementation log](../development/phase3-vision-implementation-log.md)
- [Phase 3 Linux/macOS 验证 handoff](../development/phase3-cross-platform-validation-handoff.md)
