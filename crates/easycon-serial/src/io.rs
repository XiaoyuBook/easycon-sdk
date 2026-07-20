use std::fmt;
use std::sync::Arc;

use easycon_controller::WriteContext;
use easycon_runtime::{CancellationToken, Clock};

use crate::SerialPortDescriptor;

/// Stable serial-system failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SerialErrorKind {
    /// The caller is not allowed to open the port.
    AccessDenied,
    /// Another owner has the port open exclusively.
    PortBusy,
    /// The selected system port no longer exists.
    NotFound,
    /// An open device disappeared or stopped responding as a connected device.
    Disconnected,
    /// Operation or resource cancellation interrupted I/O.
    Cancelled,
    /// The absolute I/O deadline elapsed.
    DeadlineExceeded,
    /// The backend returned success without accepting or producing any byte.
    ZeroProgress,
    /// A descriptor or port path is invalid.
    InvalidPort,
    /// A system I/O operation failed.
    Io,
    /// Bytes violated the serial protocol boundary.
    Protocol,
    /// No production implementation exists for the current target.
    UnsupportedPlatform,
}

/// Owned serial error with an optional native Windows status code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialError {
    kind: SerialErrorKind,
    message: Arc<str>,
    os_code: Option<u32>,
}

impl SerialError {
    /// Creates an error without a platform code.
    #[must_use]
    pub fn new(kind: SerialErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            os_code: None,
        }
    }

    /// Creates an error carrying the exact Windows status code.
    #[must_use]
    pub fn with_os_code(kind: SerialErrorKind, message: impl Into<Arc<str>>, os_code: u32) -> Self {
        Self {
            kind,
            message: message.into(),
            os_code: Some(os_code),
        }
    }

    pub(crate) fn invalid_port(message: &'static str) -> Self {
        Self::new(SerialErrorKind::InvalidPort, message)
    }

    /// Returns the stable category.
    #[must_use]
    pub const fn kind(&self) -> SerialErrorKind {
        self.kind
    }

    /// Returns diagnostic text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the native status when the system supplied one.
    #[must_use]
    pub const fn os_code(&self) -> Option<u32> {
        self.os_code
    }
}

impl fmt::Display for SerialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)?;
        if let Some(code) = self.os_code {
            write!(formatter, " (os_code={code})")?;
        }
        Ok(())
    }
}

impl std::error::Error for SerialError {}

/// Shared absolute-deadline and cancellation context for one byte-I/O call.
#[derive(Clone)]
pub struct ByteIoRequest {
    /// Logical operation at the serial/Controller boundary.
    pub operation: ByteIoOperation,
    /// Runtime monotonic clock used to evaluate the absolute deadline.
    pub clock: Arc<dyn Clock>,
    /// Absolute deadline on `clock`.
    pub deadline_ns: u64,
    /// Cancellation owned by the current Controller operation.
    pub cancellation: CancellationToken,
    /// Cancellation owned by the Controller resource.
    pub resource_cancellation: CancellationToken,
}

/// Logical purpose carried across injectable byte-I/O calls without interpreting device state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ByteIoOperation {
    /// Opening a stream for one handshake attempt.
    Open,
    /// Writing source-exact handshake bytes.
    HandshakeWrite,
    /// Reading the handshake reply.
    HandshakeRead,
    /// One partial call belonging to a Controller logical write.
    ControllerWrite(WriteContext),
    /// Reading one ACK for the current matcher generation.
    AckRead { generation: u64 },
    /// Purging bytes before a command generation starts.
    DiscardInput { write_sequence: u64 },
}

impl ByteIoRequest {
    /// Returns cancellation or deadline failure before entering a backend call.
    #[must_use]
    pub fn interruption(&self) -> Option<SerialError> {
        if self.cancellation.is_cancelled() || self.resource_cancellation.is_cancelled() {
            Some(SerialError::new(
                SerialErrorKind::Cancelled,
                "serial byte I/O was cancelled",
            ))
        } else if self.clock.now_ns() >= self.deadline_ns {
            Some(SerialError::new(
                SerialErrorKind::DeadlineExceeded,
                "serial byte I/O deadline elapsed",
            ))
        } else {
            None
        }
    }
}

/// Open byte stream owned by exactly one Controller transport lane.
pub trait ByteIo: Send + 'static {
    /// Reads a non-empty prefix or returns a stable failure.
    fn read(&mut self, buffer: &mut [u8], request: ByteIoRequest) -> Result<usize, SerialError>;

    /// Writes a non-empty prefix or returns a stable failure.
    fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError>;

    /// Discards bytes received before a new request/response generation starts.
    fn discard_input(&mut self, request: ByteIoRequest) -> Result<(), SerialError>;

    /// Interrupts pending calls and releases the stream. Repeated calls are harmless.
    fn close(&mut self);
}

/// Injectable open boundary used by production Win32 and byte-level test devices.
pub trait ByteIoFactory: Send + 'static {
    /// Opens one descriptor at one explicit baud rate.
    fn open(
        &mut self,
        port: &SerialPortDescriptor,
        baud_rate: u32,
        request: ByteIoRequest,
    ) -> Result<Box<dyn ByteIo>, SerialError>;
}
