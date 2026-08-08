use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use easycon_model::{EasyConError, ErrorCode, ErrorDomain};
use easycon_runtime::{
    CancellationReason, CloseOutcome, EventKind, Operation, OperationState, OperationValue,
    Runtime, RuntimeCounts, SettlementEvidence, SettlementOwnerMode, SubscriptionOptions,
    SubscriptionRead, TerminalCandidate, TransitionOutcome, VirtualClock, WaitResult, WaitTimeout,
};

const WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
enum CleanupCase {
    Success,
    Failure,
    Panic,
}

#[derive(Clone, Copy, Debug)]
enum PrimaryCase {
    Success,
    Failure,
    Cancel(CancellationReason),
}

fn primary_failure() -> EasyConError {
    EasyConError::new(
        ErrorDomain::Validation,
        ErrorCode::InvalidArgument,
        "primary failure",
    )
}

fn cleanup_failure() -> EasyConError {
    EasyConError::new(ErrorDomain::Runtime, ErrorCode::Internal, "cleanup failure")
}

fn terminal_events(
    runtime_events: &easycon_runtime::EventSubscription,
    operation: &Operation,
) -> usize {
    std::iter::from_fn(
        || match runtime_events.read(WaitTimeout::Poll).expect("event read") {
            SubscriptionRead::Event(event) => Some(event),
            SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
        },
    )
    .filter(|event| event.kind == EventKind::Terminal && event.operation_id == Some(operation.id()))
    .count()
}

// conformance: operation.owner-evidence-cleanup-matrix
#[test]
fn legitimate_owner_runs_five_terminal_paths_with_cleanup_projection() {
    let primaries = [
        PrimaryCase::Success,
        PrimaryCase::Failure,
        PrimaryCase::Cancel(CancellationReason::Requested),
        PrimaryCase::Cancel(CancellationReason::Deadline),
        PrimaryCase::Cancel(CancellationReason::ParentClose),
    ];
    let cleanups = [
        CleanupCase::Success,
        CleanupCase::Failure,
        CleanupCase::Panic,
    ];

    for primary in primaries {
        for cleanup in cleanups {
            let runtime = Runtime::new(Arc::new(VirtualClock::default()));
            let events = runtime
                .subscribe(SubscriptionOptions::default())
                .expect("events");
            let operation_slot = Arc::new(Mutex::new(None::<Operation>));
            let observed_nonterminal = Arc::new(AtomicBool::new(false));
            let observed_slot = Arc::clone(&operation_slot);
            let observed_state = Arc::clone(&observed_nonterminal);
            let (cleanup_result, observed_cleanup_result) = mpsc::sync_channel(1);
            let (operation, owner) = runtime
                .create_operation_with_settlement_owner(
                    None,
                    SettlementOwnerMode::Exclusive,
                    move || match cleanup {
                        CleanupCase::Success => Ok(()),
                        CleanupCase::Failure => Err(cleanup_failure()),
                        CleanupCase::Panic => panic!("scripted cleanup panic"),
                    },
                    move |result| {
                        let operation = observed_slot
                            .lock()
                            .expect("operation slot")
                            .clone()
                            .expect("operation installed");
                        observed_state
                            .store(!operation.snapshot().state.is_terminal(), Ordering::Release);
                        cleanup_result
                            .send(result)
                            .expect("cleanup result observer");
                    },
                )
                .expect("owned operation");
            *operation_slot.lock().expect("operation slot") = Some(operation.clone());
            assert_eq!(operation.start(), TransitionOutcome::Applied);

            let (evidence, candidate) = match primary {
                PrimaryCase::Success => (
                    SettlementEvidence::EffectAccepted,
                    TerminalCandidate::Success(OperationValue::Unit),
                ),
                PrimaryCase::Failure => (
                    SettlementEvidence::ExecutionFailed,
                    TerminalCandidate::Failure(primary_failure()),
                ),
                PrimaryCase::Cancel(reason) => {
                    assert_eq!(operation.request_cancel(reason), TransitionOutcome::Applied);
                    (
                        SettlementEvidence::NotDelivered,
                        TerminalCandidate::Cancellation,
                    )
                }
            };
            let (settled, observed_settled) = mpsc::sync_channel(1);
            let task = runtime
                .spawn_operation_owner("terminal-matrix", owner, move |owner| {
                    settled
                        .send(owner.settle(evidence, candidate))
                        .expect("settled");
                })
                .expect("owner task");
            let outcome = observed_settled
                .recv_timeout(WAIT)
                .expect("terminal outcome");
            assert_eq!(
                task.join(),
                Ok(easycon_runtime::SupervisedTaskOutcome::Completed)
            );
            assert!(
                observed_nonterminal.load(Ordering::Acquire),
                "{primary:?}/{cleanup:?}"
            );

            let observed_cleanup = observed_cleanup_result
                .recv_timeout(WAIT)
                .expect("cleanup observer");
            match cleanup {
                CleanupCase::Success => {
                    assert_eq!(outcome, TransitionOutcome::Applied);
                    assert_eq!(observed_cleanup, Ok(()));
                }
                CleanupCase::Failure => {
                    assert_eq!(outcome, TransitionOutcome::CleanupFailed);
                    assert_eq!(observed_cleanup, Err(cleanup_failure()));
                }
                CleanupCase::Panic => {
                    assert_eq!(outcome, TransitionOutcome::CleanupFailed);
                    let error = observed_cleanup.expect_err("panic becomes cleanup failure");
                    assert_eq!(error.domain(), ErrorDomain::Internal);
                    assert_eq!(error.code(), ErrorCode::Internal);
                }
            }

            let snapshot = operation.snapshot();
            match primary {
                PrimaryCase::Success if matches!(cleanup, CleanupCase::Success) => {
                    assert_eq!(snapshot.state, OperationState::Succeeded);
                    assert_eq!(snapshot.result, Some(OperationValue::Unit));
                }
                PrimaryCase::Success => {
                    assert_eq!(snapshot.state, OperationState::Failed);
                    assert!(snapshot.error.is_some());
                }
                PrimaryCase::Failure => {
                    assert_eq!(snapshot.state, OperationState::Failed);
                    assert_eq!(snapshot.error, Some(primary_failure()));
                }
                PrimaryCase::Cancel(reason) => {
                    assert_eq!(snapshot.state, OperationState::Cancelled);
                    assert_eq!(snapshot.cancellation_reason, Some(reason));
                    assert_eq!(
                        snapshot.error.expect("cancellation error").code(),
                        if reason == CancellationReason::Deadline {
                            ErrorCode::DeadlineExceeded
                        } else {
                            ErrorCode::Cancelled
                        }
                    );
                }
            }
            assert_eq!(
                terminal_events(&events, &operation),
                1,
                "{primary:?}/{cleanup:?}"
            );
            assert_eq!(runtime.counts().active_operations, 0);
            assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        }
    }
}

