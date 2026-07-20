//! Narrow Windows system boundary for serial discovery and byte I/O.

mod discovery;
mod error;
mod io;

pub use discovery::{WindowsSerialDiscovery, discover_system_ports};
pub use io::WindowsByteIoFactory;
