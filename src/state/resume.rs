use super::{Phase, RunState};

/// Errors that can occur when attempting to resume a run.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ResumeError {
    InvalidState(String),
    CorruptedArtifacts(Vec<String>),
    ActiveRunConflict(String),
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeError::InvalidState(msg) => write!(f, "Cannot resume: {msg}"),
            ResumeError::CorruptedArtifacts(paths) => {
                write!(f, "Corrupted artifacts: {}", paths.join(", "))
            }
            ResumeError::ActiveRunConflict(msg) => {
                write!(f, "Active run conflict: {msg}")
            }
        }
    }
}

impl std::error::Error for ResumeError {}

/// Check whether the current state can be safely resumed.
pub fn can_resume(state: &RunState) -> Result<(), ResumeError> {
    match state.current_phase {
        Phase::Done => {
            return Err(ResumeError::InvalidState(
                "Cannot resume a completed run".to_string(),
            ));
        }
        Phase::IdeaInput => {
            return Err(ResumeError::InvalidState(
                "No work to resume from IdeaInput phase".to_string(),
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Resume a run, logging the resumption event.
pub fn resume_run(state: &mut RunState) -> Result<(), ResumeError> {
    can_resume(state)?;
    state
        .log_event("resuming run")
        .map_err(|e| ResumeError::InvalidState(format!("Failed to log resume event: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RunState;

    #[test]
    fn test_cannot_resume_done() {
        let mut state = RunState::new("test-resume-done".to_string());
        state.current_phase = Phase::Done;
        let err = can_resume(&state).unwrap_err();
        assert!(format!("{err}").contains("completed run"));
    }

    #[test]
    fn test_cannot_resume_idea_input() {
        let mut state = RunState::new("test-resume-idea".to_string());
        state.current_phase = Phase::IdeaInput;
        let err = can_resume(&state).unwrap_err();
        assert!(format!("{err}").contains("No work to resume"));
    }

    #[test]
    fn test_can_resume_brainstorm() {
        let mut state = RunState::new("test-resume-brainstorm".to_string());
        state.current_phase = Phase::BrainstormRunning;
        assert!(can_resume(&state).is_ok());
    }

    #[test]
    fn test_can_resume_implementation_round() {
        let mut state = RunState::new("test-resume-impl".to_string());
        state.current_phase = Phase::ImplementationRound(2);
        assert!(can_resume(&state).is_ok());
    }

    #[test]
    fn test_resume_run_logs_event() {
        let mut state = RunState::new("test-resume-log".to_string());
        state.current_phase = Phase::PlanningRunning;

        // Set up run directory so log_event can write
        let dir = std::path::Path::new(".codexize").join("runs").join("test-resume-log");
        let _ = std::fs::create_dir_all(&dir);

        assert!(resume_run(&mut state).is_ok());

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
