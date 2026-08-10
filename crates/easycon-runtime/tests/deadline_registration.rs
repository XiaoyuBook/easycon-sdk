use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::Duration;

use easycon_model::ErrorCode;
use easycon_runtime::{
    Clock, ClockChangeRegistration, CloseOutcome, CloseRejection, DeadlineId, DeadlineResolution,
    DeadlineSignal, DeadlineWaitResult, EventDraft, EventKind, OperationValue, Runtime,
    RuntimeState, SettlementEvidence, SettlementOwnerMode, Severity, SubscriptionOptions,
    SystemClock, TerminalCandidate, TransitionOutcome, VirtualClock, WaitTimeout,
};

const REENTRY_WAIT: Duration = Duration::from_secs(2);
const SAME_THREAD_REENTRY_HELPER: &str = "EASYCON_RUNTIME_SAME_THREAD_REENTRY_HELPER";
const SAME_THREAD_REENTRY_TIMEOUT_EXIT: i32 = 97;

struct CountingWake {
    count: AtomicUsize,
}

impl CountingWake {
    fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

impl Wake for CountingWake {
    fn wake(self: Arc<Self>) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }
}

struct EventWake {
    label: &'static str,
    events: mpsc::Sender<&'static str>,
    count: AtomicUsize,
}

impl EventWake {
    fn new(label: &'static str, events: mpsc::Sender<&'static str>) -> Self {
        Self {
            label,
            events,
            count: AtomicUsize::new(0),
        }
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

impl Wake for EventWake {
    fn wake(self: Arc<Self>) {
        self.count.fetch_add(1, Ordering::AcqRel);
        self.events
            .send(self.label)
            .expect("deadline waker event receiver remains alive");
    }
}

struct ReentrantBatchWake {
    runtime: Runtime,
    other_signal: DeadlineSignal,
    events: mpsc::Sender<&'static str>,
}

impl Wake for ReentrantBatchWake {
    fn wake(self: Arc<Self>) {
        assert!(matches!(
            self.other_signal.resolution(),
            Some(outcome) if outcome.resolution == DeadlineResolution::Fired
        ));
        assert_eq!(self.runtime.state(), RuntimeState::Active);
        let nested = self
            .runtime
            .register_deadline(u64::MAX)
            .expect("reentrant deadline admission");
        assert_eq!(nested.disarm(), DeadlineResolution::Disarmed);
        assert!(matches!(
            self.runtime.close(),
            Err(CloseRejection::SupervisedTask)
        ));
        self.events
            .send("low")
            .expect("deadline waker event receiver remains alive");
    }
}

struct PanickingWake;

impl Wake for PanickingWake {
    fn wake(self: Arc<Self>) {
        panic!("scripted deadline observer panic");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClockCallback {
    RegisterDeadline,
    Now,
    Dispatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeReentry {
    StateQuery,
    DeadlineAdmission,
    SubscriptionAdmission,
    Close,
}

struct WorkerWaitGate {
    reached: mpsc::SyncSender<()>,
    release: mpsc::Receiver<()>,
}

struct WorkerWaitBarrier {
    reached: mpsc::Receiver<()>,
    release: mpsc::SyncSender<()>,
}

impl WorkerWaitBarrier {
    fn wait_until_reached(&self, message: &str) {
        self.reached.recv_timeout(REENTRY_WAIT).expect(message);
    }
}

impl Drop for WorkerWaitBarrier {
    fn drop(&mut self) {
        let _ = self.release.try_send(());
    }
}

struct ReentrantRuntimeClock {
    inner: VirtualClock,
    callback: ClockCallback,
    reentry: RuntimeReentry,
    runtime: Mutex<Option<Runtime>>,
    armed_thread: Mutex<Option<ThreadId>>,
    reenter_once: AtomicBool,
    callback_completed: Mutex<Option<bool>>,
    callback_thread: Mutex<Option<JoinHandle<()>>>,
    worker_wait_gate: Mutex<Option<WorkerWaitGate>>,
    worker_wait_release_on_callback: Mutex<Option<mpsc::SyncSender<()>>>,
}

impl ReentrantRuntimeClock {
    fn new(now_ns: u64, callback: ClockCallback, reentry: RuntimeReentry) -> Self {
        Self {
            inner: VirtualClock::new(now_ns),
            callback,
            reentry,
            runtime: Mutex::new(None),
            armed_thread: Mutex::new(None),
            reenter_once: AtomicBool::new(false),
            callback_completed: Mutex::new(None),
            callback_thread: Mutex::new(None),
            worker_wait_gate: Mutex::new(None),
            worker_wait_release_on_callback: Mutex::new(None),
        }
    }

    fn attach(&self, runtime: &Runtime) {
        *self.runtime.lock().expect("reentry Runtime slot") = Some(runtime.clone());
    }

    fn arm_current_thread(&self) {
        *self.armed_thread.lock().expect("reentry armed thread") = Some(thread::current().id());
        self.reenter_once.store(true, Ordering::Release);
    }

    fn hold_next_worker_wait(&self) -> WorkerWaitBarrier {
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let replaced = self
            .worker_wait_gate
            .lock()
            .expect("worker wait gate")
            .replace(WorkerWaitGate {
                reached: reached_sender,
                release: release_receiver,
            });
        assert!(replaced.is_none(), "only one worker wait gate is active");
        WorkerWaitBarrier {
            reached: reached_receiver,
            release: release_sender,
        }
    }

    fn release_worker_wait_on_callback(&self, barrier: &WorkerWaitBarrier) {
        let replaced = self
            .worker_wait_release_on_callback
            .lock()
            .expect("worker wait callback release")
            .replace(barrier.release.clone());
        assert!(
            replaced.is_none(),
            "only one worker wait callback release is active"
        );
    }

    fn release_worker_wait_after_callback_entry(&self) {
        let release = self
            .worker_wait_release_on_callback
            .lock()
            .expect("worker wait callback release")
            .take();
        if let Some(release) = release {
            release
                .try_send(())
                .expect("worker remains at the callback release barrier");
        }
    }

    fn wait_for_test_release(&self) {
        let Some(gate) = self
            .worker_wait_gate
            .lock()
            .expect("worker wait gate")
            .take()
        else {
            return;
        };
        let _ = gate.reached.send(());
        let _ = gate.release.recv_timeout(REENTRY_WAIT);
    }

    fn assert_reentry_completed(&self) {
        let completed = self
            .callback_completed
            .lock()
            .expect("reentry completion")
            .take()
            .expect("Clock callback ran");
        self.callback_thread
            .lock()
            .expect("reentry thread")
            .take()
            .expect("Clock callback spawned Runtime reentry")
            .join()
            .expect("Runtime reentry thread");
        assert!(
            completed,
            "Clock callback could not complete its public Runtime reentry before returning"
        );
    }

    fn maybe_reenter(&self, callback: ClockCallback) {
        if callback != self.callback {
            return;
        }
        let runtime = {
            let armed_thread = *self.armed_thread.lock().expect("reentry armed thread");
            if armed_thread != Some(thread::current().id())
                || !self.reenter_once.swap(false, Ordering::AcqRel)
            {
                return;
            }
            self.runtime
                .lock()
                .expect("reentry Runtime slot")
                .clone()
                .expect("Runtime attached before callback is armed")
        };
        let reentry = self.reentry;
        self.release_worker_wait_after_callback_entry();
        let (completion_sender, observed_completion) = mpsc::sync_channel(1);
        let callback_thread = thread::spawn(move || {
            let reentry_completed = match reentry {
                RuntimeReentry::StateQuery => runtime.state() == RuntimeState::Active,
                RuntimeReentry::DeadlineAdmission => runtime
                    .register_deadline(1_000)
                    .map(|registration| registration.disarm() == DeadlineResolution::Disarmed)
                    .unwrap_or(false),
                RuntimeReentry::SubscriptionAdmission => {
                    runtime.subscribe(SubscriptionOptions::default()).is_ok()
                }
                RuntimeReentry::Close => runtime.close() == Ok(CloseOutcome::Closed),
            };
            let _ = completion_sender.send(reentry_completed);
        });
        let completed = observed_completion
            .recv_timeout(REENTRY_WAIT)
            .unwrap_or(false);
        *self.callback_completed.lock().expect("reentry completion") = Some(completed);
        *self.callback_thread.lock().expect("reentry thread") = Some(callback_thread);
    }
}

impl Clock for ReentrantRuntimeClock {
    fn now_ns(&self) -> u64 {
        self.maybe_reenter(ClockCallback::Now);
        self.inner.now_ns()
    }

    fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
        self.inner.on_change(hook)
    }

    fn register_deadline(&self, target_ns: u64) -> DeadlineId {
        let id = self.inner.register_deadline(target_ns);
        self.maybe_reenter(ClockCallback::RegisterDeadline);
        id
    }

    fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
        self.maybe_reenter(ClockCallback::Dispatch);
        self.inner.record_dispatch(id, actual_ns);
    }

    fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
        self.wait_for_test_release();
        self.inner.real_wait_duration(target_ns)
    }
}

struct SameThreadStateClock {
    inner: VirtualClock,
    runtime: Mutex<Option<Runtime>>,
    armed_thread: Mutex<Option<ThreadId>>,
    reenter_once: AtomicBool,
    callback_entered: Mutex<Option<mpsc::SyncSender<()>>>,
    callback_completed: Mutex<Option<mpsc::Sender<()>>>,
}

impl SameThreadStateClock {
    fn new(
        now_ns: u64,
        callback_entered: mpsc::SyncSender<()>,
        callback_completed: mpsc::Sender<()>,
    ) -> Self {
        Self {
            inner: VirtualClock::new(now_ns),
            runtime: Mutex::new(None),
            armed_thread: Mutex::new(None),
            reenter_once: AtomicBool::new(false),
            callback_entered: Mutex::new(Some(callback_entered)),
            callback_completed: Mutex::new(Some(callback_completed)),
        }
    }

    fn attach(&self, runtime: &Runtime) {
        *self.runtime.lock().expect("same-thread Runtime slot") = Some(runtime.clone());
    }

    fn arm_current_thread(&self) {
        *self.armed_thread.lock().expect("same-thread armed thread") = Some(thread::current().id());
        self.reenter_once.store(true, Ordering::Release);
    }

    fn reenter_state(&self) {
        if *self.armed_thread.lock().expect("same-thread armed thread")
            != Some(thread::current().id())
            || !self.reenter_once.swap(false, Ordering::AcqRel)
        {
            return;
        }
        let runtime = self
            .runtime
            .lock()
            .expect("same-thread Runtime slot")
            .clone()
            .expect("Runtime attached before callback is armed");
        self.callback_entered
            .lock()
            .expect("same-thread callback entry")
            .take()
            .expect("Clock callback enters once")
            .send(())
            .expect("same-thread watchdog is waiting");
        assert_eq!(runtime.state(), RuntimeState::Active);
        self.callback_completed
            .lock()
            .expect("same-thread callback completion")
            .take()
            .expect("Clock callback completes once")
            .send(())
            .expect("same-thread watchdog remains alive");
    }
}

impl Clock for SameThreadStateClock {
    fn now_ns(&self) -> u64 {
        self.inner.now_ns()
    }

    fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
        self.inner.on_change(hook)
    }

    fn register_deadline(&self, target_ns: u64) -> DeadlineId {
        let id = self.inner.register_deadline(target_ns);
        self.reenter_state();
        id
    }

    fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
        self.inner.record_dispatch(id, actual_ns);
    }

    fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
        self.inner.real_wait_duration(target_ns)
    }
}

fn runtime_with_reentrant_clock(
    now_ns: u64,
    callback: ClockCallback,
    reentry: RuntimeReentry,
) -> (Arc<ReentrantRuntimeClock>, Runtime) {
    let clock = Arc::new(ReentrantRuntimeClock::new(now_ns, callback, reentry));
    let runtime = Runtime::new(clock.clone());
    clock.attach(&runtime);
    (clock, runtime)
}

#[test]
fn same_thread_register_deadline_callback_can_reenter_public_runtime_state_query() {
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("same_thread_register_deadline_callback_helper")
        .arg("--nocapture")
        .env(SAME_THREAD_REENTRY_HELPER, "1")
        .status()
        .expect("same-thread reentry helper process");
    assert!(
        status.success(),
        "same-thread reentry helper exited with {status}; watchdog timeout exits with {SAME_THREAD_REENTRY_TIMEOUT_EXIT}"
    );
}

#[test]
fn same_thread_register_deadline_callback_helper() {
    if std::env::var_os(SAME_THREAD_REENTRY_HELPER).is_none() {
        return;
    }

    let (callback_entered, observed_callback_entry) = mpsc::sync_channel(0);
    let (callback_completed, observed_callback_completion) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if observed_callback_entry.recv_timeout(REENTRY_WAIT).is_err() {
            std::process::exit(SAME_THREAD_REENTRY_TIMEOUT_EXIT);
        }
        if observed_callback_completion
            .recv_timeout(REENTRY_WAIT)
            .is_err()
        {
            // The child watchdog keeps a broken same-thread self-lock from retaining a test thread.
            std::process::exit(SAME_THREAD_REENTRY_TIMEOUT_EXIT);
        }
    });
    let clock = Arc::new(SameThreadStateClock::new(
        0,
        callback_entered,
        callback_completed,
    ));
    let runtime = Runtime::new(clock.clone());
    clock.attach(&runtime);
    clock.arm_current_thread();

    let registration = runtime.register_deadline(100).expect("deadline");
    watchdog.join().expect("same-thread watchdog");
    assert_eq!(registration.disarm(), DeadlineResolution::Disarmed);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn register_deadline_clock_callback_can_reenter_public_runtime_admission() {
    let (clock, runtime) = runtime_with_reentrant_clock(
        0,
        ClockCallback::RegisterDeadline,
        RuntimeReentry::DeadlineAdmission,
    );

    clock.arm_current_thread();
    let registration = runtime.register_deadline(100).expect("outer deadline");
    clock.assert_reentry_completed();

    assert_eq!(registration.disarm(), DeadlineResolution::Disarmed);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn now_clock_callback_can_reenter_public_runtime_state_query() {
    let (clock, runtime) =
        runtime_with_reentrant_clock(0, ClockCallback::Now, RuntimeReentry::StateQuery);

    clock.arm_current_thread();
    let registration = runtime.register_deadline(100).expect("deadline");
    clock.assert_reentry_completed();

    assert_eq!(registration.disarm(), DeadlineResolution::Disarmed);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn event_timestamp_clock_callback_can_reenter_public_runtime_subscription() {
    let (clock, runtime) =
        runtime_with_reentrant_clock(0, ClockCallback::Now, RuntimeReentry::SubscriptionAdmission);

    clock.arm_current_thread();
    let event = runtime
        .publish(EventDraft::ordinary(
            EventKind::Data,
            "runtime.reentry-clock",
            Severity::Info,
        ))
        .expect("event");
    clock.assert_reentry_completed();

    assert_eq!(event.sequence, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn reserved_deadline_is_resolved_by_close_that_reenters_from_clock_registration() {
    let (clock, runtime) =
        runtime_with_reentrant_clock(0, ClockCallback::RegisterDeadline, RuntimeReentry::Close);

    clock.arm_current_thread();
    let registration = runtime.register_deadline(100).expect("reserved deadline");
    clock.assert_reentry_completed();

    assert_eq!(
        registration
            .signal()
            .resolution()
            .expect("deadline resolution")
            .resolution,
        DeadlineResolution::RuntimeClosed
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn already_due_dispatch_callback_can_reenter_public_runtime_close() {
    let (clock, runtime) =
        runtime_with_reentrant_clock(10, ClockCallback::Dispatch, RuntimeReentry::Close);

    let first_worker_wait = clock.hold_next_worker_wait();
    let worker_anchor = runtime
        .register_deadline(100)
        .expect("worker anchor deadline");
    first_worker_wait.wait_until_reached("deadline worker completed its empty due scan");
    clock.release_worker_wait_on_callback(&first_worker_wait);
    clock.arm_current_thread();
    let registration = runtime.register_deadline(10).expect("already-due deadline");

    assert_eq!(
        registration
            .signal()
            .resolution()
            .expect("deadline resolution")
            .resolution,
        DeadlineResolution::Fired
    );
    clock.assert_reentry_completed();
    assert_eq!(
        worker_anchor
            .signal()
            .resolution()
            .expect("worker anchor resolution")
            .resolution,
        DeadlineResolution::RuntimeClosed
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn operation_deadline_constructors_release_runtime_state_before_clock_registration() {
    let (clock, runtime) = runtime_with_reentrant_clock(
        0,
        ClockCallback::RegisterDeadline,
        RuntimeReentry::StateQuery,
    );
    clock.arm_current_thread();
    let operation = runtime.create_operation(Some(100)).expect("operation");
    clock.assert_reentry_completed();
    drop(operation);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));

    let (clock, runtime) = runtime_with_reentrant_clock(
        0,
        ClockCallback::RegisterDeadline,
        RuntimeReentry::StateQuery,
    );
    let parent = runtime.child_cancellation_token();
    clock.arm_current_thread();
    let operation = runtime
        .create_operation_with_parent(Some(100), &parent)
        .expect("child operation");
    clock.assert_reentry_completed();
    drop(operation);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));

    let (clock, runtime) = runtime_with_reentrant_clock(
        0,
        ClockCallback::RegisterDeadline,
        RuntimeReentry::StateQuery,
    );
    clock.arm_current_thread();
    let (operation, owner) = runtime
        .create_operation_with_settlement_owner(
            Some(100),
            SettlementOwnerMode::Exclusive,
            || Ok(()),
            |_| {},
        )
        .expect("owned operation");
    clock.assert_reentry_completed();
    assert_eq!(operation.start(), TransitionOutcome::Applied);
    assert_eq!(
        owner.settle(
            SettlementEvidence::EffectAccepted,
            TerminalCandidate::Success(OperationValue::Unit),
        ),
        TransitionOutcome::Applied
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: deadline.registration-immediate
#[test]
fn already_due_registration_is_fired_before_return_without_registry_changes() {
    let clock = Arc::new(VirtualClock::new(10));
    let runtime = Runtime::new(clock);
    let before = runtime.counts();

    let registration = runtime.register_deadline(10).expect("deadline");
    let signal = registration.signal();

    assert_eq!(
        signal
            .resolution()
            .expect("synchronous resolution")
            .resolution,
        DeadlineResolution::Fired
    );
    assert_eq!(runtime.counts(), before);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: deadline.registration-virtual-order
#[test]
fn virtual_clock_fires_only_after_advance_and_orders_same_target_by_id() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let later = runtime.register_deadline(30).expect("later");
    let first = runtime.register_deadline(20).expect("first");
    let second = runtime.register_deadline(20).expect("second");
    let later_signal = later.signal();
    let first_signal = first.signal();
    let second_signal = second.signal();

    assert_eq!(first_signal.resolution(), None);
    assert_eq!(second_signal.resolution(), None);
    clock.advance_to(19);
    assert_eq!(first_signal.resolution(), None);

    clock.advance_to(30);
    let DeadlineWaitResult::Resolved(first_outcome) =
        first_signal.wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("first deadline did not fire");
    };
    let DeadlineWaitResult::Resolved(second_outcome) =
        second_signal.wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("second deadline did not fire");
    };
    let DeadlineWaitResult::Resolved(later_outcome) =
        later_signal.wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("later deadline did not fire");
    };
    assert_eq!(first_outcome.resolution, DeadlineResolution::Fired);
    assert_eq!(second_outcome.resolution, DeadlineResolution::Fired);
    assert_eq!(later_outcome.resolution, DeadlineResolution::Fired);
    assert!(first.id() < second.id());
    assert!(first_outcome.order < second_outcome.order);
    assert!(second_outcome.order < later_outcome.order);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: deadline.registration-system-progress
#[test]
fn system_clock_registration_fires_without_consumer_progress() {
    let clock = Arc::new(SystemClock::new());
    let runtime = Runtime::new(clock.clone());
    let target = clock.now_ns().saturating_add(20_000_000);
    let registration = runtime.register_deadline(target).expect("deadline");

    assert!(matches!(
        registration.signal().wait(WaitTimeout::For(Duration::from_secs(5))),
        DeadlineWaitResult::Resolved(outcome)
            if outcome.resolution == DeadlineResolution::Fired
    ));
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: deadline.registration-exactly-once
#[test]
fn disarm_drop_close_and_stale_entries_resolve_once() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());

    let disarmed = runtime.register_deadline(10).expect("disarmed");
    let disarmed_signal = disarmed.signal();
    assert_eq!(disarmed.disarm(), DeadlineResolution::Disarmed);
    assert_eq!(disarmed.disarm(), DeadlineResolution::Disarmed);

    let dropped = runtime.register_deadline(10).expect("dropped");
    let dropped_signal = dropped.signal();
    let dropped_id = dropped.id();
    drop(dropped);

    let fired = runtime.register_deadline(10).expect("fired");
    let fired_signal = fired.signal();
    assert!(dropped_id < fired.id());
    clock.advance_to(10);

    assert!(matches!(
        fired_signal.wait(WaitTimeout::For(Duration::from_secs(5))),
        DeadlineWaitResult::Resolved(outcome)
            if outcome.resolution == DeadlineResolution::Fired
    ));

    assert_eq!(
        disarmed_signal.resolution().expect("disarmed").resolution,
        DeadlineResolution::Disarmed
    );
    assert_eq!(
        dropped_signal.resolution().expect("dropped").resolution,
        DeadlineResolution::Disarmed
    );
    assert_eq!(
        fired_signal.resolution().expect("fired").resolution,
        DeadlineResolution::Fired
    );

    let closed = runtime.register_deadline(20).expect("closed");
    let closed_signal = closed.signal();
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(
        closed_signal.resolution().expect("closed").resolution,
        DeadlineResolution::RuntimeClosed
    );

    clock.advance_to(20);
    assert_eq!(
        disarmed_signal
            .resolution()
            .expect("stable disarm")
            .resolution,
        DeadlineResolution::Disarmed
    );
    assert_eq!(
        dropped_signal.resolution().expect("stable drop").resolution,
        DeadlineResolution::Disarmed
    );
    assert_eq!(
        fired_signal.resolution().expect("stable fire").resolution,
        DeadlineResolution::Fired
    );
    assert_eq!(
        closed_signal.resolution().expect("stable close").resolution,
        DeadlineResolution::RuntimeClosed
    );

    let error = match runtime.register_deadline(30) {
        Err(error) => error,
        Ok(_) => panic!("closed Runtime accepted a deadline"),
    };
    assert_eq!(error.code(), ErrorCode::RuntimeClosing);
}

// conformance: deadline.registration-production-race
#[test]
fn fire_disarm_and_close_race_resolves_the_production_registration_once() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let registration = runtime.register_deadline(10).expect("deadline");
    let signal = registration.signal();
    let barrier = Arc::new(Barrier::new(4));

    let advancing_clock = clock.clone();
    let advancing_barrier = barrier.clone();
    let advancing = std::thread::spawn(move || {
        advancing_barrier.wait();
        advancing_clock.advance_to(10);
    });
    let disarming_barrier = barrier.clone();
    let disarming = std::thread::spawn(move || {
        disarming_barrier.wait();
        registration.disarm()
    });
    let closing_runtime = runtime.clone();
    let closing_barrier = barrier.clone();
    let closing = std::thread::spawn(move || {
        closing_barrier.wait();
        closing_runtime.close()
    });

    barrier.wait();
    advancing.join().expect("clock advance");
    let disarm_resolution = disarming.join().expect("deadline disarm");
    assert_eq!(
        closing.join().expect("Runtime close"),
        Ok(CloseOutcome::Closed)
    );
    let outcome = signal.resolution().expect("deadline resolution");
    assert_eq!(outcome.resolution, disarm_resolution);
    assert!(matches!(
        outcome.resolution,
        DeadlineResolution::Fired
            | DeadlineResolution::Disarmed
            | DeadlineResolution::RuntimeClosed
    ));
    assert_eq!(signal.resolution(), Some(outcome));
}

#[test]
fn deadline_signal_poll_resolution_contract_is_nonblocking() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock);
    let registration = runtime.register_deadline(10).expect("deadline");
    let signal = registration.signal();
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);

    assert_eq!(signal.poll_resolution(&mut context), Poll::Pending);
    assert_eq!(registration.disarm(), DeadlineResolution::Disarmed);
    assert!(matches!(
        signal.poll_resolution(&mut context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Disarmed
    ));
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn already_resolved_deadline_signal_polls_ready_without_retaining_a_waker() {
    let clock = Arc::new(VirtualClock::new(10));
    let runtime = Runtime::new(clock);
    let registration = runtime.register_deadline(10).expect("already-due deadline");
    let signal = registration.signal();
    let wake = Arc::new(CountingWake::new());
    let waker = Waker::from(Arc::clone(&wake));
    let mut context = Context::from_waker(&waker);

    assert!(matches!(
        signal.poll_resolution(&mut context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Fired
    ));
    assert_eq!(wake.count(), 0);
    assert!(matches!(
        signal.poll_resolution(&mut context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Fired
    ));
    assert_eq!(wake.count(), 0);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn deadline_signal_poll_resolution_deduplicates_and_replaces_wakers() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock);

    let same_registration = runtime.register_deadline(10).expect("same waker deadline");
    let same_signal = same_registration.signal();
    let same_wake = Arc::new(CountingWake::new());
    let same_waker = Waker::from(Arc::clone(&same_wake));
    let mut same_context = Context::from_waker(&same_waker);
    assert_eq!(
        same_signal.poll_resolution(&mut same_context),
        Poll::Pending
    );
    assert_eq!(
        same_signal.poll_resolution(&mut same_context),
        Poll::Pending
    );
    assert_eq!(same_wake.count(), 0);
    assert_eq!(same_registration.disarm(), DeadlineResolution::Disarmed);
    assert_eq!(same_wake.count(), 1);
    assert!(matches!(
        same_signal.poll_resolution(&mut same_context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Disarmed
    ));
    assert_eq!(same_wake.count(), 1);

    let replacement_registration = runtime.register_deadline(20).expect("replacement deadline");
    let replacement_signal = replacement_registration.signal();
    let old_wake = Arc::new(CountingWake::new());
    let old_waker = Waker::from(Arc::clone(&old_wake));
    let mut old_context = Context::from_waker(&old_waker);
    let latest_wake = Arc::new(CountingWake::new());
    let latest_waker = Waker::from(Arc::clone(&latest_wake));
    let mut latest_context = Context::from_waker(&latest_waker);
    assert_eq!(
        replacement_signal.poll_resolution(&mut old_context),
        Poll::Pending
    );
    assert_eq!(
        replacement_signal.poll_resolution(&mut latest_context),
        Poll::Pending
    );
    assert_eq!(
        replacement_registration.disarm(),
        DeadlineResolution::Disarmed
    );
    assert_eq!(old_wake.count(), 0);
    assert_eq!(latest_wake.count(), 1);
    assert!(matches!(
        replacement_signal.poll_resolution(&mut latest_context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Disarmed
    ));
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn deadline_signal_terminal_paths_wake_one_async_observer_once() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock);

    let disarmed = runtime.register_deadline(10).expect("disarmed deadline");
    let disarmed_signal = disarmed.signal();
    let disarmed_wake = Arc::new(CountingWake::new());
    let disarmed_waker = Waker::from(Arc::clone(&disarmed_wake));
    let mut disarmed_context = Context::from_waker(&disarmed_waker);
    assert_eq!(
        disarmed_signal.poll_resolution(&mut disarmed_context),
        Poll::Pending
    );

    let dropped = runtime.register_deadline(20).expect("dropped deadline");
    let dropped_signal = dropped.signal();
    let dropped_wake = Arc::new(CountingWake::new());
    let dropped_waker = Waker::from(Arc::clone(&dropped_wake));
    let mut dropped_context = Context::from_waker(&dropped_waker);
    assert_eq!(
        dropped_signal.poll_resolution(&mut dropped_context),
        Poll::Pending
    );

    let closed = runtime.register_deadline(30).expect("closed deadline");
    let closed_signal = closed.signal();
    let closed_wake = Arc::new(CountingWake::new());
    let closed_waker = Waker::from(Arc::clone(&closed_wake));
    let mut closed_context = Context::from_waker(&closed_waker);
    assert_eq!(
        closed_signal.poll_resolution(&mut closed_context),
        Poll::Pending
    );

    assert_eq!(disarmed.disarm(), DeadlineResolution::Disarmed);
    drop(dropped);
    assert_eq!(disarmed_wake.count(), 1);
    assert_eq!(dropped_wake.count(), 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(closed_wake.count(), 1);

    assert!(matches!(
        disarmed_signal.poll_resolution(&mut disarmed_context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Disarmed
    ));
    assert!(matches!(
        dropped_signal.poll_resolution(&mut dropped_context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Disarmed
    ));
    assert!(matches!(
        closed_signal.poll_resolution(&mut closed_context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::RuntimeClosed
    ));
    assert_eq!(disarmed_wake.count(), 1);
    assert_eq!(dropped_wake.count(), 1);
    assert_eq!(closed_wake.count(), 1);
}

#[test]
fn system_clock_poll_resolution_wakes_without_consumer_progress() {
    let clock = Arc::new(SystemClock::new());
    let runtime = Runtime::new(clock.clone());
    let target = clock.now_ns().saturating_add(20_000_000);
    let registration = runtime.register_deadline(target).expect("deadline");
    let signal = registration.signal();
    let (events, observed_events) = mpsc::channel();
    let wake = Arc::new(EventWake::new("system", events));
    let waker = Waker::from(Arc::clone(&wake));
    let mut context = Context::from_waker(&waker);

    assert_eq!(signal.poll_resolution(&mut context), Poll::Pending);
    assert_eq!(
        observed_events
            .recv_timeout(Duration::from_secs(5))
            .expect("SystemClock deadline observer wake"),
        "system"
    );
    assert_eq!(wake.count(), 1);
    assert!(matches!(
        signal.poll_resolution(&mut context),
        Poll::Ready(outcome) if outcome.resolution == DeadlineResolution::Fired
    ));
    assert_eq!(wake.count(), 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn deadline_signal_blocking_wait_and_async_observer_can_coexist() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let registration = runtime.register_deadline(10).expect("deadline");
    let signal = registration.signal();
    let blocking_signal = signal.clone();
    let (blocking_result, observed_blocking_result) = mpsc::sync_channel(1);
    let blocking_waiter = thread::spawn(move || {
        blocking_result
            .send(blocking_signal.wait(WaitTimeout::For(REENTRY_WAIT)))
            .expect("blocking waiter receiver remains alive");
    });
    let (events, observed_events) = mpsc::channel();
    let wake = Arc::new(EventWake::new("async", events));
    let waker = Waker::from(Arc::clone(&wake));
    let mut context = Context::from_waker(&waker);

    assert_eq!(signal.poll_resolution(&mut context), Poll::Pending);
    clock.advance_to(10);
    assert_eq!(
        observed_events
            .recv_timeout(Duration::from_secs(5))
            .expect("async observer wake"),
        "async"
    );
    assert!(matches!(
        observed_blocking_result
            .recv_timeout(Duration::from_secs(5))
            .expect("blocking observer result"),
        DeadlineWaitResult::Resolved(outcome) if outcome.resolution == DeadlineResolution::Fired
    ));
    blocking_waiter.join().expect("blocking waiter");
    assert_eq!(wake.count(), 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

// conformance: deadline.registration-waker
#[test]
fn same_target_waker_batch_commits_before_reentrant_runtime_calls() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let low = runtime.register_deadline(10).expect("low deadline");
    let high = runtime.register_deadline(10).expect("high deadline");
    let low_signal = low.signal();
    let high_signal = high.signal();
    let (events, observed_events) = mpsc::channel();
    let low_wake = Waker::from(Arc::new(ReentrantBatchWake {
        runtime: runtime.clone(),
        other_signal: high_signal.clone(),
        events: events.clone(),
    }));
    let high_wake = Waker::from(Arc::new(EventWake::new("high", events)));
    let mut low_context = Context::from_waker(&low_wake);
    let mut high_context = Context::from_waker(&high_wake);

    assert_eq!(low_signal.poll_resolution(&mut low_context), Poll::Pending);
    assert_eq!(
        high_signal.poll_resolution(&mut high_context),
        Poll::Pending
    );
    clock.advance_to(10);
    assert_eq!(
        observed_events
            .recv_timeout(Duration::from_secs(5))
            .expect("low reentrant wake"),
        "low"
    );
    assert_eq!(
        observed_events
            .recv_timeout(Duration::from_secs(5))
            .expect("high wake"),
        "high"
    );
    let low_outcome = low_signal.resolution().expect("low outcome");
    let high_outcome = high_signal.resolution().expect("high outcome");
    assert_eq!(low_outcome.resolution, DeadlineResolution::Fired);
    assert_eq!(high_outcome.resolution, DeadlineResolution::Fired);
    assert!(low.id() < high.id());
    assert!(low_outcome.order < high_outcome.order);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn panicking_waker_notifies_same_batch_before_worker_close_failure() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let first = runtime.register_deadline(10).expect("first deadline");
    let second = runtime.register_deadline(10).expect("second deadline");
    let first_signal = first.signal();
    let second_signal = second.signal();
    let first_waker = Waker::from(Arc::new(PanickingWake));
    let (events, observed_events) = mpsc::channel();
    let second_wake = Arc::new(EventWake::new("second", events));
    let second_waker = Waker::from(Arc::clone(&second_wake));
    let mut first_context = Context::from_waker(&first_waker);
    let mut second_context = Context::from_waker(&second_waker);

    assert_eq!(
        first_signal.poll_resolution(&mut first_context),
        Poll::Pending
    );
    assert_eq!(
        second_signal.poll_resolution(&mut second_context),
        Poll::Pending
    );
    clock.advance_to(10);
    assert_eq!(
        observed_events
            .recv_timeout(Duration::from_secs(5))
            .expect("second batch observer wake"),
        "second"
    );
    assert_eq!(second_wake.count(), 1);
    assert!(matches!(
        first_signal.resolution(),
        Some(outcome) if outcome.resolution == DeadlineResolution::Fired
    ));
    assert!(matches!(
        second_signal.resolution(),
        Some(outcome) if outcome.resolution == DeadlineResolution::Fired
    ));
    assert!(matches!(runtime.close(), Ok(CloseOutcome::Failed(_))));
}

#[test]
fn runtime_closed_waker_panic_notifies_batch_before_close_failure() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock);
    let first = runtime.register_deadline(10).expect("first deadline");
    let second = runtime.register_deadline(10).expect("second deadline");
    let first_signal = first.signal();
    let second_signal = second.signal();
    let first_waker = Waker::from(Arc::new(PanickingWake));
    let (events, observed_events) = mpsc::channel();
    let second_wake = Arc::new(EventWake::new("second", events));
    let second_waker = Waker::from(Arc::clone(&second_wake));
    let mut first_context = Context::from_waker(&first_waker);
    let mut second_context = Context::from_waker(&second_waker);

    assert_eq!(
        first_signal.poll_resolution(&mut first_context),
        Poll::Pending
    );
    assert_eq!(
        second_signal.poll_resolution(&mut second_context),
        Poll::Pending
    );
    assert!(matches!(runtime.close(), Ok(CloseOutcome::Failed(_))));
    assert_eq!(
        observed_events
            .recv_timeout(Duration::from_secs(5))
            .expect("second close-drain observer wake"),
        "second"
    );
    assert_eq!(second_wake.count(), 1);
    assert!(matches!(
        first_signal.resolution(),
        Some(outcome) if outcome.resolution == DeadlineResolution::RuntimeClosed
    ));
    assert!(matches!(
        second_signal.resolution(),
        Some(outcome) if outcome.resolution == DeadlineResolution::RuntimeClosed
    ));
}
