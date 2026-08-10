#![allow(linker_messages)]
#![cfg(feature = "runtime-model")]

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::task::{Poll, Waker};

use easycon_runtime::runtime_model::{
    CancellationNode, DeadlineResolutionState, TaskLifecycleState, TaskOwnerBinding,
    TerminalArbiterState, TerminalClaimResult, TerminalEvidenceKind, TerminalWinnerKind,
    admit_child_while_locked, cancellation_admission_open, claim_cancellation, invoke_isolated,
    runtime_close_rejected, seal_cancelled_tree, seal_deactivated_tree, task_join_rejected,
    unlink_then_notify,
};
use easycon_runtime::{DeadlineOutcome, DeadlineResolution};
use loom::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

static PANIC_HOOK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_silent_panic_hook(action: impl FnOnce()) {
    let _guard = PANIC_HOOK_LOCK.lock().expect("panic hook lock");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = catch_unwind(AssertUnwindSafe(action));
    std::panic::set_hook(previous);
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

enum ModelHook {
    Panic,
    AssertChildCancelled(Arc<ModelCancellationNode>),
    Count(Arc<AtomicUsize>),
    ReenterStateOnDrop {
        state: Arc<Mutex<bool>>,
        drops: Arc<AtomicUsize>,
    },
}

impl Drop for ModelHook {
    fn drop(&mut self) {
        if let Self::ReenterStateOnDrop { state, drops } = self {
            let _state = state.lock().expect("operation state re-entry");
            drops.fetch_add(1, Ordering::AcqRel);
        }
    }
}

struct ModelCancellationNode {
    active: AtomicBool,
    cancelled: AtomicBool,
    children: Mutex<Vec<Arc<ModelCancellationNode>>>,
    hooks: Mutex<Vec<ModelHook>>,
}

impl ModelCancellationNode {
    fn new() -> Self {
        Self {
            active: AtomicBool::new(true),
            cancelled: AtomicBool::new(false),
            children: Mutex::new(Vec::new()),
            hooks: Mutex::new(Vec::new()),
        }
    }

    fn admit_child(&self) -> Option<Arc<Self>> {
        let child = Arc::new(Self::new());
        let linked_child = Arc::clone(&child);
        let cancelled_child = Arc::clone(&child);
        let mut children = self.children.lock().expect("child registry");
        let admitted = admit_child_while_locked(
            || {
                cancellation_admission_open(
                    self.active.load(Ordering::Acquire),
                    self.cancelled.load(Ordering::Acquire),
                )
            },
            || children.push(linked_child),
            || {
                let mut hooks = Vec::new();
                let mut discarded = Vec::new();
                let mut retained_children = Vec::new();
                seal_cancelled_tree(
                    &*cancelled_child,
                    &mut hooks,
                    &mut discarded,
                    &mut retained_children,
                );
                propagate_model_hooks(hooks, discarded, retained_children);
            },
        );
        admitted.then_some(child)
    }

    fn seal_and_cancel(&self) {
        let mut hooks = Vec::new();
        let mut discarded = Vec::new();
        let mut retained_children = Vec::new();
        seal_deactivated_tree(self, &mut hooks, &mut discarded, &mut retained_children);
        propagate_model_hooks(hooks, discarded, retained_children);
    }
}

impl CancellationNode for ModelCancellationNode {
    type Hook = ModelHook;
    type Child = Arc<Self>;

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn claim_cancel(&self) -> bool {
        claim_cancellation(self.cancelled.swap(true, Ordering::AcqRel))
    }

    fn claim_deactivate(&self) -> bool {
        self.active.swap(false, Ordering::AcqRel)
    }

    fn take_active_hooks(&self, active: &mut Vec<Self::Hook>, _discarded: &mut Vec<Self::Hook>) {
        active.extend(std::mem::take(
            &mut *self.hooks.lock().expect("cancellation hooks"),
        ));
    }

    fn take_discarded_hooks(&self, discarded: &mut Vec<Self::Hook>) {
        discarded.extend(std::mem::take(
            &mut *self.hooks.lock().expect("cancellation hooks"),
        ));
    }

    fn take_live_children(&self) -> Vec<Self::Child> {
        std::mem::take(&mut *self.children.lock().expect("child registry"))
    }
}

fn propagate_model_hooks(
    hooks: Vec<ModelHook>,
    discarded: Vec<ModelHook>,
    retained_children: Vec<Arc<ModelCancellationNode>>,
) {
    drop(discarded);
    for hook in hooks {
        match &hook {
            ModelHook::Panic => {
                let _ = invoke_isolated(|| panic!("scripted cancellation hook panic"));
            }
            ModelHook::AssertChildCancelled(child) => {
                let _ = invoke_isolated(|| assert!(child.cancelled.load(Ordering::Acquire)));
            }
            ModelHook::Count(calls) => {
                let _ = invoke_isolated(|| {
                    calls.fetch_add(1, Ordering::AcqRel);
                });
            }
            ModelHook::ReenterStateOnDrop { .. } => {
                unreachable!("reentrant destructor hooks are discarded, not invoked");
            }
        }
    }
    drop(retained_children);
}

#[test]
fn parent_terminal_linearizes_against_child_admission() {
    loom::model(|| {
        let parent = Arc::new(ModelCancellationNode::new());
        let observed_child = Arc::new(Mutex::new(None));

        let admitting_parent = Arc::clone(&parent);
        let admitting_observation = Arc::clone(&observed_child);
        let admitting = thread::spawn(move || {
            *admitting_observation.lock().expect("child observation") =
                admitting_parent.admit_child();
        });
        let terminal_parent = Arc::clone(&parent);
        let terminal = thread::spawn(move || terminal_parent.seal_and_cancel());

        admitting.join().expect("admitting thread");
        terminal.join().expect("terminal thread");
        if let Some(child) = observed_child.lock().expect("child observation").as_ref() {
            assert!(child.cancelled.load(Ordering::Acquire));
        }
        assert!(parent.admit_child().is_none());
    });
}

#[test]
fn cancellation_hook_panic_does_not_truncate_propagation() {
    with_silent_panic_hook(|| {
        loom::model(|| {
            let root = Arc::new(ModelCancellationNode::new());
            let child = Arc::new(ModelCancellationNode::new());
            let later_hook_calls = Arc::new(AtomicUsize::new(0));
            root.children
                .lock()
                .expect("child registry")
                .push(Arc::clone(&child));
            root.hooks.lock().expect("cancellation hooks").extend([
                ModelHook::Panic,
                ModelHook::AssertChildCancelled(Arc::clone(&child)),
                ModelHook::Count(Arc::clone(&later_hook_calls)),
            ]);

            let callers: Vec<_> = (0..2)
                .map(|_| {
                    let root = Arc::clone(&root);
                    thread::spawn(move || {
                        let mut hooks = Vec::new();
                        let mut discarded = Vec::new();
                        let mut retained_children = Vec::new();
                        seal_cancelled_tree(
                            &*root,
                            &mut hooks,
                            &mut discarded,
                            &mut retained_children,
                        );
                        propagate_model_hooks(hooks, discarded, retained_children);
                    })
                })
                .collect();
            for caller in callers {
                caller.join().expect("cancel caller");
            }

            assert!(root.cancelled.load(Ordering::Acquire));
            assert_eq!(later_hook_calls.load(Ordering::Acquire), 1);
            assert!(child.cancelled.load(Ordering::Acquire));
        });
    });
}

#[test]
fn terminal_deactivation_defers_hook_drop_until_state_unlock() {
    loom::model(|| {
        let state = Arc::new(Mutex::new(false));
        let cancellation = Arc::new(ModelCancellationNode::new());
        let drops = Arc::new(AtomicUsize::new(0));
        cancellation.hooks.lock().expect("cancellation hooks").push(
            ModelHook::ReenterStateOnDrop {
                state: Arc::clone(&state),
                drops: Arc::clone(&drops),
            },
        );

        let terminal_state = Arc::clone(&state);
        let terminal_cancellation = Arc::clone(&cancellation);
        let terminal = thread::spawn(move || {
            let mut state = terminal_state.lock().expect("operation state");
            let mut hooks = Vec::new();
            let mut discarded = Vec::new();
            let mut retained_children = Vec::new();
            seal_deactivated_tree(
                &*terminal_cancellation,
                &mut hooks,
                &mut discarded,
                &mut retained_children,
            );
            assert!(hooks.is_empty());
            drop(state);
            propagate_model_hooks(hooks, discarded, retained_children);
            state = terminal_state.lock().expect("operation state");
            *state = true;
        });

        terminal.join().expect("terminal transaction");
        assert!(*state.lock().expect("operation state"));
        assert_eq!(drops.load(Ordering::Acquire), 1);
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelRuntimeState {
    Active,
    Closing,
    Closed,
}

struct TaskModel {
    state: Mutex<TaskState>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelTaskOutcome {
    Completed,
    Panicked,
}

struct TaskState {
    runtime: ModelRuntimeState,
    lifecycle: TaskLifecycleState<ModelTaskOutcome>,
    handle: Option<thread::JoinHandle<()>>,
    close_attempted: bool,
    body_close_rejected: bool,
    exit_close_rejected: bool,
    exit_join_rejected: bool,
}

impl TaskModel {
    fn new() -> Self {
        Self {
            state: Mutex::new(TaskState {
                runtime: ModelRuntimeState::Active,
                lifecycle: TaskLifecycleState::registered(),
                handle: None,
                close_attempted: false,
                body_close_rejected: false,
                exit_close_rejected: false,
                exit_join_rejected: false,
            }),
            changed: Condvar::new(),
        }
    }
}

fn model_supervised_task_lifecycle(outcome: ModelTaskOutcome) {
    loom::model(move || {
        let model = Arc::new(TaskModel::new());
        let task_model = Arc::clone(&model);
        let task = thread::spawn(move || {
            let owner = TaskOwnerBinding::new(1_usize, 7_usize);
            {
                let mut state = task_model.state.lock().expect("task state");
                while !state.lifecycle.owner_bound() {
                    state = task_model.changed.wait(state).expect("task state");
                }
                let before = state.runtime;
                state.body_close_rejected = runtime_close_rejected(Some(owner), 1);
                assert_eq!(state.runtime, before);
                state.close_attempted = true;
                assert!(state.lifecycle.complete_body(outcome));
                task_model.changed.notify_all();
            }
            thread::yield_now();
            let mut state = task_model.state.lock().expect("task state");
            state.exit_close_rejected = runtime_close_rejected(Some(owner), 1);
            state.exit_join_rejected = task_join_rejected(Some(owner), 1, 7);
            task_model.changed.notify_all();
        });
        {
            let mut state = model.state.lock().expect("task state");
            state.handle = Some(task);
            assert!(state.lifecycle.bind_owner_and_retain_handle());
            model.changed.notify_all();
        }

        let closer_model = Arc::clone(&model);
        let closer = thread::spawn(move || {
            let handle = {
                let mut state = closer_model.state.lock().expect("task state");
                while !state.close_attempted || state.lifecycle.body_outcome().is_none() {
                    state = closer_model.changed.wait(state).expect("task state");
                }
                state.runtime = ModelRuntimeState::Closing;
                assert!(state.lifecycle.claim_join_handle());
                state.handle.take().expect("retained task handle")
            };
            handle
                .join()
                .expect("supervised wrapper catches body panic");

            let mut state = closer_model.state.lock().expect("task state");
            assert!(state.lifecycle.finish_join());
            let panic_diagnostic_required = outcome == ModelTaskOutcome::Panicked;
            if panic_diagnostic_required {
                assert!(!state.lifecycle.unlink_registry(true));
                assert!(state.lifecycle.persist_panic_diagnostic());
            }
            assert!(state.lifecycle.unlink_registry(panic_diagnostic_required));
            state.runtime = ModelRuntimeState::Closed;
            closer_model.changed.notify_all();
        });

        closer.join().expect("external closer");
        let state = model.state.lock().expect("task state");
        assert!(state.lifecycle.owner_bound());
        assert_eq!(state.lifecycle.body_outcome(), Some(outcome));
        assert!(state.body_close_rejected);
        assert!(state.exit_close_rejected);
        assert!(state.exit_join_rejected);
        assert!(state.lifecycle.thread_joined());
        assert!(!state.lifecycle.join_handle_retained());
        assert_eq!(
            state.lifecycle.panic_diagnostic_durable(),
            outcome == ModelTaskOutcome::Panicked
        );
        assert!(!state.lifecycle.registry_linked());
        assert!(state.handle.is_none());
        assert_eq!(state.runtime, ModelRuntimeState::Closed);
    });
}

#[test]
fn supervised_task_self_close_is_rejected_through_thread_exit() {
    model_supervised_task_lifecycle(ModelTaskOutcome::Completed);
}

#[test]
fn supervised_task_panic_is_durable_before_registry_unlink() {
    model_supervised_task_lifecycle(ModelTaskOutcome::Panicked);
}

struct TerminalModel {
    state: Mutex<TerminalState>,
    changed: Condvar,
}

struct TerminalState {
    terminal_committed: bool,
    event_fault_isolated: bool,
    registry_fault_isolated: bool,
    registry_linked: bool,
    notified: bool,
}

impl TerminalModel {
    fn new() -> Self {
        Self {
            state: Mutex::new(TerminalState {
                terminal_committed: false,
                event_fault_isolated: false,
                registry_fault_isolated: false,
                registry_linked: true,
                notified: false,
            }),
            changed: Condvar::new(),
        }
    }

    fn wait_terminal(&self) -> bool {
        let mut state = self.state.lock().expect("terminal state");
        while !state.notified {
            state = self.changed.wait(state).expect("terminal state");
        }
        state.terminal_committed
            && state.event_fault_isolated
            && state.registry_fault_isolated
            && !state.registry_linked
    }
}

#[test]
fn terminal_commit_unlinks_registry_before_waiter_notification() {
    with_silent_panic_hook(|| {
        loom::model(|| {
            let model = Arc::new(TerminalModel::new());
            let waiter_model = Arc::clone(&model);
            let waiter = thread::spawn(move || waiter_model.wait_terminal());

            let terminal_model = Arc::clone(&model);
            let terminal = thread::spawn(move || {
                terminal_model
                    .state
                    .lock()
                    .expect("terminal state")
                    .terminal_committed = true;
                let event_ok = invoke_isolated(|| panic!("scripted terminal event fault"));
                terminal_model
                    .state
                    .lock()
                    .expect("terminal state")
                    .event_fault_isolated = !event_ok;

                let unlink_model = Arc::clone(&terminal_model);
                let notify_model = Arc::clone(&terminal_model);
                unlink_then_notify(
                    move || {
                        let unlink_ok =
                            invoke_isolated(|| panic!("scripted registry unlink fault"));
                        let mut state = unlink_model.state.lock().expect("terminal state");
                        state.registry_fault_isolated = !unlink_ok;
                        state.registry_linked = false;
                    },
                    move || {
                        notify_model.state.lock().expect("terminal state").notified = true;
                        notify_model.changed.notify_all();
                    },
                );
            });

            terminal.join().expect("terminal transaction");
            assert!(waiter.join().expect("terminal waiter"));
        });
    });
}

#[test]
fn deadline_fire_disarm_and_close_resolve_exactly_once() {
    loom::model(|| {
        let state = Arc::new(Mutex::new(DeadlineResolutionState::armed()));
        let wins = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = [
            DeadlineResolution::Fired,
            DeadlineResolution::Disarmed,
            DeadlineResolution::RuntimeClosed,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, resolution)| {
            let state = Arc::clone(&state);
            let wins = Arc::clone(&wins);
            thread::spawn(move || {
                if state
                    .lock()
                    .expect("deadline resolution")
                    .resolve(DeadlineOutcome {
                        resolution,
                        order: u64::try_from(index + 1).expect("model order"),
                    })
                {
                    wins.fetch_add(1, Ordering::AcqRel);
                }
            })
        })
        .collect();
        for worker in workers {
            worker.join().expect("deadline resolver");
        }

        assert_eq!(wins.load(Ordering::Acquire), 1);
        assert!(
            state
                .lock()
                .expect("deadline resolution")
                .outcome()
                .is_some()
        );
    });
}

#[test]
fn deadline_poll_registration_linearizes_against_resolution() {
    loom::model(|| {
        let state = Arc::new(Mutex::new(DeadlineResolutionState::armed()));
        let poll_result = Arc::new(AtomicUsize::new(0));
        let delivered_waker = Arc::new(AtomicBool::new(false));

        let observer_state = Arc::clone(&state);
        let observer_result = Arc::clone(&poll_result);
        let observer = thread::spawn(move || {
            let candidate = Waker::noop().clone();
            let (poll, displaced) = {
                observer_state
                    .lock()
                    .expect("deadline resolution")
                    .poll_resolution_for_model(candidate)
            };
            drop(displaced);
            observer_result.store(
                match poll {
                    Poll::Pending => 1,
                    Poll::Ready(_) => 2,
                },
                Ordering::Release,
            );
        });

        let resolver_state = Arc::clone(&state);
        let resolver_delivered = Arc::clone(&delivered_waker);
        let resolver = thread::spawn(move || {
            let notification = {
                resolver_state
                    .lock()
                    .expect("deadline resolution")
                    .resolve_for_model(DeadlineOutcome {
                        resolution: DeadlineResolution::Fired,
                        order: 1,
                    })
                    .expect("one resolver commits the model deadline")
            };
            if notification.is_some() {
                resolver_delivered.store(true, Ordering::Release);
            }
            drop(notification);
        });

        observer.join().expect("deadline observer");
        resolver.join().expect("deadline resolver");
        let observed = poll_result.load(Ordering::Acquire);
        assert!(
            observed == 2 || delivered_waker.load(Ordering::Acquire),
            "a pending observer must have its registered waker taken by the resolver"
        );
        assert!(
            state
                .lock()
                .expect("deadline resolution")
                .outcome()
                .is_some()
        );
    });
}

#[test]
fn cancellation_intent_does_not_claim_before_accepted_evidence() {
    loom::model(|| {
        let intent = Arc::new(AtomicBool::new(false));
        let arbiter = Arc::new(Mutex::new(TerminalArbiterState::new(1, None)));
        let intent_writer = Arc::clone(&intent);
        let intent_task = thread::spawn(move || {
            intent_writer.store(true, Ordering::Release);
        });
        let owner_arbiter = Arc::clone(&arbiter);
        let owner = thread::spawn(move || {
            let mut state = owner_arbiter.lock().expect("terminal arbiter");
            assert_eq!(
                state.claim(
                    1,
                    TerminalEvidenceKind::EffectAccepted,
                    TerminalWinnerKind::Success,
                ),
                TerminalClaimResult::Claimed
            );
            assert!(state.commit(1, TerminalWinnerKind::Success));
        });
        intent_task.join().expect("intent task");
        owner.join().expect("settlement owner");

        let state = arbiter.lock().expect("terminal arbiter");
        assert!(intent.load(Ordering::Acquire));
        assert_eq!(state.winner(), Some(TerminalWinnerKind::Success));
        assert!(state.committed());
    });
}

#[test]
fn accepted_claim_is_stable_against_late_cancellation() {
    loom::model(|| {
        let shared = Arc::new((
            Mutex::new(TerminalArbiterState::new(1, None)),
            Condvar::new(),
        ));
        let owner_shared = Arc::clone(&shared);
        let owner = thread::spawn(move || {
            let (state, claimed) = &*owner_shared;
            let mut state = state.lock().expect("terminal arbiter");
            assert_eq!(
                state.claim(
                    1,
                    TerminalEvidenceKind::EffectAccepted,
                    TerminalWinnerKind::Success,
                ),
                TerminalClaimResult::Claimed
            );
            claimed.notify_all();
            drop(state);
            thread::yield_now();
            assert!(
                owner_shared
                    .0
                    .lock()
                    .expect("terminal arbiter")
                    .commit(1, TerminalWinnerKind::Success)
            );
        });
        let late_shared = Arc::clone(&shared);
        let late = thread::spawn(move || {
            let (state, claimed) = &*late_shared;
            let mut state = state.lock().expect("terminal arbiter");
            while state.winner().is_none() {
                state = claimed.wait(state).expect("terminal arbiter");
            }
            assert_eq!(
                state.claim(
                    9,
                    TerminalEvidenceKind::NotDelivered,
                    TerminalWinnerKind::Cancellation,
                ),
                TerminalClaimResult::Observe
            );
        });
        owner.join().expect("settlement owner");
        late.join().expect("late cancellation");

        let state = shared.0.lock().expect("terminal arbiter");
        assert_eq!(state.winner(), Some(TerminalWinnerKind::Success));
        assert!(state.committed());
    });

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum DeferredClaimPhase {
        Held,
        Abandoned,
        Finishing,
        Committed,
    }

    struct DeferredClaimModel {
        arbiter: TerminalArbiterState,
        phase: DeferredClaimPhase,
        finishing_observed: bool,
    }

    loom::model(|| {
        let shared = Arc::new((
            Mutex::new(DeferredClaimModel {
                arbiter: TerminalArbiterState::new(1, None),
                phase: DeferredClaimPhase::Held,
                finishing_observed: false,
            }),
            Condvar::new(),
        ));
        {
            let mut state = shared.0.lock().expect("deferred claim model");
            assert_eq!(
                state.arbiter.claim(
                    1,
                    TerminalEvidenceKind::EffectAccepted,
                    TerminalWinnerKind::Success,
                ),
                TerminalClaimResult::Claimed
            );
            // Claim token Drop marks the frozen winner abandoned; it does not commit a result.
            state.phase = DeferredClaimPhase::Abandoned;
        }

        let reclaim_shared = Arc::clone(&shared);
        let reclaim = thread::spawn(move || {
            let (state_lock, changed) = &*reclaim_shared;
            let mut guard = state_lock.lock().expect("deferred claim model");
            assert_eq!(guard.phase, DeferredClaimPhase::Abandoned);
            // The original owner restores this exact frozen winner without a second arbiter claim.
            guard.phase = DeferredClaimPhase::Finishing;
            changed.notify_all();
            while !guard.finishing_observed {
                guard = changed.wait(guard).expect("deferred claim model");
            }

            assert!(guard.arbiter.commit(1, TerminalWinnerKind::Success));
            guard.phase = DeferredClaimPhase::Committed;
            changed.notify_all();
        });
        let (state_lock, changed) = &*shared;
        let mut state = state_lock.lock().expect("deferred claim model");
        while state.phase != DeferredClaimPhase::Finishing {
            state = changed.wait(state).expect("deferred claim model");
        }
        assert_eq!(
            state.arbiter.claim(
                1,
                TerminalEvidenceKind::EffectAccepted,
                TerminalWinnerKind::Success,
            ),
            TerminalClaimResult::Observe
        );
        state.finishing_observed = true;
        changed.notify_all();
        while state.phase != DeferredClaimPhase::Committed {
            state = changed.wait(state).expect("deferred claim model");
        }
        assert_eq!(state.arbiter.winner(), Some(TerminalWinnerKind::Success));
        assert!(state.arbiter.committed());
        drop(state);
        reclaim.join().expect("same-owner reclaim");
    });
}

#[test]
fn terminal_claim_and_commit_remain_unique() {
    loom::model(|| {
        let arbiter = Arc::new(Mutex::new(TerminalArbiterState::new(1, None)));
        let claims = Arc::new(AtomicUsize::new(0));
        let owner_arbiter = Arc::clone(&arbiter);
        let owner_claims = Arc::clone(&claims);
        let owner = thread::spawn(move || {
            let mut state = owner_arbiter.lock().expect("terminal arbiter");
            if state.claim(
                1,
                TerminalEvidenceKind::EffectAccepted,
                TerminalWinnerKind::Success,
            ) == TerminalClaimResult::Claimed
            {
                owner_claims.fetch_add(1, Ordering::AcqRel);
                assert!(state.commit(1, TerminalWinnerKind::Success));
            }
        });
        let stranger_arbiter = Arc::clone(&arbiter);
        let stranger_claims = Arc::clone(&claims);
        let stranger = thread::spawn(move || {
            let mut state = stranger_arbiter.lock().expect("terminal arbiter");
            if state.claim(
                2,
                TerminalEvidenceKind::NotDelivered,
                TerminalWinnerKind::Cancellation,
            ) == TerminalClaimResult::Claimed
            {
                stranger_claims.fetch_add(1, Ordering::AcqRel);
            }
        });
        owner.join().expect("settlement owner");
        stranger.join().expect("non-owner");

        let state = arbiter.lock().expect("terminal arbiter");
        assert_eq!(claims.load(Ordering::Acquire), 1);
        assert_eq!(state.winner(), Some(TerminalWinnerKind::Success));
        assert!(state.committed());
    });
}

#[test]
fn terminal_handoff_requires_join_and_preheld_transfer_owner() {
    loom::model(|| {
        let mut state = TerminalArbiterState::new(1, Some(2));
        assert!(!state.handoff(2));
        assert!(state.mark_primary_owner_joined(1));
        let state = Arc::new(Mutex::new(state));
        let handoffs = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let state = Arc::clone(&state);
                let handoffs = Arc::clone(&handoffs);
                thread::spawn(move || {
                    if state.lock().expect("terminal arbiter").handoff(2) {
                        handoffs.fetch_add(1, Ordering::AcqRel);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("handoff contender");
        }

        let mut state = state.lock().expect("terminal arbiter");
        assert_eq!(handoffs.load(Ordering::Acquire), 1);
        assert_eq!(
            state.claim(
                2,
                TerminalEvidenceKind::NotDelivered,
                TerminalWinnerKind::Cancellation,
            ),
            TerminalClaimResult::Claimed
        );
        assert!(state.commit(2, TerminalWinnerKind::Cancellation));
        assert_eq!(state.winner(), Some(TerminalWinnerKind::Cancellation));
    });
}
