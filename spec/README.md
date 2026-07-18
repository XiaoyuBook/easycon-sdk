# EasyCon SDK v1 milestone specifications

This directory contains the machine-checkable behavior and conformance baseline for the first
Rust Runtime + Controller fake vertical slice. It is intentionally narrower than the complete v1
architecture.

- `behavior/runtime-controller-v1.json` fixes operation, timeout, event, shutdown, and controller
  scheduling behavior.
- `fixtures/controller/reports-v1.json` records source-exact protocol values and report bytes plus
  corrected source behaviors that must not be preserved.
- `fixtures/controller/sequence-traces-v1.json` fixes precise-sequence dispatch and cancellation
  traces in virtual monotonic nanoseconds.
- `conformance/runtime-controller-v1.json` defines the hardware-free vertical slice and fault
  scenarios. Every step and assertion has a stable ID. Each scenario declares the exact executable
  Rust test suite covering its assertions, and every assertion maps through a matching
  `// conformance:` source marker.
- `schemas/` contains JSON Schema Draft 2020-12 documents for every asset shape. The local validator
  applies the schema subset used here to each concrete instance rather than only parsing the schema
  documents.

The fixtures were transcribed from the source evidence named in each file. Controller golden-vector
and precise-sequence tests read these tracked files directly. Tests and validation do not open,
build, download, or otherwise depend on `EasyCon/`.

`tools/validate_specs.py` compares the complete assertion and Rust marker sets, checks each scenario
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
