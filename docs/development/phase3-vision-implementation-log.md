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

## Node B: immutable images, frames, and codec

### Test-first and review evidence

The first Rust image contract and native component tests failed because `Frame`, `Image`, image
limits, codec ABI entries, and actual OpenCV implementations did not exist. A second regression
proved that a blanket four-channel preflight incorrectly rejected the exact twelve-byte limit for
the tracked 24-bit BMP and RGB PNG.

An independent read-only review of the completed diff then reproduced two private-boundary bugs:

- a null error output returned before the four codec result outputs were zeroed;
- a borrowed image could declare a buffer length above `max_decoded_bytes` while its required
  layout remained small.

Sentinel and oversized-declared-length component regressions failed five assertions before the
fix. The codec entries now zero outputs before error-storage validation, and native view validation
enforces both required layout and declared length ceilings. The same review found that the first
fixture generator depended on the host zlib compression strategy. It was replaced by a byte-exact
generator that writes the zlib header, one final stored-DEFLATE block, LEN/NLEN, and Adler-32
directly. A final directed re-review cleared every finding and found no new reproducible in-scope
defect.

The completed node provides:

- immutable Rust `Image` and `Frame` values backed by `Arc<[u8]>`, with checked dimensions,
  pixel count, stride, buffer length, ROI, sequence, and timestamp metadata;
- BGR8, BGRA8, and Gray8 format conversion, tight native outputs, padded borrowed inputs, and
  owned crop results;
- structured BMP and PNG header preflight before OpenCV allocation, followed by actual
  `imdecode`, `imencode`, `cvtColor`, and ROI clone operations;
- exact BGR/BGRA/Gray PNG round trips and ROI pixels, opaque alpha insertion, GrayAlpha-to-BGRA
  normalization, and deterministic rejection of palette, unsupported depth, invalid, truncated,
  zero-dimension, and oversized inputs;
- conservative checked PNG output bounds based on zlib `compressBound`, plus bridge allocation
  release on success, failure, invariant failure, and repeated zero release;
- five synthetic codec fixtures whose generator, GPL-3.0-only provenance, decoded sizes, hashes,
  PNG chunk CRCs, and exact file set are enforced by `validate_specs.py`.

### Native gate details

All native configurations were regenerated after the review fixes and deterministic fixture
changes.

| Gate | Final result |
| --- | --- |
| MSVC Debug configure/build and CTest | passed, 1/1 |
| MSVC Release configure/build and CTest | passed, 1/1 |
| clang-cl ASan configure/build and CTest | passed, 1/1 |
| clang-cl UBSan trap configure/build and CTest | passed, 1/1 |
| clang-tidy build with warnings as errors and CTest | passed, 2/2 including seed replay |
| MSVC `/analyze /analyze:external- /WX` and CTest | passed, 1/1 |
| libFuzzer build and `fuzz-seed-replay` CTest | passed, 2/2 |

### Workspace and repository gates

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `cargo test --workspace --all-features` | passed, 167 tests |
| `python tools/run_runtime_models.py` | passed, 6 Loom models |
| `python tools/validate_specs.py` | passed, including 5 generated Vision codec fixtures |
| `python tools/check_markdown_links.py` | passed |
| `python tools/check_repository_guards.py` | passed |
| `git diff --check` | passed |

Node B makes no capture-device, OCR model, OCR accuracy, hardware profile, or performance claim.
It adds no traineddata, EasyCon source, public C ABI, installed header, or package artifact.

## Node C: normalized template, edge, and HSV color

### Test-first evidence

Before production symbols were added, the new fixture-backed Rust contract test was compiled
directly against the last Node B `easycon_vision` rlib. Rust failed with `E0432` for exactly the
six absent Node C entries: `EdgeMethod`, `HsvRange`, `TemplateMethod`, `match_edge`,
`match_template`, and `preprocess_edge`. A concurrent first native configure exceeded the command
wrapper timeout while the isolated vcpkg install root compiled locked OpenCV 4.12.0, so that
timeout is not counted as RED evidence. The generated corpus and tests were retained unchanged
before production implementation began.

The completed node provides:

