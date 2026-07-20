//! Narrow Windows system boundary for serial discovery and byte I/O.

mod discovery;
mod error;
mod io;

const MAX_COM_PORT_NAME_CHARS: usize = 64;

pub use discovery::{WindowsSerialDiscovery, discover_system_ports};
pub use io::WindowsByteIoFactory;
