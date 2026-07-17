#![forbid(unsafe_code)]
//! Stable domain values shared by the EasyCon SDK v1 Rust crates.

mod controller;
mod error;
mod id;

pub use controller::{Button, Hat, StickPosition};
pub use error::{EasyConError, ErrorCode, ErrorDomain};
pub use id::{OperationId, ResourceId, RuntimeId, TaskId};

/// Version of the tracked runtime/controller behavior fixture set.
pub const BEHAVIOR_SCHEMA_VERSION: u32 = 1;
