#![forbid(unsafe_code)]
//! Rust-owned Vision types and lifecycle coordination.

mod capture;
mod color;
mod error;
mod image;
mod label;
mod matching;
mod ocr;
mod pool;

pub use capture::{
    CaptureBackendKind, CaptureOptions, CaptureProfile, CaptureSession, CaptureSnapshotWait,
    CaptureSourceDescriptor, CaptureState, FileCapture, NativeCaptureOptions, SyntheticCapture,
    SyntheticCaptureControl, SyntheticCaptureCounts, discover_capture_sources,
};
pub use color::{ColorStatistics, HsvRange};
pub use error::{VisionError, VisionErrorKind};
pub use image::{Frame, Image, ImageError, ImageErrorKind, PixelFormat, Roi, VisionLimits};
pub use label::{
    Label, LabelDiagnostic, LabelDiagnosticCode, LabelDiagnosticSeverity, LabelEvaluation,
    LabelEvaluator, LabelMethod, LabelParseReport, LabelRegistry, LabelRegistryReport, LabelSource,
    LabelTarget, MAX_LABEL_DIAGNOSTICS_PER_SOURCE, MAX_LABEL_JSON_BYTES, MAX_LABEL_SOURCES,
    build_legacy_label_registry, parse_legacy_il,
};
pub use matching::{
    EdgeMethod, MatchResult, TemplateMethod, match_edge, match_template, preprocess_edge,
};
pub use ocr::{OcrConfig, OcrEngineMode, OcrOutput, OcrPageSegmentation, OcrPool, OcrPoolCounts};
pub use pool::{NativePool, NativePoolCounts, NativePoolOptions};

pub use easycon_native_sys::NativeError as NativeBridgeError;

/// Counts exposed for internal lifecycle evidence without exposing bridge details.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeResourceCounts {
    pub live_handles: u64,
    pub live_allocations: u64,
}

/// Reads the private bridge counters through the safe native-sys boundary.
pub fn native_resource_counts() -> Result<NativeResourceCounts, NativeBridgeError> {
    easycon_native_sys::debug::counts().map(|counts| NativeResourceCounts {
        live_handles: counts.live_handles,
        live_allocations: counts.live_allocations,
    })
}
