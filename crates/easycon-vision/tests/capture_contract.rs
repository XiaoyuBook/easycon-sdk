use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use easycon_runtime::{
    CancellationToken, CloseOutcome, OperationState, Runtime, RuntimeCounts, VirtualClock,
    WaitResult, WaitTimeout,
};
use easycon_vision::{
    CaptureBackendKind, CaptureOptions, CaptureProfile, CaptureSession, CaptureSnapshotWait,
    CaptureState, Image, PixelFormat, SyntheticCapture, VisionErrorKind, VisionLimits,
};

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(4096, 64, 64, 4096, 64 * 1024, 256).expect("capture limits")
}

fn profile() -> CaptureProfile {
    CaptureProfile::new(
        "synthetic:contract",
        "Synthetic contract source",
        CaptureBackendKind::Synthetic,
        2,
        2,
        PixelFormat::Bgr8,
        None,
    )
    .expect("capture profile")
}

fn frame(seed: u8) -> Image {
    let pixels = (0_u8..12)
        .map(|value| value.wrapping_add(seed))
        .collect::<Vec<_>>();
    Image::new(Arc::from(pixels), 2, 2, 6, PixelFormat::Bgr8, &limits()).expect("synthetic frame")
}

fn runtime() -> (Arc<VirtualClock>, Runtime) {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    (clock, runtime)
}

#[test]
fn latest_slot_replaces_without_mutating_borrowed_frames_and_close_interrupts_read() {
    let (clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");

    control.wait_for_read_calls(1).expect("first blocked read");
    control.push_frame(frame(0)).expect("first frame");
    control.wait_for_read_calls(2).expect("second blocked read");
    let first = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Until(100))
        .expect("first snapshot");
    assert_eq!(first.sequence(), 1);
    assert_eq!(first.timestamp_ns(), 0);
    assert_eq!(first.image().pixels(), frame(0).pixels());
    assert_eq!(session.state(), CaptureState::Streaming);
    assert_eq!(
        session.startup_operation().snapshot().state,
        OperationState::Succeeded
    );

    clock.advance_to(10);
    control.push_frame(frame(20)).expect("second frame");
    control.wait_for_read_calls(3).expect("third blocked read");
    let latest = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
        .expect("latest snapshot");
    assert_eq!(latest.sequence(), 2);
    assert_eq!(latest.timestamp_ns(), 10);
    assert_eq!(latest.image().pixels(), frame(20).pixels());
    assert_eq!(first.image().pixels(), frame(0).pixels());
    let cancelled = CancellationToken::root();
    cancelled.cancel();
    assert_eq!(
        session
            .snapshot(&cancelled, CaptureSnapshotWait::Poll)
            .expect("streaming latest frame wins over caller cancellation")
            .sequence(),
        2
    );

    session.close().expect("capture close");
    session.close().expect("repeated capture close");
    assert_eq!(session.state(), CaptureState::Closed);
    assert_eq!(
        session
            .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
            .expect_err("closed capture has no snapshot")
            .kind(),
        VisionErrorKind::Closed
    );
    let counts = control.counts();
    assert_eq!(counts.open_calls, 1);
    assert_eq!(counts.read_calls, 3);
    assert_eq!(counts.close_calls, 1);
    assert_eq!(counts.finalize_calls, 1);
    assert!(counts.interrupt_calls >= 1);
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 1,
        }
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 0,
        }
    );
}

