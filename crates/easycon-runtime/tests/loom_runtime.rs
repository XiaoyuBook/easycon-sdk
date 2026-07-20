#![allow(linker_messages)]
#![cfg(feature = "runtime-model")]

use std::panic::{AssertUnwindSafe, catch_unwind};

use easycon_runtime::runtime_model::{
    CancellationNode, TaskLifecycleState, TaskOwnerBinding, admit_child_while_locked,
    cancellation_admission_open, claim_cancellation, invoke_isolated, runtime_close_rejected,
    seal_cancelled_tree, seal_deactivated_tree, task_join_rejected, unlink_then_notify,
};
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
