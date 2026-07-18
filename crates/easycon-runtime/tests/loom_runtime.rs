#![allow(linker_messages)]

use std::panic::{AssertUnwindSafe, catch_unwind};

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

struct ParentNode {
    state: Mutex<ParentState>,
}

struct ParentState {
    sealed: bool,
    children: Vec<Arc<AtomicBool>>,
}

impl ParentNode {
    fn new() -> Self {
        Self {
            state: Mutex::new(ParentState {
                sealed: false,
                children: Vec::new(),
            }),
        }
    }

    fn admit_child(&self) -> Option<Arc<AtomicBool>> {
        let mut state = self.state.lock().expect("parent state");
        if state.sealed {
            return None;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        state.children.push(Arc::clone(&cancelled));
        Some(cancelled)
    }

    fn seal_and_cancel(&self) {
        let children = {
            let mut state = self.state.lock().expect("parent state");
            state.sealed = true;
            std::mem::take(&mut state.children)
        };
        for child in children {
            child.store(true, Ordering::Release);
        }
    }
}

#[test]
fn parent_terminal_linearizes_against_child_admission() {
    loom::model(|| {
        let parent = Arc::new(ParentNode::new());
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
            assert!(child.load(Ordering::Acquire));
        }
        assert!(parent.admit_child().is_none());
    });
}

fn cancel_with_isolated_hooks(
    cancelled: &AtomicBool,
    later_hook_calls: &AtomicUsize,
    child_cancelled: &AtomicBool,
) {
    if cancelled.swap(true, Ordering::AcqRel) {
        return;
    }
    let hooks: [Box<dyn Fn() + Send>; 2] = [
        Box::new(|| panic!("scripted cancellation hook panic")),
        Box::new(|| {
            later_hook_calls.fetch_add(1, Ordering::AcqRel);
        }),
    ];
    for hook in hooks {
        let _ = catch_unwind(AssertUnwindSafe(hook));
    }
    child_cancelled.store(true, Ordering::Release);
}

#[test]
fn cancellation_hook_panic_does_not_truncate_propagation() {
    with_silent_panic_hook(|| {
        loom::model(|| {
            let cancelled = Arc::new(AtomicBool::new(false));
            let later_hook_calls = Arc::new(AtomicUsize::new(0));
            let child_cancelled = Arc::new(AtomicBool::new(false));

            let callers: Vec<_> = (0..2)
                .map(|_| {
                    let cancelled = Arc::clone(&cancelled);
                    let later_hook_calls = Arc::clone(&later_hook_calls);
                    let child_cancelled = Arc::clone(&child_cancelled);
                    thread::spawn(move || {
                        cancel_with_isolated_hooks(&cancelled, &later_hook_calls, &child_cancelled);
                    })
                })
                .collect();
            for caller in callers {
                caller.join().expect("cancel caller");
            }

            assert!(cancelled.load(Ordering::Acquire));
            assert_eq!(later_hook_calls.load(Ordering::Acquire), 1);
            assert!(child_cancelled.load(Ordering::Acquire));
        });
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

struct TaskState {
    runtime: ModelRuntimeState,
    task_active: bool,
    close_attempted: bool,
    self_close_rejected: bool,
    join_handle_retained: bool,
}

impl TaskModel {
    fn new() -> Self {
        Self {
            state: Mutex::new(TaskState {
                runtime: ModelRuntimeState::Active,
                task_active: true,
                close_attempted: false,
                self_close_rejected: false,
                join_handle_retained: true,
            }),
            changed: Condvar::new(),
        }
    }
}

#[test]
fn supervised_task_self_close_is_rejected_before_external_close() {
    loom::model(|| {
        let model = Arc::new(TaskModel::new());
        let task_model = Arc::clone(&model);
        let task = thread::spawn(move || {
            {
                let mut state = task_model.state.lock().expect("task state");
                let before = state.runtime;
                state.self_close_rejected = true;
                assert_eq!(state.runtime, before);
                state.close_attempted = true;
                task_model.changed.notify_all();
            }
            let mut state = task_model.state.lock().expect("task state");
            state.task_active = false;
            task_model.changed.notify_all();
        });

        let closer_model = Arc::clone(&model);
        let closer = thread::spawn(move || {
            let mut state = closer_model.state.lock().expect("task state");
            while !state.close_attempted {
                state = closer_model.changed.wait(state).expect("task state");
            }
            state.runtime = ModelRuntimeState::Closing;
            while state.task_active {
                state = closer_model.changed.wait(state).expect("task state");
            }
            state.join_handle_retained = false;
            state.runtime = ModelRuntimeState::Closed;
            closer_model.changed.notify_all();
        });

        task.join().expect("supervised task");
        closer.join().expect("external closer");
        let state = model.state.lock().expect("task state");
        assert!(state.self_close_rejected);
        assert!(!state.task_active);
        assert!(!state.join_handle_retained);
        assert_eq!(state.runtime, ModelRuntimeState::Closed);
    });
}

struct TerminalModel {
    state: Mutex<TerminalState>,
    changed: Condvar,
}

struct TerminalState {
    child_admission_open: bool,
    owner_cleanup_finished: bool,
    terminal_committed: bool,
    event_fault_isolated: bool,
    registry_fault_isolated: bool,
    registry_linked: bool,
}

impl TerminalModel {
    fn new() -> Self {
        Self {
            state: Mutex::new(TerminalState {
                child_admission_open: true,
                owner_cleanup_finished: false,
                terminal_committed: false,
                event_fault_isolated: false,
                registry_fault_isolated: false,
                registry_linked: true,
            }),
            changed: Condvar::new(),
        }
    }

    fn commit_with_faults(&self) {
        let mut state = self.state.lock().expect("terminal state");
        state.child_admission_open = false;
        state.owner_cleanup_finished = true;
        state.terminal_committed = true;
        state.event_fault_isolated = Result::<(), ()>::Err(()).is_err();
        state.registry_fault_isolated = Result::<(), ()>::Err(()).is_err();
        state.registry_linked = false;
        self.changed.notify_all();
    }

    fn wait_terminal(&self) -> bool {
        let mut state = self.state.lock().expect("terminal state");
        while !state.terminal_committed {
            state = self.changed.wait(state).expect("terminal state");
        }
        state.owner_cleanup_finished
            && state.event_fault_isolated
            && state.registry_fault_isolated
            && !state.registry_linked
            && !state.child_admission_open
    }
}

#[test]
fn terminal_commit_unlinks_registry_before_waiter_notification() {
    loom::model(|| {
        let model = Arc::new(TerminalModel::new());
        let waiter_model = Arc::clone(&model);
        let waiter = thread::spawn(move || waiter_model.wait_terminal());
        let terminal_model = Arc::clone(&model);
        let terminal = thread::spawn(move || terminal_model.commit_with_faults());

        terminal.join().expect("terminal transaction");
        assert!(waiter.join().expect("terminal waiter"));
    });
}
