use std::sync::Arc;

use easycon_model::{EasyConError, ErrorCode, ErrorDomain};

use crate::session::ControllerAction;

/// Maximum number of input actions in one first-milestone sequence.
pub const MAX_SEQUENCE_STEPS: usize = 10_000;
/// Maximum absolute sequence duration: 24 monotonic hours.
pub const MAX_SEQUENCE_DURATION_NS: u64 = 86_400_000_000_000;

/// One action at an absolute offset from lane acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SequenceStep {
    /// Absolute offset from the sequence's lane start.
    pub offset_ns: u64,
    /// Desired-state mutation.
    pub action: ControllerAction,
}

impl SequenceStep {
    /// Creates one validated-by-container step.
    #[must_use]
    pub const fn new(offset_ns: u64, action: ControllerAction) -> Self {
        Self { offset_ns, action }
    }
}

/// Fully validated immutable precise sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreciseSequence {
    steps: Arc<[SequenceStep]>,
}

impl PreciseSequence {
    /// Validates monotonic offsets, step count, and maximum duration before admission.
    pub fn new(steps: impl Into<Vec<SequenceStep>>) -> Result<Self, EasyConError> {
        let steps = steps.into();
        if steps.is_empty() {
            return Err(validation_error(
                "precise sequence must contain at least one step",
            ));
        }
        if steps.len() > MAX_SEQUENCE_STEPS {
            return Err(validation_error("precise sequence exceeds the step limit"));
        }
        if steps
            .windows(2)
            .any(|pair| pair[0].offset_ns > pair[1].offset_ns)
        {
            return Err(validation_error(
                "precise sequence offsets must be monotonically non-decreasing",
            ));
        }
        if steps
            .last()
            .is_some_and(|step| step.offset_ns > MAX_SEQUENCE_DURATION_NS)
        {
            return Err(validation_error(
                "precise sequence exceeds the duration limit",
            ));
        }
        Ok(Self {
            steps: steps.into(),
        })
    }

    /// Returns validated steps in input order.
    #[must_use]
    pub fn steps(&self) -> &[SequenceStep] {
        &self.steps
    }
}

fn validation_error(message: &'static str) -> EasyConError {
    EasyConError::new(ErrorDomain::Validation, ErrorCode::InvalidArgument, message)
}

#[cfg(test)]
mod tests {
    use easycon_model::Button;

    use super::*;

    #[test]
    fn validates_offsets_and_preserves_same_offset_order() {
        let sequence = PreciseSequence::new(vec![
            SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
            SequenceStep::new(0, ControllerAction::ButtonDown(Button::B)),
            SequenceStep::new(30, ControllerAction::ButtonUp(Button::A)),
        ])
        .expect("sequence");
        assert_eq!(sequence.steps()[0].offset_ns, 0);
        assert_eq!(
            sequence.steps()[1].action,
            ControllerAction::ButtonDown(Button::B)
        );

        let error = PreciseSequence::new(vec![
            SequenceStep::new(2, ControllerAction::Reset),
            SequenceStep::new(1, ControllerAction::Reset),
        ])
        .expect_err("backwards offset");
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
    }
}
