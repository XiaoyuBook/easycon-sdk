use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use easycon_runtime::{CancellationToken, CloseOutcome, Runtime, VirtualClock};
use easycon_vision::{
    Frame, Image, LabelEvaluator, NativePool, NativePoolOptions, OcrConfig, OcrEngineMode,
    OcrPageSegmentation, OcrPool, PixelFormat, VisionErrorKind, VisionLimits, parse_legacy_il,
};

static LABEL_TEST_LOCK: Mutex<()> = Mutex::new(());

fn serial_test() -> MutexGuard<'static, ()> {
    LABEL_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/fixtures/vision")
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(fixture_root().join("labels").join(name)).expect("label fixture")
}

fn decode_hex(text: &str) -> Vec<u8> {
    let text = text.trim();
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex fixture"))
        .collect()
}

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(64 * 1024, 1024, 1024, 1024 * 1024, 4 * 1024 * 1024, 4096)
        .expect("evaluation limits")
}

fn native_runtime() -> (Runtime, NativePool, CancellationToken) {
    let runtime = Runtime::new(Arc::new(VirtualClock::default()));
    let pool = NativePool::new(
        &runtime,
        NativePoolOptions::new(2, 16).expect("native options"),
    )
    .expect("native pool");
    let cancellation = runtime.child_cancellation_token();
    (runtime, pool, cancellation)
}

fn bgr_frame(sequence: u64, timestamp_ns: u64, limits: &VisionLimits) -> Arc<Frame> {
    let mut pixels = [17_u8, 23, 31].repeat(16);
    let target = decode_hex(include_str!(
        "../../../spec/fixtures/vision/codec/expected-bgr.hex"
    ));
    for row in 0..2 {
        let target_start = row * 6;
        let frame_start = ((row + 1) * 4 + 1) * 3;
        pixels[frame_start..frame_start + 6]
            .copy_from_slice(&target[target_start..target_start + 6]);
    }
    Arc::new(
        Frame::new(
            Image::new(Arc::from(pixels), 4, 4, 12, PixelFormat::Bgr8, limits)
                .expect("BGR frame image"),
            sequence,
            timestamp_ns,
        )
        .expect("frame"),
    )
}