// conformance: operation.cleanup-first-error
#[test]
fn settlement_observer_panic_preserves_the_earlier_cleanup_error() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let expected = cleanup_failure();
    let returned = expected.clone();
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Exclusive,
            move || Err(returned),
            |_| panic!("scripted settlement observer panic"),
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);

    assert_eq!(
        owner.settle(
            SettlementEvidence::EffectAccepted,
            TerminalCandidate::Success(OperationValue::Unit),
        ),
        TransitionOutcome::CleanupFailed
    );
    let snapshot = operation.snapshot();
    assert_eq!(snapshot.state, OperationState::Failed);
    assert_eq!(snapshot.error, Some(expected));
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: operation.intent-does-not-claim
// conformance: operation.accepted-claim-late-cancel
#[test]
fn cancellation_intent_before_accepted_evidence_does_not_override_success() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let (cleanup_started, observed_cleanup) = mpsc::sync_channel(0);
    let (release_cleanup, cleanup_released) = mpsc::sync_channel(0);
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Exclusive,
            move || {
                cleanup_started.send(()).expect("cleanup start");
                cleanup_released.recv().expect("cleanup release");
                Ok(())
            },
            |_| {},
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    let (cancelled, observed_cancel) = mpsc::sync_channel(1);
    operation.on_cancel(move || cancelled.send(()).expect("cancel observer"));
    assert_eq!(
        operation.request_cancel(CancellationReason::Requested),
        TransitionOutcome::Applied
    );
    observed_cancel.recv_timeout(WAIT).expect("owner wake");
    assert_eq!(operation.snapshot().state, OperationState::Running);
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);

    let (settled, observed_settled) = mpsc::sync_channel(1);
    let task = runtime
        .spawn_operation_owner("accepted-owner", owner, move |owner| {
            settled
                .send(owner.settle(
                    SettlementEvidence::EffectAccepted,
                    TerminalCandidate::Success(OperationValue::Unit),
                ))
                .expect("settled");
        })
        .expect("owner task");
    observed_cleanup
        .recv_timeout(WAIT)
        .expect("cleanup started");
    assert_eq!(operation.snapshot().state, OperationState::Running);
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);
    assert_eq!(terminal_events(&events, &operation), 0);
    assert_eq!(
        operation.request_cancel(CancellationReason::Deadline),
        TransitionOutcome::Unchanged
    );
    assert_eq!(
        operation.snapshot().cancellation_reason,
        Some(CancellationReason::Requested)
    );

    release_cleanup.send(()).expect("release cleanup");
    assert_eq!(
        observed_settled.recv_timeout(WAIT),
        Ok(TransitionOutcome::Applied)
    );
    assert_eq!(
        task.join(),
        Ok(easycon_runtime::SupervisedTaskOutcome::Completed)
    );
    let snapshot = operation.snapshot();
    assert_eq!(snapshot.state, OperationState::Succeeded);
    assert_eq!(snapshot.result, Some(OperationValue::Unit));
    assert_eq!(
        snapshot.cancellation_reason,
        Some(CancellationReason::Requested)
    );
    assert_eq!(terminal_events(&events, &operation), 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: operation.non-owner-rejected
#[test]
fn observation_handle_cannot_claim_an_owned_operation() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Exclusive,
            || Ok(()),
            |_| {},
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    assert_eq!(
        operation.succeed(OperationValue::Unit),
        TransitionOutcome::Invalid
    );
    assert_eq!(operation.snapshot().state, OperationState::Running);
    assert_eq!(
        owner.settle(
            SettlementEvidence::EffectAccepted,
            TerminalCandidate::Success(OperationValue::Unit),
        ),
        TransitionOutcome::Applied
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn owned_success_cannot_skip_the_running_state() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Exclusive,
            || Ok(()),
            |_| {},
        )
        .expect("owned operation");

    assert_eq!(
        owner.settle(
            SettlementEvidence::EffectAccepted,
            TerminalCandidate::Success(OperationValue::Unit),
        ),
        TransitionOutcome::Invalid
    );
    assert_eq!(operation.snapshot().state, OperationState::Pending);
    assert!(matches!(
        runtime.close(),
        Ok(CloseOutcome::Failed(report))
            if report.phase == easycon_runtime::ClosePhase::OperationFinalization
    ));
    assert_eq!(operation.snapshot().state, OperationState::Pending);
}

