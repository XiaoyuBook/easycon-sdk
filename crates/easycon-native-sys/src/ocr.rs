//! Exclusive RAII ownership for private Tesseract engine calls.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::path::Path;
use std::ptr::NonNull;

use crate::codec::ImageView;
use crate::{NativeError, NativeErrorKind, call, ffi};

pub const MAX_OCR_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum EngineMode {
    Default,
    LstmOnly,
}

impl EngineMode {
    pub(crate) const fn into_raw(self) -> u32 {
        match self {
            Self::Default => ffi::OCR_ENGINE_DEFAULT,
            Self::LstmOnly => ffi::OCR_ENGINE_LSTM_ONLY,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum PageSegmentation {
    Auto,
    SingleBlock,
    SingleLine,
    SingleWord,
}

impl PageSegmentation {
    pub(crate) const fn into_raw(self) -> u32 {
        match self {
            Self::Auto => ffi::OCR_PSM_AUTO,
            Self::SingleBlock => ffi::OCR_PSM_SINGLE_BLOCK,
            Self::SingleLine => ffi::OCR_PSM_SINGLE_LINE,
            Self::SingleWord => ffi::OCR_PSM_SINGLE_WORD,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OcrOutput {
    pub text: String,
    pub confidence: f64,
}

#[derive(Debug)]
pub struct OcrProcessError {
    error: NativeError,
    poisons_engine: bool,
}

impl OcrProcessError {
    pub(crate) fn input(error: NativeError) -> Self {
        Self {
            error,
            poisons_engine: false,
        }
    }

    pub(crate) fn invariant(error: NativeError) -> Self {
        Self {
            error,
            poisons_engine: true,
        }
    }

    pub(crate) fn native(error: NativeError) -> Self {
        let poisons_engine = matches!(
            error.kind(),
            NativeErrorKind::InvalidArgument
                | NativeErrorKind::Backend
                | NativeErrorKind::ComputerVisionException
                | NativeErrorKind::StandardException
                | NativeErrorKind::UnknownException
                | NativeErrorKind::Internal
        );
        Self {
            error,
            poisons_engine,
        }
    }

    #[must_use]
    pub const fn error(&self) -> &NativeError {
        &self.error
    }

    #[must_use]
    pub const fn poisons_engine(&self) -> bool {
        self.poisons_engine
    }

    pub fn into_error(self) -> NativeError {
        self.error
    }
}

impl fmt::Display for OcrProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for OcrProcessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Unique owner for one Tesseract engine.
///
/// The handle can move to another worker, but every operation requires `&mut self` and the
/// `Cell` marker prevents shared cross-thread access.
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<easycon_native_sys::ocr::OcrEngine>();
/// ```
#[derive(Debug)]
pub struct OcrEngine {
    raw: NonNull<ffi::OcrEngine>,
    not_sync: PhantomData<Cell<()>>,
}

// SAFETY: the bridge engine has no C++ thread and is only callable through exclusive `&mut self`;
// moving its unique owner between Rust workers cannot create concurrent native access.
#[allow(unsafe_code)]
unsafe impl Send for OcrEngine {}

impl OcrEngine {
    pub fn create(
        model_root: &Path,
        language: &str,
        mode: EngineMode,
    ) -> Result<Self, NativeError> {
        if !model_root.is_absolute() {
            return Err(NativeError::invalid_argument(
                "OCR model root must be absolute",
            ));
        }
        if language.is_empty() || language.len() > 128 {
            return Err(NativeError::invalid_argument(
                "OCR language length is invalid",
            ));
        }
        call::ocr::create(model_root, language, mode).map(|raw| Self {
            raw,
            not_sync: PhantomData,
        })
    }

    pub fn process(
        &mut self,
        image: ImageView<'_>,
        segmentation: PageSegmentation,
        max_output_bytes: usize,
    ) -> Result<OcrOutput, OcrProcessError> {
        if max_output_bytes == 0 || max_output_bytes > MAX_OCR_OUTPUT_BYTES {
            return Err(OcrProcessError::input(NativeError::out_of_range(
                "OCR output limit exceeds the hard ceiling",
            )));
        }
        call::ocr::process(self.raw, image, segmentation, max_output_bytes)
    }
}

impl Drop for OcrEngine {
    fn drop(&mut self) {
        call::ocr::destroy(self.raw);
    }
}

#[cfg(test)]
mod tests {
    use super::OcrEngine;

    fn assert_send<T: Send>() {}

    #[test]
    fn engine_is_send() {
        assert_send::<OcrEngine>();
    }
}
