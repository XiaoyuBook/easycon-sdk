use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Instant;

use crate::clock::{Clock, DeadlineId};
use crate::wait::WaitTimeout;

/// Runtime-local identity for one generic deadline registration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeadlineRegistrationId(u64);

impl DeadlineRegistrationId {
    /// Returns the numeric registration identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Unique terminal resolution of a generic deadline registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineResolution {
    /// The Runtime clock reached the absolute target.
    Fired,
    /// The registration owner disarmed or dropped the handle first.
    Disarmed,
    /// Runtime close drained the still-armed registration.
    RuntimeClosed,
}

/// Observable resolution and deterministic scheduler order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineOutcome {
    /// The unique terminal resolution.
    pub resolution: DeadlineResolution,
    /// Runtime-local order in which the scheduler committed the resolution.
    pub order: u64,
}

/// Result of waiting for a generic deadline signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeadlineWaitResult {
    /// The registration reached its unique resolution.
    Resolved(DeadlineOutcome),
    /// Only this observation wait elapsed.
    Timeout,
}

/// Cloneable observation handle for one generic deadline resolution.
#[derive(Clone)]
pub struct DeadlineSignal {
    inner: Arc<DeadlineSignalInner>,
}

/// RAII owner for one armed Runtime deadline queue entry.
pub struct DeadlineRegistration {
    id: DeadlineRegistrationId,
    target_ns: u64,
    scheduler: Weak<DeadlineScheduler>,
    signal: DeadlineSignal,
}

struct DeadlineSignalInner {
    outcome: Mutex<DeadlineResolutionState>,
    changed: Condvar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadlineResolutionState {
    outcome: Option<DeadlineOutcome>,
}

impl DeadlineResolutionState {
    pub const fn armed() -> Self {
        Self { outcome: None }
    }

    pub fn resolve(&mut self, outcome: DeadlineOutcome) -> bool {
        if self.outcome.is_some() {
            return false;
        }
        self.outcome = Some(outcome);
        true
    }

    pub const fn outcome(&self) -> Option<DeadlineOutcome> {
        self.outcome
    }
}

impl DeadlineSignalInner {
    fn new() -> Self {
        Self {
            outcome: Mutex::new(DeadlineResolutionState::armed()),
            changed: Condvar::new(),
        }
    }

    fn resolve(&self, outcome: DeadlineOutcome) -> bool {
        let mut current = lock_recover(&self.outcome);
        if !current.resolve(outcome) {
            return false;
        }
        drop(current);
        self.changed.notify_all();
        true
    }

    fn resolution(&self) -> Option<DeadlineOutcome> {
        lock_recover(&self.outcome).outcome()
    }

    fn wait(&self, wait: WaitTimeout) -> DeadlineWaitResult {
        let started = Instant::now();
        let mut outcome = lock_recover(&self.outcome);
        loop {
            if let Some(outcome) = outcome.outcome() {
                return DeadlineWaitResult::Resolved(outcome);
            }
            match wait {
                WaitTimeout::Poll => return DeadlineWaitResult::Timeout,
                WaitTimeout::Infinite => {
                    outcome = self
                        .changed
                        .wait(outcome)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                WaitTimeout::For(limit) => {
                    let Some(remaining) = limit.checked_sub(started.elapsed()) else {
                        return DeadlineWaitResult::Timeout;
                    };
                    let (next, timed_out) = self
                        .changed
                        .wait_timeout(outcome, remaining)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    outcome = next;
                    if timed_out.timed_out() && outcome.outcome().is_none() {
                        return DeadlineWaitResult::Timeout;
                    }
                }
            }
        }
    }
}

impl DeadlineSignal {
    /// Returns the committed resolution without blocking.
    #[must_use]
    pub fn resolution(&self) -> Option<DeadlineOutcome> {
        self.inner.resolution()
    }

    /// Waits only for this one-shot signal; timeout has no scheduler side effect.
    #[must_use]
    pub fn wait(&self, wait: WaitTimeout) -> DeadlineWaitResult {
        self.inner.wait(wait)
    }
}

impl DeadlineRegistration {
    /// Returns the Runtime-local registration identity.
    #[must_use]
    pub const fn id(&self) -> DeadlineRegistrationId {
        self.id
    }

    /// Returns the absolute target in the owning Runtime clock epoch.
    #[must_use]
    pub const fn target_ns(&self) -> u64 {
        self.target_ns
    }

