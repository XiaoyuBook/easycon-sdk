#![forbid(unsafe_code)]
//! Deterministic fakes compiled only as workspace test support.

mod controller;

pub use controller::{
    AcceptedWrite, AckOutcome, FakeControllerTransport, HandshakeAttemptRecord, HandshakeOutcome,
};
