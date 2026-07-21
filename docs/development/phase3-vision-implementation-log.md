# Phase 3 Vision implementation log

This log records implementation evidence against ADR-0011. It is not a public ABI, release, or
hardware qualification record.

## Baseline and architecture gate

- repository start: `9944dba50adc34484b65206e07ea0a444103f656`;
- architecture clearance: `751c448a4956d50bc454ed5dad7f1e575825562f`;
- implementation branch: `codex/phase-3-vision`;
- EasyCon evidence baseline: `11c4b992b9bce0ff977e9c587a6c0bb0d302853e`, read-only and untracked.

Production implementation started only after two independent architecture reviews cleared the
target ADR and design at the architecture clearance SHA.

## Frozen toolchain resolved on 2026-07-21

| Item | Resolved value |
| --- | --- |
| Rust and Cargo | 1.97.1 |
| CMake | 4.3.3 |
| Ninja | 1.13.2 |
| Visual Studio Build Tools | 17.14.36, MSVC 19.44.35228, v143 |
| Windows SDK | 10.0.26100 |
| clang-cl and clang-tidy | 20.1.8 |
| vcpkg tool | `2026-07-13-bf04c909169fdbb30821c02c6eb01f1cd1295d05` |
| vcpkg registry | release `2026.06.24`, `cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3` |
| triplet | `x64-windows-static-md` |
| OpenCV | `4.12.0#5` |
| Tesseract | 5.5.2 |
| Leptonica | 1.87.0 |

The manifest disables OpenCV default features and lists only the features needed by the frozen
Phase 3 plan. The registry baseline and triplet are repository data; downloaded archives, binary
caches, installed trees, and local proxy settings are not repository data.

## Node A: build and private bridge skeleton

### Test-first evidence

Before production files existed, the minimum component and Rust boundary tests failed because the
workspace members, native manifest, presets, private symbols, and link integration were absent. A
second regression required invalid `debug_counts` output to return a bridge-owned diagnostic; it
failed against the first skeleton and passed only after validation diagnostics and allocation
accounting were implemented.

The completed node provides:

- a private x64 `cdecl` header with fixed-width status, error, buffer, image-view, count, and opaque
  handle layouts;
- one `noexcept` guard that classifies OpenCV, allocation, standard, and unknown exceptions without
  invoking methods on an exception object;
- bridge-owned diagnostic allocation/release, idempotent zero releases, and live handle/allocation
  counters;
- a test-only exception entry point excluded from the production static library;
- narrow Rust FFI modules, copied UTF-8 diagnostics, and a safe RAII handle exposed only through
  `easycon-native-sys`;
- a safe `easycon-vision` diagnostics path with no unsafe code;
- an actual libFuzzer ABI harness with three tracked seeds and per-input resource convergence.

### Native gate details

All commands below ran from a Visual Studio x64 developer environment with `VCPKG_ROOT` set to the
official vcpkg installation. Configure directories and dependency installations were independent
per preset.

| Gate | Final result |
| --- | --- |
| `cmake --preset msvc-debug`, build, `ctest --preset msvc-debug --no-tests=error` | passed, 1/1 |
| `cmake --preset msvc-release`, build, `ctest --preset msvc-release --no-tests=error` | passed, 1/1 |
| `cmake --preset clang-asan`, build, `ctest --preset clang-asan --no-tests=error` | passed, 1/1 |
| `cmake --preset clang-ubsan`, build, `ctest --preset clang-ubsan --no-tests=error` | passed, 1/1 |
| clang-tidy target with warnings as errors | passed for bridge, test support, component test, and fuzz harness |
| MSVC `/analyze /analyze:external- /WX` owned targets | passed |
| libFuzzer target and `fuzz-seed-replay` CTest label | passed, three seeds and 128 runs |

The ASan targets query clang-cl for the matching runtime, explicitly link its Windows dynamic
runtime and thunk, and stage the runtime DLL only in ignored build output. MSVC STL string/vector
annotations are disabled for sanitizer targets because the locked static vcpkg dependencies are
not built with those annotations; all owned translation units remain address-instrumented.

LLVM's packaged Windows standalone UBSan C++ runtime is built with `/MT`, which is incompatible
with the frozen `/MD` bridge and OpenCV ABI. The `clang-ubsan` preset therefore retains
`-fsanitize=undefined` instrumentation in official trap mode. Any detected undefined behavior
terminates the component test and fails CTest without linking an incompatible runtime.

LLVM's packaged Windows libFuzzer runtime is also `/MT`. The fuzz-only runner uses that CRT while
the address-instrumented SUT remains a `/MD` private DLL with an explicit export definition. Calls
cross only the frozen opaque C-compatible boundary, and every allocation is released by the DLL
that created it. This DLL is test-only and has no install rule.

Findings closed while running the gates:

- clang-cl ASan exposed unsafe reliance on `cv::Exception::what()` and
  `std::exception::what()` across the runtime boundary; fixed diagnostics now preserve status
  classification without dereferencing exception objects;
- MSVC analysis reported the aggregate write performed by `nothrow new`; the guarded handle
  allocation now uses throwing `new`, and the existing `bad_alloc` catch performs the mapping;
- clang-tidy's allocation rule cannot follow the call through the templated guard, so the exact
  allocation line carries a scoped suppression with the guard invariant documented immediately
  above it.

### Workspace and repository gates

After the final native fixes, the required sequence completed successfully:

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `cargo test --workspace --all-features` | passed, 160 tests |
| `python tools/run_runtime_models.py` | passed, 6 Loom models |
| `python tools/validate_specs.py` | passed, 5 schemas, 1 behavior spec, 3 controller fixtures, 9 scenarios, and 65 exact Rust tests |
| `python tools/check_markdown_links.py` | passed |
| `python tools/check_repository_guards.py` | passed |
| `git diff --check` | passed |

No physical capture device, OCR model, OCR success case, capture profile, or hardware timing claim
is part of Node A. No traineddata, EasyCon source file, public C ABI symbol, installed header,
language binding, or package artifact is produced.
