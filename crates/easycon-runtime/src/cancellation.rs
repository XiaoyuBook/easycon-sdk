use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

type CancelHook = Arc<dyn Fn() + Send + Sync + 'static>;

/// A node in the Runtime-owned cancellation tree.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

struct CancellationInner {
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
        self.inner
            .children
            .lock()
            .expect("cancellation child lock poisoned")
            .push(Arc::downgrade(&child.inner));
        if self.is_cancelled() {
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
        if self.is_cancelled() {
            hook();
            return;
        }

        let mut hooks = self
            .inner
            .hooks
            .lock()
            .expect("cancellation hook lock poisoned");
        if self.is_cancelled() {
            drop(hooks);
            hook();
        } else {
            hooks.push(hook);
        }
    }
}

fn cancel_inner(inner: &Arc<CancellationInner>) {
    if inner.cancelled.swap(true, Ordering::AcqRel) {
        return;
    }

    let hooks = inner
        .hooks
        .lock()
        .expect("cancellation hook lock poisoned")
        .clone();
    for hook in hooks {
        hook();
    }

    let children = inner
        .children
        .lock()
        .expect("cancellation child lock poisoned")
        .clone();
    for child in children.into_iter().filter_map(|child| child.upgrade()) {
        cancel_inner(&child);
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
}
