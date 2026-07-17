use std::fmt;
use std::sync::Arc;

/// Stable subsystem associated with an SDK error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorDomain {
    /// Runtime admission, scheduling, or lifecycle.
    Runtime,
    /// Controller protocol or state machine.
    Controller,
    /// Transport I/O.
    Io,
    /// Invalid caller input.
    Validation,
    /// An internal invariant failed.
    Internal,
}

/// Stable machine-readable error code for the first vertical slice.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// A new operation or resource was submitted after closing started.
    RuntimeClosing,
    /// A wait ended without changing the observed operation.
    WaitTimeout,
    /// An operation deadline requested cancellation.
    DeadlineExceeded,
    /// The caller or parent runtime requested cancellation.
    Cancelled,
    /// A protocol exchange exceeded its own timeout.
    ProtocolTimeout,
    /// The controller transport disconnected.
    DeviceDisconnected,
    /// A transport read or write failed.
    Transport,
    /// An exclusive resource lease is already owned.
    ResourceBusy,
    /// Input failed validation before admission.
    InvalidArgument,
    /// A core invariant was violated.
    Internal,
}

/// Immutable error stored by a failed operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EasyConError {
    domain: ErrorDomain,
    code: ErrorCode,
    message: Arc<str>,
}

impl EasyConError {
    /// Creates an immutable error with a stable domain and code.
    pub fn new(domain: ErrorDomain, code: ErrorCode, message: impl Into<Arc<str>>) -> Self {
        Self {
            domain,
            code,
            message: message.into(),
        }
    }

    /// Returns the stable subsystem.
    #[must_use]
    pub const fn domain(&self) -> ErrorDomain {
        self.domain
    }

    /// Returns the stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns the diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for EasyConError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?}/{:?}: {}",
            self.domain, self.code, self.message
        )
    }
}

impl std::error::Error for EasyConError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_clone_keeps_stable_identity() {
        let error = EasyConError::new(
            ErrorDomain::Controller,
            ErrorCode::ProtocolTimeout,
            "hello timed out",
        );

        assert_eq!(error.clone(), error);
        assert_eq!(error.domain(), ErrorDomain::Controller);
        assert_eq!(error.code(), ErrorCode::ProtocolTimeout);
        assert!(error.to_string().contains("hello timed out"));
    }
}