    /// Returns a cloneable one-shot observation handle.
    #[must_use]
    pub fn signal(&self) -> DeadlineSignal {
        self.signal.clone()
    }

    /// Idempotently resolves an armed registration as disarmed.
    pub fn disarm(&self) -> DeadlineResolution {
        if let Some(scheduler) = self.scheduler.upgrade() {
            scheduler.disarm(self.id, &self.signal)
        } else {
            match self.signal.wait(WaitTimeout::Infinite) {
                DeadlineWaitResult::Resolved(outcome) => outcome.resolution,
                DeadlineWaitResult::Timeout => unreachable!("infinite deadline wait timed out"),
            }
        }
    }
}

impl Drop for DeadlineRegistration {
    fn drop(&mut self) {
        if self.signal.resolution().is_some() {
            return;
        }
        if let Some(scheduler) = self.scheduler.upgrade() {
            let _ = scheduler.disarm(self.id, &self.signal);
        }
    }
}

struct ScheduledDeadline {
    target_ns: u64,
    trace_id: DeadlineId,
    signal: Arc<DeadlineSignalInner>,
}

struct PendingDeadline {
    target_ns: u64,
    signal: Arc<DeadlineSignalInner>,
}

struct DeadlineSchedulerState {
    admission_open: bool,
    pending: HashMap<DeadlineRegistrationId, PendingDeadline>,
    entries: HashMap<DeadlineRegistrationId, ScheduledDeadline>,
    queue: BinaryHeap<Reverse<(u64, DeadlineRegistrationId)>>,
}

pub(crate) struct DeadlineScheduler {
    next_id: AtomicU64,
    next_resolution_order: AtomicU64,
    fire_gate: Mutex<()>,
    state: Mutex<DeadlineSchedulerState>,
    wake_worker: Arc<dyn Fn() + Send + Sync>,
    #[cfg(test)]
    fire_entry_observer: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

pub(crate) struct DeadlineAdmission {
    id: DeadlineRegistrationId,
    target_ns: u64,
    scheduler: Arc<DeadlineScheduler>,
    signal: Arc<DeadlineSignalInner>,
    active: bool,
}

impl DeadlineAdmission {
    pub(crate) fn commit(
        mut self,
        trace_id: DeadlineId,
        now_ns: u64,
        clock: &dyn Clock,
    ) -> DeadlineRegistration {
        let committed = self.scheduler.commit_reservation(self.id, trace_id);
        self.active = false;
        if committed {
            self.scheduler.finish_commit(self.target_ns, clock, now_ns);
        }
        DeadlineRegistration {
            id: self.id,
            target_ns: self.target_ns,
            scheduler: Arc::downgrade(&self.scheduler),
            signal: DeadlineSignal {
                inner: Arc::clone(&self.signal),
            },
        }
    }
}

impl Drop for DeadlineAdmission {
    fn drop(&mut self) {
        if self.active {
            self.scheduler.abandon_reservation(self.id);
        }
    }
}

impl DeadlineScheduler {
    pub(crate) fn new(wake_worker: Arc<dyn Fn() + Send + Sync>) -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            next_resolution_order: AtomicU64::new(1),
            fire_gate: Mutex::new(()),
            state: Mutex::new(DeadlineSchedulerState {
                admission_open: true,
                pending: HashMap::new(),
                entries: HashMap::new(),
                queue: BinaryHeap::new(),
            }),
            wake_worker,
            #[cfg(test)]
            fire_entry_observer: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn register(
        self: &Arc<Self>,
        target_ns: u64,
        clock: &dyn Clock,
    ) -> Result<DeadlineRegistration, ()> {
        let admission = self.reserve(target_ns)?;
        let trace_id = clock.register_deadline(target_ns);
        let now_ns = clock.now_ns();
        Ok(admission.commit(trace_id, now_ns, clock))
    }

    pub(crate) fn reserve(self: &Arc<Self>, target_ns: u64) -> Result<DeadlineAdmission, ()> {
        let id = DeadlineRegistrationId(self.next_id.fetch_add(1, Ordering::Relaxed));
        assert!(
            id.0 != 0,
            "Runtime deadline registration ID space exhausted"
        );
        let signal = Arc::new(DeadlineSignalInner::new());
        {
            let mut state = lock_recover(&self.state);
            if !state.admission_open {
                return Err(());
            }
            let replaced = state.pending.insert(
                id,
                PendingDeadline {
                    target_ns,
                    signal: Arc::clone(&signal),
                },
            );
            debug_assert!(replaced.is_none(), "deadline admission ID is unique");
        }
        Ok(DeadlineAdmission {
            id,
            target_ns,
            scheduler: Arc::clone(self),
            signal,
            active: true,
        })
    }