#[test]
fn first_frame_deadline_faults_opening_and_interrupts_blocked_open() {
    let (clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), true);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(50, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_open_calls(1).expect("blocked open");
    assert_eq!(session.state(), CaptureState::Opening);
    assert_eq!(
        session
            .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
            .expect_err("poll before first frame")
            .kind(),
        VisionErrorKind::NoFrame
    );

    clock.advance_to(50);
    control.wait_for_close_calls(1).expect("deadline cleanup");
    assert_eq!(session.state(), CaptureState::Faulted);
    assert_eq!(
        session
            .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Until(100))
            .expect_err("deadline fault")
            .kind(),
        VisionErrorKind::Deadline
    );
    session.close().expect("faulted close still finalizes");
    let startup = session.startup_operation().snapshot();
    assert_eq!(startup.state, OperationState::Cancelled);
    assert_eq!(control.counts().finalize_calls, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn read_fault_wins_over_a_stale_latest_frame() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    control.push_frame(frame(1)).expect("frame");
    control.wait_for_read_calls(2).expect("next read");
    let borrowed = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
        .expect("borrowed frame");
    control
        .fail_read("scripted hot unplug")
        .expect("read fault");
    control.wait_for_close_calls(1).expect("fault cleanup");
    assert_eq!(session.state(), CaptureState::Faulted);
    let error = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
        .expect_err("fault suppresses stale latest");
    assert_eq!(error.kind(), VisionErrorKind::Faulted);
    assert!(error.message().contains("scripted hot unplug"));
    assert_eq!(borrowed.sequence(), 1);
    assert_eq!(borrowed.image().pixels(), frame(1).pixels());
    session.close().expect("fault close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn snapshot_cancel_and_concurrent_close_have_one_cleanup_owner() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let session = Arc::new(
        CaptureSession::open_synthetic(
            &runtime,
            backend,
            CaptureOptions::new(1_000, limits()).expect("capture options"),
        )
        .expect("capture construction"),
    );
    control.wait_for_read_calls(1).expect("blocked read");
    let cancelled = CancellationToken::root();
    cancelled.cancel();
    assert_eq!(
        session
            .snapshot(&cancelled, CaptureSnapshotWait::Until(100))
            .expect_err("cancelled snapshot")
            .kind(),
        VisionErrorKind::Cancelled
    );

    let barrier = Arc::new(Barrier::new(3));
    let left_session = Arc::clone(&session);
    let left_barrier = Arc::clone(&barrier);
    let left = thread::spawn(move || {
        left_barrier.wait();
        left_session.close()
    });
    let right_session = Arc::clone(&session);
    let right_barrier = Arc::clone(&barrier);
    let right = thread::spawn(move || {
        right_barrier.wait();
        right_session.close()
    });
    barrier.wait();
    assert_eq!(left.join().expect("left close"), Ok(()));
    assert_eq!(right.join().expect("right close"), Ok(()));
    assert_eq!(control.counts().close_calls, 1);
    assert_eq!(control.counts().finalize_calls, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn final_session_drop_requests_stop_and_runtime_owns_finalization() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    drop(session);
    control.wait_for_interrupt_calls(1).expect("drop interrupt");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(control.counts().close_calls, 1);
    assert_eq!(control.counts().finalize_calls, 1);
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 0,
        }
    );
}

