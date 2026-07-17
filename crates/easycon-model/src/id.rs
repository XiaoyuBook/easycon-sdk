use std::fmt;

macro_rules! define_id {
    ($name:ident, $label:literal) => {
        #[doc = concat!("Runtime-local identifier for a ", $label, ".")]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            #[doc = concat!("Creates a ", $label, " identifier from its non-zero numeric value.")]
            #[must_use]
            pub const fn new(value: u64) -> Self {
                assert!(value != 0, "IDs must be non-zero");
                Self(value)
            }

            /// Returns the numeric identifier.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

define_id!(RuntimeId, "Runtime");
define_id!(OperationId, "operation");
define_id!(ResourceId, "resource");
define_id!(TaskId, "supervised task");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_preserve_numeric_value() {
        let operation = OperationId::new(42);
        assert_eq!(operation.get(), 42);
        assert_eq!(operation.to_string(), "42");
    }

    #[test]
    #[should_panic(expected = "IDs must be non-zero")]
    fn zero_id_is_rejected() {
        let _ = ResourceId::new(0);
    }
}
