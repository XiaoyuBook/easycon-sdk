use std::fmt;
use std::sync::Arc;

use easycon_controller::{WriteContext, WriteSettlement};
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
    backend_proved_zero_effect: bool,
}

impl SerialError {
    /// Creates an error without a platform code.
    #[must_use]
    pub fn new(kind: SerialErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            os_code: None,
            backend_proved_zero_effect: false,
        }
    }

    /// Creates an error after the backend proved that this byte-I/O attempt accepted no bytes.
    #[must_use]
    pub fn zero_effect(kind: SerialErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
            os_code: None,
            backend_proved_zero_effect: true,
        }
    }

    /// Creates an error carrying the exact Windows status code.
    #[must_use]
    pub fn with_os_code(kind: SerialErrorKind, message: impl Into<Arc<str>>, os_code: u32) -> Self {
        Self {
            kind,
            message: message.into(),
            os_code: Some(os_code),
            backend_proved_zero_effect: false,
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

    /// Returns whether the concrete backend proved this failed call accepted no bytes.
    #[must_use]
    pub const fn backend_proved_zero_effect(&self) -> bool {
        self.backend_proved_zero_effect
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
    /// Final-byte gate carried only by one Controller logical write.
    pub final_write_settlement: Option<ByteIoFinalWriteSettlement>,
}

/// Typed final-byte settlement capability carried from the Controller transport to one Byte-I/O
/// backend call.
///
/// The capability is inert for partial completions. A concrete byte backend must consume its own
/// physical completion count and invoke [`ByteIoRequest::publish_final_write_acceptance`] before
/// exposing a complete logical report to its caller, telemetry, or a test observer.
#[derive(Clone)]
pub struct ByteIoFinalWriteSettlement {
    settlement: WriteSettlement,
    accepted_before: usize,
    total_len: usize,
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
    /// Returns a zero-effect cancellation or deadline failure before entering a backend call.
    #[must_use]
    pub fn interruption(&self) -> Option<SerialError> {
        if self.cancellation.is_cancelled() || self.resource_cancellation.is_cancelled() {
            Some(SerialError::zero_effect(
                SerialErrorKind::Cancelled,
                "serial byte I/O was cancelled",
            ))
        } else if self.clock.now_ns() >= self.deadline_ns {
            Some(SerialError::zero_effect(
                SerialErrorKind::DeadlineExceeded,
                "serial byte I/O deadline elapsed",
            ))
        } else {
            None
        }
    }

    /// Attaches the final-byte settlement gate to a Controller write fragment.
    #[must_use]
    pub(crate) fn with_final_write_settlement(
        mut self,
        settlement: WriteSettlement,
        accepted_before: usize,
        total_len: usize,
    ) -> Self {
        self.final_write_settlement = Some(ByteIoFinalWriteSettlement {
            settlement,
            accepted_before,
            total_len,
        });
        self
    }

    /// Reserves and publishes a final logical write at this backend's physical completion point.
    ///
    /// Callers must invoke this after they have consumed the exact count for `written`, while they
    /// still exclusively own that completion, and after releasing backend-local locks. It samples
    /// `Clock` only after reservation, then invokes the Controller hook without any Byte-I/O lock
    /// held. Partial calls are intentionally inert.
    pub fn publish_final_write_acceptance(&self, written: usize) -> Result<(), SerialError> {
        let Some(final_write) = &self.final_write_settlement else {
            return Ok(());
        };
        let accepted = final_write
            .accepted_before
            .checked_add(written)
            .ok_or_else(|| {
                SerialError::new(
                    SerialErrorKind::Protocol,
                    "serial final-byte settlement count overflowed",
                )
            })?;
        if accepted > final_write.total_len {
            return Err(SerialError::new(
                SerialErrorKind::Protocol,
                "serial backend completion exceeded its logical report length",
            ));
        }
        if accepted != final_write.total_len {
            return Ok(());
        }
        if !final_write.settlement.reserve_full_acceptance() {
            return Err(SerialError::new(
                SerialErrorKind::Io,
                "serial backend rejected final-byte settlement reservation",
            ));
        }
        let accepted_at_ns = self.clock.now_ns();
        if !final_write
            .settlement
            .publish_reserved_full_acceptance(accepted_at_ns)
        {
            return Err(SerialError::new(
                SerialErrorKind::Io,
                "serial backend could not publish final-byte settlement",
            ));
        }
        Ok(())
    }
}

/// Open byte stream owned by exactly one Controller transport lane.
pub trait ByteIo: Send + 'static {
    /// Reads a non-empty prefix or returns a stable failure.
    fn read(&mut self, buffer: &mut [u8], request: ByteIoRequest) -> Result<usize, SerialError>;

    /// Writes a non-empty prefix or returns a stable failure.
    ///
    /// Implementations use [`SerialError::zero_effect`] only when they have consumed the
    /// underlying completion and proved the failed call accepted no bytes. All other errors leave
    /// framing uncertain for the Controller transport to settle.
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