#[test]
fn open_failure_and_stream_end_are_faults_with_terminal_startup_cleanup() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    control
        .fail_open("scripted open failure")
        .expect("open failpoint");
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control
        .wait_for_close_calls(1)
        .expect("failed open cleanup");
    let error = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Until(100))
        .expect_err("open failure");
    assert_eq!(error.kind(), VisionErrorKind::Faulted);
    assert!(error.message().contains("scripted open failure"));
    session.close().expect("failed-open finalization");
    assert_eq!(
        session.startup_operation().snapshot().state,
        OperationState::Failed
    );

    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let ended = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("second capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    control.end().expect("end stream");
    control.wait_for_close_calls(1).expect("end cleanup");
    assert_eq!(
        ended
            .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
            .expect_err("ended stream")
            .kind(),
        VisionErrorKind::Faulted
    );
    ended.close().expect("ended close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn snapshot_deadline_is_waiter_local_and_does_not_fault_the_session() {
    let (clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), true);
    let session = Arc::new(
        CaptureSession::open_synthetic(
            &runtime,
            backend,
            CaptureOptions::new(1_000, limits()).expect("capture options"),
        )
        .expect("capture construction"),
    );
    control.wait_for_open_calls(1).expect("blocked open");
    let barrier = Arc::new(Barrier::new(2));
    let waiter_session = Arc::clone(&session);
    let waiter_barrier = Arc::clone(&barrier);
    let waiter = thread::spawn(move || {
        waiter_barrier.wait();
        waiter_session.snapshot(&CancellationToken::root(), CaptureSnapshotWait::Until(50))
    });
    barrier.wait();
    clock.advance_to(50);
    assert_eq!(
        waiter
            .join()
            .expect("snapshot waiter")
            .expect_err("snapshot deadline")
            .kind(),
        VisionErrorKind::Deadline
    );
    assert_eq!(session.state(), CaptureState::Opening);
    session.close().expect("capture close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn mismatched_frame_profile_faults_before_publication() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    let mismatched = Image::new(
        Arc::from(vec![0_u8; 3]),
        1,
        1,
        3,
        PixelFormat::Bgr8,
        &limits(),
    )
    .expect("mismatched image");
    control.push_frame(mismatched).expect("mismatched frame");
    control.wait_for_close_calls(1).expect("mismatch cleanup");
    assert_eq!(
        session
            .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
            .expect_err("profile mismatch")
            .kind(),
        VisionErrorKind::Faulted
    );
    session.close().expect("mismatch close");
    assert_eq!(
        session.startup_operation().snapshot().state,
        OperationState::Failed
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn capture_profile_stride_and_decoded_layout_are_checked_before_read() {
    assert_eq!(
        CaptureProfile::new_with_stride(
            "synthetic:short-stride",
            "Short stride",
            CaptureBackendKind::Synthetic,
            2,
            2,
            5,
            PixelFormat::Bgr8,
            None,
        )
        .expect_err("stride shorter than row bytes")
        .kind(),
        VisionErrorKind::Validation
    );

    let oversized = CaptureProfile::new_with_stride(
        "synthetic:oversized-stride",
        "Oversized stride",
        CaptureBackendKind::Synthetic,
        2,
        2,
        257,
        PixelFormat::Bgr8,
        None,
    )
    .expect("profile construction is independent of session limits");
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(oversized, false);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control
        .wait_for_close_calls(1)
        .expect("invalid profile cleanup");
    let WaitResult::Completed(startup) = session
        .startup_operation()
        .wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("invalid profile startup did not finish cleanup");
    };
    assert_eq!(session.state(), CaptureState::Faulted);
    assert_eq!(
        session
            .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
            .expect_err("profile exceeds limits")
            .kind(),
        VisionErrorKind::Limit
    );
    assert_eq!(startup.state, OperationState::Failed);
    session.close().expect("invalid profile close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn unconsumed_finalize_retries_without_losing_the_worker_close_error() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    control.fail_close("scripted close diagnostic");
    control.fail_finalize(1);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    let first = session.close().expect_err("unconsumed finalize");
    assert!(first.message().contains("finalize failure"));
    assert_eq!(session.state(), CaptureState::Stopping);
    assert_eq!(runtime.counts().active_resources, 1);
    let second = session.close().expect_err("saved close diagnostic");
    assert!(second.message().contains("scripted close diagnostic"));
    assert_eq!(session.state(), CaptureState::Closed);
    assert_eq!(control.counts().close_calls, 1);
    assert_eq!(control.counts().finalize_calls, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn runtime_close_quarantines_an_unconsumed_owner_after_the_session_is_dropped() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    control.fail_finalize(1);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    drop(session);

    let CloseOutcome::Failed(report) = runtime.close().expect("external Runtime close") else {
        panic!("unconsumed capture owner must prevent a false Closed outcome");
    };
    assert_eq!(
        report.phase,
        easycon_runtime::ClosePhase::RegistryConvergence
    );
    assert_eq!(runtime.close(), Ok(CloseOutcome::Failed(report)));
    assert_eq!(runtime.counts().active_resources, 1);
    assert_eq!(control.counts().close_calls, 1);
    assert_eq!(control.counts().finalize_calls, 0);
}

#[test]
fn interrupt_drain_diagnostic_survives_successful_destroy() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    control.fail_interrupt("scripted interrupt diagnostic");
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    let error = session.close().expect_err("interrupt drain diagnostic");
    assert!(error.message().contains("scripted interrupt diagnostic"));
    assert_eq!(session.state(), CaptureState::Closed);
    assert_eq!(
        session
            .close()
            .expect_err("repeated close preserves diagnostic"),
        error
    );
    assert_eq!(control.counts().close_calls, 1);
    assert_eq!(control.counts().finalize_calls, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn closed_runtime_rejection_finalizes_the_never_started_backend() {
    let (_clock, runtime) = runtime();
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    control.fail_finalize(1);
    let error = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect_err("closed Runtime rejects capture");
    assert_eq!(error.kind(), VisionErrorKind::Closed);
    assert_eq!(control.counts().open_calls, 0);
    assert_eq!(control.counts().close_calls, 0);
    assert_eq!(control.counts().finalize_calls, 1);
    assert!(error.message().contains("cleanup failed"));
}

#[test]
fn deadline_overflow_finalizes_backend_before_operation_creation() {
    let clock = Arc::new(VirtualClock::new(u64::MAX - 10));
    let runtime = Runtime::new(clock);
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let error = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(50, limits()).expect("capture options"),
    )
    .expect_err("deadline overflow");
    assert_eq!(error.kind(), VisionErrorKind::Limit);
    assert_eq!(control.counts().open_calls, 0);
    assert_eq!(control.counts().finalize_calls, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn worker_panic_hands_off_and_finalizes_before_runtime_reports_failure() {
    let (_clock, runtime) = runtime();
    let (backend, control) = SyntheticCapture::controlled(profile(), false);
    let session = CaptureSession::open_synthetic(
        &runtime,
        backend,
        CaptureOptions::new(1_000, limits()).expect("capture options"),
    )
    .expect("capture construction");
    control.wait_for_read_calls(1).expect("blocked read");
    control.panic_read().expect("read panic failpoint");
    control.wait_for_close_calls(1).expect("panic cleanup");
    assert_eq!(session.state(), CaptureState::Faulted);
    assert_eq!(
        session.close().expect_err("panicked worker").kind(),
        VisionErrorKind::Internal
    );
    assert_eq!(control.counts().finalize_calls, 1);
    assert!(matches!(runtime.close(), Ok(CloseOutcome::Failed(_))));
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 0,
        }
    );
}

#[test]
fn first_frame_and_deadline_race_has_one_coherent_winner() {
    for _ in 0..64 {
        let (clock, runtime) = runtime();
        let (backend, control) = SyntheticCapture::controlled(profile(), false);
        let session = CaptureSession::open_synthetic(
            &runtime,
            backend,
            CaptureOptions::new(50, limits()).expect("capture options"),
        )
        .expect("capture construction");
        control.wait_for_read_calls(1).expect("blocked first read");
        let barrier = Arc::new(Barrier::new(2));
        let producer_control = control.clone();
        let producer_barrier = Arc::clone(&barrier);
        let producer = thread::spawn(move || {
            producer_barrier.wait();
            producer_control.push_frame(frame(7))
        });
        barrier.wait();
        clock.advance_to(50);
        let _ = producer.join().expect("frame producer");
        let WaitResult::Completed(startup) = session
            .startup_operation()
            .wait(WaitTimeout::For(Duration::from_secs(5)))
        else {
            panic!("startup race did not complete");
        };
        match startup.state {
            OperationState::Succeeded => {
                assert_eq!(session.state(), CaptureState::Streaming);
                assert_eq!(
                    session
                        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
                        .expect("winning first frame")
                        .sequence(),
                    1
                );
                assert_eq!(control.counts().interrupt_calls, 0);
            }
            OperationState::Cancelled => {
                assert_eq!(session.state(), CaptureState::Faulted);
                assert_eq!(
                    session
                        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
                        .expect_err("winning deadline")
                        .kind(),
                    VisionErrorKind::Deadline
                );
            }
            state => panic!("incoherent startup race terminal: {state:?}"),
        }
        session.close().expect("race capture close");
        assert_eq!(control.counts().finalize_calls, 1);
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }
}
