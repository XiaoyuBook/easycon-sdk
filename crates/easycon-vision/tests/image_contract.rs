use std::sync::Arc;

use easycon_vision::{
    Frame, Image, ImageErrorKind, PixelFormat, Roi, VisionLimits, native_resource_counts,
};

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
    let padded_round_trip = Image::decode(
        &image.encode_png(&limits).expect("padded PNG encode"),
        &limits,
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
        .crop(Roi::new(1, 0, 1, 2).expect("ROI"), &limits)
        .expect("crop");
    assert_eq!(crop.stride(), 3);
    assert_eq!(crop.pixels(), &[0, 255, 0, 255, 255, 255]);
    assert_eq!(
        Roi::new(0, 0, 0, 1).expect_err("empty ROI").kind(),
        ImageErrorKind::InvalidArgument
    );
    assert_eq!(
        frame
            .crop(Roi::new(2, 0, 1, 1).expect("bounded coordinates"), &limits)
            .expect_err("out-of-bounds ROI")
            .kind(),
        ImageErrorKind::OutOfRange
    );
}

#[test]
fn actual_opencv_codec_and_all_format_conversions_are_lossless_where_required() {
    let limits = limits();
    let expected = fixture("bgr");
    let bmp = Image::decode(&fixture("bmp"), &limits).expect("BMP decode");
    let png = Image::decode(&fixture("png"), &limits).expect("PNG decode");

    assert_eq!(bmp.pixels(), expected);
    assert_eq!(png.pixels(), expected);
    assert_eq!(bmp.format(), PixelFormat::Bgr8);

    let encoded = bmp.encode_png(&limits).expect("PNG encode");
    let round_trip = Image::decode(&encoded, &limits).expect("PNG round trip");
    assert_eq!(round_trip.pixels(), expected);

    let bgra = bmp.convert(PixelFormat::Bgra8, &limits).expect("BGRA");
    assert!(bgra.pixels().chunks_exact(4).all(|pixel| pixel[3] == 255));
    let bgra_round_trip =
        Image::decode(&bgra.encode_png(&limits).expect("BGRA PNG encode"), &limits)
            .expect("BGRA PNG decode");
    assert_eq!(bgra_round_trip, bgra);
    assert_eq!(
        bgra.crop(Roi::new(1, 0, 1, 2).expect("BGRA ROI"), &limits)
            .expect("BGRA crop")
            .pixels(),
        &[0, 255, 0, 255, 255, 255, 255, 255]
    );
    assert_eq!(
        bgra.convert(PixelFormat::Bgr8, &limits)
            .expect("BGR")
            .pixels(),
        expected
    );
    let gray = bmp.convert(PixelFormat::Gray8, &limits).expect("Gray");
    assert_eq!(gray.pixels(), &[76, 150, 29, 255]);
    let gray_round_trip =
        Image::decode(&gray.encode_png(&limits).expect("Gray PNG encode"), &limits)
            .expect("Gray PNG decode");
    assert_eq!(gray_round_trip, gray);
    assert_eq!(
        gray.crop(Roi::new(1, 0, 1, 2).expect("Gray ROI"), &limits)
            .expect("Gray crop")
            .pixels(),
        &[150, 255]
    );
    assert_eq!(
        gray.convert(PixelFormat::Bgr8, &limits)
            .expect("Gray BGR")
            .pixels(),
        &[76, 76, 76, 150, 150, 150, 29, 29, 29, 255, 255, 255]
    );
    assert!(
        gray.convert(PixelFormat::Bgra8, &limits)
            .expect("Gray BGRA")
            .pixels()
            .chunks_exact(4)
            .all(|pixel| pixel[3] == 255)
    );
}

#[test]
fn invalid_truncated_and_oversized_encoded_inputs_are_bounded_and_leak_free() {
    let baseline = native_resource_counts().expect("baseline");
    let limits = limits();
    assert_eq!(
        Image::decode(&[], &limits).expect_err("empty").kind(),
        ImageErrorKind::InvalidArgument
    );

    let png = fixture("png");
    assert_eq!(
        Image::decode(&png[..20], &limits)
            .expect_err("truncated")
            .kind(),
        ImageErrorKind::InvalidImage
    );
    let mut oversized = png;
    oversized[16..20].copy_from_slice(&65_536_u32.to_be_bytes());
    assert_eq!(
        Image::decode(&oversized, &limits)
            .expect_err("oversized")
            .kind(),
        ImageErrorKind::OutOfRange
    );
    assert_eq!(native_resource_counts().expect("final"), baseline);
}

#[test]
fn header_preflight_uses_the_encoded_color_channels() {
    let exact = VisionLimits::try_for_images(4096, 64, 64, 4096, 12, 6).expect("exact BGR limits");
    assert_eq!(
        Image::decode(&fixture("bmp"), &exact)
            .expect("24-bit BMP fits twelve bytes")
            .pixels(),
        fixture("bgr")
    );
    assert_eq!(
        Image::decode(&fixture("png"), &exact)
            .expect("RGB PNG fits twelve bytes")
            .pixels(),
        fixture("bgr")
    );
}

#[test]
fn hard_ceilings_roi_overflow_and_png_output_bound_are_explicit() {
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
        image
            .encode_png(&output_limited)
            .expect_err("conservative output bound")
            .kind(),
        ImageErrorKind::OutOfRange
    );
    assert_eq!(
        Frame::new(image, 0, 0).expect_err("zero sequence").kind(),
        ImageErrorKind::InvalidArgument
    );
}
