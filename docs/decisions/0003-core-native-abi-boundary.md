# 0003：Rust 核心、私有 C++ 桥接与公共 C ABI

- 状态：Accepted
- 日期：2026-07-17

## 背景

EasyCon 的 Vision 能力依赖 OpenCV、Tesseract 和视频采集。直接在四语言中分别绑定这些库会泄漏第三方类型、复制资源管理并造成包不一致；把全部业务交给 C++ 又会形成第二套状态和错误模型。

公共边界还必须能跨编译器和语言长期演进，Rust ABI、C++ class ABI 和 STL 都不满足这一条件。

## 决策

1. Rust 拥有所有 public domain state、operation、事件、调度和资源树。
2. C++ bridge 只承载 OpenCV、Tesseract、capture 和经批准的窄系统接口。
3. C++ bridge 以私有 C-compatible `noexcept` 接口链接进 `easycon_core.dll`，不单独发布公共库或头。
4. 唯一公共 native 边界是 `easycon_v1_*` C ABI。
5. 只有 `easycon-capi` 导出 public symbol；语言绑定不能调用 bridge 或 Rust 内部符号。
6. 原生依赖对象通过私有 handle 和 Rust RAII wrapper 管理，allocator 不跨边界配对。

## 依赖约束

- ECS 不依赖 Controller/Vision concrete implementation，只使用内部 ports。
- Controller 不依赖系统串口 concrete implementation。
- 只有 Vision 通过 `easycon-native-sys` 依赖 C++ bridge。
- C++ 不实现 `.IL` 业务格式、ECS、Controller、retry、timeout 或 event queue。
- bridge 不反向调用用户代码，也不自行拥有长期后台任务。

## 理由

- 把第三方 native API 收在最窄边界，Rust 能统一所有权和错误。
- 一个 public DLL 消除双 ABI、跨 CRT free 和 package loader 复杂度。
- C ABI 的固定宽度标量、版本化结构体和 opaque handle 可供四语言可靠生成声明。

## 结果

- C++ bridge 必须捕获所有异常并返回 owned error。
- Rust export 必须捕获 panic；OOM/进程破坏不伪装成可恢复错误。
- OpenCV/Tesseract 升级不应改变 public ABI，只能通过可测试的 Vision 行为体现。
- C++ SDK wrapper 与 native bridge 是两个不同层：前者是 public RAII 包装，后者完全私有。

## 被否决的方案

- 公开 C++ class ABI：编译器、STL、异常和 allocator 兼容风险不可接受。
- 四语言直接绑定 OpenCV/Tesseract：资源和语义分叉。
- 在 Rust 中重写成熟图像/OCR 引擎：成本和正确性风险不合理。
- 独立发布 bridge DLL：产生第二个版本/装载/ABI 面。

## 关联

- [架构总览](../architecture/architecture-overview.md)
- [C ABI v1](../architecture/c-abi-v1.md)
- [构建发布](../architecture/build-release.md)
