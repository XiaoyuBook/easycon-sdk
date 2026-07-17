# 0005：Windows x64 首发与同源原生包

- 状态：Accepted
- 日期：2026-07-17

## 背景

EasyCon 源码中出现 Windows、Linux 和 macOS 的 OpenCV runtime 线索，但实际 Controller/capture 生命周期、系统枚举和硬件支持没有形成跨平台验收矩阵。首批还需要同时交付 C++、.NET、Python 和 Node.js/TypeScript 包。

如果 v1 同时承诺多个平台，会在核心语义尚未固定前放大 serial/capture/backend、原生依赖和 package 组合；如果四种包各自构建 native core，又无法证明它们运行的是同一实现。

## 决策

1. v1.0 GA 只支持 Windows 10/11 x64，Rust target 为 `x86_64-pc-windows-msvc`。
2. Linux x64 和 macOS arm64 保留架构扩展点，但不进入 v1.0 支持声明。
3. 每个 version + target 只构建一次 canonical native bundle。
4. CMake ZIP、NuGet、PyPI wheel 和 npm platform package 只重新包装该 bundle，不重新编译核心。
5. 所有包验证相同 build ID、manifest hash 和 ABI。
6. 原生依赖默认静态并入一个 public `easycon_core.dll`；无法静态链接的 private DLL 必须同目录、入 manifest/SBOM 并安全加载。
7. Node.js/TypeScript 只支持 Node，不发布浏览器入口。

## 理由

- Windows 是当前串口与采集实现证据最完整的平台，可先闭合硬件与四语言质量门槛。
- target-specific 代码位于叶子层，后续增加平台不需要改 C ABI 或核心状态机。
- canonical bundle 避免 registry 包中的 native feature/version 漂移。
- package-private loader 避免 PATH、当前目录和 DLL 搜索劫持。

## 结果

- ARM64/x86 process 初始化明确失败，不能通过 AnyCPU 或纯 Python 伪装支持。
- 平台扩展需要 serial/capture、原生依赖、四语言、硬件、关闭和 conformance 全套门槛。
- release pipeline 必须先产 native bundle，再并行包装四语言。
- OCR 模型不会从本地 EasyCon 快照或网络隐式取得；来源/许可证核验是发布门槛。

## 被否决的方案

- v1.0 同时承诺三个桌面平台：现有源码事实不足以支持。
- 每语言自带独立 native build：无法保证共享核心同源。
- 依赖系统 PATH 中的 OpenCV/Tesseract：不可重复且有安全风险。
- Node 浏览器兼容层：Controller/capture/native ABI 无法在浏览器中实现同一语义。

## 关联

- [构建、发布与合规](../architecture/build-release.md)
- [测试策略](../architecture/testing-strategy.md)
- [目标仓库与实施路线](../architecture/repository-roadmap.md)
