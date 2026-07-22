use std::path::{Path, PathBuf};

use easycon_native_sys::NativeErrorKind;
use easycon_native_sys::codec::{ImageLimits, ImageView, PixelFormat};
use easycon_native_sys::ocr::{EngineMode, OcrEngine, PageSegmentation};

fn test_model_root() -> PathBuf {
    std::env::var_os("EASYCON_VISION_TEST_TESSDATA")
        .map(PathBuf::from)
        .expect("EASYCON_VISION_TEST_TESSDATA must name the provisioned test model")
}

fn fixture_pixels() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spec/fixtures/vision/ocr/easycon-gray.hex");
    let text = std::fs::read_to_string(path).expect("OCR fixture");
    text.trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let value = std::str::from_utf8(pair).expect("ASCII fixture");
            u8::from_str_radix(value, 16).expect("hex fixture")
        })
        .collect()
}

fn limits() -> ImageLimits {
    ImageLimits::new(64 * 1024, 1024, 1024, 1024 * 1024, 4 * 1024 * 1024, 4096)
        .expect("OCR test limits")
}

#[test]
fn explicit_model_processes_twice_and_releases_the_engine() {
    let baseline = easycon_native_sys::debug::counts().expect("baseline counts");
    let mut engine =
        OcrEngine::create(&test_model_root(), "eng", EngineMode::Default).expect("test OCR engine");
    let live = easycon_native_sys::debug::counts().expect("live counts");
    assert_eq!(live.live_handles, baseline.live_handles + 1);

    let pixels = fixture_pixels();
    let view = ImageView::new(&pixels, 273, 77, 273, PixelFormat::Gray8, limits())
        .expect("OCR image view");
    for _ in 0..2 {
        let output = engine
            .process(view, PageSegmentation::SingleLine, 4096)
            .expect("OCR output");
        assert!(
            output
                .text
                .split_whitespace()
                .collect::<String>()
                .contains("EASCON")
        );
        assert!((0.0..=1.0).contains(&output.confidence));
    }
    drop(engine);
    assert_eq!(
        easycon_native_sys::debug::counts().expect("final counts"),
        baseline
    );
}

#[test]
fn missing_model_has_a_stable_error_and_no_handle() {
    let baseline = easycon_native_sys::debug::counts().expect("baseline counts");
    let missing = std::env::temp_dir().join("easycon-sdk-missing-tessdata");
    let error =
        OcrEngine::create(&missing, "eng", EngineMode::Default).expect_err("missing OCR model");
    assert_eq!(error.kind(), NativeErrorKind::ModelNotFound);
    assert_eq!(
        easycon_native_sys::debug::counts().expect("final counts"),
        baseline
    );
}
