use super::{Phase, SessionState};

/// Errors that can occur when attempting to resume a session.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ResumeError {
    InvalidState(String),
    CorruptedArtifacts(Vec<String>),
    ActiveSessionConflict(String),
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResumeError::InvalidState(msg) => write!(f, "Cannot resume: {msg}"),
            ResumeError::CorruptedArtifacts(paths) => {
                write!(f, "Corrupted artifacts: {}", paths.join(", "))
            }
            ResumeError::ActiveSessionConflict(msg) => {
                write!(f, "Active session conflict: {msg}")
            }
        }
    }
}

impl std::error::Error for ResumeError {}

/// Check whether the current state can be safely resumed.
pub fn can_resume(state: &SessionState) -> Result<(), ResumeError> {
    match state.current_phase {
        Phase::Done => {
            return Err(ResumeError::InvalidState(
                "Cannot resume a completed session".to_string(),
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

/// Resume a session, logging the resumption event.
pub fn resume_session(state: &mut SessionState) -> Result<(), ResumeError> {
    can_resume(state)?;
    state
        .log_event("resuming session")
        .map_err(|e| ResumeError::InvalidState(format!("Failed to log resume event: {e}")))?;
    Ok(())
}

