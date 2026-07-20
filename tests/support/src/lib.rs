#![forbid(unsafe_code)]
//! Deterministic fakes compiled only as workspace test support.

mod ch32;
mod controller;

pub use ch32::{
    Ch32AcceptedReport, Ch32AckBehavior, Ch32ByteSimulator, Ch32HandshakeBehavior, Ch32Snapshot,
};
pub use controller::{
    AcceptedWrite, AckOutcome, FakeControllerTransport, HandshakeAttemptRecord, HandshakeOutcome,
};