    fn commit_reservation(&self, id: DeadlineRegistrationId, trace_id: DeadlineId) -> bool {
        let mut state = lock_recover(&self.state);
        let Some(pending) = state.pending.remove(&id) else {
            return false;
        };
        // Already-due registrations use the same queue so they cannot bypass lower IDs.
        state.entries.insert(
            id,
            ScheduledDeadline {
                target_ns: pending.target_ns,
                trace_id,
                signal: pending.signal,
            },
        );
        state.queue.push(Reverse((pending.target_ns, id)));
        true
    }

    fn finish_commit(&self, target_ns: u64, clock: &dyn Clock, now_ns: u64) {
        if now_ns >= target_ns {
            self.fire_due_at(clock, now_ns);
        } else {
            (self.wake_worker)();
        }
    }

    fn abandon_reservation(&self, id: DeadlineRegistrationId) {
        let removed = lock_recover(&self.state).pending.remove(&id).is_some();
        if removed {
            (self.wake_worker)();
        }
    }

    pub(crate) fn seal_admission(&self) {
        lock_recover(&self.state).admission_open = false;
    }

    pub(crate) fn fire_due(&self, clock: &dyn Clock) -> usize {
        let now_ns = clock.now_ns();
        self.fire_due_at(clock, now_ns)
    }

    fn fire_due_at(&self, clock: &dyn Clock, now_ns: u64) -> usize {
        #[cfg(test)]
        if let Some(observer) = lock_recover(&self.fire_entry_observer).take() {
            let _ = observer.send(());
        }
        // Commit each collected batch before invoking external Clock instrumentation. Callbacks may
        // panic or reenter the scheduler, so no scheduler lock can remain held across dispatch.
        let due = {
            let _fire = lock_recover(&self.fire_gate);
            let mut state = lock_recover(&self.state);
            prune_stale(&mut state);
            let mut due = Vec::new();
            while let Some(Reverse((target_ns, id))) = state.queue.peek().copied() {
                if target_ns > now_ns {
                    break;
                }
                if pending_precedes_or_matches(&state, target_ns, id, now_ns) {
                    break;
                }
                state.queue.pop();
                if let Some(entry) = state.entries.remove(&id) {
                    due.push(entry);
                }
                prune_stale(&mut state);
            }
            drop(state);
            for entry in &due {
                self.resolve_signal(&entry.signal, DeadlineResolution::Fired);
            }
            due
        };
        let count = due.len();
        for entry in due {
            clock.record_dispatch(entry.trace_id, now_ns);
        }
        count
    }

    #[cfg(test)]
    fn observe_next_fire_entry(&self, observer: std::sync::mpsc::Sender<()>) {
        *lock_recover(&self.fire_entry_observer) = Some(observer);
    }

    #[cfg(test)]
    fn fire_gate_is_available(&self) -> bool {
        self.fire_gate.try_lock().is_ok()
    }

    pub(crate) fn next_target_ns(&self) -> Option<u64> {
        let mut state = lock_recover(&self.state);
        prune_stale(&mut state);
        let next_entry = state
            .queue
            .peek()
            .map(|Reverse((target_ns, id))| (*target_ns, *id));
        let next_pending = state
            .pending
            .iter()
            .map(|(id, pending)| (pending.target_ns, *id))
            .min();
        match (next_entry, next_pending) {
            (Some(entry), Some(pending)) if pending <= entry => None,
            (Some((target_ns, _)), _) => Some(target_ns),
            (None, _) => None,
        }
    }

    pub(crate) fn drain_runtime_closed(&self) {
        self.seal_admission();
        let mut entries: Vec<_> = {
            let mut state = lock_recover(&self.state);
            let mut entries: Vec<_> = state
                .pending
                .drain()
                .map(|(id, pending)| (pending.target_ns, id, pending.signal))
                .collect();
            entries.extend(
                state
                    .entries
                    .drain()
                    .map(|(id, entry)| (entry.target_ns, id, entry.signal)),
            );
            entries
        };
        entries.sort_by_key(|(target_ns, id, _)| (*target_ns, *id));
        for (_, _, signal) in entries {
            self.resolve_signal(&signal, DeadlineResolution::RuntimeClosed);
        }
        (self.wake_worker)();
    }

