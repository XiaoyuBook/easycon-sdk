use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DICS_FLAG_GLOBAL, DIGCF_PRESENT, DIREG_DEV, GUID_DEVCLASS_PORTS, HDEVINFO,
    SETUP_DI_REGISTRY_PROPERTY, SP_DEVINFO_DATA, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SPDRP_MFG,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDevRegKey,
};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_DATA, ERROR_NO_MORE_ITEMS,
    ERROR_SUCCESS, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Registry::{
    HKEY, KEY_READ, REG_MULTI_SZ, REG_SZ, RegCloseKey, RegQueryValueExW,
};

use crate::{SerialDiscovery, SerialError, SerialErrorKind, SerialPortDescriptor, UsbIdentifiers};

use super::error::{from_code, last_error};

const MAX_INSTANCE_ID_CHARS: u32 = 4_096;
const MAX_PROPERTY_BYTES: u32 = 65_536;

/// Windows 10/11 SetupAPI discovery implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsSerialDiscovery;

impl SerialDiscovery for WindowsSerialDiscovery {
    fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError> {
        discover_system_ports()
    }
}

/// Enumerates present COM ports and returns descriptors ordered by stable device-instance ID.
pub fn discover_system_ports() -> Result<Vec<SerialPortDescriptor>, SerialError> {
    let list = DeviceInfoSet::ports()?;
    let mut ports = Vec::new();
    let mut index = 0_u32;
    loop {
        let mut info = SP_DEVINFO_DATA {
            cbSize: u32::try_from(size_of::<SP_DEVINFO_DATA>())
                .expect("SP_DEVINFO_DATA size fits u32"),
            ..SP_DEVINFO_DATA::default()
        };
        // SAFETY: `list` owns a valid HDEVINFO; `info` has the documented cbSize and remains
        // writable for the duration of the call.
        if unsafe { SetupDiEnumDeviceInfo(list.raw(), index, &mut info) } == 0 {
            // SAFETY: sampled immediately after SetupDiEnumDeviceInfo on the same thread.
            let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
            if code == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(from_code("SetupDiEnumDeviceInfo", code));
        }
        index = index.checked_add(1).ok_or_else(|| {
            SerialError::new(SerialErrorKind::Io, "serial discovery index exhausted")
        })?;

        let Some(port_name) = registry_port_name(list.raw(), &info)? else {
            continue;
        };
        if !is_com_port_name(&port_name) {
            continue;
        }
        let stable_id = device_instance_id(list.raw(), &info)?;
        let mut descriptor = SerialPortDescriptor::new(stable_id, port_name)?;
        if let Some(friendly_name) =
            registry_property_string(list.raw(), &info, SPDRP_FRIENDLYNAME)?
        {
            descriptor = descriptor.with_friendly_name(friendly_name);
        }
        if let Some(manufacturer) = registry_property_string(list.raw(), &info, SPDRP_MFG)? {
            descriptor = descriptor.with_manufacturer(manufacturer);
        }
        if let Some(hardware_ids) = registry_property_strings(list.raw(), &info, SPDRP_HARDWAREID)?
            && let Some(usb) = hardware_ids
                .iter()
                .find_map(|value| parse_usb_identifiers(value))
        {
            descriptor = descriptor.with_usb_identifiers(usb);
        }
        ports.push(descriptor);
    }
    ports.sort_by(|left, right| left.stable_id().cmp(right.stable_id()));
    ports.dedup_by(|left, right| left.stable_id() == right.stable_id());
    Ok(ports)
}

struct DeviceInfoSet(HDEVINFO);

impl DeviceInfoSet {
    fn ports() -> Result<Self, SerialError> {
        // SAFETY: GUID_DEVCLASS_PORTS is a process-lifetime constant; null enumerator/parent are
        // explicitly permitted. The returned list is owned by this RAII wrapper.
        let raw = unsafe {
            SetupDiGetClassDevsW(&GUID_DEVCLASS_PORTS, null(), null_mut(), DIGCF_PRESENT)
        };
        if raw == INVALID_HANDLE_VALUE as isize {
            Err(last_error("SetupDiGetClassDevsW"))
        } else {
            Ok(Self(raw))
        }
    }

