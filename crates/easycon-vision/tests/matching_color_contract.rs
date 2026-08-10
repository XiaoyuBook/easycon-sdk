use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};
use std::time::{Duration, Instant};

use easycon_native_sys::NativeErrorKind;
use easycon_runtime::{CancellationToken, CloseOutcome, Runtime, VirtualClock};
use easycon_vision::{
    EdgeMethod, HsvRange, Image, ImageErrorKind, NativePool, NativePoolOptions, PixelFormat, Roi,
    TemplateMethod, VisionErrorKind, VisionLimits, match_edge, match_template,
    native_resource_counts, preprocess_edge,
};

static NATIVE_COUNTER_GATE: RwLock<()> = RwLock::new(());

fn shared_native_gate() -> RwLockReadGuard<'static, ()> {
    let started = Instant::now();
    loop {
        match NATIVE_COUNTER_GATE.try_read() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(error)) => return error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "native counter gate did not release before the bounded test deadline"
                );
                std::thread::yield_now();
            }
        }
    }
}

fn exclusive_native_counter_gate() -> RwLockWriteGuard<'static, ()> {
    let started = Instant::now();
    loop {
        match NATIVE_COUNTER_GATE.try_write() {
            Ok(guard) => return guard,
            Err(TryLockError::Poisoned(error)) => return error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "native counter gate did not release before the bounded test deadline"
                );
                std::thread::yield_now();
            }
        }
    }
}

struct TestNative {
    runtime: Runtime,
    pool: NativePool,
    cancellation: CancellationToken,
}

impl TestNative {
    fn new() -> Self {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let pool = NativePool::new(
            &runtime,
            NativePoolOptions::new(2, 8).expect("native pool options"),
        )
        .expect("native pool");
        let cancellation = runtime.child_cancellation_token();
        Self {
            runtime,
            pool,
            cancellation,
        }
    }
}

impl Drop for TestNative {
    fn drop(&mut self) {
        self.pool.close().expect("native pool close");
        assert_eq!(self.runtime.close(), Ok(CloseOutcome::Closed));
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    let text = text.trim();
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("fixture hex"))
        .collect()
}

fn fixture(name: &str) -> Vec<u8> {
    match name {
        "template-search" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/template-search-gray.hex"
        )),
        "template-target" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/template-target-gray.hex"
        )),
        "edge-search" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/edge-search-gray.hex"
        )),
        "edge-target" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/edge-target-gray.hex"
        )),
        "edge-search-xy" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/edge-search-xy-gray.hex"
        )),
        "edge-target-xy" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/edge-target-xy-gray.hex"
        )),
        "edge-search-laplacian" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/edge-search-laplacian-gray.hex"
        )),
        "edge-target-laplacian" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/edge-target-laplacian-gray.hex"
        )),
        "hsv" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/operations/hsv-bgr-5x3.hex"
        )),
        _ => panic!("unknown fixture"),
    }
}

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(4096, 64, 64, 4096, 16_384, 1024).expect("test limits")
}

fn gray_image(name: &str, width: u32, height: u32) -> Image {
    Image::new(
        Arc::from(fixture(name)),
        width,
        height,
        usize::try_from(width).expect("small fixture width"),
        PixelFormat::Gray8,
        &limits(),
    )
    .expect("gray fixture")
}

fn bgr_from_gray(name: &str, width: u32, height: u32) -> Image {
    let pixels = fixture(name)
        .into_iter()
        .flat_map(|value| [value, value, value])
        .collect::<Vec<_>>();
    Image::new(
        Arc::from(pixels),
        width,
        height,
        usize::try_from(width).expect("small fixture width") * 3,
        PixelFormat::Bgr8,
        &limits(),
    )
    .expect("BGR fixture")
}

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= 0.000_01,
        "{actual} != {expected}"
    );
}

