use super::{Phase, RunState};
use anyhow::{Context, Result};

/// Errors that can occur during phase transitions.
#[derive(Debug)]
pub enum TransitionError {
    InvalidTransition {
        from: Phase,
        to: Phase,
        reason: String,
    },
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransitionError::InvalidTransition { from, to, reason } => {
                write!(
                    f,
                    "Cannot transition from {} to {}: {}",
                    from.display_name(),
                    to.display_name(),
                    reason
                )
            }
        }
    }
}

impl std::error::Error for TransitionError {}

/// Validate that a transition from `from` to `to` is allowed.
pub fn validate_transition(from: &Phase, to: &Phase) -> Result<(), TransitionError> {
    if !from.can_transition_to(to) {
        return Err(TransitionError::InvalidTransition {
            from: *from,
            to: *to,
            reason: format!(
                "Transition from {} to {} is not allowed",
                from.display_name(),
                to.display_name()
            ),
        });
    }
    Ok(())
}

/// Execute a validated transition, updating the state and persisting it.
pub fn execute_transition(state: &mut RunState, to: Phase) -> Result<()> {
    validate_transition(&state.current_phase, &to)
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let old_phase = state.current_phase;
    state.current_phase = to;

    state
        .log_event(format!(
            "transitioned phase from {:?} to {:?}",
            old_phase, to
        ))
        .context("failed to log transition event")?;

    state.save().context("failed to save state after transition")?;

    Ok(())
}