    const fn raw(&self) -> HDEVINFO {
        self.0
    }
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        // SAFETY: this wrapper is the unique owner of the valid HDEVINFO and destroys it once.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

struct RegistryKey(HKEY);

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the HKEY returned by SetupDiOpenDevRegKey and closes it once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn registry_port_name(
    list: HDEVINFO,
    info: &SP_DEVINFO_DATA,
) -> Result<Option<String>, SerialError> {
    // SAFETY: list/info originate from the same live SetupAPI list; requested scope/key are the
    // documented device parameters key and the returned HKEY is wrapped immediately.
    let raw_key =
        unsafe { SetupDiOpenDevRegKey(list, info, DICS_FLAG_GLOBAL, 0, DIREG_DEV, KEY_READ) };
    if raw_key == INVALID_HANDLE_VALUE {
        return Err(last_error("SetupDiOpenDevRegKey"));
    }
    let key = RegistryKey(raw_key);
    let value_name = wide_nul("PortName");
    let mut value_type = 0_u32;
    let mut required_bytes = 0_u32;
    // SAFETY: value_name is NUL-terminated; null data with a valid size pointer is the documented
    // sizing call and key stays live through both queries.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            value_name.as_ptr(),
            null(),
            &mut value_type,
            null_mut(),
            &mut required_bytes,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(from_code("RegQueryValueExW(PortName size)", status));
    }
    if value_type != REG_SZ || required_bytes == 0 {
        return Ok(None);
    }
    validate_property_size(required_bytes, "PortName")?;
    let mut bytes = vec![0_u8; usize::try_from(required_bytes).expect("u32 fits usize")];
    // SAFETY: bytes has exactly the capacity reported by the sizing call; all pointers remain
    // valid and required_bytes is updated within the allocated length.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            value_name.as_ptr(),
            null(),
            &mut value_type,
            bytes.as_mut_ptr(),
            &mut required_bytes,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(from_code("RegQueryValueExW(PortName)", status));
    }
    Ok(decode_first_wide_string(&bytes))
}

fn device_instance_id(list: HDEVINFO, info: &SP_DEVINFO_DATA) -> Result<String, SerialError> {
    let mut required_chars = 0_u32;
    // SAFETY: list/info are a valid pair; null buffer with zero length is the documented sizing
    // query and required_chars is writable.
    let first =
        unsafe { SetupDiGetDeviceInstanceIdW(list, info, null_mut(), 0, &mut required_chars) };
    if first == 0 {
        // SAFETY: sampled immediately after the sizing query on the same thread.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if code != ERROR_INSUFFICIENT_BUFFER || required_chars == 0 {
            return Err(from_code("SetupDiGetDeviceInstanceIdW(size)", code));
        }
    }
    if required_chars > MAX_INSTANCE_ID_CHARS {
        return Err(SerialError::new(
            SerialErrorKind::Io,
            "Windows serial device instance ID exceeds the discovery limit",
        ));
    }
    let mut buffer = vec![0_u16; usize::try_from(required_chars).expect("u32 fits usize")];
    // SAFETY: buffer is writable for the character count returned by the sizing call; list/info
    // remain live and paired.
    if unsafe {
        SetupDiGetDeviceInstanceIdW(
            list,
            info,
            buffer.as_mut_ptr(),
            required_chars,
            &mut required_chars,
        )
    } == 0
    {
        return Err(last_error("SetupDiGetDeviceInstanceIdW"));
    }
    decode_wide(&buffer).ok_or_else(|| {
        SerialError::new(
            SerialErrorKind::Io,
            "Windows returned an empty serial device instance ID",
        )
    })
}

fn registry_property_string(
    list: HDEVINFO,
    info: &SP_DEVINFO_DATA,
    property: SETUP_DI_REGISTRY_PROPERTY,
) -> Result<Option<String>, SerialError> {
    Ok(registry_property_strings(list, info, property)?
        .and_then(|values| values.into_iter().next()))
}

