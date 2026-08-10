use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use easycon_controller::{
    AckFrame, AckRequest, AutomationLease, AutomationLeaseAcquireOutcome,
    AutomationLeaseReleaseOutcome, ConnectOptions, ControllerAction, ControllerLeaseState,
    ControllerOptions, ControllerSession, ControllerState, ControllerTransport, HandshakeRequest,
    TransportError, TransportErrorKind, WriteKind, WriteRequest,
};
use easycon_model::{Button, ErrorCode, ErrorDomain};
use easycon_runtime::{
    CancellationReason, CancellationToken, Clock, CloseOutcome, Operation, OperationState, Runtime,
    RuntimeState, SystemClock, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_test_support::{FakeControllerTransport, HandshakeOutcome};

const INTERVAL_NS: u64 = 10;
const CHANNEL_WAIT: Duration = Duration::from_secs(2);

struct OwnershipLostTransport {
    inner: FakeControllerTransport,
    panic_next_write: Arc<AtomicBool>,
    panicked: mpsc::SyncSender<()>,
}

impl ControllerTransport for OwnershipLostTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        self.inner.handshake(request)
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        if self.panic_next_write.swap(false, Ordering::AcqRel) {
            let _ = self.panicked.send(());
            panic!("scripted controller transport ownership loss");
        }
        self.inner.write(request)
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        self.inner.wait_for_ack(request)
    }

    fn close(&mut self) {
        self.inner.close();
    }

    fn close_checked(&mut self) -> Result<(), TransportError> {
        self.inner.close_checked()
    }
}

/// Panics after the physical backend has published final-byte acceptance but before the
/// Controller lane receives `write`'s return value. This is the gate-time owner-loss boundary.
struct PostAcceptancePanicTransport {
    inner: FakeControllerTransport,
    panic_after_acceptance: Arc<AtomicBool>,
    panicked: mpsc::SyncSender<()>,
}

/// Waits for a test-owned cancellation intent, then reports a non-reusable write failure. The
/// fake's checked close is separately scripted so this exercises strict cleanup precedence.
struct CancelThenCleanupFailureTransport {
    inner: FakeControllerTransport,
    fail_next_write: Arc<AtomicBool>,
    entered: mpsc::SyncSender<()>,
    resume: mpsc::Receiver<()>,
}

/// Holds the handshake after the operation has started so the test can register cancellation
/// before the failed recovery close proves a strict cleanup failure.
struct CancelThenHandshakeFailureTransport {
    inner: FakeControllerTransport,
    entered: mpsc::SyncSender<()>,
    resume: mpsc::Receiver<()>,
}

impl ControllerTransport for CancelThenHandshakeFailureTransport {
    fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
        self.entered
            .send(())
            .expect("test observes the scripted handshake barrier");
        self.resume
            .recv_timeout(CHANNEL_WAIT)
            .expect("test must release the scripted handshake failure");
        Err(TransportError::new(
            TransportErrorKind::Protocol,
            "scripted handshake failure after cancellation intent",
        ))
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        self.inner.write(request)
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        self.inner.wait_for_ack(request)
    }

    fn close(&mut self) {
        self.inner.close();
    }

    fn close_checked(&mut self) -> Result<(), TransportError> {
        self.inner.close_checked()
    }
}

impl ControllerTransport for CancelThenCleanupFailureTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        self.inner.handshake(request)
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        if self.fail_next_write.swap(false, Ordering::AcqRel) {
            let _ = self.entered.send(());
            self.resume
                .recv_timeout(CHANNEL_WAIT)
                .expect("test must release the scripted write failure");
            return Err(TransportError::new(
                TransportErrorKind::Io,
                "scripted write failure after cancellation intent",
            ));
        }
        self.inner.write(request)
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        self.inner.wait_for_ack(request)
    }

    fn close(&mut self) {
        self.inner.close();
    }

    fn close_checked(&mut self) -> Result<(), TransportError> {
        self.inner.close_checked()
    }
}

impl ControllerTransport for PostAcceptancePanicTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        self.inner.handshake(request)
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        let settlement = request.settlement.clone();
        let result = self.inner.write(request);
        if result.is_ok()
            && settlement.is_full_accepted()
            && self.panic_after_acceptance.swap(false, Ordering::AcqRel)
        {
            let _ = self.panicked.send(());
            panic!("scripted controller transport panic after final-byte acceptance");
        }
        result
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        self.inner.wait_for_ack(request)
    }

    fn close(&mut self) {
        self.inner.close();
    }

    fn close_checked(&mut self) -> Result<(), TransportError> {
        self.inner.close_checked()
    }
}

struct ChannelWake(mpsc::SyncSender<()>);

impl Wake for ChannelWake {
    fn wake(self: Arc<Self>) {
        let _ = self.0.try_send(());
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.try_send(());
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let (wake_sender, wake_receiver) = mpsc::sync_channel(1);
    let waker = Waker::from(Arc::new(ChannelWake(wake_sender)));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => wake_receiver
                .recv_timeout(CHANNEL_WAIT)
                .expect("future did not wake before bounded test deadline"),
        }
    }
}

fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
    let (wake_sender, _wake_receiver) = mpsc::sync_channel(1);
    let waker = Waker::from(Arc::new(ChannelWake(wake_sender)));
    let mut context = Context::from_waker(&waker);
    future.poll(&mut context)
}

fn wait_terminal(operation: &Operation) {
    assert!(matches!(
        operation.wait(WaitTimeout::For(CHANNEL_WAIT)),
        WaitResult::Completed(_)
    ));
}

fn wait_for_close_intent(controller: &ControllerSession) {
    let started = Instant::now();
    loop {
        match controller.snapshot().state {
            ControllerState::Disconnecting | ControllerState::Closed => return,
            _ => {
                assert!(
                    started.elapsed() < CHANNEL_WAIT,
                    "controller did not accept close intent before the bounded test deadline"
                );
                std::thread::yield_now();
            }
        }
    }
}

fn wait_for_parent_close_cancellation(operation: &Operation) {
    let started = Instant::now();
    loop {
        if operation.snapshot().cancellation_reason == Some(CancellationReason::ParentClose) {
            return;
        }
        assert!(
            started.elapsed() < CHANNEL_WAIT,
            "close did not register the pending operation cancellation before the bounded test deadline"
        );
        std::thread::yield_now();
    }
}

fn teardown_after_terminal_timeout(
    clock: &VirtualClock,
    runtime: &Runtime,
    controller: &ControllerSession,
) {
    clock.advance_to(u64::MAX);
    controller.close();
    let _ = runtime.close();
}

fn wait_terminal_with_teardown(
    operation: &Operation,
    clock: &VirtualClock,
    runtime: &Runtime,
    controller: &ControllerSession,
) {
    if matches!(
        operation.wait(WaitTimeout::For(CHANNEL_WAIT)),
        WaitResult::Completed(_)
    ) {
        return;
    }
    let snapshot = operation.snapshot();
    let controller_snapshot = controller.snapshot();
    let runtime_counts = runtime.counts();
    teardown_after_terminal_timeout(clock, runtime, controller);
    panic!(
        "operation did not settle before the bounded test deadline: {snapshot:?}; controller={controller_snapshot:?}; runtime_counts={runtime_counts:?}"
    );
}

fn join_close_thread_after_bounded_wait(
    clock: &VirtualClock,
    close_receiver: mpsc::Receiver<()>,
    close_thread: std::thread::JoinHandle<()>,
) {
    if close_receiver.recv_timeout(CHANNEL_WAIT).is_ok() {
        close_thread.join().expect("close thread");
        return;
    }
    clock.advance_to(u64::MAX);
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close thread did not settle during bounded teardown");
    close_thread.join().expect("close thread");
    panic!("close thread did not join before the bounded test deadline");
}

fn connected() -> (
    Arc<VirtualClock>,
    Runtime,
    FakeControllerTransport,
    ControllerSession,
) {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let controller = ControllerSession::new(
        &runtime,
        Box::new(fake.clone()),
        ControllerOptions {
            minimum_report_interval_ns: INTERVAL_NS,
            ..ControllerOptions::default()
        },
    )
    .expect("controller");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect");
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Succeeded);
    (clock, runtime, fake, controller)
}

fn granted(
    controller: &ControllerSession,
    cancellation: &CancellationToken,
    deadline_ns: Option<u64>,
) -> AutomationLease {
    match block_on(controller.acquire_automation_lease(cancellation, deadline_ns)) {
        AutomationLeaseAcquireOutcome::Granted(lease) => lease,
        AutomationLeaseAcquireOutcome::Cancelled => panic!("acquire was cancelled"),
        AutomationLeaseAcquireOutcome::Deadline => panic!("acquire reached its deadline"),
        AutomationLeaseAcquireOutcome::Closed => panic!("controller was closed"),
        AutomationLeaseAcquireOutcome::Failure(error) => panic!("acquire failed: {error}"),
    }
}

fn observe_terminal(operation: Operation) -> (mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
    let (sender, receiver) = mpsc::sync_channel(1);
    let observer = std::thread::spawn(move || {
        wait_terminal(&operation);
        sender.send(()).expect("terminal observer remains alive");
    });
    (receiver, observer)
}

