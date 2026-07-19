use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use easycon_model::RuntimeId;

use crate::concurrency::{
    CancellationNode, admit_child_while_locked, cancellation_admission_open, claim_cancellation,
    drop_isolated, invoke_isolated, seal_cancelled_tree, seal_deactivated_tree,
};

type CancelHook = Arc<dyn Fn() + Send + Sync + 'static>;

struct CancelHookEntry {
    lifetime: Option<Weak<()>>,
    hook: CancelHook,
}

impl CancelHookEntry {
    fn is_active(&self) -> bool {
        self.lifetime
            .as_ref()
            .is_none_or(|lifetime| lifetime.strong_count() != 0)
    }
}

/// RAII lifetime for a cancellation hook used only by one blocking call.
pub struct CancellationHookRegistration {
    _lifetime: Arc<()>,
}

pub(crate) struct CancellationPropagation {
    hooks: Vec<CancelHook>,
}

impl CancellationPropagation {
    pub(crate) fn propagate(mut self) {
        for hook in self.hooks.drain(..) {
            invoke_hook(hook);
        }
    }
}

impl Drop for CancellationPropagation {
    fn drop(&mut self) {
        drop_hooks(std::mem::take(&mut self.hooks));
    }
}

/// A node in the Runtime-owned cancellation tree.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

struct CancellationInner {
    owner: Option<RuntimeId>,
    active: AtomicBool,
    cancelled: AtomicBool,
    children: Mutex<Vec<Weak<CancellationInner>>>,
    hooks: Mutex<Vec<CancelHookEntry>>,
}

impl CancellationNode for CancellationInner {
    type Hook = CancelHook;
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

    fn take_active_hooks(&self) -> Vec<Self::Hook> {
        let entries = std::mem::take(&mut *lock_recover(&self.hooks));
        let mut active = Vec::with_capacity(entries.len());
        for entry in entries {
            if entry.is_active() {
                active.push(entry.hook);
            } else {
                drop_hook(entry.hook);
            }
        }
        active
    }

    fn clear_hooks(&self) {
        let entries = std::mem::take(&mut *lock_recover(&self.hooks));
        drop_hook_entries(entries);
    }

    fn take_live_children(&self) -> Vec<Self::Child> {
        let mut children = lock_recover(&self.children);
        let live = children
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|child| child.is_active())
            .collect();
        children.clear();
        live
    }
}

impl CancellationToken {
    /// Creates an uncancelled root token.
    #[must_use]
    pub fn root() -> Self {
        Self::root_with_owner(None)
    }

    pub(crate) fn root_for_runtime(owner: RuntimeId) -> Self {
        Self::root_with_owner(Some(owner))
    }

