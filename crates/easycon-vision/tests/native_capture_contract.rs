use std::sync::Mutex;
use std::time::Duration;

use easycon_native_sys::debug::counts;
use easycon_runtime::{
    CancellationToken, CloseOutcome, OperationState, Runtime, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_vision::{
    CaptureBackendKind, CaptureOptions, CaptureSession, CaptureSnapshotWait,
    CaptureSourceDescriptor, CaptureState, FileCapture, NativeCaptureOptions, VisionError,
    VisionLimits, discover_capture_sources,
};

static NATIVE_CAPTURE_GATE: Mutex<()> = Mutex::new(());

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(4096, 64, 64, 4096, 16 * 1024, 1024).expect("capture limits")
}

fn session_options() -> CaptureOptions {
    CaptureOptions::new(5_000_000_000, limits()).expect("session options")
}

#[test]
fn prevalidated_file_uses_the_platform_neutral_capture_lifecycle() {
    let _gate = NATIVE_CAPTURE_GATE.lock().expect("native capture gate");
    let baseline = counts().expect("baseline native counts");
    let runtime = Runtime::new(std::sync::Arc::new(VirtualClock::default()));
    let image = easycon_vision::Image::new(
        std::sync::Arc::from([1_u8, 2, 3].as_slice()),
        1,
        1,
        3,
        easycon_vision::PixelFormat::Bgr8,
        &limits(),
    )
    .expect("file frame");
    let file = FileCapture::new("file:fixture", "Fixture frame", image).expect("file capture");
    let session =
        easycon_vision::CaptureSession::open_prevalidated_file(&runtime, file, session_options())
            .expect("file capture construction");
    let WaitResult::Completed(startup) = session
        .startup_operation()
        .wait(WaitTimeout::For(Duration::from_secs(5)))
    else {
        panic!("file capture startup did not complete");
    };
    assert_eq!(startup.state, OperationState::Succeeded);
    let frame = session
        .snapshot(&CancellationToken::root(), CaptureSnapshotWait::Poll)
        .expect("prevalidated file frame");
    assert_eq!(frame.image().pixels(), [1, 2, 3]);
    assert_eq!(session.state(), CaptureState::Streaming);
    assert_eq!(
        session.profile().expect("file profile").backend(),
        CaptureBackendKind::File
    );
    session.close().expect("file capture close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    drop(session);
    assert_eq!(counts().expect("file final counts"), baseline);

    let _open_device_signature: fn(
        &Runtime,
        CaptureSourceDescriptor,
        CaptureOptions,
        NativeCaptureOptions,
    ) -> Result<CaptureSession, VisionError> = CaptureSession::open_device;
    #[cfg(target_os = "windows")]
    {
        let descriptors = discover_capture_sources().expect("Windows device discovery");
        assert!(
            descriptors
                .iter()
                .all(|descriptor| descriptor.backend() == CaptureBackendKind::SystemDevice)
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| !descriptor.adapter_diagnostic().is_empty())
        );
    }
    #[cfg(not(target_os = "windows"))]
    {
        assert_eq!(
            discover_capture_sources()
                .expect_err("unqualified platform discovery must fail closed")
                .kind(),
            easycon_vision::VisionErrorKind::BackendUnavailable
        );
    }
    assert_eq!(counts().expect("discovery final counts"), baseline);
}
