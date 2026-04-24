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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RunState;

    #[test]
    fn test_validate_transition_ok() {
        assert!(validate_transition(&Phase::IdeaInput, &Phase::BrainstormRunning).is_ok());
    }

    #[test]
    fn test_validate_transition_err() {
        let err = validate_transition(&Phase::IdeaInput, &Phase::Done).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Cannot transition from Idea Input to Done"));
    }

    #[test]
    fn test_execute_transition_updates_phase() {
        let mut state = RunState::new("test-run".to_string());
        assert_eq!(state.current_phase, Phase::IdeaInput);

        // execute_transition writes to disk, so give it a temporary run directory
        let dir = std::path::Path::new(".codexize").join("runs").join("test-run");
        let _ = std::fs::create_dir_all(&dir);

        execute_transition(&mut state, Phase::BrainstormRunning).unwrap();
        assert_eq!(state.current_phase, Phase::BrainstormRunning);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
