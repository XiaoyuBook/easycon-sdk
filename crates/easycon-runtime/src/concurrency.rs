use std::ops::Deref;
use std::panic::{AssertUnwindSafe, catch_unwind};

pub trait CancellationNode: Sized {
    type Hook;
    type Child: Deref<Target = Self>;

    fn is_active(&self) -> bool;
    fn claim_cancel(&self) -> bool;
    fn claim_deactivate(&self) -> bool;
    fn take_active_hooks(&self, active: &mut Vec<Self::Hook>, discarded: &mut Vec<Self::Hook>);
    fn take_discarded_hooks(&self, discarded: &mut Vec<Self::Hook>);
    fn take_live_children(&self) -> Vec<Self::Child>;
}

pub fn seal_cancelled_tree<N: CancellationNode>(
    node: &N,
    hooks: &mut Vec<N::Hook>,
    discarded: &mut Vec<N::Hook>,
    retained_children: &mut Vec<N::Child>,
) {
    if !node.is_active() || !node.claim_cancel() {
        return;
    }
    node.take_active_hooks(hooks, discarded);
    for child in node.take_live_children() {
        seal_cancelled_tree(&*child, hooks, discarded, retained_children);
        retained_children.push(child);
    }
}

pub fn seal_deactivated_tree<N: CancellationNode>(
    node: &N,
    hooks: &mut Vec<N::Hook>,
    discarded: &mut Vec<N::Hook>,
    retained_children: &mut Vec<N::Child>,
) {
    if !node.claim_deactivate() {
        return;
    }
    node.take_discarded_hooks(discarded);
    for child in node.take_live_children() {
        seal_cancelled_tree(&*child, hooks, discarded, retained_children);
        retained_children.push(child);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalEvidenceKind {
    EffectAccepted,
    NotDelivered,
    ExecutionFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalWinnerKind {
    Success,
    Failure,
    Cancellation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalClaimResult {
    Claimed,
    Observe,
    RejectedOwner,
    RejectedEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalArbiterState {
    primary_owner: u64,
    transfer_owner: Option<u64>,
    active_owner: u64,
    primary_owner_joined: bool,
    handoff_used: bool,
    winner: Option<TerminalWinnerKind>,
    committed: bool,
}

impl TerminalArbiterState {
    pub const fn new(primary_owner: u64, transfer_owner: Option<u64>) -> Self {
        Self {
            primary_owner,
            transfer_owner,
            active_owner: primary_owner,
            primary_owner_joined: false,
            handoff_used: false,
            winner: None,
            committed: false,
        }
    }

    pub fn claim(
        &mut self,
        owner: u64,
        evidence: TerminalEvidenceKind,
        winner: TerminalWinnerKind,
    ) -> TerminalClaimResult {
        if self.winner.is_some() || self.committed {
            return TerminalClaimResult::Observe;
        }
        if owner != self.active_owner {
            return TerminalClaimResult::RejectedOwner;
        }
        if !evidence_allows_winner(evidence, winner) {
            return TerminalClaimResult::RejectedEvidence;
        }
        self.winner = Some(winner);
        TerminalClaimResult::Claimed
    }

    pub fn mark_primary_owner_joined(&mut self, owner: u64) -> bool {
        if owner != self.primary_owner || self.primary_owner_joined {
            return false;
        }
        self.primary_owner_joined = true;
        true
    }

    pub fn handoff(&mut self, transfer_owner: u64) -> bool {
        if self.committed
            || self.winner.is_some()
            || self.handoff_used
            || !self.primary_owner_joined
            || self.transfer_owner != Some(transfer_owner)
            || self.active_owner != self.primary_owner
        {
            return false;
        }
        self.active_owner = transfer_owner;
        self.handoff_used = true;
        true
    }

    pub fn commit(&mut self, owner: u64, winner: TerminalWinnerKind) -> bool {
        if self.committed || self.active_owner != owner || self.winner != Some(winner) {
            return false;
        }
        self.committed = true;
        true
    }

    pub const fn winner(&self) -> Option<TerminalWinnerKind> {
        self.winner
    }

    pub const fn committed(&self) -> bool {
        self.committed
    }

    pub const fn primary_owner_joined(&self) -> bool {
        self.primary_owner_joined
    }

    pub const fn transfer_owner(&self) -> Option<u64> {
        self.transfer_owner
    }

    pub const fn primary_owner(&self) -> u64 {
        self.primary_owner
    }
}

pub const fn evidence_allows_winner(
    evidence: TerminalEvidenceKind,
    winner: TerminalWinnerKind,
) -> bool {
    matches!(
        (evidence, winner),
        (
            TerminalEvidenceKind::EffectAccepted,
            TerminalWinnerKind::Success
        ) | (
            TerminalEvidenceKind::NotDelivered,
            TerminalWinnerKind::Cancellation
        ) | (
            TerminalEvidenceKind::ExecutionFailed,
            TerminalWinnerKind::Failure
        )
    )
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

pub fn drop_isolated<T>(value: T) -> bool {
    catch_isolated(|| drop(value)).is_ok()
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
