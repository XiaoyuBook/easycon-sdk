use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
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

type ClockChangeHook = dyn Fn() + Send + Sync;

/// RAII ownership for one explicit clock-change callback.
///
/// A callback already selected by a concurrent clock advance may finish after this guard drops.
#[derive(Clone)]
pub struct ClockChangeRegistration {
    _hook: Arc<ClockChangeHook>,
}

impl ClockChangeRegistration {
    /// Keeps a callback alive for a `Clock` implementation until this guard is dropped.
    #[must_use]
    pub fn new(hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { _hook: hook }
    }
}

/// Monotonic time source used by runtime and controller scheduling.
pub trait Clock: Send + Sync + 'static {
    /// Returns nanoseconds since this clock's private monotonic epoch.
    fn now_ns(&self) -> u64;

    /// Registers a non-blocking hook invoked after an explicit clock change.
    /// Dropping the returned registration prevents selection by later clock changes.
    #[must_use]
    fn on_change(&self, hook: Arc<ClockChangeHook>) -> ClockChangeRegistration;

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
    hooks: Vec<Weak<ClockChangeHook>>,
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

    /// Advances to an absolute instant and invokes registered change callbacks.
    ///
    /// # Panics
    ///
    /// Panics if `target_ns` is earlier than the current monotonic time.
    pub fn advance_to(&self, target_ns: u64) {
        let mut state = self.state.lock().expect("virtual clock lock poisoned");
        let previous = self.now_ns.load(Ordering::Acquire);
        if target_ns < previous {
            drop(state);
            panic!("virtual monotonic time cannot move backwards");
        }
        self.now_ns.store(target_ns, Ordering::Release);
        let hooks = advance_state(&mut state, target_ns);
        drop(state);
        invoke_hooks(hooks);
    }

    /// Advances by a checked duration.
    ///
    /// # Panics
    ///
    /// Panics if the new timestamp would overflow `u64`.
    pub fn advance_by(&self, duration: Duration) {
        let delta = u64::try_from(duration.as_nanos()).expect("duration exceeds u64 nanoseconds");
        let mut state = self.state.lock().expect("virtual clock lock poisoned");
        let previous = self.now_ns.load(Ordering::Acquire);
        let Some(target_ns) = previous.checked_add(delta) else {
            drop(state);
            panic!("virtual monotonic timestamp overflow");
        };
        self.now_ns.store(target_ns, Ordering::Release);
        let hooks = advance_state(&mut state, target_ns);
        drop(state);
        invoke_hooks(hooks);
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

fn advance_state(state: &mut VirtualClockState, target_ns: u64) -> Vec<Arc<ClockChangeHook>> {
    let mut newly_woken: Vec<_> = state
        .deadlines
        .iter_mut()
        .filter(|deadline| !deadline.woken && deadline.target_ns <= target_ns)
        .map(|deadline| {
            deadline.woken = true;
            (deadline.target_ns, deadline.id)
        })
        .collect();
    newly_woken.sort_unstable();
    state
        .wake_order
        .extend(newly_woken.into_iter().map(|(_, id)| id));
    state.hooks.retain(|hook| hook.strong_count() != 0);
    state.hooks.iter().filter_map(Weak::upgrade).collect()
}

fn invoke_hooks(hooks: Vec<Arc<ClockChangeHook>>) {
    for hook in hooks {
        hook();
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

    fn on_change(&self, hook: Arc<ClockChangeHook>) -> ClockChangeRegistration {
        let mut state = self.state.lock().expect("virtual clock lock poisoned");
        state.hooks.retain(|existing| existing.strong_count() != 0);
        state.hooks.push(Arc::downgrade(&hook));
        ClockChangeRegistration::new(hook)
    }

    fn register_deadline(&self, target_ns: u64) -> DeadlineId {
        let mut state = self.state.lock().expect("virtual clock lock poisoned");
        let id = DeadlineId(self.next_deadline.fetch_add(1, Ordering::Relaxed));
        assert!(id.0 != 0, "virtual deadline ID space exhausted");
        let woken = target_ns <= self.now_ns();
        state.deadlines.push(DeadlineTrace {
            id,
            target_ns,
            dispatched_at_ns: None,
            woken,
        });
        if woken {
            state.wake_order.push(id);
        }
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
}

impl SystemClock {
    /// Creates a new private monotonic epoch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            next_deadline: AtomicU64::new(1),
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

    fn on_change(&self, hook: Arc<ClockChangeHook>) -> ClockChangeRegistration {
        ClockChangeRegistration::new(hook)
    }

    fn register_deadline(&self, _target_ns: u64) -> DeadlineId {
        let id = DeadlineId(self.next_deadline.fetch_add(1, Ordering::Relaxed));
        assert!(id.0 != 0, "system deadline ID space exhausted");
        id
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
    use std::sync::Barrier;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use super::*;

    #[test]
    fn virtual_clock_records_deadlines_wakes_and_dispatch() {
        let clock = VirtualClock::new(10);
        let later = clock.register_deadline(30);
        let earlier = clock.register_deadline(20);
        let (wake, receiver) = mpsc::channel();
        let _registration = clock.on_change(Arc::new(move || {
            let _ = wake.send(());
        }));

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

    #[test]
    fn dropped_change_registration_stops_callbacks() {
        let clock = VirtualClock::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let registration = clock.on_change(Arc::new(move || {
            observed.fetch_add(1, Ordering::AcqRel);
        }));
        clock.advance_to(1);
        assert_eq!(calls.load(Ordering::Acquire), 1);

        drop(registration);
        clock.advance_to(2);

        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(
            clock
                .state
                .lock()
                .expect("virtual clock lock")
                .hooks
                .is_empty()
        );
    }

    #[test]
    fn already_due_deadline_is_recorded_in_wake_order() {
        let clock = VirtualClock::new(10);

        let deadline = clock.register_deadline(10);

        assert!(clock.deadline_trace()[0].woken);
        assert_eq!(clock.wake_order(), [deadline]);
    }

    #[test]
    fn one_jump_wakes_deadlines_by_target_then_registration() {
        let clock = VirtualClock::default();
        let later = clock.register_deadline(30);
        let earlier = clock.register_deadline(20);
        let same_time = clock.register_deadline(20);

        clock.advance_to(30);

        assert_eq!(clock.wake_order(), [earlier, same_time, later]);
    }

    #[test]
    fn concurrent_relative_advances_are_not_lost() {
        const THREADS: usize = 8;
        const ADVANCES_PER_THREAD: usize = 1_000;

        let clock = Arc::new(VirtualClock::default());
        let barrier = Arc::new(Barrier::new(THREADS + 1));
        let workers: Vec<_> = (0..THREADS)
            .map(|_| {
                let clock = clock.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..ADVANCES_PER_THREAD {
                        clock.advance_by(Duration::from_nanos(1));
                    }
                })
            })
            .collect();
        barrier.wait();
        for worker in workers {
            worker.join().expect("clock worker");
        }

        assert_eq!(
            clock.now_ns(),
            u64::try_from(THREADS * ADVANCES_PER_THREAD).expect("test timestamp")
        );
    }
}