fn close_after_pacing(clock: &VirtualClock, runtime: &Runtime, controller: &ControllerSession) {
    if controller.snapshot().state == ControllerState::Connected
        && let Some(last) = controller.snapshot().last_report_timestamp_ns
    {
        clock.advance_to(clock.now_ns().max(last.saturating_add(INTERVAL_NS)));
    }
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.acquire-cancelled
#[test]
fn cancelled_acquire_wins_before_lane_grant() {
    let (clock, runtime, _fake, controller) = connected();
    let cancellation = CancellationToken::root();
    cancellation.cancel();

    assert!(matches!(
        block_on(controller.acquire_automation_lease(&cancellation, None)),
        AutomationLeaseAcquireOutcome::Cancelled
    ));

    close_after_pacing(&clock, &runtime, &controller);
}

// conformance: controller.lease.acquire-failure
#[test]
fn busy_acquire_projects_failure_without_grant() {
    let (clock, runtime, _fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);

    let AutomationLeaseAcquireOutcome::Failure(error) =
        block_on(controller.acquire_automation_lease(&cancellation, None))
    else {
        panic!("a second acquire must fail while the lease owns the lane");
    };
    assert_eq!(error.domain(), ErrorDomain::Controller);
    assert_eq!(error.code(), ErrorCode::ResourceBusy);

    assert_eq!(
        block_on(lease.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_after_pacing(&clock, &runtime, &controller);
}

// conformance: controller.lease.generation-non-reuse
#[test]
fn successive_lease_generations_are_nonzero_and_not_reused() {
    let (clock, runtime, _fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let first = granted(&controller, &cancellation, None);
    let first_id = first.lease_id();
    assert_ne!(first_id, 0);
    assert_eq!(
        block_on(first.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );

    let second = granted(&controller, &cancellation, None);
    let second_id = second.lease_id();
    assert_ne!(second_id, 0);
    assert!(second_id > first_id);
    clock.advance_to(INTERVAL_NS);
    assert_eq!(
        block_on(second.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_after_pacing(&clock, &runtime, &controller);
}

// conformance: controller.lease.generation-wrong-controller
#[test]
fn wrong_controller_lease_is_rejected_without_affecting_its_owner() {
    let (first_clock, first_runtime, _first_fake, first_controller) = connected();
    let (second_clock, second_runtime, _second_fake, second_controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&first_controller, &cancellation, None);

    let error = match second_controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
    {
        Err(error) => error,
        Ok(_) => panic!("a lease cannot authorize another Controller"),
    };
    assert_eq!(error.domain(), ErrorDomain::Controller);
    assert_eq!(error.code(), ErrorCode::InvalidArgument);

    assert_eq!(
        block_on(lease.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_after_pacing(&first_clock, &first_runtime, &first_controller);
    close_after_pacing(&second_clock, &second_runtime, &second_controller);
}

// conformance: controller.lease.drop-cleanup
#[test]
fn dropping_a_lease_starts_one_neutral_cleanup_before_next_grant() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let first = granted(&controller, &cancellation, None);
    let first_id = first.lease_id();
    drop(first);

    let second = granted(&controller, &cancellation, None);
    assert!(second.lease_id() > first_id);
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].context.kind,
        WriteKind::Neutralize
    );

    clock.advance_to(INTERVAL_NS);
    assert_eq!(
        block_on(second.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_after_pacing(&clock, &runtime, &controller);
}

// conformance: controller.lease.release-pacing
#[test]
fn release_neutral_does_not_dispatch_before_its_pacing_target() {
    let (clock, runtime, fake, controller) = connected();
    let direct = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("initial accepted report");
    wait_terminal(&direct);
    assert_eq!(direct.snapshot().state, OperationState::Succeeded);
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_acceptance();
    let mut release = pin!(lease.neutralize_and_release());

    assert!(matches!(poll_once(release.as_mut()), Poll::Pending));
    clock.advance_to(INTERVAL_NS - 1);
    assert_eq!(fake.accepted_writes().len(), 1);
    clock.advance_to(INTERVAL_NS);
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("paced neutral reached its transport barrier");
    assert_eq!(fake.accepted_writes().len(), 2);
    fake.release_blocked_write();
    assert_eq!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_after_pacing(&clock, &runtime, &controller);
}

// conformance: controller.lease.release-accepted
#[test]
fn release_accepts_one_neutral_and_settles_its_waiter() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);

    assert_eq!(
        block_on(lease.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].context.kind,
        WriteKind::Neutralize
    );
    close_after_pacing(&clock, &runtime, &controller);
}

// conformance: controller.lease.acquire-deadline
#[test]
fn system_clock_acquire_deadline_settles_without_lane_progress() {
    let clock = Arc::new(SystemClock::new());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(Arc::new(VirtualClock::default()));
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let controller = ControllerSession::new(
        &runtime,
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("controller");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect");
    wait_terminal(&connect);

    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_before_acceptance();
    let blocker = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("blocking action");
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("write barrier reached");

    let cancellation = CancellationToken::root();
    let acquire = controller.acquire_automation_lease(
        &cancellation,
        Some(clock.now_ns().saturating_add(20_000_000)),
    );
    let (sender, receiver) = mpsc::sync_channel(1);
    let acquire_thread = std::thread::spawn(move || {
        sender
            .send(block_on(acquire))
            .expect("deadline observer remains alive");
    });

    assert!(matches!(
        receiver.recv_timeout(CHANNEL_WAIT),
        Ok(AutomationLeaseAcquireOutcome::Deadline)
    ));
    acquire_thread.join().expect("deadline observer thread");
    controller.close();
    wait_terminal(&blocker);
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.action-seal-inflight
#[test]
fn release_interrupts_an_uneffected_generation_action_before_neutral() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_before_acceptance();
    let action = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("blocking action");
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("action write barrier reached");

    let release = lease.neutralize_and_release();
    wait_terminal(&action);
    assert_eq!(action.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        action.snapshot().cancellation_reason,
        Some(CancellationReason::ParentClose)
    );
    clock.advance_to(INTERVAL_NS);
    assert_eq!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].context.kind,
        WriteKind::Neutralize
    );
    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    clock.advance_to(clock.now_ns().saturating_add(INTERVAL_NS));
    join_close_thread_after_bounded_wait(&clock, close_receiver, close_thread);
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.backend-last-byte-late-cancel
#[test]
fn backend_last_byte_acceptance_wins_over_late_close_cancellation() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_acceptance_then_cancelled();
    let action = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("accepted action");
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("backend accepted its final byte");

    let closing = controller.clone();
    let (started_sender, started_receiver) = mpsc::sync_channel(1);
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        started_sender
            .send(())
            .expect("close-start observer remains alive");
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    started_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close call started after backend acceptance");
    fake.release_blocked_write();
    clock.advance_to(INTERVAL_NS);
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close joined after the backend completion");
    close_thread.join().expect("close thread");
    runtime.close().expect("Runtime close");

    assert_eq!(action.snapshot().state, OperationState::Succeeded);
}

// conformance: controller.report.physical-final-completion-gate
#[test]
fn physical_final_completion_reserves_success_before_close_can_cancel_the_action() {
    let (clock, runtime, fake, controller) = connected();
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_final_completion_before_settlement();
    let action = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct action");
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("fake must stop at the final physical completion boundary");

    action.cancel();
    fake.release_blocked_write();
    wait_terminal_with_teardown(&action, &clock, &runtime, &controller);

    assert_eq!(action.snapshot().state, OperationState::Succeeded);
    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    clock.advance_to(INTERVAL_NS);
    join_close_thread_after_bounded_wait(&clock, close_receiver, close_thread);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn release_projects_checked_close_failure_after_neutral_write_failure() {
    let (_clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    fake.fail_write_call(
        1,
        TransportError::new(TransportErrorKind::Io, "scripted neutral write failure"),
    );
    fake.fail_next_close(TransportError::new(
        TransportErrorKind::Io,
        "scripted close failure",
    ));

    let release = lease.neutralize_and_release();
    let error = block_on(release).expect_err("cleanup failure must remain typed");

    assert_eq!(error.domain(), ErrorDomain::Io);
    assert_eq!(error.code(), ErrorCode::Transport);
    assert!(error.message().contains("scripted neutral write failure"));
    assert!(error.message().contains("scripted close failure"));
    let direct_error = match controller.direct(ControllerAction::ButtonDown(Button::A)) {
        Err(error) => error,
        Ok(_) => panic!("cleanup failure must permanently seal direct-write admission"),
    };
    assert_eq!(direct_error, error);
    let reconnect_error = match controller.connect(ConnectOptions::default()) {
        Err(error) => error,
        Ok(_) => panic!("cleanup failure must permanently seal reconnect admission"),
    };
    assert_eq!(reconnect_error, error);
    assert!(matches!(
        runtime.close().expect("Runtime close"),
        CloseOutcome::Failed(_)
    ));
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

#[test]
fn release_projects_checked_close_panic_after_neutral_write_failure() {
    let (_clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    fake.fail_write_call(
        1,
        TransportError::new(TransportErrorKind::Io, "scripted neutral write failure"),
    );
    fake.panic_next_close();

    let error = block_on(lease.neutralize_and_release())
        .expect_err("checked-close panic must settle the release future");

    assert_eq!(error.domain(), ErrorDomain::Io);
    assert_eq!(error.code(), ErrorCode::Transport);
    assert!(error.message().contains("scripted neutral write failure"));
    assert!(error.message().contains("close panicked"));
    let direct_error = match controller.direct(ControllerAction::ButtonDown(Button::A)) {
        Err(error) => error,
        Ok(_) => panic!("cleanup panic must permanently seal direct-write admission"),
    };
    assert_eq!(direct_error, error);
    assert!(matches!(
        runtime.close().expect("Runtime close"),
        CloseOutcome::Failed(_)
    ));
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

#[test]
fn final_controller_close_panic_isolated_and_projects_runtime_close_failed() {
    let (_clock, runtime, fake, controller) = connected();
    fake.panic_next_close_with_dropping_payload();

    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| controller.close())).is_ok(),
        "final transport close panic must not escape Controller close"
    );
    assert_eq!(controller.snapshot().state, ControllerState::Closed);

    let CloseOutcome::Failed(_) = runtime.close().expect("Runtime close") else {
        panic!("final Controller close panic cannot project Runtime Closed");
    };
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

// conformance: controller.close.strict-failure-pending-report
#[test]
fn final_close_failure_overrides_a_pending_report_cancellation() {
    let (clock, runtime, fake, controller) = connected();
    let accepted = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("first direct action");
    wait_terminal(&accepted);
    let pending = controller
        .direct(ControllerAction::ButtonDown(Button::B))
        .expect("paced direct action");
    fake.fail_next_close(TransportError::new(
        TransportErrorKind::Io,
        "scripted final close failure",
    ));

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    wait_for_parent_close_cancellation(&pending);
    clock.advance_to(INTERVAL_NS);
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close must settle after the final checked-close failure");
    close_thread.join().expect("close thread");

    wait_terminal(&pending);
    let pending_snapshot = pending.snapshot();
    assert_eq!(pending_snapshot.state, OperationState::Failed);
    let error = pending_snapshot
        .error
        .expect("pending report retains final close failure");
    assert!(error.message().contains("scripted final close failure"));

    let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
        panic!("final checked-close failure cannot project Runtime Closed");
    };
    assert_eq!(report.diagnostic.as_ref(), error.to_string());
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

#[test]
fn connect_recovery_close_panic_isolated_and_permanently_seals_controller() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(
        115_200,
        HandshakeOutcome::Error {
            kind: TransportErrorKind::Protocol,
            message: "scripted handshake failure".into(),
        },
    );
    fake.panic_next_close_with_dropping_payload();
    let controller = ControllerSession::new(
        &runtime,
        Box::new(fake),
        ControllerOptions {
            minimum_report_interval_ns: INTERVAL_NS,
            ..ControllerOptions::default()
        },
    )
    .expect("controller");

    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    wait_terminal(&connect);
    let connect_error = connect.snapshot().error.expect("connect cleanup failure");
    assert!(connect_error.message().contains("close panicked"));
    let direct_error = match controller.direct(ControllerAction::ButtonDown(Button::A)) {
        Err(error) => error,
        Ok(_) => panic!("recovery close panic must permanently seal direct admission"),
    };
    assert_eq!(direct_error, connect_error);

    let CloseOutcome::Failed(_) = runtime.close().expect("Runtime close") else {
        panic!("connect recovery close panic cannot project Runtime Closed");
    };
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

fn assert_cancelled_connect_recovery_strict_failure(
    script_close: impl FnOnce(&FakeControllerTransport),
    expected_close_detail: &str,
) {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    script_close(&fake);
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let (resume_sender, resume_receiver) = mpsc::sync_channel(1);
    let controller = ControllerSession::new(
        &runtime,
        Box::new(CancelThenHandshakeFailureTransport {
            inner: fake,
            entered: entered_sender,
            resume: resume_receiver,
        }),
        ControllerOptions {
            minimum_report_interval_ns: INTERVAL_NS,
            ..ControllerOptions::default()
        },
    )
    .expect("controller");

    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    entered_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("connect must reach the scripted handshake barrier");
    connect.cancel();
    resume_sender
        .send(())
        .expect("lane still owns the scripted handshake barrier");

    wait_terminal(&connect);
    let connect_snapshot = connect.snapshot();
    assert_eq!(connect_snapshot.state, OperationState::Failed);
    let connect_error = connect_snapshot
        .error
        .clone()
        .unwrap_or_else(|| panic!("strict recovery failure was lost: {connect_snapshot:?}"));
    assert!(
        connect_error
            .message()
            .contains("scripted handshake failure after cancellation intent")
    );
    assert!(connect_error.message().contains(expected_close_detail));
    let direct_error = match controller.direct(ControllerAction::ButtonDown(Button::A)) {
        Err(error) => error,
        Ok(_) => panic!("strict recovery failure must permanently seal Controller admission"),
    };
    assert_eq!(direct_error, connect_error);

    let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
        panic!("strict recovery failure cannot project Runtime Closed");
    };
    assert_eq!(report.diagnostic.as_ref(), connect_error.to_string());
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

// conformance: controller.connect.strict-cleanup-over-cancel
#[test]
fn cancelled_connect_recovery_checked_close_failure_overrides_cancellation() {
    assert_cancelled_connect_recovery_strict_failure(
        |fake| {
            fake.fail_next_close(TransportError::new(
                TransportErrorKind::Io,
                "scripted recovery checked-close failure",
            ));
        },
        "scripted recovery checked-close failure",
    );
    assert_cancelled_connect_recovery_strict_failure(
        FakeControllerTransport::panic_next_close,
        "close panicked",
    );
    assert_cancelled_connect_recovery_strict_failure(
        FakeControllerTransport::panic_next_close_with_dropping_payload,
        "close panicked",
    );
}

fn assert_action_cleanup_failure_is_projected_to_its_sealed_release(
    script_close: impl FnOnce(&FakeControllerTransport),
    expected_close_detail: &str,
) {
    let (_clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    fake.fail_write_call(
        1,
        TransportError::new(TransportErrorKind::Io, "scripted action write failure"),
    );
    script_close(&fake);

    let action = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("Automation action");
    wait_terminal(&action);
    let action_snapshot = action.snapshot();
    assert_eq!(action_snapshot.state, OperationState::Failed);
    let action_error = action_snapshot.error.expect("action cleanup failure");
    assert_eq!(action_error.domain(), ErrorDomain::Io);
    assert_eq!(action_error.code(), ErrorCode::Transport);
    assert!(
        action_error
            .message()
            .contains("scripted action write failure")
    );
    assert!(action_error.message().contains(expected_close_detail));

    let release_error = block_on(lease.neutralize_and_release())
        .expect_err("release must retain the action cleanup failure");
    assert_eq!(release_error, action_error);

    controller.close();
    let CloseOutcome::Failed(report) = runtime.close().expect("Runtime close") else {
        panic!("proven cleanup failure cannot project Runtime Closed");
    };
    assert_eq!(report.diagnostic.as_ref(), action_error.to_string());
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
}

// conformance: controller.lease.cleanup-failure-projection
#[test]
fn action_cleanup_failure_is_projected_to_its_sealed_release() {
    assert_action_cleanup_failure_is_projected_to_its_sealed_release(
        |fake| {
            fake.fail_next_close(TransportError::new(
                TransportErrorKind::Io,
                "scripted close failure",
            ));
        },
        "scripted close failure",
    );
    assert_action_cleanup_failure_is_projected_to_its_sealed_release(
        FakeControllerTransport::panic_next_close,
        "close panicked",
    );
    assert_action_cleanup_failure_is_projected_to_its_sealed_release(
        FakeControllerTransport::panic_next_close_with_dropping_payload,
        "close panicked",
    );
}

// conformance: controller.cleanup.strict-failure-over-cancel
#[test]
fn cleanup_failure_after_registered_cancel_wins_over_cancellation_for_action_and_release() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    fake.fail_next_close(TransportError::new(
        TransportErrorKind::Io,
        "scripted checked-close failure",
    ));
    let fail_next_write = Arc::new(AtomicBool::new(false));
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let (resume_sender, resume_receiver) = mpsc::sync_channel(1);
    let controller = ControllerSession::new(
        &runtime,
        Box::new(CancelThenCleanupFailureTransport {
            inner: fake,
            fail_next_write: fail_next_write.clone(),
            entered: entered_sender,
            resume: resume_receiver,
        }),
        ControllerOptions {
            minimum_report_interval_ns: INTERVAL_NS,
            ..ControllerOptions::default()
        },
    )
    .expect("controller");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect");
    wait_terminal(&connect);

    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    fail_next_write.store(true, Ordering::Release);
    let action = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("Automation action");
    entered_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("action must reach the scripted failure gate");
    action.cancel();
    resume_sender
        .send(())
        .expect("lane still owns the scripted failure gate");

    wait_terminal(&action);
    let action_snapshot = action.snapshot();
    assert_eq!(action_snapshot.state, OperationState::Failed);
    let action_error = action_snapshot
        .error
        .clone()
        .unwrap_or_else(|| panic!("strict cleanup error was lost: {action_snapshot:?}"));
    assert!(
        action_error
            .message()
            .contains("scripted checked-close failure")
    );
    assert_eq!(
        block_on(lease.neutralize_and_release()).expect_err("release must retain strict failure"),
        action_error
    );
    controller.close();
    let CloseOutcome::Failed(_) = runtime.close().expect("Runtime close") else {
        panic!("strict cleanup failure cannot project Runtime Closed");
    };
}

// conformance: controller.lease.ownership-loss
#[test]
fn controller_ownership_loss_preserves_release_record_until_runtime_close_failed() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let panic_next_write = Arc::new(AtomicBool::new(false));
    let (panicked_sender, panicked_receiver) = mpsc::sync_channel(1);
    let controller = ControllerSession::new(
        &runtime,
        Box::new(OwnershipLostTransport {
            inner: fake,
            panic_next_write: panic_next_write.clone(),
            panicked: panicked_sender,
        }),
        ControllerOptions {
            minimum_report_interval_ns: INTERVAL_NS,
            ..ControllerOptions::default()
        },
    )
    .expect("controller");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect");
    wait_terminal(&connect);

    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    panic_next_write.store(true, Ordering::Release);
    let mut release = pin!(lease.neutralize_and_release());
    panicked_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("lane-owned neutral write panicked before completion evidence");

    assert!(matches!(poll_once(release.as_mut()), Poll::Pending));
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| controller.close())).is_ok(),
        "explicit Controller close must retain the ownership-loss state for Runtime close"
    );
    assert!(matches!(poll_once(release.as_mut()), Poll::Pending));

    let CloseOutcome::Failed(_) = runtime.close().expect("Runtime close") else {
        panic!("ownership loss cannot project Runtime Closed");
    };
    assert_eq!(runtime.state(), RuntimeState::CloseFailed);
    assert!(matches!(poll_once(release.as_mut()), Poll::Pending));
}

// conformance: controller.lease.release-full-accepted-before-close-final-neutral
#[test]
fn full_accepted_release_requires_an_independent_final_close_neutral() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_acceptance();
    let release = lease.neutralize_and_release();
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("release neutral accepted its final byte before return");

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    fake.release_blocked_write();
    clock.advance_to(INTERVAL_NS);

    assert_eq!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close joined after consuming the release record");
    close_thread.join().expect("close thread");
    assert_eq!(fake.accepted_writes().len(), 2);
    assert_eq!(
        fake.accepted_writes()[1].context.kind,
        WriteKind::Neutralize
    );
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.release-physical-final-completion-gate
#[test]
fn physical_final_completion_before_close_requires_an_independent_final_neutral() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_final_completion_before_settlement();
    let release = lease.neutralize_and_release();
    clock.advance_to(INTERVAL_NS);
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("release neutral must stop at the final physical completion boundary");

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    fake.release_blocked_write();

    assert_eq!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    clock.advance_to(INTERVAL_NS.saturating_mul(2));
    join_close_thread_after_bounded_wait(&clock, close_receiver, close_thread);
    assert_eq!(fake.accepted_writes().len(), 2);
    runtime.close().expect("Runtime close");
}

// conformance: controller.report.gate-time-claim-owner-loss
#[test]
fn post_acceptance_lane_panic_keeps_the_claimed_report_settleable_after_join() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let panic_after_acceptance = Arc::new(AtomicBool::new(false));
    let (panicked_sender, panicked_receiver) = mpsc::sync_channel(1);
    let controller = ControllerSession::new(
        &runtime,
        Box::new(PostAcceptancePanicTransport {
            inner: fake,
            panic_after_acceptance: panic_after_acceptance.clone(),
            panicked: panicked_sender,
        }),
        ControllerOptions {
            minimum_report_interval_ns: INTERVAL_NS,
            ..ControllerOptions::default()
        },
    )
    .expect("controller");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect");
    wait_terminal(&connect);

    panic_after_acceptance.store(true, Ordering::Release);
    let action = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("accepted direct action");
    panicked_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("transport must panic after publishing final-byte acceptance");

    controller.close();
    let CloseOutcome::Failed(_) = runtime.close().expect("Runtime close") else {
        panic!("supervised lane panic must keep Runtime close failed");
    };
    assert_eq!(action.snapshot().state, OperationState::Succeeded);
}

// conformance: controller.lease.close-gate-claimed-drop-final-neutral
#[test]
fn close_after_gate_claimed_drop_release_runs_one_independent_final_neutral() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    let interrupted = fake.observe_next_write_interrupted();
    fake.block_next_write_after_acceptance();
    drop(lease);
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("drop-started release neutral reached its acceptance barrier");

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    assert!(
        interrupted.try_recv().is_err(),
        "close after FullAccepted must not cancel the gate-claimed release"
    );
    fake.release_blocked_write();
    clock.advance_to(INTERVAL_NS);
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close joined after its independent final neutral");
    close_thread.join().expect("close thread");
    let accepted_write_count = fake.accepted_writes().len();
    runtime.close().expect("Runtime close");

    assert_eq!(accepted_write_count, 2);
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
}

// conformance: controller.lease.release-settled-before-close-final-neutral
#[test]
fn settled_release_requires_one_independent_final_close_neutral() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let first = granted(&controller, &cancellation, None);
    let mut retained_release = pin!(first.neutralize_and_release());
    let _second = granted(&controller, &cancellation, None);
    assert_eq!(fake.accepted_writes().len(), 1);

    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_acceptance();
    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });

    wait_for_close_intent(&controller);
    clock.advance_to(INTERVAL_NS);
    let final_neutral_reached = blocked.recv_timeout(CHANNEL_WAIT).is_ok();
    if final_neutral_reached {
        fake.release_blocked_write();
    }
    join_close_thread_after_bounded_wait(&clock, close_receiver, close_thread);
    let accepted_write_count = fake.accepted_writes().len();
    runtime.close().expect("Runtime close");

    assert!(matches!(
        poll_once(retained_release.as_mut()),
        Poll::Ready(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted))
    ));
    assert!(
        final_neutral_reached,
        "a settled release cannot consume a later resource close"
    );
    assert_eq!(accepted_write_count, 2);
    assert_eq!(
        fake.accepted_writes()[1].context.kind,
        WriteKind::Neutralize
    );
}

