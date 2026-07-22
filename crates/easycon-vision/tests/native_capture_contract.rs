use std::sync::Mutex;
use std::time::Duration;

use easycon_native_sys::NativeErrorKind;
use easycon_native_sys::debug::counts;
use easycon_runtime::{
    CancellationToken, CloseOutcome, OperationState, Runtime, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_vision::{
    CaptureBackendKind, CaptureOptions, CaptureSession, CaptureSnapshotWait, CaptureState,
    NativeCaptureOptions, VisionErrorKind, VisionLimits, discover_capture_sources,
};

static NATIVE_CAPTURE_GATE: Mutex<()> = Mutex::new(());

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(4096, 64, 64, 4096, 16 * 1024, 1024).expect("capture limits")
}

fn session_options() -> CaptureOptions {
    CaptureOptions::new(5_000_000_000, limits()).expect("session options")
}

fn native_options() -> NativeCaptureOptions {
    NativeCaptureOptions::new(1_000_000_000, 100_000_000, 8).expect("native options")
}

#[test]
fn unqualified_native_backends_follow_the_rust_lifecycle() {
    let _gate = NATIVE_CAPTURE_GATE.lock().expect("native capture gate");
    let baseline = counts().expect("baseline native counts");
    let pattern = std::env::temp_dir().join("easycon-no-file-access/frame-%02d.bmp");
    let runtime = Runtime::new(std::sync::Arc::new(VirtualClock::default()));
    let session =
        CaptureSession::open_file(&runtime, &pattern, session_options(), native_options())
            .expect("file capture construction");
    let WaitResult::Completed(startup) = session
        .startup_operation()
        .wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("file capture startup did not complete");
    };
    assert_eq!(startup.state, OperationState::Failed);
    let fault = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
        .expect_err("unqualified file capture is a stable fault");
    assert_eq!(fault.kind(), VisionErrorKind::Faulted);
    assert_eq!(
        fault.native().expect("native diagnostic").kind(),
        NativeErrorKind::Unsupported
    );
    assert_eq!(session.state(), CaptureState::Faulted);
    assert_eq!(session.profile(), None);
    session.close().expect("file capture close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    drop(session);
    assert_eq!(counts().expect("file final counts"), baseline);

    let _ = discover_capture_sources(CaptureBackendKind::DirectShow)
        .expect("bounded DirectShow discovery");
    let _ = discover_capture_sources(CaptureBackendKind::MediaFoundation)
        .expect("bounded Media Foundation discovery");
    let runtime = Runtime::new(std::sync::Arc::new(VirtualClock::default()));
    let session = CaptureSession::open_device(
        &runtime,
        CaptureBackendKind::DirectShow,
        "dshow:0",
        "Unqualified DirectShow source",
        session_options(),
        native_options(),
    )
    .expect("hardware request construction");
    let WaitResult::Completed(startup) = session
        .startup_operation()
        .wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("hardware startup did not complete");
    };
    assert_eq!(startup.state, OperationState::Failed);
    let fault = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
        .expect_err("unqualified hardware is a stable fault");
    assert_eq!(fault.kind(), VisionErrorKind::Faulted);
    assert_eq!(
        fault.native().expect("native diagnostic").kind(),
        NativeErrorKind::Unsupported
    );
    session.close().expect("hardware capture close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    drop(session);
    assert_eq!(counts().expect("hardware final counts"), baseline);
}
