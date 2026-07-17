use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use easycon_model::RuntimeId;

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
        let child = Self::root_with_owner(self.inner.owner);
        let mut children = self
            .inner
            .children
            .lock()
            .expect("cancellation child lock poisoned");
        children.retain(|existing| {
            existing
                .upgrade()
                .is_some_and(|child| child.active.load(Ordering::Acquire))
        });
        children.push(Arc::downgrade(&child.inner));
        drop(children);
        if !self.inner.active.load(Ordering::Acquire) {
            child.deactivate();
        } else if self.is_cancelled() {
            child.cancel();
        }
        child
    }

    /// Returns whether this token has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::Acquire)
    }

    pub(crate) fn owner(&self) -> Option<RuntimeId> {
        self.inner.owner
    }

    pub(crate) fn is_active(&self) -> bool {
        self.inner.active.load(Ordering::Acquire)
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
            return;
        }
        if self.is_cancelled() {
            hook();
            return;
        }

        let mut hooks = self
            .inner
            .hooks
            .lock()
            .expect("cancellation hook lock poisoned");
        hooks.retain(CancelHookEntry::is_active);
        if !self.inner.active.load(Ordering::Acquire) {
            return;
        }
        if self.is_cancelled() {
            drop(hooks);
            hook();
        } else {
            hooks.push(CancelHookEntry { lifetime, hook });
        }
    }

    pub(crate) fn deactivate(&self) {
        deactivate_inner(&self.inner);
    }
}

fn cancel_inner(inner: &Arc<CancellationInner>) {
    if !inner.active.load(Ordering::Acquire) {
        return;
    }
    if inner.cancelled.swap(true, Ordering::AcqRel) {
        return;
    }

    let hooks = std::mem::take(&mut *inner.hooks.lock().expect("cancellation hook lock poisoned"));
    for entry in hooks.into_iter().filter(CancelHookEntry::is_active) {
        (entry.hook)();
    }

    let children = {
        let mut children = inner
            .children
            .lock()
            .expect("cancellation child lock poisoned");
        let live: Vec<_> = children
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|child| child.active.load(Ordering::Acquire))
            .collect();
        children.clear();
        children.extend(live.iter().map(Arc::downgrade));
        live
    };
    for child in children {
        cancel_inner(&child);
    }
}

fn deactivate_inner(inner: &Arc<CancellationInner>) {
    if !inner.active.swap(false, Ordering::AcqRel) {
        return;
    }
    inner
        .hooks
        .lock()
        .expect("cancellation hook lock poisoned")
        .clear();
    let children: Vec<_> = inner
        .children
        .lock()
        .expect("cancellation child lock poisoned")
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    for child in children {
        deactivate_inner(&child);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

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
}
