# 0028: Windows candidate gate layering

- Status: Accepted / Effective for candidate validation orchestration
- Date: 2026-08-13
- Scope: Windows controlled validation only; no product, ABI, package, hardware, or release claim
- Evolves: [ADR-0022](0022-windows-workspace-policy-identity-and-evidence-v2.md)

## Context

The Windows Workspace candidate path accumulated deterministic policy contracts and real operating-
system qualification in the same blocking run as product compilation and tests. That duplicated
validation, made process and filesystem timing part of every candidate credential, and obscured
which result actually authorized a candidate. The environment Verify boundary, frozen product and
supply-chain checks, and schema-v2 tree evidence remain required.

## Decision

Windows validation is split into four layers:

| Layer | Role | Blocking boundary |
| --- | --- | --- |
| A | The only formal candidate credential | Every candidate subject to the repository risk profile |
| B | Bootstrap plus 12 deterministic Fast contract groups | Only infrastructure candidates selected by the required-CI path classifier |
| C | Real process, worktree, cache, lock, download, reparse, cleanup, and timing Qualification | Scheduled or manually dispatched observation; not a normal candidate required status |
| D | Removed duplicate harness registrations, synonymous variants, and the separate lifecycle runner | No independent result |

### A: candidate credential

`tools/run_windows_workspace.ps1 -Mode Workspace` remains the controlled entry. Verify still runs
before gates. `tools/windows_gate_policy.json` is the sole ordered gate data source and contains:

1. `cargo fmt --all --check`;
2. default-feature, all-target workspace `cargo check`;
3. all-feature, all-target workspace Clippy with warnings denied;
4. all-feature workspace tests;
5. Runtime models;
6. the frozen specification validator;
7. Markdown links;
8. repository guards;
9. `git diff --check`.

Staged candidates additionally run the cached diff check. A supplied base commit additionally runs
the base-to-HEAD diff check. The PowerShell policy parser validates strict JSON structure, command
rendering, tool and argument safety, Cargo job injection, and exclusion of infrastructure contracts;
it does not contain a second full command table. The Python repository guard independently checks
generic schema, safety, and required A properties without becoming an execution list.

ADR-0022 evidence remains UTF-8 schema version 2 with no migration. Every passed record remains
bound to base, HEAD, candidate tree, environment fingerprint, policy hash, ordered gate status and
timings, and no-replace publication. Changing the policy may change the gate count, not the binding.

### B: deterministic infrastructure contracts

`tools/test_windows_workspace.ps1 -Mode Fast` runs the bootstrap contracts followed by exactly 12
registered, unique groups. Selection and registered/executed counts fail closed; unknown, duplicate,
cross-mode, or wrong-case input fails. Fast covers configuration/fingerprint, version and pin parsers,
module/wrapper behavior, Targeted planning, candidate/evidence binding, policy revalidation, stamp and
tool damage, atomic publication, transport policy, deterministic MSVC/environment seams, and in-
process lifecycle restoration. Synonymous mutations are table-driven inside those groups.

Required CI runs B only when its explicit classifier sees runner, policy, environment, bootstrap,
configuration, manifest, or related workflow paths. An unavailable or all-zero base is classified
conservatively as infrastructure. Ordinary product-source paths do not run B. The warm target is at
most 90 seconds and is not achieved by sleeps or weaker assertions. The Workspace job depends on
the Policy job, so a selected B failure stops the workflow before A starts.

### C: operating-system qualification

`tools/test_windows_workspace.ps1 -Mode Qualification` owns four groups for real worktree
concurrency, child-process lifecycle, cache/lock/download behavior, and junction/cleanup behavior.
The existing Native Quality schedule and manual dispatch run it as an independent non-required job.

The real-worktree group creates two detached worktrees at one fixed commit. Each child holds the
environment, workspace, and shared-cache lifecycle leases while it verifies checkout identity and
publishes its isolated vcpkg layout. It records `verification.completed` before `ready.written`, waits
for parent release, and only then records `release.observed`; the parent proves the leases remain held
at the Ready barrier. Teardown terminates all live children first, preserves the primary failure and
residual diagnostics, then removes owned worktrees and bounded fixture roots.

### D: consolidation

The separate `tools/test_windows_environment_lifecycle.ps1` runner is removed. Its deterministic
lifecycle coverage moves into Fast and its operating-system coverage moves into Qualification. Marker-
wait, final-observation, and teardown harness self-tests are not separately registered. One runner,
one helper set, and one process harness own selection and cleanup.

## Consequences

- A remains the only reusable candidate credential; B and C cannot substitute for its evidence.
- Infrastructure changes fail quickly and deterministically in B before A, while ordinary product
  candidates no longer block on PowerShell infrastructure contracts.
- Real operating-system behavior remains observable in C without making timing fixtures a normal
  required status.
- This decision does not close a product stage, approve an H review, or authorize integration or
  release.

## Related

- [Build, release, and compliance](../architecture/build-release.md)
- [Testing strategy](../architecture/testing-strategy.md)
- [GitHub CI boundary](../../.github/CI.md)
