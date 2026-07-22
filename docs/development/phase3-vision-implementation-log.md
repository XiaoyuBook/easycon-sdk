# Phase 3 Vision implementation log

This log records implementation evidence against ADR-0012. It is not a public ABI, release, or
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

## Node D: OCR and Rust-owned native pools

### Test-first and implementation evidence

The first OCR component, safe-wrapper, and pool tests failed because no Tesseract bridge,
`OcrEngine`, `NativePool`, or `OcrPool` existed. The tests fixed explicit model configuration,
exclusive engine access, FIFO admission, cancellation, poison discard, close ordering, and native
resource convergence before the implementations were added.

The completed node provides:

- private Tesseract 5.5.2 / Leptonica 1.87.0 create, process, reuse, and destroy entries with an
  explicit UTF-8 model root, language, engine mode, page segmentation mode, and output ceiling;
- `easycon-native-sys::ocr::OcrEngine` as the unique RAII owner: it is proven `Send`, intentionally
  not `Sync`, and requires exclusive mutable access for every recognition;
- a Runtime-supervised `NativePool` with a fixed worker count, bounded FIFO queue, queued and
  in-flight cancellation semantics, panic isolation, close/join, self-retention, and resource
  registration convergence;
- a bounded FIFO `OcrPool` that creates engines outside its state lock, reuses healthy engines,
  discards backend-poisoned engines, serializes cancellation notify with the wait predicate mutex,
  and waits for creating and borrowed owners during close;
- actual OCR through `Clear`, `SetPageSegMode`, `SetImage`, `Recognize`, `GetUTF8Text`, and
  `MeanTextConf`, with bounded UTF-8 output and confidence normalized to `0.0..1.0`;
- one generated Gray8 `EASCON` fixture with exact dimensions, bytes, hash, provenance, and an exact
  normalized recognition assertion;
- a test-only English model manifest and provisioner restricted to ignored
  `.tools/vision-models`, with every redirect hop, final URL, byte count, SHA-256, and license
  verified before use; no model byte is tracked;
- OCR validation calls in the ABI fuzzer and per-call handle/allocation convergence.

The test-only model is `tesseract-ocr/tessdata_fast` tag `4.1.0`, Apache-2.0. The ignored local
cache was revalidated as follows:

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `eng.traineddata` | 4,113,088 | `7D4322BD2A7749724879683FC3912CB542F19906C83BCC1A52132556427170B2` |
| `LICENSE` | 11,358 | `CFC7749B96F63BD31C3C42B5C471BF756814053E847C10F3EB003417BC523D30` |

This English test asset does not close O-03. The EasyCon-compatible `chi_sim` source, license, and
redistribution decision remain open, so neither the model nor OCR success asset is packaged.

### Independent review

The fixed Node D baseline received an independent read-only review. It found five reproducible
issues:

- a throwing filesystem/string model check was incorrectly marked `noexcept`;
- destroy could reject an engine-invalid poison state without consuming the Rust owner;
- a confidence exception after text allocation could return a failed call with nonzero output;
- conversion from `ImageError` to `VisionError` dropped the native kind/message owner;
- the provisioner allowed output outside the ignored model cache and checked only the final
  redirect URL.

Failpoints and sentinel regressions reproduced the native issues before the fixes. Model checking
now runs inside the entry guard; confidence is obtained and validated before committing the text
buffer; destroy clears the caller owner before teardown and always consumes a handle created by
the bridge; `VisionError` retains the complete native diagnostic; and the provisioner validates
cache containment before any directory or download action and rejects an off-host intermediate
redirect.

The review also confirmed that cancellation notify must hold the same mutex as the wait predicate
to close the check-to-wait lost-wakeup interval, and that Tesseract `Backend` process failures must
poison the engine while preflight, limit, allocation, and ordinary text mismatch paths do not.

The first final clang-tidy run then rejected an otherwise isolated empty catch in destroy. A
test-only `DESTROY_UNKNOWN` failpoint and `teardown_exceptions` counter now make that policy
observable: the component test combines an invalid marker with a teardown exception and proves
pointer consumption, exception count increment, created/destroyed symmetry, and handle/allocation
return to baseline. A directed final re-review reported zero open finding and cleared Node D.

### Native gate details

Every component CTest re-verifies the model and license hashes through the tracked runner. The
final source passed:

