use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Identifier for one registered monotonic deadline.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeadlineId(u64);

impl DeadlineId {
    /// Returns the numeric identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Deterministic trace of one registered deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineTrace {
    /// Registration order within the clock.
    pub id: DeadlineId,
    /// Absolute monotonic target.
    pub target_ns: u64,
    /// Time at which a consumer recorded dispatch, if any.
    pub dispatched_at_ns: Option<u64>,
    /// Whether a virtual advance crossed this target.
    pub woken: bool,
}

/// Monotonic time source used by runtime and controller scheduling.
pub trait Clock: Send + Sync + 'static {
    /// Returns nanoseconds since this clock's private monotonic epoch.
    fn now_ns(&self) -> u64;

    /// Subscribes to explicit clock changes. Virtual clocks notify on every advance.
    fn subscribe(&self) -> Receiver<()>;

    /// Registers a non-blocking hook invoked after an explicit clock change.
    fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>);

    /// Registers an absolute deadline for deterministic trace inspection.
    fn register_deadline(&self, target_ns: u64) -> DeadlineId;

    /// Records actual dispatch for a previously registered deadline.
    fn record_dispatch(&self, id: DeadlineId, actual_ns: u64);

    /// Returns a real wait duration for a target, or `None` for manually advanced clocks.
    fn real_wait_duration(&self, target_ns: u64) -> Option<Duration>;
}

/// A manually advanced monotonic clock for deterministic tests and fake backends.
pub struct VirtualClock {
    now_ns: AtomicU64,
    next_deadline: AtomicU64,
    state: Mutex<VirtualClockState>,
}

#[derive(Default)]
struct VirtualClockState {
    listeners: Vec<SyncSender<()>>,
    hooks: Vec<Arc<dyn Fn() + Send + Sync>>,
    deadlines: Vec<DeadlineTrace>,
    wake_order: Vec<DeadlineId>,
}

impl VirtualClock {
    /// Creates a virtual clock at the supplied monotonic instant.
    #[must_use]
    pub fn new(now_ns: u64) -> Self {
        Self {
            now_ns: AtomicU64::new(now_ns),
            next_deadline: AtomicU64::new(1),
            state: Mutex::new(VirtualClockState::default()),
        }
    }

    /// Advances to an absolute instant and wakes registered listeners.
    ///
    /// # Panics
    ///
    /// Panics if `target_ns` is earlier than the current monotonic time.
    pub fn advance_to(&self, target_ns: u64) {
        let previous = self.now_ns.fetch_max(target_ns, Ordering::AcqRel);
        assert!(
            target_ns >= previous,
            "virtual monotonic time cannot move backwards"
        );

        let mut state = self.state.lock().expect("virtual clock lock poisoned");
        let newly_woken: Vec<_> = state
            .deadlines
            .iter_mut()
            .filter(|deadline| !deadline.woken && deadline.target_ns <= target_ns)
            .map(|deadline| {
                deadline.woken = true;
                deadline.id
            })
            .collect();
        state.wake_order.extend(newly_woken);
        state
            .listeners
            .retain(|listener| match listener.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) => true,
                Err(TrySendError::Disconnected(())) => false,
            });
        let hooks = state.hooks.clone();
        drop(state);
        for hook in hooks {
            hook();
        }
    }

    /// Advances by a checked duration.
    ///
    /// # Panics
    ///
    /// Panics if the new timestamp would overflow `u64`.
    pub fn advance_by(&self, duration: Duration) {
        let delta = u64::try_from(duration.as_nanos()).expect("duration exceeds u64 nanoseconds");
        let target = self
            .now_ns()
            .checked_add(delta)
            .expect("virtual monotonic timestamp overflow");
        self.advance_to(target);
    }

    /// Returns deadline records in registration order.
    #[must_use]
    pub fn deadline_trace(&self) -> Vec<DeadlineTrace> {
        self.state
            .lock()
            .expect("virtual clock lock poisoned")
            .deadlines
            .clone()
    }

    /// Returns deadline identifiers in deterministic wake order.
    #[must_use]
    pub fn wake_order(&self) -> Vec<DeadlineId> {
        self.state
            .lock()
            .expect("virtual clock lock poisoned")
            .wake_order
            .clone()
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Clock for VirtualClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.load(Ordering::Acquire)
    }

    fn subscribe(&self) -> Receiver<()> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.state
            .lock()
            .expect("virtual clock lock poisoned")
            .listeners
            .push(sender);
        receiver
    }

    fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.state
            .lock()
            .expect("virtual clock lock poisoned")
            .hooks
            .push(hook);
    }

    fn register_deadline(&self, target_ns: u64) -> DeadlineId {
        let id = DeadlineId(self.next_deadline.fetch_add(1, Ordering::Relaxed));
        self.state
            .lock()
            .expect("virtual clock lock poisoned")
            .deadlines
            .push(DeadlineTrace {
                id,
                target_ns,
                dispatched_at_ns: None,
                woken: target_ns <= self.now_ns(),
            });
        id
    }

    fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
        if let Some(deadline) = self
            .state
            .lock()
            .expect("virtual clock lock poisoned")
            .deadlines
            .iter_mut()
            .find(|deadline| deadline.id == id)
        {
            deadline.dispatched_at_ns = Some(actual_ns);
        }
    }

    fn real_wait_duration(&self, _target_ns: u64) -> Option<Duration> {
        None
    }
}

/// Production monotonic clock backed by [`Instant`].
pub struct SystemClock {
    epoch: Instant,
    next_deadline: AtomicU64,
    listeners: Mutex<Vec<SyncSender<()>>>,
    hooks: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
}

impl SystemClock {
    /// Creates a new private monotonic epoch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            next_deadline: AtomicU64::new(1),
            listeners: Mutex::new(Vec::new()),
            hooks: Mutex::new(Vec::new()),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    fn subscribe(&self) -> Receiver<()> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.listeners
            .lock()
            .expect("system clock listener lock poisoned")
            .push(sender);
        receiver
    }

    fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        self.hooks
            .lock()
            .expect("system clock hook lock poisoned")
            .push(hook);
    }

    fn register_deadline(&self, _target_ns: u64) -> DeadlineId {
        DeadlineId(self.next_deadline.fetch_add(1, Ordering::Relaxed))
    }

    fn record_dispatch(&self, _id: DeadlineId, _actual_ns: u64) {}

    fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
        Some(Duration::from_nanos(
            target_ns.saturating_sub(self.now_ns()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_records_deadlines_wakes_and_dispatch() {
        let clock = VirtualClock::new(10);
        let later = clock.register_deadline(30);
        let earlier = clock.register_deadline(20);
        let receiver = clock.subscribe();

        clock.advance_to(20);
        assert!(receiver.try_recv().is_ok());
        assert_eq!(clock.wake_order(), vec![earlier]);

        clock.advance_to(30);
        clock.record_dispatch(later, 31);
        assert_eq!(clock.wake_order(), vec![earlier, later]);
        assert_eq!(clock.deadline_trace()[0].dispatched_at_ns, Some(31));
    }

    #[test]
    #[should_panic(expected = "cannot move backwards")]
    fn virtual_clock_rejects_backwards_time() {
        VirtualClock::new(10).advance_to(9);
    }
}
