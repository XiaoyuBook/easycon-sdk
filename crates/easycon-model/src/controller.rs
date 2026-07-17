use crate::{EasyConError, ErrorCode, ErrorDomain};

/// Source-exact Nintendo Switch button bit values.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u16)]
pub enum Button {
    /// Y face button.
    Y = 0x0001,
    /// B face button.
    B = 0x0002,
    /// A face button.
    A = 0x0004,
    /// X face button.
    X = 0x0008,
    /// Left shoulder.
    L = 0x0010,
    /// Right shoulder.
    R = 0x0020,
    /// Left trigger.
    ZL = 0x0040,
    /// Right trigger.
    ZR = 0x0080,
    /// Minus button.
    Minus = 0x0100,
    /// Plus button.
    Plus = 0x0200,
    /// Left stick click.
    LClick = 0x0400,
    /// Right stick click.
    RClick = 0x0800,
    /// Home button.
    Home = 0x1000,
    /// Capture button.
    Capture = 0x2000,
}

impl Button {
    /// All v1 buttons in source declaration order.
    pub const ALL: [Self; 14] = [
        Self::Y,
        Self::B,
        Self::A,
        Self::X,
        Self::L,
        Self::R,
        Self::ZL,
        Self::ZR,
        Self::Minus,
        Self::Plus,
        Self::LClick,
        Self::RClick,
        Self::Home,
        Self::Capture,
    ];

    /// Returns the source-exact report bit.
    #[must_use]
    pub const fn mask(self) -> u16 {
        self as u16
    }
}

/// Source-exact HAT values.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Hat {
    /// Up.
    Top = 0,
    /// Up and right.
    TopRight = 1,
    /// Right.
    Right = 2,
    /// Down and right.
    BottomRight = 3,
    /// Down.
    Bottom = 4,
    /// Down and left.
    BottomLeft = 5,
    /// Left.
    Left = 6,
    /// Up and left.
    TopLeft = 7,
    /// No HAT direction.
    #[default]
    Center = 8,
}

impl Hat {
    /// All source-exact HAT positions, including center.
    pub const ALL: [Self; 9] = [
        Self::Top,
        Self::TopRight,
        Self::Right,
        Self::BottomRight,
        Self::Bottom,
        Self::BottomLeft,
        Self::Left,
        Self::TopLeft,
        Self::Center,
    ];

    /// Returns the protocol value.
    #[must_use]
    pub const fn value(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for Hat {
    type Error = EasyConError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|hat| hat.value() == value)
            .ok_or_else(|| {
                EasyConError::new(
                    ErrorDomain::Validation,
                    ErrorCode::InvalidArgument,
                    "HAT value must be in 0..=8",
                )
            })
    }
}

/// Explicit byte coordinates for one analog stick.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StickPosition {
    /// Horizontal coordinate in `0..=255`.
    pub x: u8,
    /// Vertical coordinate in `0..=255`.
    pub y: u8,
}

impl StickPosition {
    /// Source-exact neutral center.
    pub const CENTER: Self = Self { x: 128, y: 128 };

    /// Creates a position. Byte inputs make range validity explicit at the type boundary.
    #[must_use]
    pub const fn new(x: u8, y: u8) -> Self {
        Self { x, y }
    }
}

impl Default for StickPosition {
    fn default() -> Self {
        Self::CENTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_values_are_unique_and_complete() {
        let mask = Button::ALL
            .into_iter()
            .fold(0_u16, |combined, button| combined | button.mask());
        assert_eq!(mask, 0x3fff);
        assert_eq!(Hat::ALL.map(Hat::value), [0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(StickPosition::CENTER, StickPosition::new(128, 128));
    }

    #[test]
    fn invalid_hat_is_a_validation_error() {
        let error = Hat::try_from(9).expect_err("nine is not a HAT value");
        assert_eq!(error.domain(), ErrorDomain::Validation);
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
    }
}
