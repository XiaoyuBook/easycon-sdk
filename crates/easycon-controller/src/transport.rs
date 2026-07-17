use std::fmt;
use std::sync::Arc;

use easycon_model::{OperationId, ResourceId};
use easycon_runtime::CancellationToken;

/// Source-exact automatic connection order.
pub const AUTO_BAUD_RATES: [u32; 2] = [115_200, 9_600];
/// Source-exact hello request.
pub const HANDSHAKE_REQUEST: [u8; 3] = [0xA5, 0xA5, 0x81];
/// Source-exact successful hello reply.
pub const HANDSHAKE_REPLY: u8 = 0x80;

/// One bounded handshake attempt.
pub struct HandshakeRequest {
    /// Related connect operation.
    pub operation_id: OperationId,
    /// Attempted baud rate.
    pub baud_rate: u32,
    /// Source-exact hello request bytes.
    pub request_bytes: [u8; 3],
    /// Source-exact expected reply byte.
    pub expected_reply: u8,
    /// Absolute protocol timeout on the Runtime clock.
    pub deadline_ns: u64,
    /// Cancellation propagated from the operation and Runtime root.
    pub cancellation: CancellationToken,
    /// Resource cancellation used to interrupt close while the lane is blocked.
    pub resource_cancellation: CancellationToken,
}

/// One generation-tagged protocol reply byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AckFrame {
    /// Request generation assigned by the single matcher.
    pub generation: u64,
    /// Reply byte.
    pub byte: u8,
}

/// One bounded generation-aware ACK wait.
pub struct AckRequest {
    /// Related operation.
    pub operation_id: OperationId,
    /// Current request generation.
    pub generation: u64,
    /// Required reply byte.
    pub expected_reply: u8,
    /// Absolute protocol deadline.
    pub deadline_ns: u64,
    /// Operation cancellation.
    pub cancellation: CancellationToken,
    /// Resource close cancellation.
    pub resource_cancellation: CancellationToken,
}

/// Semantic purpose of a transport write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteKind {
    /// Normal desired-state report.
    Report,
    /// Safety report sent during cancellation or close.
    Neutralize,
    /// Request/response command bytes.
    Command,
}

/// Stable context repeated across partial writes for one logical payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteContext {
    /// Controller resource that owns the single writer.
    pub resource_id: ResourceId,
    /// Related operation, absent for final resource close cleanup.
    pub operation_id: Option<OperationId>,
    /// Strictly increasing logical write sequence.
    pub sequence: u64,
    /// Runtime-clock timestamp chosen for this logical dispatch.
    pub timestamp_ns: u64,
    /// Complete logical payload length.
    pub total_len: usize,
    /// Write purpose.
    pub kind: WriteKind,
}

/// One bounded, cancellable partial-write attempt.
pub struct WriteRequest<'a> {
    /// Stable context shared by every partial call for the logical payload.
    pub context: WriteContext,
    /// Remaining bytes; a successful call accepts a non-zero prefix.
    pub bytes: &'a [u8],
    /// Absolute I/O deadline on the Runtime clock.
    pub deadline_ns: u64,
    /// Operation cancellation for normal report or command work.
    pub cancellation: CancellationToken,
    /// Controller resource cancellation used by deterministic close.
    pub resource_cancellation: CancellationToken,
}

/// Stable fake/system transport failure category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    /// One bounded handshake or ACK exchange timed out.
    Timeout,
    /// A bounded transport write failed to make progress before its I/O deadline.
    WriteTimeout,
    /// Cancellation interrupted a blocking exchange.
    Cancelled,
    /// The device or port disconnected.
    Disconnected,
    /// Operating-system I/O failed.
    Io,
    /// Bytes violated the expected protocol.
    Protocol,
}

/// Owned transport error that never exposes an OS or serial package type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportError {
    kind: TransportErrorKind,
    message: Arc<str>,
}

impl TransportError {
    /// Creates an owned transport error.
    #[must_use]
    pub fn new(kind: TransportErrorKind, message: impl Into<Arc<str>>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the stable category.
    #[must_use]
    pub const fn kind(&self) -> TransportErrorKind {
        self.kind
    }

    /// Returns diagnostic text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for TransportError {}

/// Controller-owned transport boundary. System serial implementations remain leaf dependencies.
pub trait ControllerTransport: Send + 'static {
    /// Opens at one baud and completes the source-exact hello exchange by `deadline_ns`.
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError>;

    /// Accepts a non-zero prefix and must wake for cancellation or the supplied deadline.
    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError>;

    /// Waits for one generation-tagged ACK frame or a bounded/cancelled outcome.
    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError>;

    /// Interrupts blocking I/O and releases the transport. Calling repeatedly is harmless.
    fn close(&mut self);
}
