use std::sync::Mutex;

use easycon_native_sys::NativeErrorKind;
use easycon_native_sys::capture::{
    CaptureBackend, CaptureDestroyOutcome, CaptureHandle, CaptureOptions, discover,
};
use easycon_native_sys::codec::ImageLimits;
use easycon_native_sys::debug::counts;

static CAPTURE_TEST_GATE: Mutex<()> = Mutex::new(());

fn options() -> CaptureOptions {
    CaptureOptions::new(
        ImageLimits::new(4096, 64, 64, 4096, 16 * 1024, 1024).expect("image limits"),
        1_000_000_000,
        100_000_000,
        8,
    )
    .expect("capture options")
}

#[test]
fn safe_capture_rejects_unqualified_file_before_access_and_explicitly_destroys() {
    let _gate = CAPTURE_TEST_GATE.lock().expect("capture test gate");
    let baseline = counts().expect("baseline counts");
    let pattern = std::env::temp_dir().join("easycon-no-file-access/frame-%02d.bmp");
    let source = pattern.to_str().expect("temporary path is UTF-8");

    let (mut capture, interrupt) =
        CaptureHandle::create(CaptureBackend::File, source, options()).expect("capture create");
    assert_eq!(
        counts().expect("owners").live_handles,
        baseline.live_handles + 2
    );
    assert_eq!(
        capture
            .open()
            .expect_err("file capture is not qualified")
            .kind(),
        NativeErrorKind::Unsupported
    );
    interrupt.request_stop().expect("capture interrupt");
    capture.close().expect("capture close");
    let CaptureDestroyOutcome::Consumed { diagnostic } = capture.destroy() else {
        panic!("normal destroy must consume its owner");
    };
    assert_eq!(diagnostic, None);
    drop(interrupt);
    assert_eq!(counts().expect("final counts"), baseline);
}

#[test]
fn hardware_open_is_unsupported_before_device_access() {
    let _gate = CAPTURE_TEST_GATE.lock().expect("capture test gate");
    let baseline = counts().expect("baseline counts");
    let (mut capture, interrupt) =
        CaptureHandle::create(CaptureBackend::DirectShow, "dshow:0", options())
            .expect("capture request create");
    assert_eq!(
        capture
            .open()
            .expect_err("hardware support is not qualified")
            .kind(),
        NativeErrorKind::Unsupported
    );
    capture.close().expect("close unopened capture");
    assert!(matches!(
        capture.destroy(),
        CaptureDestroyOutcome::Consumed { diagnostic: None }
    ));
    drop(interrupt);
    assert_eq!(counts().expect("final counts"), baseline);
}

#[test]
fn windows_discovery_returns_only_bounded_owned_descriptors() {
    let _gate = CAPTURE_TEST_GATE.lock().expect("capture test gate");
    let baseline = counts().expect("baseline counts");
    for backend in [CaptureBackend::DirectShow, CaptureBackend::MediaFoundation] {
        let descriptors = discover(backend).expect("Windows capture discovery");
        assert!(descriptors.len() <= 64);
        for descriptor in descriptors {
            assert_eq!(descriptor.backend(), backend);
            assert!(!descriptor.source_id().is_empty());
            assert!(descriptor.source_id().len() <= 4096);
            assert!(!descriptor.display_name().is_empty());
            assert!(descriptor.display_name().len() <= 1024);
        }
    }
    assert_eq!(counts().expect("final counts"), baseline);
}
