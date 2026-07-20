#![forbid(unsafe_code)]
//! Source-exact Controller protocol and a Runtime-supervised single-writer command lane.

mod amiibo;
mod protocol;
mod sequence;
mod session;
mod transport;

pub use protocol::SwitchReport;
pub use sequence::{MAX_SEQUENCE_DURATION_NS, MAX_SEQUENCE_STEPS, PreciseSequence, SequenceStep};
pub use session::{
    AutomationLease, ConnectOptions, ControllerAction, ControllerLeaseState, ControllerOptions,
    ControllerSession, ControllerSnapshot, ControllerState,
};
pub use transport::{
    AUTO_BAUD_RATES, AckFrame, AckRequest, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST,
    HandshakeRequest, TransportError, TransportErrorKind, WriteContext, WriteKind, WriteRequest,
};

/// Behavior schema version implemented by this crate.
pub const BEHAVIOR_SCHEMA_VERSION: u32 = easycon_model::BEHAVIOR_SCHEMA_VERSION;
pub use amiibo::{
    AMIIBO_ACK, AMIIBO_CHUNK_SIZE, AMIIBO_PROTOCOL_MAX_BYTES, AmiiboLimits, AmiiboSaveOptions,
    AmiiboSelectOptions, MAX_AMIIBO_CHUNK_RETRIES,
};
