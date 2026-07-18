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

pub fn invoke_isolated(action: impl FnOnce()) -> bool {
    catch_unwind(AssertUnwindSafe(action)).is_ok()
}

pub fn runtime_close_rejected<R: PartialEq, T>(current: Option<(R, T)>, runtime: R) -> bool {
    matches!(current, Some((owner, _)) if owner == runtime)
}

pub fn task_join_rejected<R: PartialEq, T: PartialEq>(
    current: Option<(R, T)>,
    runtime: R,
    task: T,
) -> bool {
    current == Some((runtime, task))
}

pub fn unlink_then_notify(unlink: impl FnOnce(), notify: impl FnOnce()) {
    let _ = invoke_isolated(unlink);
    notify();
}