| Gate | Final result |
| --- | --- |
| MSVC Debug fresh configure/build and CTest | passed, 2/2 |
| MSVC Release fresh configure/build and CTest | passed, 2/2 |
| clang-cl ASan fresh configure/build and CTest | passed, 2/2 |
| clang-cl UBSan trap fresh configure/build and CTest | passed, 2/2 |
| clang-tidy warnings-as-errors | passed for all bridge, support, component, and fuzz translation units |
| MSVC `/analyze /analyze:external- /WX` | passed for bridge and both component executables |
| clang-cl libFuzzer full CTest | passed, 3/3 including OCR success and 128-run seed replay |

### Workspace and repository gates

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `cargo test --workspace --all-features` | passed, 192 tests plus 1 compile-fail doctest |
| `python tools/run_runtime_models.py` | passed, 6 Loom models |
| `python tools/validate_specs.py` | passed, including 15 Vision binary fixtures and provisioner regressions |
| `python tools/check_markdown_links.py` | passed, 123 references |
| `python tools/check_repository_guards.py` | passed |
| `git diff --check` | passed |

Node D makes no capture-device, hardware timing, performance, packaged OCR model, or Chinese OCR
accuracy claim. It adds no traineddata, EasyCon source, public C ABI, installed header, language
binding, or package artifact.

## Node E: legacy `.IL` and immutable Label evaluation

### Test-first and implementation evidence

The first parser and evaluator contract build failed with unresolved imports for every Label API.
That RED baseline fixed the 24-entry generated legacy corpus, registry behavior, one-Frame
evaluation, actual OCR scoring, and resource convergence before production implementation existed.

The completed node provides:

- an exact-case structured JSON visitor that detects duplicate known and unknown top-level keys,
  reports unknown fields as stable warnings, rejects invalid UTF-8/types/numbers/extensions, and
  applies the source-exact missing `searchMethod` default of numeric `5`;
- strict canonical padded Base64 validation with alphabet, padding, trailing-bit, decoded-size, and
  checked-arithmetic preflight before actual OpenCV BMP/PNG decode;
- immutable `Label`, `LabelTarget`, parse report, diagnostics, and registry types with hard JSON,
  source-count, per-source diagnostic, name, text-scalar, and edit-cell ceilings;
- absolute Range/Target ROI validation, Target-in-Range containment, decoded target dimension
  equality, and no `.ILX` entry;
- UTF-8 byte-order source sorting, explicit diagnostics for every conflicting source, and no
  partial registry publication after any error;
- an error-prioritized per-source diagnostic budget implemented with indexed `BTreeMap` state, so
  warning saturation and repeated identical source strings remain bounded without quadratic merge;
- image evaluation against exactly one caller-owned `Arc<Frame>`, preserving its sequence and
  monotonic timestamp while returning an absolute match location and normalized `0.0..1.0` score;
- OCR evaluation of only the absolute Target ROI through the Rust-owned engine pool, preserving raw
  bounded text and computing case-sensitive Unicode-scalar Levenshtein similarity times confidence;
- cancellation checks before, during, and immediately before committing text similarity, with
  rolling two-row storage and a checked edit-cell ceiling;
- deterministic parser fuzz seeds and bounded invalid/deep/random-input coverage.

The generated corpus is GPL-3.0-only and reuses only repository-owned synthetic codec bytes. Its
manifest records every path, byte count, SHA-256, expected outcome, generator, provenance, active
legacy method, score range, and `.ILX` exclusion. `.IL`, `.ILX`, and `.seed` paths are marked binary
so a Windows checkout cannot rewrite corpus bytes. No EasyCon file or model byte is copied.

### Independent review

The latest Node E working tree received an independent read-only review against ADR-0012, the Phase
3 design, and the EasyCon `ImgLabel`, `Search`, and `MatchFacts` facts. Reproducible findings were
first fixed by regressions:

- warning saturation could hide a later schema error and reach an internal `expect`;
- duplicate-name diagnostics could exceed the per-source cap, including repeated identical source
  strings;
- merging each parse diagnostic by scanning the global vector produced quadratic work at the legal
  `4096 * 32` ceiling;
- invalid Base64 could be misclassified as a decoded-size limit under small image limits;
- OCR Range validation and native crop ran before the frozen Target-only/configuration checks;
- empty-string and final-chunk text comparison could commit success after cancellation.

The final implementation prioritizes errors over warnings within the same fixed budget, uses
per-source indexed budget state, validates Base64 syntax before size, validates the OCR pool before
native admission, and checks cancellation at the text-comparison entry and final commit. The
reviewer reran all directed checks on the latest files and reported zero open finding.

### Workspace and repository gates