// conformance: operation.transferable-handoff
#[test]
fn close_handoffs_only_preheld_owner_with_shared_settlement_evidence() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_cleanup_calls = Arc::clone(&cleanup_calls);
    let settlement_calls = Arc::new(AtomicUsize::new(0));
    let observed_settlement_calls = Arc::clone(&settlement_calls);
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Transferable,
            move || {
                observed_cleanup_calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            },
            move |_| {
                observed_settlement_calls.fetch_add(1, Ordering::AcqRel);
            },
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    let owner_operation = operation.clone();
    let task = runtime
        .spawn_operation_owner("transferable-owner", owner, move |owner| {
            assert_eq!(
                owner_operation.request_cancel(CancellationReason::Requested),
                TransitionOutcome::Applied
            );
            assert_eq!(
                owner.record_for_handoff(
                    SettlementEvidence::NotDelivered,
                    TerminalCandidate::Cancellation,
                ),
                TransitionOutcome::Applied
            );
        })
        .expect("owner task");
    assert_eq!(
        task.join(),
        Ok(easycon_runtime::SupervisedTaskOutcome::Completed)
    );
    assert_eq!(operation.snapshot().state, OperationState::Running);
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);

    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        operation.snapshot().cancellation_reason,
        Some(CancellationReason::Requested)
    );
    assert_eq!(cleanup_calls.load(Ordering::Acquire), 1);
    assert_eq!(settlement_calls.load(Ordering::Acquire), 1);
}

