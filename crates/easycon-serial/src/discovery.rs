use std::fmt;
use std::sync::Arc;

use crate::SerialError;

/// USB identifiers reported by Windows for a serial device.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct UsbIdentifiers {
    /// Vendor identifier from system device properties.
    pub vid: u16,
    /// Product identifier from system device properties.
    pub pid: u16,
}

/// One structured serial port reported by the operating system.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SerialPortDescriptor {
    stable_id: Arc<str>,
    port_name: Arc<str>,
    friendly_name: Option<Arc<str>>,
    manufacturer: Option<Arc<str>>,
    usb: Option<UsbIdentifiers>,
}

impl SerialPortDescriptor {
    /// Creates a descriptor from a stable system instance ID and a usable port name.
    pub fn new(
        stable_id: impl Into<Arc<str>>,
        port_name: impl Into<Arc<str>>,
    ) -> Result<Self, SerialError> {
        let stable_id = stable_id.into();
        let port_name = port_name.into();
        if stable_id.trim().is_empty() || port_name.trim().is_empty() {
            return Err(SerialError::invalid_port(
                "serial stable ID and port name must be non-empty",
            ));
        }
        Ok(Self {
            stable_id,
            port_name,
            friendly_name: None,
            manufacturer: None,
            usb: None,
        })
    }

    /// Adds a system-provided friendly name.
    #[must_use]
    pub fn with_friendly_name(mut self, value: impl Into<Arc<str>>) -> Self {
        self.friendly_name = non_empty(value.into());
        self
    }

    /// Adds a system-provided manufacturer.
    #[must_use]
    pub fn with_manufacturer(mut self, value: impl Into<Arc<str>>) -> Self {
        self.manufacturer = non_empty(value.into());
        self
    }

    /// Adds USB VID/PID only when Windows supplied those identifiers.
    #[must_use]
    pub const fn with_usb_identifiers(mut self, value: UsbIdentifiers) -> Self {
        self.usb = Some(value);
        self
    }

    /// Stable Windows device-instance identity; independent of the current COM assignment.
    #[must_use]
    pub fn stable_id(&self) -> &str {
        &self.stable_id
    }

    /// Current system port name such as `COM4`.
    #[must_use]
    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    /// Optional friendly name reported by the system.
    #[must_use]
    pub fn friendly_name(&self) -> Option<&str> {
        self.friendly_name.as_deref()
    }

    /// Optional manufacturer reported by the system.
    #[must_use]
    pub fn manufacturer(&self) -> Option<&str> {
        self.manufacturer.as_deref()
    }

    /// Optional VID/PID reported by system hardware properties.
    #[must_use]
    pub const fn usb_identifiers(&self) -> Option<UsbIdentifiers> {
        self.usb
    }
}

impl fmt::Display for SerialPortDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.port_name, self.stable_id)
    }
}

fn non_empty(value: Arc<str>) -> Option<Arc<str>> {
    (!value.trim().is_empty()).then_some(value)
}

/// Injectable structured serial discovery boundary.
pub trait SerialDiscovery: Send + Sync + 'static {
    /// Returns present ports in stable-ID order.
    fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_identity_is_independent_from_optional_labels_and_port_assignment() {
        let first = SerialPortDescriptor::new("USB\\VID_1234&PID_ABCD\\SERIAL", "COM3")
            .expect("descriptor")
            .with_friendly_name("USB serial port")
            .with_manufacturer("Example")
            .with_usb_identifiers(UsbIdentifiers {
                vid: 0x1234,
                pid: 0xabcd,
            });
        let reassigned =
            SerialPortDescriptor::new(first.stable_id(), "COM19").expect("reassigned descriptor");

        assert_eq!(first.stable_id(), reassigned.stable_id());
        assert_ne!(first.port_name(), reassigned.port_name());
        assert_eq!(
            first.usb_identifiers(),
            Some(UsbIdentifiers {
                vid: 0x1234,
                pid: 0xabcd
            })
        );
    }

    #[test]
    fn identity_fields_must_be_explicit_and_non_empty() {
        let error = SerialPortDescriptor::new(" ", "COM1").expect_err("empty stable ID");
        assert_eq!(error.kind(), crate::SerialErrorKind::InvalidPort);

        let error = SerialPortDescriptor::new("instance", "").expect_err("empty port");
        assert_eq!(error.kind(), crate::SerialErrorKind::InvalidPort);
    }
}