#[test]
fn normalized_template_modes_keep_location_and_frozen_score_mapping() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let search = gray_image("template-search", 6, 5);
    let target = gray_image("template-target", 3, 3);
    let cases = [
        (TemplateMethod::SqDiffNormed, 0.005_045_493_6, 0.994_954_5),
        (TemplateMethod::CCorrNormed, 0.998_100_8, 0.998_100_8),
        (TemplateMethod::CCoeffNormed, 0.994_260_25, 0.997_130_16),
    ];

    for (method, expected_raw, expected_score) in cases {
        let result = match_template(
            &native.pool,
            &native.cancellation,
            &search,
            &target,
            method,
            &limits(),
        )
        .expect("template match");
        assert_eq!((result.x(), result.y()), (2, 1));
        assert_close(result.raw(), expected_raw);
        assert_close(result.score(), expected_score);
    }
}

#[test]
fn edge_preprocess_pixels_and_final_match_are_independently_fixed() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let search = bgr_from_gray("edge-search", 18, 17);
    let target = bgr_from_gray("edge-target", 11, 11);
    let cases = [
        (EdgeMethod::Xy, "edge-search-xy", "edge-target-xy"),
        (
            EdgeMethod::Laplacian,
            "edge-search-laplacian",
            "edge-target-laplacian",
        ),
    ];

    for (method, expected_search, expected_target) in cases {
        let search_edge = preprocess_edge(
            &native.pool,
            &native.cancellation,
            &search,
            method,
            &limits(),
        )
        .expect("search edge");
        let target_edge = preprocess_edge(
            &native.pool,
            &native.cancellation,
            &target,
            method,
            &limits(),
        )
        .expect("target edge");
        assert_eq!(search_edge.format(), PixelFormat::Gray8);
        assert_eq!(target_edge.format(), PixelFormat::Gray8);
        assert_eq!(search_edge.pixels(), fixture(expected_search));
        assert_eq!(target_edge.pixels(), fixture(expected_target));

        let result = match_edge(
            &native.pool,
            &native.cancellation,
            &search,
            &target,
            method,
            &limits(),
        )
        .expect("edge match");
        assert_eq!((result.x(), result.y()), (4, 3));
        assert_close(result.raw(), 1.0);
        assert_close(result.score(), 1.0);
    }
}

#[test]
fn hsv_normal_wrap_full_none_ratio_threshold_and_absolute_bbox_are_exact() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let image = Image::new(
        Arc::from(fixture("hsv")),
        5,
        3,
        15,
        PixelFormat::Bgr8,
        &limits(),
    )
    .expect("HSV fixture");
    let roi = Roi::new(1, 0, 4, 3).expect("ROI");

    let normal = image
        .hsv_statistics(
            &native.pool,
            &native.cancellation,
            roi,
            HsvRange::new(20, 100, 200, 255, 200, 255).expect("normal range"),
            &limits(),
        )
        .expect("normal HSV");
    assert_eq!(normal.count(), 3);
    assert_close(normal.ratio(), 0.25);
    assert_eq!(normal.bounding_box(), Roi::new(2, 0, 3, 1).ok());
    assert!(normal.meets(0.25).expect("inclusive threshold"));
    assert!(!normal.meets(0.251).expect("higher threshold"));

    let wrap = image
        .hsv_statistics(
            &native.pool,
            &native.cancellation,
            roi,
            HsvRange::new(170, 10, 100, 255, 100, 255).expect("wrap range"),
            &limits(),
        )
        .expect("wrap HSV");
    assert_eq!(wrap.count(), 5);
    assert_close(wrap.ratio(), 5.0 / 12.0);
    assert_eq!(wrap.bounding_box(), Roi::new(2, 1, 3, 2).ok());

    let full = image
        .hsv_statistics(
            &native.pool,
            &native.cancellation,
            roi,
            HsvRange::new(0, 179, 0, 255, 0, 255).expect("full range"),
            &limits(),
        )
        .expect("full HSV");
    assert_eq!(full.count(), 12);
    assert_close(full.ratio(), 1.0);
    assert_eq!(full.bounding_box(), Some(roi));

    let none = image
        .hsv_statistics(
            &native.pool,
            &native.cancellation,
            roi,
            HsvRange::new(101, 110, 200, 255, 200, 255).expect("empty range"),
            &limits(),
        )
        .expect("empty HSV");
    assert_eq!(none.count(), 0);
    assert_close(none.ratio(), 0.0);
    assert_eq!(none.bounding_box(), None);
}

