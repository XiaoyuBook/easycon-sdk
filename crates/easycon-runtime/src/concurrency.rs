use std::ops::Deref;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub trait CancellationNode: Sized {
    type Hook;
    type Child: Deref<Target = Self>;

    fn is_active(&self) -> bool;
    fn claim_cancel(&self) -> bool;
    fn claim_deactivate(&self) -> bool;
    fn take_active_hooks(&self) -> Vec<Self::Hook>;
    fn clear_hooks(&self);
    fn take_live_children(&self) -> Vec<Self::Child>;
}

pub fn seal_cancelled_tree<N: CancellationNode>(node: &N, hooks: &mut Vec<N::Hook>) {
    if !node.is_active() || !node.claim_cancel() {
        return;
    }
    hooks.extend(node.take_active_hooks());
    for child in node.take_live_children() {
        seal_cancelled_tree(&*child, hooks);
    }
}

pub fn seal_deactivated_tree<N: CancellationNode>(node: &N, hooks: &mut Vec<N::Hook>) {
    if !node.claim_deactivate() {
        return;
    }
    node.clear_hooks();
    for child in node.take_live_children() {
        seal_cancelled_tree(&*child, hooks);
    }
}

pub fn cancellation_admission_open(active: bool, cancelled: bool) -> bool {
    active && !cancelled
}

pub fn admit_child_while_locked(
    mut admission_open: impl FnMut() -> bool,
    link_child: impl FnOnce(),
    cancel_child: impl FnOnce(),
) -> bool {
    if !admission_open() {
        return false;
    }
    link_child();
    if !admission_open() {
        cancel_child();
    }
    true
}

pub const fn claim_cancellation(already_cancelled: bool) -> bool {
    !already_cancelled
}

pub fn contain_panic<T>(result: std::thread::Result<T>) -> Result<T, ()> {
    match result {
        Ok(value) => Ok(value),
        Err(payload) => {
            if let Err(secondary) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                // Dropping a user-controlled secondary payload could start an unbounded panic loop.
                std::mem::forget(secondary);
            }
            Err(())
        }
    }
}

pub fn catch_isolated<T>(action: impl FnOnce() -> T) -> Result<T, ()> {
    contain_panic(catch_unwind(AssertUnwindSafe(action)))
}

pub fn invoke_isolated(action: impl FnOnce()) -> bool {
    catch_isolated(action).is_ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskOwnerBinding<R, T> {
    runtime: R,
    task: T,
}

impl<R, T> TaskOwnerBinding<R, T> {
    pub const fn new(runtime: R, task: T) -> Self {
        Self { runtime, task }
    }
}

impl<R: PartialEq, T: PartialEq> TaskOwnerBinding<R, T> {
    pub fn rejects_runtime_close(&self, runtime: &R) -> bool {
        self.runtime == *runtime
    }

    pub fn rejects_task_join(&self, runtime: &R, task: &T) -> bool {
        self.runtime == *runtime && self.task == *task
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskHandleState {
    Missing,
    Retained,
    Joining,
    Joined,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLifecycleState<O> {
    owner_bound: bool,
    body_outcome: Option<O>,
    handle: TaskHandleState,
    panic_diagnostic_durable: bool,
    registry_linked: bool,
}

impl<O: Copy> TaskLifecycleState<O> {
    pub const fn registered() -> Self {
        Self {
            owner_bound: false,
            body_outcome: None,
            handle: TaskHandleState::Missing,
            panic_diagnostic_durable: false,
            registry_linked: true,
        }
    }

    pub fn bind_owner_and_retain_handle(&mut self) -> bool {
        if !self.registry_linked
            || self.owner_bound
            || self.body_outcome.is_some()
            || self.handle != TaskHandleState::Missing
        {
            return false;
        }
        self.owner_bound = true;
        self.handle = TaskHandleState::Retained;
        true
    }

    pub fn complete_body(&mut self, outcome: O) -> bool {
        if !self.registry_linked
            || !self.owner_bound
            || self.body_outcome.is_some()
            || self.handle != TaskHandleState::Retained
        {
            return false;
        }
        self.body_outcome = Some(outcome);
        true
    }

    pub fn claim_join_handle(&mut self) -> bool {
        if !self.registry_linked
            || self.body_outcome.is_none()
            || self.handle != TaskHandleState::Retained
        {
            return false;
        }
        self.handle = TaskHandleState::Joining;
        true
    }

    pub fn finish_join(&mut self) -> bool {
        if !self.registry_linked
            || self.body_outcome.is_none()
            || self.handle != TaskHandleState::Joining
        {
            return false;
        }
        self.handle = TaskHandleState::Joined;
        true
    }

    pub fn persist_panic_diagnostic(&mut self) -> bool {
        if !self.registry_linked
            || self.handle != TaskHandleState::Joined
            || self.panic_diagnostic_durable
        {
            return false;
        }
        self.panic_diagnostic_durable = true;
        true
    }

    pub fn unlink_registry(&mut self, panic_diagnostic_required: bool) -> bool {
        if !self.registry_linked
            || self.handle != TaskHandleState::Joined
            || (panic_diagnostic_required && !self.panic_diagnostic_durable)
        {
            return false;
        }
        self.registry_linked = false;
        true
    }

    pub fn abort_spawn(&mut self) -> bool {
        if !self.registry_linked
            || self.owner_bound
            || self.body_outcome.is_some()
            || self.handle != TaskHandleState::Missing
        {
            return false;
        }
        self.registry_linked = false;
        true
    }

    #[cfg(feature = "runtime-model")]
    pub const fn owner_bound(&self) -> bool {
        self.owner_bound
    }

    pub const fn body_outcome(&self) -> Option<O> {
        self.body_outcome
    }

    #[cfg(feature = "runtime-model")]
    pub const fn join_handle_retained(&self) -> bool {
        matches!(self.handle, TaskHandleState::Retained)
    }

    #[cfg(feature = "runtime-model")]
    pub const fn thread_joined(&self) -> bool {
        matches!(self.handle, TaskHandleState::Joined)
    }

    #[cfg(feature = "runtime-model")]
    pub const fn panic_diagnostic_durable(&self) -> bool {
        self.panic_diagnostic_durable
    }

    #[cfg(feature = "runtime-model")]
    pub const fn registry_linked(&self) -> bool {
        self.registry_linked
    }
}

pub fn runtime_close_rejected<R: PartialEq, T: PartialEq>(
    current: Option<TaskOwnerBinding<R, T>>,
    runtime: R,
) -> bool {
    current.is_some_and(|owner| owner.rejects_runtime_close(&runtime))
}

pub fn task_join_rejected<R: PartialEq, T: PartialEq>(
    current: Option<TaskOwnerBinding<R, T>>,
    runtime: R,
    task: T,
) -> bool {
    current.is_some_and(|owner| owner.rejects_task_join(&runtime, &task))
}

pub fn unlink_then_notify(unlink: impl FnOnce(), notify: impl FnOnce()) {
    let _ = invoke_isolated(unlink);
    notify();
}
