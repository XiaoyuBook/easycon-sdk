use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use easycon_runtime::{CloseOutcome, Runtime, VirtualClock};
use easycon_vision::{
    Image, NativePool, NativePoolOptions, OcrConfig, OcrEngineMode, OcrPageSegmentation, OcrPool,
    PixelFormat, VisionErrorKind, VisionLimits,
};

static OCR_TEST_LOCK: Mutex<()> = Mutex::new(());

fn serial_ocr_test() -> MutexGuard<'static, ()> {
    OCR_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn test_model_root() -> PathBuf {
    std::env::var_os("EASYCON_VISION_TEST_TESSDATA")
        .map(PathBuf::from)
        .expect("EASYCON_VISION_TEST_TESSDATA must name the provisioned test model")
}

fn fixture_image(limits: &VisionLimits) -> Image {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spec/fixtures/vision/ocr/easycon-gray.hex");
    let text = std::fs::read_to_string(path).expect("OCR fixture");
    let pixels: Vec<u8> = text
        .trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).expect("ASCII"), 16).expect("hex"))
        .collect();
    Image::new(Arc::from(pixels), 273, 77, 273, PixelFormat::Gray8, limits)
        .expect("OCR fixture image")
}

#[test]
fn runtime_supervised_pool_reuses_ocr_engines_and_closes_to_zero() {
    let _serial = serial_ocr_test();
    let baseline = easycon_vision::native_resource_counts().expect("baseline counts");
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let native = NativePool::new(&runtime, NativePoolOptions::new(2, 8).expect("pool limits"))
        .expect("native pool");
    let config = OcrConfig::new(
        &test_model_root(),
        "eng",
        OcrEngineMode::Default,
        OcrPageSegmentation::SingleLine,
        4096,
    )
    .expect("OCR config");
    let ocr = OcrPool::new(&runtime, config, 1).expect("OCR pool");
    let limits = VisionLimits::default();
    let image = fixture_image(&limits);
    let cancellation = runtime.child_cancellation_token();

    for _ in 0..2 {
        let output = ocr
            .recognize(&native, &image, &limits, &cancellation)
            .expect("OCR output");
        assert!(
            output
                .text()
                .split_whitespace()
                .collect::<String>()
                .contains("EASCON")
        );
        assert!((0.0..=1.0).contains(&output.confidence()));
    }
    assert_eq!(ocr.counts().created, 1);
    assert_eq!(ocr.counts().idle, 1);

    native.close().expect("native pool close");
    ocr.close().expect("OCR pool close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(
        easycon_vision::native_resource_counts().expect("final counts"),
        baseline
    );
}

#[test]
fn missing_model_is_rejected_without_falling_back_to_the_working_directory() {
    let _serial = serial_ocr_test();
    let error = OcrConfig::new(
        Path::new("Z:/easycon-sdk-missing-tessdata"),
        "eng",
        OcrEngineMode::Default,
        OcrPageSegmentation::SingleLine,
        4096,
    )
    .expect_err("missing model");
    assert_eq!(error.kind(), VisionErrorKind::ModelNotFound);
}

#[test]
fn runtime_close_orders_native_workers_before_idle_ocr_engine_release() {
    let _serial = serial_ocr_test();
    let baseline = easycon_vision::native_resource_counts().expect("baseline counts");
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let native = NativePool::new(&runtime, NativePoolOptions::new(1, 2).expect("pool limits"))
        .expect("native pool");
    let config = OcrConfig::new(
        &test_model_root(),
        "eng",
        OcrEngineMode::Default,
        OcrPageSegmentation::SingleLine,
        4096,
    )
    .expect("OCR config");
    let ocr = OcrPool::new(&runtime, config, 1).expect("OCR pool");
    let limits = VisionLimits::default();
    ocr.recognize(
        &native,
        &fixture_image(&limits),
        &limits,
        &runtime.child_cancellation_token(),
    )
    .expect("OCR output");
    assert_eq!(ocr.counts().idle, 1);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert!(native.counts().closed);
    assert!(!ocr.counts().open);
    assert_eq!(ocr.counts().created, 0);
    assert_eq!(
        easycon_vision::native_resource_counts().expect("final counts"),
        baseline
    );
}
