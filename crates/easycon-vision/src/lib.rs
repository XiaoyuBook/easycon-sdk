#![forbid(unsafe_code)]
//! Rust-owned Vision types and lifecycle coordination.

mod color;
mod image;
mod matching;

pub use color::{ColorStatistics, HsvRange};
pub use image::{Frame, Image, ImageError, ImageErrorKind, PixelFormat, Roi, VisionLimits};
pub use matching::{
    EdgeMethod, MatchResult, TemplateMethod, match_edge, match_template, preprocess_edge,
};

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
