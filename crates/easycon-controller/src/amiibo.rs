use easycon_model::{EasyConError, ErrorCode, ErrorDomain};

/// Source-exact Amiibo save chunk size.
pub const AMIIBO_CHUNK_SIZE: usize = 20;
/// Protocol maximum implied by two seven-bit offset bytes.
pub const AMIIBO_PROTOCOL_MAX_BYTES: usize = 16_384;
/// Resource-safety ceiling for caller-selected reset-and-retry cycles per chunk.
pub const MAX_AMIIBO_CHUNK_RETRIES: u8 = 8;
/// Source-exact command prefix.
pub const AMIIBO_READY: u8 = 0xa5;
/// Source-exact save command.
pub const AMIIBO_SAVE: u8 = 0x90;
/// Source-exact slot-selection command.
pub const AMIIBO_SELECT: u8 = 0x91;
/// Source-exact positive command acknowledgement.
pub const AMIIBO_ACK: u8 = 0xff;
/// Source-exact reset hello reply.
pub const AMIIBO_RESET_REPLY: u8 = 0x80;
/// Source-compatible reset hello timeout used by selection cancellation cleanup.
pub const AMIIBO_DEFAULT_RESET_TIMEOUT_NS: u64 = 50_000_000;

/// Explicit device limits supplied by capability evidence outside Phase 2A.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmiiboLimits {
    slot_count: u16,
    maximum_data_len: usize,
}

impl AmiiboLimits {
    /// Creates bounded limits without claiming that any physical device supports them.
    pub fn new(slot_count: u16, maximum_data_len: usize) -> Result<Self, EasyConError> {
        if slot_count == 0 || slot_count > 256 {
            return Err(validation_error("Amiibo slot count must be in 1..=256"));
        }
        if maximum_data_len == 0 || maximum_data_len > AMIIBO_PROTOCOL_MAX_BYTES {
            return Err(validation_error(
                "Amiibo maximum data length must be in 1..=16384",
            ));
        }
        Ok(Self {
            slot_count,
            maximum_data_len,
        })
    }

    /// Returns the caller-supplied number of valid zero-based slots.
    #[must_use]
    pub const fn slot_count(self) -> u16 {
        self.slot_count
    }

    /// Returns the caller-supplied maximum save length.
    #[must_use]
    pub const fn maximum_data_len(self) -> usize {
        self.maximum_data_len
    }

    pub(crate) fn validate_slot(self, slot: u8) -> Result<(), EasyConError> {
        if u16::from(slot) >= self.slot_count {
            Err(validation_error("Amiibo slot exceeds the explicit limit"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn validate_data(self, data_len: usize) -> Result<(), EasyConError> {
        if data_len == 0 {
            Err(validation_error("Amiibo save data must be non-empty"))
        } else if data_len > self.maximum_data_len {
            Err(validation_error(
                "Amiibo save data exceeds the explicit device limit",
            ))
        } else {
            Ok(())
        }
    }
}

/// Deadline and bounded retry policy for one Amiibo save operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmiiboSaveOptions {
    /// Absolute operation deadline, if any.
    pub operation_deadline_ns: Option<u64>,
    /// Per-header and per-payload ACK timeout.
    pub ack_timeout_ns: u64,
    /// Reset hello timeout before retrying a chunk.
    pub reset_timeout_ns: u64,
    /// Maximum reset-and-retry cycles for each chunk.
    pub maximum_chunk_retries: u8,
}

impl Default for AmiiboSaveOptions {
    fn default() -> Self {
        Self {
            operation_deadline_ns: None,
            ack_timeout_ns: 1_000_000_000,
            reset_timeout_ns: AMIIBO_DEFAULT_RESET_TIMEOUT_NS,
            maximum_chunk_retries: 1,
        }
    }
}

/// Deadline policy for one Amiibo slot-selection operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmiiboSelectOptions {
    /// Absolute operation deadline, if any.
    pub operation_deadline_ns: Option<u64>,
    /// Source-compatible selection ACK timeout.
    pub ack_timeout_ns: u64,
}

impl Default for AmiiboSelectOptions {
    fn default() -> Self {
        Self {
            operation_deadline_ns: None,
            ack_timeout_ns: 200_000_000,
        }
    }
}

pub(crate) fn save_header(slot: u8, offset: usize, length: usize) -> [u8; 7] {
    debug_assert!(offset < AMIIBO_PROTOCOL_MAX_BYTES);
    debug_assert!(length <= AMIIBO_CHUNK_SIZE);
    [
        AMIIBO_READY,
        (offset & 0x7f) as u8,
        (offset >> 7) as u8,
        (length & 0x7f) as u8,
        (length >> 7) as u8,
        slot,
        AMIIBO_SAVE,
    ]
}

pub(crate) const fn select_command(slot: u8) -> [u8; 3] {
    [AMIIBO_READY, slot, AMIIBO_SELECT]
}

pub(crate) const fn reset_command() -> [u8; 6] {
    [AMIIBO_READY, 0x81, AMIIBO_READY, 0x81, AMIIBO_READY, 0x81]
}

fn validation_error(message: &'static str) -> EasyConError {
    EasyConError::new(ErrorDomain::Validation, ErrorCode::InvalidArgument, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_exact_save_select_and_reset_bytes_are_stable() {
        assert_eq!(save_header(3, 0, 20), [0xa5, 0, 0, 20, 0, 3, 0x90]);
        assert_eq!(save_header(3, 140, 7), [0xa5, 12, 1, 7, 0, 3, 0x90]);
        assert_eq!(select_command(3), [0xa5, 3, 0x91]);
        assert_eq!(reset_command(), [0xa5, 0x81, 0xa5, 0x81, 0xa5, 0x81]);
    }

    #[test]
    fn limits_are_explicit_and_protocol_bounded() {
        assert!(AmiiboLimits::new(1, 1).is_ok());
        assert!(AmiiboLimits::new(256, AMIIBO_PROTOCOL_MAX_BYTES).is_ok());
        assert!(AmiiboLimits::new(0, 1).is_err());
        assert!(AmiiboLimits::new(257, 1).is_err());
        assert!(AmiiboLimits::new(1, 0).is_err());
        assert!(AmiiboLimits::new(1, AMIIBO_PROTOCOL_MAX_BYTES + 1).is_err());
    }
}
