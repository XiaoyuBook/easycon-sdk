#![forbid(unsafe_code)]
//! Source-exact Controller protocol and a Runtime-supervised single-writer command lane.

mod protocol;
mod session;
mod transport;

pub use protocol::SwitchReport;
pub use session::{
    ConnectOptions, ControllerAction, ControllerOptions, ControllerSession, ControllerSnapshot,
    ControllerState,
};
pub use transport::{
    AUTO_BAUD_RATES, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST, HandshakeRequest,
    TransportError, TransportErrorKind, WriteContext, WriteKind,
};

/// Behavior schema version implemented by this crate.
pub const BEHAVIOR_SCHEMA_VERSION: u32 = easycon_model::BEHAVIOR_SCHEMA_VERSION;