#[test]
fn matching_and_color_validation_are_deterministic_and_leak_free() {
    let _native_gate = exclusive_native_counter_gate();
    let native = TestNative::new();
    let baseline = native_resource_counts().expect("baseline");
    let search = gray_image("template-search", 6, 5);
    let target = gray_image("template-target", 3, 3);
    let bgr_target = native
        .pool
        .convert(&target, PixelFormat::Bgr8, &limits(), &native.cancellation)
        .expect("BGR target");
    assert_eq!(
        match_template(
            &native.pool,
            &native.cancellation,
            &search,
            &bgr_target,
            TemplateMethod::CCorrNormed,
            &limits(),
        )
        .expect_err("formats differ")
        .kind(),
        VisionErrorKind::Validation
    );
    assert_eq!(
        match_template(
            &native.pool,
            &native.cancellation,
            &target,
            &search,
            TemplateMethod::CCorrNormed,
            &limits(),
        )
        .expect_err("target exceeds search")
        .kind(),
        VisionErrorKind::Limit
    );

    let constant = Image::new(
        Arc::from(vec![7; 16]),
        4,
        4,
        4,
        PixelFormat::Gray8,
        &limits(),
    )
    .expect("constant image");
    let error = match_template(
        &native.pool,
        &native.cancellation,
        &constant,
        &constant,
        TemplateMethod::CCoeffNormed,
        &limits(),
    )
    .expect_err("undefined normalized coefficient");
    assert_eq!(error.kind(), VisionErrorKind::Native);
    let native_error = error.native().expect("native diagnostic is preserved");
    assert_eq!(native_error.kind(), NativeErrorKind::Backend);
    assert_eq!(
        native_error.message(),
        "normalized template denominator is zero"
    );

    assert_eq!(
        HsvRange::new(180, 0, 0, 255, 0, 255)
            .expect_err("hue ceiling")
            .kind(),
        ImageErrorKind::OutOfRange
    );
    assert_eq!(
        HsvRange::new(0, 179, 200, 100, 0, 255)
            .expect_err("S reverse")
            .kind(),
        ImageErrorKind::InvalidArgument
    );
    let image = Image::new(
        Arc::from(fixture("hsv")),
        5,
        3,
        15,
        PixelFormat::Bgr8,
        &limits(),
    )
    .expect("HSV fixture");
    assert_eq!(
        image
            .hsv_statistics(
                &native.pool,
                &native.cancellation,
                Roi::new(4, 2, 2, 1).expect("nonempty ROI"),
                HsvRange::new(0, 179, 0, 255, 0, 255).expect("full range"),
                &limits(),
            )
            .expect_err("ROI outside image")
            .kind(),
        VisionErrorKind::Limit
    );
    let stats = image
        .hsv_statistics(
            &native.pool,
            &native.cancellation,
            Roi::new(0, 0, 1, 1).expect("single pixel"),
            HsvRange::new(0, 179, 0, 255, 0, 255).expect("full range"),
            &limits(),
        )
        .expect("single pixel statistics");
    assert_eq!(
        stats.meets(f32::NAN).expect_err("finite threshold").kind(),
        ImageErrorKind::InvalidArgument
    );
    assert_eq!(
        stats.meets(1.1).expect_err("bounded threshold").kind(),
        ImageErrorKind::OutOfRange
    );
    assert_eq!(native_resource_counts().expect("final"), baseline);
}
