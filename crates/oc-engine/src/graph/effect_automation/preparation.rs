//! Checked storage limits for one effect's processing-time automation scratch.

use super::{MAX_AUTOMATION_BLOCK_FRAMES, TouchPlan};

const SCRATCH_BYTE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomationPreparationError {
    ScratchCapacity,
    ScratchAllocation,
}

impl std::fmt::Display for AutomationPreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ScratchCapacity => "effect automation exceeds its 16 MiB prepared scratch budget",
            Self::ScratchAllocation => "could not allocate bounded effect automation scratch",
        })
    }
}

impl std::error::Error for AutomationPreparationError {}

pub(super) fn event_capacity(lanes: usize) -> Result<usize, AutomationPreparationError> {
    let events = lanes.checked_mul(MAX_AUTOMATION_BLOCK_FRAMES);
    let bytes = events
        .and_then(|count| count.checked_mul(std::mem::size_of::<(u64, usize, f64)>()))
        .and_then(|bytes| {
            lanes
                .checked_mul(std::mem::size_of::<TouchPlan>())
                .and_then(|plans| bytes.checked_add(plans))
        });
    match (events, bytes) {
        (Some(events), Some(bytes)) if bytes <= SCRATCH_BYTE_LIMIT => Ok(events),
        _ => Err(AutomationPreparationError::ScratchCapacity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_parameter_scratch_rejects_overflow_and_budget_before_allocation() {
        assert_eq!(
            event_capacity(usize::MAX),
            Err(AutomationPreparationError::ScratchCapacity)
        );
        let per_lane = MAX_AUTOMATION_BLOCK_FRAMES * std::mem::size_of::<(u64, usize, f64)>()
            + std::mem::size_of::<TouchPlan>();
        let maximum = SCRATCH_BYTE_LIMIT / per_lane;
        assert_eq!(
            event_capacity(maximum),
            Ok(maximum * MAX_AUTOMATION_BLOCK_FRAMES)
        );
        assert_eq!(
            event_capacity(maximum + 1),
            Err(AutomationPreparationError::ScratchCapacity)
        );
        assert_eq!(event_capacity(0), Ok(0));
    }
}