// conformance: controller.lease.close-older-settled-before-gate-claimed-record
#[test]
fn close_traverses_a_later_gate_claimed_release_after_an_older_retained_settlement() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let first = granted(&controller, &cancellation, None);
    let mut retained_first = pin!(first.neutralize_and_release());
    let second = granted(&controller, &cancellation, None);

    let blocked = fake.observe_next_write_blocked();
    let interrupted = fake.observe_next_write_interrupted();
    fake.block_next_write_after_acceptance();
    let second_release = second.neutralize_and_release();
    clock.advance_to(INTERVAL_NS);
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("second release neutral must reach its post-acceptance barrier");

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    assert!(
        interrupted.try_recv().is_err(),
        "the later gate-claimed record freezes close ordering after the older settlement"
    );
    fake.release_blocked_write();
    clock.advance_to(INTERVAL_NS.saturating_mul(2));
    assert!(matches!(
        block_on(second_release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    ));
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close must join after every retained record transitions");
    close_thread.join().expect("close thread");
    runtime.close().expect("Runtime close");

    assert!(matches!(
        poll_once(retained_first.as_mut()),
        Poll::Ready(Ok(AutomationLeaseReleaseOutcome::NeutralAccepted))
    ));
    assert_eq!(fake.accepted_writes().len(), 3);
}

