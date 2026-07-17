use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

type CancelHook = Arc<dyn Fn() + Send + Sync + 'static>;

/// A node in the Runtime-owned cancellation tree.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

struct CancellationInner {
    active: AtomicBool,
    cancelled: AtomicBool,
    children: Mutex<Vec<Weak<CancellationInner>>>,
    hooks: Mutex<Vec<CancelHook>>,
}

impl CancellationToken {
    /// Creates an uncancelled root token.
    #[must_use]
    pub fn root() -> Self {
        Self {
            inner: Arc::new(CancellationInner {
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
        let child = Self::root();
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

    /// Cancels this token and every live descendant exactly once.
    pub fn cancel(&self) {
        cancel_inner(&self.inner);
    }

    /// Registers a non-blocking hook used to wake supervised work on cancellation.
    pub fn on_cancel(&self, hook: impl Fn() + Send + Sync + 'static) {
        let hook: CancelHook = Arc::new(hook);
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
        if !self.inner.active.load(Ordering::Acquire) {
            return;
        }
        if self.is_cancelled() {
            drop(hooks);
            hook();
        } else {
            hooks.push(hook);
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
    for hook in hooks {
        hook();
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
}