Node E changes no C++ source, private ABI, CMake, triplet, or dependency manifest, so no standalone
native preset was reopened. Cargo tests still linked the actual frozen OpenCV/Tesseract bridge and
ran the provisioned independent English OCR success asset without skipping.

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `cargo test --workspace --all-features` | passed, 207 tests plus 1 compile-fail doctest |
| `cargo test -p easycon-vision --all-features` | passed, 44 tests |
| `python tools/run_runtime_models.py` | passed, 6 Loom models |
| `python tools/validate_specs.py` | passed, including 15 Vision binary fixtures and 24 label corpus entries |
| `python tools/check_markdown_links.py` | passed, 123 references |
| `python tools/check_repository_guards.py` | passed |
| `git diff --check` | passed |

Node E makes no capture-device, hardware profile/FPS, packaged OCR model, Chinese OCR accuracy, ECS
integer-rounding, or Phase 4 claim. It adds no traineddata, EasyCon source, public C ABI, installed
header, language binding, or package artifact.

## Node F: Capture state machine and latest Frame slot

### Test-first and implementation evidence

The first capture contract builds failed because no private capture ABI, safe native owner,
`CaptureSession`, synthetic backend, state machine, or fixture manifest existed. Directed red tests
then fixed opening/streaming/fault/stop/closed transitions, one-reader ownership, immutable latest
Frame replacement, first-frame deadlines, cancellation, panic handoff, repeated close, destroy
acknowledgement, and Runtime registry convergence before the corresponding production paths were
accepted.

The completed node provides:

- a Rust-owned `CaptureSession` with one Runtime-supervised worker, an `Option<Arc<Frame>>` latest
  slot, checked sequence/timestamp/profile validation, and stable Opening, Streaming, Faulted,
  Stopping, and Closed states;
- Runtime Operation arbitration for first-frame deadlines, a `VirtualClock`-driven wait path, caller
  cancellation, fault-over-stale-frame behavior, and Streaming/latest priority over a caller token
  cancelled after the Frame became available;
- a deterministic synthetic backend with explicit open/read barriers, scripted frame/fault/end and
  panic paths, interrupt observation, finalize ownership failpoints, and no random sleeps;
- `easycon-native-sys::capture::CaptureHandle` as the unique `Send` and non-`Sync` owner, plus a
  cloneable `Send + Sync` atomic interrupt token; native destroy ownership is decided only from the
  inout pointer acknowledgement and preserves an Unconsumed owner for explicit retry or deliberate
  Runtime `CloseFailed` quarantine;
- a private Windows C++ bridge for bounded descriptor discovery, lexical absolute file-pattern
  validation, interrupt/close/destroy, three exception classes, output zeroing, allocation/handle
  counters, and consumed/unconsumed/no-ack destroy failpoints;
- a generated capture manifest and ABI fuzz seed that preserve a two-frame deterministic BMP corpus
  for future qualification while fixing the current support matrix to no native open backend.

DirectShow and Media Foundation discovery execute their real Windows enumeration APIs, but their
open paths remain Unsupported before device access. Path-backed File open likewise returns
Unsupported before filesystem or OpenCV decode access: synchronous path/decode calls cannot prove
the frozen timeout and interruptible-join contract. Only the Rust synthetic backend is qualified as
a reproducible capture input in this checkpoint.

### Independent review

The independent Node F review reproduced and closed two final implementation findings:

- synchronous File metadata/read/decode performed only a post-call elapsed check, so slow storage
  could exceed the requested quantum and make Runtime close block in join; three red tests first
  proved the old NoFrame/success behavior, then File admission was moved before all path access and
  fixed to Unsupported;
- a register-failure Abort worker could settle the startup Operation before the constructor
  committed the original Runtime admission error; a private red regression now proves Aborted
  leaves terminal ownership with the construction failure path.

The review also drove regressions for deadline-overflow backend finalization, output cleanup after a
native read error, unique `%02d` file-pattern validation, profile/stride limits, combined cleanup
diagnostics, interrupt-drain diagnostics, startup test synchronization, snapshot priority, and the
saved `CloseFailed` Unconsumed quarantine. The final directed re-review reported zero open finding
and confirmed that no public `easycon_v1_*` symbol, hardware profile/FPS claim, or support entry was
introduced.

### Native gate details

The ignored English OCR model and license were revalidated from the frozen manifest before every
component matrix that exercises OCR success. The final native source passed:

| Gate | Final result |
| --- | --- |
| MSVC Debug fresh configure/build and CTest | passed, 2/2 |
| MSVC Release fresh configure/build and CTest | passed, 2/2 |
| clang-cl ASan fresh configure/build and CTest | passed, 2/2 |
| clang-cl UBSan trap fresh configure/build and CTest | passed, 2/2 |
| clang-tidy warnings-as-errors | passed for all 9 bridge, support, component, and fuzz translation units |
| MSVC `/analyze /analyze:external- /WX` | passed for the bridge and component executable |
| clang-cl libFuzzer seed replay | passed, 1/1 CTest and 128 runs over the tracked ABI corpus |

