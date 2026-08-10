use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError};
use std::time::{Duration, Instant};

use easycon_runtime::{CancellationToken, CloseOutcome, Runtime, VirtualClock};
use easycon_vision::{
    Frame, Image, ImageErrorKind, NativePool, NativePoolOptions, PixelFormat, Roi, VisionErrorKind,
    VisionLimits, native_resource_counts,
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
        "bmp" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/codec/bgr-2x2.bmp.hex"
        )),
        "png" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/codec/bgr-2x2.png.hex"
        )),
        "bgr" => decode_hex(include_str!(
            "../../../spec/fixtures/vision/codec/expected-bgr.hex"
        )),
        _ => panic!("unknown fixture"),
    }
}

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(4096, 64, 64, 4096, 16_384, 1024).expect("test limits")
}

#[test]
fn image_validates_layout_limits_and_immutable_rows() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let limits = limits();
    let padded: Arc<[u8]> = vec![0, 0, 255, 0, 255, 0, 9, 9, 255, 0, 0, 255, 255, 255, 8, 8].into();
    let image = Image::new(padded, 2, 2, 8, PixelFormat::Bgr8, &limits).expect("padded image");

    assert_eq!(image.width(), 2);
    assert_eq!(image.height(), 2);
    assert_eq!(image.stride(), 8);
    assert_eq!(image.format(), PixelFormat::Bgr8);
    assert_eq!(
        image.rows().collect::<Vec<_>>(),
        vec![&image.pixels()[0..6], &image.pixels()[8..14]]
    );
    let padded_round_trip = native
        .pool
        .decode(
            &native
                .pool
                .encode_png(&image, &limits, &native.cancellation)
                .expect("padded PNG encode"),
            &limits,
            &native.cancellation,
        )
        .expect("padded PNG decode");
    assert_eq!(padded_round_trip.stride(), 6);
    assert_eq!(
        padded_round_trip.pixels(),
        &[0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255]
    );

    let too_short: Arc<[u8]> = vec![0; 11].into();
    assert_eq!(
        Image::new(too_short, 2, 2, 6, PixelFormat::Bgr8, &limits)
            .expect_err("short image")
            .kind(),
        ImageErrorKind::InvalidArgument
    );
    let too_wide = Image::new(
        vec![0; 65 * 3].into(),
        65,
        1,
        195,
        PixelFormat::Bgr8,
        &limits,
    )
    .expect_err("width limit");
    assert_eq!(too_wide.kind(), ImageErrorKind::OutOfRange);
}

#[test]
fn frame_and_crop_keep_owned_pixels_after_the_source_is_dropped() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let limits = limits();
    let image = Image::new(fixture("bgr").into(), 2, 2, 6, PixelFormat::Bgr8, &limits)
        .expect("fixture image");
    let frame = Frame::new(image.clone(), 7, 42).expect("frame");
    let cloned = image.clone();
    drop(image);

    assert_eq!(cloned.pixels(), fixture("bgr"));
    assert_eq!(frame.sequence(), 7);
    assert_eq!(frame.timestamp_ns(), 42);
    let crop = frame
        .crop(
            &native.pool,
            &native.cancellation,
            Roi::new(1, 0, 1, 2).expect("ROI"),
            &limits,
        )
        .expect("crop");
    assert_eq!(crop.stride(), 3);
    assert_eq!(crop.pixels(), &[0, 255, 0, 255, 255, 255]);
    assert_eq!(
        Roi::new(0, 0, 0, 1).expect_err("empty ROI").kind(),
        ImageErrorKind::InvalidArgument
    );
    assert_eq!(
        frame
            .crop(
                &native.pool,
                &native.cancellation,
                Roi::new(2, 0, 1, 1).expect("bounded coordinates"),
                &limits,
            )
            .expect_err("out-of-bounds ROI")
            .kind(),
        VisionErrorKind::Limit
    );
}

