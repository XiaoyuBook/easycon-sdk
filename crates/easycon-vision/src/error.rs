use std::fmt;

use easycon_model::{EasyConError, ErrorCode};
use easycon_native_sys::{NativeError, NativeErrorKind};

use crate::{ImageError, ImageErrorKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum VisionErrorKind {
    Validation,
    Limit,
    NoFrame,
    Closed,
    Faulted,
    Cancelled,
    Deadline,
    InvalidImage,
    ModelNotFound,
    Native,
    PoolClosed,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisionError {
    kind: VisionErrorKind,
    message: String,
    native: Option<NativeError>,
}

impl VisionError {
    pub(crate) fn new(kind: VisionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            native: None,
        }
    }

    pub(crate) fn validation(message: impl Into<String>) -> Self {
        Self::new(VisionErrorKind::Validation, message)
    }

    pub(crate) fn limit(message: impl Into<String>) -> Self {
        Self::new(VisionErrorKind::Limit, message)
    }

    pub(crate) fn cancelled(message: impl Into<String>) -> Self {
        Self::new(VisionErrorKind::Cancelled, message)
    }

    pub(crate) fn pool_closed(message: impl Into<String>) -> Self {
        Self::new(VisionErrorKind::PoolClosed, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(VisionErrorKind::Internal, message)
    }

    pub(crate) fn from_native(error: NativeError) -> Self {
        let kind = match error.kind() {
            NativeErrorKind::InvalidArgument => VisionErrorKind::Validation,
            NativeErrorKind::OutOfRange
            | NativeErrorKind::Overflow
            | NativeErrorKind::ResourceExhausted
            | NativeErrorKind::AllocationFailed => VisionErrorKind::Limit,
            NativeErrorKind::ModelNotFound => VisionErrorKind::ModelNotFound,
            NativeErrorKind::InvalidImage => VisionErrorKind::InvalidImage,
            NativeErrorKind::NoFrame => VisionErrorKind::NoFrame,
            NativeErrorKind::Cancelled => VisionErrorKind::Cancelled,
            NativeErrorKind::Internal => VisionErrorKind::Internal,
            _ => VisionErrorKind::Native,
        };
        Self {
            kind,
            message: error.message().to_owned(),
            native: Some(error),
        }
    }

    pub(crate) fn from_runtime(error: EasyConError) -> Self {
        let kind = match error.code() {
            ErrorCode::RuntimeClosing => VisionErrorKind::Closed,
            ErrorCode::DeadlineExceeded => VisionErrorKind::Deadline,
            ErrorCode::Cancelled => VisionErrorKind::Cancelled,
            ErrorCode::InvalidArgument => VisionErrorKind::Validation,
            _ => VisionErrorKind::Internal,
        };
        Self::new(kind, error.to_string())
    }

    #[must_use]
    pub const fn kind(&self) -> VisionErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn native(&self) -> Option<&NativeError> {
        self.native.as_ref()
    }
}

impl From<ImageError> for VisionError {
    fn from(error: ImageError) -> Self {
        let kind = match error.kind() {
            ImageErrorKind::InvalidArgument => VisionErrorKind::Validation,
            ImageErrorKind::OutOfRange
            | ImageErrorKind::Overflow
            | ImageErrorKind::ResourceExhausted => VisionErrorKind::Limit,
            ImageErrorKind::InvalidImage => VisionErrorKind::InvalidImage,
            ImageErrorKind::Native => VisionErrorKind::Native,
            ImageErrorKind::Internal => VisionErrorKind::Internal,
        };
        let message = error.message().to_owned();
        Self {
            kind,
            message,
            native: error.into_native(),
        }
    }
}

impl fmt::Display for VisionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for VisionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.native
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}