- all three normalized OpenCV template modes with native min/max extrema and Rust-owned legacy
  location/score mapping: `1-min`, `max`, and `(max+1)/2`, followed by a finite-only clamp;
- deterministic rejection of zero normalized denominators and non-finite native extrema;
- exact Gray8 XY preprocessing using Scharr through `Sobel(..., CV_16S, ..., -1)` and weighted
  X/Y gradients, plus the frozen Gaussian/Laplacian/absolute/threshold pipeline;
- Rust-owned edge composition that preprocesses search and target once and always performs final
  `CCoeffNormed` matching;
- bounded HSV statistics for BGR8, BGRA8, and Gray8, including hue wrap, count, ROI-relative native
  bounding boxes, checked absolute Rust bounding boxes, ratio, and caller-side threshold policy;
- transactional private outputs, complete x64 size/alignment/offset assertions on both sides of
  the FFI, and no new handle, thread, `Send`, or `Sync` implementation;
- nine generated synthetic operation fixtures with GPL-3.0-only provenance, OpenCV 4.12.0
  reference version, exact pixel hashes, explicit floating tolerance, and exact-file-set checks;
- fuzz calls for every template/edge mode and bounded HSV inputs, with error/image release and
  per-input native resource convergence.

The first fixed Laplacian pixels were captured with a host OpenCV 4.5 reference and failed exactly
against the locked OpenCV 4.12.0 implementation while XY, template, HSV, and final locations
passed. The tracked generator was corrected to the 4.12.0 exact Gray8 bytes and now rejects any
reference vector whose decoded dimensions differ before writing files. No tolerance was applied
to edge pixels.

### Independent review

The first independent read-only review found two reproducible in-scope defects:

- insertion of the operation validator had displaced the codec generator `returncode` check, so a
  broken Node B codec fixture could be silently accepted;
- the three new FFI structs fixed size and selected offsets but omitted alignment and complete
  field offsets required by the frozen x64 layout contract.

The validator now uses one injectable helper, separately binds codec and operation generators,
and executes failure-injection regressions for both paths before the real checks. C++ and Rust now
assert size, alignment, and every field offset for every non-opaque private value struct, including
the pre-existing error, buffer, image, limits, and resource-count structs. Directed re-review
marked both findings Closed and reported zero open finding. Reviewer backlog only, not a Node C
blocker: add selective rather than full-range BGRA/Gray HSV cases, explicit SqDiff/CCorr zero
normalizer cases, and padded-stride Node C cases.

Clang-tidy then found two narrowing/widening diagnostics in component-test failure reporting. The
iterator offset was replaced with an explicit bounded `size_t` loop and the BGRA reserve product
now starts in `size_t`; the full warnings-as-errors target passed after those fixes.

### Native gate details

All official presets were regenerated from the final reviewed source with the locked vcpkg
registry. OpenCV was `4.12.0#5`; Tesseract `5.5.2` and Leptonica `1.87.0` were installed for the
next node but Node C neither calls nor links OCR APIs.

| Gate | Final result |
| --- | --- |
| MSVC Debug configure/build and CTest | passed, 1/1 |
| MSVC Release configure/build and CTest | passed, 1/1 |
| clang-cl ASan configure/build and CTest | passed, 1/1 |
| clang-cl UBSan trap configure/build and CTest | passed, 1/1 |
| clang-tidy warnings-as-errors build and CTest | passed, 2/2 including seed replay |
| MSVC `/analyze /analyze:external- /WX` build and CTest | passed, 1/1 |
| clang-cl libFuzzer build and CTest | passed, 2/2 including 128-run seed replay |

### Workspace and repository gates

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `cargo test --workspace --all-features` | passed, 171 tests |
| `python tools/run_runtime_models.py` | passed, 6 Loom models |
| `python tools/validate_specs.py` | passed, including 14 Vision binary fixtures and both failure regressions |
| `python tools/check_markdown_links.py` | passed, 123 references |
| `python tools/check_repository_guards.py` | passed |
| `git diff --check` | passed |

Node C makes no OCR-model, capture-device, hardware timing, or performance claim. It adds no
traineddata, EasyCon source, public C ABI, installed header, language binding, or package artifact.