#[test]
fn image_label_evaluates_one_owned_frame_and_returns_absolute_location() {
    let _serial = serial_test();
    let (runtime, native, cancellation) = native_runtime();
    let limits = limits();
    let report = parse_legacy_il(
        "valid-png.IL",
        &fixture("valid-png.IL"),
        &native,
        &limits,
        &cancellation,
    )
    .expect("parse label");
    let label = report.label().expect("valid label");
    let evaluator = LabelEvaluator::new(native.clone(), limits);
    let frame = bgr_frame(41, 12_345, &limits);
    let _newer_unobserved_frame = bgr_frame(42, 12_346, &limits);
    let result = evaluator
        .evaluate(label, Arc::clone(&frame), &cancellation)
        .expect("label evaluation");
    assert_eq!((result.x(), result.y()), (1, 1));
    assert!((result.score() - 1.0).abs() <= 0.000_01);
    assert_eq!(result.sequence(), 41);
    assert_eq!(result.timestamp_ns(), 12_345);
    assert!(result.recognized_text().is_none());
    drop(result);
    drop(frame);
    drop(evaluator);
    native.close().expect("native close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn frame_bounds_and_precancel_are_explicit_without_native_fallback() {
    let _serial = serial_test();
    let (runtime, native, cancellation) = native_runtime();
    let limits = limits();
    let report = parse_legacy_il(
        "frame-outside.IL",
        &fixture("frame-outside.IL"),
        &native,
        &limits,
        &cancellation,
    )
    .expect("parse label");
    let evaluator = LabelEvaluator::new(native.clone(), limits);
    let frame = bgr_frame(7, 99, &limits);
    assert_eq!(
        evaluator
            .evaluate(
                report.label().expect("valid label"),
                Arc::clone(&frame),
                &cancellation
            )
            .expect_err("range exceeds frame")
            .kind(),
        VisionErrorKind::Limit
    );

    let valid = parse_legacy_il(
        "valid-png.IL",
        &fixture("valid-png.IL"),
        &native,
        &limits,
        &cancellation,
    )
    .expect("valid parse");
    let cancelled = CancellationToken::root();
    cancelled.cancel();
    assert_eq!(
        evaluator
            .evaluate(valid.label().expect("valid label"), frame, &cancelled)
            .expect_err("pre-cancelled evaluation")
            .kind(),
        VisionErrorKind::Cancelled
    );
    drop(evaluator);
    native.close().expect("native close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}

#[test]
fn actual_ocr_score_preserves_raw_text_confidence_and_frame_metadata() {
    let _serial = serial_test();
    let baseline = easycon_vision::native_resource_counts().expect("baseline");
    let (runtime, native, cancellation) = native_runtime();
    let limits = limits();
    let model_root = std::env::var_os("EASYCON_VISION_TEST_TESSDATA")
        .map(PathBuf::from)
        .expect("test model path");
    let config = OcrConfig::new(
        &model_root,
        "eng",
        OcrEngineMode::Default,
        OcrPageSegmentation::SingleLine,
        4096,
    )
    .expect("OCR config");
    let ocr = OcrPool::new(&runtime, config, 1).expect("OCR pool");
    let report = parse_legacy_il(
        "valid-ocr.IL",
        &fixture("valid-ocr.IL"),
        &native,
        &limits,
        &cancellation,
    )
    .expect("OCR label parse");
    let pixels = decode_hex(include_str!(
        "../../../spec/fixtures/vision/ocr/easycon-gray.hex"
    ));
    let frame = Arc::new(
        Frame::new(
            Image::new(Arc::from(pixels), 273, 77, 273, PixelFormat::Gray8, &limits)
                .expect("OCR image"),
            88,
            765_432,
        )
        .expect("OCR frame"),
    );
    let evaluator = LabelEvaluator::new(native.clone(), limits).with_ocr_pool(ocr.clone());
    let result = evaluator
        .evaluate(report.label().expect("OCR label"), frame, &cancellation)
        .expect("OCR label evaluation");
    assert_eq!(result.sequence(), 88);
    assert_eq!(result.timestamp_ns(), 765_432);
    assert_eq!((result.x(), result.y()), (0, 0));
    assert_eq!(result.text_similarity(), Some(1.0));
    let confidence = result.ocr_confidence().expect("OCR confidence");
    assert_eq!(result.score(), confidence);
    assert_eq!(
        result
            .recognized_text()
            .expect("recognized text")
            .split_whitespace()
            .collect::<String>(),
        "EASCON"
    );
    drop(result);
    drop(evaluator);
    native.close().expect("native close");
    ocr.close().expect("OCR close");
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    assert_eq!(
        easycon_vision::native_resource_counts().expect("final"),
        baseline
    );
}

#[test]
fn ocr_uses_only_the_target_frame_bounds_and_validates_pool_before_native_work() {
    let _serial = serial_test();
    let (runtime, native, cancellation) = native_runtime();
    let limits = limits();
    let bytes = String::from_utf8(fixture("valid-ocr.IL"))
        .expect("UTF-8 fixture")
        .replace(r#""RangeWidth":273"#, r#""RangeWidth":300"#);
    let report = parse_legacy_il(
        "target-only.IL",
        bytes.as_bytes(),
        &native,
        &limits,
        &cancellation,
    )
    .expect("OCR label parse");
    let pixels = decode_hex(include_str!(
        "../../../spec/fixtures/vision/ocr/easycon-gray.hex"
    ));
    let frame = Arc::new(
        Frame::new(
            Image::new(Arc::from(pixels), 273, 77, 273, PixelFormat::Gray8, &limits)
                .expect("OCR image"),
            89,
            765_433,
        )
        .expect("OCR frame"),
    );

    let without_ocr = LabelEvaluator::new(native.clone(), limits);
    native.close().expect("native close");
    assert_eq!(
        without_ocr
            .evaluate(report.label().expect("OCR label"), frame, &cancellation)
            .expect_err("missing OCR pool must win before native admission")
            .kind(),
        VisionErrorKind::Validation
    );
    drop(without_ocr);
    assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
}
