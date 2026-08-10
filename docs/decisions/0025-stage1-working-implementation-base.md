# 0025: Stage 1 working implementation base

- Status: Accepted working-base record / Not a refreeze
- Date: 2026-08-08
- Working implementation base: `main@3f1481480b3ee4206aa75a248a001213f960fcb6`
- Working implementation tree: `97afa1a69f389b4395fd6dc02b89300f9097e958`
- Runtime prerequisite: [ADR-0020](0020-controller-settlement-runtime-prerequisites.md),
  [ADR-0024](0024-runtime-r0-v2-refreeze.md), and the reviewed DeadlineSignal observer
  implementation at the working base
- Controller contract: [ADR-0018](0018-phase-2a-controller-lease-reopen.md) and
  [ADR-0020](0020-controller-settlement-runtime-prerequisites.md)

## Purpose

This record authorizes one H-level development candidate to use the named `main` object as its
working implementation base. The base contains the independently reviewed Runtime R0-v2 work and
the subsequent DeadlineSignal observer. It is a development input for Controller D1; it is not a
new Runtime freeze, Controller refreeze, Stage 1 closeout, release candidate, or product support
claim.

The implementation candidate begins with the existing Runtime `DeadlineRegistration`,
`DeadlineSignal`, and `OperationSettlementOwner` contracts to implement Controller lease/action/
release/close settlement. Backend final-byte acceptance exposed a deterministic cross-crate blocker:
the Runtime winner must be claimed at the physical gate, while Controller still needs to complete
post-write bookkeeping before publishing the terminal result. The candidate therefore narrowly
extends `OperationSettlementOwner` with a nonblocking claim and deferred finish transaction. This
reopens only that Runtime implementation surface under ADR-0020's existing claim -> cleanup ->
commit contract; it is not a Runtime refreeze. Any later independently authorized review of this
candidate must cover both the Runtime transaction and its Controller gate integration.

## Preserved decisions

ADR-0018, ADR-0020, and ADR-0024 remain historical records and are not edited by this working-base
record. In particular, ADR-0024 continues to identify its own Runtime R0-v2 refreeze object; this
record does not rewrite that object or describe `3f1481480b3ee4206aa75a248a001213f960fcb6` as
refrozen.

The D1 candidate must preserve the four existing Controller RED intents named by ADR-0020:

- `system_clock_acquire_deadline_settles_without_lane_progress`
- `release_interrupts_an_uneffected_generation_action_before_neutral`
- `close_settles_pending_acquire_actions_and_release_before_join`
- `close_interrupts_release_neutral_before_acceptance_and_joins`

They are implementation obligations, not evidence that D1 or Stage 1 has completed.

## Deferred closeout

After this builder candidate has its own staged Workspace evidence and an independent H review,
a separate decision may record a Controller refreeze. A later independent Stage 1 closeout may
evaluate Runtime, Controller, and Vision together. Neither decision is made here, and this record
does not authorize canonical integration, release, hardware claims, public C ABI, bindings,
packages, ECS, or Automation product scope.

## Verification boundary

This record is documentation for the development-candidate boundary. Its own presence does not
prove Rust compilation, deterministic concurrency behavior, serial/hardware behavior, independent
review, or a staged Workspace credential. Those claims require the D1 candidate's later evidence.

## Related

- [ADR-0018: Controller lease settlement reopen](0018-phase-2a-controller-lease-reopen.md)
- [ADR-0020: Controller settlement Runtime prerequisites](0020-controller-settlement-runtime-prerequisites.md)
- [ADR-0024: Runtime R0-v2 refreeze](0024-runtime-r0-v2-refreeze.md)
