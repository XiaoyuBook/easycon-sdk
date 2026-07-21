use std::path::Path;
use std::ptr::NonNull;

use crate::codec::ImageView;
use crate::ocr::{EngineMode, OcrOutput, OcrProcessError, PageSegmentation};
use crate::{NativeError, ffi};

pub(crate) fn create(
    model_root: &Path,
    language: &str,
    mode: EngineMode,
) -> Result<NonNull<ffi::OcrEngine>, NativeError> {
    let model_root = model_root
        .to_str()
        .ok_or_else(|| NativeError::invalid_argument("OCR model root is not UTF-8"))?;
    let model_root_length = u64::try_from(model_root.len())
        .map_err(|_| NativeError::overflow("OCR model root length does not fit u64"))?;
    let language_length = u64::try_from(language.len())
        .map_err(|_| NativeError::overflow("OCR language length does not fit u64"))?;
    let mut engine = std::ptr::null_mut();
    let mut error = ffi::Error::default();
    // SAFETY: both UTF-8 slices remain live for the synchronous call and both out pointers are
    // unique initialized storage. Native zeroes the handle before any failure can return.
    let status = unsafe {
        ffi::easycon_native_ocr_engine_create(
            model_root.as_ptr(),
            model_root_length,
            language.as_ptr(),
            language_length,
            mode.into_raw(),
            &mut engine,
            &mut error,
        )
    };
    super::finish(status, error)?;
    NonNull::new(engine).ok_or_else(|| NativeError::internal("native OCR create returned null"))
}

pub(crate) fn process(
    engine: NonNull<ffi::OcrEngine>,
    image: ImageView<'_>,
    segmentation: PageSegmentation,
    max_output_bytes: usize,
) -> Result<OcrOutput, OcrProcessError> {
    let raw_image = super::image::raw_view(image).map_err(OcrProcessError::input)?;
    let max_output_bytes = u64::try_from(max_output_bytes)
        .map_err(|_| OcrProcessError::input(NativeError::overflow("OCR output limit")))?;
    let mut output = TextOwner::default();
    let mut confidence = 0.0;
    let mut error = ffi::Error::default();
    // SAFETY: engine is exclusively borrowed by OcrEngine::process, raw_image borrows a live
    // immutable slice, and every out pointer names unique initialized storage for this call.
    let status = unsafe {
        ffi::easycon_native_ocr_engine_process(
            engine.as_ptr(),
            &raw_image,
            segmentation.into_raw(),
            max_output_bytes,
            &mut output.raw,
            &mut confidence,
            &mut error,
        )
    };
    if let Err(error) = super::finish(status, error) {
        return Err(OcrProcessError::native(error));
    }
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(OcrProcessError::invariant(NativeError::internal(
            "native OCR confidence is invalid",
        )));
    }
    let text = output.copy_utf8().map_err(OcrProcessError::invariant)?;
    Ok(OcrOutput { text, confidence })
}

pub(crate) fn destroy(engine: NonNull<ffi::OcrEngine>) {
    let mut raw = engine.as_ptr();
    let mut error = ffi::Error::default();
    // SAFETY: raw is the unique Rust-owned native engine and the bridge consumes it exactly once.
    let status = unsafe { ffi::easycon_native_ocr_engine_destroy(&mut raw, &mut error) };
    let result = super::finish(status, error);
    assert!(
        raw.is_null(),
        "OCR engine destroy did not consume its owner"
    );
    result.expect("OCR engine destroy is infallible for a Rust-owned handle");
}

#[derive(Default)]
struct TextOwner {
    raw: ffi::Buffer,
}

impl TextOwner {
    fn copy_utf8(&self) -> Result<String, NativeError> {
        let length = usize::try_from(self.raw.length)
            .map_err(|_| NativeError::internal("native OCR text length does not fit usize"))?;
        if length == 0 {
            if self.raw.data.is_null() {
                return Ok(String::new());
            }
            return Err(NativeError::internal(
                "native OCR empty text retained an allocation",
            ));
        }
        if self.raw.data.is_null() {
            return Err(NativeError::internal(
                "native OCR text has a null allocation",
            ));
        }
        // SAFETY: a successful bridge call owns exactly length readable bytes until this guard
        // drops; the bytes are copied before the bridge allocation is released.
        let bytes = unsafe { std::slice::from_raw_parts(self.raw.data, length) };
        String::from_utf8(bytes.to_vec())
            .map_err(|_| NativeError::internal("native OCR text is not UTF-8"))
    }
}

impl Drop for TextOwner {
    fn drop(&mut self) {
        // SAFETY: this guard is the unique owner of the bridge text allocation.
        unsafe { ffi::easycon_native_buffer_release(&mut self.raw) };
    }
}
