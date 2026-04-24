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