fn registry_property_strings(
    list: HDEVINFO,
    info: &SP_DEVINFO_DATA,
    property: SETUP_DI_REGISTRY_PROPERTY,
) -> Result<Option<Vec<String>>, SerialError> {
    let mut value_type = 0_u32;
    let mut required_bytes = 0_u32;
    // SAFETY: list/info are valid and paired; null property buffer is a documented sizing query.
    let first = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            list,
            info,
            property,
            &mut value_type,
            null_mut(),
            0,
            &mut required_bytes,
        )
    };
    if first == 0 {
        // SAFETY: sampled immediately after the property sizing call on the same thread.
        let code = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        if code == ERROR_INVALID_DATA || code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if code != ERROR_INSUFFICIENT_BUFFER || required_bytes == 0 {
            return Err(from_code("SetupDiGetDeviceRegistryPropertyW(size)", code));
        }
    }
    if value_type != REG_SZ && value_type != REG_MULTI_SZ {
        return Ok(None);
    }
    validate_property_size(required_bytes, "device property")?;
    let mut bytes = vec![0_u8; usize::try_from(required_bytes).expect("u32 fits usize")];
    // SAFETY: the byte buffer is sized from SetupAPI's required length and remains writable for
    // the call; list/info remain live and paired.
    if unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            list,
            info,
            property,
            &mut value_type,
            bytes.as_mut_ptr(),
            required_bytes,
            &mut required_bytes,
        )
    } == 0
    {
        return Err(last_error("SetupDiGetDeviceRegistryPropertyW"));
    }
    let strings = decode_wide_strings(&bytes);
    Ok((!strings.is_empty()).then_some(strings))
}

fn decode_first_wide_string(bytes: &[u8]) -> Option<String> {
    decode_wide_strings(bytes).into_iter().next()
}

fn decode_wide_strings(bytes: &[u8]) -> Vec<String> {
    let units: Vec<_> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    units
        .split(|unit| *unit == 0)
        .filter(|value| !value.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

fn decode_wide(units: &[u16]) -> Option<String> {
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    (end != 0).then(|| String::from_utf16_lossy(&units[..end]))
}

fn wide_nul(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn validate_property_size(size: u32, property: &'static str) -> Result<(), SerialError> {
    if size > MAX_PROPERTY_BYTES {
        Err(SerialError::new(
            SerialErrorKind::Io,
            format!("Windows serial {property} exceeds the discovery limit"),
        ))
    } else {
        Ok(())
    }
}

fn is_com_port_name(value: &str) -> bool {
    value
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("COM"))
        && value.get(3..).is_some_and(|suffix| {
            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn parse_usb_identifiers(value: &str) -> Option<UsbIdentifiers> {
    let uppercase = value.to_ascii_uppercase();
    let vid = parse_hex_component(&uppercase, "VID_")?;
    let pid = parse_hex_component(&uppercase, "PID_")?;
    Some(UsbIdentifiers { vid, pid })
}

fn parse_hex_component(value: &str, marker: &str) -> Option<u16> {
    let start = value.find(marker)?.checked_add(marker.len())?;
    let end = start.checked_add(4)?;
    let digits = value.get(start..end)?;
    if value.as_bytes().get(end).is_some_and(u8::is_ascii_hexdigit) {
        return None;
    }
    (digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| u16::from_str_radix(digits, 16).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    // conformance: phase2a.serial.system-properties
    #[test]
    fn usb_ids_come_only_from_explicit_hardware_id_components() {
        assert_eq!(
            parse_usb_identifiers("USB\\VID_1A86&PID_7523&REV_0264"),
            Some(UsbIdentifiers {
                vid: 0x1a86,
                pid: 0x7523
            })
        );
        assert_eq!(parse_usb_identifiers("ROOT\\PORTS\\0000"), None);
        assert_eq!(parse_usb_identifiers("COM3"), None);
        assert_eq!(parse_usb_identifiers("USB\\VID_ZZZZ&PID_7523"), None);
        assert_eq!(parse_usb_identifiers("USB\\VID_12345&PID_7523"), None);
    }

    #[test]
    fn only_system_com_port_values_are_openable_serial_names() {
        assert!(is_com_port_name("COM1"));
        assert!(is_com_port_name("com42"));
        assert!(!is_com_port_name("LPT1"));
        assert!(!is_com_port_name("COM"));
        assert!(!is_com_port_name("COM4 description"));
    }

    // conformance: phase2a.serial.live-discovery
    #[test]
    fn live_discovery_is_sorted_and_has_stable_identity() {
        let ports = discover_system_ports().expect("Windows serial discovery");
        assert!(
            ports
                .windows(2)
                .all(|pair| pair[0].stable_id() < pair[1].stable_id())
        );
        assert!(
            ports
                .iter()
                .all(|port| { !port.stable_id().is_empty() && is_com_port_name(port.port_name()) })
        );
    }
}