// conformance: controller.lease.action-seal-future
#[test]
fn release_cancels_a_future_generation_action_without_waiting_for_its_target() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let accepted = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("accepted action");
    wait_terminal(&accepted);
    assert_eq!(accepted.snapshot().state, OperationState::Succeeded);

    let future = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::B))
        .expect("future action");
    let release = lease.neutralize_and_release();
    wait_terminal_with_teardown(&future, &clock, &runtime, &controller);
    assert_eq!(future.snapshot().state, OperationState::Cancelled);
    assert_eq!(fake.accepted_writes().len(), 1);

    clock.advance_to(INTERVAL_NS);
    assert_eq!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    assert_eq!(fake.accepted_writes().len(), 2);
    assert_eq!(
        fake.accepted_writes()[1].context.kind,
        WriteKind::Neutralize
    );

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    wait_for_close_intent(&controller);
    clock.advance_to(clock.now_ns().saturating_add(INTERVAL_NS));
    join_close_thread_after_bounded_wait(&clock, close_receiver, close_thread);
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.partial-stream-settlement
#[test]
fn partial_automation_write_failure_settles_stream_before_release() {
    let (_clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    fake.set_maximum_write_chunk(4);
    fake.return_invalid_write_count(2, 0);
    let action = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("partial action");
    wait_terminal(&action);
    assert_eq!(action.snapshot().state, OperationState::Failed);
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    assert!(fake.is_closed(), "partial prefix must close the stream");
    assert!(fake.accepted_writes().is_empty());

    assert!(matches!(
        block_on(lease.neutralize_and_release()),
        Ok(AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(_))
    ));
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.close-settlement
#[test]
fn close_settles_pending_acquire_actions_and_release_before_join() {
    let (clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_acceptance();
    let accepted = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::A))
        .expect("accepted action");
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("accepted action barrier reached");
    let cancelled = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::B))
        .expect("queued action");
    let acquire = controller.acquire_automation_lease(&cancellation, None);
    let (accepted_terminal, accepted_terminal_thread) = observe_terminal(accepted.clone());
    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });

    assert!(matches!(
        block_on(acquire),
        AutomationLeaseAcquireOutcome::Closed
    ));
    let release = lease.neutralize_and_release();
    accepted_terminal
        .recv_timeout(CHANNEL_WAIT)
        .expect("accepted action settled");
    accepted_terminal_thread
        .join()
        .expect("accepted action terminal observer");
    wait_terminal(&cancelled);
    assert_eq!(accepted.snapshot().state, OperationState::Succeeded);
    assert_eq!(cancelled.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        cancelled.snapshot().cancellation_reason,
        Some(CancellationReason::ParentClose)
    );

    clock.advance_to(clock.now_ns().saturating_add(INTERVAL_NS));
    assert_eq!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralAccepted)
    );
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close joined after settled release");
    close_thread.join().expect("close thread");
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.close-late-release
#[test]
fn close_settles_release_registered_after_close_seal() {
    let (_clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_after_acceptance();
    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("close neutral was accepted before returning");

    let mut release = pin!(lease.neutralize_and_release());
    fake.release_blocked_write();
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close joined after accepting its neutral");
    close_thread.join().expect("close thread");

    assert!(matches!(
        poll_once(release.as_mut()),
        Poll::Ready(Ok(
            AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(_)
        ))
    ));
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    runtime.close().expect("Runtime close");
}

// conformance: controller.lease.close-interrupt-release
#[test]
fn close_interrupts_release_neutral_before_acceptance_and_joins() {
    let (_clock, runtime, fake, controller) = connected();
    let cancellation = CancellationToken::root();
    let lease = granted(&controller, &cancellation, None);
    let blocked = fake.observe_next_write_blocked();
    fake.block_next_write_before_acceptance();
    let release = lease.neutralize_and_release();
    blocked
        .recv_timeout(CHANNEL_WAIT)
        .expect("release neutral barrier reached");

    let closing = controller.clone();
    let (close_sender, close_receiver) = mpsc::sync_channel(1);
    let close_thread = std::thread::spawn(move || {
        closing.close();
        close_sender.send(()).expect("close observer remains alive");
    });

    assert!(matches!(
        block_on(release),
        Ok(AutomationLeaseReleaseOutcome::NeutralNotDeliveredStreamSettled(_))
    ));
    close_receiver
        .recv_timeout(CHANNEL_WAIT)
        .expect("close joined after interrupted neutral");
    close_thread.join().expect("close thread");
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    assert!(fake.accepted_writes().is_empty());
    controller.close();
    runtime.close().expect("Runtime close");
}
