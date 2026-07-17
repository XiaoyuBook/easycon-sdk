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
  scenarios.
- `schemas/` contains JSON Schema Draft 2020-12 documents for every asset shape. The local validator
  applies the schema subset used here to each concrete instance rather than only parsing the schema
  documents.

The fixtures were transcribed from the source evidence named in each file. Controller golden-vector
and precise-sequence tests read these tracked files directly. Tests and validation do not open,
build, download, or otherwise depend on `EasyCon/`.

Run the local validator with:

```powershell
python tools/validate_specs.py
```

Repository and documentation guards are separate so failures stay attributable:

```powershell
python tools/check_markdown_links.py
python tools/check_repository_guards.py
```
