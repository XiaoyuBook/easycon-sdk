use easycon_model::{Button, Hat, StickPosition};

/// Mutable desired Switch input state owned exclusively by the controller lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitchReport {
    buttons: u16,
    hat: Hat,
    left_stick: StickPosition,
    right_stick: StickPosition,
}

impl SwitchReport {
    /// Source-exact neutral state.
    pub const NEUTRAL: Self = Self {
        buttons: 0,
        hat: Hat::Center,
        left_stick: StickPosition::CENTER,
        right_stick: StickPosition::CENTER,
    };

    /// Creates a report from validated stable values.
    #[must_use]
    pub const fn new(
        buttons: u16,
        hat: Hat,
        left_stick: StickPosition,
        right_stick: StickPosition,
    ) -> Self {
        Self {
            buttons,
            hat,
            left_stick,
            right_stick,
        }
    }

    /// Returns the complete button mask.
    #[must_use]
    pub const fn buttons(&self) -> u16 {
        self.buttons
    }

    /// Returns the HAT state.
    #[must_use]
    pub const fn hat(&self) -> Hat {
        self.hat
    }

    /// Returns the left stick state.
    #[must_use]
    pub const fn left_stick(&self) -> StickPosition {
        self.left_stick
    }

    /// Returns the right stick state.
    #[must_use]
    pub const fn right_stick(&self) -> StickPosition {
        self.right_stick
    }

    /// Applies a button down transition.
    pub fn press(&mut self, button: Button) {
        self.buttons |= button.mask();
    }

    /// Applies a button up transition.
    pub fn release(&mut self, button: Button) {
        self.buttons &= !button.mask();
    }

    /// Sets the complete HAT state.
    pub fn set_hat(&mut self, hat: Hat) {
        self.hat = hat;
    }

    /// Sets the left stick state.
    pub fn set_left_stick(&mut self, position: StickPosition) {
        self.left_stick = position;
    }

    /// Sets the right stick state.
    pub fn set_right_stick(&mut self, position: StickPosition) {
        self.right_stick = position;
    }

    /// Resets every input to its neutral value.
    pub fn reset(&mut self) {
        *self = Self::NEUTRAL;
    }

    /// Returns whether all buttons and axes are neutral.
    #[must_use]
    pub const fn is_neutral(&self) -> bool {
        self.buttons == 0
            && self.hat.value() == Hat::Center.value()
            && self.left_stick.x == StickPosition::CENTER.x
            && self.left_stick.y == StickPosition::CENTER.y
            && self.right_stick.x == StickPosition::CENTER.x
            && self.right_stick.y == StickPosition::CENTER.y
    }

    /// Encodes seven source bytes into eight 7-bit data chunks with a final high-bit marker.
    #[must_use]
    pub fn encode(self) -> [u8; 8] {
        let serialized = [
            (self.buttons >> 8) as u8,
            self.buttons as u8,
            self.hat.value(),
            self.left_stick.x,
            self.left_stick.y,
            self.right_stick.x,
            self.right_stick.y,
        ];
        let mut packet = [0_u8; 8];
        let mut packet_index = 0;
        let mut accumulator = 0_u64;
        let mut bit_count = 0_u32;
        for byte in serialized {
            accumulator = (accumulator << 8) | u64::from(byte);
            bit_count += 8;
            while bit_count >= 7 {
                bit_count -= 7;
                packet[packet_index] = (accumulator >> bit_count) as u8;
                packet_index += 1;
                accumulator &= (1_u64 << bit_count) - 1;
            }
        }
        debug_assert_eq!(packet_index, packet.len());
        packet[packet.len() - 1] |= 0x80;
        packet
    }
}

impl Default for SwitchReport {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;

    #[test]
    fn every_tracked_golden_vector_matches_source_encoding() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../spec/fixtures/controller/reports-v1.json"
        );
        let document: Value =
            serde_json::from_str(&fs::read_to_string(path).expect("fixture read"))
                .expect("fixture JSON");
        for vector in document["reports"].as_array().expect("reports array") {
            let state = &vector["state"];
            let report = SwitchReport::new(
                as_u16(&state["button_mask"]),
                Hat::try_from(as_u8(&state["hat"])).expect("fixture HAT"),
                StickPosition::new(as_u8(&state["lx"]), as_u8(&state["ly"])),
                StickPosition::new(as_u8(&state["rx"]), as_u8(&state["ry"])),
            );
            let expected: Vec<_> = vector["encoded"]
                .as_array()
                .expect("encoded array")
                .iter()
                .map(as_u8)
                .collect();
            assert_eq!(report.encode().as_slice(), expected, "{}", vector["name"]);
        }
    }

    #[test]
    fn mutations_share_one_report_and_reset_is_neutral() {
        let mut report = SwitchReport::default();
        report.press(Button::A);
        report.press(Button::Home);
        report.set_hat(Hat::TopRight);
        report.set_left_stick(StickPosition::new(0, 255));
        assert_eq!(report.buttons(), Button::A.mask() | Button::Home.mask());
        assert!(!report.is_neutral());

        report.release(Button::A);
        assert_eq!(report.buttons(), Button::Home.mask());
        report.reset();
        assert_eq!(report, SwitchReport::NEUTRAL);
        assert!(report.is_neutral());
    }

    fn as_u8(value: &Value) -> u8 {
        u8::try_from(value.as_u64().expect("fixture integer")).expect("fixture byte")
    }

    fn as_u16(value: &Value) -> u16 {
        u16::try_from(value.as_u64().expect("fixture integer")).expect("fixture u16")
    }
}