    fn root_with_owner(owner: Option<RuntimeId>) -> Self {
        Self {
            inner: Arc::new(CancellationInner {
                owner,
                active: AtomicBool::new(true),
                cancelled: AtomicBool::new(false),
                children: Mutex::new(Vec::new()),
                hooks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Creates a child cancelled recursively by this token.
    #[must_use]
    pub fn child(&self) -> Self {
        self.try_child().unwrap_or_else(|| {
            let child = Self::root_with_owner(self.inner.owner);
            child.cancel();
            child
        })
    }

    pub(crate) fn try_child(&self) -> Option<Self> {
        let child = Self::root_with_owner(self.inner.owner);
        let mut children = self
            .inner
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        children.retain(|existing| {
            existing
                .upgrade()
                .is_some_and(|child| child.active.load(Ordering::Acquire))
        });
        let admitted = admit_child_while_locked(
            || {
                cancellation_admission_open(
                    self.inner.active.load(Ordering::Acquire),
                    self.is_cancelled(),
                )
            },
            || children.push(Arc::downgrade(&child.inner)),
            || child.cancel(),
        );
        if !admitted {
            return None;
        }
        drop(children);
        Some(child)
    }

    /// Returns whether this token has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn owner(&self) -> Option<RuntimeId> {
        self.inner.owner
    }

    /// Cancels this token and every live descendant exactly once.
    pub fn cancel(&self) {
        cancel_inner(&self.inner);
    }

    /// Registers a non-blocking hook used to wake supervised work on cancellation.
    pub fn on_cancel(&self, hook: impl Fn() + Send + Sync + 'static) {
        self.register_hook(Arc::new(hook), None);
    }

    /// Registers a wake hook that later cancellations skip after the returned guard is dropped.
    /// A hook already selected by a concurrent cancellation may still finish.
    #[must_use]
    pub fn on_cancel_scoped(
        &self,
        hook: impl Fn() + Send + Sync + 'static,
    ) -> CancellationHookRegistration {
        let lifetime = Arc::new(());
        self.register_hook(Arc::new(hook), Some(Arc::downgrade(&lifetime)));
        CancellationHookRegistration {
            _lifetime: lifetime,
        }
    }

    fn register_hook(&self, hook: CancelHook, lifetime: Option<Weak<()>>) {
        if !self.inner.active.load(Ordering::Acquire) {
            drop_hook(hook);
            return;
        }
        if self.is_cancelled() {
            invoke_hook(hook);
            return;
        }

        let mut hooks = self
            .inner
            .hooks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries = std::mem::take(&mut *hooks);
        let mut inactive = Vec::new();
        for entry in entries {
            if entry.is_active() {
                hooks.push(entry);
            } else {
                inactive.push(entry);
            }
        }
        if !self.inner.active.load(Ordering::Acquire) {
            drop(hooks);
            drop_hook_entries(inactive);
            drop_hook(hook);
            return;
        }
        if self.is_cancelled() {
            drop(hooks);
            drop_hook_entries(inactive);
            invoke_hook(hook);
        } else {
            hooks.push(CancelHookEntry { lifetime, hook });
            drop(hooks);
            drop_hook_entries(inactive);
        }
    }

    pub(crate) fn deactivate_deferred(&self) -> CancellationPropagation {
        let mut hooks = Vec::new();
        seal_deactivated_tree(&*self.inner, &mut hooks);
        CancellationPropagation { hooks }
    }
}

fn cancel_inner(inner: &Arc<CancellationInner>) {
    let mut hooks = Vec::new();
    seal_cancelled_tree(&**inner, &mut hooks);
    CancellationPropagation { hooks }.propagate();
}

impl Drop for CancellationInner {
    fn drop(&mut self) {
        let hooks = self
            .hooks
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop_hook_entries(std::mem::take(hooks));
    }
}

fn invoke_hook(hook: CancelHook) {
    let _ = invoke_isolated(|| hook());
    drop_hook(hook);
}

fn drop_hook(hook: CancelHook) {
    let _ = drop_isolated(hook);
}

fn drop_hooks(hooks: Vec<CancelHook>) {
    for hook in hooks {
        drop_hook(hook);
    }
}

fn drop_hook_entries(entries: Vec<CancelHookEntry>) {
    for entry in entries {
        drop_hook(entry.hook);
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;

    use super::*;

    struct DropPanickingPayload {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropPanickingPayload {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::AcqRel);
            panic!("scripted panic payload drop");
        }
    }

    #[test]
    fn parent_cancel_propagates_and_wakes_once() {
        let root = CancellationToken::root();
        let child = root.child();
        let grandchild = child.child();
        let (sender, receiver) = mpsc::channel();
        grandchild.on_cancel(move || sender.send(()).expect("receiver remains alive"));

        root.cancel();
        root.cancel();

        assert!(root.is_cancelled());
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        assert_eq!(receiver.try_iter().count(), 1);
    }

    #[test]
    fn child_cancel_does_not_cancel_siblings_or_parent() {
        let root = CancellationToken::root();
        let left = root.child();
        let right = root.child();

        left.cancel();

        assert!(left.is_cancelled());
        assert!(!right.is_cancelled());
        assert!(!root.is_cancelled());
    }

    #[test]
    fn parent_cancel_releases_live_child_history_after_propagation() {
        let root = CancellationToken::root();
        let child = root.child();

        root.cancel();

        assert!(child.is_cancelled());
        assert!(
            root.inner
                .children
                .lock()
                .expect("cancellation children")
                .is_empty()
        );
    }

    #[test]
    fn creating_children_prunes_dropped_history() {
        let root = CancellationToken::root();
        for _ in 0..128 {
            drop(root.child());
        }
        let live = root.child();

        assert_eq!(
            root.inner
                .children
                .lock()
                .expect("cancellation child lock")
                .len(),
            1
        );
        assert!(!live.is_cancelled());
    }

    #[test]
    fn dropped_scoped_hook_is_not_called() {
        let token = CancellationToken::root();
        let (called, observed) = mpsc::channel();
        let registration = token.on_cancel_scoped(move || {
            let _ = called.send(());
        });
        drop(registration);

        token.cancel();

        assert_eq!(observed.try_iter().count(), 0);
    }

    #[test]
    fn repeated_scoped_hooks_prune_inactive_history() {
        let token = CancellationToken::root();
        for _ in 0..128 {
            drop(token.on_cancel_scoped(|| {}));
        }
        token.on_cancel(|| {});

        assert_eq!(
            token.inner.hooks.lock().expect("cancellation hooks").len(),
            1
        );
    }

    #[test]
    fn child_created_after_parent_deactivation_is_cancelled_without_history() {
        let root = CancellationToken::root();
        root.deactivate_deferred().propagate();

        let child = root.child();

        assert!(child.is_cancelled());
        assert!(
            root.inner
                .children
                .lock()
                .expect("cancellation children")
                .is_empty()
        );
    }

    // conformance: operation.hook-panic-isolated
    #[test]
    fn hook_panic_does_not_skip_later_hooks_or_children() {
        let root = CancellationToken::root();
        let child = root.child();
        let grandchild = child.child();
        let payload_drops = Arc::new(AtomicUsize::new(0));
        let observed_payload_drops = Arc::clone(&payload_drops);
        root.on_cancel(move || {
            std::panic::panic_any(DropPanickingPayload {
                drops: Arc::clone(&observed_payload_drops),
            });
        });
        let closure_drops = Arc::new(AtomicUsize::new(0));
        let closure_capture = DropPanickingPayload {
            drops: Arc::clone(&closure_drops),
        };
        root.on_cancel(move || {
            let _ = &closure_capture;
        });
        let later_hook_called = Arc::new(AtomicBool::new(false));
        let observed_later_hook = Arc::clone(&later_hook_called);
        root.on_cancel(move || {
            observed_later_hook.store(true, Ordering::Release);
        });
        let child_hook_called = Arc::new(AtomicBool::new(false));
        let observed_child_hook = Arc::clone(&child_hook_called);
        child.on_cancel(move || {
            observed_child_hook.store(true, Ordering::Release);
        });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| root.cancel()));

        assert!(
            result.is_ok(),
            "cancellation hook panic escaped propagation"
        );
        assert!(later_hook_called.load(Ordering::Acquire));
        assert!(child_hook_called.load(Ordering::Acquire));
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        assert_eq!(payload_drops.load(Ordering::Acquire), 1);
        assert_eq!(closure_drops.load(Ordering::Acquire), 1);

        let root = CancellationToken::root();
        let child = root.child();
        let closure_drops = Arc::new(AtomicUsize::new(0));
        let closure_capture = DropPanickingPayload {
            drops: Arc::clone(&closure_drops),
        };
        let hook_called = Arc::new(AtomicBool::new(false));
        let observed_hook = Arc::clone(&hook_called);
        root.on_cancel(move || {
            let _ = &closure_capture;
            observed_hook.store(true, Ordering::Release);
        });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            root.deactivate_deferred().propagate();
        }));

        assert!(result.is_ok(), "hook drop escaped cancellation sealing");
        assert!(!hook_called.load(Ordering::Acquire));
        assert_eq!(closure_drops.load(Ordering::Acquire), 1);
        assert!(child.is_cancelled());
    }

    #[test]
    fn poisoned_cancellation_locks_recover_without_losing_propagation() {
        let root = CancellationToken::root();
        let poisoned = Arc::clone(&root.inner);
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned.hooks.lock().expect("hooks before scripted panic");
                panic!("scripted hook registry poison");
            })
            .join()
            .is_err()
        );
        let hook_called = Arc::new(AtomicBool::new(false));
        let observed_hook = Arc::clone(&hook_called);
        let registration = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            root.on_cancel(move || observed_hook.store(true, Ordering::Release));
        }));
        assert!(registration.is_ok(), "poisoned hook registry escaped");

        let poisoned = Arc::clone(&root.inner);
        assert!(
            std::thread::spawn(move || {
                let _guard = poisoned
                    .children
                    .lock()
                    .expect("children before scripted panic");
                panic!("scripted child registry poison");
            })
            .join()
            .is_err()
        );
        let child = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| root.child()))
            .expect("poisoned child registry escaped");

        root.cancel();

        assert!(hook_called.load(Ordering::Acquire));
        assert!(child.is_cancelled());
    }
}