// conformance: operation.transferable-handoff-after-task-panic
#[test]
fn task_panic_after_shared_evidence_still_settles_before_close_failure() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_cleanup_calls = Arc::clone(&cleanup_calls);
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Transferable,
            move || {
                observed_cleanup_calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            },
            |_| {},
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    let owner_operation = operation.clone();
    let (recorded, observed_recorded) = mpsc::sync_channel(1);
    let task = runtime
        .spawn_operation_owner("panicked-transferable-owner", owner, move |owner| {
            assert_eq!(
                owner_operation.request_cancel(CancellationReason::Requested),
                TransitionOutcome::Applied
            );
            assert_eq!(
                owner.record_for_handoff(
                    SettlementEvidence::NotDelivered,
                    TerminalCandidate::Cancellation,
                ),
                TransitionOutcome::Applied
            );
            recorded.send(()).expect("handoff record observer");
            panic!("scripted owner panic after durable evidence");
        })
        .expect("owner task");
    let task_id = task.id();
    observed_recorded
        .recv_timeout(WAIT)
        .expect("handoff evidence recorded");

    let CloseOutcome::Failed(report) = runtime.close().expect("close outcome") else {
        panic!("task panic cannot report Closed");
    };
    assert_eq!(report.phase, easycon_runtime::ClosePhase::TaskJoin);
    assert_eq!(report.task_id, Some(task_id));
    assert_eq!(
        task.join(),
        Ok(easycon_runtime::SupervisedTaskOutcome::Panicked)
    );
    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        operation.snapshot().cancellation_reason,
        Some(CancellationReason::Requested)
    );
    assert_eq!(cleanup_calls.load(Ordering::Acquire), 1);
    assert_eq!(terminal_events(&events, &operation), 1);
    assert_eq!(runtime.counts().active_operations, 0);
    assert_eq!(runtime.counts().active_tasks, 0);
}

// conformance: operation.task-panic-owner-loss-preserves-registry
#[test]
fn task_panic_without_transfer_or_evidence_preserves_nonterminal_operation() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_cleanup_calls = Arc::clone(&cleanup_calls);
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Exclusive,
            move || {
                observed_cleanup_calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            },
            |_| {},
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    let (started, observed_started) = mpsc::sync_channel(1);
    let task = runtime
        .spawn_operation_owner("panicked-exclusive-owner", owner, move |_owner| {
            started.send(()).expect("owner start observer");
            panic!("scripted owner panic without settlement evidence");
        })
        .expect("owner task");
    let task_id = task.id();
    observed_started.recv_timeout(WAIT).expect("owner started");

    let CloseOutcome::Failed(report) = runtime.close().expect("close outcome") else {
        panic!("task panic cannot report Closed");
    };
    assert_eq!(report.phase, easycon_runtime::ClosePhase::TaskJoin);
    assert_eq!(report.task_id, Some(task_id));
    assert_eq!(
        task.join(),
        Ok(easycon_runtime::SupervisedTaskOutcome::Panicked)
    );
    assert_eq!(operation.snapshot().state, OperationState::Running);
    assert_eq!(
        operation.snapshot().cancellation_reason,
        Some(CancellationReason::ParentClose)
    );
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);
    assert_eq!(cleanup_calls.load(Ordering::Acquire), 0);
    assert_eq!(terminal_events(&events, &operation), 0);
    assert_eq!(runtime.counts().active_operations, 1);
    assert_eq!(runtime.counts().active_tasks, 0);
}

// conformance: operation.ownership-loss-preserves-registry
#[test]
fn close_failed_preserves_nonterminal_operation_when_owner_and_evidence_are_lost() {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let cleanup_calls = Arc::new(AtomicUsize::new(0));
    let observed_cleanup_calls = Arc::clone(&cleanup_calls);
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            None,
            SettlementOwnerMode::Exclusive,
            move || {
                observed_cleanup_calls.fetch_add(1, Ordering::AcqRel);
                Ok(())
            },
            |_| {},
        )
        .expect("owned operation");
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    let task = runtime
        .spawn_operation_owner("lost-owner", owner, |_owner| {})
        .expect("owner task");
    assert_eq!(
        task.join(),
        Ok(easycon_runtime::SupervisedTaskOutcome::Completed)
    );

    let CloseOutcome::Failed(report) = runtime.close().expect("close outcome") else {
        panic!("owner loss cannot report Closed");
    };
    assert_eq!(
        report.phase,
        easycon_runtime::ClosePhase::OperationFinalization
    );
    assert_eq!(operation.snapshot().state, OperationState::Running);
    assert_eq!(
        operation.snapshot().cancellation_reason,
        Some(CancellationReason::ParentClose)
    );
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);
    assert_eq!(cleanup_calls.load(Ordering::Acquire), 0);
    assert_eq!(runtime.counts().active_operations, 1);
    assert_eq!(terminal_events(&events, &operation), 0);
    assert_eq!(
        report.counts,
        RuntimeCounts {
            active_operations: 1,
            active_resources: 0,
            active_tasks: 0,
        }
    );
}