### Workspace and repository gates

| Command | Final result |
| --- | --- |
| `cargo fmt --all --check` | passed |
| `cargo check --workspace --all-targets` | passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | passed |
| `cargo test --workspace --all-features` | passed, 229 tests plus 2 compile-fail doctests |
| `python tools/run_runtime_models.py` | passed, 6 Loom models |
| `python tools/validate_specs.py` | passed, including 15 Vision binary fixtures, 1 capture manifest, and 24 label corpus entries |
| `python tools/check_markdown_links.py` | passed, 123 references across 31 files |
| `python tools/check_repository_guards.py` | passed |
| `git diff --check` | passed |

Node F remains a Windows 10/11 x64, MSVC/CMake private-native checkpoint. The capture bridge directly
uses Windows DirectShow and Media Foundation headers and libraries; no non-Windows build contract is
claimed here. It makes no capture-card, stable device identity, backend/profile, resolution, pixel
format, FPS, close-SLO, packaged OCR model, public C ABI, language binding, ECS, Phase 4, or final
Phase 3 freeze claim.

## Reopened fix: NativePool decode admission before owned input construction

The independent final review of frozen candidate `76436de98836dc2fff9a43c4560e7dc7f3fea780`
found that `NativePool::decode` copied the complete encoded slice before cancellation, pool lifecycle,
queue capacity, and encoded-byte admission. ADR-0013 therefore remains a historical frozen record;
Phase 3 is reopened until this fix has a new SHA, complete gates, and a fresh independent review.

Four deterministic unit regressions instrument the exact owned-copy builder and native decode
boundary for unique large inputs. Against the unchanged old ordering, each test first proved the
stable error, zero native calls, and converged pool/Runtime/native counts, then failed only because
one equal-length, byte-identical, distinct-storage copy had completed:

- `oversized_decode_rejects_before_input_owned_copy_or_native_call`;
- `pre_cancelled_decode_rejects_before_input_owned_copy_or_native_call`;
- `closed_pool_decode_rejects_before_input_owned_copy_or_native_call`;
- `full_queue_decode_rejects_before_input_owned_copy_or_native_call`.

The fix reserves one FIFO ticket and queue slot under the pool state mutex before owned job
construction. Later admissions cannot pass the reservation. Encoded size is checked without copying
after the existing cancellation/lifecycle/capacity priority and before OpenCV. Job allocation and
capture destruction run outside the state mutex; reservation Drop rolls back capacity during unwind.
Close changes lifecycle to Closing, waits for an already-linearized reservation to commit or roll
back, cancels its queued job without running native code, then preserves the existing worker join and
registry convergence order. Existing Image captures remain `Arc` clones and were not widened into
this fix.

The four regressions and the complete 16-test `pool::tests` suite are green after the fix, including
reservation FIFO, cancellation wake, close handoff, builder panic rollback, and reentrant rejected
capture Drop. The complete `easycon-vision` suite passed 71 tests across pool, image, label, OCR,
matching/color, capture, and native boundaries. `easycon-native-sys` passed 11 tests plus two
compile-fail doctests. Fresh Windows MSVC Debug and Release configure/build/CTest each passed 2/2,
including OCR success. No behavior JSON, conformance fixture, native C++, CMake, vcpkg, public API,
or ceiling changed; the fix enforces the already-frozen ADR-0012 bounded-admission contract.

### Independent rereview and refreeze qualification

A fresh read-only reviewer fixed the candidate at `27444f16d0625a7ab7e4e1543736c7c3c225ce8a`,
tree `334a3fb21bf4d6ca55e0bcbe3e2daa6e675b3194`, and independently reproduced the old-to-new copy
transition with an external allocator harness: the pre-cancelled public decode call observed one
input-sized allocation on `76436de` and zero on `27444f16`, with the same stable error and converged
pool counts. The reviewer then reran the four regressions, the 16-test pool suite, 71 Vision tests,
11 native-sys tests, two compile-fail doctests, and six Loom models.

Fresh Windows MSVC Debug/Release, clang-cl ASan/UBSan, MSVC analyze, clang-tidy, and libFuzzer 128-run
gates all passed on the repaired implementation. The reviewer found no unresolved, reproducible,
actionable in-scope finding and concluded that the implementation qualified for refreeze. ADR-0014
records that refreeze without rewriting the historical ADR-0013. Linux and macOS builds and all
capture hardware remain unverified.