    fn disarm(&self, id: DeadlineRegistrationId, signal: &DeadlineSignal) -> DeadlineResolution {
        let removed = lock_recover(&self.state).entries.remove(&id);
        if let Some(entry) = removed {
            self.resolve_signal(&entry.signal, DeadlineResolution::Disarmed);
            (self.wake_worker)();
        }
        match signal.wait(WaitTimeout::Infinite) {
            DeadlineWaitResult::Resolved(outcome) => outcome.resolution,
            DeadlineWaitResult::Timeout => unreachable!("infinite deadline wait timed out"),
        }
    }

    fn resolve_signal(&self, signal: &DeadlineSignalInner, resolution: DeadlineResolution) {
        let order = self.next_resolution_order.fetch_add(1, Ordering::Relaxed);
        assert!(
            order != 0,
            "Runtime deadline resolution order space exhausted"
        );
        let resolved = signal.resolve(DeadlineOutcome { resolution, order });
        debug_assert!(resolved, "deadline queue entry resolves exactly once");
    }
}

impl Drop for DeadlineScheduler {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.admission_open = false;
        let mut entries: Vec<_> = state
            .pending
            .drain()
            .map(|(id, pending)| (pending.target_ns, id, pending.signal))
            .collect();
        entries.extend(
            state
                .entries
                .drain()
                .map(|(id, entry)| (entry.target_ns, id, entry.signal)),
        );
        entries.sort_by_key(|(target_ns, id, _)| (*target_ns, *id));
        for (_, _, signal) in entries {
            let order = self.next_resolution_order.fetch_add(1, Ordering::Relaxed);
            assert!(
                order != 0,
                "Runtime deadline resolution order space exhausted"
            );
            let _ = signal.resolve(DeadlineOutcome {
                resolution: DeadlineResolution::RuntimeClosed,
                order,
            });
        }
    }
}

fn prune_stale(state: &mut DeadlineSchedulerState) {
    while let Some(Reverse((target_ns, id))) = state.queue.peek().copied() {
        if state
            .entries
            .get(&id)
            .is_some_and(|entry| entry.target_ns == target_ns)
        {
            break;
        }
        state.queue.pop();
    }
}

fn pending_precedes_or_matches(
    state: &DeadlineSchedulerState,
    target_ns: u64,
    id: DeadlineRegistrationId,
    now_ns: u64,
) -> bool {
    state.pending.iter().any(|(pending_id, pending)| {
        pending.target_ns <= now_ns && (pending.target_ns, *pending_id) <= (target_ns, id)
    })
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::mem;
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::mpsc::{self, Receiver, SyncSender};
    use std::thread::{self, ThreadId};
    use std::time::Duration;

    use super::*;
    use crate::clock::{ClockChangeRegistration, VirtualClock};
    use crate::runtime::{CloseOutcome, ClosePhase, Runtime};

    const TARGET_NS: u64 = 10;

    struct Pause {
        reached: SyncSender<()>,
        release: Receiver<()>,
    }

    struct ScriptedClock {
        inner: VirtualClock,
        first_registration_thread: Mutex<Option<ThreadId>>,
        first_now_pause: Mutex<Option<Pause>>,
        first_dispatch_pause: Mutex<Option<Pause>>,
    }

    impl ScriptedClock {
        fn new(first_now_pause: Option<Pause>, first_dispatch_pause: Option<Pause>) -> Self {
            Self {
                inner: VirtualClock::default(),
                first_registration_thread: Mutex::new(None),
                first_now_pause: Mutex::new(first_now_pause),
                first_dispatch_pause: Mutex::new(first_dispatch_pause),
            }
        }

        fn advance_to(&self, target_ns: u64) {
            self.inner.advance_to(target_ns);
        }
    }

    impl Clock for ScriptedClock {
        fn now_ns(&self) -> u64 {
            let now_ns = self.inner.now_ns();
            let current = thread::current().id();
            let should_pause = lock_recover(&self.first_registration_thread)
                .is_some_and(|thread| thread == current);
            if should_pause {
                *lock_recover(&self.first_registration_thread) = None;
                if let Some(pause) = lock_recover(&self.first_now_pause).take() {
                    pause.reached.send(()).expect("first now reached");
                    pause.release.recv().expect("release first now");
                }
            }
            now_ns
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> DeadlineId {
            let id = self.inner.register_deadline(target_ns);
            if id.get() == 1 && lock_recover(&self.first_now_pause).is_some() {
                *lock_recover(&self.first_registration_thread) = Some(thread::current().id());
            }
            id
        }

        fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
            if id.get() == 1
                && let Some(pause) = lock_recover(&self.first_dispatch_pause).take()
            {
                pause.reached.send(()).expect("first dispatch reached");
                pause.release.recv().expect("release first dispatch");
            }
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    struct ReentrantDispatchClock {
        inner: VirtualClock,
        scheduler: Mutex<Weak<DeadlineScheduler>>,
        nested: Mutex<Option<DeadlineRegistration>>,
        reenter_once: AtomicBool,
    }

    impl ReentrantDispatchClock {
        fn new() -> Self {
            Self {
                inner: VirtualClock::default(),
                scheduler: Mutex::new(Weak::new()),
                nested: Mutex::new(None),
                reenter_once: AtomicBool::new(true),
            }
        }

        fn attach(&self, scheduler: &Arc<DeadlineScheduler>) {
            *lock_recover(&self.scheduler) = Arc::downgrade(scheduler);
        }

        fn advance_to(&self, target_ns: u64) {
            self.inner.advance_to(target_ns);
        }

        fn take_nested(&self) -> DeadlineRegistration {
            lock_recover(&self.nested)
                .take()
                .expect("dispatch callback registered nested deadline")
        }
    }

    impl Clock for ReentrantDispatchClock {
        fn now_ns(&self) -> u64 {
            self.inner.now_ns()
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> DeadlineId {
            self.inner.register_deadline(target_ns)
        }

        fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
            if self.reenter_once.swap(false, Ordering::AcqRel) {
                let scheduler = lock_recover(&self.scheduler)
                    .upgrade()
                    .expect("scheduler remains alive during dispatch");
                assert!(
                    scheduler.fire_gate_is_available(),
                    "deadline fire gate remained held across Clock::record_dispatch"
                );
                let nested = scheduler
                    .register(TARGET_NS, self)
                    .expect("nested due registration");
                *lock_recover(&self.nested) = Some(nested);
            }
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    struct PanickingDispatchClock {
        inner: VirtualClock,
        panic_once: AtomicBool,
        dispatch_started: SyncSender<()>,
    }

    impl PanickingDispatchClock {
        fn new(dispatch_started: SyncSender<()>) -> Self {
            Self {
                inner: VirtualClock::default(),
                panic_once: AtomicBool::new(true),
                dispatch_started,
            }
        }

        fn advance_to(&self, target_ns: u64) {
            self.inner.advance_to(target_ns);
        }
    }

    impl Clock for PanickingDispatchClock {
        fn now_ns(&self) -> u64 {
            self.inner.now_ns()
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> DeadlineId {
            self.inner.register_deadline(target_ns)
        }

        fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
            if self.panic_once.swap(false, Ordering::AcqRel) {
                self.dispatch_started
                    .send(())
                    .expect("dispatch observer remains alive");
                panic!("scripted dispatch panic");
            }
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    struct NowPanicsAfterRegistrationClock {
        inner: VirtualClock,
        panic_next_now: AtomicBool,
        dispatch_count: AtomicUsize,
    }

    impl NowPanicsAfterRegistrationClock {
        fn new() -> Self {
            Self {
                inner: VirtualClock::default(),
                panic_next_now: AtomicBool::new(false),
                dispatch_count: AtomicUsize::new(0),
            }
        }

        fn advance_to(&self, target_ns: u64) {
            self.inner.advance_to(target_ns);
        }
    }

    impl Clock for NowPanicsAfterRegistrationClock {
        fn now_ns(&self) -> u64 {
            assert!(
                !self.panic_next_now.swap(false, Ordering::AcqRel),
                "scripted now panic"
            );
            self.inner.now_ns()
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.inner.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> DeadlineId {
            let id = self.inner.register_deadline(target_ns);
            self.panic_next_now.store(true, Ordering::Release);
            id
        }

        fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
            self.dispatch_count.fetch_add(1, Ordering::AcqRel);
            self.inner.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.inner.real_wait_duration(target_ns)
        }
    }

    fn scheduler() -> Arc<DeadlineScheduler> {
        DeadlineScheduler::new(Arc::new(|| {}))
    }

    fn outcome(registration: &DeadlineRegistration) -> DeadlineOutcome {
        registration
            .signal()
            .resolution()
            .expect("deadline must resolve")
    }

    #[test]
    fn cached_not_due_registration_cannot_be_overtaken_at_same_target() {
        let (now_reached, observed_now) = mpsc::sync_channel(0);
        let (release_now, now_released) = mpsc::sync_channel(0);
        let clock = Arc::new(ScriptedClock::new(
            Some(Pause {
                reached: now_reached,
                release: now_released,
            }),
            None,
        ));
        let scheduler = scheduler();

        let first_scheduler = Arc::clone(&scheduler);
        let first_clock = Arc::clone(&clock);
        let first = thread::spawn(move || {
            first_scheduler
                .register(TARGET_NS, &*first_clock)
                .expect("first registration")
        });
        observed_now
            .recv()
            .expect("first registration read old time");
        clock.advance_to(TARGET_NS);
        release_now.send(()).expect("release first registration");
        let first = first.join().expect("first registration thread");

        let second = scheduler
            .register(TARGET_NS, &*clock)
            .expect("second registration");
        scheduler.fire_due(&*clock);

        let first_outcome = outcome(&first);
        let second_outcome = outcome(&second);
        assert!(first.id() < second.id());
        assert!(
            first_outcome.order < second_outcome.order,
            "same-target registration order inverted: first_id={} first_order={} second_id={} second_order={}",
            first.id().get(),
            first_outcome.order,
            second.id().get(),
            second_outcome.order
        );
    }

    #[test]
    fn pending_lower_id_blocks_same_target_dispatch_until_it_commits() {
        let (now_reached, observed_now) = mpsc::sync_channel(0);
        let (release_now, now_released) = mpsc::sync_channel(0);
        let clock = Arc::new(ScriptedClock::new(
            Some(Pause {
                reached: now_reached,
                release: now_released,
            }),
            None,
        ));
        let scheduler = scheduler();

        let first_scheduler = Arc::clone(&scheduler);
        let first_clock = Arc::clone(&clock);
        let first = thread::spawn(move || {
            first_scheduler
                .register(TARGET_NS, &*first_clock)
                .expect("first registration")
        });
        observed_now
            .recv()
            .expect("lower ID registration entered Clock::now_ns");
        clock.advance_to(TARGET_NS);

        let second = scheduler
            .register(TARGET_NS, &*clock)
            .expect("second registration");
        assert_eq!(
            second.signal().resolution(),
            None,
            "higher ID must wait for an earlier pending registration at the same target"
        );

        release_now.send(()).expect("release lower ID registration");
        let first = first.join().expect("first registration thread");
        assert_eq!(scheduler.fire_due(&*clock), 2);

        let first_outcome = outcome(&first);
        let second_outcome = outcome(&second);
        assert!(first.id() < second.id());
        assert!(
            first_outcome.order < second_outcome.order,
            "pending lower ID was overtaken: first_id={} first_order={} second_id={} second_order={}",
            first.id().get(),
            first_outcome.order,
            second.id().get(),
            second_outcome.order
        );
    }

    #[test]
    fn collected_lower_id_fire_batch_cannot_be_overtaken_by_due_registration() {
        let (dispatch_reached, observed_dispatch) = mpsc::sync_channel(0);
        let (release_dispatch, dispatch_released) = mpsc::sync_channel(0);
        let clock = Arc::new(ScriptedClock::new(
            None,
            Some(Pause {
                reached: dispatch_reached,
                release: dispatch_released,
            }),
        ));
        let scheduler = scheduler();
        let first = scheduler
            .register(TARGET_NS, &*clock)
            .expect("first registration");
        clock.advance_to(TARGET_NS);

        let firing_scheduler = Arc::clone(&scheduler);
        let firing_clock = Arc::clone(&clock);
        let firing = thread::spawn(move || firing_scheduler.fire_due(&*firing_clock));
        observed_dispatch
            .recv()
            .expect("lower ID batch reached dispatch");

        let (high_progress, observed_high_progress) = mpsc::channel();
        scheduler.observe_next_fire_entry(high_progress.clone());
        let high_scheduler = Arc::clone(&scheduler);
        let high_clock = Arc::clone(&clock);
        let second = thread::spawn(move || {
            let registration = high_scheduler
                .register(TARGET_NS, &*high_clock)
                .expect("second registration");
            let _ = high_progress.send(());
            registration
        });
        observed_high_progress
            .recv()
            .expect("higher ID completed or entered ordered fire");
        release_dispatch.send(()).expect("release lower ID batch");

        assert_eq!(firing.join().expect("lower ID fire thread"), 1);
        let second = second.join().expect("second registration thread");
        let first_outcome = outcome(&first);
        let second_outcome = outcome(&second);
        assert!(first.id() < second.id());
        assert!(
            first_outcome.order < second_outcome.order,
            "collected lower ID was overtaken: first_id={} first_order={} second_id={} second_order={}",
            first.id().get(),
            first_outcome.order,
            second.id().get(),
            second_outcome.order
        );
    }

    #[test]
    fn dispatch_callback_can_reenter_due_registration_without_fire_gate() {
        let clock = Arc::new(ReentrantDispatchClock::new());
        let scheduler = scheduler();
        clock.attach(&scheduler);
        let outer = scheduler
            .register(TARGET_NS, &*clock)
            .expect("outer registration");
        clock.advance_to(TARGET_NS);

        let firing_scheduler = Arc::clone(&scheduler);
        let firing_clock = Arc::clone(&clock);
        let firing = thread::spawn(move || firing_scheduler.fire_due(&*firing_clock));
        let fired = match firing.join() {
            Ok(fired) => fired,
            Err(payload) => {
                mem::forget(outer);
                resume_unwind(payload);
            }
        };
        let nested = clock.take_nested();

        assert_eq!(fired, 1);
        let outer_outcome = outcome(&outer);
        let nested_outcome = outcome(&nested);
        assert_eq!(outer_outcome.resolution, DeadlineResolution::Fired);
        assert_eq!(nested_outcome.resolution, DeadlineResolution::Fired);
        assert!(outer_outcome.order < nested_outcome.order);
    }

    #[test]
    fn dispatch_panic_cannot_orphan_claimed_deadline_batch() {
        let (dispatch_started, observed_dispatch) = mpsc::sync_channel(0);
        let clock = Arc::new(PanickingDispatchClock::new(dispatch_started));
        let runtime = Runtime::new(clock.clone());
        let first = runtime
            .register_deadline(TARGET_NS)
            .expect("first registration");
        let second = runtime
            .register_deadline(TARGET_NS)
            .expect("second registration");
        let first_signal = first.signal();
        let second_signal = second.signal();

        clock.advance_to(TARGET_NS);
        observed_dispatch
            .recv_timeout(Duration::from_secs(2))
            .expect("deadline worker reached dispatch callback");

        let first_outcome = first_signal.resolution();
        let second_outcome = second_signal.resolution();
        let batch_committed = first_outcome.is_some() && second_outcome.is_some();
        if batch_committed {
            assert_eq!(first.disarm(), DeadlineResolution::Fired);
            assert_eq!(second.disarm(), DeadlineResolution::Fired);
            drop(first);
            drop(second);
        } else {
            mem::forget(first);
            mem::forget(second);
        }

        let close = runtime.close().expect("Runtime close caller");
        let CloseOutcome::Failed(report) = close else {
            panic!("dispatch callback panic cannot report Closed");
        };
        assert_eq!(report.phase, ClosePhase::InternalTaskJoin);
        assert!(
            batch_committed,
            "dispatch callback observed claimed batch before all signals were committed: first={first_outcome:?} second={second_outcome:?}"
        );
        assert!(
            first_outcome.expect("first outcome").order
                < second_outcome.expect("second outcome").order
        );
    }

    #[test]
    fn now_panic_after_trace_registration_cannot_leave_scheduler_entry() {
        let clock = NowPanicsAfterRegistrationClock::new();
        let scheduler = scheduler();

        let registration = catch_unwind(AssertUnwindSafe(|| scheduler.register(TARGET_NS, &clock)));
        assert!(registration.is_err(), "scripted now_ns call must panic");
        let target_after_panic = scheduler.next_target_ns();

        clock.advance_to(TARGET_NS);
        let fired = scheduler.fire_due(&clock);

        assert_eq!(
            target_after_panic, None,
            "now_ns panic left an unreachable queued deadline"
        );
        assert_eq!(fired, 0);
        assert_eq!(clock.dispatch_count.load(Ordering::Acquire), 0);
    }
}
