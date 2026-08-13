# EasyCon SDK product specifications and dormant ECS maintenance corpus

This directory indexes two separately scoped sets of tracked machine-checkable assets.
[ADR-0023](../docs/decisions/0023-v1-host-language-sdk-roadmap-and-ecs-deferral.md) defines the
current SDK v1 product around Runtime, Controller, and Vision, and defers ECS/Automation.

## Current v1 product specifications

These behavior and conformance assets cover the current v1 Runtime/Controller product and any
applicable Vision contracts. [ADR-0026](../docs/decisions/0026-controller-d1-settlement-refreeze.md)
proposes the Controller D1 software-settlement refreeze, and
[ADR-0027](../docs/decisions/0027-stage-1-software-core-closeout.md) assembles the Stage 1 Windows
software-core closeout candidate from tracked fake/synthetic/native evidence and the
[license initial review](../docs/development/stage1-license-initial-review.md). The retained
Controller/Serial scope remains `Hardware Unverified`; this R document candidate remains
`Pending Stage Review`, requires user-authorized integration before it can become canonical `main`,
and does not authorize Stage 2.

- `behavior/runtime-controller-v1.json` fixes operation, timeout, event, shutdown, serial,
  Controller, Amiibo, sequence, timing, and Hardware Unverified behavior.
- `fixtures/controller/reports-v1.json` records source-exact protocol values, report bytes, and
  Amiibo save/select/reset facts plus corrected source behaviors that must not be preserved.
- `fixtures/controller/sequence-traces-v1.json` fixes precise-sequence dispatch and cancellation
  traces in virtual monotonic nanoseconds.
- `fixtures/controller/phase2a-latency-result-v1.json` records the fixed-machine 10,000-sample
  software-path distribution and its explicit non-hardware scope.
- `fixtures/controller/lease-settlement-v1.json` fixes the Controller D1 software-only acquire,
  action, release, close, partial-stream, and Win32 completion-owner contract used by the
  ADR-0026/0027 candidate; it does not declare real hardware, ABI, binding, package, or release
  qualification.
- `conformance/runtime-controller-v1.json` defines the hardware-free Runtime and Phase 2A fault
  scenarios. Every step and assertion has a stable ID. Each scenario declares the exact executable
  Rust test suite covering its assertions, and every assertion maps through a matching
  `// conformance:` source marker.

## Dormant ECS maintenance corpus

The static Phase 4 S0 ECS provenance foundation remains tracked as dormant maintenance under its
historical/future ECS contracts. It is not current v1 product scope or completion, and does not
establish a public ABI, shared language acceptance, hardware/soak evidence, release evidence, or a
v1 product prerequisite. The ECS fixtures, schemas, generator, validator, and their health checks
remain maintained rather than deleted or downgraded.

- `fixtures/ecs/manifest.json` separates Legacy Exact, Corrected, and v1-native provenance. Its 11
  records own 33 SDK-local input, observed, expected, source-snapshot, and profile artifacts. The
  Corrected records preserve every ADR-0017 PRINT and label production/test oracle independently;
  the v1-native records preserve the fixed heap-order pair without claiming legacy evidence.

`v1-native` is an existing ECS provenance-class name. It does not describe a current
ADR-0023 v1 product capability, support status, or release status.

## Shared schemas and validation

- `schemas/` contains JSON Schema Draft 2020-12 documents for every asset shape. The local validator
  applies the schema subset used here to each concrete instance rather than only parsing the schema
  documents.

Protocol fixtures were transcribed from the source evidence named in each file; the latency fixture
is a dated local measurement with an explicit software-only scope. Controller golden-vector and
precise-sequence tests read their tracked files directly. Tests and validation do not open, build,
download, or otherwise depend on `EasyCon/`.

`tools/generate_ecs_provenance_fixtures.py --check` deterministically verifies the tracked dormant
ECS file set as a maintenance health check. Its legacy commit, full source blob identities, and
exact source excerpts are fixed data; the generator performs no checkout discovery, network access,
or external source read.

`tools/validate_specs.py` validates both the current product specifications and the dormant ECS
maintenance corpus. It compares the complete assertion and Rust marker sets, checks each scenario
suite for exact assertion coverage, resolves every mapped source through Cargo metadata to one
workspace package and target, then executes each unique test against that exact target. A mapping is
valid only when libtest reports exactly one passed and zero ignored tests. The validator rejects
duplicate assertion/step IDs, missing or renamed tests, stale source markers, `cfg`-disabled or
ignored tests, cross-target same-name collisions, incomplete scenario suites, and mappings that
disagree with the test carrying the marker. Runtime stabilization assertions execute production
regression paths with named fault injection where failure behavior is part of the contract.

Run the local validator with:

```powershell
python tools/validate_specs.py
```

Repository and documentation guards are separate so failures stay attributable:

```powershell
python tools/check_markdown_links.py
python tools/check_repository_guards.py
```
