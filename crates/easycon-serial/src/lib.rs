#![cfg_attr(not(windows), forbid(unsafe_code))]
#![deny(unsafe_op_in_unsafe_fn)]
//! Windows serial discovery, cancellable byte I/O, and the system Controller transport leaf.

mod discovery;
mod io;
mod transport;
#[cfg(windows)]
mod windows;

pub use discovery::{SerialDiscovery, SerialPortDescriptor, UsbIdentifiers};
pub use io::{ByteIo, ByteIoFactory, ByteIoRequest, SerialError, SerialErrorKind};
pub use transport::SerialControllerTransport;
#[cfg(windows)]
pub use windows::{WindowsByteIoFactory, WindowsSerialDiscovery, discover_system_ports};

/// Behavior schema version implemented by this crate.
pub const BEHAVIOR_SCHEMA_VERSION: u32 = easycon_controller::BEHAVIOR_SCHEMA_VERSION;