#[test]
fn actual_opencv_codec_and_all_format_conversions_are_lossless_where_required() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let limits = limits();
    let expected = fixture("bgr");
    let bmp = native
        .pool
        .decode(&fixture("bmp"), &limits, &native.cancellation)
        .expect("BMP decode");
    let png = native
        .pool
        .decode(&fixture("png"), &limits, &native.cancellation)
        .expect("PNG decode");

    assert_eq!(bmp.pixels(), expected);
    assert_eq!(png.pixels(), expected);
    assert_eq!(bmp.format(), PixelFormat::Bgr8);

    let encoded = native
        .pool
        .encode_png(&bmp, &limits, &native.cancellation)
        .expect("PNG encode");
    let round_trip = native
        .pool
        .decode(&encoded, &limits, &native.cancellation)
        .expect("PNG round trip");
    assert_eq!(round_trip.pixels(), expected);

    let bgra = native
        .pool
        .convert(&bmp, PixelFormat::Bgra8, &limits, &native.cancellation)
        .expect("BGRA");
    assert!(bgra.pixels().chunks_exact(4).all(|pixel| pixel[3] == 255));
    let bgra_encoded = native
        .pool
        .encode_png(&bgra, &limits, &native.cancellation)
        .expect("BGRA PNG encode");
    let bgra_round_trip = native
        .pool
        .decode(&bgra_encoded, &limits, &native.cancellation)
        .expect("BGRA PNG decode");
    assert_eq!(bgra_round_trip, bgra);
    assert_eq!(
        native
            .pool
            .crop(
                &bgra,
                Roi::new(1, 0, 1, 2).expect("BGRA ROI"),
                &limits,
                &native.cancellation,
            )
            .expect("BGRA crop")
            .pixels(),
        &[0, 255, 0, 255, 255, 255, 255, 255]
    );
    assert_eq!(
        native
            .pool
            .convert(&bgra, PixelFormat::Bgr8, &limits, &native.cancellation)
            .expect("BGR")
            .pixels(),
        expected
    );
    let gray = native
        .pool
        .convert(&bmp, PixelFormat::Gray8, &limits, &native.cancellation)
        .expect("Gray");
    assert_eq!(gray.pixels(), &[76, 150, 29, 255]);
    let gray_encoded = native
        .pool
        .encode_png(&gray, &limits, &native.cancellation)
        .expect("Gray PNG encode");
    let gray_round_trip = native
        .pool
        .decode(&gray_encoded, &limits, &native.cancellation)
        .expect("Gray PNG decode");
    assert_eq!(gray_round_trip, gray);
    assert_eq!(
        native
            .pool
            .crop(
                &gray,
                Roi::new(1, 0, 1, 2).expect("Gray ROI"),
                &limits,
                &native.cancellation,
            )
            .expect("Gray crop")
            .pixels(),
        &[150, 255]
    );
    assert_eq!(
        native
            .pool
            .convert(&gray, PixelFormat::Bgr8, &limits, &native.cancellation)
            .expect("Gray BGR")
            .pixels(),
        &[76, 76, 76, 150, 150, 150, 29, 29, 29, 255, 255, 255]
    );
    assert!(
        native
            .pool
            .convert(&gray, PixelFormat::Bgra8, &limits, &native.cancellation)
            .expect("Gray BGRA")
            .pixels()
            .chunks_exact(4)
            .all(|pixel| pixel[3] == 255)
    );
}

#[test]
fn invalid_truncated_and_oversized_encoded_inputs_are_bounded_and_leak_free() {
    let _native_gate = exclusive_native_counter_gate();
    let native = TestNative::new();
    let baseline = native_resource_counts().expect("baseline");
    let limits = limits();
    assert_eq!(
        native
            .pool
            .decode(&[], &limits, &native.cancellation)
            .expect_err("empty")
            .kind(),
        VisionErrorKind::Validation
    );

    let png = fixture("png");
    assert_eq!(
        native
            .pool
            .decode(&png[..20], &limits, &native.cancellation)
            .expect_err("truncated")
            .kind(),
        VisionErrorKind::InvalidImage
    );
    let mut oversized = png;
    oversized[16..20].copy_from_slice(&65_536_u32.to_be_bytes());
    assert_eq!(
        native
            .pool
            .decode(&oversized, &limits, &native.cancellation)
            .expect_err("oversized")
            .kind(),
        VisionErrorKind::Limit
    );
    assert_eq!(native_resource_counts().expect("final"), baseline);
}

#[test]
fn header_preflight_uses_the_encoded_color_channels() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    let exact = VisionLimits::try_for_images(4096, 64, 64, 4096, 12, 6).expect("exact BGR limits");
    assert_eq!(
        native
            .pool
            .decode(&fixture("bmp"), &exact, &native.cancellation)
            .expect("24-bit BMP fits twelve bytes")
            .pixels(),
        fixture("bgr")
    );
    assert_eq!(
        native
            .pool
            .decode(&fixture("png"), &exact, &native.cancellation)
            .expect("RGB PNG fits twelve bytes")
            .pixels(),
        fixture("bgr")
    );
}

#[test]
fn hard_ceilings_roi_overflow_and_png_output_bound_are_explicit() {
    let _native_gate = shared_native_gate();
    let native = TestNative::new();
    assert_eq!(
        VisionLimits::try_for_images(usize::MAX, 1, 1, 1, 1, 1)
            .expect_err("hard encoded ceiling")
            .kind(),
        ImageErrorKind::OutOfRange
    );
    assert_eq!(
        Roi::new(u32::MAX, 0, 1, 1)
            .expect_err("ROI overflow")
            .kind(),
        ImageErrorKind::Overflow
    );

    let output_limited =
        VisionLimits::try_for_images(64, 64, 64, 4096, 16_384, 1024).expect("output limit");
    let image = Image::new(
        fixture("bgr").into(),
        2,
        2,
        6,
        PixelFormat::Bgr8,
        &output_limited,
    )
    .expect("raw image");
    assert_eq!(
        native
            .pool
            .encode_png(&image, &output_limited, &native.cancellation)
            .expect_err("conservative output bound")
            .kind(),
        VisionErrorKind::Limit
    );
    assert_eq!(
        Frame::new(image, 0, 0).expect_err("zero sequence").kind(),
        ImageErrorKind::InvalidArgument
    );
}
