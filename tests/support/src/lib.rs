#![forbid(unsafe_code)]
//! Deterministic fakes compiled only as workspace test support.

mod controller;

pub use controller::{
    AcceptedWrite, FakeControllerTransport, HandshakeAttemptRecord, HandshakeOutcome,
};
