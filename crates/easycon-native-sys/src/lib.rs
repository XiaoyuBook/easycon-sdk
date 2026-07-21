#![deny(unsafe_code)]
//! Safe ownership wrappers for the private C-compatible native bridge.

#[allow(unsafe_code)]
mod call;
pub mod codec;
#[allow(unsafe_code)]
mod ffi;

use std::fmt;

/// Stable Rust-side classification for private native failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NativeErrorKind {
    InvalidArgument,
    OutOfRange,
    Overflow,
    ResourceExhausted,
    NotFound,
    ModelNotFound,
    InvalidImage,
    NoFrame,
    Cancelled,
    Backend,
    ComputerVisionException,
    StandardException,
    UnknownException,
    AllocationFailed,
    Internal,
}

impl NativeErrorKind {
    fn from_status(status: i32) -> Self {
        match status {
            ffi::STATUS_INVALID_ARGUMENT => Self::InvalidArgument,
            ffi::STATUS_OUT_OF_RANGE => Self::OutOfRange,
            ffi::STATUS_OVERFLOW => Self::Overflow,
            ffi::STATUS_RESOURCE_EXHAUSTED => Self::ResourceExhausted,
            ffi::STATUS_NOT_FOUND => Self::NotFound,
            ffi::STATUS_MODEL_NOT_FOUND => Self::ModelNotFound,
            ffi::STATUS_INVALID_IMAGE => Self::InvalidImage,
            ffi::STATUS_NO_FRAME => Self::NoFrame,
            ffi::STATUS_CANCELLED => Self::Cancelled,
            ffi::STATUS_BACKEND_ERROR => Self::Backend,
            ffi::STATUS_CV_EXCEPTION => Self::ComputerVisionException,
            ffi::STATUS_STD_EXCEPTION => Self::StandardException,
            ffi::STATUS_UNKNOWN_EXCEPTION => Self::UnknownException,
            ffi::STATUS_ALLOCATION_FAILED => Self::AllocationFailed,
            ffi::STATUS_INTERNAL => Self::Internal,
            _ => Self::Internal,
        }
    }
}

/// Owned copy of a private native diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeError {
    kind: NativeErrorKind,
    message: String,
}

impl NativeError {
    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            kind: NativeErrorKind::InvalidArgument,
            message: message.into(),
        }
    }

    pub(crate) fn out_of_range(message: impl Into<String>) -> Self {
        Self {
            kind: NativeErrorKind::OutOfRange,
            message: message.into(),
        }
    }

    pub(crate) fn overflow(message: impl Into<String>) -> Self {
        Self {
            kind: NativeErrorKind::Overflow,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: NativeErrorKind::Internal,
            message: message.into(),
        }
    }

    /// Returns the Rust-side error class.
    #[must_use]
    pub const fn kind(&self) -> NativeErrorKind {
        self.kind
    }

    /// Returns the copied UTF-8 diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for NativeError {}

/// Snapshot of bridge-owned resources, intended for diagnostics and isolation tests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NativeResourceCounts {
    pub live_handles: u64,
    pub live_allocations: u64,
}

/// Private diagnostics used by Vision and component tests.
pub mod debug {
    use std::ptr::NonNull;

    use crate::ffi;
    use crate::{NativeError, NativeResourceCounts, call};

    /// Unique RAII owner used to verify native handle lifecycle before real handles are added.
    pub struct DebugHandle {
        raw: NonNull<ffi::DebugHandle>,
    }

    impl DebugHandle {
        /// Creates one private native handle.
        pub fn create() -> Result<Self, NativeError> {
            call::create_debug_handle().map(|raw| Self { raw })
        }
    }

    impl Drop for DebugHandle {
        fn drop(&mut self) {
            call::destroy_debug_handle(self.raw);
        }
    }

    /// Returns bridge resource counters without exposing FFI values or pointers.
    pub fn counts() -> Result<NativeResourceCounts, NativeError> {
        call::counts()
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeErrorKind, ffi};

    #[test]
    fn internal_and_unknown_statuses_are_both_contained() {
        assert_eq!(
            NativeErrorKind::from_status(ffi::STATUS_INTERNAL),
            NativeErrorKind::Internal
        );
        assert_eq!(
            NativeErrorKind::from_status(i32::MAX),
            NativeErrorKind::Internal
        );
    }
}
